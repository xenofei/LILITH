//! The core's transport (ARCH-024–026, ARCH-033): the named-pipe listener, the single instance
//! per channel, and the running core (infrastructure, not a CORE-014 module).
//!
//! - [`claim_pipe`] creates the pipe's *first* instance. If the pipe already exists, another
//!   core owns the channel and this one exits with code 3 before it touches anything else.
//! - [`serve`] accepts connections. A new instance is always created before a connected one is
//!   handed to its task, so the pipe name never disappears while the core runs. If one cannot
//!   be created, `serve` fails, and the running core stops as after a fatal error (exit 4).
//! - Each connection runs in [`conn`]: NDJSON framing and the order rules of the [`Router`].
//!   After the handshake at most [`MAX_IN_FLIGHT_PER_CONN`] requests of one connection run at
//!   once; the next line is read only when one of them has ended (backpressure on the pipe).
//! - [`RunningCore`] shuts down once the [`Lifecycle`] drains (ARCH-021): the lifecycle's
//!   wait and steps, then the open workspaces close (their `state.db` first, then
//!   `core.lock`), then the state databases close, then the pipe.
//! - The jobs ([`crate::jobs`]) are the dispatcher's: before it serves, the core registers
//!   their handlers (the test channels' `test.job`), reads `[jobs] preempt_agent_batches`,
//!   stops them starting queued jobs once a drain begins, recovers the suite scope's jobs
//!   (ARCH-022), and adds the `jobs` idle blocker (ARCH-020) and the `PersistInterrupted`
//!   shutdown step (ARCH-021). The default workspace's jobs are
//!   recovered once it opens. The GPU scheduler ([`crate::jobs::scheduler`]) is their
//!   admission, and every telemetry sample kicks them: a job waiting for VRAM is asked about
//!   again, and the watchdog looks at the running ones.
//! - The hardware telemetry ([`crate::telemetry`]) starts before the dispatcher is built: this
//!   machine's hardware sampled once a second, or on a test channel the fake hardware script
//!   (`LIL_TEST_FAKE_HW`) advanced only by `test.hw_tick`. Its tools and its topic's snapshot
//!   are registered, the default workspace's hardware profile is checked once the workspace is
//!   open and the first sample is in (CORE-067), and sampling stops right after the shutdown
//!   steps.
//! - The confirmations ([`crate::confirm`]) are stage 5's confirm half ([`AgentConfirm`]), the
//!   router's `app/*` methods and an idle blocker (ARCH-020; never a drain's). The suite
//!   database's pending ones are loaded at start, the default workspace's once it opens, and
//!   expired ones are marked every second.
//! - The engines ([`crate::engines`]) are built once the dispatcher's jobs exist (their tools
//!   find them through a slot filled before the core serves). They register the node-install
//!   handler, are the GPU scheduler's engine processes and unload, have ComfyUI's models folder
//!   measured with the workspace, and stop an owned ComfyUI in the `StopEngines` shutdown step.
//!   Their outside-jobs watch is an idle blocker (it holds no drain), and their observation
//!   ([`Engines::observe`]: the outside-jobs poll every 2 s while ComfyUI is ready, the model
//!   inventory each time it becomes ready) starts with the suite's jobs recovered.

mod conn;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use lil_platform::pipe::{CreateError, client_pid, create_instance};
use lil_platform::session_end::{SessionEnd, SessionEndSim, SessionEndWatcher};
use lil_platform::{Env, process};
use lil_protocol::{Id, Request, Response, encode_response_capped};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::config::{CoreConfig, CoreExit, ParentWatchFault, ParentWatchStep, PipeInstanceFault};
use crate::confirm::{AgentConfirm, Confirm, MCP_CONFIRMATION, NoNotificationPath, UiPresence};
use crate::engines::Engines;
use crate::engines::tools::{EnginesSlot, register_engine_test_tools, register_engine_tools};
use crate::events::EventBus;
use crate::jobs::scheduler::GpuScheduler;
use crate::lifecycle::{DrainReason, IDLE_TICK, Lifecycle, ShutdownReport};
use crate::meter::LogSink;
use crate::projects::ActiveProjects;
use crate::resources::{Resources, register_resource_tools, spawn_personal_maintenance};
use crate::rpc::RpcRouter;
use crate::state::{OnFatal, StateError, StateRegistry};
use crate::store::Personal;
use crate::telemetry::{self, HwSources, PowerSettings, Telemetry, TickMode};
use crate::tools::{
    Dispatcher, PipelineHooks, TestGates, TestJobHandler, core_registry, register_hw_tick,
    register_telemetry_tools,
};
use crate::workspace::WorkspaceHub;

/// Per-connection writer queue (responses and events).
pub const OUTBOUND_CAPACITY: usize = 256;

