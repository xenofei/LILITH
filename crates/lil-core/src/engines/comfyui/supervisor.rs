//! The ComfyUI supervisor (the module docs of [`crate::engines`]): one owned process tree or one
//! attached engine, the cached engine processes, the idle free and the shutdown stop.
//!
//! **Locks.** `ops` (async) serialises start, stop and restart and holds the owned engine; the
//! supervisor pass only `try_lock`s it, never across a request, so a start in progress owns the
//! engine's state until it ends and nothing waits behind a slow engine. `view`, `pids` and
//! `idle` are plain mutexes held only to copy or set a value, never across an await; `pids` is
//! also held while the telemetry's tracking is updated (the telemetry takes only its own lock),
//! so [`Comfy::pids`] (read under the jobs queue's lock) never waits for slow work.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use lil_platform::engine_spawn::{EngineSpawn, OnLine, pid_listening_on, spawn_engine};
use lil_platform::job_object::{KillOnCloseJob, descendants};
use lil_platform::process::ProcessHandle;
use lil_platform::sockets::SocketTable;
use lil_platform::{Clock, Env};
use lil_protocol::{ErrorBody, codes};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::client::{ComfyClient, ComfyError};
use super::handler::{SuitePrompts, output_tail_detail};
use super::inventory::Inventory;
use super::outside::{OutsideWatch, Suite};
use super::uploads::Uploads;
use crate::engines::config::{ComfyConfig, DEFAULT_PORT, LOOPBACK, scrubbed_env};
use crate::engines::{
    COMFYUI_ROLE, EngineClass, EngineState, EngineStatus, InventoryStatus, Listen, OutsideJobs,
    TOPIC,
};
use crate::events::EventBus;
use crate::jobs::{EngineKind, Jobs};
use crate::meter::UsageSink;
use crate::state::StateDb;
use crate::telemetry::Telemetry;

/// How often the supervisor sweeps the unused uploads.
const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// How often the supervisor asks `/system_stats`.
const MONITOR_EVERY: Duration = Duration::from_secs(1);
/// How often a start asks whether the engine answers yet.
const READY_POLL: Duration = Duration::from_millis(100);
/// How long an owned engine may take to answer after it was started.
/// A first start after a node install may pip-install requirements (ruling P3-R20).
const START_TIMEOUT: Duration = Duration::from_secs(600);
/// The socket checks (an owned engine's loopback re-check, an attached engine's processes and
/// listeners) run on every this many supervisor passes, and on a change of class.
const SOCKET_CHECK_EVERY: u64 = 5;
/// An attached ComfyUI that hasn't answered for this long (the core's clock) was stopped or
/// restarted by whoever runs it: a node install no longer needs a restart.
const RESTART_CLEARS_AFTER: Duration = Duration::from_secs(30);
/// How long a stop waits for the engine's processes to be gone.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// The output lines a failed start shows.
const OUTPUT_TAIL: usize = 40;
/// How long the scheduler's unload (`/free`) may take: it waits for the unload before an
/// oversized job starts, so one that never ends must not hold that job forever.
pub const UNLOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// The `tracing` target of the engine's output lines (ARCH-119).
pub const LOG_TARGET: &str = "engine.comfyui";

/// Where a job reaches the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyEndpoint {
    pub url: String,
    /// The launcher of an owned engine, the listener of an attached one.
    pub pid: Option<u32>,
    pub class: EngineClass,
    /// `[engines.comfyui] root`, when it is set: the ComfyUI-side cleanup needs it.
    pub root: Option<PathBuf>,
}

/// An engine the core started.
struct Owned {
    job: KillOnCloseJob,
    launcher: ProcessHandle,
}

impl Owned {
    fn running(&self) -> bool {
        matches!(self.launcher.exit_code(), Ok(None))
    }
}

/// What `engines.status` shows, but the parts that come from elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
struct View {
    class: EngineClass,
    state: EngineState,
    pid: Option<u32>,
    listen: Listen,
    version: Option<String>,
}

const STOPPED: View = View {
    class: EngineClass::None,
    state: EngineState::Stopped,
    pid: None,
    listen: Listen::Unknown,
    version: None,
};

/// What the supervisor saw of an attached engine, for `restart_needed`: the last listener it
/// found, and since when (the core's clock) the engine hasn't answered.
#[derive(Debug, Clone, Copy, Default)]
struct AttachedSeen {
    listener: Option<u32>,
    unreachable_since: Option<Duration>,
}

