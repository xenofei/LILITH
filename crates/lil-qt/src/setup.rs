//! The app's notification setup (APP-022; P4a-R26; Appendix G "Activation and registration"):
//! what its "Set up" button does, after your click only, Qt-free. [`set_up`] blocks (COM, the
//! registry, a pipe): it is never called on the GUI thread (QA-026); `qt::setup_bridge` runs it on
//! a thread of its own.
//!
//! It writes only this channel's own Start-menu shortcut and `lil:` scheme, each only when it
//! isn't right already: never `.lilref`, which `lil doctor` keeps offering, and never a scheme
//! another exe holds (plan 4a ruling, 2026-10-25). A write that fails doesn't stop the other, and
//! the report says which is which. Outside a test channel nothing is written while the channel's
//! `lil-image.exe` isn't there ([`app_missing`], `lil doctor`'s rule). Then `desktop.get {fresh:
//! true}` asks the core, over a connection of its own that never starts one, what it now reads:
//! the core's own answer is what the app shows. That connection is a `cli` caller's
//! ([`read_config`]): the core takes a second `ui:*` session of one process, in another workspace,
//! for a workspace switch, and cancels the cap prompts of the session it thinks was left
//! (Appendix G "Connections").

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lil_client::{Client, ClientConfig, StartPolicy};
use lil_platform::desktop::{HandlerPart, ShortcutState};
use lil_platform::{Channel, Env};
use lil_protocol::app::NotificationPath;
use lil_protocol::{CallerKind, NO_WORKSPACE, Outcome, ToolOutput};
use serde::Serialize;
use serde_json::{Map, Value};

/// The app the shortcut and the scheme open.
pub const APP_EXE: &str = "lil-image.exe";
/// The longest the core's answer may take, connection included.
pub const REREAD_BOUND: Duration = Duration::from_secs(5);

/// The channel's `lil-image.exe`, exactly the path the core checks the shortcut and the scheme
/// against (`lil_core::server`): in the install folder on stable, else beside `lil-core.exe`.
/// `None` when the core's path has no folder.
pub fn app_exe(cfg: &ClientConfig) -> Option<PathBuf> {
    cfg.env
        .install_dir()
        .or_else(|| cfg.core_exe.parent().map(Path::to_path_buf))
        .map(|dir| dir.join(APP_EXE))
}

/// Whether the setup is withheld because the app isn't at `exe`: never on a test channel, whose
/// registry root and Start-menu folder the shell never reads. It reads the disk.
pub fn app_missing(env: &Env, exe: &Path) -> bool {
    !matches!(env.channel(), Channel::Test(_)) && !exe.is_file()
}

/// What the setup reads and writes on this PC.
pub trait Registrar {
    fn shortcut_state(&self, env: &Env, exe: &Path) -> ShortcutState;
    /// The channel's own scheme, alone: `.lilref` isn't this setup's.
    fn scheme_part(&self, env: &Env, exe: &Path) -> HandlerPart;
    fn write_shortcut(&self, env: &Env, exe: &Path) -> io::Result<()>;
    fn register_scheme(&self, env: &Env, exe: &Path) -> io::Result<()>;
}

/// The real desktop, through `lil_platform`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealRegistrar;

impl Registrar for RealRegistrar {
    fn shortcut_state(&self, env: &Env, exe: &Path) -> ShortcutState {
        lil_platform::desktop::shortcut_state(env, exe)
    }

    fn scheme_part(&self, env: &Env, exe: &Path) -> HandlerPart {
        lil_platform::desktop::handler_parts(env, exe).scheme
    }

    fn write_shortcut(&self, env: &Env, exe: &Path) -> io::Result<()> {
        lil_platform::desktop::write_shortcut(env, exe)
    }

    fn register_scheme(&self, env: &Env, exe: &Path) -> io::Result<()> {
        lil_platform::desktop::register_scheme(env, exe)
    }
}

/// What became of the shortcut or the scheme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Step {
    /// It was right already, and wasn't touched.
    Ok,
    Written,
    /// Another exe holds the scheme, and it was left alone.
    Held,
    /// The write failed, and `why` says how.
    Failed {
        why: String,
    },
    /// Nothing was tried: the app isn't there.
    NotTried,
}

/// What [`set_up`] did and what the core then read, as the app's `finished` signal carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupReport {
    /// The path `lil-image.exe` should be at, when it isn't there: then nothing was written.
    pub app_missing: Option<String>,
    pub shortcut: Step,
    pub scheme: Step,
    /// `desktop.get {fresh: true}`'s `notification_path`; `None` when it couldn't be read.
    pub notification_path: Option<NotificationPath>,
    /// Why it couldn't be read.
    pub reread_error: Option<String>,
}

