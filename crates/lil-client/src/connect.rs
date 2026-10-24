//! Finding, starting, verifying and greeting the core, and draining it (ARCH-004, ARCH-014,
//! ARCH-018, ARCH-019, ARCH-023, ARCH-027).
//!
//! [`Client::connect`]:
//! 1. **Find or start.** `open_client` is retried with a backoff from 25 ms, doubling to
//!    250 ms. A busy pipe is retried. No pipe is [`ClientError::NotRunning`] under
//!    [`StartPolicy::NoStart`]; otherwise the core is spawned detached (no arguments, the
//!    configured `spawn_env`) and the search goes on. The deadline is `start_timeout`; while
//!    the core this client spawned is still alive it extends up to `start_hard_cap`. When that
//!    core has exited, code 3 (it lost the start race to another core) or 0 (it ran and was
//!    stopped) means the search goes on, and a new core is spawned only if no other serves;
//!    any other code is [`ClientError::StartFailed`].
//! 2. **Verify** (ARCH-027): the pipe's server PID is opened at once; its user must be the
//!    current user and its executable must be in `server_dirs`, else
//!    [`ClientError::Untrusted`]. Nothing is sent to an untrusted server.
//! 3. **`core/hello`.** The core claims its pipe before it opens the suite `state.db`, so the
//!    connection may close before the hello reply when that core then fails to start. If this
//!    client spawned that core, its exit code (waited for up to 10 s) says why. Otherwise the
//!    find loop runs once more (the client's own core then reports its failure); a second
//!    early close is [`ClientError::Closed`]. A server that cannot be inspected because it is
//!    gone is the same race; one that cannot be inspected for any other reason (another
//!    user's process) is [`ClientError::Untrusted`].
//! 4. **Version direction** (controller ruling 4). On `protocol.incompatible` a client whose
//!    major is greater than the core's sends `core/drain` on that connection, waits up to 60 s
//!    for the pipe to go, starts a new core and says hello once more. Any other client, and
//!    any client under [`StartPolicy::NoStart`] (it could not start the replacement), reports
//!    [`ClientError::Incompatible`] at once and leaves the core running (ARCH-014).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lil_platform::IdSource as _;
use lil_platform::RandomIds;
use lil_platform::paths::folder_key;
use lil_platform::pipe::{ConnectError, open_client, server_pid};
use lil_platform::process::{
    ProcessHandle, ProcessInfo, ProcessInspector as _, SpawnRequest, SystemInspector,
    current_user_sid, spawn_detached,
};
use lil_protocol::codes::{PROTOCOL_INCOMPATIBLE, WORKSPACE_UNKNOWN};
use lil_protocol::{
    CAPABILITIES, DrainResult, HelloParams, HelloResult, NO_WORKSPACE, Outcome, RpcError,
    SubscribeParams, SubscribeResult, ToolCallParams, ToolDescriptor, ToolOutput, ToolsListResult,
    UnsubscribeParams, UnsubscribeResult, method, rpc_code, schema_hash,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(test)]
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::time::{Instant, sleep, timeout, timeout_at};

use crate::conn::{Conn, EventStream};
use crate::{ClientConfig, ClientError, StartFailure, StartPolicy, WRONG_FOLDER};

/// The first pause between attempts to open the pipe.
const BACKOFF_START: Duration = Duration::from_millis(25);
/// The longest pause between attempts.
const BACKOFF_MAX: Duration = Duration::from_millis(250);
/// The exit code of a core that lost the race for the pipe to another core.
const LOST_RACE: u32 = 3;
/// The exit code of a core that stopped normally (idle, drained, session end).
const NORMAL_EXIT: u32 = 0;
/// How long a client waits for the exit code of a core it started that closed the connection
/// before saying hello.
const EARLY_CLOSE_WAIT: Duration = Duration::from_secs(10);
/// How long an automatic drain-and-replace waits for the old core's pipe to go.
const REPLACE_WAIT: Duration = Duration::from_secs(60);

/// A connection with a session on the core.
pub struct Client {
    conn: Arc<Conn>,
    hello: HelloResult,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("session_id", &self.hello.session_id)
            .field("core", &self.hello.core)
            .finish_non_exhaustive()
    }
}

/// What `core/hello` led to.
enum Handshake {
    Ready(Box<Client>),
    /// The core refused the hello for another major version; the connection is still open
    /// (`core/drain` is allowed on it when this client's major is greater).
    Incompatible {
        core_protocol: String,
        conn: Arc<Conn>,
    },
}