/// The requests one connection may have running in their own tasks at once. Requests that
/// run on the reader ([`Router::runs_inline`], and everything before the handshake) do not
/// count.
pub const MAX_IN_FLIGHT_PER_CONN: usize = 64;

/// What a [`Router`] knows about one connection.
#[derive(Debug)]
pub struct ConnCtx {
    /// Unique within this core, counting from 1.
    pub conn_id: u64,
    /// From `GetNamedPipeClientProcessId` (ARCH-045); 0 if Windows did not report it.
    pub client_pid: u32,
    /// Lines to send to the client, without their `\n`.
    pub outbound: tokio::sync::mpsc::Sender<String>,
    /// Cancelling it ends the connection.
    pub close: CancellationToken,
}

impl ConnCtx {
    /// The line for `response`, within the 4 MiB cap (ARCH-025): one that would be longer is
    /// never sent; it becomes a -32603 error for the same id, and is logged.
    pub fn response_line(&self, response: &Response) -> String {
        let (line, oversized) = encode_response_capped(response);
        if let Some(bytes) = oversized {
            tracing::error!(
                conn_id = self.conn_id,
                bytes,
                "a response was too large to send; the caller gets -32603 instead"
            );
        }
        line
    }

    /// Queues `response` ([`ConnCtx::response_line`]). A connection whose writer has stopped
    /// drops it.
    pub async fn send_response(&self, response: &Response) {
        let _ = self.outbound.send(self.response_line(response)).await;
    }
}

/// Decides what each request on a connection gets.
pub trait Router: Send + Sync + 'static {
    /// The connection was accepted; runs before any line is read (e.g. starts the hello
    /// timeout).
    fn opened(&self, conn: &ConnCtx);
    /// `false`: each request is handled inline, in order, before the next line is read, so a
    /// request pipelined right behind `core/hello` sees the session. `true`: each request
    /// runs in its own task (unless [`Router::runs_inline`]), holding one of the connection's
    /// [`MAX_IN_FLIGHT_PER_CONN`] permits.
    fn handshake_done(&self, conn: &ConnCtx) -> bool;
    /// After the handshake: `true` if `req` is still handled on the reader, in order and
    /// without a permit (cheap requests that must not wait behind a full connection).
    fn runs_inline(&self, req: &Request) -> bool {
        let _ = req;
        false
    }
    /// Handles one request (never a notification). `None` sends nothing.
    fn handle<'a>(
        &'a self,
        conn: &'a ConnCtx,
        req: Request,
    ) -> Pin<Box<dyn Future<Output = Option<Response>> + Send + 'a>>;
    /// The response to request `id`, whose handler panicked in its own task (a request run
    /// on the reader that panics ends the connection instead).
    fn panicked(&self, conn: &ConnCtx, id: Id) -> Response;
    /// The connection ended; runs exactly once, also when a handler on the reader panicked.
    fn closed(&self, conn: &ConnCtx);
}

/// Counters the transport keeps. The running core counts its connections in the
/// [`Lifecycle`] (through its router); this count is the transport's own, whatever the
/// router, and is what the transport tests (`tests/transport.rs`, with a fake [`Router`])
/// watch to see a connection task end.
#[derive(Debug, Default)]
pub struct ServerStats {
    /// Open connections.
    pub connections: AtomicUsize,
}

/// The claimed pipe: its first, not yet connected, instance.
#[derive(Debug)]
pub struct Listener {
    name: String,
    first: NamedPipeServer,
}

