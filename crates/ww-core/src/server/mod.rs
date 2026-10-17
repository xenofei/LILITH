//! The core's transport (ARCH-024–026, ARCH-033): the named-pipe listener, the single instance
//! per channel, and the running core (infrastructure, not a CORE-014 module).
//!
//! - [`claim_pipe`] creates the pipe's *first* instance. If the pipe already exists, another
//!   core owns the channel and this one exits with code 3 before it touches anything else.
//! - [`serve`] accepts connections. A new instance is always created before a connected one is
//!   handed to its task, so the pipe name never disappears while the core runs.
//! - Each connection runs in [`conn`]: NDJSON framing and the order rules of the [`Router`].
//!   After the handshake at most [`MAX_IN_FLIGHT_PER_CONN`] requests of one connection run at
//!   once; the next line is read only when one of them has ended (backpressure on the pipe).
//! - [`RunningCore`] shuts down once the [`Lifecycle`] drains (ARCH-021): the lifecycle's
//!   wait and steps, then the open workspaces close (their `state.db` first, then
//!   `core.lock`), then the state databases close, then the pipe.

mod conn;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use ww_platform::pipe::{CreateError, client_pid, create_instance};
use ww_platform::session_end::{SessionEnd, SessionEndWatcher};
use ww_platform::{Env, process};
use ww_protocol::{Id, Request, Response, encode_response_capped};

use crate::config::{CoreConfig, CoreExit};
use crate::events::EventBus;
use crate::lifecycle::{DrainReason, IDLE_TICK, Lifecycle, ShutdownReport};
use crate::projects::ActiveProjects;
use crate::rpc::RpcRouter;
use crate::state::{OnFatal, StateError, StateRegistry};
use crate::tools::{Dispatcher, PipelineHooks, TestGates, core_registry};
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
                let fresh = match create_instance(&name, false) {
                    Ok(fresh) => fresh,
                    Err(e) => break Err(create_error(e)),
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
    /// The gates `test.block` waits on (test channels).
    test_gates: Arc<TestGates>,
    /// Each session's active project (ARCH-088), shared with the dispatcher.
    active_projects: Arc<ActiveProjects>,
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
    pub fn simulate_session_end(&self, msg: SessionEnd) -> bool {
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
/// [`CoreExit::StateTooNew`], a damaged one [`CoreExit::StateDamaged`], anything else
/// [`CoreExit::Startup`]; each is logged with the path, nothing on disk is changed, and the
/// pipe is released as this returns.
///
/// Then it serves, opens the default workspace in the background (a failure is logged; the
/// first call in it tries again), ticks the idle check every [`IDLE_TICK`] (or `idle_after`,
/// if a test channel set it shorter), watches the session end and, on a test channel, the
/// test parent. A state actor that dies begins a fatal drain (QA-012).
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
    let dispatcher = Dispatcher::new(
        core_registry(
            cfg.test_tools,
            &test_gates,
            &state,
            &hub,
            &cfg.env,
            &cfg.probe,
        ),
        PipelineHooks::plan2(),
        state.clone(),
        events.clone(),
        hub.clone(),
        cfg.ids.clone(),
        cfg.clock.clone(),
        lifecycle.clone(),
    );
    let active_projects = dispatcher.active_projects().clone();
    let router: Arc<dyn Router> = Arc::new(RpcRouter::new(&cfg, &pipe, dispatcher));
    let stats = Arc::new(ServerStats::default());
    let stop = CancellationToken::new();
    let serving = tokio::spawn(serve(listener, router, stats, stop.clone()));

    if let Some(ws) = hub.default_ref() {
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = hub.open(&ws).await {
                tracing::warn!(workspace = %ws.path.display(), code = %e.code, error = %e.message, "the default workspace didn't open at start");
            }
        });
    }
    let mut helpers = vec![tokio::spawn(tick_idle(lifecycle.clone(), cfg.idle_after))];
    if let Some(pid) = cfg.test_parent_pid.filter(|_| cfg.test_tools) {
        helpers.push(tokio::spawn(watch_test_parent(pid, lifecycle.clone())));
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
        session_end: session_end.clone(),
    }));
    tracing::info!(pipe = %pipe, channel = %cfg.env.channel(), "core serving");
    Ok(RunningCore {
        pipe,
        lifecycle,
        state,
        hub,
        events,
        test_gates,
        active_projects,
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

/// Opens the suite `state.db` and maps its failures to the core's exit codes (5, 6 or 2),
/// logging each with the path.
fn open_suite_state(env: &Env, on_fatal: OnFatal) -> Result<StateRegistry, CoreExit> {
    let path = env.suite_state_db();
    StateRegistry::open(env, on_fatal).map_err(|e| match e {
        StateError::TooNew { found, known } => {
            tracing::error!(
                path = %path.display(),
                found,
                known,
                "the suite state.db is from a newer version of Worldwright; nothing was changed"
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
/// outlives its test.
async fn watch_test_parent(pid: u32, lifecycle: Arc<Lifecycle>) {
    match process::open(pid) {
        Ok(parent) => {
            if let Err(e) = parent.wait().await {
                tracing::warn!(pid, error = %e, "cannot wait for the test parent; stopping");
            }
        }
        // Gone (`NotFound`), or a PID that now belongs to a process this user cannot open,
        // which is not the test parent either.
        Err(e) => tracing::info!(pid, error = %e, "the test parent is not running"),
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