impl Client {
    /// Finds or starts the channel's core, verifies it and says hello (module docs).
    pub async fn connect(cfg: &ClientConfig) -> Result<Client, ClientError> {
        let (core_protocol, conn) = match handshake(cfg).await? {
            Handshake::Ready(client) => return Ok(*client),
            Handshake::Incompatible {
                core_protocol,
                conn,
            } => (core_protocol, conn),
        };
        // Controller ruling 4: only a newer client replaces the core (ARCH-023); an older one
        // leaves it running (ARCH-014). A client that may not start a core never stops one
        // either: it could not put a new one in its place.
        let older_core = core_protocol
            .parse::<lil_protocol::ProtocolVersion>()
            .is_ok_and(|core| cfg.protocol.major > core.major);
        if !older_core || cfg.start == StartPolicy::NoStart {
            return Err(ClientError::Incompatible { core_protocol });
        }
        let deadline = Instant::now() + REPLACE_WAIT;
        send_drain(&conn, deadline).await?;
        drop(conn);
        wait_gone(&cfg.env.pipe_name(), deadline).await?;
        match handshake(cfg).await? {
            Handshake::Ready(client) => Ok(*client),
            Handshake::Incompatible { core_protocol, .. } => {
                Err(ClientError::Incompatible { core_protocol })
            }
        }
    }

    /// The core's reply to `core/hello`.
    pub fn hello(&self) -> &HelloResult {
        &self.hello
    }

    /// Whether the connection has closed: every later call is [`ClientError::Closed`], and a
    /// new [`Client::connect`] is needed (it starts a core if none runs).
    pub fn is_closed(&self) -> bool {
        self.conn.is_closed()
    }

    /// `tools/call`. Without `key`, a fresh idempotency key `ik_<16 hex>` is minted
    /// (ARCH-110: `lil-client` owns idempotency keys), so the call runs once; pass the same key
    /// again to retry it. A tool's failure is an error [`Outcome`], not a [`ClientError`].
    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
        key: Option<String>,
    ) -> Result<Outcome<ToolOutput>, ClientError> {
        self.call_tool_traced(name, args, key, None).await
    }

    /// [`Client::call_tool`], carrying the caller's `trace_id` (ARCH-118).
    pub(crate) async fn call_tool_traced(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
        key: Option<String>,
        trace_id: Option<String>,
    ) -> Result<Outcome<ToolOutput>, ClientError> {
        let params = ToolCallParams {
            name: name.to_string(),
            arguments: args,
            idempotency_key: Some(key.unwrap_or_else(|| RandomIds.id("ik"))),
            trace_id,
        };
        let result = self
            .conn
            .call(method::TOOLS_CALL, to_value(&params))
            .await?;
        decode(result, method::TOOLS_CALL)
    }

    /// `app/confirmations.list` (APP-043): the confirmations in `state`, newest first. A `cli`
    /// session may call it too (read-only); it needs `confirmations.v1` in the shared
    /// capabilities, which this client offers.
    pub async fn confirmations(
        &self,
        state: lil_protocol::app::ListWhich,
    ) -> Result<Outcome<lil_protocol::app::ConfirmationsListResult>, ClientError> {
        let params = lil_protocol::app::ConfirmationsListParams { state, limit: None };
        let method = lil_protocol::app::CONFIRMATIONS_LIST;
        let result = self.conn.call(method, to_value(&params)).await?;
        decode(result, method)
    }

    /// An `app/*` method (APP-043) with `params`, its result as an [`Outcome`]: a refusal
    /// (`caller.not_allowed` for a caller that may not call it, say) is an error outcome, not
    /// a [`ClientError`]. A method outside `app/` is [`ClientError::Protocol`] and is not sent:
    /// the session methods have their own calls.
    pub async fn app_call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Outcome<Value>, ClientError> {
        if !method.starts_with(method::APP_PREFIX) {
            return Err(ClientError::Protocol(RpcError::new(
                rpc_code::INVALID_REQUEST,
                format!("{method} is not an app/* method"),
            )));
        }
        let result = self.conn.call(method, params).await?;
        decode(result, method)
    }

    /// The connection, for the session's own event sink.
    pub(crate) fn conn(&self) -> &Arc<Conn> {
        &self.conn
    }

    /// `lil/subscribe` on this connection with no stream of its own: the events reach every
    /// sink that wants them (the session's).
    pub(crate) async fn subscribe_only(
        &self,
        topics: Vec<String>,
        since_seq: BTreeMap<String, u64>,
    ) -> Result<SubscribeResult, ClientError> {
        let params = SubscribeParams { topics, since_seq };
        let result = self.conn.call(method::SUBSCRIBE, to_value(&params)).await?;
        match decode::<SubscribeResult>(result, method::SUBSCRIBE)? {
            Outcome::Ok(subscribed) => Ok(subscribed),
            Outcome::Error { error } => Err(ClientError::Refused(error)),
            Outcome::PendingConfirmation(_) => Err(unexpected_pending(method::SUBSCRIBE)),
        }
    }

    /// `lil/unsubscribe` on this connection.
    pub(crate) async fn unsubscribe_only(&self, topics: Vec<String>) -> Result<(), ClientError> {
        let params = UnsubscribeParams { topics };
        let result = self
            .conn
            .call(method::UNSUBSCRIBE, to_value(&params))
            .await?;
        match decode::<UnsubscribeResult>(result, method::UNSUBSCRIBE)? {
            Outcome::Ok(_) => Ok(()),
            Outcome::Error { error } => Err(ClientError::Refused(error)),
            Outcome::PendingConfirmation(_) => Err(unexpected_pending(method::UNSUBSCRIBE)),
        }
    }

    /// `tools/list`: the tools this session may see.
    pub async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ClientError> {
        let result = self.conn.call(method::TOOLS_LIST, json!({})).await?;
        match decode::<ToolsListResult>(result, method::TOOLS_LIST)? {
            Outcome::Ok(list) => Ok(list.tools),
            Outcome::Error { error } => Err(ClientError::Refused(error)),
            Outcome::PendingConfirmation(_) => Err(unexpected_pending(method::TOOLS_LIST)),
        }
    }

    /// `lil/subscribe` (ARCH-032). The stream gets every event of `topics` from now on, after
    /// the replay from `since` (per topic; a topic missing from it means live events only).
    /// Cursors for topics not in `topics` are dropped.
    ///
    /// A cursor is honoured only for a topic new to this connection: the core ignores it for a
    /// topic already subscribed here, which keeps its place in the sequence instead (and its
    /// events then reach every stream subscribed to it).
    pub async fn subscribe(
        &self,
        topics: Vec<String>,
        since: BTreeMap<String, u64>,
    ) -> Result<EventStream, ClientError> {
        let since_seq = since
            .into_iter()
            .filter(|(topic, _)| topics.contains(topic))
            .collect();
        // Registered before the request is written: replays may beat the reply.
        let (sink, rx) = self.conn.add_sink(&topics).ok_or(ClientError::Closed)?;
        let params = SubscribeParams { topics, since_seq };
        let subscribed = match self.conn.call(method::SUBSCRIBE, to_value(&params)).await {
            Ok(result) => match decode::<SubscribeResult>(result, method::SUBSCRIBE) {
                Ok(Outcome::Ok(subscribed)) => Ok(subscribed),
                Ok(Outcome::Error { error }) => Err(ClientError::Refused(error)),
                Ok(Outcome::PendingConfirmation(_)) => Err(unexpected_pending(method::SUBSCRIBE)),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        match subscribed {
            Ok(subscribed) => Ok(EventStream::new(
                rx,
                subscribed.topics,
                sink,
                self.conn.clone(),
            )),
            Err(e) => {
                self.conn.remove_sink(sink);
                Err(e)
            }
        }
    }
}

/// What [`drain_and_wait`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// No core was running.
    NotRunning,
    /// The core accepted the drain and its pipe is gone.
    Stopped,
}

