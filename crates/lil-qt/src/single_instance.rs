//! One instance per app (APP-013, APP-014; Appendix G "Apps"; P4a-R9, P4a-R11), Qt-free.
//!
//! Each app owns the per-user pipe `Env::app_pipe_name(app, false)`, and its confirm window
//! `Env::app_pipe_name(app, true)`, each created as a first instance with the core pipe's
//! user-only security descriptor. The exchange, each message one JSON line of at most 64 KiB:
//! 1. the running instance ([`InstanceServer`]) writes `{"app": "<app>", "pid": <pid>}` when a
//!    client connects;
//! 2. the second launch ([`forward`]) checks that the server's image path is its own executable
//!    (else [`ForwardError::Untrusted`], and nothing is written), calls
//!    `AllowSetForegroundWindow(pid)` and writes `{"args": [<argument>, …]}`;
//! 3. the running instance reads it, answers `{"ok": true}` and queues the arguments
//!    ([`InstanceServer::next`]); the second launch then exits 0.
//!
//! The second launch gives up 2 s after it started ([`ForwardError::Timeout`]). The server takes
//! one client at a time, in the order they connect, each within 2 s, so two quick launches are
//! both read, in turn, by the one running instance.
//!
//! [`start`] is what a start does before Qt: which pipe a plan goes to, and the deny of a toast
//! (P4a-R9), which never shows a window: it goes to the running app, else to a running confirm
//! window, else this process sends it itself with [`send_deny`] (`StartPolicy::NoStart`, so a
//! stale toast's Deny starts no core) and exits 0.
//!
//! **Threads.** [`forward`] and [`send_deny`] each run on a current-thread tokio runtime of their
//! own, made and shut down within the call, so nothing is left running when they return. A
//! claimed [`InstanceServer`] serves on one thread of its own (`lil-instance`) with its own
//! current-thread runtime, for as long as it lives; dropping it stops and joins that thread.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lil_client::{Client, ClientConfig, ClientError, StartPolicy};
use lil_platform::Env;
use lil_platform::pipe::{ConnectError, CreateError, create_instance, open_client, server_pid};
use lil_platform::process::{ProcessInspector, SystemInspector};
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
/// The wait between tries while every instance of the pipe is busy.
const BUSY_RETRY: Duration = Duration::from_millis(10);

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
    /// The pipe's server isn't this executable: nothing was written.
    Untrusted,
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
            ForwardError::Untrusted => {
                f.write_str("the pipe's server is another program than this one")
            }
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

/// What the server thread hands to [`InstanceServer`].
struct Shared {
    hook: Mutex<Option<Box<dyn Fn() + Send>>>,
}

/// The running instance's side of the pipe.
pub struct InstanceServer {
    rx: Mutex<mpsc::Receiver<Forwarded>>,
    shared: Arc<Shared>,
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
    /// serves it: `None` when another process owns it.
    ///
    /// # Errors
    /// When the pipe can't be created for another reason, or the thread can't start.
    pub fn claim(env: &Env, app: &str, confirm: bool) -> io::Result<Option<InstanceServer>> {
        let name = env.app_pipe_name(app, confirm);
        let rt = runtime()?;
        let first = {
            let _in = rt.enter();
            match create_instance(&name, true) {
                Ok(server) => server,
                Err(CreateError::AlreadyOwned) => return Ok(None),
                Err(CreateError::Io(e)) => return Err(e),
            }
        };
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            hook: Mutex::new(None),
        });
        let stop = Arc::new(Notify::new());
        let app = app.to_string();
        let thread = {
            let (shared, stop) = (shared.clone(), stop.clone());
            std::thread::Builder::new()
                .name("lil-instance".into())
                .spawn(move || {
                    rt.block_on(serve(first, name, app, tx, shared, stop));
                    // The runtime is dropped here, on its own thread.
                })?
        };
        Ok(Some(InstanceServer {
            rx: Mutex::new(rx),
            shared,
            stop,
            thread: Some(thread),
        }))
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

    /// Calls `hook` (from the server's thread) each time arguments arrive, after they are
    /// queued for [`InstanceServer::next`].
    pub fn on_arrival(&self, hook: impl Fn() + Send + 'static) {
        *self
            .shared
            .hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(hook));
    }
}

