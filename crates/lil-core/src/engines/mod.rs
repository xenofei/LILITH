//! The `engines` module (CORE-014; ARCH-063–065, ARCH-068, ARCH-119, CORE-108, CORE-131;
//! Appendix F "ComfyUI engine"): in plan 3 the core manages ComfyUI only (ARCH-065), as an
//! **owned** engine it started or an **attached** one someone else started.
//!
//! - **Owned.** [`Engines::start`] runs `<python> main.py --listen 127.0.0.1 --port <port>
//!   <profile flags> <extra_args> --disable-assets` in `<root>` (the launch profile,
//!   [`LaunchProfile`], ARCH-126; the last flag keeps ComfyUI's asset catalogue off,
//!   ARCH-065), in a kill-on-close Job Object, with its output in the core's log (target
//!   `engine.comfyui`, ARCH-119) and a scrubbed environment ([`scrubbed_env`], ARCH-063).
//!   `extra_args` holding `--listen`, `--port`, `--enable-assets` or
//!   `--windows-standalone-build` (or a prefix argparse takes for one) is refused before
//!   anything starts, as is a launch-profile value that isn't valid. Once it answers
//!   `GET /system_stats`, every listening socket of every process in its Job Object must be
//!   on loopback, read through the core's socket table
//!   (`CoreConfig::sockets`), or the core stops it and fails with `comfyui.not_loopback`
//!   (ARCH-068). An owned engine idle for `idle_free_after_s` with no suite job gets
//!   `/free {unload_models: true, free_memory: true}`; it is stopped in the shutdown's
//!   `StopEngines` phase and dies with the core (the Job Object, ARCH-064).
//! - **Attached.** Something the core didn't start answers `/system_stats` on
//!   `127.0.0.1:<port>`. Its processes are the one listening on the port and its descendants;
//!   its listening sockets are reported as `listen`. The suite never stops, restarts or idles
//!   it down (`engine.attached`).
//! - **The class is re-decided** whenever the engine stops answering: a supervisor task asks
//!   `/system_stats` every second.
//! - **Engine processes** ([`Engines::engine_pids`]) are a cached set the supervisor refreshes
//!   off every lock; the GPU scheduler reads them under the jobs queue's lock, so reading them
//!   never blocks or does I/O. Every one is tracked by the telemetry (role `comfyui`).
//! - **Node installs** ([`comfyui::install`]) run comfy-cli as a `cpu`-lane job; a success sets
//!   `restart_needed` and records one usage draft for the registry call (CORE-069).
//! - **Prompts** ([`comfyui::handler`]): `comfy.prompt` jobs in the `gpu-heavy` lane, one
//!   suite prompt in ComfyUI at a time; `engines.status`'s `suite_job` is the one there now.
//!   The `CancelEnginePrompts` shutdown step ([`Engines::cancel_prompts_step`]) cancels them
//!   through the per-job endpoint (ARCH-021).
//!
//! - **Observation** ([`Engines::observe`]): outside jobs ([`comfyui::outside`]) keep the core
//!   up and are waited for, never cancelled; each time ComfyUI becomes ready a `comfy.inventory`
//!   job ([`comfyui::inventory`]) captures the environment (kept in the suite `state.db`, the
//!   newest 10, [`comfyui::environment`]) and the model files with their families; before the
//!   POST, a prompt's graph is checked ([`comfyui::graph`]). The supervisor probes the asset
//!   catalogue (`GET /api/features`) once each time the engine becomes ready (ARCH-065).
//!
//! The engine tools are in [`tools`] (registered by the server, since they need the running
//! engines). The `engines` topic carries `{kind: "status", engine}`,
//! `{kind: "outside_job", engine}` (the outside counts changed) and
//! `{kind: "restart_needed", engine}`; its snapshot is `engines.status`'s result.
//!
//! `jobs` and `telemetry` never use this module: the scheduler reaches the engines through
//! [`EngineControl`].

pub mod comfyui;
pub mod config;
pub mod tools;

use std::sync::{Arc, Weak};

