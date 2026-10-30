//! One instance per app (APP-013, APP-014; Appendix G "Apps"; P4a-R9, P4a-R11), Qt-free.
//!
//! Each app owns the per-user pipe `Env::app_pipe_name(app, false)`, and its confirm window
//! `Env::app_pipe_name(app, true)`, each created as a first instance with the core pipe's
//! user-only security descriptor. The exchange, each message one JSON line of at most 64 KiB:
//! 1. the running instance ([`InstanceServer`]) writes `{"app": "<app>", "pid": <pid>}` when a
//!    client connects;
//! 2. the second launch ([`forward`]) checks the pipe's reported server as ARCH-027 says, this
//!    exe as this user, and that it isn't this process and didn't start after it (else
//!    [`ForwardError::Untrusted`], and nothing is written), then has it prove it holds the pipe:
//!    it writes `{"prove": "<name>"}`, the name of a fresh event ([`ProofEvent`]); the instance
//!    sets that event and answers `{"proved": true}`, and without the event set nothing more is
//!    written (`GetNamedPipeServerProcessId` names the pipe's creator, even after it has died
//!    and the handle has passed to another, so a pid can't vouch for a holder);
//! 3. the second launch calls `AllowSetForegroundWindow(pid)` unless the launch never comes
//!    forward (a deny, `--no-activate`), and writes `{"args": [<argument>, …]}`;
//! 4. the running instance reads it, answers `{"ok": true}` and queues the arguments
//!    ([`InstanceServer::next`]); the second launch then exits 0. An instance that is closing
//!    ([`InstanceServer::close`]) answers `{"ok": false, "closing": true}` instead, and queues
//!    nothing: the second launch waits for it to go and becomes the instance itself.
//!
//! The second launch gives up 2 s after it started ([`ForwardError::Timeout`]). The server takes
//! one client at a time, in the order they connect, each within 2 s.
//!
//! [`start`] is what a start does before Qt: which pipe a plan goes to, and the deny of a toast
//! (P4a-R9), which never shows a window: it goes to the running app, else to a running confirm
//! window, else this process sends it itself with [`send_deny`] (`StartPolicy::NoStart`, so a
//! stale toast's Deny starts no core, and at most 10 s in all). When another program holds the
//! pipe (it can't be opened, or its server isn't this exe as this user), the start goes on as an
//! instance of its own, without the pipe, and says so ([`BootPlan::pipe_notice`]).
//!
//! **Threads.** [`forward`] and [`send_deny`] each run on a current-thread tokio runtime of their
//! own, made and shut down within the call, so nothing of theirs is left when they return. A
//! claimed [`InstanceServer`] makes its pipe and serves it on one thread of its own
//! (`lil-instance`), with that thread's own current-thread runtime: nothing tokio runs on the
//! caller's thread. Dropping the server stops and joins the thread.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lil_client::{Client, ClientConfig, ClientError, StartPolicy};
use lil_platform::Env;
use lil_platform::pipe::{
    ConnectError, CreateError, ProofEvent, client_pid, create_instance, open_client, server_pid,
    set_proof_event,
};
use lil_platform::process::{
    PeerInfo, current_user_sid, inspect_peer, own_integrity_rid, own_start_time,
};
use lil_protocol::Outcome;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::Notify;

use crate::args::{BootPlan, Mode, Opened};

/// The longest message line, its newline included.
pub const MESSAGE_MAX: usize = 64 * 1024;
/// How long a second launch tries, and how long the server gives one client.
pub const BUDGET: Duration = Duration::from_secs(2);
/// The longest a toast's deny may take, connection included.
pub const DENY_BOUND: Duration = Duration::from_secs(10);
/// How long a launch waits for a closing instance to go before it starts without the pipe:
/// longer than the instance's end may take (its denies in flight, then its last exchange), so
/// a launch never gives up just before the pipe goes (review M-13).
pub const CLOSING_WAIT: Duration = Duration::from_secs(15);
/// What the shell's banner says when another program holds the pipe (review I-2, M-10).
pub const NOTICE_HELD: &str = "Another program is holding LILITH's launch channel, so a second \
                               launch may open a second window.";
/// What it says when LILITH's own instance never answered (review M-14).
pub const NOTICE_BUSY: &str =
    "The running LILITH didn't answer in time, so this launch opened a window of its own.";
/// What it says when LILITH's own instance refused this launch (review M-21).
pub const NOTICE_NOT_ACCEPTED: &str =
    "The running LILITH didn't accept this launch, so it opened a window of its own.";
/// What it says when this start can't tell who it is, so trusts no instance (review M-21).
pub const NOTICE_SELF_UNKNOWN: &str = "LILITH couldn't check which program it is, so this \
                                       launch opened a window of its own.";
/// What it says when LILITH's own instance was still closing after [`CLOSING_WAIT`] (M-13).
pub const NOTICE_CLOSING: &str =
    "The running LILITH was still closing, so this launch opened a window of its own.";
/// The wait between tries while every instance of the pipe is busy.
const BUSY_RETRY: Duration = Duration::from_millis(10);
/// The wait between tries while the running instance closes.
const CLOSING_RETRY: Duration = Duration::from_millis(50);

/// The arguments a second launch handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forwarded {
    pub args: Vec<String>,
}

