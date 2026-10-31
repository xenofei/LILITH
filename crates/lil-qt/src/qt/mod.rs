//! The cxx-qt bridge (feature `qt`): the `Lilith.Core` QML module ([`core_object`], on the
//! background [`runtime`], with the list models of [`json_list_model`], the live-preview feed
//! of [`preview_feed`], and [`layout`], the shell's layout file and shortcut map), and the calls
//! `apps/lil-image/main.cpp` makes: the boot plan and the single instance before Qt starts
//! ([`boot`]), the app's log (ARCH-117, ARCH-119), the Qt log lines, the start times and the
//! core session.

use std::sync::{Mutex, OnceLock, PoisonError};

use lil_client::ClientConfig;
use lil_protocol::CallerKind;

pub mod boot;
pub mod bundle_export;
pub mod core_object;
pub mod csv_export;
pub mod hardware;
pub mod job_text;
pub mod json_list_model;
pub mod layout;
pub mod live_jobs_model;
pub mod money_text;
pub mod preview_feed;
pub mod runtime;

#[cxx_qt::bridge]
pub mod ffi {
    #[namespace = "lil_qt"]
    extern "Rust" {
        /// The process environment's names that `qt_env_to_clear` would still clear: empty once
        /// `main.cpp` has cleared them (logged at start, checked by the deploy check).
        fn env_left() -> Vec<String>;

        /// Starts the app's log, `<logs dir>\lil-image.<date>.log`, with the panic hook
        /// (ARCH-117, ARCH-119). Empty, or why it didn't start (the app goes on without it).
        fn init_logs() -> String;

        /// One line of the Qt log bridge (`lilshim::logEvent`, a Qt message among them) into
        /// the app's log: a Qt message with its `qt_category`.
        fn log_line(line: &str);

        /// `main.cpp` measured `first_frame` or `interactive` (APP-136): `Core.startTimes`.
        fn note_start_time(what: &str, ms: f64);

        /// The app's `Core` connects to the core of this process's channel as `ui:image`,
        /// starting one if none runs (ARCH-018), in `suite.toml`'s default workspace (ARCH-030).
        /// Call it before the QML engine makes `Core`. Empty, or why it can't (then `Core` is
        /// `unavailable`).
        fn serve_core() -> String;
    }
}

/// How this process's `Core` reaches the core: none until [`serve`] (the QML tests have none).
static SETUP: Mutex<Option<ClientConfig>> = Mutex::new(None);

/// The `Core` objects made from now on connect with `cfg` (each its own session). lil-image
/// calls it through `serve_core`; the e2e tests call it with the test channel's `ui:test`.
pub fn serve(cfg: ClientConfig) {
    *SETUP.lock().unwrap_or_else(PoisonError::into_inner) = Some(cfg);
}

fn setup() -> Option<ClientConfig> {
    SETUP.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn serve_core() -> String {
    let env = match lil_platform::Env::from_process() {
        Ok(env) => env,
        Err(e) => return format!("Can't tell which LILITH folders to use: {e}"),
    };
    let caller: CallerKind = match format!("ui:{}", crate::APP).parse() {
        Ok(caller) => caller,
        Err(e) => return format!("ui:{} isn't a caller kind: {e}", crate::APP),
    };
    let mut cfg = match ClientConfig::for_env(env, caller) {
        Ok(cfg) => cfg,
        Err(e) => return format!("Can't find this program's folder: {e}"),
    };
    // An app opens suite.toml's default workspace (ARCH-030: the session's choice, which
    // starts there). The local data folder holds no workspace, so only that step applies.
    match lil_client::resolve_workspace(&cfg.env, None, &cfg.env.local_dir()) {
        Ok(ws) => cfg.workspace = ws.id,
        Err(e) => tracing::warn!(error = %e, "no default workspace: the app starts without one"),
    }
    serve(cfg);
    String::new()
}

/// The app's log lasts for the process.
static LOG: OnceLock<lil_platform::logs::LogGuard> = OnceLock::new();

fn init_logs() -> String {
    let env = match lil_platform::Env::from_process() {
        Ok(env) => env,
        Err(e) => return format!("Can't tell which LILITH folders to use: {e}"),
    };
    match lil_platform::logs::init(&env, crate::QT_ARGV0) {
        Ok(guard) => {
            let _ = LOG.set(guard);
            String::new()
        }
        Err(e) => e.to_string(),
    }
}

fn log_line(line: &str) {
    let Ok(serde_json::Value::Object(mut fields)) = serde_json::from_str(line) else {
        tracing::info!(target: "qt", line, "log line");
        return;
    };
    let text = |fields: &serde_json::Map<String, serde_json::Value>, key: &str| {
        fields
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let event = text(&fields, "event");
    if event == "qt_msg" {
        let (qt_category, msg) = (text(&fields, "category"), text(&fields, "msg"));
        match text(&fields, "type").as_str() {
            "debug" => tracing::debug!(target: "qt", qt_category, "{msg}"),
            "info" => tracing::info!(target: "qt", qt_category, "{msg}"),
            "warning" => tracing::warn!(target: "qt", qt_category, "{msg}"),
            _ => tracing::error!(target: "qt", qt_category, "{msg}"),
        }
        return;
    }
    fields.remove("event");
    let fields = serde_json::Value::Object(fields).to_string();
    tracing::info!(target: "app", event, fields, "app event");
}

/// `firstFrameMs` and `interactiveMs`, once noted.
static START_TIMES: Mutex<Vec<(&'static str, f64)>> = Mutex::new(Vec::new());

fn note_start_time(what: &str, ms: f64) {
    let key = match what {
        "first_frame" => "firstFrameMs",
        "interactive" => "interactiveMs",
        _ => return,
    };
    {
        let mut times = START_TIMES.lock().unwrap_or_else(PoisonError::into_inner);
        times.retain(|(k, _)| *k != key);
        times.push((key, ms));
    }
    core_object::start_time_noted();
}

/// `Core.startTimes` as JSON.
fn start_times_json() -> String {
    let times = START_TIMES.lock().unwrap_or_else(PoisonError::into_inner);
    let map: serde_json::Map<String, serde_json::Value> = times
        .iter()
        .map(|(k, ms)| ((*k).to_string(), serde_json::Value::from(*ms)))
        .collect();
    serde_json::Value::Object(map).to_string()
}

fn env_left() -> Vec<String> {
    let names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    crate::qt_env_to_clear(names.iter().map(String::as_str))
}