use futures_util::future::BoxFuture;
use lil_platform::fs::SystemProbe;
use lil_platform::sockets::SocketTable;
use lil_platform::{Clock, Env};
use lil_protocol::{ErrorBody, codes};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::events::{EventBus, SnapshotProvider};
use crate::jobs::scheduler::EngineControl;
use crate::jobs::{EngineKind, JobCaller, Jobs, SubmittedSlot};
use crate::lifecycle::{ShutdownPhase, ShutdownStep};
use crate::meter::UsageSink;
use crate::store::FailPoints;
use crate::telemetry::Telemetry;

pub use comfyui::handler::ComfyPromptHandler;
pub use comfyui::install::NodeInstallHandler;
pub use comfyui::inventory::InventoryHandler;
pub use comfyui::outside::OutsideWatch;
pub use comfyui::supervisor::ComfyEndpoint;
pub use config::{
    ComfyConfig, LaunchProfile, classify_pass_env, refuse_enable_assets, scrubbed_env,
};

/// The module the engine tools declare.
pub const ENGINES_MODULE: &str = "engines";
/// The topic.
pub const TOPIC: &str = "engines";
/// The telemetry role of ComfyUI's processes.
pub const COMFYUI_ROLE: &str = "comfyui";

/// Who started the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineClass {
    /// The core did.
    Owned,
    /// Someone else did.
    Attached,
    /// Nothing answers.
    None,
}

/// How the engine is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    Stopped,
    Starting,
    Ready,
    /// The core started it and it runs, but it doesn't answer.
    Unreachable,
}

/// Where the engine's processes listen (Appendix F).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Listen {
    Loopback,
    /// A socket on any address that isn't loopback.
    AllInterfaces,
    Unknown,
}

/// Prompts in ComfyUI that the suite didn't submit, as the last poll saw them
/// ([`comfyui::outside`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, JsonSchema)]
pub struct OutsideJobs {
    pub running: u32,
    pub queued: u32,
}

/// One engine as `engines.status` shows it (Appendix F "Product tools").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EngineStatus {
    pub engine: EngineKind,
    pub class: EngineClass,
    pub state: EngineState,
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    /// The launcher of an owned engine, the listener of an attached one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// From `/system_stats`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub listen: Listen,
    pub outside_jobs: OutsideJobs,
    /// The suite job in the engine now (Task 12).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite_job: Option<String>,
    /// A node pack was installed since the engine started.
    pub restart_needed: bool,
    /// `[engines.comfyui] root` is set: without it `models.report` is `engine.not_configured`.
    pub models_root_set: bool,
    /// The model inventory refreshed since the engine became ready ([`comfyui::inventory`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<InventoryStatus>,
    /// The launch profile of an owned engine, or the one an owned start would use when no
    /// engine runs; `null` for an attached engine or when ComfyUI isn't configured (ARCH-126;
    /// Appendix G "ComfyUI launch profile").
    pub launch_profile: Option<LaunchProfileStatus>,
    /// Whether ComfyUI's asset catalogue is on (ARCH-065), probed once each time the engine
    /// becomes ready; `unknown` until then.
    pub asset_catalogue: Catalogue,
}

/// `engines.status`'s `launch_profile`: the profile's keys and the flags they give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LaunchProfileStatus {
    pub reserve_vram_gb: String,
    pub preview_method: String,
    pub manager: bool,
    pub dynamic_vram: bool,
    pub flags: Vec<String>,
}

impl LaunchProfileStatus {
    /// What `engines.status` shows of `profile`.
    pub fn of(profile: &LaunchProfile) -> LaunchProfileStatus {
        LaunchProfileStatus {
            reserve_vram_gb: profile.reserve_vram_gb.clone(),
            preview_method: profile.preview_method.clone(),
            manager: profile.manager,
            dynamic_vram: profile.dynamic_vram,
            flags: profile.flags(),
        }
    }
}

/// Whether ComfyUI's asset catalogue is on, by `GET /api/features`'s `assets` boolean (ARCH-065;
/// Appendix G): `unknown` when the request fails, the route is missing or the key is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Catalogue {
    On,
    Off,
    Unknown,
}

/// `engines.status`'s `inventory`: how many model files, refreshed when (RFC 3339 UTC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InventoryStatus {
    pub models: u32,
    pub refreshed: String,
}

/// `engine.unknown` for an engine the core doesn't manage (no input reaches
/// `EngineKind::Test`: [`EngineKind::parse`] doesn't know it).
pub fn unknown_engine(name: &str) -> ErrorBody {
    ErrorBody::new(
        &codes::ENGINE_UNKNOWN,
        format!("{name:?} is not an engine the core manages; it manages \"comfyui\"."),
        "",
    )
}

