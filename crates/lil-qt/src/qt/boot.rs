//! The start before Qt (Checkpoint A I-3; APP-013, APP-014, APP-020; P4a-R9, P4a-R11,
//! P4a-R21): what `apps/lil-image/main.cpp` calls before its `QGuiApplication` exists, the way a
//! launch reaches the window afterwards, and the instance's end.
//!
//! 1. `boot()` plans the start from the raw command line (`crate::plan_boot`) and the process
//!    (its folder, the variables Qt reads); a refused plan exits 2 in `main.cpp`. `boot_from`
//!    is the same with a command line of the caller's (the QML test runner's second launch).
//! 2. `claim_instance()` runs `single_instance::start` on that plan, once the environment is
//!    clean and the log has started: a forwarded launch, or a deny with no window, returns the
//!    exit code, still before Qt starts. Otherwise this process is the instance and it returns
//!    -1. Nothing tokio runs on the caller's thread: the pipe is served on the instance's own
//!    thread (`single_instance`), and the forwarded launches are routed on one more,
//!    `lil-launches`, so a `.lilref` read never holds up the pipe (review M-8).
//! 3. Each launch the window must show (this start's own first, then each forwarded one; a
//!    forwarded deny is sent on a thread of its own, and the window never hears of it) waits
//!    here until `main.cpp` has named its window (`lil_qt::deliverLaunchesTo`,
//!    `src/qt/launch_sink.h`) and called `launches_ready()`; from then on each goes to the
//!    window as it comes, and the window says when it has taken one (`launch_handled`).
//! 4. `release_instance()`, once the event loop has ended and before the window goes: the pipe
//!    answers "closing" from then on; the router ends; the pipe ends and its thread is joined,
//!    so a new launch becomes the instance itself; what the pipe took is routed; a forwarded
//!    launch the window never took is started again as a launch of its own; a deny in flight
//!    is joined (review I-3, M-12).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use lil_client::ClientConfig;
use lil_platform::Env;

use crate::args::{BootPlan, Mode, launch_json};
use crate::single_instance::{
    Forwarded, InstanceServer, Routed, Start, restart_untaken, route, start,
};

#[cxx_qt::bridge]
pub mod ffi {
    /// [`crate::BootPlan`] as `main.cpp` reads it, with what the plan needs from the process.
    #[namespace = "lil_qt"]
    struct QtBoot {
        /// Empty to start; else why the start is refused (exit 2, Qt never starts).
        refused: String,
        no_activate: bool,
        smoke_exit: bool,
        /// The confirm window's start (`--confirm`, a `confirm/` link) rather than the app's.
        confirm_mode: bool,
        org: String,
        app: String,
        /// The `argv` Qt is built from: the program name alone.
        qt_args: Vec<String>,
        /// The exe's folder: Qt's only library path.
        app_dir: String,
        /// The environment variables to remove before any Qt call.
        clear_env: Vec<String>,
    }

    #[namespace = "lil_qt"]
    unsafe extern "C++" {
        include!("lil_qt/src/qt/launch_sink.h");

        /// Hands a launch's JSON (its number `seq`) to the window; false when there is none.
        #[cxx_name = "launchArrived"]
        fn launch_arrived(seq: u64, json: &str) -> bool;
    }

    #[namespace = "lil_qt"]
    extern "Rust" {
        /// Plans this start from the raw command line and the process environment.
        fn boot() -> QtBoot;

        /// [`boot`] with `raw_cmdline` (exe first) instead of the process's own.
        fn boot_from(raw_cmdline: &str) -> QtBoot;

        /// The single instance (module docs, step 2): the exit code of a start that ends here,
        /// before Qt starts, or -1 to go on.
        fn claim_instance() -> i32;

        /// The window is named: the launches waiting go to it, then each as it comes.
        fn launches_ready();

        /// The window has taken launch `seq`.
        fn launch_handled(seq: u64);

        /// The instance's end (module docs, step 4).
        fn release_instance();
    }
}