/// When a suite job last used the engine, and whether it was freed since.
#[derive(Debug, Clone, Copy)]
struct Idle {
    last_used: Duration,
    freed: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The ComfyUI supervisor (module docs).
pub(crate) struct Comfy {
    config: Option<ComfyConfig>,
    /// Why `[engines.comfyui]` was not read, when it wasn't.
    config_error: Option<String>,
    client: Option<ComfyClient>,
    /// Weak: the jobs hold the GPU scheduler, which holds the engines.
    jobs: Weak<Jobs>,
    telemetry: Arc<Telemetry>,
    events: Arc<EventBus>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) usage: Arc<dyn UsageSink>,
    /// Supervisor passes so far.
    passes: AtomicU64,
    /// Held for a whole pass: passes (the supervisor's, a test's) run one at a time. Not
    /// `ops`, so a start, stop or `StopEngines` never waits for a pass.
    pass_gate: tokio::sync::Mutex<()>,
    attached: Mutex<AttachedSeen>,
    sockets: Arc<dyn SocketTable>,
    view: Mutex<View>,
    pids: Mutex<Vec<u32>>,
    restart_needed: AtomicBool,
    ops: tokio::sync::Mutex<Option<Owned>>,
    idle: Mutex<Idle>,
    output: Arc<Mutex<VecDeque<String>>>,
    /// Cancelled by the `StopEngines` step: the supervisor task ends, a start gives up.
    stopped: CancellationToken,
    /// The suite `state.db`: the client id, the uploads.
    suite: Arc<StateDb>,
    /// `Env::local_dir()`: the job caches of jobs without a workspace.
    local_dir: PathBuf,
    /// The suite's prompts in ComfyUI now.
    prompts: Arc<SuitePrompts>,
    /// The channel's `client_id`, read (or minted) once.
    client_id: tokio::sync::OnceCell<String>,
    /// When the unused uploads were last swept (the core's monotonic clock).
    swept: Mutex<Option<Duration>>,
    env: Env,
    /// Outside jobs ([`super::outside`]).
    outside: Arc<OutsideWatch>,
    /// `(readiness epoch, ready)`: the epoch counts the times the engine became ready (0:
    /// never), so what was learnt of one engine run ([`GraphInfo`], the inventory) is dropped
    /// with it.
    ready: tokio::sync::watch::Sender<(u64, bool)>,
    /// `/object_info` and `/node_replacements`, read once per readiness epoch.
    graph_info: tokio::sync::Mutex<Option<(u64, Arc<GraphInfo>)>>,
    /// The model inventory refreshed in a readiness epoch.
    inventory: Mutex<Option<(u64, Arc<Inventory>)>>,
}

/// What the graph checks need of one engine run: `/object_info` and `/node_replacements`
/// (`{}` for a ComfyUI without that route).
#[derive(Debug)]
pub(crate) struct GraphInfo {
    pub object_info: Arc<Value>,
    pub replacements: Value,
}

impl Comfy {
    pub(crate) fn new(
        env: &Env,
        jobs: Arc<Jobs>,
        telemetry: Arc<Telemetry>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        usage: Arc<dyn UsageSink>,
        sockets: Arc<dyn SocketTable>,
    ) -> Arc<Comfy> {
        let (config, config_error) = match read_config(env) {
            Ok(config) => (config, None),
            Err(why) => {
                tracing::warn!(error = %why, "[engines.comfyui] in suite.toml can't be read; ComfyUI counts as not configured");
                (None, Some(why))
            }
        };
        let client = config
            .as_ref()
            .and_then(|c| ComfyClient::new(&c.url()).ok());
        let now = clock.monotonic();
        let suite = jobs.suite_state();
        Arc::new(Comfy {
            config,
            config_error,
            client,
            jobs: Arc::downgrade(&jobs),
            telemetry,
            events,
            clock,
            usage,
            passes: AtomicU64::new(0),
            pass_gate: tokio::sync::Mutex::new(()),
            attached: Mutex::default(),
            sockets,
            view: Mutex::new(STOPPED),
            pids: Mutex::new(Vec::new()),
            restart_needed: AtomicBool::new(false),
            ops: tokio::sync::Mutex::new(None),
            idle: Mutex::new(Idle {
                last_used: now,
                freed: false,
            }),
            output: Arc::default(),
            stopped: CancellationToken::new(),
            suite,
            local_dir: env.local_dir(),
            prompts: Arc::default(),
            client_id: tokio::sync::OnceCell::new(),
            swept: Mutex::new(None),
            env: env.clone(),
            outside: Arc::default(),
            ready: tokio::sync::watch::Sender::new((0, false)),
            graph_info: tokio::sync::Mutex::new(None),
            inventory: Mutex::new(None),
        })
    }

    /// At the core's start: the upload references a dead core left are dropped (review M3).
    pub(crate) async fn reset_uploads(&self) {
        Uploads::new(self.suite.clone(), self.clock.clone())
            .reset()
            .await;
    }

    /// Whether the owned engine whose launcher was `pid` has stopped: its prompts went with
    /// it (an attached engine that stops answering may only be busy, so it never counts).
    pub(crate) fn owned_gone(&self, pid: Option<u32>) -> bool {
        let v = lock(&self.view).clone();
        !(v.class == EngineClass::Owned && v.pid == pid) && matches!(v.state, EngineState::Stopped)
    }

    /// The HTTP client, when ComfyUI is configured.
    pub(crate) fn client(&self) -> Option<&ComfyClient> {
        self.client.as_ref()
    }

    pub(crate) fn prompts(&self) -> &Arc<SuitePrompts> {
        &self.prompts
    }

    pub(crate) fn suite_state(&self) -> Arc<StateDb> {
        self.suite.clone()
    }

    pub(crate) fn local_dir(&self) -> &Path {
        &self.local_dir
    }

    /// The client of the `comfy.prompt` jobs: with the channel's stable `client_id`, kept in
    /// the suite `state.db` (`comfy_client`), minted with `mint` the first time.
    ///
    /// # Errors
    /// `engine.not_configured`; `state.unavailable` when the id can't be read or kept.
    pub(crate) async fn prompt_client(
        &self,
        mint: impl FnOnce() -> String,
    ) -> Result<ComfyClient, ErrorBody> {
        let (_, client) = self.configured()?;
        let suite = self.suite.clone();
        let id = self
            .client_id
            .get_or_try_init(|| async move {
                let minted = mint();
                suite
                    .call(move |conn| {
                        let run = || -> rusqlite::Result<String> {
                            let tx = conn.transaction_with_behavior(
                                rusqlite::TransactionBehavior::Immediate,
                            )?;
                            let kept: Option<String> = tx
                                .query_row("SELECT id FROM comfy_client LIMIT 1", [], |r| r.get(0))
                                .map(Some)
                                .or_else(|e| match e {
                                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                                    e => Err(e),
                                })?;
                            let id = match kept {
                                Some(id) => id,
                                None => {
                                    tx.execute(
                                        "INSERT INTO comfy_client (id) VALUES (?1)",
                                        [&minted],
                                    )?;
                                    minted
                                }
                            };
                            tx.commit()?;
                            Ok(id)
                        };
                        run().map_err(|e| crate::state::StateError::Io(e.to_string()))
                    })
                    .await
                    .map_err(|e| {
                        ErrorBody::new(
                            &codes::STATE_UNAVAILABLE,
                            format!("ComfyUI's client id can't be read or kept: {e}"),
                            "",
                        )
                    })
            })
            .await?;
        Ok(client.clone().with_client_id(id.clone()))
    }

