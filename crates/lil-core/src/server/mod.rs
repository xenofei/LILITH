//! The running core (infrastructure, not a CORE-014 module): it builds every module, serves
//! them through the [`crate::transport`] (the named-pipe listener, ARCH-024–026, ARCH-033, whose
//! items are re-exported here), and shuts down.
//!
//! - [`RunningCore`] shuts down once the [`Lifecycle`] drains (ARCH-021): the lifecycle's
//!   wait and steps, then the open workspaces close (their `state.db` first, then
//!   `core.lock`), then the state databases close, then the pipe.
//! - The jobs ([`crate::jobs`]) are the dispatcher's: before it serves, the core registers
//!   their handlers (the test channels' `test.job`), reads `[jobs] preempt_agent_batches`,
//!   stops them starting queued jobs once a drain begins, recovers the suite scope's jobs
//!   (ARCH-022; a job that never reached an engine is held instead, P4a-R41), checks the stall
//!   rule every 5 s (ARCH-072), and adds the `jobs` idle blocker (ARCH-020) and the
//!   `PersistInterrupted` shutdown step (ARCH-021). The default workspace's jobs are
//!   recovered once it opens. The GPU scheduler ([`crate::jobs::scheduler`]) is their
//!   admission, and every telemetry sample kicks them: a job waiting for VRAM is asked about
//!   again, and the watchdog looks at the running ones.
//! - The hardware telemetry ([`crate::telemetry`]) starts before the dispatcher is built: this
//!   machine's hardware sampled once a second, or on a test channel the fake hardware script
//!   (`LIL_TEST_FAKE_HW`) advanced only by `test.hw_tick`. Its tools and its topic's snapshot
//!   are registered, the default workspace's hardware profile is checked once the workspace is
//!   open and the first sample is in (CORE-067), and sampling stops right after the shutdown
//!   steps. Its history gives each job its estimate at submission (the jobs' estimator,
//!   CORE-161), and each verified app's process is tracked with role `app` while it is
//!   connected (`AppTracker`, P4a-R27).
//! - The confirmations ([`crate::confirm`]) are stage 5's confirm half ([`AgentConfirm`]), the
//!   router's `app/*` methods and an idle blocker (ARCH-020; never a drain's). The suite
//!   database's pending ones are loaded at start, the default workspace's once it opens, and
//!   expired ones are marked every second.
//! - The meter ([`crate::meter`]) is stage 8 ([`MeterSettle`]), the engines' usage sink, and the
//!   jobs' first completion; it registers `usage.summary` (and `test.meter_external` on test
//!   channels), and turns the usage rows a dead core left into `pending` lines: the suite's
//!   before the core serves, the default workspace's once it opens.
//! - The caps ([`crate::meter::Caps`]) are stage 5's cap half (through [`Confirm`]), the reserve
//!   stage, the settle after stage 8's usage lines, the jobs' second completion and their
//!   reservation hand-over, `caps.get` and the router's `app/caps.set`. A dead core's held
//!   reservations are repaired before the meter's recovery of the same scope: the suite's
//!   before the core serves, the default workspace's once it opens.
//! - The engines ([`crate::engines`]) are built once the dispatcher's jobs exist (their tools
//!   find them through a slot filled before the core serves). They register the node-install
//!   handler, are the GPU scheduler's engine processes and unload, have ComfyUI's models folder
//!   measured with the workspace, and stop an owned ComfyUI in the `StopEngines` shutdown step.
//!   Their outside-jobs watch is an idle blocker (it holds no drain), and their observation
//!   ([`Engines::observe`]: the outside-jobs poll every 2 s while ComfyUI is ready, the model
//!   inventory each time it becomes ready) starts with the suite's jobs recovered.
//! - The desktop ([`crate::desktop`], P4a-R24): `real_desktop` on the stable and dev channels,
//!   a `FakeDesktop` on test channels (which `test.desktop` sets). Its settings are read from
//!   `suite.toml` before the dispatcher is built (`desktop.get`, and the router's
//!   `app/desktop.set`); once the jobs exist, the fullscreen poll starts (every 2 s, or
//!   `LIL_TEST_DESKTOP_POLL_MS` on a test channel), with the auto-pause and keep-awake, and
//!   the session-end window's sleep broadcasts go to the poll's sleep log. Keep-awake releases
//!   its request when the shutdown ends. A test channel's fake desktop starts with the
//!   handlers `missing`, as before the notification setup, so its notification path can't
//!   reach you until a test sets it up (`test.desktop`).
//! - The notifier ([`crate::interchange::Notifier`], APP-146) starts with the fullscreen poll,
//!   before the dispatcher is built: it is the confirmations' notification path (ARCH-055)
//!   and posts `notify.claude_code`'s and an agent's `open_in`'s toasts, and it reads the
//!   jobs' batches once they exist. Each recovery that holds jobs (P4a-R41), at start or of a
//!   workspace first used later, posts one toast for them (the jobs' restart-hold sink). After
//!   each confirmation load (the suite's at start, the default workspace's once it opens) the
//!   notifier reconciles the confirmation toasts with the pending confirmations.
//! - The diagnostics ([`crate::diagnostics`], P4a-R24) are built with the telemetry and the
//!   engines' slot before the dispatcher; they are its error sink (each error outcome is a
//!   recent error) and a router observer (the apps' build ids for the bundle).

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use lil_platform::desktop::{Desktop, HandlerState, real_desktop};
use lil_platform::desktop_fake::FakeDesktop;
use lil_platform::session_end::{PowerEvent, SessionEnd, SessionEndSim, SessionEndWatcher};
use lil_platform::{Env, process};
use lil_protocol::CallerKind;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{CoreConfig, CoreExit, ParentWatchFault, ParentWatchStep};
use crate::confirm::{AgentConfirm, Confirm, MCP_CONFIRMATION, NotificationPath, UiPresence};
use crate::desktop::{
    AutoPause, DesktopSettingsStore, DesktopWatch, KeepAwake, UiApps, start_autopause,
    start_keep_awake,
};
use crate::diagnostics::{Diagnostics, DiagnosticsSources, register_diagnostics_tools};
use crate::docs::{Holds, TestHolds, register_doc_tools, register_test_hold};
use crate::engines::Engines;
use crate::engines::tools::{EnginesSlot, register_engine_test_tools, register_engine_tools};
use crate::events::EventBus;
use crate::interchange::{Interchange, Notifier, NotifierParts, register_interchange_tools};
use crate::jobs::Estimator;
use crate::jobs::scheduler::GpuScheduler;
use crate::lifecycle::{DrainReason, IDLE_TICK, Lifecycle, ShutdownReport};
use crate::meter::{Caps, Meter, MeterSettle, register_caps_tools, register_meter_tools};
use crate::projects::ActiveProjects;
use crate::resources::{Resources, register_resource_tools, spawn_personal_maintenance};
use crate::rpc::RpcRouter;
use crate::session::Session;
use crate::state::{OnFatal, StateError, StateRegistry};
use crate::store::Personal;
use crate::telemetry::{self, HwSources, PowerSettings, Telemetry, TickMode};
use crate::tools::{
    Dispatcher, PipelineHooks, TestGates, TestJobHandler, ToolEstimates, core_registry,
    register_desktop_tools, register_hw_tick, register_telemetry_tools, register_test_desktop,
};
use crate::transport::serve_with;
pub use crate::transport::{
    ConnCtx, ConnObserver, Listener, MAX_IN_FLIGHT_PER_CONN, OUTBOUND_CAPACITY, Router,
    ServerStats, claim_pipe, serve,
};
use crate::workspace::WorkspaceHub;

