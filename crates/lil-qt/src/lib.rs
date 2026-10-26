//! lil_qt: the Rust side of the LILITH apps (P4a-R1).
//!
//! Everything outside [`qt`] is Qt-free, so it builds and tests with default features and no Qt
//! installed. [`boot`] settles a start before Qt does: `apps/lil-image/main.cpp` calls it first,
//! and Qt sees only what the plan hands it (Checkpoint A I-3):
//! - **argv:** [`BootPlan::qt_args`] is the program name alone, whatever the command line says, so
//!   Qt never parses an option such as `-platformpluginpath` from a crafted link. LILITH's own
//!   arguments come only from the raw command line (P4a-R3).
//! - **The command line:** its rest must be well formed, or the start is refused before Qt starts.
//!   Until Task 19 adds the one positional argument (a `lil:` URL or a `.lilref` path), that
//!   means nothing, or the switches `--smoke-exit` and `--no-activate`, each at most once.
//! - **The environment:** [`qt_env_to_clear`] names the variables that load Qt or QML code from
//!   another folder, or open WebEngine to debugging; `main.cpp` removes them before any Qt call.
//!
//! With the `qt` feature, [`qt`] is the cxx-qt bridge: the `Lilith.Core` QML module and the
//! boot call `main.cpp` makes.

#[cfg(feature = "qt")]
extern crate cxx_qt_lib;

#[cfg(feature = "qt")]
pub mod qt;

use lil_platform::Env;
use lil_platform::desktop::split_first_argument;

/// Qt's `applicationName`: the app's folder under the channel's local data folder.
pub const APP: &str = "image";

/// The program name Qt gets as its only argument.
pub const QT_ARGV0: &str = "lil-image";

/// `--smoke-exit`: quit with 0 once the window's first frame is shown and the event loop has gone
/// idle (`tools/deploy/check-deploy.ps1`).
pub const SMOKE_EXIT: &str = "--smoke-exit";

/// `--no-activate`: show the window without taking the focus; its taskbar button flashes.
pub const NO_ACTIVATE: &str = "--no-activate";

/// The variables removed by name, compared without regard to ASCII case (Windows' rule).
const CLEAR_EXACT: [&str; 5] = [
    "QT_PLUGIN_PATH",
    "QML_IMPORT_PATH",
    "QML2_IMPORT_PATH",
    "QTWEBENGINE_CHROMIUM_FLAGS",
    "QTWEBENGINE_REMOTE_DEBUGGING",
];

/// The variables removed by prefix, compared the same way.
const CLEAR_PREFIXES: [&str; 2] = ["QT_QPA_", "QT_QML_"];

/// The window a start opens. Task 19 adds the confirmation window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Shell,
}

/// What `main.cpp` does, settled before Qt starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPlan {
    pub mode: Mode,
    pub no_activate: bool,
    pub smoke_exit: bool,
    /// Why the start is refused: Qt never starts and the process exits 2. Never the argument
    /// itself, which may carry a token.
    pub refused: Option<String>,
    /// Qt's `organizationName`: the channel's data-folder name (`LILITH`, `LILITH-dev`).
    pub org: String,
    /// Qt's `applicationName` ([`APP`]).
    pub app: String,
    /// The `argv` Qt is built from: [`QT_ARGV0`] alone.
    pub qt_args: Vec<String>,
}

/// Plans a start from the raw command line (`lil_platform::desktop::raw_command_line`).
pub fn boot(raw_cmdline: &str, env: &Env) -> BootPlan {
    let org = env
        .data_dir()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut plan = BootPlan {
        mode: Mode::Shell,
        no_activate: false,
        smoke_exit: false,
        refused: None,
        org,
        app: APP.to_string(),
        qt_args: vec![QT_ARGV0.to_string()],
    };
    let (_exe, rest) = split_first_argument(raw_cmdline);
    for piece in rest.split([' ', '\t']).filter(|p| !p.is_empty()) {
        let seen = match piece {
            SMOKE_EXIT => std::mem::replace(&mut plan.smoke_exit, true),
            NO_ACTIVATE => std::mem::replace(&mut plan.no_activate, true),
            _ => {
                plan.refused = Some(
                    "LILITH didn't start: its command line has an argument it doesn't take."
                        .to_string(),
                );
                break;
            }
        };
        if seen {
            plan.refused =
                Some("LILITH didn't start: its command line repeats a switch.".to_string());
            break;
        }
    }
    if plan.refused.is_some() {
        plan.no_activate = false;
        plan.smoke_exit = false;
    }
    plan
}