    /// The `CancelEnginePrompts` step: the suite's own prompts get their per-job cancel.
    pub(crate) async fn cancel_prompts(&self) {
        if let Some(client) = &self.client {
            self.prompts.cancel_all(client).await;
        }
    }

    /// Starts the supervisor task (when ComfyUI is configured): a pass now, then one every
    /// second, until the engines are dropped or stopped.
    pub(crate) fn spawn_monitor(this: &Arc<Comfy>) {
        if this.client.is_none() {
            return;
        }
        let weak = Arc::downgrade(this);
        let stopped = this.stopped.clone();
        tokio::spawn(async move {
            loop {
                let Some(comfy) = weak.upgrade() else {
                    return;
                };
                comfy.tick().await;
                drop(comfy);
                tokio::select! {
                    () = stopped.cancelled() => return,
                    () = tokio::time::sleep(MONITOR_EVERY) => {}
                }
            }
        });
    }

    pub(crate) fn config(&self) -> Option<&ComfyConfig> {
        self.config.as_ref()
    }

    /// The cached engine processes.
    pub(crate) fn pids(&self) -> Vec<u32> {
        lock(&self.pids).clone()
    }

    fn url(&self) -> String {
        self.config.as_ref().map_or_else(
            || format!("http://{LOOPBACK}:{DEFAULT_PORT}"),
            ComfyConfig::url,
        )
    }

    pub(crate) fn status(&self) -> EngineStatus {
        let v = lock(&self.view).clone();
        EngineStatus {
            engine: EngineKind::ComfyUi,
            class: v.class,
            state: v.state,
            url: self.url(),
            pid: v.pid,
            version: v.version,
            listen: v.listen,
            outside_jobs: OutsideJobs {
                running: self.outside.running(),
                queued: self.outside.queued(),
            },
            suite_job: self.prompts.current(),
            restart_needed: self.restart_needed.load(Ordering::SeqCst),
            inventory: self.current_inventory().map(|inv| InventoryStatus {
                models: u32::try_from(inv.files.len()).unwrap_or(u32::MAX),
                refreshed: inv.refreshed.clone(),
            }),
        }
    }

    fn endpoint(&self) -> ComfyEndpoint {
        let v = lock(&self.view).clone();
        ComfyEndpoint {
            url: self.url(),
            pid: v.pid,
            class: v.class,
            root: self.config.as_ref().and_then(|c| c.root.clone()),
        }
    }

    fn publish(&self, kind: &str) {
        let status = serde_json::to_value(self.status()).unwrap_or(Value::Null);
        self.events
            .publish(TOPIC, json!({"kind": kind, "engine": status}));
    }

    /// Sets what `engines.status` shows; a change is published.
    fn set_view(&self, v: View) {
        if self.store_view(v) {
            self.publish("status");
        }
    }

    /// Stores `v`; whether it changed. Becoming ready (from another state, or as another class)
    /// starts a new readiness epoch. An owned engine that stopped runs no outside job any more;
    /// an attached one that stops answering may only be busy, so its outside counts clear only
    /// after [`RESTART_CLEARS_AFTER`] of silence (`attached_unreachable`).
    fn store_view(&self, v: View) -> bool {
        let old = {
            let mut view = lock(&self.view);
            std::mem::replace(&mut *view, v.clone())
        };
        let ready = v.state == EngineState::Ready;
        let new_run = ready && (old.state != EngineState::Ready || old.class != v.class);
        self.ready.send_if_modified(|(epoch, was)| {
            let before = (*epoch, *was);
            if new_run {
                *epoch += 1;
            }
            *was = ready;
            before != (*epoch, *was)
        });
        if v.state == EngineState::Stopped
            && old.class == EngineClass::Owned
            && self.outside.clear()
        {
            self.publish("outside_job");
        }
        old != v
    }

    /// The readiness epoch now (see the `ready` field).
    pub(crate) fn ready_epoch(&self) -> u64 {
        self.ready.borrow().0
    }

    /// `(readiness epoch, ready)`, as it changes.
    pub(crate) fn ready_rx(&self) -> tokio::sync::watch::Receiver<(u64, bool)> {
        self.ready.subscribe()
    }

    /// Cancelled by the `StopEngines` step.
    pub(crate) fn stopped(&self) -> CancellationToken {
        self.stopped.clone()
    }

    pub(crate) fn outside(&self) -> &Arc<OutsideWatch> {
        &self.outside
    }

    /// The jobs, while the core has them.
    pub(crate) fn jobs(&self) -> Option<Arc<Jobs>> {
        self.jobs.upgrade()
    }

    /// The NVML driver version of the latest GPU reading.
    pub(crate) fn driver(&self) -> Option<String> {
        self.telemetry.snapshot().gpu.map(|g| g.driver)
    }

