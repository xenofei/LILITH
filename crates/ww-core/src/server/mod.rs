//! The core's transport (ARCH-024–026, ARCH-033): the named-pipe listener, the single instance
//! per channel, and the running core (infrastructure, not a CORE-014 module).
//!
//! - [`claim_pipe`] creates the pipe's *first* instance. If the pipe already exists, another
//!   core owns the channel and this one exits with code 3 before it touches anything else.
//! - [`serve`] accepts connections. A new instance is always created before a connected one is
//!   handed to its task, so the pipe name never disappears while the core runs.
//! - Each connection runs in [`conn`]: NDJSON framing and the order rules of the [`Router`].

mod conn;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use ww_platform::pipe::{CreateError, client_pid, create_instance};
use ww_platform::{Env, process};
use ww_protocol::{Request, Response};

use crate::config::{CoreConfig, CoreExit};
use crate::rpc::RpcRouter;
use crate::state::{OnFatal, StateError, StateRegistry};

/// Per-connection writer queue (responses and events).
pub const OUTBOUND_CAPACITY: usize = 256;

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

/// Decides what each request on a connection gets.
pub trait Router: Send + Sync + 'static {
    /// The connection was accepted; runs before any line is read (e.g. starts the hello
    /// timeout).
    fn opened(&self, conn: &ConnCtx);
    /// `false`: each request is handled inline, in order, before the next line is read, so a
    /// request pipelined right behind `core/hello` sees the session. `true`: each request
    /// runs in its own task.
    fn handshake_done(&self, conn: &ConnCtx) -> bool;
    /// Handles one request (never a notification). `None` sends nothing.
    fn handle<'a>(
        &'a self,
        conn: &'a ConnCtx,
        req: Request,
    ) -> Pin<Box<dyn Future<Output = Option<Response>> + Send + 'a>>;
    /// The connection ended; runs exactly once.
    fn closed(&self, conn: &ConnCtx);
}

/// Counters the transport keeps.
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
#[derive(Debug)]
pub struct RunningCore {
    pipe: String,
    stop: CancellationToken,
    serving: JoinHandle<io::Result<()>>,
    parent_watch: Option<JoinHandle<()>>,
    state: Arc<StateRegistry>,
    /// What died, once something reported a fatal error (the state actor, QA-012).
    fatal: Arc<OnceLock<String>>,
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

    /// Asks the core to stop. (In this stage it stops serving at once.)
    pub fn request_drain(&self) {
        self.stop.cancel();
    }

    /// Waits until the core has stopped serving. After a fatal error (a database actor died)
    /// that is [`CoreExit::Fatal`].
    pub async fn wait(self) -> Result<(), CoreExit> {
        let served = self.serving.await;
        if let Some(watch) = self.parent_watch {
            watch.abort();
        }
        match served {
            Ok(Ok(())) => match self.fatal.get() {
                Some(what) => Err(CoreExit::Fatal(format!("the {what} actor died"))),
                None => Ok(()),
            },
            Ok(Err(e)) => Err(CoreExit::Fatal(format!("the pipe listener failed: {e}"))),
            Err(e) => Err(CoreExit::Fatal(format!("the pipe listener ended: {e}"))),
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
pub async fn start_claimed(cfg: CoreConfig, listener: Listener) -> Result<RunningCore, CoreExit> {
    let pipe = listener.name().to_string();
    let stop = CancellationToken::new();
    let fatal = Arc::new(OnceLock::new());
    let on_fatal: OnFatal = {
        let (stop, fatal) = (stop.clone(), fatal.clone());
        Arc::new(move |what: &str| {
            tracing::error!(what, "fatal error; the core stops");
            let _ = fatal.set(what.to_string());
            stop.cancel();
        })
    };
    let state = match open_suite_state(&cfg.env, on_fatal) {
        Ok(state) => Arc::new(state),
        Err(exit) => {
            drop(listener);
            return Err(exit);
        }
    };
    let router: Arc<dyn Router> = Arc::new(RpcRouter::new(&cfg, &pipe));
    let stats = Arc::new(ServerStats::default());
    let serving = tokio::spawn(serve(listener, router, stats, stop.clone()));
    let parent_watch = cfg
        .test_parent_pid
        .filter(|_| cfg.test_tools)
        .map(|pid| tokio::spawn(watch_test_parent(pid, stop.clone())));
    tracing::info!(pipe = %pipe, channel = %cfg.env.channel(), "core serving");
    Ok(RunningCore {
        pipe,
        stop,
        serving,
        parent_watch,
        state,
        fatal,
    })
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

/// Test channels: stops the core once the test process `pid` is gone, so a test core never
/// outlives its test.
async fn watch_test_parent(pid: u32, stop: CancellationToken) {
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
    stop.cancel();
}

/// A core on its own multi-thread runtime, for in-process tests. Dropping it stops the core.
#[derive(Debug)]
pub struct CoreThread {
    core: Option<RunningCore>,
    rt: Option<tokio::runtime::Runtime>,
}

impl CoreThread {
    pub fn core(&self) -> &RunningCore {
        self.core.as_ref().expect("the core runs until drop")
    }
}

impl Drop for CoreThread {
    fn drop(&mut self) {
        let (Some(core), Some(rt)) = (self.core.take(), self.rt.take()) else {
            return;
        };
        core.request_drain();
        if tokio::runtime::Handle::try_current().is_ok() {
            // Inside another runtime blocking is not allowed; let it wind down by itself.
            rt.shutdown_background();
            return;
        }
        let _ = rt.block_on(async { tokio::time::timeout(STOP_WAIT, core.wait()).await });
        rt.shutdown_timeout(STOP_WAIT);
    }
}

/// How long dropping a [`CoreThread`] waits for its core to stop.
const STOP_WAIT: Duration = Duration::from_secs(5);

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