/// `lil core stop`: asks the channel's core to finish its work and exit, then waits up to
/// `within` for its pipe to go. Never starts a core.
///
/// The hello names no workspace (a stop does not depend on one). A core owned by the current
/// user is asked even when its executable is not in `server_dirs`: a drain is harmless and
/// clears a foreign core (ARCH-027). A core whose protocol is newer than this client's refuses
/// the drain: [`ClientError::Incompatible`].
pub async fn drain_and_wait(
    cfg: &ClientConfig,
    within: Duration,
) -> Result<DrainOutcome, ClientError> {
    let pipe_name = cfg.env.pipe_name();
    let deadline = Instant::now() + within;
    let mut backoff = Backoff::new();
    let pipe = loop {
        match open_client(&pipe_name) {
            Ok(pipe) => break pipe,
            Err(ConnectError::NotFound) => return Ok(DrainOutcome::NotRunning),
            Err(ConnectError::Busy) => {}
            Err(ConnectError::Io(e)) => return Err(ClientError::Io(e)),
        }
        if Instant::now() >= deadline {
            return Err(ClientError::Timeout);
        }
        backoff.pause().await;
    };
    let gone = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
    let inspected = match server_pid(&pipe) {
        Ok(pid) => SystemInspector
            .inspect(pid)
            .map_err(|e| unverifiable(pid, &e)),
        Err(e) if gone(&e) => Err(None),
        Err(e) => return Err(ClientError::Io(e)),
    };
    match inspected {
        Ok(info) => check_user(&info)?,
        // A server that cannot be verified is never asked (ARCH-027).
        Err(Some(untrusted)) => return Err(untrusted),
        // The server went away under us.
        Err(None) => {
            drop(pipe);
            wait_gone(&pipe_name, deadline).await?;
            return Ok(DrainOutcome::Stopped);
        }
    }

    let conn = Conn::start(pipe);
    let hello = hello_params(cfg, NO_WORKSPACE);
    match timeout_at(deadline, conn.call(method::HELLO, hello)).await {
        Err(_) => return Err(ClientError::Timeout),
        Ok(Err(ClientError::Closed)) => {}
        Ok(Err(e)) => return Err(e),
        Ok(Ok(result)) => match decode::<HelloResult>(result, method::HELLO)? {
            Outcome::Ok(_) => {}
            // The core decides about the drain from the refused connection's major.
            Outcome::Error { error } if error.code == PROTOCOL_INCOMPATIBLE.code => {}
            Outcome::Error { error } => return Err(ClientError::Refused(error)),
            Outcome::PendingConfirmation(_) => return Err(unexpected_pending(method::HELLO)),
        },
    }
    send_drain(&conn, deadline).await?;
    drop(conn);
    wait_gone(&pipe_name, deadline).await?;
    Ok(DrainOutcome::Stopped)
}