    /// Now, in seconds since 1970, by the core's clock.
    fn now_secs(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    /// Now, RFC 3339 UTC, by the core's clock.
    pub(crate) fn now_rfc3339(&self) -> String {
        lil_schema::time::rfc3339(self.now_secs())
    }

    /// The default workspace's folder (its `registry/models.yaml` names model files).
    pub(crate) fn default_workspace(&self) -> Option<PathBuf> {
        crate::workspace::default_of(&self.env).map(|w| w.path)
    }

    /// Keeps the inventory a job refreshed in readiness `epoch`, unless the engine became ready
    /// again since.
    pub(crate) fn set_inventory(&self, epoch: u64, inventory: Inventory) {
        if epoch == self.ready_epoch() {
            *lock(&self.inventory) = Some((epoch, Arc::new(inventory)));
        }
    }

    /// The inventory refreshed since the engine last became ready, if any.
    pub(crate) fn current_inventory(&self) -> Option<Arc<Inventory>> {
        let epoch = self.ready_epoch();
        lock(&self.inventory)
            .as_ref()
            .filter(|(e, _)| *e == epoch)
            .map(|(_, inv)| inv.clone())
    }

    /// `/object_info` and `/node_replacements` of this engine run, read once per readiness
    /// epoch. A ComfyUI that doesn't serve `/node_replacements` (404) has none.
    ///
    /// # Errors
    /// [`ComfyError`] of `/object_info` or `/node_replacements`.
    pub(crate) async fn graph_info(
        &self,
        client: &ComfyClient,
    ) -> Result<Arc<GraphInfo>, ComfyError> {
        let epoch = self.ready_epoch();
        let mut cached = self.graph_info.lock().await;
        if let Some((e, info)) = cached.as_ref()
            && *e == epoch
        {
            return Ok(info.clone());
        }
        let object_info = client.object_info().await?;
        let replacements = match client.node_replacements().await {
            Ok(r) => r,
            Err(ComfyError::Http(404, _)) => json!({}),
            Err(e) => return Err(e),
        };
        let info = Arc::new(GraphInfo {
            object_info: Arc::new(object_info),
            replacements,
        });
        *cached = Some((epoch, info.clone()));
        Ok(info)
    }

    /// A new call id (`c_<16 hex>`) for a usage draft no tool call made.
    fn call_id(&self) -> String {
        self.jobs
            .upgrade()
            .map_or_else(|| "c_0000000000000000".to_string(), |j| j.mint_id("c"))
    }

    /// One poll of outside jobs ([`super::outside`]): the counts (a change is published on the
    /// `engines` topic as `outside_job`) and the usage of paid outside prompts.
    pub(crate) async fn poll_outside(&self) {
        let Some(client) = &self.client else {
            return;
        };
        let jobs = self.jobs.upgrade();
        // Taken once ComfyUI's answers are in (re-review N2).
        let suite = || {
            let mut prompt_ids = self.prompts.prompt_ids();
            if let Some(jobs) = &jobs {
                prompt_ids.extend(jobs.adopted_engine_ids());
            }
            Suite { prompt_ids }
        };
        let open_job = |id: String| {
            let jobs = jobs.clone();
            async move {
                match jobs {
                    Some(jobs) => jobs.is_open_job_engine_id(&id).await,
                    None => false,
                }
            }
        };
        let now = self.now_secs();
        let object_info = || async {
            match self.graph_info(client).await {
                Ok(info) => Some(info.object_info.clone()),
                Err(e) => {
                    tracing::debug!(error = %e, "ComfyUI's object info could not be read for an outside prompt");
                    None
                }
            }
        };
        let mut call_id = || self.call_id();
        let (changed, drafts) = self
            .outside
            .poll(
                client,
                suite,
                open_job,
                &self.suite,
                now,
                object_info,
                &mut call_id,
            )
            .await;
        if changed {
            self.publish("outside_job");
        }
        // Handed over, then marked: a core that stops in between records it once more rather
        // than never (ruling P3-R27).
        for u in drafts {
            self.usage.record(u.draft, u.ctx).await;
            if let Err(e) = super::outside::mark_recorded(&self.suite, &u.prompt_id, now).await {
                tracing::warn!(error = %e, "a paid outside prompt's usage can't be marked recorded");
            }
        }
    }

    /// Replaces the cached engine processes; the telemetry tracks the new ones and forgets the
    /// gone ones. The diff is applied while the cache is locked, so concurrent calls leave the
    /// telemetry and the cache in step (the telemetry takes only its own lock and never calls
    /// back).
    fn set_pids(&self, new: Vec<u32>) {
        let mut pids = lock(&self.pids);
        for pid in pids.iter().filter(|p| !new.contains(p)) {
            self.telemetry.untrack(*pid);
        }
        for pid in new.iter().filter(|p| !pids.contains(p)) {
            self.telemetry.track(*pid, COMFYUI_ROLE);
        }
        *pids = new;
    }

    pub(crate) fn set_restart_needed(&self) {
        self.restart_needed.store(true, Ordering::SeqCst);
        self.publish("restart_needed");
    }

    fn configured(&self) -> Result<(&ComfyConfig, &ComfyClient), ErrorBody> {
        match (&self.config, &self.client) {
            (Some(config), Some(client)) => Ok((config, client)),
            _ => Err(not_configured(match &self.config_error {
                Some(why) => format!("ComfyUI is not configured: {why}"),
                None => "ComfyUI is not configured: suite.toml has no [engines.comfyui] table."
                    .to_string(),
            })),
        }
    }

    /// A suite job uses the engine now: the idle time starts again.
    fn touch(&self) {
        let now = self.clock.monotonic();
        *lock(&self.idle) = Idle {
            last_used: now,
            freed: false,
        };
    }

    /// One supervision pass (module docs of [`crate::engines`]).
    pub(crate) async fn tick(&self) {
        let (Some(config), Some(client)) = (&self.config, &self.client) else {
            return;
        };
        let _pass = self.pass_gate.lock().await;
        // Uploads no job has used for 24 h leave `<root>\input` (CORE-130), looked at once a
        // minute.
        if let Some(root) = &config.root {
            let now = self.clock.monotonic();
            let due = {
                let mut swept = lock(&self.swept);
                let due = swept.is_none_or(|at| now.saturating_sub(at) >= SWEEP_EVERY);
                if due {
                    *swept = Some(now);
                }
                due
            };
            if due {
                Uploads::new(self.suite.clone(), self.clock.clone())
                    .sweep(root)
                    .await;
            }
        }
        let checks = self
            .passes
            .fetch_add(1, Ordering::SeqCst)
            .is_multiple_of(SOCKET_CHECK_EVERY);
        // What the pass needs of the owned engine, copied: `ops` is never held across a
        // request, so a start, stop or `StopEngines` never waits behind a slow engine. A start,
        // stop or restart in progress owns the state, and the pass leaves it alone.
        let owned = {
            let Ok(ops) = self.ops.try_lock() else {
                return;
            };
            ops.as_ref().map(|o| (o.launcher.pid(), o.job.pids().ok()))
        };
        let answer = client.system_stats().await;
        match (owned, answer) {
            (Some((launcher, pids)), Ok(stats)) => {
                if checks
                    && let Some(pids) = &pids
                    && let Err(e) = self.check_still_loopback(pids).await
                {
                    self.stop_exposed(launcher, e).await;
                    return;
                }
                {
                    let Ok(ops) = self.ops.try_lock() else {
                        return;
                    };
                    if ops.as_ref().map(|o| o.launcher.pid()) != Some(launcher) {
                        return;
                    }
                    if let Some(pids) = pids {
                        self.set_pids(pids);
                    }
                    self.set_view(View {
                        class: EngineClass::Owned,
                        state: EngineState::Ready,
                        pid: Some(launcher),
                        listen: Listen::Loopback,
                        version: version_of(&stats),
                    });
                }
                self.idle_free(config, client).await;
            }
            (Some((launcher, _)), Err(e)) => {
                let Ok(mut ops) = self.ops.try_lock() else {
                    return;
                };
                let Some(owned) = ops.as_ref().filter(|o| o.launcher.pid() == launcher) else {
                    return;
                };
                if owned.running() {
                    if let Ok(pids) = owned.job.pids() {
                        self.set_pids(pids);
                    }
                    let mut v = lock(&self.view).clone();
                    v.state = EngineState::Unreachable;
                    self.set_view(v);
                } else {
                    tracing::warn!(error = %e, "the owned ComfyUI has exited");
                    if let Some(owned) = ops.take() {
                        end(owned).await;
                    }
                    self.set_pids(Vec::new());
                    self.set_view(STOPPED);
                }
            }
            (None, Ok(stats)) => {
                let known = lock(&self.view).class == EngineClass::Attached;
                let found = if checks || !known {
                    Some(self.discover_attached(config.port).await)
                } else {
                    None
                };
                let Ok(ops) = self.ops.try_lock() else {
                    return;
                };
                if ops.is_some() {
                    return;
                }
                lock(&self.attached).unreachable_since = None;
                match found {
                    Some(found) => self.apply_attached(found, &stats),
                    None => {
                        let mut v = lock(&self.view).clone();
                        v.version = version_of(&stats);
                        self.set_view(v);
                    }
                }
            }
            (None, Err(_)) => {
                let Ok(ops) = self.ops.try_lock() else {
                    return;
                };
                if ops.is_none() {
                    self.set_pids(Vec::new());
                    self.set_view(STOPPED);
                    self.attached_unreachable();
                }
            }
        }
    }

    /// An attached engine that once answered doesn't now: after [`RESTART_CLEARS_AFTER`] of
    /// that, it was stopped or restarted, so `restart_needed` clears. One failed request alone
    /// clears nothing.
    fn attached_unreachable(&self) {
        let now = self.clock.monotonic();
        let gone = {
            let mut seen = lock(&self.attached);
            let since = *seen.unreachable_since.get_or_insert(now);
            let gone = now.saturating_sub(since) >= RESTART_CLEARS_AFTER;
            if gone && seen.listener.is_some() {
                *seen = AttachedSeen::default();
                self.restart_needed.store(false, Ordering::SeqCst);
            }
            gone
        };
        // Its outside prompts went with it (review M5: not at the first missed answer).
        if gone && self.outside.clear() {
            self.publish("outside_job");
        }
    }

    /// Re-checks a running owned engine's listening sockets (ARCH-068): a custom node can open
    /// a server long after the start.
    async fn check_still_loopback(&self, pids: &[u32]) -> Result<(), ErrorBody> {
        let sockets = self.sockets.clone();
        let listed = pids.to_vec();
        let exposed =
            tokio::task::spawn_blocking(move || exposed_sockets(&listed, sockets.as_ref()))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
        match exposed {
            Ok(exposed) if exposed.is_empty() => Ok(()),
            Ok(exposed) => Err(not_loopback(format!(
                "ComfyUI began listening on {}: it must listen on loopback only (ARCH-068), so \
                 the core stopped it. Check the custom nodes that open a server.",
                list_sockets(&exposed)
            ))),
            // A table that can't be read once isn't a finding; the next check reads it again.
            Err(why) => {
                tracing::warn!(error = %why, "ComfyUI's sockets could not be re-checked");
                Ok(())
            }
        }
    }

    /// Stops the owned engine `launcher` found listening off loopback, and says why on the
    /// `engines` topic.
    async fn stop_exposed(&self, launcher: u32, error: ErrorBody) {
        let mut ops = self.ops.lock().await;
        if ops.as_ref().map(|o| o.launcher.pid()) != Some(launcher) {
            return;
        }
        tracing::error!(error = %error.message, "the owned ComfyUI listens off loopback; it is stopped");
        if let Some(owned) = ops.take() {
            end(owned).await;
        }
        self.set_pids(Vec::new());
        self.store_view(STOPPED);
        let status = serde_json::to_value(self.status()).unwrap_or(Value::Null);
        self.events.publish(
            TOPIC,
            json!({"kind": "status", "engine": status, "error": error}),
        );
    }

    /// An owned engine with no suite job for `idle_free_after_s` is asked to free its models
    /// and memory, once until a job uses it again.
    async fn idle_free(&self, config: &ComfyConfig, client: &ComfyClient) {
        // An outside prompt uses the engine too: it is never freed under one.
        let busy = self.jobs.upgrade().is_some_and(|jobs| jobs.busy()) || self.outside.any();
        let now = self.clock.monotonic();
        let due = {
            let mut idle = lock(&self.idle);
            if busy {
                *idle = Idle {
                    last_used: now,
                    freed: false,
                };
                false
            } else {
                !idle.freed
                    && now.saturating_sub(idle.last_used)
                        >= Duration::from_secs(config.idle_free_after_s)
            }
        };
        if !due {
            return;
        }
        match client.free(true, true).await {
            Ok(()) => {
                lock(&self.idle).freed = true;
                tracing::info!(
                    idle_s = config.idle_free_after_s,
                    "the owned ComfyUI was idle; it was asked to free its models and memory"
                );
            }
            Err(e) => tracing::warn!(error = %e, "the idle ComfyUI's /free failed"),
        }
    }

    /// Records the engine answering on `port` as attached: its processes and where they
    /// listen.
    async fn observe_attached(&self, port: u16, stats: &Value) {
        let found = self.discover_attached(port).await;
        self.apply_attached(found, stats);
    }

    /// The process listening on `port`, it and its descendants, and where they listen (read
    /// off the runtime's threads).
    async fn discover_attached(&self, port: u16) -> (Option<u32>, Vec<u32>, Listen) {
        let sockets = self.sockets.clone();
        tokio::task::spawn_blocking(move || attached_processes(port, sockets.as_ref()))
            .await
            .unwrap_or((None, Vec::new(), Listen::Unknown))
    }

    /// Records a discovered attached engine. When its listener is another process than the one
    /// seen before, whoever runs it restarted it: `restart_needed` clears. A lookup that found
    /// no listener changes nothing there.
    fn apply_attached(&self, found: (Option<u32>, Vec<u32>, Listen), stats: &Value) {
        let (listener, pids, listen) = found;
        if let Some(now) = listener {
            let mut seen = lock(&self.attached);
            if seen.listener.is_some_and(|before| before != now) {
                self.restart_needed.store(false, Ordering::SeqCst);
            }
            *seen = AttachedSeen {
                listener: Some(now),
                unreachable_since: None,
            };
        }
        self.set_pids(pids);
        self.set_view(View {
            class: EngineClass::Attached,
            state: EngineState::Ready,
            pid: listener,
            listen,
            version: version_of(stats),
        });
    }

    pub(crate) async fn start(&self) -> Result<EngineStatus, ErrorBody> {
        let (config, client) = self.configured()?;
        // Refused before anything starts (ARCH-068).
        let args = config.launch_args()?;
        let mut ops = self.ops.lock().await;
        if ops.as_ref().is_some_and(Owned::running) {
            return Ok(self.status());
        }
        if let Some(stale) = ops.take() {
            end(stale).await;
            self.set_pids(Vec::new());
        }
        if let Ok(stats) = client.system_stats().await {
            // Someone else's ComfyUI: attached, nothing to start.
            drop(ops);
            self.observe_attached(config.port, &stats).await;
            return Ok(self.status());
        }
        let root = config.root.clone().filter(|r| r.is_dir()).ok_or_else(|| {
            not_configured(
                "[engines.comfyui] root is not set or is not a folder: the core starts ComfyUI \
                 only from its own folder."
                    .to_string(),
            )
        })?;
        let python = config
            .python_path()
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                not_configured(
                    "ComfyUI's Python was not found: set [engines.comfyui] python, or keep a venv \
                 where the default search looks (<root>\\..\\.venv, <root>\\venv, \
                 <root>\\..\\python_embeded)."
                        .to_string(),
                )
            })?;
        lock(&self.output).clear();
        let owned = self.launch(python, args, root)?;
        let pid = owned.launcher.pid();
        tracing::info!(pid, port = config.port, "the owned ComfyUI is starting");
        self.set_pids(vec![pid]);
        self.set_view(View {
            class: EngineClass::Owned,
            state: EngineState::Starting,
            pid: Some(pid),
            listen: Listen::Unknown,
            version: None,
        });
        let checked = match self.wait_ready(&owned, client).await {
            Ok(stats) => self
                .check_loopback(&owned, config.port)
                .await
                .map(|pids| (stats, pids)),
            Err(e) => Err(e),
        };
        let (stats, pids) = match checked {
            Ok(ready) => ready,
            Err(e) => {
                end(owned).await;
                self.set_pids(Vec::new());
                self.set_view(STOPPED);
                return Err(e);
            }
        };
        self.set_pids(pids);
        self.restart_needed.store(false, Ordering::SeqCst);
        *lock(&self.attached) = AttachedSeen::default();
        self.touch();
        self.set_view(View {
            class: EngineClass::Owned,
            state: EngineState::Ready,
            pid: Some(pid),
            listen: Listen::Loopback,
            version: version_of(&stats),
        });
        *ops = Some(owned);
        tracing::info!(pid, "the owned ComfyUI is ready");
        Ok(self.status())
    }

    /// Starts `<python> <args>` in `root`, in a new kill-on-close Job Object, with the
    /// scrubbed environment; its output goes to the log and the start's tail.
    fn launch(
        &self,
        python: PathBuf,
        args: Vec<OsString>,
        root: PathBuf,
    ) -> Result<Owned, ErrorBody> {
        let job = KillOnCloseJob::new()
            .map_err(|e| start_failed(format!("no Job Object for ComfyUI: {e}"), Vec::new()))?;
        let request = EngineSpawn {
            exe: python,
            args,
            cwd: root,
            env: scrubbed_env(std::env::vars_os()),
        };
        let launcher = spawn_engine(&request, &job, self.output_sink()).map_err(|e| {
            start_failed(
                format!("{} could not be started: {e}", request.exe.display()),
                Vec::new(),
            )
        })?;
        Ok(Owned { job, launcher })
    }

    /// Each output line into the core's log (ARCH-119) and the last [`OUTPUT_TAIL`] into the
    /// start's tail.
    fn output_sink(&self) -> OnLine {
        let tail = self.output.clone();
        Box::new(move |stream: &str, line: &str| {
            tracing::info!(target: "engine.comfyui", stream, "{line}");
            let mut tail = lock(&tail);
            if tail.len() == OUTPUT_TAIL {
                tail.pop_front();
            }
            tail.push_back(line.to_string());
        })
    }

    fn output_tail(&self) -> Vec<String> {
        lock(&self.output).iter().cloned().collect()
    }

    /// Waits until the engine answers `/system_stats`; fails when it exits first, takes longer
    /// than [`START_TIMEOUT`], or the core stops.
    async fn wait_ready(&self, owned: &Owned, client: &ComfyClient) -> Result<Value, ErrorBody> {
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        loop {
            if let Ok(Some(code)) = owned.launcher.exit_code() {
                return Err(start_failed(
                    format!("ComfyUI exited with code {code} before it answered."),
                    self.output_tail(),
                ));
            }
            if let Ok(stats) = client.system_stats().await {
                return Ok(stats);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(start_failed(
                    format!(
                        "ComfyUI did not answer within {} s.",
                        START_TIMEOUT.as_secs()
                    ),
                    self.output_tail(),
                ));
            }
            tokio::select! {
                () = self.stopped.cancelled() => {
                    return Err(start_failed("The core is stopping.".to_string(), Vec::new()));
                }
                () = tokio::time::sleep(READY_POLL) => {}
            }
        }
    }

    /// The engine's processes once it answers, after checking that the one answering on
    /// `port` is among them and that every listening socket of every one is on loopback
    /// (ARCH-068), read through the core's socket table.
    async fn check_loopback(&self, owned: &Owned, port: u16) -> Result<Vec<u32>, ErrorBody> {
        let pids = owned.job.pids().map_err(|e| {
            not_loopback(format!(
                "ComfyUI's processes could not be listed, so its sockets can't be checked: {e}"
            ))
        })?;
        let sockets = self.sockets.clone();
        let listed = pids.clone();
        let found = tokio::task::spawn_blocking(move || {
            let listener = pid_listening_on(port);
            let exposed = exposed_sockets(&listed, sockets.as_ref());
            (listener, exposed)
        })
        .await
        .map_err(|e| not_loopback(format!("the socket check did not run: {e}")))?;
        match found {
            (Ok(Some(listener)), _) if !pids.contains(&listener) => Err(start_failed(
                format!(
                    "Another program (process {listener}) answers on port {port}; the ComfyUI \
                     the core started is not the one listening."
                ),
                self.output_tail(),
            )),
            (_, Err(why)) => Err(not_loopback(why)),
            (_, Ok(exposed)) if !exposed.is_empty() => Err(not_loopback(format!(
                "ComfyUI listened on {}: it must listen on loopback only (ARCH-068), so the \
                     core stopped it. Check extra_args and any custom node that opens a server.",
                list_sockets(&exposed)
            ))),
            _ => Ok(pids),
        }
    }

    pub(crate) async fn stop(&self) -> Result<EngineStatus, ErrorBody> {
        let mut ops = self.ops.lock().await;
        if let Some(owned) = ops.take() {
            end(owned).await;
            self.set_pids(Vec::new());
            self.set_view(STOPPED);
            tracing::info!("the owned ComfyUI was stopped");
            return Ok(self.status());
        }
        drop(ops);
        self.refuse_attached().await?;
        Ok(self.status())
    }

    /// `engine.attached` when an engine the core didn't start answers.
    async fn refuse_attached(&self) -> Result<(), ErrorBody> {
        let (Some(config), Some(client)) = (&self.config, &self.client) else {
            return Ok(());
        };
        if let Ok(stats) = client.system_stats().await {
            self.observe_attached(config.port, &stats).await;
            return Err(ErrorBody::new(
                &codes::ENGINE_ATTACHED,
                "This ComfyUI was started outside the suite (it is attached), so the suite never \
                 stops or restarts it. Stop it where it was started.",
                "",
            ));
        }
        Ok(())
    }

    pub(crate) async fn restart(&self) -> Result<EngineStatus, ErrorBody> {
        {
            let mut ops = self.ops.lock().await;
            match ops.take() {
                Some(owned) => {
                    end(owned).await;
                    self.set_pids(Vec::new());
                    self.set_view(STOPPED);
                }
                None => {
                    drop(ops);
                    self.refuse_attached().await?;
                }
            }
        }
        self.start().await
    }

    pub(crate) async fn ensure_ready(&self) -> Result<ComfyEndpoint, ErrorBody> {
        let (config, client) = self.configured()?;
        self.touch();
        let owned = {
            let ops = self.ops.lock().await;
            ops.as_ref().is_some_and(Owned::running)
        };
        if let Ok(stats) = client.system_stats().await {
            if !owned {
                self.observe_attached(config.port, &stats).await;
            }
            return Ok(self.endpoint());
        }
        if !owned && config.autostart && config.root.is_some() {
            self.start().await?;
            return Ok(self.endpoint());
        }
        Err(ErrorBody::new(
            &codes::COMFYUI_UNREACHABLE,
            format!(
                "ComfyUI isn't answering on {}. Start it (engines.start), or set \
                 [engines.comfyui] autostart = true and root so the suite starts it.",
                self.url()
            ),
            "",
        ))
    }

    /// The scheduler's unload (`/free`), for an owned or attached engine; best effort, and
    /// at most [`UNLOAD_TIMEOUT`]: whether it worked, failed or timed out, it then ends, so the
    /// scheduler carries on. Whether ComfyUI confirmed it.
    pub(crate) async fn unload(&self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        match tokio::time::timeout(UNLOAD_TIMEOUT, client.free(true, true)).await {
            Ok(Ok(())) => {
                lock(&self.idle).freed = true;
                true
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "ComfyUI's /free for the GPU lease failed");
                false
            }
            Err(_) => {
                tracing::warn!(
                    timeout_s = UNLOAD_TIMEOUT.as_secs(),
                    "ComfyUI's /free for the GPU lease did not answer in time; the unload counts as failed"
                );
                false
            }
        }
    }

    /// The `StopEngines` step: the supervisor ends and an owned engine is stopped.
    pub(crate) async fn shutdown(&self) {
        self.stopped.cancel();
        // Paid outside prompts still open stay open in the suite `state.db`: the next core
        // records them (ruling P3-R27).
        let mut ops = self.ops.lock().await;
        if let Some(owned) = ops.take() {
            end(owned).await;
            self.set_pids(Vec::new());
            self.set_view(STOPPED);
            tracing::info!("the owned ComfyUI was stopped with the core");
        }
    }
}