impl Drop for InstanceServer {
    fn drop(&mut self) {
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
    shared: Arc<Shared>,
    stop: Arc<Notify>,
) {
    let mut server = first;
    loop {
        tokio::select! {
            connected = server.connect() => {
                if let Err(e) = connected {
                    tracing::warn!(error = %e, "a launch's connection to the app's pipe failed");
                }
            }
            () = stop.notified() => return,
        }
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
        match tokio::time::timeout(BUDGET, exchange(client, &app)).await {
            Ok(Ok(args)) => {
                if tx.send(Forwarded { args }).is_err() {
                    return;
                }
                if let Some(hook) = shared
                    .hook
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                {
                    hook();
                }
            }
            Ok(Err(e)) => tracing::warn!(error = %e, "a launch's arguments were refused"),
            Err(_) => tracing::warn!("a launch didn't hand its arguments over within 2 s"),
        }
    }
}

/// The server's side of one exchange: the arguments, once answered `{"ok": true}`.
async fn exchange(mut client: NamedPipeServer, app: &str) -> io::Result<Vec<String>> {
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
    write_line(&mut writer, &json!({"ok": true})).await?;
    Ok(args)
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

/// Hands `args` to the running instance of `app` (its confirm window with `confirm`): the
/// instance's pid.
///
/// # Errors
/// [`ForwardError`]: no instance, a server that isn't this executable, no exchange within
/// [`BUDGET`], or a broken pipe.
pub fn forward(env: &Env, app: &str, confirm: bool, args: &[String]) -> Result<u32, ForwardError> {
    let own = std::env::current_exe().map_err(ForwardError::Io)?;
    forward_as(env, app, confirm, args, &own)
}

/// [`forward`], with `own` as this executable.
fn forward_as(
    env: &Env,
    app: &str,
    confirm: bool,
    args: &[String],
    own: &Path,
) -> Result<u32, ForwardError> {
    let name = env.app_pipe_name(app, confirm);
    let deadline = Instant::now() + BUDGET;
    let rt = runtime().map_err(ForwardError::Io)?;
    let result = rt.block_on(async {
        let exchange = async {
            let client = loop {
                match open_client(&name) {
                    Ok(client) => break client,
                    Err(ConnectError::NotFound) => return Err(ForwardError::NotRunning),
                    Err(ConnectError::Busy) => tokio::time::sleep(BUSY_RETRY).await,
                    Err(ConnectError::Io(e)) => return Err(ForwardError::Io(e)),
                }
            };
            let pid = server_pid(&client).map_err(ForwardError::Io)?;
            let image: PathBuf = SystemInspector
                .inspect(pid)
                .map_err(ForwardError::Io)?
                .image;
            if !same_file(&image, own) {
                return Err(ForwardError::Untrusted);
            }
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            let hello = read_line(&mut reader).await.map_err(ForwardError::Io)?;
            if hello["app"] != app || hello["pid"].as_u64() != Some(u64::from(pid)) {
                return Err(ForwardError::Refused);
            }
            if !lil_platform::desktop::allow_set_foreground(pid) {
                tracing::info!(
                    pid,
                    "AllowSetForegroundWindow was refused: the window may only flash"
                );
            }
            write_line(&mut writer, &json!({ "args": args }))
                .await
                .map_err(ForwardError::Io)?;
            let ok = read_line(&mut reader).await.map_err(ForwardError::Io)?;
            if ok != json!({"ok": true}) {
                return Err(ForwardError::Refused);
            }
            Ok(pid)
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
    /// The core couldn't be reached or answered oddly.
    Failed(String),
}

/// Sends a toast's deny (P4a-R9): `app/confirmation.answer {confirmation_id, answer: "deny",
/// toast_token}` over a new connection made with `cfg` (a verified `ui:*` caller), never
/// starting a core. Shows nothing; writes one log line, with the token cut.
pub fn send_deny(cfg: &ClientConfig, id: &str, token: &str) -> DenyOutcome {
    let mut cfg = cfg.clone();
    cfg.start = StartPolicy::NoStart;
    let outcome = match runtime() {
        Err(e) => DenyOutcome::Failed(e.to_string()),
        Ok(rt) => rt.block_on(async {
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
        }),
    };
    let link = cut_deny(id);
    match &outcome {
        DenyOutcome::Denied => tracing::info!(link, outcome = "denied", "a notification's deny"),
        DenyOutcome::Refused(code) => {
            tracing::info!(link, outcome = "refused", code, "a notification's deny")
        }
        DenyOutcome::NoCore => tracing::info!(
            link,
            outcome = "no_core",
            "a notification's deny: no core runs, so its confirmation is gone"
        ),
        DenyOutcome::Failed(why) => {
            tracing::warn!(link, outcome = "failed", why, "a notification's deny")
        }
    }
    outcome
}

/// What [`start`] settled.
#[derive(Debug)]
pub enum Start {
    /// Exit with this code before Qt starts: 0 once the arguments reached the running instance
    /// or the deny was sent.
    Exit(i32),
    /// Start Qt, serving this instance's pipe (none under `--smoke-exit`, or when another
    /// program squats the pipe).
    Run(Option<InstanceServer>),
}

/// The first instance among `pipes` (`confirm` flags, in order) that takes `args`.
fn forward_first(env: &Env, app: &str, pipes: &[bool], args: &[String]) -> Option<u32> {
    for &confirm in pipes {
        match forward(env, app, confirm, args) {
            Ok(pid) => return Some(pid),
            Err(ForwardError::NotRunning) => {}
            Err(e) => {
                tracing::warn!(confirm, error = %e, "a running instance didn't take the launch")
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
        // else this process.
        if forward_first(env, &app, &[false, true], &plan.args).is_some() {
            plan.forwarded = true;
            return Start::Exit(0);
        }
        match deny_cfg() {
            Some(cfg) => {
                send_deny(&cfg, id, token);
            }
            None => tracing::warn!(
                link = cut_deny(id),
                "a notification's deny had no connection to go on"
            ),
        }
        return Start::Exit(0);
    }
    let confirm = matches!(plan.mode, Mode::Confirm { .. });
    if confirm && forward_first(env, &app, &[false], &plan.args).is_some() {
        // While the full app runs, confirm links go to it first (P4a-R11).
        plan.forwarded = true;
        return Start::Exit(0);
    }
    // Claim this mode's pipe, or hand the launch to its owner; once more if the owner went away
    // between the two.
    for _ in 0..2 {
        match InstanceServer::claim(env, &app, confirm) {
            Ok(Some(server)) => return Start::Run(Some(server)),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "the app's pipe couldn't be created");
                return Start::Exit(1);
            }
        }
        match forward(env, &app, confirm, &plan.args) {
            Ok(_) => {
                plan.forwarded = true;
                return Start::Exit(0);
            }
            Err(ForwardError::NotRunning) => continue,
            Err(ForwardError::Untrusted) => {
                // Another program holds the pipe: this start goes on without one.
                tracing::warn!("another program holds the app's pipe: starting without it");
                return Start::Run(None);
            }
            Err(e) => {
                tracing::warn!(error = %e, "the running instance didn't take the launch");
                return Start::Exit(1);
            }
        }
    }
    Start::Exit(1)
}

/// What the running instance does with a forwarded launch: the plan for its window, or `None`
/// when there is nothing to show (a deny, sent on a thread of its own with `deny_cfg`'s
/// connection, without the window knowing).
pub fn route(
    forwarded: &Forwarded,
    env: &Env,
    deny_cfg: impl FnOnce() -> Option<ClientConfig> + Send + 'static,
) -> Option<BootPlan> {
    let plan = crate::args::plan_args(&forwarded.args, env);
    if plan.refused.is_some() {
        tracing::warn!("a launch's command line was refused by the running instance");
        return None;
    }
    if let Some(Opened::Deny { id, token }) = plan.opened.clone() {
        let spawned = std::thread::Builder::new()
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
        if let Err(e) = spawned {
            tracing::error!(error = %e, "a notification's deny couldn't start");
        }
        return None;
    }
    Some(plan)
}