/// A core that is serving.
///
/// Its shutdown (ARCH-021) runs in a supervisor task once a drain begins: the lifecycle's
/// wait and shutdown steps ([`Lifecycle::run_shutdown`]), then the state databases are closed
/// (the queued jobs run, the actors stop), and only then is the pipe closed. A new core can
/// claim the pipe only after this one has stopped writing `state.db`, so its clean-up of
/// stale `running` idempotency rows never races calls of this one.
#[derive(Debug)]
pub struct RunningCore {
    pipe: String,
    lifecycle: Arc<Lifecycle>,
    state: Arc<StateRegistry>,
    /// The workspaces this core serves (ARCH-028).
    hub: Arc<WorkspaceHub>,
    /// The event bus (ARCH-032).
    events: Arc<EventBus>,
    /// The hardware telemetry (CORE-059).
    telemetry: Arc<Telemetry>,
    /// The gates `test.block` waits on (test channels).
    test_gates: Arc<TestGates>,
    /// Each session's active project (ARCH-088), shared with the dispatcher.
    active_projects: Arc<ActiveProjects>,
    /// The confirmations (ARCH-052–055).
    confirm: Arc<Confirm>,
    /// The hidden session-end window; `None` if it could not be created.
    session_end: Option<Arc<SessionEndWatcher>>,
    /// The fullscreen poll and the sleep log (P4a-R6, ARCH-127).
    desktop_watch: Arc<DesktopWatch>,
    /// The desktop settings (`desktop.get`, `app/desktop.set`).
    desktop_settings: Arc<DesktopSettingsStore>,
    /// The verified apps' processes, while they are connected (P4a-R7).
    ui_apps: Arc<UiApps>,
    /// The toasts and the confirmations' notification path (APP-146).
    notifier: Arc<Notifier>,
    /// The supervisor: ends with the shutdown's report.
    shutdown: JoinHandle<ShutdownReport>,
}

impl RunningCore {
    /// The pipe the core serves.
    pub fn pipe_name(&self) -> &str {
        &self.pipe
    }

    /// The core's databases: the suite `state.db` and the workspace ones.
    pub fn state(&self) -> &Arc<StateRegistry> {
        &self.state
    }

    /// The event bus.
    pub fn events(&self) -> &Arc<EventBus> {
        &self.events
    }

    /// The hardware telemetry.
    pub fn telemetry(&self) -> &Arc<Telemetry> {
        &self.telemetry
    }

    /// The workspace hub.
    pub fn hub(&self) -> &Arc<WorkspaceHub> {
        &self.hub
    }

    /// The gates of the test-channel tool `test.block`: in-process tests open them.
    pub fn test_gates(&self) -> &TestGates {
        &self.test_gates
    }

    /// Each session's active project (ARCH-088): set by calls that name a project, forgotten
    /// when the session's connection closes.
    pub fn active_projects(&self) -> &Arc<ActiveProjects> {
        &self.active_projects
    }

    /// The confirmations: pending ones, answers, the apps that can answer.
    pub fn confirm(&self) -> &Arc<Confirm> {
        &self.confirm
    }

    /// The fullscreen poll and the sleep log.
    pub fn desktop_watch(&self) -> &Arc<DesktopWatch> {
        &self.desktop_watch
    }

    /// The desktop settings.
    pub fn desktop_settings(&self) -> &Arc<DesktopSettingsStore> {
        &self.desktop_settings
    }

    /// The verified apps' processes, while they are connected.
    pub fn ui_apps(&self) -> &Arc<UiApps> {
        &self.ui_apps
    }

    /// The notifier (APP-146).
    pub fn notifier(&self) -> &Arc<Notifier> {
        &self.notifier
    }

    /// Idle accounting, drains and shutdown steps.
    pub fn lifecycle(&self) -> &Arc<Lifecycle> {
        &self.lifecycle
    }

    /// Asks the core to exit when safe (`core/drain`, ARCH-023).
    pub fn request_drain(&self) {
        self.lifecycle.begin_drain(DrainReason::Requested);
    }