/// Why [`forward`] handed nothing over.
#[derive(Debug)]
pub enum ForwardError {
    /// No instance owns the pipe.
    NotRunning,
    /// The pipe is held by something this launch doesn't trust: it can't be opened (a DACL that
    /// leaves this user out), or its server isn't this executable run as this user at this
    /// process's integrity level or above, is this very process, started after it, or can't be
    /// inspected, or its holder didn't prove it is that server. No argument was written.
    Untrusted,
    /// Every instance of the pipe stayed busy for the whole budget: nothing served it.
    Busy,
    /// LILITH's own instance (this exe, this user, checked) didn't accept this launch: it can't
    /// tell who it is, or it runs at a higher integrity level (review M-21).
    NotAccepted,
    /// This process can't tell who it is, so it trusts no peer (review M-21).
    SelfUnknown,
    /// The running instance is closing: it took nothing.
    Closing,
    /// No exchange within [`BUDGET`].
    Timeout,
    /// The server answered something else than the exchange.
    Refused,
    Io(io::Error),
}

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardError::NotRunning => f.write_str("no instance is running"),
            ForwardError::Untrusted => f.write_str(
                "the pipe is held by another program (it can't be opened, or its server isn't \
                 this exe run as this user at this integrity level or above)",
            ),
            ForwardError::Busy => {
                write!(f, "every instance of the pipe stayed busy for {BUDGET:?}")
            }
            ForwardError::Closing => f.write_str("the running instance is closing"),
            ForwardError::NotAccepted => {
                f.write_str("the running instance didn't accept this launch")
            }
            ForwardError::SelfUnknown => f.write_str("this process can't tell who it is"),
            ForwardError::Timeout => {
                write!(f, "the running instance didn't answer within {BUDGET:?}")
            }
            ForwardError::Refused => f.write_str("the running instance refused the arguments"),
            ForwardError::Io(e) => write!(f, "the pipe failed: {e}"),
        }
    }
}

impl std::error::Error for ForwardError {}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// The running instance's side of the pipe.
pub struct InstanceServer {
    rx: Mutex<mpsc::Receiver<Forwarded>>,
    closing: Arc<AtomicBool>,
    stop: Arc<Notify>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for InstanceServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceServer").finish_non_exhaustive()
    }
}

impl InstanceServer {
    /// Creates `app`'s pipe (its confirm window's with `confirm`) as its first instance and
    /// serves it: `None` when another process owns it. The pipe is made on the server's own
    /// thread, which ends at once when the pipe isn't claimed.
    ///
    /// # Errors
    /// When the pipe can't be created for another reason, or the thread can't start.
    pub fn claim(env: &Env, app: &str, confirm: bool) -> io::Result<Option<InstanceServer>> {
        let name = env.app_pipe_name(app, confirm);
        let (tx, rx) = mpsc::channel();
        let (claimed_tx, claimed_rx) = mpsc::channel::<io::Result<bool>>();
        let closing = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(Notify::new());
        let app = app.to_string();
        let thread = {
            let (closing, stop) = (closing.clone(), stop.clone());
            std::thread::Builder::new()
                .name("lil-instance".into())
                .spawn(move || {
                    let rt = match runtime() {
                        Ok(rt) => rt,
                        Err(e) => {
                            let _ = claimed_tx.send(Err(e));
                            return;
                        }
                    };
                    let first = {
                        let _in = rt.enter();
                        create_instance(&name, true)
                    };
                    match first {
                        Ok(first) => {
                            let _ = claimed_tx.send(Ok(true));
                            // Who a client must be (ARCH-027): unknown, and none is taken.
                            let me = Identity::of_this_process()
                                .inspect_err(|e| {
                                    tracing::error!(error = %e, "this process can't tell who it is: no launch is taken");
                                })
                                .ok();
                            rt.block_on(serve(first, name, app, me, tx, closing, stop));
                        }
                        Err(CreateError::AlreadyOwned) => {
                            let _ = claimed_tx.send(Ok(false));
                        }
                        Err(CreateError::Io(e)) => {
                            let _ = claimed_tx.send(Err(e));
                        }
                    }
                    // The runtime is dropped here, on its own thread.
                })?
        };
        let claimed = claimed_rx
            .recv()
            .unwrap_or_else(|_| Err(io::Error::other("the instance thread ended early")));
        match claimed {
            Ok(true) => Ok(Some(InstanceServer {
                rx: Mutex::new(rx),
                closing,
                stop,
                thread: Mutex::new(Some(thread)),
            })),
            other => {
                let _ = thread.join();
                other.map(|_| None)
            }
        }
    }

    /// The next arguments a second launch handed over, in the order they came; `None` when
    /// none is waiting.
    pub fn next(&self) -> Option<Forwarded> {
        self.rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .ok()
    }

    /// [`InstanceServer::next`], waiting at most `within` for one.
    pub fn next_within(&self, within: Duration) -> Option<Forwarded> {
        self.rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv_timeout(within)
            .ok()
    }