impl Listener {
    /// The full pipe name, `\\.\pipe\...`.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Creates the first instance of `pipe` (a full `\\.\pipe\...` name). If the pipe exists,
/// another core owns the channel: [`CoreExit::AlreadyRunning`]. Must be called inside a tokio
/// runtime.
pub fn claim_pipe(pipe: &str) -> Result<Listener, CoreExit> {
    match create_instance(pipe, true) {
        Ok(first) => Ok(Listener {
            name: pipe.to_string(),
            first,
        }),
        Err(CreateError::AlreadyOwned) => Err(CoreExit::AlreadyRunning),
        Err(CreateError::Io(e)) => Err(CoreExit::Startup(format!(
            "cannot create the pipe {pipe}: {e}"
        ))),
    }
}

/// Accepts connections on `listener` until `stop` is cancelled, then closes the pipe, ends
/// every connection and waits for their tasks. Fails only if a new pipe instance cannot be
/// created.
pub async fn serve(
    listener: Listener,
    router: Arc<dyn Router>,
    stats: Arc<ServerStats>,
    stop: CancellationToken,
) -> io::Result<()> {
    serve_with(listener, router, stats, stop, None).await
}

/// [`serve`], with the in-process tests' fault ([`CoreConfig::pipe_instance_fault`]) run
/// before each new instance.
async fn serve_with(
    listener: Listener,
    router: Arc<dyn Router>,
    stats: Arc<ServerStats>,
    stop: CancellationToken,
    fault: Option<PipeInstanceFault>,
) -> io::Result<()> {
    let Listener { name, first } = listener;
    let mut waiting = first;
    let mut connections = JoinSet::new();
    let next_id = AtomicU64::new(1);
    let result = loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break Ok(()),
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            connected = waiting.connect() => {
                // A fresh instance first, so the name never disappears (another core could
                // otherwise claim it).
                let fresh = match new_instance(&name, fault.as_ref()) {
                    Ok(fresh) => fresh,
                    Err(e) => {
                        tracing::error!(pipe = %name, error = %e, "cannot create a new pipe instance");
                        break Err(e);
                    }
                };
                let server = std::mem::replace(&mut waiting, fresh);
                if let Err(e) = connected {
                    // The client left before the connection completed.
                    tracing::debug!(error = %e, "a pipe connection failed");
                    continue;
                }
                let client_pid = client_pid(&server).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "the client's process id is unknown");
                    0
                });
                let conn_id = next_id.fetch_add(1, Ordering::Relaxed);
                stats.connections.fetch_add(1, Ordering::SeqCst);
                let counted = Counted(stats.clone());
                let router = router.clone();
                let close = stop.child_token();
                connections.spawn(async move {
                    let _counted = counted;
                    conn::run(server, conn_id, client_pid, router, close).await;
                });
            }
        }
    };
    // Close the pipe to new clients, then end and wait for every connection (their `close`
    // tokens are children of `stop`).
    drop(waiting);
    stop.cancel();
    while connections.join_next().await.is_some() {}
    result
}

/// A further instance of the pipe `name`; `fault` (in-process tests) runs first, and its
/// `Err` is the creation's.
fn new_instance(name: &str, fault: Option<&PipeInstanceFault>) -> io::Result<NamedPipeServer> {
    if let Some(fault) = fault {
        fault()?;
    }
    create_instance(name, false).map_err(create_error)
}

fn create_error(e: CreateError) -> io::Error {
    match e {
        CreateError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

/// Decrements the connection count when a connection task ends, however it ends.
struct Counted(Arc<ServerStats>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

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
    if cfg.test_tools {
        register_hw_tick(&mut registry, telemetry.clone());
    }
    // The engines need the jobs, which the dispatcher builds: their tools find them here.
    let engine_slot = EnginesSlot::default();
    register_engine_tools(&mut registry, &engine_slot);
    if cfg.test_tools {
        register_engine_test_tools(&mut registry);
    }
    // The confirmations (ARCH-052–055): stage 5's confirm half, answered by the apps.
    let confirm = Confirm::new(
        state.clone(),
        events.clone(),
        cfg.clock.clone(),
        cfg.ids.clone(),
        UiPresence::new(),
        Arc::new(NoNotificationPath),
        cfg.confirm_lifetime,
        MCP_CONFIRMATION,
        cfg.failpoints.clone(),
    );
    match confirm.load(state.suite()).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(count = n, "pending confirmations of an earlier core run"),
        Err(e) => {
            tracing::error!(error = %e, "the pending confirmations could not be read; a retry with its token finds each")
        }
    }
    let hooks = PipelineHooks {
        confirm: AgentConfirm::new(confirm.clone()),
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
    let active_projects = dispatcher.active_projects().clone();
    let jobs = dispatcher.jobs().clone();
    if cfg.test_tools {
        jobs.register(Arc::new(TestJobHandler::new(test_gates.clone())));
    }
    jobs.set_preempt_agent_batches(preempt_agent_batches(&cfg.env));
    // The engines (ARCH-064): they register the node-install handler, and are the scheduler's
    // engine processes and unload (ARCH-071).
    let engines = Engines::new(
        cfg.env.clone(),
        jobs.clone(),
        telemetry.clone(),
        events.clone(),
        cfg.clock.clone(),
        cfg.failpoints.clone(),
        Arc::new(LogSink),
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
    // Completions (Tasks 15–16) are added before this: it runs them for jobs that never
    // started.
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
    lifecycle.add_blocker(jobs.clone());
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
    let router: Arc<dyn Router> =
        Arc::new(RpcRouter::new(&cfg, &pipe, dispatcher, confirm.clone()));
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
        tokio::spawn(async move {
            match hub.open(&ws).await {
                Err(e) => {
                    tracing::warn!(workspace = %ws.path.display(), code = %e.code, error = %e.message, "the default workspace didn't open at start");
                }
                Ok(open) => {
                    if let Err(e) = jobs.recover_workspace(&open).await {
                        tracing::warn!(workspace = %ws.path.display(), code = %e.code, "the workspace's jobs could not be recovered; its first job call tries again");
                    }
                    if let Err(e) = confirm.load(&open.state).await {
                        tracing::warn!(workspace = %ws.path.display(), error = %e, "the workspace's pending confirmations could not be read");
                    }
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
        shutdown,
    })
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