    /// Sends `msg` to the core's own session-end window, as Windows would (tests). `End`
    /// blocks until the shutdown is complete or [`crate::lifecycle::SESSION_END_BUDGET`] is
    /// spent, so call it from outside the core's runtime. `false` if the core has no window.
    pub fn simulate_session_end(&self, msg: SessionEndSim) -> bool {
        self.session_end.as_ref().is_some_and(|w| w.simulate(msg))
    }

    /// Waits until the core has shut down. The report's `reason` says why (ruling P10) and
    /// its `exit_code` is the process exit code; its `steps` end with `state.close` and
    /// `pipe.close`.
    pub async fn wait(self) -> ShutdownReport {
        match self.shutdown.await {
            Ok(report) => report,
            Err(e) => {
                tracing::error!(error = %e, "the shutdown supervisor ended abnormally");
                ShutdownReport {
                    reason: DrainReason::Fatal(format!("the shutdown supervisor ended: {e}")),
                    steps: Vec::new(),
                    abandoned_calls: 0,
                }
            }
        }
    }
}

/// Claims the channel's pipe, then serves it. Must be called inside a tokio runtime.
pub async fn start(cfg: CoreConfig) -> Result<RunningCore, CoreExit> {
    let listener = claim_pipe(&cfg.env.pipe_name())?;
    start_claimed(cfg, listener).await
}