    /// From now on every launch is answered "closing" and taken by none: it waits for this
    /// instance to go and becomes the instance itself. What was taken before is still
    /// [`InstanceServer::next`]'s.
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    /// Closes the pipe and joins its thread, keeping what was taken: once this returns, nothing
    /// more is taken, and [`InstanceServer::next`] has every launch that was (review M-12).
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.stop.notify_one();
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for InstanceServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn serve(
    first: NamedPipeServer,
    name: String,
    app: String,
    me: Option<Identity>,
    tx: mpsc::Sender<Forwarded>,
    closing: Arc<AtomicBool>,
    stop: Arc<Notify>,
) {
    let mut server = first;
    loop {
        let connected = tokio::select! {
            connected = server.connect() => connected,
            () = stop.notified() => return,
        };
        // The next instance exists before this client is read, so the next launch can connect.
        let next = loop {
            match create_instance(&name, false) {
                Ok(next) => break next,
                Err(e) => {
                    tracing::warn!(error = %e, "the app's pipe has no new instance yet");
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(100)) => {}
                        () = stop.notified() => return,
                    }
                }
            }
        };
        let client = std::mem::replace(&mut server, next);
        if let Err(e) = connected {
            tracing::warn!(error = %e, "a launch's connection to the app's pipe failed");
            continue;
        }
        let exchanged = exchange(client, &app, me.as_ref(), &closing);
        match tokio::time::timeout(BUDGET, exchanged).await {
            Ok(Ok(Some(args))) => {
                if tx.send(Forwarded { args }).is_err() {
                    return;
                }
            }
            Ok(Ok(None)) => tracing::info!("a launch came while the app closes: told it so"),
            Ok(Err(e)) => tracing::warn!(error = %e, "a launch's arguments were refused"),
            Err(_) => tracing::warn!("a launch didn't hand its arguments over within 2 s"),
        }
    }
}

/// The server's side of one exchange: the arguments, once answered `{"ok": true}`, or `None`
/// once answered "closing". A client that isn't this exe, run as this user at this process's
/// integrity level or above, is answered `{"untrusted": true}` and nothing more (review M-16,
/// I-6): a second launch always is.
async fn exchange(
    mut client: NamedPipeServer,
    app: &str,
    me: Option<&Identity>,
    closing: &AtomicBool,
) -> io::Result<Option<Vec<String>>> {
    let refusal = match (me, client_pid(&client)) {
        (Some(me), Ok(pid)) => judged(pid, me, identity_refusal),
        (None, _) => Some("this process can't tell who it is".to_string()),
        (_, Err(e)) => Some(format!("its pid can't be read: {e}")),
    };
    if let Some(why) = refusal {
        write_line(&mut client, &json!({"untrusted": true})).await?;
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
    }
    let hello = json!({"app": app, "pid": std::process::id()});
    write_line(&mut client, &hello).await?;
    let (reader, mut writer) = tokio::io::split(client);
    let mut reader = BufReader::new(reader);
    // The proof that this process holds the pipe, before any argument comes.
    let message = read_line(&mut reader).await?;
    let name = message
        .get("prove")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not {\"prove\": name}"))?;
    let proved = set_proof_event(name);
    write_line(&mut writer, &json!({"proved": proved.is_ok()})).await?;
    proved?;
    let message = read_line(&mut reader).await?;
    let args = message
        .get("args")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not {\"args\": [string]}"))?;
    if closing.load(Ordering::SeqCst) {
        write_line(&mut writer, &json!({"ok": false, "closing": true})).await?;
        return Ok(None);
    }
    write_line(&mut writer, &json!({"ok": true})).await?;
    Ok(Some(args))
}

async fn write_line(w: &mut (impl AsyncWrite + Unpin), v: &Value) -> io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    if line.len() > MESSAGE_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the message is longer than 64 KiB",
        ));
    }
    w.write_all(line.as_bytes()).await?;
    w.flush().await
}

/// One JSON line of at most [`MESSAGE_MAX`] bytes.
async fn read_line(r: &mut BufReader<impl AsyncRead + Unpin>) -> io::Result<Value> {
    let mut line = Vec::new();
    loop {
        let available = r.fill_buf().await?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the pipe closed mid-message",
            ));
        }
        let (take, done) = match available.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (available.len(), false),
        };
        line.extend_from_slice(&available[..take]);
        r.consume(take);
        if line.len() > MESSAGE_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a message is longer than 64 KiB",
            ));
        }
        if done {
            return serde_json::from_slice(&line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
}

/// `a` and `b` are the same file: both canonical paths, compared without regard to ASCII case.
fn same_file(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).map(|c| c.to_string_lossy().to_lowercase());
    matches!((canon(a), canon(b)), (Ok(a), Ok(b)) if a == b)
}

/// Who this process is, as ARCH-027 compares a pipe peer with it: its exe, its token user and
/// its token's integrity level; and its pid and creation time (a `FILETIME`), which a server it
/// connects to can't share or come after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub exe: PathBuf,
    pub sid: String,
    pub integrity: u32,
    pub pid: u32,
    pub started: u64,
}

impl Identity {
    /// This process's.
    ///
    /// # Errors
    /// When its exe or token can't be read.
    pub fn of_this_process() -> io::Result<Identity> {
        Ok(Identity {
            exe: std::env::current_exe()?,
            sid: current_user_sid()?,
            integrity: own_integrity_rid()?,
            pid: std::process::id(),
            started: own_start_time()?,
        })
    }
}

