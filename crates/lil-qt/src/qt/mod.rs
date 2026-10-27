//! The cxx-qt bridge (feature `qt`): the `Lilith.Core` QML module, and the boot call
//! `apps/lil-image/main.cpp` makes before Qt starts.

use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// `Core`, the QML singleton of module `Lilith.Core`: this build's id. Task 16 makes it
        /// the app's connection to the core.
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, build_id, cxx_name = "buildId", READ, CONSTANT)]
        type Core = super::CoreRust;
    }

    /// [`crate::BootPlan`] as `main.cpp` reads it, with what the plan needs from the process.
    #[namespace = "lil_qt"]
    struct QtBoot {
        /// Empty to start; else why the start is refused (exit 2, Qt never starts).
        refused: String,
        no_activate: bool,
        smoke_exit: bool,
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
    extern "Rust" {
        /// Plans this start from the raw command line and the process environment.
        fn boot() -> QtBoot;

        /// The process environment's names that `qt_env_to_clear` would still clear: empty once
        /// `main.cpp` has cleared them (logged at start, checked by the deploy check).
        fn env_left() -> Vec<String>;
    }
}

fn env_left() -> Vec<String> {
    let names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    crate::qt_env_to_clear(names.iter().map(String::as_str))
}

/// `Core`'s Rust side.
pub struct CoreRust {
    build_id: QString,
}

impl Default for CoreRust {
    fn default() -> Self {
        Self {
            build_id: QString::from(lil_protocol::session::BUILD_ID),
        }
    }
}

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
    let env = match (lil_platform::Env::from_process(), &app_dir) {
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
        Ok(env) => crate::boot(&lil_platform::desktop::raw_command_line(), &env),
        Err(refused) => {
            return ffi::QtBoot {
                refused,
                no_activate: false,
                smoke_exit: false,
                org: String::new(),
                app: crate::APP.to_string(),
                qt_args: vec![crate::QT_ARGV0.to_string()],
                app_dir,
                clear_env,
            };
        }
    };
    ffi::QtBoot {
        refused: plan.refused.unwrap_or_default(),
        no_activate: plan.no_activate,
        smoke_exit: plan.smoke_exit,
        org: plan.org,
        app: plan.app,
        qt_args: plan.qt_args,
        app_dir,
        clear_env,
    }
}