/// [`start`] with the pipe already claimed: the binary claims it before it opens its log
/// (a core that loses the claim touches nothing).
///
/// Opens the suite `state.db` before serving (only the core that owns the pipe opens it, so
/// it may release the `running` rows of an earlier core). A newer database is
/// [`CoreExit::StateTooNew`], a damaged one [`CoreExit::StateDamaged`], one with a hot
/// rollback journal [`CoreExit::StateHotJournal`], anything else [`CoreExit::Startup`]; each
/// is logged with the path, nothing on disk is changed, and the pipe is released as this
/// returns.
///
/// Then it serves, opens the default workspace in the background (a failure is logged; the
/// first call in it tries again), ticks the idle check every [`IDLE_TICK`] (or `idle_after`,
/// if a test channel set it shorter), watches the session end and, on a test channel, the
/// test parent. Each of these begins a fatal drain, after which the core exits 4
/// ([`ShutdownReport::exit_code`]): a state actor that dies (QA-012); a pipe instance that
/// cannot be created (every connection needs a new one, made before the connected instance is
/// handed over; the first is the claimed one); a test parent that is running but cannot be
/// watched (opening it fails other than with `NotFound`, or waiting for it fails).
pub async fn start_claimed(cfg: CoreConfig, listener: Listener) -> Result<RunningCore, CoreExit> {
    let pipe = listener.name().to_string();
    let lifecycle = Lifecycle::new(cfg.clock.clone(), cfg.idle_after);
    let on_fatal: OnFatal = {
        let lifecycle = lifecycle.clone();
        Arc::new(move |what: &str| {
            tracing::error!(what, "fatal error; the core stops");
            lifecycle.begin_drain(DrainReason::Fatal(what.to_string()));
        })
    };
    let state = match open_suite_state(&cfg.env, on_fatal) {
        Ok(state) => Arc::new(state),
        Err(exit) => {
            drop(listener);
            return Err(exit);
        }
    };
    let test_gates = Arc::new(TestGates::new());
    let events = Arc::new(EventBus::new(cfg.clock.clone()));
    let hub = WorkspaceHub::new(
        cfg.env.clone(),
        state.clone(),
        events.clone(),
        cfg.clock.clone(),
        cfg.ids.clone(),
        cfg.failpoints.clone(),
        cfg.probe.clone(),
        cfg.watch.clone(),
    );
    // The live topics' snapshots (ARCH-044): `asset:` and `project:`.
    crate::tools::register_snapshots(&events, &hub);
    let telemetry = start_telemetry(&cfg, &events).await;
    telemetry::register_snapshot(&events, &telemetry);
    let mut registry = core_registry(
        cfg.test_tools,
        &test_gates,
        &state,
        &hub,
        &cfg.env,
        &cfg.probe,
        Arc::new(cfg.git_env.clone()),
    );
    register_telemetry_tools(&mut registry, &telemetry);
    // The meter (CORE-069): usage records, stage 8, the jobs' energy and local work.
    let meter = Meter::new(
        state.clone(),
        cfg.env.clone(),
        cfg.clock.clone(),
        cfg.ids.clone(),
        events.clone(),
        telemetry.clone(),
        power_settings(&cfg.env),
        cfg.failpoints.clone(),
    );
    register_meter_tools(&mut registry, &meter, cfg.test_tools);
    // The caps (CORE-085–092): stage 5's cap half, the reserve stage, the settles, `caps.get`
    // and `app/caps.set`.
    let caps = Caps::new(
        state.clone(),
        cfg.env.clone(),
        cfg.clock.clone(),
        events.clone(),
        cfg.failpoints.clone(),
        cfg.ids.clone(),
    );
    register_caps_tools(&mut registry, &caps);
    // The holds and the handler registry follow the connections (see `ConnObserver`).
    let holds = Holds::new(cfg.clock.clone());
    register_doc_tools(&mut registry, &holds);
    let test_holds = Arc::new(TestHolds::default());
    if cfg.test_tools {
        register_test_hold(&mut registry, &holds, &test_holds);
    }
    let interchange = Interchange::new(core_dir(), cfg.env.clone());
    register_interchange_tools(&mut registry, &interchange);
    if cfg.test_tools {
        register_hw_tick(&mut registry, telemetry.clone());
    }
    // The engines need the jobs, which the dispatcher builds: their tools find them here.
    let engine_slot = EnginesSlot::default();
    register_engine_tools(&mut registry, &engine_slot);
    if cfg.test_tools {
        register_engine_test_tools(&mut registry);
    }
    // The diagnostics (P4a-R24): recent errors from the dispatcher, the bundle, and the
    // versions of the apps that say hello.
    let diagnostics = Diagnostics::new(
        cfg.env.clone(),
        cfg.clock.clone(),
        events.clone(),
        DiagnosticsSources {
            telemetry: Some(telemetry.clone()),
            engines: Some(engine_slot.clone()),
        },
    );
    register_diagnostics_tools(&mut registry, &diagnostics);
    // The desktop (P4a-R24): the real one on stable and dev, the fake on test channels, so no
    // test reads the real desktop or holds a real power request (QA-038).
    let fake_desktop = cfg.test_tools.then(|| {
        let fake = Arc::new(FakeDesktop::new());
        // As before the notification setup: no toast can reach you until a test sets it up.
        fake.set_handlers(HandlerState::Missing);
        fake
    });
    let desktop: Arc<dyn Desktop> = match &fake_desktop {
        Some(fake) => fake.clone(),
        None => real_desktop(&cfg.env),
    };
    let desktop_settings = DesktopSettingsStore::load(
        cfg.env.clone(),
        desktop.clone(),
        // The folder identity trusts apps from: the installed one on stable.
        cfg.env
            .install_dir()
            .unwrap_or_else(core_dir)
            .join(crate::identity::APP_EXES[0].0),
        cfg.clock.clone(),
        cfg.ids.clone(),
        events.clone(),
        cfg.failpoints.clone(),
    );
    register_desktop_tools(&mut registry, &desktop_settings);
    let ui_apps = Arc::new(UiApps::default());
    if let Some(fake) = &fake_desktop {
        register_test_desktop(
            &mut registry,
            fake.clone(),
            ui_apps.clone(),
            desktop_settings.clone(),
        );
    }
    // The fullscreen poll (CORE-160, APP-146), shared by the auto-pause and the quiet mode.
    let desktop_watch = DesktopWatch::start(
        desktop.clone(),
        cfg.clock.clone(),
        cfg.desktop_poll.unwrap_or(crate::desktop::POLL_EVERY),
        events.clone(),
    );
    // The notifier (APP-146): the toasts, and the confirmations' notification path.
    let notifier = Notifier::start(NotifierParts {
        desktop: desktop.clone(),
        watch: desktop_watch.clone(),
        settings: desktop_settings.clone(),
        events: events.clone(),
        apps: ui_apps.clone(),
        env: cfg.env.clone(),
        clock: cfg.clock.clone(),
        ids: cfg.ids.clone(),
    })
    .await;
    interchange.set_notifier(notifier.clone());
    // The confirmations (ARCH-052–055): stage 5's confirm half, answered by the apps, or
    // waiting for you through the notifier's toasts.
    let path: Arc<dyn NotificationPath> = notifier.clone();
    let confirm = Confirm::new(
        state.clone(),
        events.clone(),
        cfg.clock.clone(),
        cfg.ids.clone(),
        UiPresence::new(),
        path,
        cfg.confirm_lifetime,
        MCP_CONFIRMATION,
        cfg.failpoints.clone(),
    );
    confirm.set_caps(caps.clone());
    notifier.set_confirm(&confirm);
    match confirm.load(state.suite()).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(count = n, "pending confirmations of an earlier core run"),
        Err(e) => {
            tracing::error!(error = %e, "the pending confirmations could not be read; a retry with its token finds each")
        }
    }
    confirm.reconcile_toasts();
    let display = {
        let caps = caps.clone();
        Arc::new(move || caps.display_currency())
    };
    let hooks = PipelineHooks {
        estimate: Arc::new(ToolEstimates::new(display)),
        confirm: AgentConfirm::new(confirm.clone()),
        reserve: caps.clone(),
        meter: Arc::new(MeterSettle::new(meter.clone()).with_caps(caps.clone())),
        ..PipelineHooks::plan2()
    };
    // The Libraries: the personal scope's folder and its trash, purged in the background.
    let personal = Arc::new(Personal::new(
        cfg.env.data_dir().join("resources"),
        cfg.clock.clone(),
        cfg.ids.clone(),
        cfg.probe.clone(),
    ));
    register_resource_tools(
        &mut registry,
        &Arc::new(Resources::new(personal.clone(), cfg.clock.clone())),
    );
    spawn_personal_maintenance(personal);
    let dispatcher = Dispatcher::new(
        registry,
        hooks,
        state.clone(),
        events.clone(),
        hub.clone(),
        cfg.ids.clone(),
        cfg.clock.clone(),
        lifecycle.clone(),
    );
    dispatcher.set_error_sink(diagnostics.clone());
    let active_projects = dispatcher.active_projects().clone();
    let jobs = dispatcher.jobs().clone();
    notifier.set_jobs(&jobs).await;
    // Whether a suite window is in front as each job ends, before its final row (P4a-R7).
    {
        let notifier = Arc::downgrade(&notifier);
        jobs.set_end_hook(Arc::new(move |id: &crate::jobs::JobId| {
            if let Some(notifier) = notifier.upgrade() {
                notifier.job_ending(id);
            }
        }));
    }
    // One restart-hold toast per recovery that holds jobs (P4a-R41), set before any recovery.
    {
        let notifier = notifier.clone();
        jobs.set_restart_hold_sink(Arc::new(move |held: Vec<crate::jobs::JobId>| {
            let notifier = notifier.clone();
            tokio::spawn(async move {
                if let Some(first) = held.first() {
                    let outcome = notifier.restart_held(held.len(), first).await;
                    tracing::info!(held = held.len(), outcome = ?outcome, "the restart hold's toast");
                }
            });
        }));
    }
    // The estimate each job takes at submission, from the telemetry's history (CORE-161).
    let display = {
        let caps = caps.clone();
        Arc::new(move || caps.display_currency())
    };
    jobs.set_estimator(Estimator::new(
        telemetry.clone(),
        &power_settings(&cfg.env),
        display,
    ));
    if cfg.test_tools {
        jobs.register(Arc::new(TestJobHandler::new(test_gates.clone())));
    }
    jobs.set_preempt_agent_batches(preempt_agent_batches(&cfg.env));
    // The stall rule (ARCH-072): checked every 5 s while the core serves.
    jobs.set_stall_after(cfg.stall_after);
    jobs.spawn_stall_check(lifecycle.drain_token());
    // The engines (ARCH-064): they register the node-install handler, and are the scheduler's
    // engine processes and unload (ARCH-071).
    let engines = Engines::new(
        cfg.env.clone(),
        jobs.clone(),
        telemetry.clone(),
        events.clone(),
        cfg.clock.clone(),
        cfg.failpoints.clone(),
        meter.clone(),
        cfg.sockets.clone(),
    );
    let _ = engine_slot.set(engines.clone());
    engines.startup().await;
    jobs.set_admission(GpuScheduler::new(telemetry.clone(), engines.clone()));
    // ComfyUI's models folder is measured with the workspace (CORE-061).
    let models = engines
        .comfy_config()
        .and_then(|c| c.root.as_ref())
        .map(|root| root.join("models"));
    if models.is_some() {
        telemetry.set_sizes_probe(telemetry::sizes_probe(None, models.clone()));
    }
    let kicked = Arc::downgrade(&jobs);
    telemetry.on_sample(Box::new(move || {
        if let Some(jobs) = kicked.upgrade() {
            jobs.kick();
        }
    }));
    jobs.stop_on_drain(lifecycle.drain_token());
    // Completions (Meter, then Caps) and the reservation hand-over are set before the recovery:
    // it runs the completions of jobs that never started.
    jobs.add_completion(meter.clone());
    jobs.add_completion(caps.clone());
    jobs.set_reservations(caps.clone());
    // A dead core's held reservations, decided by its calls' usage rows before the meter's
    // recovery removes them (CORE-088); then the usage rows a dead core left become `pending`
    // lines (CORE-071), but only after a successful repair, so a failed repair finds the rows
    // again at the next start (ruling P3-R33).
    let start = caps.repair_then_recover(&meter).await;
    match start.repaired {
        Ok(0) => {}
        Ok(n) => tracing::warn!(
            count = n,
            "cap reservations of an earlier core run were repaired"
        ),
        Err(e) => {
            tracing::error!(code = %e.code, error = %e.message, "the cap reservations could not be repaired; they stay held, and the suite's usage rows stay, until the next start")
        }
    }
    match start.recovered {
        None | Some(Ok(0)) => {}
        Some(Ok(n)) => tracing::warn!(
            count = n,
            "usage records of an earlier core run were written"
        ),
        Some(Err(e)) => {
            tracing::error!(code = %e.code, error = %e.message, "the suite's pending usage rows could not be recovered; the next start tries again")
        }
    }
    match jobs.recover_suite().await {
        Ok(marked) if !marked.is_empty() => tracing::warn!(
            count = marked.len(),
            "suite jobs of an earlier core run are interrupted; none runs again"
        ),
        Ok(_) => {}
        Err(e) => {
            tracing::error!(code = %e.code, error = %e.message, "the suite jobs could not be recovered; the first job call tries again")
        }
    }
    // A dead core's held job reservations, decided by their jobs once those are recovered.
    match caps.repair_jobs().await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(
            count = n,
            "job cap reservations of an earlier core run were repaired"
        ),
        Err(e) => {
            tracing::error!(code = %e.code, error = %e.message, "the job cap reservations could not be repaired; they stay held until the next start")
        }
    }
    lifecycle.add_blocker(jobs.clone());
    // The auto-pause and keep-awake (CORE-160, ARCH-127).
    let autopause = start_autopause(&desktop_watch, jobs.clone(), desktop_settings.current());
    let keep_awake = start_keep_awake(desktop, jobs.clone(), desktop_settings.current());
    // Outside ComfyUI jobs keep the core up but hold no drain (ARCH-020, ARCH-069); the poller
    // runs while the engine is ready.
    let outside = engines.outside_watch();
    outside.stop_on_drain(lifecycle.drain_token());
    lifecycle.add_blocker(outside);
    engines.observe();
    lifecycle.add_blocker(confirm.clone());
    lifecycle.add_shutdown_step(engines.cancel_prompts_step());
    lifecycle.add_shutdown_step(jobs.persist_interrupted_step());
    lifecycle.add_shutdown_step(engines.stop_engines_step());
    let mut router = RpcRouter::new(&cfg, &pipe, dispatcher, confirm.clone())
        .with_caps(caps.clone())
        .with_desktop(desktop_settings.clone())
        .with_observer(ui_apps.clone())
        .with_observer(holds)
        .with_observer(interchange)
        .with_observer(diagnostics)
        .with_observer(Arc::new(AppTracker::new(telemetry.clone())));
    if cfg.test_tools {
        router = router.with_observer(test_holds);
    }
    let router: Arc<dyn Router> = Arc::new(router);
    let stats = Arc::new(ServerStats::default());
    let stop = CancellationToken::new();
    let serving = tokio::spawn(serve_with(
        listener,
        router,
        stats,
        stop.clone(),
        cfg.pipe_instance_fault.clone(),
    ));

    if let Some(ws) = hub.default_ref() {
        let (hub, jobs, telemetry) = (hub.clone(), jobs.clone(), telemetry.clone());
        let confirm = confirm.clone();
        let models = models.clone();
        let meter = meter.clone();
        let caps = caps.clone();
        tokio::spawn(async move {
            match hub.open(&ws).await {
                Err(e) => {
                    tracing::warn!(workspace = %ws.path.display(), code = %e.code, error = %e.message, "the default workspace didn't open at start");
                }
                Ok(open) => {
                    if let Err(e) = jobs.recover_workspace(&open).await {
                        tracing::warn!(workspace = %ws.path.display(), code = %e.code, "the workspace's jobs could not be recovered; its first job call tries again");
                    }
                    // The meter's recovery only after a successful repair (ruling P3-R33).
                    let start = caps.repair_then_recover_workspace(&meter, &open).await;
                    if let Err(e) = start.repaired {
                        tracing::warn!(workspace = %ws.path.display(), code = %e.code, "the workspace's cap reservations could not be repaired; they stay held, and its usage rows stay, until the next start");
                    }
                    if let Some(Err(e)) = start.recovered {
                        tracing::warn!(workspace = %ws.path.display(), code = %e.code, "the workspace's pending usage rows could not be recovered; the next start tries again");
                    }
                    if let Err(e) = confirm.load(&open.state).await {
                        tracing::warn!(workspace = %ws.path.display(), error = %e, "the workspace's pending confirmations could not be read");
                    }
                    confirm.reconcile_toasts();
                    telemetry.set_sizes_probe(telemetry::sizes_probe(
                        Some(open.ws.path.clone()),
                        models,
                    ));
                    telemetry::profile::spawn_check(telemetry, &open);
                }
            }
        });
    }
    let mut helpers = vec![
        tokio::spawn(tick_idle(lifecycle.clone(), cfg.idle_after)),
        tokio::spawn(sweep_confirmations(confirm.clone())),
    ];
    if let Some(pid) = cfg.test_parent_pid.filter(|_| cfg.test_tools) {
        helpers.push(tokio::spawn(watch_test_parent(
            pid,
            lifecycle.clone(),
            cfg.parent_watch_fault.clone(),
        )));
    }
    let session_end = match SessionEndWatcher::start() {
        Ok((watcher, messages)) => {
            helpers.push(tokio::spawn(watch_session_end(messages, lifecycle.clone())));
            if let Some(power) = watcher.power_events() {
                helpers.push(tokio::spawn(watch_power(power, desktop_watch.clone())));
            }
            Some(Arc::new(watcher))
        }
        Err(e) => {
            tracing::warn!(error = %e, "no session-end window; a logoff will not drain the core");
            None
        }
    };
    let shutdown = tokio::spawn(supervise(Supervised {
        lifecycle: lifecycle.clone(),
        state: state.clone(),
        hub: hub.clone(),
        stop,
        serving,
        helpers,
        telemetry: telemetry.clone(),
        session_end: session_end.clone(),
        desktop_tasks: (autopause, keep_awake),
    }));
    tracing::info!(pipe = %pipe, channel = %cfg.env.channel(), "core serving");
    Ok(RunningCore {
        pipe,
        lifecycle,
        state,
        hub,
        events,
        telemetry,
        test_gates,
        active_projects,
        confirm,
        session_end,
        desktop_watch,
        desktop_settings,
        ui_apps,
        notifier,
        shutdown,
    })
}