/// ARCH-027 for the app's pipe, either way: the peer is this exe, run as this user, at an
/// integrity level no lower than this process's, with a token that is neither restricted nor
/// an AppContainer's (review I-1, I-6, M-20). A lower-integrity or restricted copy of the same
/// exe (a sandboxed helper's child, say) is no peer to hand a launch or a deny token to, nor to
/// take one from.
pub fn peer_is_trusted(peer: &PeerInfo, me: &Identity) -> bool {
    peer.token.integrity >= me.integrity
        && !peer.token.restricted
        && !peer.token.app_container
        && peer.process.user_sid.eq_ignore_ascii_case(&me.sid)
        && same_file(&peer.process.image, &me.exe)
}

/// Why [`peer_is_trusted`] says no to `peer`, for the log. What the running instance asks of a
/// client.
fn identity_refusal(peer: &PeerInfo, me: &Identity) -> Option<String> {
    (!peer_is_trusted(peer, me)).then(|| {
        format!(
            "process {} (integrity {:#x}, restricted {}, app container {}) isn't this exe run as \
             this user, unrestricted, at this integrity level or above",
            peer.process.pid, peer.token.integrity, peer.token.restricted, peer.token.app_container
        )
    })
}

/// Why a launch won't take `peer`, which the pipe reports as its server: it must pass
/// [`peer_is_trusted`], and could have made the pipe this launch found. A process that is this
/// launch itself, or started after it, didn't: the pipe's creator has died and its pid has been
/// reused. An older process on that pid is what the proof is for.
fn server_refusal(peer: &PeerInfo, me: &Identity) -> Option<String> {
    if peer.process.pid == me.pid {
        return Some("the pipe reports this very process as its server".to_string());
    }
    if peer.started > me.started {
        return Some(format!(
            "process {}, which the pipe reports as its server, started after this launch",
            peer.process.pid
        ));
    }
    identity_refusal(peer, me)
}

/// Process `pid`, inspected now through one handle (review M-19), judged by `refusal`: why it
/// can't be trusted, which includes that it can't be inspected.
fn judged(
    pid: u32,
    me: &Identity,
    refusal: impl Fn(&PeerInfo, &Identity) -> Option<String>,
) -> Option<String> {
    match inspect_peer(pid) {
        Ok(peer) => refusal(&peer, me),
        Err(e) => Some(format!("process {pid} can't be inspected: {e}")),
    }
}

/// Hands `args` to the running instance of `app` (its confirm window with `confirm`), which may
/// then take the foreground: the instance's pid.
///
/// # Errors
/// [`ForwardError`]: no instance, a pipe held by another program, a closing instance, no
/// exchange within [`BUDGET`], or a broken pipe.
pub fn forward(env: &Env, app: &str, confirm: bool, args: &[String]) -> Result<u32, ForwardError> {
    forward_granting(env, app, confirm, args, true)
}

/// [`forward`], allowing the instance the foreground only with `grant_foreground` (review M-3).
fn forward_granting(
    env: &Env,
    app: &str,
    confirm: bool,
    args: &[String],
    grant_foreground: bool,
) -> Result<u32, ForwardError> {
    let me = Identity::of_this_process().map_err(|e| {
        tracing::warn!(error = %e, "this process can't tell who it is, so it trusts no pipe peer");
        ForwardError::SelfUnknown
    })?;
    forward_as(env, app, confirm, args, &me, grant_foreground)
}

/// `true` for an error that means the server went away mid-exchange (it was closing).
fn gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    )
}