/// This start's plan, from `boot()` to `claim_instance()`.
static PLAN: Mutex<Option<BootPlan>> = Mutex::new(None);
/// The instance's pipe, until `release_instance`; the router holds the other reference.
static INSTANCE: Mutex<Option<Arc<InstanceServer>>> = Mutex::new(None);
/// The thread that routes the forwarded launches, and the flag that ends it.
static ROUTER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static ROUTER_STOP: AtomicBool = AtomicBool::new(false);
/// Denies in flight, joined at the end.
static DENIES: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());
/// The launches for the window.
static LAUNCHES: Mutex<Launches> = Mutex::new(Launches::new());

/// How long the router waits for a launch before it looks at its stop flag.
const ROUTER_TICK: Duration = Duration::from_millis(100);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The launches for the window: those waiting for it, and those handed to it and not yet taken,
/// with the arguments a forwarded one came with.
struct Launches {
    ready: bool,
    next: u64,
    waiting: Vec<(u64, String)>,
    /// Forwarded launches handed out or waiting, not yet taken: their arguments.
    open: BTreeMap<u64, Vec<String>>,
}

impl Launches {
    const fn new() -> Launches {
        Launches {
            ready: false,
            next: 1,
            waiting: Vec::new(),
            open: BTreeMap::new(),
        }
    }
}

fn boot() -> ffi::QtBoot {
    boot_from(&lil_platform::desktop::raw_command_line())
}

fn boot_from(raw_cmdline: &str) -> ffi::QtBoot {
    let (names, odd): (Vec<_>, Vec<_>) = std::env::vars_os()
        .map(|(name, _)| name.into_string())
        .partition(Result::is_ok);
    let names: Vec<String> = names.into_iter().filter_map(Result::ok).collect();
    let clear_env = crate::qt_env_to_clear(names.iter().map(String::as_str));
    // A name that isn't UTF-8 can't be handed to main.cpp to clear: one that would be cleared
    // refuses the start instead.
    let odd: Vec<String> = odd
        .into_iter()
        .filter_map(Result::err)
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    let unclearable = !crate::qt_env_to_clear(odd.iter().map(String::as_str)).is_empty();
    let app_dir = crate::app_dir(std::env::current_exe().ok().as_deref());
    let env = match (Env::from_process(), &app_dir) {
        (Err(e), _) => Err(format!("LILITH didn't start: {e}")),
        (Ok(_), _) if unclearable => Err(
            "LILITH didn't start: an environment variable Qt reads couldn't be cleared."
                .to_string(),
        ),
        (Ok(_), None) => {
            Err("LILITH didn't start: it couldn't tell which folder it runs from.".to_string())
        }
        (Ok(env), Some(_)) => Ok(env),
    };
    let app_dir = app_dir.unwrap_or_default();
    let plan = match env {
        Ok(env) => crate::plan_boot(raw_cmdline, &env),
        Err(refused) => {
            return ffi::QtBoot {
                refused,
                no_activate: false,
                smoke_exit: false,
                confirm_mode: false,
                org: String::new(),
                app: crate::APP.to_string(),
                qt_args: vec![crate::QT_ARGV0.to_string()],
                app_dir,
                clear_env,
            };
        }
    };
    *lock(&PLAN) = Some(plan.clone());
    ffi::QtBoot {
        refused: plan.refused.unwrap_or_default(),
        no_activate: plan.no_activate,
        smoke_exit: plan.smoke_exit,
        confirm_mode: matches!(plan.mode, Mode::Confirm { .. }),
        org: plan.org,
        app: plan.app,
        qt_args: plan.qt_args,
        app_dir,
        clear_env,
    }
}

/// The connection a toast's deny goes on: this app's verified `ui:image`, never starting a core.
fn deny_cfg(env: &Env) -> Option<ClientConfig> {
    let caller = format!("ui:{}", crate::APP).parse().ok()?;
    match ClientConfig::for_env(env.clone(), caller) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::warn!(error = %e, "no connection for a notification's deny");
            None
        }
    }
}