/// Tracks each verified `ui:*` connection's process in the telemetry with role `app` while it
/// is connected (P4a-R27): Hardware's "who's using the GPU" shows the app, and a later plan's
/// RAM admission reads its working set. A process with two connections stays tracked until
/// both closed; a connection whose process Windows didn't report (pid 0) isn't tracked.
struct AppTracker {
    telemetry: Arc<Telemetry>,
    /// Each tracked connection's process.
    conns: std::sync::Mutex<HashMap<u64, u32>>,
}

impl AppTracker {
    fn new(telemetry: Arc<Telemetry>) -> AppTracker {
        AppTracker {
            telemetry,
            conns: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn conns(&self) -> std::sync::MutexGuard<'_, HashMap<u64, u32>> {
        self.conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ConnObserver for AppTracker {
    fn ready(&self, conn_id: u64, session: &Session) {
        if matches!(session.caller, CallerKind::Ui(_)) && session.client_pid != 0 {
            self.conns().insert(conn_id, session.client_pid);
            self.telemetry
                .track(session.client_pid, telemetry::APP_ROLE);
        }
    }

    fn closed(&self, conn_id: u64) {
        let mut conns = self.conns();
        if let Some(pid) = conns.remove(&conn_id)
            && !conns.values().any(|p| *p == pid)
        {
            self.telemetry.untrack_if(pid, telemetry::APP_ROLE);
        }
    }
}

/// What the shutdown supervisor owns.
struct Supervised {
    lifecycle: Arc<Lifecycle>,
    state: Arc<StateRegistry>,
    hub: Arc<WorkspaceHub>,
    stop: CancellationToken,
    serving: JoinHandle<io::Result<()>>,
    helpers: Vec<JoinHandle<()>>,
    telemetry: Arc<Telemetry>,
    session_end: Option<Arc<SessionEndWatcher>>,
    /// Stopped once the pipe is closed; keep-awake releases its request then.
    desktop_tasks: (Arc<AutoPause>, Arc<KeepAwake>),
}

/// Waits for a drain (or for the listener to fail, which is fatal), then shuts down in the
/// order of [`RunningCore`]'s docs.
async fn supervise(s: Supervised) -> ShutdownReport {
    let Supervised {
        lifecycle,
        state,
        hub,
        stop,
        mut serving,
        helpers,
        telemetry,
        session_end,
        desktop_tasks,
    } = s;
    let drain = lifecycle.drain_token();
    let served_early = tokio::select! {
        () = drain.cancelled() => None,
        served = &mut serving => Some(served),
    };
    if let Some(served) = &served_early {
        let why = match served {
            Ok(Ok(())) => "the pipe listener stopped".to_string(),
            Ok(Err(e)) => format!("the pipe listener failed: {e}"),
            Err(e) => format!("the pipe listener ended: {e}"),
        };
        tracing::error!(why, "fatal error; the core stops");
        lifecycle.begin_drain(DrainReason::Fatal(why));
    }
    let mut report = lifecycle.run_shutdown().await;

    // Sampling stops and telemetry.db is flushed; it is disposable, so nothing waits on it.
    telemetry.shutdown().await;

    // The open workspaces close before the state databases (module docs). It is part of the
    // state step, so the report's steps stay as plan 2 named them.
    hub.close_all().await;

    // The state databases close before the pipe is released (module docs).
    let closing = state.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || closing.close()).await {
        tracing::error!(error = %e, "closing the state databases failed");
    }
    report.steps.push("state.close");

    stop.cancel();
    if served_early.is_none() {
        match serving.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "the pipe listener failed while stopping"),
            Err(e) => tracing::warn!(error = %e, "the pipe listener ended while stopping"),
        }
    }
    report.steps.push("pipe.close");