fn forward_as(
    env: &Env,
    app: &str,
    confirm: bool,
    args: &[String],
    me: &Identity,
    grant_foreground: bool,
) -> Result<u32, ForwardError> {
    let name = env.app_pipe_name(app, confirm);
    let deadline = Instant::now() + BUDGET;
    let rt = runtime().map_err(ForwardError::Io)?;
    let after_trust = |e: io::Error| {
        if gone(&e) {
            ForwardError::Closing
        } else {
            ForwardError::Io(e)
        }
    };
    let result = rt.block_on(async {
        let exchange = async {
            let client = loop {
                match open_client(&name) {
                    Ok(client) => break client,
                    Err(ConnectError::NotFound) => return Err(ForwardError::NotRunning),
                    // Every instance busy for the whole budget: nothing serves the pipe (a hung
                    // instance, or a squatter that never answers).
                    Err(ConnectError::Busy) if Instant::now() + BUSY_RETRY >= deadline => {
                        tracing::warn!("every instance of the app's pipe stayed busy for 2 s");
                        return Err(ForwardError::Busy);
                    }
                    Err(ConnectError::Busy) => tokio::time::sleep(BUSY_RETRY).await,
                    // Held by a pipe whose DACL, or whose integrity label, leaves this launch
                    // out (review I-2).
                    Err(ConnectError::Io(e)) => {
                        tracing::warn!(error = %e, "the app's pipe can't be opened");
                        return Err(ForwardError::Untrusted);
                    }
                }
            };
            let pid = server_pid(&client).map_err(|_| ForwardError::Untrusted)?;
            // Before a byte is written (I-1, I-6).
            if let Some(why) = judged(pid, me, server_refusal) {
                tracing::warn!(
                    pid,
                    why,
                    "the app's pipe has a server this launch won't take"
                );
                return Err(ForwardError::Untrusted);
            }
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            let hello = read_line(&mut reader).await.map_err(after_trust)?;
            if hello["untrusted"] == true {
                // LILITH's own instance, checked above, refused this launch (review M-21).
                tracing::warn!(pid, "the running LILITH didn't accept this launch");
                return Err(ForwardError::NotAccepted);
            }
            if hello["app"] != app || hello["pid"].as_u64() != Some(u64::from(pid)) {
                return Err(ForwardError::Refused);
            }
            // The holder proves it, before it gets an argument or the foreground.
            let proof = ProofEvent::new().map_err(ForwardError::Io)?;
            write_line(&mut writer, &json!({"prove": proof.name()}))
                .await
                .map_err(after_trust)?;
            let answer = read_line(&mut reader).await.map_err(after_trust)?;
            if answer["proved"] != true || !proof.is_set() {
                tracing::warn!(
                    pid,
                    "the app's pipe is held by a process that can't prove it is its server"
                );
                return Err(ForwardError::Untrusted);
            }
            if grant_foreground && !lil_platform::desktop::allow_set_foreground(pid) {
                tracing::info!(
                    pid,
                    "AllowSetForegroundWindow was refused: the window may only flash"
                );
            }
            write_line(&mut writer, &json!({ "args": args }))
                .await
                .map_err(after_trust)?;
            let answer = read_line(&mut reader).await.map_err(after_trust)?;
            if answer == json!({"ok": true}) {
                Ok(pid)
            } else if answer["closing"] == true {
                Err(ForwardError::Closing)
            } else {
                Err(ForwardError::Refused)
            }
        };
        match tokio::time::timeout_at(deadline.into(), exchange).await {
            Ok(result) => result,
            Err(_) => Err(ForwardError::Timeout),
        }
    });
    drop(rt);
    result
}

/// `deny/<id>/<token>` with the token cut, as every log line writes it (P4a-R9).
pub fn cut_deny(id: &str) -> String {
    format!("deny/{id}/…")
}

/// How a toast's deny ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyOutcome {
    /// The confirmation is denied.
    Denied,
    /// The core refused it: its code (`confirm.unknown` for an unknown, used or dead token).
    Refused(String),
    /// No core runs: nothing is started, and the token died with the core that made it.
    NoCore,
    /// The core couldn't be reached, answered oddly, or not within [`DENY_BOUND`].
    Failed(String),
}

impl DenyOutcome {
    /// The exit code of a process that only sent this deny: 0 unless it failed.
    pub fn exit_code(&self) -> i32 {
        match self {
            DenyOutcome::Failed(_) => 1,
            _ => 0,
        }
    }
}

/// `work`, or [`DenyOutcome::Failed`] with `timeout` once `bound` has passed.
fn bounded(bound: Duration, work: impl std::future::Future<Output = DenyOutcome>) -> DenyOutcome {
    match runtime() {
        Err(e) => DenyOutcome::Failed(e.to_string()),
        Ok(rt) => rt.block_on(async {
            tokio::time::timeout(bound, work)
                .await
                .unwrap_or_else(|_| DenyOutcome::Failed(format!("no answer within {bound:?}")))
        }),
    }
}

/// Sends a toast's deny (P4a-R9): `app/confirmation.answer {confirmation_id, answer: "deny",
/// toast_token}` over a new connection made with `cfg` (a verified `ui:*` caller), never
/// starting a core, in at most [`DENY_BOUND`]. Shows nothing; writes one log line, with the
/// token cut.
pub fn send_deny(cfg: &ClientConfig, id: &str, token: &str) -> DenyOutcome {
    let mut cfg = cfg.clone();
    cfg.start = StartPolicy::NoStart;
    let outcome = bounded(DENY_BOUND, async {
        let client = match Client::connect(&cfg).await {
            Ok(client) => client,
            Err(ClientError::NotRunning) => return DenyOutcome::NoCore,
            Err(e) => return DenyOutcome::Failed(e.to_string()),
        };
        let params = json!({"confirmation_id": id, "answer": "deny", "toast_token": token});
        match client.app_call("app/confirmation.answer", params).await {
            Ok(Outcome::Ok(_)) => DenyOutcome::Denied,
            Ok(Outcome::Error { error }) => DenyOutcome::Refused(error.code),
            Ok(_) => DenyOutcome::Failed("an unexpected outcome".into()),
            Err(e) => DenyOutcome::Failed(e.to_string()),
        }
    });
    let link = cut_deny(id);
    match &outcome {
        DenyOutcome::Denied => tracing::info!(link, outcome = "denied", "a notification's deny"),
        DenyOutcome::Refused(code) => {
            tracing::info!(link, outcome = "refused", code, "a notification's deny");
        }
        DenyOutcome::NoCore => tracing::info!(
            link,
            outcome = "no_core",
            "a notification's deny: no core runs, so its confirmation is gone"
        ),
        DenyOutcome::Failed(why) => {
            tracing::warn!(link, outcome = "failed", why, "a notification's deny");
        }
    }
    outcome
}