/// The core's engines (module docs).
pub struct Engines {
    comfy: Arc<comfyui::supervisor::Comfy>,
}

impl std::fmt::Debug for Engines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engines").finish_non_exhaustive()
    }
}

impl Engines {
    /// The engines of a core: reads `[engines.comfyui]` from `env`'s `suite.toml` (a table that
    /// doesn't parse is logged and the engine counts as not configured), registers the node
    /// install handler with `jobs`, and starts the supervisor task, which ends when the
    /// engines are dropped or stopped by [`Engines::stop_engines_step`]. `sockets` is where the
    /// loopback check reads listening sockets (`CoreConfig::sockets`, ruling P3-R11). Call it
    /// inside a tokio runtime.
    #[allow(
        clippy::too_many_arguments,
        reason = "the plan's constructor, plus the core's socket table"
    )]
    pub fn new(
        env: Env,
        jobs: Arc<Jobs>,
        telemetry: Arc<Telemetry>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        failpoints: Arc<FailPoints>,
        usage: Arc<dyn UsageSink>,
        sockets: Arc<dyn SocketTable>,
    ) -> Arc<Engines> {
        // No engine fail point exists in plan 3a; the jobs' own (`JobCtx::failpoint`) cover the
        // engine jobs.
        let _ = failpoints;
        let comfy = comfyui::supervisor::Comfy::new(
            &env,
            jobs.clone(),
            telemetry,
            events.clone(),
            clock,
            usage,
            sockets,
        );
        let engines = Arc::new_cyclic(|weak: &Weak<Engines>| {
            jobs.register(Arc::new(NodeInstallHandler::new(weak.clone())));
            jobs.register(Arc::new(ComfyPromptHandler::new(weak.clone())));
            // A paused lane's pending prompt is withdrawn through ComfyUI (CORE-160).
            jobs.set_withdraw(
                EngineKind::ComfyUi,
                Arc::new(comfyui::handler::ComfyWithdraw::new(weak.clone())),
            );
            jobs.register(Arc::new(InventoryHandler::new(
                weak.clone(),
                Arc::new(SystemProbe),
            )));
            Engines { comfy }
        });
        comfyui::supervisor::Comfy::spawn_monitor(&engines.comfy);
        events.register_snapshot(
            TOPIC,
            Arc::new(Snapshots {
                engines: Arc::downgrade(&engines),
            }),
        );
        engines
    }

    /// Every engine's status (`engines.status`).
    pub async fn status(&self) -> Vec<EngineStatus> {
        vec![self.comfy.status()]
    }

    /// The engine's processes, from the cache (module docs): an owned engine's Job Object, an
    /// attached one's listener and its descendants; empty when it doesn't run. Never blocks.
    pub fn engine_pids(&self, e: EngineKind) -> Vec<u32> {
        match e {
            EngineKind::ComfyUi => self.comfy.pids(),
            EngineKind::Test => Vec::new(),
        }
    }

    /// Where a job reaches the engine: a running one (attached or owned), else an owned one
    /// started now when `autostart` allows it.
    ///
    /// # Errors
    /// `engine.not_configured`, `comfyui.unreachable` (not running, no autostart), or the
    /// start's errors.
    pub async fn ensure_ready(&self, e: EngineKind) -> Result<ComfyEndpoint, ErrorBody> {
        match e {
            EngineKind::ComfyUi => self.comfy.ensure_ready().await,
            EngineKind::Test => Err(unknown_engine(e.wire())),
        }
    }

    /// Starts an owned engine; an engine that already runs (owned or attached) is returned as
    /// it is.
    ///
    /// # Errors
    /// `engine.not_configured`, `comfyui.not_loopback`, `comfyui.start_failed`.
    pub async fn start(&self, e: EngineKind) -> Result<EngineStatus, ErrorBody> {
        match e {
            EngineKind::ComfyUi => self.comfy.start().await,
            EngineKind::Test => Err(unknown_engine(e.wire())),
        }
    }

    /// Stops an owned engine (every process in its Job Object); a stopped one is returned as it
    /// is.
    ///
    /// # Errors
    /// `engine.attached`: the suite never stops an engine it didn't start.
    pub async fn stop(&self, e: EngineKind) -> Result<EngineStatus, ErrorBody> {
        match e {
            EngineKind::ComfyUi => self.comfy.stop().await,
            EngineKind::Test => Err(unknown_engine(e.wire())),
        }
    }

    /// Stops an owned engine and starts it again (clearing `restart_needed`).
    ///
    /// # Errors
    /// `engine.attached`, and the start's errors.
    pub async fn restart(&self, e: EngineKind) -> Result<EngineStatus, ErrorBody> {
        match e {
            EngineKind::ComfyUi => self.comfy.restart().await,
            EngineKind::Test => Err(unknown_engine(e.wire())),
        }
    }

    /// Asks the engine to free its VRAM (the scheduler's unload, [`EngineControl::unload`]);
    /// `true` when it confirmed. It ends within `comfyui::supervisor::UNLOAD_TIMEOUT` whatever
    /// the engine does: an unload that doesn't answer counts as failed.
    pub async fn unload(&self, e: EngineKind) -> bool {
        match e {
            EngineKind::ComfyUi => self.comfy.unload().await,
            EngineKind::Test => false,
        }
    }

    /// Marks the engine as needing a restart (a node pack was installed) and publishes it.
    pub fn set_restart_needed(&self, e: EngineKind) {
        if e == EngineKind::ComfyUi {
            self.comfy.set_restart_needed();
        }
    }

    /// One supervision pass now, as the supervisor task makes every second: the class and
    /// state, the engine processes, the idle free. Tests call it to step the idle time with a
    /// fake clock.
    #[doc(hidden)]
    pub async fn tick(&self) {
        self.comfy.tick().await;
    }

    /// One poll of outside jobs now, as [`Engines::observe`] makes every
    /// [`comfyui::outside::POLL_EVERY`] while ComfyUI is ready, then a kick of the jobs.
    #[doc(hidden)]
    pub async fn poll_outside(&self) {
        self.comfy.poll_outside().await;
        if let Some(jobs) = self.comfy.jobs() {
            jobs.kick();
        }
    }

    /// At the core's start, before any job runs: drops the ComfyUI upload references a dead
    /// core left behind (they would keep those uploads forever).
    pub async fn startup(&self) {
        self.comfy.reset_uploads().await;
    }

    /// The `CancelEnginePrompts` shutdown step (ARCH-021 step 2): every suite prompt in an
    /// engine gets its per-job cancel (never `/interrupt`); prompts from outside the suite are
    /// never touched.
    pub fn cancel_prompts_step(self: &Arc<Self>) -> Arc<dyn ShutdownStep> {
        Arc::new(CancelEnginePrompts(self.clone()))
    }

    /// The `StopEngines` shutdown step (ARCH-021 step 4): stops the supervisor and an owned
    /// engine; an attached one is left alone.
    pub fn stop_engines_step(self: &Arc<Self>) -> Arc<dyn ShutdownStep> {
        Arc::new(StopEngines(self.clone()))
    }

    /// The `[engines.comfyui]` the core read, when it has one.
    pub fn comfy_config(&self) -> Option<&ComfyConfig> {
        self.comfy.config()
    }

    pub(crate) fn comfy(&self) -> &Arc<comfyui::supervisor::Comfy> {
        &self.comfy
    }

    /// The outside jobs: the core's idle blocker for them (ARCH-020).
    pub fn outside_watch(&self) -> Arc<OutsideWatch> {
        self.comfy.outside().clone()
    }

    /// Starts observing ComfyUI (ARCH-069; [`comfyui::outside`]): each time it becomes ready, a
    /// `comfy.inventory` job is submitted; while it stays ready, outside jobs are polled every
    /// [`comfyui::outside::POLL_EVERY`] and the jobs are kicked after each poll, so a job that
    /// waits for them is looked at again. Ends with the engines or the `StopEngines` step.
    /// The core calls it once it serves; call it inside a tokio runtime.
    pub fn observe(self: &Arc<Self>) {
        if self.comfy.client().is_none() {
            return;
        }
        let weak = Arc::downgrade(self);
        let mut ready = self.comfy.ready_rx();
        let stopped = self.comfy.stopped();
        tokio::spawn(async move {
            let mut inventoried = 0u64;
            let mut open: Option<String> = None;
            loop {
                let (epoch, is_ready) = *ready.borrow_and_update();
                if is_ready {
                    let Some(engines) = weak.upgrade() else {
                        return;
                    };
                    // One inventory at a time (review M5): an engine that came back while one
                    // waits or runs gets its own once that one has ended (it inventories the
                    // epoch it starts in).
                    let waiting = match &open {
                        Some(id) => !engines.job_ended(id).await,
                        None => false,
                    };
                    if !waiting && epoch != inventoried {
                        inventoried = epoch;
                        open = engines.submit_inventory(epoch).await;
                    }
                    engines.poll_outside().await;
                }
                let pause = async {
                    if is_ready {
                        tokio::time::sleep(comfyui::outside::POLL_EVERY).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                };
                tokio::select! {
                    () = pause => {}
                    changed = ready.changed() => if changed.is_err() { return },
                    () = stopped.cancelled() => return,
                }
            }
        });
    }

    /// Submits the `comfy.inventory` job of readiness `epoch`: the core's own job, with no
    /// tool call, so `cli` (`agent`) in the suite's scope (Appendix F "Jobs").
    async fn submit_inventory(&self, epoch: u64) -> Option<String> {
        let jobs = self.comfy.jobs()?;
        match jobs
            .submit(
                &core_caller(&jobs),
                comfyui::inventory::inventory_spec(epoch),
            )
            .await
        {
            Ok(job) => {
                tracing::info!(job_id = %job.job_id, "ComfyUI is ready; its model inventory is refreshed");
                Some(job.job_id)
            }
            Err(e) => {
                tracing::warn!(code = %e.code, error = %e.message, "the model inventory job could not be submitted");
                None
            }
        }
    }

    /// Whether the core's job `id` has ended (or can't be read any more).
    async fn job_ended(&self, id: &str) -> bool {
        let Some(jobs) = self.comfy.jobs() else {
            return true;
        };
        jobs.get(&core_caller(&jobs), &crate::jobs::JobId(id.to_string()))
            .await
            .map_or(true, |j| j.status.is_final())
    }
}