    for helper in helpers {
        helper.abort();
    }
    drop(desktop_tasks);
    if let Some(window) = session_end {
        window.shutdown_complete();
    }
    tracing::info!(
        reason = %report.reason,
        steps = ?report.steps,
        abandoned_calls = report.abandoned_calls,
        exit_code = report.exit_code(),
        "core stopped"
    );
    report
}

/// `suite.toml` for a reader of `section` at start: `None` for no file, and for a file that
/// can't be read or doesn't parse, which is logged (QA1 M11: a malformed file never turns a
/// setting off silently; the engine supervisor logs its own `[engines.comfyui]` the same way).
fn read_suite_toml(env: &Env, section: &str) -> Option<lil_schema::SuiteToml> {
    let path = env.suite_toml_to_read();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, section, "suite.toml can't be read; the section's defaults apply");
            return None;
        }
    };
    match lil_schema::SuiteToml::parse(&text) {
        Ok(suite) => Some(suite),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, section, "suite.toml doesn't parse; the section's defaults apply");
            None
        }
    }
}

/// The folder of the core executable, where installed apps' `apps\` folder is (APP-061).
fn core_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default()
}

/// `[jobs] preempt_agent_batches` of `suite.toml` (CORE-125): `false` for no file, a file that
/// doesn't parse (logged), or no such key. Read once, at start.
fn preempt_agent_batches(env: &Env) -> bool {
    read_suite_toml(env, "[jobs]").is_some_and(|suite| suite.preempt_agent_batches())
}