/// What [`start`] settled.
#[derive(Debug)]
pub enum Start {
    /// Exit with this code before Qt starts: 0 once the arguments reached the running instance
    /// or the deny was sent, 1 when it failed (logged).
    Exit(i32),
    /// Start Qt, serving this instance's pipe (none under `--smoke-exit`, or when another
    /// program holds the pipe: then [`BootPlan::pipe_notice`] says why).
    Run(Option<InstanceServer>),
}

/// The first instance among `pipes` (`confirm` flags, in order) that takes `args`.
fn forward_first(
    env: &Env,
    app: &str,
    pipes: &[bool],
    args: &[String],
    grant_foreground: bool,
) -> Option<u32> {
    for &confirm in pipes {
        match forward_granting(env, app, confirm, args, grant_foreground) {
            Ok(pid) => return Some(pid),
            Err(ForwardError::NotRunning) => {}
            Err(e) => {
                tracing::warn!(confirm, error = %e, "a running instance didn't take the launch");
            }
        }
    }
    None
}

/// What a start does with `plan` before Qt starts (module docs). `deny_cfg` makes the
/// connection a deny is sent on, when this process has to send it.
pub fn start(
    plan: &mut BootPlan,
    env: &Env,
    deny_cfg: impl FnOnce() -> Option<ClientConfig>,
) -> Start {
    if plan.refused.is_some() {
        return Start::Exit(2);
    }
    if plan.smoke_exit {
        return Start::Run(None);
    }
    let app = plan.app.clone();
    if let Some(Opened::Deny { id, token }) = &plan.opened {
        // A deny never shows a window (P4a-R9, P4a-R11): the running app, else a confirm window,
        // else this process. None of them is allowed the foreground.
        if forward_first(env, &app, &[false, true], &plan.args, false).is_some() {
            plan.forwarded = true;
            return Start::Exit(0);
        }
        let Some(cfg) = deny_cfg() else {
            tracing::warn!(
                link = cut_deny(id),
                "a notification's deny had no connection to go on"
            );
            return Start::Exit(1);
        };
        return Start::Exit(send_deny(&cfg, id, token).exit_code());
    }
    let grant = !plan.no_activate;
    let confirm = matches!(plan.mode, Mode::Confirm { .. });
    if confirm && forward_first(env, &app, &[false], &plan.args, grant).is_some() {
        // While the full app runs, confirm links go to it first (P4a-R11).
        plan.forwarded = true;
        return Start::Exit(0);
    }
    instance(plan, env, confirm, grant)
}

/// Claims this mode's pipe, or hands the launch to its owner: retried while the owner closes or
/// goes away between the two, for at most [`CLOSING_WAIT`].
fn instance(plan: &mut BootPlan, env: &Env, confirm: bool, grant: bool) -> Start {
    let app = plan.app.clone();
    let until = Instant::now() + CLOSING_WAIT;
    loop {
        match InstanceServer::claim(env, &app, confirm) {
            Ok(Some(server)) => return Start::Run(Some(server)),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "the app's pipe couldn't be created: starting as an instance of its own"
                );
                plan.pipe_notice = Some(NOTICE_HELD);
                return Start::Run(None);
            }
        }
        match forward_granting(env, &app, confirm, &plan.args, grant) {
            Ok(_) => {
                plan.forwarded = true;
                return Start::Exit(0);
            }
            Err(ForwardError::NotRunning | ForwardError::Closing) if Instant::now() < until => {
                std::thread::sleep(CLOSING_RETRY);
            }
            Err(ForwardError::NotRunning | ForwardError::Closing) => {
                tracing::warn!(
                    "the running instance was still closing after 15 s: starting as an \
                     instance of its own, without the pipe"
                );
                plan.pipe_notice = Some(NOTICE_CLOSING);
                return Start::Run(None);
            }
            Err(ForwardError::Untrusted) => {
                tracing::warn!(
                    "another program holds the app's pipe: starting as an instance of its own, \
                     without single-instance protection"
                );
                plan.pipe_notice = Some(NOTICE_HELD);
                return Start::Run(None);
            }
            Err(ForwardError::NotAccepted) => {
                plan.pipe_notice = Some(NOTICE_NOT_ACCEPTED);
                return Start::Run(None);
            }
            Err(ForwardError::SelfUnknown) => {
                plan.pipe_notice = Some(NOTICE_SELF_UNKNOWN);
                return Start::Run(None);
            }
            Err(ForwardError::Busy) => {
                tracing::warn!(
                    "the running LILITH didn't answer in time: starting as an instance of its \
                     own, without the pipe"
                );
                plan.pipe_notice = Some(NOTICE_BUSY);
                return Start::Run(None);
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "the running LILITH didn't take this launch, so it ends here"
                );
                return Start::Exit(1);
            }
        }
    }
}

/// Starts again, each as a launch of its own, the forwarded launches `open` the window never
/// took before the instance's end (review I-3, M-15): `spawn(exe, args)` is
/// `lil_platform::process::spawn_no_activate_verbatim` in the app, whose command line
/// (`"<exe>" <arg> …`, nothing escaped) reads back through `plan_boot` to the same arguments.
/// How many started.
pub fn restart_untaken(
    exe: &Path,
    open: impl IntoIterator<Item = Vec<String>>,
    spawn: impl Fn(&Path, &[String]) -> io::Result<u32>,
) -> usize {
    let mut started = 0;
    for args in open {
        match spawn(exe, &args) {
            Ok(_) => {
                started += 1;
                tracing::info!("a launch the window never took starts again on its own");
            }
            Err(e) => tracing::error!(error = %e, "a launch the window never took was lost"),
        }
    }
    started
}