/// Sends `core/drain` on `conn`. A connection that closes instead of answering counts as
/// accepted (the core is going); the frozen refusal is [`ClientError::Incompatible`].
async fn send_drain(conn: &Conn, deadline: Instant) -> Result<(), ClientError> {
    match timeout_at(deadline, conn.call(method::DRAIN, json!({}))).await {
        Err(_) => Err(ClientError::Timeout),
        Ok(Err(ClientError::Closed)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Ok(Ok(result)) => match decode::<DrainResult>(result, method::DRAIN)? {
            Outcome::Ok(_) => Ok(()),
            Outcome::Error { error } if error.code == PROTOCOL_INCOMPATIBLE.code => {
                Err(ClientError::Incompatible {
                    core_protocol: core_protocol(&error.data).unwrap_or_default(),
                })
            }
            Outcome::Error { error } => Err(ClientError::Refused(error)),
            Outcome::PendingConfirmation(_) => Err(unexpected_pending(method::DRAIN)),
        },
    }
}

/// Polls the pipe until it no longer exists; [`ClientError::Timeout`] at `deadline`.
async fn wait_gone(pipe_name: &str, deadline: Instant) -> Result<(), ClientError> {
    let mut backoff = Backoff::new();
    loop {
        match open_client(pipe_name) {
            Err(ConnectError::NotFound) => return Ok(()),
            // A draining core may still accept a connection; it is closed again at once.
            Ok(pipe) => drop(pipe),
            Err(ConnectError::Busy) => {}
            Err(ConnectError::Io(e)) => return Err(ClientError::Io(e)),
        }
        if Instant::now() >= deadline {
            return Err(ClientError::Timeout);
        }
        backoff.pause().await;
    }
}

/// One find-verify-hello pass.
async fn handshake(cfg: &ClientConfig) -> Result<Handshake, ClientError> {
    let mut finder = Finder::new(cfg);
    let pipe = finder.open().await?;
    handshake_from(cfg, &mut finder, pipe).await
}

/// Verify and hello on `pipe`, which `finder` opened. A connection that closes before the
/// hello reply without a start failure to report (another client's core that exited, or this
/// client's own core stopped normally) sends the client round the find loop once more, within
/// the same deadline: hello has no side effects, and a core that cannot start is then this
/// client's own, whose exit code is reported.
async fn handshake_from(
    cfg: &ClientConfig,
    finder: &mut Finder<'_>,
    pipe: NamedPipeClient,
) -> Result<Handshake, ClientError> {
    match greet(cfg, finder, pipe).await {
        Err(ClientError::Closed) => {
            let pipe = finder.open().await?;
            greet(cfg, finder, pipe).await
        }
        other => other,
    }
}

/// Steps 2 and 3 on the pipe `finder` opened: verify the server, then say hello.
async fn greet(
    cfg: &ClientConfig,
    finder: &mut Finder<'_>,
    pipe: NamedPipeClient,
) -> Result<Handshake, ClientError> {
    // ARCH-027: the server process is opened at once, before anything is sent.
    let pid = match server_pid(&pipe) {
        Ok(pid) => pid,
        Err(_) => return Err(finder.closed_early(None).await),
    };
    let info = match SystemInspector.inspect(pid) {
        Ok(info) => info,
        Err(e) => match unverifiable(pid, &e) {
            Some(untrusted) if !finder.spawned_pid(pid) => return Err(untrusted),
            // Gone (the connect race), or this client's own core.
            _ => return Err(finder.closed_early(Some(pid)).await),
        },
    };
    check_user(&info)?;
    check_dir(cfg, &info)?;

    let conn = Conn::start(pipe);
    let hello = hello_params(cfg, &cfg.workspace);
    let result = match timeout_at(finder.hello_deadline(pid), conn.call(method::HELLO, hello)).await
    {
        Err(_) => return Err(ClientError::Timeout),
        Ok(Err(ClientError::Closed)) => return Err(finder.closed_early(Some(pid)).await),
        Ok(Err(e)) => return Err(e),
        Ok(Ok(result)) => result,
    };
    match decode::<HelloResult>(result, method::HELLO)? {
        Outcome::Ok(hello) => Ok(Handshake::Ready(Box::new(Client { conn, hello }))),
        Outcome::Error { error } if error.code == PROTOCOL_INCOMPATIBLE.code => {
            match core_protocol(&error.data) {
                Some(core_protocol) => Ok(Handshake::Incompatible {
                    core_protocol,
                    conn,
                }),
                None => Err(ClientError::Refused(error)),
            }
        }
        Outcome::Error { error } if error.code == WORKSPACE_UNKNOWN.code => {
            match &cfg.workspace_origin {
                Some(origin) => Err(ClientError::UnknownWorkspace {
                    origin: origin.clone(),
                    default: crate::workspace::registered_default(&cfg.env),
                    error,
                }),
                None => Err(ClientError::Refused(error)),
            }
        }
        Outcome::Error { error } => Err(ClientError::Refused(error)),
        Outcome::PendingConfirmation(_) => Err(unexpected_pending(method::HELLO)),
    }
}

/// Finds the core's pipe, starting the core when allowed (step 1 of the module docs).
struct Finder<'a> {
    cfg: &'a ClientConfig,
    pipe_name: String,
    started: Instant,
    /// The core this client spawned, while it may still be the one starting.
    spawned: Option<ProcessHandle>,
}