/// The `[power]` settings of `suite.toml` (CORE-064); the defaults where there are none.
fn power_settings(env: &Env) -> PowerSettings {
    read_suite_toml(env, "[power]").map_or_else(PowerSettings::default, |suite| {
        PowerSettings::from_toml(&suite.power())
    })
}

/// Starts the hardware telemetry (CORE-059): this machine's hardware sampled once a second, or,
/// on a test channel, the fake hardware script `LIL_TEST_FAKE_HW` names (none: no hardware at
/// all) advanced only by `test.hw_tick`. A test never reads real hardware counters (QA-038).
async fn start_telemetry(cfg: &CoreConfig, events: &Arc<EventBus>) -> Arc<Telemetry> {
    let (env, clock, events) = (cfg.env.clone(), cfg.clock.clone(), events.clone());
    let (test, fake) = (cfg.test_tools, cfg.fake_hw.clone());
    // The sources block (NVML, DXGI, PDH), and so does opening `telemetry.db`.
    tokio::task::spawn_blocking(move || {
        let (src, mode) = if test {
            let fake = fake.and_then(|path| match lil_platform::hw_fake::sources_from_file(&path) {
                Ok(sources) => Some(HwSources::from_fake(sources)),
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "the fake hardware script can't be read; the core samples no hardware");
                    None
                }
            });
            (fake.unwrap_or_else(HwSources::none), TickMode::Manual)
        } else {
            (HwSources::real(), TickMode::Real)
        };
        let telemetry = Telemetry::start(
            src,
            env.local_dir().join("telemetry.db"),
            clock,
            power_settings(&env),
            events,
            mode,
        );
        if mode == TickMode::Real {
            telemetry.set_gpu_opener(Box::new(lil_platform::hw::nvml_gpu));
        }
        telemetry
    })
    .await
    .expect("starting the telemetry does not panic")
}

/// Opens the suite `state.db` and maps its failures to the core's exit codes (5, 6, 7 or 2),
/// logging each with the path.
fn open_suite_state(env: &Env, on_fatal: OnFatal) -> Result<StateRegistry, CoreExit> {
    let path = env.suite_state_db();
    StateRegistry::open(env, on_fatal).map_err(|e| match e {
        StateError::TooNew { found, known } => {
            tracing::error!(
                path = %path.display(),
                found,
                known,
                "the suite state.db is from a newer version of LILITH; nothing was changed"
            );
            CoreExit::StateTooNew(path)
        }
        StateError::Corrupt(why) => {
            tracing::error!(
                path = %path.display(),
                why,
                "the suite state.db is damaged; nothing was changed"
            );
            CoreExit::StateDamaged(path)
        }
        StateError::HotJournal => {
            tracing::error!(
                path = %path.display(),
                "the suite state.db has a hot rollback journal (a crash mid-write); nothing was changed"
            );
            CoreExit::StateHotJournal(path)
        }
        other => {
            tracing::error!(path = %path.display(), error = %other, "cannot open the suite state.db");
            CoreExit::Startup(format!(
                "cannot open the state database {}: {other}",
                path.display()
            ))
        }
    })
}

