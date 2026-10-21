//! The core's transport (ARCH-024–026, ARCH-033): the named-pipe listener and the single
//! instance per channel (infrastructure, not a CORE-014 module). The running core
//! ([`crate::server`]) builds the router and serves it here.
//!
//! - [`claim_pipe`] creates the pipe's *first* instance. If the pipe already exists, another
//!   core owns the channel and this one exits with code 3 before it touches anything else.
//! - [`serve`] accepts connections. A new instance is always created before a connected one is
//!   handed to its task, so the pipe name never disappears while the core runs. If one cannot
//!   be created, `serve` fails, and the running core stops as after a fatal error (exit 4).
//! - Each connection runs in [`conn`]: NDJSON framing and the order rules of the [`Router`].
//!   After the handshake at most [`MAX_IN_FLIGHT_PER_CONN`] requests of one connection run at
//!   once; the next line is read only when one of them has ended (backpressure on the pipe).

mod conn;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use lil_platform::pipe::{CreateError, client_pid, create_instance};
use lil_protocol::{Id, Request, Response, encode_response_capped};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::config::{CoreExit, PipeInstanceFault};
use crate::session::Session;

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

/// Follows the connections that said hello, for modules that tie state to a connection: the
/// document holds (APP-104) and the running apps `open_in` finds (APP-063). The production
/// router calls its observers when a session is established and when its connection ends.
pub trait ConnObserver: Send + Sync + 'static {
    /// Connection `conn_id` said hello and has `session`.
    fn ready(&self, conn_id: u64, session: &Session) {
        let _ = (conn_id, session);
    }
    /// Connection `conn_id` ended (also one that never said hello).
    fn closed(&self, conn_id: u64);
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
pub(crate) async fn serve_with(
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