impl<'a> Finder<'a> {
    fn new(cfg: &'a ClientConfig) -> Finder<'a> {
        Finder {
            cfg,
            pipe_name: cfg.env.pipe_name(),
            started: Instant::now(),
            spawned: None,
        }
    }

    /// Whether `pid` is the core this client spawned.
    fn spawned_pid(&self, pid: u32) -> bool {
        self.spawned.as_ref().is_some_and(|h| h.pid() == pid)
    }

    fn spawned_alive(&self) -> bool {
        self.spawned
            .as_ref()
            .is_some_and(|h| matches!(h.exit_code(), Ok(None)))
    }

    /// `start_timeout`, extended up to `start_hard_cap` while the spawned core is alive.
    fn deadline(&self) -> Instant {
        let budget = if self.spawned_alive() {
            self.cfg.start_hard_cap.max(self.cfg.start_timeout)
        } else {
            self.cfg.start_timeout
        };
        self.started + budget
    }

    /// The hello may take until the start's hard cap when this client started the core
    /// serving `pid` (it may still be opening its state), else `start_timeout` from now.
    fn hello_deadline(&self, pid: u32) -> Instant {
        match &self.spawned {
            Some(h) if h.pid() == pid => {
                self.started + self.cfg.start_hard_cap.max(self.cfg.start_timeout)
            }
            _ => Instant::now() + self.cfg.start_timeout,
        }
    }

    async fn open(&mut self) -> Result<NamedPipeClient, ClientError> {
        let mut backoff = Backoff::new();
        loop {
            match open_client(&self.pipe_name) {
                Ok(pipe) => return Ok(pipe),
                // A connection took the free instance; the core makes the next one at once.
                Err(ConnectError::Busy) => {}
                Err(ConnectError::NotFound) => self.not_found()?,
                Err(ConnectError::Io(e)) => return Err(ClientError::Io(e)),
            }
            if Instant::now() >= self.deadline() {
                return Err(ClientError::Timeout);
            }
            backoff.pause().await;
        }
    }

    /// No pipe: wait for the core this client started, or start one.
    fn not_found(&mut self) -> Result<(), ClientError> {
        if let Some(handle) = &self.spawned {
            match handle.exit_code() {
                // Still starting.
                Ok(None) => return Ok(()),
                // It lost the race and the winner is gone too, or it ran and was stopped
                // (drained by another client): not a start failure; start another below.
                Ok(Some(LOST_RACE | NORMAL_EXIT)) => self.spawned = None,
                Ok(Some(code)) => return Err(self.start_failed(Some(code))),
                Err(e) => return Err(ClientError::Io(e)),
            }
        }
        match self.cfg.start {
            StartPolicy::NoStart => Err(ClientError::NotRunning),
            StartPolicy::StartIfNeeded => {
                let exe = &self.cfg.core_exe;
                let handle = spawn_detached(&SpawnRequest {
                    exe: exe.clone(),
                    args: Vec::new(),
                    extra_env: self.cfg.spawn_env.clone(),
                    remove_env: Vec::new(),
                })
                .map_err(|e| {
                    ClientError::Io(io::Error::new(
                        e.kind(),
                        format!("Cannot start the LILITH core {}: {e}", exe.display()),
                    ))
                })?;
                self.spawned = Some(handle);
                Ok(())
            }
        }
    }

    /// The connection to the core serving `server_pid` (if known) closed before the hello
    /// reply, or the server could not be inspected. When this client started that core, its
    /// exit code says why (it claims the pipe before it opens the suite `state.db`).
    async fn closed_early(&mut self, server_pid: Option<u32>) -> ClientError {
        let Some(handle) = &self.spawned else {
            return ClientError::Closed;
        };
        if server_pid.is_some_and(|pid| pid != handle.pid()) {
            return ClientError::Closed;
        }
        match timeout(EARLY_CLOSE_WAIT, handle.wait()).await {
            // It lost the race, or ran and was stopped normally (another client drained it
            // inside the hello window): not a start failure. Forgotten, so a retry of the
            // find loop may start a new one.
            Ok(Ok(LOST_RACE | NORMAL_EXIT)) => {
                self.spawned = None;
                ClientError::Closed
            }
            Ok(Ok(code)) => self.start_failed(Some(code)),
            _ => ClientError::Closed,
        }
    }

    fn start_failed(&self, exit_code: Option<u32>) -> ClientError {
        ClientError::StartFailed {
            exit_code,
            kind: StartFailure::from_exit_code(exit_code),
            state_db: self.cfg.env.suite_state_db(),
            logs_dir: self.cfg.env.logs_dir(),
        }
    }
}

/// 25 ms, doubling to 250 ms.
struct Backoff(Duration);

impl Backoff {
    fn new() -> Backoff {
        Backoff(BACKOFF_START)
    }