/// The [`JobCaller`] of a job the core submits itself, with no tool call: `cli` (so `agent`)
/// in the suite's scope, with fresh call and trace ids (Appendix F "Jobs").
fn core_caller(jobs: &Jobs) -> JobCaller {
    JobCaller {
        caller: lil_protocol::CallerKind::Cli,
        idem_scope: "cli|none".to_string(),
        workspace: None,
        trace_id: jobs.mint_id("t"),
        idem_key: None,
        call_id: jobs.mint_id("c"),
        reservation: None,
        submitted: Arc::new(SubmittedSlot::new()),
        origin: None,
        retry_of: None,
    }
}

impl EngineControl for Engines {
    fn pids(&self, e: EngineKind) -> Vec<u32> {
        self.engine_pids(e)
    }

    fn unload<'a>(&'a self, e: EngineKind) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            Engines::unload(self, e).await;
        })
    }

    fn outside_busy(&self, e: EngineKind) -> bool {
        e == EngineKind::ComfyUi && self.comfy.outside().any()
    }
}

/// The `CancelEnginePrompts` step.
struct CancelEnginePrompts(Arc<Engines>);

impl ShutdownStep for CancelEnginePrompts {
    fn phase(&self) -> ShutdownPhase {
        ShutdownPhase::CancelEnginePrompts
    }

    fn name(&self) -> &'static str {
        "engines.cancel_prompts"
    }

    fn run(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.0.comfy.cancel_prompts().await })
    }
}

/// The `StopEngines` step.
struct StopEngines(Arc<Engines>);

impl ShutdownStep for StopEngines {
    fn phase(&self) -> ShutdownPhase {
        ShutdownPhase::StopEngines
    }

    fn name(&self) -> &'static str {
        "engines.stop"
    }

    fn run(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.0.comfy.shutdown().await })
    }
}

/// The `engines` topic's snapshot: `{engines: [EngineStatus]}`.
struct Snapshots {
    engines: Weak<Engines>,
}

impl SnapshotProvider for Snapshots {
    fn snapshot<'a>(
        &'a self,
        _topic: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            match self.engines.upgrade() {
                Some(engines) => json!({ "engines": engines.status().await }),
                None => json!({"error": "the core is shutting down"}),
            }
        })
    }
}