/// [`set_up_with`] on the real desktop and the core of `cfg`'s channel, as far as `cfg` names the
/// app's path. Blocks; never call it on the GUI thread.
pub fn set_up(cfg: &ClientConfig) -> SetupReport {
    let Some(exe) = app_exe(cfg) else {
        return SetupReport {
            app_missing: Some(cfg.core_exe.join(APP_EXE).display().to_string()),
            shortcut: Step::NotTried,
            scheme: Step::NotTried,
            notification_path: None,
            reread_error: None,
        };
    };
    set_up_with(&cfg.env, &exe, &RealRegistrar, |tool, args| {
        ask_core(cfg, tool, args)
    })
}

/// The setup itself (module docs): `call` makes the one tool call, after every write.
pub fn set_up_with(
    env: &Env,
    exe: &Path,
    registrar: &dyn Registrar,
    call: impl FnOnce(&str, Map<String, Value>) -> Result<Outcome<ToolOutput>, String>,
) -> SetupReport {
    if app_missing(env, exe) {
        return SetupReport {
            app_missing: Some(exe.display().to_string()),
            shortcut: Step::NotTried,
            scheme: Step::NotTried,
            notification_path: None,
            reread_error: None,
        };
    }
    let written = |what: &str, outcome: io::Result<()>| match outcome {
        Ok(()) => {
            tracing::info!(what, "notification setup: written");
            Step::Written
        }
        Err(e) => {
            // The log names no path: the error is kept to its kind.
            tracing::warn!(what, kind = ?e.kind(), "notification setup: the write failed");
            Step::Failed { why: e.to_string() }
        }
    };
    let shortcut = match registrar.shortcut_state(env, exe) {
        ShortcutState::Ok => Step::Ok,
        _ => written("the shortcut", registrar.write_shortcut(env, exe)),
    };
    let scheme = match registrar.scheme_part(env, exe) {
        HandlerPart::Ok => Step::Ok,
        HandlerPart::Missing | HandlerPart::Stale(_) => {
            written("the scheme", registrar.register_scheme(env, exe))
        }
        HandlerPart::OtherBuild(_) | HandlerPart::Foreign(_) => Step::Held,
    };
    let mut args = Map::new();
    args.insert("fresh".to_string(), Value::Bool(true));
    let (notification_path, reread_error) = match call("desktop.get", args).and_then(read_path) {
        Ok(path) => (Some(path), None),
        Err(why) => (None, Some(why)),
    };
    SetupReport {
        app_missing: None,
        shortcut,
        scheme,
        notification_path,
        reread_error,
    }
}

/// `desktop.get`'s answer, as far as this setup reads it.
fn read_path(outcome: Outcome<ToolOutput>) -> Result<NotificationPath, String> {
    match outcome {
        Outcome::Ok(output) => {
            serde_json::from_value(output.structured["notification_path"].clone())
                .map_err(|e| format!("the core's answer doesn't read: {e}"))
        }
        Outcome::Error { error } => Err(error.message),
        Outcome::PendingConfirmation(_) => Err("the core asked for a confirmation".to_string()),
    }
}

/// The connection [`ask_core`] makes from the app's `cfg`: a `cli` caller's, so that the core never
/// takes it for the app's session (module docs), with no workspace, never starting a core.
pub fn read_config(cfg: &ClientConfig) -> ClientConfig {
    let mut cfg = cfg.clone();
    cfg.caller_kind = CallerKind::Cli;
    cfg.start = StartPolicy::NoStart;
    cfg.workspace = NO_WORKSPACE.to_string();
    cfg.workspace_origin = None;
    cfg
}

/// One tool call over a connection of its own ([`read_config`]), on a runtime of its own, in at most
/// [`REREAD_BOUND`].
pub fn ask_core(
    cfg: &ClientConfig,
    tool: &str,
    args: Map<String, Value>,
) -> Result<Outcome<ToolOutput>, String> {
    let cfg = read_config(cfg);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let asked = async {
            let client = Client::connect(&cfg).await.map_err(|e| e.to_string())?;
            client
                .call_tool(tool, args, None)
                .await
                .map_err(|e| e.to_string())
        };
        tokio::time::timeout(REREAD_BOUND, asked)
            .await
            .unwrap_or_else(|_| Err(format!("no answer within {} s", REREAD_BOUND.as_secs())))
    })
}