    async fn pause(&mut self) {
        sleep(self.0).await;
        self.0 = (self.0 * 2).min(BACKOFF_MAX);
    }
}

/// What an `inspect` failure for the server `pid` means. `None`: the process is gone (the
/// connect race). Anything else (another user's process that cannot be opened, say) makes
/// the server untrusted (ARCH-027), named by its PID.
fn unverifiable(pid: u32, e: &io::Error) -> Option<ClientError> {
    if e.kind() == io::ErrorKind::NotFound {
        return None;
    }
    Some(ClientError::Untrusted {
        pid,
        // Unknown: the process could not be opened to read it.
        image: PathBuf::new(),
        reason: format!("it cannot be verified: {e}"),
    })
}

/// ARCH-027: the server runs as the current user.
fn check_user(info: &ProcessInfo) -> Result<(), ClientError> {
    let me = current_user_sid().map_err(ClientError::Io)?;
    if info.user_sid.eq_ignore_ascii_case(&me) {
        Ok(())
    } else {
        Err(ClientError::Untrusted {
            pid: info.pid,
            image: info.image.clone(),
            reason: "it runs as another user".to_string(),
        })
    }
}

/// ARCH-027: the server's executable is in one of `server_dirs`.
fn check_dir(cfg: &ClientConfig, info: &ProcessInfo) -> Result<(), ClientError> {
    let trusted = info.image.parent().is_some_and(|dir| {
        let dir = folder_key(dir);
        !dir.is_empty() && cfg.server_dirs.iter().any(|d| folder_key(d) == dir)
    });
    if trusted {
        return Ok(());
    }
    let dirs: Vec<String> = cfg
        .server_dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect();
    Err(ClientError::Untrusted {
        pid: info.pid,
        image: info.image.clone(),
        reason: format!("{WRONG_FOLDER} ({})", dirs.join("; ")),
    })
}

/// The `core/hello` params of `cfg`, naming `workspace`.
fn hello_params(cfg: &ClientConfig, workspace: &str) -> Value {
    to_value(&HelloParams {
        protocol: cfg.protocol.to_string(),
        build_id: cfg.build_id.clone(),
        schema_hash: schema_hash(),
        capabilities: CAPABILITIES.iter().map(|c| (*c).to_string()).collect(),
        caller_kind: cfg.caller_kind.to_string(),
        non_interactive: cfg.non_interactive,
        workspace: workspace.to_string(),
        engine_token: None,
        profile_id: None,
    })
}

/// `data.core_protocol` of a `protocol.incompatible` refusal.
fn core_protocol(data: &Option<Value>) -> Option<String> {
    data.as_ref()?
        .get("core_protocol")?
        .as_str()
        .map(str::to_string)
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("protocol params serialise")
}

/// A method's `result` as its `Outcome<T>`.
fn decode<T: DeserializeOwned>(result: Value, method: &str) -> Result<Outcome<T>, ClientError> {
    serde_json::from_value(result).map_err(|e| {
        ClientError::Protocol(RpcError::new(
            rpc_code::PARSE_ERROR,
            format!("the core's {method} result is not readable: {e}"),
        ))
    })
}

fn unexpected_pending(method: &str) -> ClientError {
    ClientError::Protocol(RpcError::new(
        rpc_code::INVALID_REQUEST,
        format!("the core answered {method} with pending_confirmation, which it never does"),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lil_protocol::CallerKind;
    use lil_testkit::{TestEnv, ensure_built, sha256_file};

    use super::*;

    /// The connect race of step 3, met on purpose: the core claims its pipe before it checks
    /// the suite `state.db`, and releases it before it exits. The pipe is opened the moment it
    /// exists, so every connection here is made before the core exits (a loop of plain
    /// connects at the production backoff rarely lands in that window). Each must still be the
    /// start failure, never `Closed` or `Io`.
    #[test] // spec: CORE-004
    fn connection_made_before_a_failing_core_exits_is_a_start_failure() {
        const DEADLINE: std::time::Duration = Duration::from_secs(30);
        let env = TestEnv::new();
        let bins = ensure_built();
        let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        cfg.core_exe = bins.lil_core.clone();
        cfg.server_dirs = vec![bins.dir.clone()];
        cfg.spawn_env = env.child_env();
        let db = env.env.suite_state_db();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
        let before = sha256_file(&db);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut met = 0;
        for round in 0..20 {
            let mut finder = Finder::new(&cfg);
            finder.not_found().expect("spawn the core");
            let pipe = rt.block_on(async {
                let deadline = Instant::now() + DEADLINE;
                loop {
                    match open_client(&finder.pipe_name) {
                        Ok(pipe) => return Some(pipe),
                        Err(ConnectError::NotFound | ConnectError::Busy) => {}
                        Err(ConnectError::Io(e)) => panic!("round {round}: {e}"),
                    }
                    // Exited before the pipe was seen: no connection this round.
                    if !finder.spawned_alive() {
                        return None;
                    }
                    assert!(Instant::now() < deadline, "round {round}: no pipe, no exit");
                    tokio::task::yield_now().await;
                }
            });
            let Some(pipe) = pipe else { continue };
            met += 1;
            let result = rt.block_on(async {
                tokio::time::timeout(DEADLINE, greet(&cfg, &mut finder, pipe)).await
            });
            match result.expect("greet did not finish within the deadline") {
                Err(ClientError::StartFailed {
                    exit_code: Some(5),
                    kind: StartFailure::StateDamaged,
                    state_db,
                    ..
                }) => assert_eq!(state_db, db),
                Err(other) => panic!("round {round}: expected StartFailed(5), got {other:?}"),
                Ok(_) => panic!("round {round}: a damaged core said hello"),
            }
        }
        assert!(met > 0, "no round connected before the core exited");
        assert_eq!(sha256_file(&db), before, "{} was changed", db.display());
    }

    /// A `HelloResult` for a client whose connection goes to a fake server.
    fn fake_hello() -> HelloResult {
        HelloResult {
            session_id: "s_0000000000000001".into(),
            instance_id: "c_0000000000000001".into(),
            protocol: "1.0".into(),
            build_id: "fake".into(),
            schema_hash: schema_hash(),
            capabilities: vec!["events.v1".into()],
            caller_kind: "cli".into(),
            workspace: None,
            core: lil_protocol::CoreInfo {
                pid: std::process::id(),
                channel: "test-fake".into(),
                pipe: "fake".into(),
            },
        }
    }

    /// A client connected to a fake server on a pipe of its own, named after `label`.
    async fn fake_pair(label: &str) -> (Client, NamedPipeServer) {
        let name = format!(r"\\.\pipe\lil-client-unit-{}-{label}", std::process::id());
        let server = lil_platform::pipe::create_instance(&name, true).expect("fake server");
        let pipe = open_client(&name).expect("open the fake server");
        server.connect().await.expect("accept");
        let client = Client {
            conn: Conn::start(pipe),
            hello: fake_hello(),
        };
        (client, server)
    }

    /// A line from the core that is not the protocol ends the connection: the call waiting
    /// for its reply gets `Closed` instead of hanging.
    #[test] // spec: ARCH-025
    fn an_unreadable_line_closes_the_connection() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (client, server) = fake_pair("garbage").await;
            let fake = tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(server);
                let mut lines = BufReader::new(read).lines();
                lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("the tools/list request");
                write.write_all(b"this is not JSON\n").await.unwrap();
                write.flush().await.unwrap();
                // The fake keeps its end open: only the client may end the connection.
                let _ = lines.next_line().await;
            });
            let result = tokio::time::timeout(Duration::from_secs(10), client.list_tools())
                .await
                .expect("the call hung after an unreadable line");
            assert!(matches!(result, Err(ClientError::Closed)), "{result:?}");
            drop(client);
            fake.await.expect("the fake server");
        });
    }

    /// Inspecting the server fails for another user's process (access denied): that server is
    /// untrusted and named by its PID, never a closed connection. Only a process that is gone
    /// is the connect race.
    #[test] // spec: ARCH-027
    fn inspect_failures_are_untrusted_unless_the_process_is_gone() {
        let denied = io::Error::from_raw_os_error(5);
        match unverifiable(4242, &denied) {
            Some(ClientError::Untrusted { pid, image, reason }) => {
                assert_eq!(pid, 4242);
                assert!(image.as_os_str().is_empty(), "{}", image.display());
                assert!(reason.contains("cannot be verified"), "{reason}");
                let text = ClientError::Untrusted { pid, image, reason }.to_string();
                assert!(text.contains("4242"), "{text}");
                assert!(!text.contains("lil core stop"), "{text}");
                assert!(!text.contains("by  ("), "{text}");
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }
        let gone = io::Error::new(io::ErrorKind::NotFound, "process 4242 is not running");
        assert!(unverifiable(4242, &gone).is_none());
    }

    /// Two clients start at once on a damaged `state.db`, and this one connects to the
    /// *other* client's core, which then exits: it retries the find loop once, starts its own
    /// core and reports that core's start failure, not `Closed`.
    #[test] // spec: CORE-004
    fn early_close_of_another_clients_core_retries_into_the_start_failure() {
        const DEADLINE: std::time::Duration = Duration::from_secs(30);
        let env = TestEnv::new();
        let bins = ensure_built();
        let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        cfg.core_exe = bins.lil_core.clone();
        cfg.server_dirs = vec![bins.dir.clone()];
        cfg.spawn_env = env.child_env();
        let db = env.env.suite_state_db();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut met = 0;
        for round in 0..5 {
            // The other client's core, connected to the moment its pipe exists.
            let mut other = Finder::new(&cfg);
            other.not_found().expect("spawn the other core");
            let pipe = rt.block_on(async {
                let deadline = Instant::now() + DEADLINE;
                loop {
                    match open_client(&other.pipe_name) {
                        Ok(pipe) => return Some(pipe),
                        Err(ConnectError::NotFound | ConnectError::Busy) => {}
                        Err(ConnectError::Io(e)) => panic!("round {round}: {e}"),
                    }
                    if !other.spawned_alive() {
                        return None;
                    }
                    assert!(Instant::now() < deadline, "round {round}: no pipe, no exit");
                    tokio::task::yield_now().await;
                }
            });
            let Some(pipe) = pipe else { continue };
            met += 1;
            let mut finder = Finder::new(&cfg);
            let result = rt.block_on(async {
                tokio::time::timeout(DEADLINE, handshake_from(&cfg, &mut finder, pipe)).await
            });
            match result.expect("the handshake did not finish within the deadline") {
                Err(ClientError::StartFailed {
                    exit_code: Some(5),
                    kind: StartFailure::StateDamaged,
                    ..
                }) => {}
                Err(other) => panic!("round {round}: expected StartFailed(5), got {other:?}"),
                Ok(_) => panic!("round {round}: a damaged core said hello"),
            }
            let other_core = other.spawned.take().expect("the other core");
            rt.block_on(async { tokio::time::timeout(DEADLINE, other_core.wait()).await })
                .expect("the other core did not exit")
                .unwrap();
        }
        assert!(met > 0, "no round connected before the other core exited");
    }

    /// The core may queue a subscription's replayed events before its `lil/subscribe` reply
    /// (Task 11). A fake server sends them in that order, plus an event of a topic not
    /// subscribed: the stream gets every replayed and later event in order, nothing else, and
    /// ends when the server goes away.
    #[test] // spec: ARCH-032
    fn replayed_events_before_the_subscribe_reply_are_kept_in_order() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (client, server) = fake_pair("replay").await;

            let fake = tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(server);
                let mut lines = BufReader::new(read).lines();
                let request: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(request["method"], "lil/subscribe");
                // The stray cursor was dropped (ruling P21).
                assert_eq!(
                    request["params"],
                    json!({"topics": ["test:r"], "since_seq": {"test:r": 0}})
                );
                let event = |topic: &str, seq: u64| {
                    let params = json!({"topic": topic, "seq": seq, "kind": "event", "data": {"n": seq}});
                    format!("{}\n", lil_protocol::encode_notification(method::EVENT, &params))
                };
                let mut out = String::new();
                for seq in 1..=3 {
                    out.push_str(&event("test:r", seq));
                }
                out.push_str(&event("test:other", 1));
                out.push_str(&format!(
                    "{}\n",
                    json!({"jsonrpc": "2.0", "id": request["id"], "result": {"status": "ok", "topics": [{"topic": "test:r", "seq": 3}]}})
                ));
                out.push_str(&event("test:r", 4));
                write.write_all(out.as_bytes()).await.unwrap();
                write.flush().await.unwrap();
                // Dropping both halves closes the fake server's end.
            });

            let since = BTreeMap::from([("test:r".to_string(), 0), ("test:stray".to_string(), 9)]);
            let mut stream = client
                .subscribe(vec!["test:r".to_string()], since)
                .await
                .expect("lil/subscribe");
            fake.await.expect("the fake server");
            assert_eq!(stream.topics()[0].seq, 3);
            let mut seqs = Vec::new();
            while let Some(event) = tokio::time::timeout(Duration::from_secs(30), stream.next())
                .await
                .expect("the stream neither ended nor delivered")
            {
                assert_eq!(event.topic, "test:r");
                assert_eq!(event.data, json!({"n": event.seq}));
                seqs.push(event.seq);
            }
            assert_eq!(seqs, [1, 2, 3, 4]);
        });
    }

    #[test] // spec: ARCH-027
    fn folder_keys_ignore_case_prefix_and_trailing_separator() {
        assert_eq!(
            folder_key(Path::new(r"\\?\Z:\No Such\Bin\")),
            folder_key(Path::new(r"z:/no such/bin"))
        );
        assert_eq!(folder_key(Path::new(r"Z:\")), r"z:\");
        assert_ne!(
            folder_key(Path::new(r"Z:\no such\bin")),
            folder_key(Path::new(r"Z:\no such\other bin"))
        );
    }
}