fn claim_instance() -> i32 {
    let Some(mut plan) = lock(&PLAN).take() else {
        tracing::error!("claim_instance before boot");
        return 1;
    };
    let env = match Env::from_process() {
        Ok(env) => env,
        Err(e) => {
            tracing::error!(error = %e, "no LILITH folders for the single instance");
            return 1;
        }
    };
    let server = match start(&mut plan, &env, || deny_cfg(&env)) {
        Start::Exit(code) => {
            tracing::info!(
                code,
                forwarded = plan.forwarded,
                "the launch ended before Qt"
            );
            return code;
        }
        Start::Run(server) => server,
    };
    // This start's own launch: the window was shown by main.cpp, so it isn't activated again.
    if let Some(json) = launch_json(&plan, false) {
        deliver(json, None);
    }
    if let Some(server) = server {
        let server = Arc::new(server);
        *lock(&INSTANCE) = Some(server.clone());
        ROUTER_STOP.store(false, Ordering::SeqCst);
        match std::thread::Builder::new()
            .name("lil-launches".into())
            .spawn(move || route_launches(&server, &env))
        {
            Ok(router) => *lock(&ROUTER) = Some(router),
            Err(e) => tracing::error!(error = %e, "the launches' router couldn't start"),
        }
    }
    -1
}

/// One forwarded launch: shown by the window, or a deny sent without it.
fn take(forwarded: &Forwarded, env: &Env) {
    let deny_env = env.clone();
    match route(forwarded, env, move || deny_cfg(&deny_env)) {
        Routed::Show(plan) => {
            if let Some(json) = launch_json(&plan, true) {
                deliver(json, Some(forwarded.args.clone()));
            }
        }
        Routed::Deny(handle) => lock(&DENIES).push(handle),
        Routed::Nothing => {}
    }
}

/// The router's thread: each forwarded launch in turn, until `release_instance`.
fn route_launches(server: &InstanceServer, env: &Env) {
    while !ROUTER_STOP.load(Ordering::SeqCst) {
        if let Some(forwarded) = server.next_within(ROUTER_TICK) {
            take(&forwarded, env);
        }
    }
}

/// The sink only queues the launch to the GUI thread, so it is called under the lock, which
/// keeps the launches in order.
fn deliver(json: String, args: Option<Vec<String>>) {
    let mut launches = lock(&LAUNCHES);
    let seq = launches.next;
    launches.next += 1;
    if let Some(args) = args {
        launches.open.insert(seq, args);
    }
    if !launches.ready {
        launches.waiting.push((seq, json));
    } else if !ffi::launch_arrived(seq, &json) {
        tracing::warn!("a launch had no window to go to");
    }
}

fn launches_ready() {
    let mut launches = lock(&LAUNCHES);
    launches.ready = true;
    for (seq, json) in std::mem::take(&mut launches.waiting) {
        if !ffi::launch_arrived(seq, &json) {
            tracing::warn!("a launch had no window to go to");
        }
    }
}

fn launch_handled(seq: u64) {
    lock(&LAUNCHES).open.remove(&seq);
}

fn release_instance() {
    let Some(server) = lock(&INSTANCE).take() else {
        return;
    };
    // No launch is taken from here on: each one waits for this process to go.
    server.close();
    ROUTER_STOP.store(true, Ordering::SeqCst);
    // The router ends within a tick, and drops its reference.
    if let Some(router) = lock(&ROUTER).take() {
        let _ = router.join();
    }
    // The pipe ends and its thread is joined first, so every launch it took is in hand before
    // the last ones are routed (review M-12). A launch waiting on the pipe now becomes the
    // instance itself.
    server.shutdown();
    if let Ok(env) = Env::from_process() {
        while let Some(forwarded) = server.next() {
            take(&forwarded, &env);
        }
    }
    // Forwarded launches the window never took start again, as launches of their own, in the
    // app's own grammar (review M-15).
    let open = std::mem::take(&mut lock(&LAUNCHES).open);
    if !open.is_empty() {
        match std::env::current_exe() {
            Ok(exe) => {
                restart_untaken(
                    &exe,
                    open.into_values(),
                    lil_platform::process::spawn_no_activate_verbatim,
                );
            }
            Err(e) => tracing::error!(error = %e, "launches the window never took were lost"),
        }
    }
    for deny in std::mem::take(&mut *lock(&DENIES)) {
        let _ = deny.join();
    }
    drop(server);
}