/// `[engines.comfyui]` of `env`'s `suite.toml`; no file is no table.
fn read_config(env: &Env) -> Result<Option<ComfyConfig>, String> {
    match std::fs::read_to_string(env.suite_toml_to_read()) {
        Ok(text) => ComfyConfig::from_suite_toml(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("suite.toml can't be read: {e}")),
    }
}

/// `system.comfyui_version` of a `/system_stats` answer.
fn version_of(stats: &Value) -> Option<String> {
    stats["system"]["comfyui_version"]
        .as_str()
        .map(str::to_string)
}

/// Ends every process of an owned engine and waits (at most [`STOP_TIMEOUT`]) until each has
/// exited. Termination is asynchronous, so the wait is on each process itself (handles opened
/// before the job is ended, so no pid can be reused meanwhile), not on the job's list. Dropping
/// the job afterwards ends anything left.
async fn end(owned: Owned) {
    let others: Vec<ProcessHandle> = owned
        .job
        .pids()
        .unwrap_or_default()
        .into_iter()
        .filter(|pid| *pid != owned.launcher.pid())
        .filter_map(|pid| lil_platform::process::open(pid).ok())
        .collect();
    if let Err(e) = owned.job.terminate() {
        tracing::warn!(error = %e, "ComfyUI's Job Object could not be ended; killing its processes");
        let _ = owned.launcher.kill();
        for p in &others {
            let _ = p.kill();
        }
    }
    let exited = async {
        let _ = owned.launcher.wait().await;
        for p in &others {
            let _ = p.wait().await;
        }
    };
    if tokio::time::timeout(STOP_TIMEOUT, exited).await.is_err() {
        tracing::warn!(
            "ComfyUI's processes were still there after the stop; the Job Object is closed"
        );
    }
    drop(owned);
}

