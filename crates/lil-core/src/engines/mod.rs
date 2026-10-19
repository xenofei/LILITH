//! The `engines` module (CORE-014; ARCH-063–065, ARCH-068, ARCH-119, CORE-108, CORE-131;
//! Appendix F "ComfyUI engine"): in plan 3 the core manages ComfyUI only (ARCH-065), as an
//! **owned** engine it started or an **attached** one someone else started.
//!
//! - **Owned.** [`Engines::start`] runs `<python> main.py --listen 127.0.0.1 --port <port>
//!   <extra_args>` in `<root>`, in a kill-on-close Job Object, with its output in the core's
//!   log (target `engine.comfyui`, ARCH-119) and a scrubbed environment
//!   ([`scrubbed_env`], ARCH-063). `extra_args` holding `--listen` is refused before anything
//!   starts. Once it answers `GET /system_stats`, every listening socket of every process in
//!   its Job Object must be on loopback, read through the core's socket table
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
//! The engine tools are in [`tools`] (registered by the server, since they need the running
//! engines). The `engines` topic carries `{kind: "status", engine}` and
//! `{kind: "restart_needed", engine}`; its snapshot is `engines.status`'s result.
//!
//! `jobs` and `telemetry` never use this module: the scheduler reaches the engines through
//! [`EngineControl`].

pub mod comfyui;
pub mod config;
pub mod tools;

use std::sync::{Arc, Weak};

use futures_util::future::BoxFuture;
use lil_platform::sockets::SocketTable;
use lil_platform::{Clock, Env};
use lil_protocol::{ErrorBody, codes};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::events::{EventBus, SnapshotProvider};
use crate::jobs::scheduler::EngineControl;
use crate::jobs::{EngineKind, Jobs};
use crate::lifecycle::{ShutdownPhase, ShutdownStep};
use crate::meter::UsageSink;
use crate::store::FailPoints;
use crate::telemetry::Telemetry;

pub use comfyui::handler::ComfyPromptHandler;
pub use comfyui::install::NodeInstallHandler;
pub use comfyui::supervisor::ComfyEndpoint;
pub use config::{ComfyConfig, scrubbed_env};

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

/// Prompts in ComfyUI that the suite didn't submit (Task 13 fills them in).
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
