//! The start before Qt (Checkpoint A I-3; APP-013, APP-014, APP-020; P4a-R9, P4a-R11,
//! P4a-R21): what `apps/lil-image/main.cpp` calls before its `QGuiApplication` exists, and the
//! way a launch reaches the window afterwards.
//!
//! 1. `boot()` plans the start from the raw command line (`crate::plan_boot`) and the process
//!    (its folder, the variables Qt reads); a refused plan exits 2 in `main.cpp`.
//! 2. `claim_instance()` runs `single_instance::start` on that plan, once the environment is
//!    clean and the log has started: a forwarded launch, or a deny with no window, returns the
//!    exit code (0), still before Qt starts. Otherwise this process is the instance: its pipe is
//!    served on the instance's own thread for as long as the process runs, and it returns -1.
//! 3. Each launch the window must show (this start's own first, then each forwarded one,
//!    routed by `single_instance::route`: a forwarded deny is sent on a thread of its own, and
//!    the window never hears of it) waits here until `main.cpp` has named its sink
//!    (`lil_qt::setLaunchSink`, `src/qt/launch_sink.h`) and called `launches_ready()`; from
//!    then on each goes to the sink as it comes.

use std::sync::{Mutex, OnceLock, PoisonError};

use lil_client::ClientConfig;
use lil_platform::Env;

use crate::args::{BootPlan, Mode, launch_json};
use crate::single_instance::{InstanceServer, Start, route, start};

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

        /// Hands a launch's JSON to `main.cpp`'s sink; false when there is none yet.
        #[cxx_name = "launchArrived"]
        fn launch_arrived(json: &str) -> bool;
    }

    #[namespace = "lil_qt"]
    extern "Rust" {
        /// Plans this start from the raw command line and the process environment.
        fn boot() -> QtBoot;

        /// The single instance (module docs, step 2): the exit code of a start that ends here,
        /// before Qt starts, or -1 to go on.
        fn claim_instance() -> i32;

        /// The window is loaded and the sink named: the launches waiting go to it, then each
        /// as it comes.
        fn launches_ready();
    }
}

/// This start's plan, from `boot()` to `claim_instance()`.
static PLAN: Mutex<Option<BootPlan>> = Mutex::new(None);
/// The instance's pipe, served for the process's life.
static INSTANCE: OnceLock<InstanceServer> = OnceLock::new();
/// The launches waiting for the sink, and whether it is there.
static LAUNCHES: Mutex<(bool, Vec<String>)> = Mutex::new((false, Vec::new()));

fn boot() -> ffi::QtBoot {
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
        Ok(env) => crate::plan_boot(&lil_platform::desktop::raw_command_line(), &env),
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
    *PLAN.lock().unwrap_or_else(PoisonError::into_inner) = Some(plan.clone());
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
    let Some(mut plan) = PLAN.lock().unwrap_or_else(PoisonError::into_inner).take() else {
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
        deliver(json);
    }
    if let Some(server) = server
        && INSTANCE.set(server).is_ok()
        && let Some(server) = INSTANCE.get()
    {
        let hook_env = env.clone();
        server.on_arrival(move || drain(&hook_env));
        // Anything that came before the hook was set.
        drain(&env);
    }
    -1
}

/// Routes every forwarded launch waiting on the pipe.
fn drain(env: &Env) {
    let Some(server) = INSTANCE.get() else {
        return;
    };
    while let Some(forwarded) = server.next() {
        let deny_env = env.clone();
        if let Some(plan) = route(&forwarded, env, move || deny_cfg(&deny_env))
            && let Some(json) = launch_json(&plan, true)
        {
            deliver(json);
        }
    }
}

/// The sink only queues the launch to the GUI thread, so it is called under the lock, which
/// keeps the launches in order.
fn deliver(json: String) {
    let mut launches = LAUNCHES.lock().unwrap_or_else(PoisonError::into_inner);
    if !launches.0 {
        launches.1.push(json);
    } else if !ffi::launch_arrived(&json) {
        tracing::warn!("a launch had no window to go to");
    }
}

fn launches_ready() {
    let mut launches = LAUNCHES.lock().unwrap_or_else(PoisonError::into_inner);
    launches.0 = true;
    for json in std::mem::take(&mut launches.1) {
        if !ffi::launch_arrived(&json) {
            tracing::warn!("a launch had no window to go to");
        }
    }
}