/// Ticks the idle check (ARCH-020) every [`IDLE_TICK`], or every `idle_after` when a test
/// channel set that shorter (at least once a second), until the core is idle long enough.
async fn tick_idle(lifecycle: Arc<Lifecycle>, idle_after: Duration) {
    let every = IDLE_TICK.min(idle_after.max(Duration::from_secs(1)));
    loop {
        tokio::time::sleep(every).await;
        if lifecycle.is_draining() {
            return;
        }
        if lifecycle.drain_if_idle() {
            return;
        }
    }
}

/// Marks expired confirmations every second ([`Confirm::sweep`]) until the core stops.
async fn sweep_confirmations(confirm: Arc<Confirm>) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        confirm.sweep().await;
    }
}

/// The session-end window's messages: a query is only logged (another application can
/// still cancel the logoff); the end of the session begins the drain.
async fn watch_session_end(
    mut messages: tokio::sync::mpsc::UnboundedReceiver<SessionEnd>,
    lifecycle: Arc<Lifecycle>,
) {
    while let Some(msg) = messages.recv().await {
        match msg {
            SessionEnd::QueryEnd => tracing::info!("the Windows session may end"),
            SessionEnd::CloseIgnored => {
                tracing::warn!("the session-end window was sent WM_CLOSE from outside; ignored");
            }
            SessionEnd::End => {
                tracing::info!("the Windows session ends");
                lifecycle.begin_drain(DrainReason::SessionEnd);
            }
        }
    }
}

/// The session-end window's sleep broadcasts, for the desktop's sleep log (ARCH-127).
async fn watch_power(
    mut events: tokio::sync::mpsc::UnboundedReceiver<PowerEvent>,
    watch: Arc<DesktopWatch>,
) {
    while let Some(event) = events.recv().await {
        watch.power_event(event);
    }
}

/// Test channels: drains the core once the test process `pid` is gone, so a test core never
/// outlives its test. A parent that is not running (`NotFound`) is gone. One that cannot be
/// watched (opening it fails otherwise, for example access denied, or the wait cannot be
/// registered) is a fatal error: the core would otherwise outlive its test unseen. `fault`
/// (in-process tests, [`CoreConfig::parent_watch_fault`]) runs before each step, and its `Err`
/// is that step's.
async fn watch_test_parent(pid: u32, lifecycle: Arc<Lifecycle>, fault: Option<ParentWatchFault>) {
    let fault = |step| fault.as_ref().map_or(Ok(()), |f| f(step));
    let cannot_watch = |e: &io::Error| {
        tracing::error!(pid, error = %e, "cannot watch the test parent; the core stops");
        lifecycle.begin_drain(DrainReason::Fatal(format!(
            "cannot watch the test parent {pid}: {e}"
        )));
    };
    match fault(ParentWatchStep::Open).and_then(|()| process::open(pid)) {
        Ok(parent) => {
            let waited = match fault(ParentWatchStep::Wait) {
                Ok(()) => parent.wait().await.map(drop),
                Err(e) => Err(e),
            };
            if let Err(e) = waited {
                cannot_watch(&e);
                return;
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            tracing::info!(pid, error = %e, "the test parent is not running");
        }
        Err(e) => {
            cannot_watch(&e);
            return;
        }
    }
    tracing::info!(pid, "the test parent is gone; stopping");
    lifecycle.begin_drain(DrainReason::TestParentGone);
}

/// A core on its own multi-thread runtime, for in-process tests. Dropping it stops the core
/// (as a test whose parent is gone: running calls get at most `FATAL_BUDGET`).
#[derive(Debug)]
pub struct CoreThread {
    core: Option<RunningCore>,
    rt: Option<tokio::runtime::Runtime>,
}

impl CoreThread {
    pub fn core(&self) -> &RunningCore {
        self.core.as_ref().expect("the core runs until drop")
    }

    /// Waits up to `within` for the core to shut down by itself (a drain something else
    /// began) and returns its report; `None` if it still runs then (the core is then stopped
    /// as on drop). `within` is a failure deadline, never a measurement.
    ///
    /// # Panics
    /// If called inside a tokio runtime.
    pub fn wait_stopped(mut self, within: Duration) -> Option<ShutdownReport> {
        let (core, rt) = (self.core.take()?, self.rt.take()?);
        let lifecycle = core.lifecycle().clone();
        let report = rt.block_on(async {
            let mut shutdown = std::pin::pin!(core.wait());
            match tokio::time::timeout(within, &mut shutdown).await {
                Ok(report) => Some(report),
                Err(_) => {
                    lifecycle.begin_drain(DrainReason::TestParentGone);
                    let _ = tokio::time::timeout(STOP_WAIT, shutdown).await;
                    None
                }
            }
        });
        rt.shutdown_timeout(STOP_WAIT);
        report
    }
}

impl Drop for CoreThread {
    fn drop(&mut self) {
        let (Some(core), Some(rt)) = (self.core.take(), self.rt.take()) else {
            return;
        };
        core.lifecycle().begin_drain(DrainReason::TestParentGone);
        if tokio::runtime::Handle::try_current().is_ok() {
            // Inside another runtime blocking is not allowed; let it wind down by itself.
            rt.shutdown_background();
            return;
        }
        let _ = rt.block_on(async { tokio::time::timeout(STOP_WAIT, core.wait()).await });
        rt.shutdown_timeout(STOP_WAIT);
    }
}

/// How long dropping a [`CoreThread`] waits for its core to stop: past `FATAL_BUDGET`.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// Starts a core on a new multi-thread runtime owned by the returned [`CoreThread`].
///
/// # Panics
/// If the runtime cannot be built or the core cannot start; call it outside any runtime.
pub fn start_in_thread(cfg: CoreConfig) -> CoreThread {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("cannot build the core's runtime");
    let core = rt
        .block_on(start(cfg))
        .unwrap_or_else(|e| panic!("the core did not start: {e}"));
    CoreThread {
        core: Some(core),
        rt: Some(rt),
    }
}