/// What the running instance does with one forwarded launch ([`route`]).
#[derive(Debug)]
pub enum Routed {
    /// The window shows it.
    Show(Box<BootPlan>),
    /// A deny, being sent on this thread without the window knowing; join it before exiting.
    Deny(JoinHandle<()>),
    /// Nothing: the arguments were refused (logged).
    Nothing,
}

/// What the running instance does with a forwarded launch: the plan for its window, or a
/// deny sent on a thread of its own with `deny_cfg`'s connection.
pub fn route(
    forwarded: &Forwarded,
    env: &Env,
    deny_cfg: impl FnOnce() -> Option<ClientConfig> + Send + 'static,
) -> Routed {
    let plan = crate::args::plan_args(&forwarded.args, env);
    if plan.refused.is_some() {
        tracing::warn!("a launch's command line was refused by the running instance");
        return Routed::Nothing;
    }
    let Some(Opened::Deny { id, token }) = plan.opened.clone() else {
        return Routed::Show(Box::new(plan));
    };
    let spawned =
        std::thread::Builder::new()
            .name("lil-deny".into())
            .spawn(move || match deny_cfg() {
                Some(cfg) => {
                    send_deny(&cfg, &id, &token);
                }
                None => tracing::warn!(
                    link = cut_deny(&id),
                    "a notification's deny had no connection to go on"
                ),
            });
    match spawned {
        Ok(handle) => Routed::Deny(handle),
        Err(e) => {
            tracing::error!(error = %e, "a notification's deny couldn't start");
            Routed::Nothing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::process::{ProcessInfo, TokenTrust};
    use std::path::PathBuf;

    fn info(image: PathBuf, sid: &str) -> ProcessInfo {
        ProcessInfo {
            pid: 1,
            image,
            user_sid: sid.to_string(),
            parent_pid: None,
        }
    }

    #[test] // spec: ARCH-027
    fn a_peer_must_be_this_exe_run_as_this_user_unrestricted_at_no_lower_integrity() {
        const LOW: u32 = 0x1000;
        const MEDIUM: u32 = 0x2000;
        const HIGH: u32 = 0x3000;
        let own = std::env::current_exe().unwrap();
        let sid = "S-1-5-21-1-2-3-1001";
        let me = Identity {
            exe: own.clone(),
            sid: sid.to_string(),
            integrity: MEDIUM,
            pid: 2,
            started: 100,
        };
        let peer = |image: PathBuf, sid: &str, integrity: u32| PeerInfo {
            process: info(image, sid),
            token: TokenTrust {
                integrity,
                restricted: false,
                app_container: false,
            },
            started: 1,
        };
        assert!(peer_is_trusted(&peer(own.clone(), sid, MEDIUM), &me));
        assert!(peer_is_trusted(
            &peer(own.clone(), &sid.to_lowercase(), MEDIUM),
            &me
        ));
        // A peer above this process (elevated) is no risk to it; one below is (review I-6).
        assert!(peer_is_trusted(&peer(own.clone(), sid, HIGH), &me));
        assert!(!peer_is_trusted(&peer(own.clone(), sid, LOW), &me));
        assert!(!peer_is_trusted(&peer(own.clone(), sid, 0), &me));
        // A restricted or AppContainer token, at Medium all the same (review M-20).
        let mut restricted = peer(own.clone(), sid, MEDIUM);
        restricted.token.restricted = true;
        assert!(!peer_is_trusted(&restricted, &me));
        let mut contained = peer(own.clone(), sid, MEDIUM);
        contained.token.app_container = true;
        assert!(!peer_is_trusted(&contained, &me));
        // An elevated instance takes nothing from a Medium launch (review M-16).
        let elevated = Identity {
            integrity: HIGH,
            ..me.clone()
        };
        assert!(!peer_is_trusted(&peer(own.clone(), sid, MEDIUM), &elevated));
        // Another user running the same exe.
        for other_sid in ["S-1-5-21-1-2-3-1002", "S-1-5-18"] {
            assert!(!peer_is_trusted(&peer(own.clone(), other_sid, MEDIUM), &me));
        }
        // This user running another exe, or one that doesn't exist.
        let other = own.with_file_name("not-this-exe.exe");
        assert!(!peer_is_trusted(&peer(other, sid, MEDIUM), &me));
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("copy.exe");
        std::fs::copy(&own, &copy).unwrap();
        assert!(!peer_is_trusted(&peer(copy, sid, MEDIUM), &me));
        // This test process itself, read through one handle.
        let this = inspect_peer(std::process::id()).unwrap();
        let me_now = Identity::of_this_process().unwrap();
        assert!(peer_is_trusted(&this, &me_now));
    }

    fn this_identity() -> Identity {
        Identity::of_this_process().unwrap()
    }

    #[test] // spec: ARCH-027
    fn a_launch_takes_no_server_that_is_itself_or_started_after_it() {
        let me = this_identity();
        let peer = |pid: u32, started: u64| {
            let mut peer = inspect_peer(std::process::id()).unwrap();
            peer.process.pid = pid;
            peer.started = started;
            peer
        };
        assert_eq!(server_refusal(&peer(me.pid + 4, me.started - 1), &me), None);
        assert_eq!(
            server_refusal(&peer(me.pid + 4, me.started), &me),
            None,
            "the same tick isn't after"
        );
        let refused = |peer: &PeerInfo| server_refusal(peer, &me).unwrap();
        assert!(refused(&peer(me.pid, me.started)).contains("this very process"));
        assert!(refused(&peer(me.pid + 4, me.started + 1)).contains("started after"));
    }

    #[test] // spec: ARCH-027, APP-013
    fn a_launch_hands_over_only_after_the_proof_and_only_to_an_older_server() {
        // A real exchange in this one process, as if this launch were another process: its pid
        // told apart, and its start time before or after the server's.
        let env = lil_testkit::TestEnv::new();
        let server = InstanceServer::claim(&env.env, "image", false)
            .unwrap()
            .expect("the pipe is free");
        let args = vec!["--no-activate".to_string()];
        let launch = |started: u64| Identity {
            pid: std::process::id() + 4,
            started,
            ..this_identity()
        };
        assert!(matches!(
            forward_as(&env.env, "image", false, &args, &launch(0), false),
            Err(ForwardError::Untrusted)
        ));
        assert_eq!(server.next(), None, "nothing was handed over");
        let pid = forward_as(&env.env, "image", false, &args, &launch(u64::MAX), false).unwrap();
        assert_eq!(pid, std::process::id());
        assert_eq!(
            server.next_within(Duration::from_secs(5)),
            Some(Forwarded { args })
        );
    }

    #[test] // spec: APP-014
    fn a_forward_gives_up_after_2_s() {
        let env = lil_testkit::TestEnv::new();
        // A server that passes every check and never says hello.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let name = env.env.app_pipe_name("image", false);
        let server = {
            let _in = rt.enter();
            create_instance(&name, true).unwrap()
        };
        rt.spawn(async move {
            let _ = server.connect().await;
            tokio::time::sleep(Duration::from_secs(10)).await;
            drop(server);
        });
        let launch = Identity {
            pid: std::process::id() + 4,
            started: u64::MAX,
            ..this_identity()
        };
        let started = Instant::now();
        let result = forward_as(&env.env, "image", false, &[], &launch, false);
        let took = started.elapsed();
        assert!(matches!(result, Err(ForwardError::Timeout)), "{result:?}");
        assert!(
            took >= Duration::from_millis(1900) && took < Duration::from_secs(3),
            "{took:?}"
        );
    }

    #[test] // spec: APP-014
    fn an_untaken_launch_starts_again_with_its_own_arguments() {
        let exe = Path::new(r"C:\Program Files\LILITH\lil-image.exe");
        let open = vec![
            vec![
                "--intent".to_string(),
                "edit".to_string(),
                r"C:\My Art\".to_string(),
            ],
            vec![
                "--no-activate".to_string(),
                "lil-dev:task/j_0123456789abcdef".to_string(),
            ],
            vec![],
        ];
        let lines = Mutex::new(Vec::new());
        let started = restart_untaken(exe, open.clone(), |exe, args| {
            lines
                .lock()
                .unwrap()
                .push(lil_platform::process::verbatim_command_line(exe, args)?);
            Ok(1)
        });
        assert_eq!(started, 3);
        let env = lil_platform::Env::with_roots(
            lil_platform::Channel::Dev,
            lil_platform::Roots {
                roaming: PathBuf::from(r"C:\R"),
                local: PathBuf::from(r"C:\L"),
                user_profile: PathBuf::from(r"C:\P"),
            },
            "S-1-5-21-1-2-3-1001".to_string(),
        );
        // Each restart's command line reads back to the same arguments: the trailing backslash
        // of a quoted folder stays one (review M-15).
        let lines = lines.into_inner().unwrap();
        for (line, args) in lines.iter().zip(&open) {
            let plan = crate::args::plan_boot(line, &env);
            assert_eq!(plan.refused, None, "{line}");
            assert_eq!(&plan.args, args, "{line}");
        }
        assert_eq!(
            crate::args::plan_boot(&lines[0], &env).opened,
            Some(crate::args::Opened::Target(r"C:\My Art\".into()))
        );
        // A spawn that fails starts nothing and is counted out.
        assert_eq!(
            restart_untaken(exe, open, |_, _| Err(io::Error::other("no"))),
            0
        );
    }

    #[test] // spec: P4a-R9
    fn a_deny_that_never_answers_fails_within_its_bound() {
        let started = Instant::now();
        let outcome = bounded(
            Duration::from_millis(200),
            std::future::pending::<DenyOutcome>(),
        );
        assert!(matches!(outcome, DenyOutcome::Failed(_)), "{outcome:?}");
        assert_eq!(outcome.exit_code(), 1);
        assert!(started.elapsed() < Duration::from_secs(2));
        for ok in [
            DenyOutcome::Denied,
            DenyOutcome::NoCore,
            DenyOutcome::Refused("confirm.unknown".into()),
        ] {
            assert_eq!(ok.exit_code(), 0);
        }
    }
}