/// The environment variables to remove before any Qt or WebEngine call (Checkpoint A I-3):
/// `QT_PLUGIN_PATH`, every `QT_QPA_*`, `QML_IMPORT_PATH`, `QML2_IMPORT_PATH`, every `QT_QML_*`,
/// `QTWEBENGINE_CHROMIUM_FLAGS` and `QTWEBENGINE_REMOTE_DEBUGGING`, from `names` (the process's
/// variable names), keeping each name as it was spelled.
pub fn qt_env_to_clear<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| {
            let upper = name.to_ascii_uppercase();
            CLEAR_EXACT.contains(&upper.as_str())
                || CLEAR_PREFIXES.iter().any(|p| upper.starts_with(p))
        })
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::{Channel, Roots};
    use std::path::PathBuf;

    fn env(channel: Channel) -> Env {
        let roots = Roots {
            roaming: PathBuf::from(r"C:\R"),
            local: PathBuf::from(r"C:\L"),
            user_profile: PathBuf::from(r"C:\P"),
        };
        Env::with_roots(channel, roots, "S-1-5-21-1-2-3-1001".to_string())
    }

    const EXE: &str = r#""C:\b\lil-image.exe""#;

    #[test] // spec: APP-001
    fn boot_returns_shell_mode_by_default() {
        let plan = boot(EXE, &env(Channel::Dev));
        assert_eq!(
            plan,
            BootPlan {
                mode: Mode::Shell,
                no_activate: false,
                smoke_exit: false,
                refused: None,
                org: "LILITH-dev".to_string(),
                app: "image".to_string(),
                qt_args: vec!["lil-image".to_string()],
            }
        );
        assert_eq!(boot(EXE, &env(Channel::Stable)).org, "LILITH");
    }

    #[test] // spec: APP-001
    fn boot_reads_the_smoke_switches() {
        let plan = boot(
            &format!("{EXE} --smoke-exit\t --no-activate "),
            &env(Channel::Dev),
        );
        assert!(plan.smoke_exit && plan.no_activate);
        assert_eq!(plan.refused, None);
        let plan = boot(r"lil-image.exe --no-activate", &env(Channel::Dev));
        assert!(plan.no_activate && !plan.smoke_exit);
    }

    #[test] // spec: APP-020
    fn boot_refuses_anything_else_before_qt_starts() {
        for rest in [
            r#""lil:x" -platformpluginpath "\\host\share""#,
            "-platform offscreen",
            "-qmljsdebugger=port:1234",
            "--no-activate --no-activate",
            "--smoke-exit=1",
            r#""--no-activate""#,
            "--SMOKE-EXIT",
            "lil:task/1",
        ] {
            let plan = boot(&format!("{EXE} {rest}"), &env(Channel::Dev));
            assert!(plan.refused.is_some(), "{rest}");
            assert!(!plan.refused.as_deref().unwrap().contains(rest), "{rest}");
            assert!(!plan.no_activate && !plan.smoke_exit, "{rest}");
        }
    }

    #[test] // spec: APP-020
    fn qt_gets_only_the_program_name() {
        for cmdline in [
            EXE.to_string(),
            format!("{EXE} --smoke-exit --no-activate"),
            format!(r#"{EXE} "lil:x" -platformpluginpath "\\host\share""#),
            r#""\\host\share\lil-image.exe" -platform minimal"#.to_string(),
        ] {
            assert_eq!(
                boot(&cmdline, &env(Channel::Dev)).qt_args,
                ["lil-image"],
                "{cmdline}"
            );
        }
    }

    #[test] // spec: APP-020
    fn qt_env_to_clear_names_the_load_and_debug_variables() {
        let names = [
            "PATH",
            "QT_PLUGIN_PATH",
            "qt_plugin_path",
            "QT_QPA_PLATFORM",
            "QT_QPA_PLATFORM_PLUGIN_PATH",
            "QML_IMPORT_PATH",
            "QML2_IMPORT_PATH",
            "QT_QML_DEBUG",
            "Qt_Qml_Something",
            "QTWEBENGINE_CHROMIUM_FLAGS",
            "QTWEBENGINE_REMOTE_DEBUGGING",
            "QT_QUICK_BACKEND",
            "QTDIR",
            "QT_PLUGIN_PATHS",
            "XQT_QPA_PLATFORM",
            "LIL_CHANNEL",
        ];
        assert_eq!(
            qt_env_to_clear(names),
            [
                "QT_PLUGIN_PATH",
                "qt_plugin_path",
                "QT_QPA_PLATFORM",
                "QT_QPA_PLATFORM_PLUGIN_PATH",
                "QML_IMPORT_PATH",
                "QML2_IMPORT_PATH",
                "QT_QML_DEBUG",
                "Qt_Qml_Something",
                "QTWEBENGINE_CHROMIUM_FLAGS",
                "QTWEBENGINE_REMOTE_DEBUGGING",
            ]
        );
    }
}
