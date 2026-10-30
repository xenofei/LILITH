//! One instance per app (APP-013, APP-014; Appendix G "Apps"; P4a-R9, P4a-R11), Qt-free.
//!
//! Each app owns the per-user pipe `Env::app_pipe_name(app, false)`, and its confirm window
//! `Env::app_pipe_name(app, true)`, each created as a first instance with the core pipe's
//! user-only security descriptor. The exchange, each message one JSON line of at most 64 KiB:
//! 1. the running instance ([`InstanceServer`]) writes `{"app": "<app>", "pid": <pid>}` when a
//!    client connects;
//! 2. the second launch ([`forward`]) checks the server as ARCH-027 says, its image path is its
//!    own executable and its token user is the current user (else [`ForwardError::Untrusted`],
//!    and nothing is written), calls `AllowSetForegroundWindow(pid)` unless the launch never
//!    comes forward (a deny, `--no-activate`), and writes `{"args": [<argument>, …]}`;
//! 3. the running instance reads it, answers `{"ok": true}` and queues the arguments
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
//! instance of its own, without the pipe, and says so ([`BootPlan::pipe_held`]).
//!
//! **Threads.** [`forward`] and [`send_deny`] each run on a current-thread tokio runtime of their
//! own, made and shut down within the call, so nothing of theirs is left when they return. A
//! claimed [`InstanceServer`] makes its pipe and serves it on one thread of its own
//! (`lil-instance`), with that thread's own current-thread runtime: nothing tokio runs on the
//! caller's thread. Dropping the server stops and joins the thread.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lil_client::{Client, ClientConfig, ClientError, StartPolicy};
use lil_platform::Env;
use lil_platform::pipe::{ConnectError, CreateError, create_instance, open_client, server_pid};
use lil_platform::process::{ProcessInfo, ProcessInspector, SystemInspector, current_user_sid};
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
/// How long a launch waits for a closing instance to go before it starts without the pipe.
pub const CLOSING_WAIT: Duration = Duration::from_secs(10);
/// The longest a toast's deny may take, connection included.
pub const DENY_BOUND: Duration = Duration::from_secs(10);
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
    /// leaves this user out, or every instance stays busy for the whole budget), or its server
    /// isn't this executable run as this user, or can't be inspected. Nothing was written.
    Untrusted,
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
                 this exe run as this user)",
            ),
            ForwardError::Closing => f.write_str("the running instance is closing"),
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
    thread: Option<JoinHandle<()>>,
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
                            rt.block_on(serve(first, name, app, tx, closing, stop));
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
                thread: Some(thread),
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
}

impl Drop for InstanceServer {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        self.stop.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve(
    first: NamedPipeServer,
    name: String,
    app: String,
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
        match tokio::time::timeout(BUDGET, exchange(client, &app, &closing)).await {
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
/// once answered "closing".
async fn exchange(
    mut client: NamedPipeServer,
    app: &str,
    closing: &AtomicBool,
) -> io::Result<Option<Vec<String>>> {
    let hello = json!({"app": app, "pid": std::process::id()});
    write_line(&mut client, &hello).await?;
    let (reader, mut writer) = tokio::io::split(client);
    let mut reader = BufReader::new(reader);
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

/// ARCH-027 for the app's pipe: its server is `own_exe`, run as `own_sid`.
pub fn server_is_trusted(info: &ProcessInfo, own_exe: &Path, own_sid: &str) -> bool {
    info.user_sid.eq_ignore_ascii_case(own_sid) && same_file(&info.image, own_exe)
}

/// What a client of the pipe is: its exe and user, how it inspects the server, and whether the
/// server may take the foreground.
struct Forwarder<'a> {
    own_exe: &'a Path,
    own_sid: &'a str,
    inspector: &'a dyn ProcessInspector,
    grant_foreground: bool,
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
    let own_exe = std::env::current_exe().map_err(ForwardError::Io)?;
    let own_sid = current_user_sid().map_err(ForwardError::Io)?;
    let client = Forwarder {
        own_exe: &own_exe,
        own_sid: &own_sid,
        inspector: &SystemInspector,
        grant_foreground,
    };
    forward_as(env, app, confirm, args, &client)
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
    me: &Forwarder<'_>,
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
                    // Every instance busy for the whole budget: nothing serves the pipe (a
                    // squatter that never answers, or a hung instance), so it counts as held.
                    Err(ConnectError::Busy) if Instant::now() + BUSY_RETRY >= deadline => {
                        tracing::warn!("every instance of the app's pipe stayed busy for 2 s");
                        return Err(ForwardError::Untrusted);
                    }
                    Err(ConnectError::Busy) => tokio::time::sleep(BUSY_RETRY).await,
                    // Held by a pipe whose DACL leaves this user out (review I-2).
                    Err(ConnectError::Io(e)) => {
                        tracing::warn!(error = %e, "the app's pipe can't be opened");
                        return Err(ForwardError::Untrusted);
                    }
                }
            };
            let pid = server_pid(&client).map_err(|_| ForwardError::Untrusted)?;
            let info = me.inspector.inspect(pid).map_err(|e| {
                tracing::warn!(pid, error = %e, "the app's pipe server can't be inspected");
                ForwardError::Untrusted
            })?;
            if !server_is_trusted(&info, me.own_exe, me.own_sid) {
                return Err(ForwardError::Untrusted);
            }
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            let hello = read_line(&mut reader).await.map_err(after_trust)?;
            if hello["app"] != app || hello["pid"].as_u64() != Some(u64::from(pid)) {
                return Err(ForwardError::Refused);
            }
            if me.grant_foreground && !lil_platform::desktop::allow_set_foreground(pid) {
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
    /// program holds the pipe: then [`BootPlan::pipe_held`] is set).
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
                plan.pipe_held = true;
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
                    "the running instance was still closing after 10 s: starting as an \
                     instance of its own, without the pipe"
                );
                return Start::Run(None);
            }
            Err(ForwardError::Untrusted) => {
                tracing::warn!(
                    "another program holds the app's pipe: starting as an instance of its own, \
                     without single-instance protection"
                );
                plan.pipe_held = true;
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
    fn the_server_must_be_this_exe_run_as_this_user() {
        let own = std::env::current_exe().unwrap();
        let sid = "S-1-5-21-1-2-3-1001";
        assert!(server_is_trusted(&info(own.clone(), sid), &own, sid));
        assert!(server_is_trusted(
            &info(own.clone(), &sid.to_lowercase()),
            &own,
            sid
        ));
        // Another user running the same exe.
        assert!(!server_is_trusted(
            &info(own.clone(), "S-1-5-21-1-2-3-1002"),
            &own,
            sid
        ));
        assert!(!server_is_trusted(
            &info(own.clone(), "S-1-5-18"),
            &own,
            sid
        ));
        // This user running another exe, or one that doesn't exist.
        let other = own.with_file_name("not-this-exe.exe");
        assert!(!server_is_trusted(&info(other, sid), &own, sid));
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("copy.exe");
        std::fs::copy(&own, &copy).unwrap();
        assert!(!server_is_trusted(&info(copy, sid), &own, sid));
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
