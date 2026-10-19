//! The ComfyUI supervisor (the module docs of [`crate::engines`]): one owned process tree or one
//! attached engine, the cached engine processes, the idle free and the shutdown stop.
//!
//! **Locks.** `ops` (async) serialises start, stop and restart and holds the owned engine; the
//! supervisor pass only `try_lock`s it, so a start in progress owns the engine's state until it
//! ends. `view`, `pids` and `idle` are plain mutexes held only to copy or set a value, never
//! across an await or a call into another module, so [`Comfy::pids`] (read under the jobs
//! queue's lock) never waits for slow work.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lil_platform::engine_spawn::{EngineSpawn, OnLine, pid_listening_on, spawn_engine};
use lil_platform::job_object::{KillOnCloseJob, descendants};
use lil_platform::process::ProcessHandle;
use lil_platform::sockets::SocketTable;
use lil_platform::{Clock, Env};
use lil_protocol::{Detail, ErrorBody, codes};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::client::ComfyClient;
use crate::engines::config::{ComfyConfig, DEFAULT_PORT, LOOPBACK, scrubbed_env};
use crate::engines::{
    COMFYUI_ROLE, EngineClass, EngineState, EngineStatus, Listen, OutsideJobs, TOPIC,
};
use crate::events::EventBus;
use crate::jobs::{EngineKind, Jobs};
use crate::meter::UsageSink;
use crate::telemetry::Telemetry;

/// How often the supervisor asks `/system_stats`.
const MONITOR_EVERY: Duration = Duration::from_secs(1);
/// How often a start asks whether the engine answers yet.
const READY_POLL: Duration = Duration::from_millis(100);
/// How long an owned engine may take to answer after it was started.
const START_TIMEOUT: Duration = Duration::from_secs(180);
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
    jobs: Arc<Jobs>,
    telemetry: Arc<Telemetry>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    pub(crate) usage: Arc<dyn UsageSink>,
    sockets: Arc<dyn SocketTable>,
    view: Mutex<View>,
    pids: Mutex<Vec<u32>>,
    restart_needed: AtomicBool,
    ops: tokio::sync::Mutex<Option<Owned>>,
    idle: Mutex<Idle>,
    output: Arc<Mutex<VecDeque<String>>>,
    /// Cancelled by the `StopEngines` step: the supervisor task ends, a start gives up.
    stopped: CancellationToken,
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
        Arc::new(Comfy {
            config,
            config_error,
            client,
            jobs,
            telemetry,
            events,
            clock,
            usage,
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
        })
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
            outside_jobs: OutsideJobs::default(),
            suite_job: None,
            restart_needed: self.restart_needed.load(Ordering::SeqCst),
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
        let changed = {
            let mut view = lock(&self.view);
            let changed = *view != v;
            *view = v;
            changed
        };
        if changed {
            self.publish("status");
        }
    }

    /// Replaces the cached engine processes; the telemetry tracks the new ones and forgets the
    /// gone ones.
    fn set_pids(&self, new: Vec<u32>) {
        let old = std::mem::replace(&mut *lock(&self.pids), new.clone());
        for pid in old.iter().filter(|p| !new.contains(p)) {
            self.telemetry.untrack(*pid);
        }
        for pid in new.iter().filter(|p| !old.contains(p)) {
            self.telemetry.track(*pid, COMFYUI_ROLE);
        }
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
        // A start, stop or restart in progress owns the state.
        let Ok(mut ops) = self.ops.try_lock() else {
            return;
        };
        let answer = client.system_stats().await;
        match (ops.as_ref(), answer) {
            (Some(owned), Ok(stats)) => {
                if let Ok(pids) = owned.job.pids() {
                    self.set_pids(pids);
                }
                self.set_view(View {
                    class: EngineClass::Owned,
                    state: EngineState::Ready,
                    pid: Some(owned.launcher.pid()),
                    listen: Listen::Loopback,
                    version: version_of(&stats),
                });
                self.idle_free(config, client).await;
            }
            (Some(owned), Err(e)) => {
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
                drop(ops);
                self.observe_attached(config.port, &stats).await;
            }
            (None, Err(_)) => {
                self.set_pids(Vec::new());
                self.set_view(STOPPED);
            }
        }
    }

    /// An owned engine with no suite job for `idle_free_after_s` is asked to free its models
    /// and memory, once until a job uses it again.
    async fn idle_free(&self, config: &ComfyConfig, client: &ComfyClient) {
        let busy = self.jobs.busy();
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
        let sockets = self.sockets.clone();
        let (listener, pids, listen) =
            tokio::task::spawn_blocking(move || attached_processes(port, sockets.as_ref()))
                .await
                .unwrap_or((None, Vec::new(), Listen::Unknown));
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
            (_, Ok(exposed)) if !exposed.is_empty() => {
                let list: Vec<String> = exposed
                    .iter()
                    .map(|(pid, addr)| format!("{addr} (process {pid})"))
                    .collect();
                Err(not_loopback(format!(
                    "ComfyUI listened on {}: it must listen on loopback only (ARCH-068), so the \
                     core stopped it. Check extra_args and any custom node that opens a server.",
                    list.join(", ")
                )))
            }
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
    let mut exposed = Vec::new();
    for &pid in pids {
        let addrs = sockets
            .listening(pid)
            .map_err(|e| format!("the listening sockets of process {pid} can't be read: {e}"))?;
        exposed.extend(
            addrs
                .into_iter()
                .filter(|a| !a.ip().is_loopback())
                .map(|a| (pid, a)),
        );
    }
    Ok(exposed)
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
    if !tail.is_empty() {
        error.detail = Some(Detail {
            text: tail.join("\n"),
            untrusted: true,
        });
    }
    error
}