/// The process listening on `port` (`None`: none), it and its descendants, and where they
/// listen.
fn attached_processes(port: u16, sockets: &dyn SocketTable) -> (Option<u32>, Vec<u32>, Listen) {
    let Ok(Some(listener)) = pid_listening_on(port) else {
        return (None, Vec::new(), Listen::Unknown);
    };
    let mut pids = vec![listener];
    match descendants(listener) {
        Ok(more) => pids.extend(more),
        Err(e) => {
            tracing::warn!(error = %e, "the attached ComfyUI's child processes can't be listed")
        }
    }
    let listen = match exposed_sockets(&pids, sockets) {
        Ok(exposed) if exposed.is_empty() => Listen::Loopback,
        Ok(_) => Listen::AllInterfaces,
        Err(_) => Listen::Unknown,
    };
    (Some(listener), pids, listen)
}

/// Every listening socket of `pids` that is not on a loopback address.
///
/// # Errors
/// Why a process's sockets could not be read.
fn exposed_sockets(
    pids: &[u32],
    sockets: &dyn SocketTable,
) -> Result<Vec<(u32, SocketAddr)>, String> {
    let rows = sockets
        .listening_of(pids)
        .map_err(|e| format!("the listening sockets of ComfyUI's processes can't be read: {e}"))?;
    Ok(rows
        .into_iter()
        .filter(|(_, a)| !a.ip().is_loopback())
        .collect())
}

/// `<address> (process <pid>), …`.
fn list_sockets(exposed: &[(u32, SocketAddr)]) -> String {
    exposed
        .iter()
        .map(|(pid, addr)| format!("{addr} (process {pid})"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn not_configured(message: String) -> ErrorBody {
    ErrorBody::new(&codes::ENGINE_NOT_CONFIGURED, message, "")
}

fn not_loopback(message: String) -> ErrorBody {
    ErrorBody::new(&codes::COMFYUI_NOT_LOOPBACK, message, "")
}

/// `comfyui.start_failed`; the engine's last output lines are untrusted detail (ARCH-060).
fn start_failed(message: String, tail: Vec<String>) -> ErrorBody {
    let mut error = ErrorBody::new(&codes::COMFYUI_START_FAILED, message, "");
    error.detail = output_tail_detail(&tail);
    error
}
