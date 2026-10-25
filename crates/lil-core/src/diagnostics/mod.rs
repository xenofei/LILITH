//! Diagnostics (ARCH-117–122, QA-082–085; P4a-R19, P4a-R24; Appendix G "Diagnostics"):
//! infrastructure inside `lil-core`, like `desktop`, with the always-available module
//! `diagnostics`.
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `diagnostics.recent` | read | `{limit?}` (1–200, default 50) | `{errors: [ErrorGroup]}` |
//! | `diagnostics.bundle` | write | `{job_id?}` | [`BundleResult`] |
//!
//! - **Recent errors** ([`recent`]): the dispatcher tells [`Diagnostics::record_error`] of
//!   every error outcome of a `tools/call` (through `tools`' `ErrorSink`, so `tools` names no
//!   type of this module); the last [`RECENT_CAPACITY`] are kept in memory, grouped by code,
//!   and each is published on the `diagnostics` topic as `{kind: "error", code, tool?, at}`.
//! - **The bundle** ([`bundle`]): one zip in `<local dir>\diagnostics\`, written through the
//!   store's cache writer, never holding a workspace file, every entry redacted
//!   ([`redact`]). Nothing is sent anywhere (ARCH-122): the bundle leaves the machine only
//!   when you hand it on.
//! - **Versions**: the apps whose `core/hello` this core answered, by app, with the
//!   `build_id` each gave (a [`ConnObserver`]).
//!
//! The server builds it with the telemetry and the engines' slot, registers its tools, sets
//! it as the dispatcher's error sink and adds it to the router's observers. No other module
//! names it.

pub mod bundle;
pub mod recent;
pub mod redact;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lil_platform::{Clock, Env};
use lil_protocol::{CallerKind, EffectClass, ErrorBody, codes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use bundle::{BundleResult, LOG_TAIL_BYTES};
pub use recent::{DEFAULT_RECENT, ErrorGroup, MAX_RECENT, RECENT_CAPACITY};
pub use redact::Redactor;

use crate::engines::tools::EnginesSlot;
use crate::events::EventBus;
use crate::jobs::JobId;
use crate::session::Session;
use crate::telemetry::{Telemetry, format_ts};
use crate::tools::{ErrorSink, Registry, typed_tool};
use crate::transport::ConnObserver;

/// The module of the diagnostics tools (always available, P4a-R24).
pub const DIAGNOSTICS_MODULE: &str = "diagnostics";
/// The topic each recorded error is published on.
pub const DIAGNOSTICS_TOPIC: &str = "diagnostics";

/// Where the bundle reads the hardware and the engines; either may be missing (tests).
#[derive(Clone, Default)]
pub struct DiagnosticsSources {
    pub telemetry: Option<Arc<Telemetry>>,
    pub engines: Option<EnginesSlot>,
}

/// The core's diagnostics (module docs).
pub struct Diagnostics {
    env: Env,
    clock: Arc<dyn Clock>,
    events: Arc<EventBus>,
    sources: DiagnosticsSources,
    errors: Mutex<recent::Ring>,
    /// Each app that said hello, with its `build_id`.
    apps: Mutex<BTreeMap<String, String>>,
}

impl std::fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Diagnostics").finish_non_exhaustive()
    }
}

/// `m`, also when a holder panicked: every update is one statement.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Diagnostics {
    pub fn new(
        env: Env,
        clock: Arc<dyn Clock>,
        events: Arc<EventBus>,
        sources: DiagnosticsSources,
    ) -> Arc<Diagnostics> {
        Arc::new(Diagnostics {
            env,
            clock,
            events,
            sources,
            errors: Mutex::new(recent::Ring::default()),
            apps: Mutex::new(BTreeMap::new()),
        })
    }

    /// Keeps an error outcome (at most the last [`RECENT_CAPACITY`]) and publishes it on the
    /// `diagnostics` topic as `{kind: "error", code, tool?, at}`.
    pub fn record_error(&self, code: &str, tool: Option<&str>, message: &str, at_ms: i64) {
        lock(&self.errors).push(code, tool, message, at_ms);
        let mut data = json!({"kind": "error", "code": code});
        if let Some(tool) = tool {
            data["tool"] = json!(tool);
        }
        data["at"] = json!(format_ts(at_ms));
        self.events.publish(DIAGNOSTICS_TOPIC, data);
    }

    /// The kept errors, one group per code, the one whose last error is newest first; at most
    /// `limit` groups.
    pub fn recent(&self, limit: u32) -> Vec<ErrorGroup> {
        lock(&self.errors).groups(limit)
    }

    /// The apps that said hello, by app, with their `build_id`.
    pub(crate) fn apps(&self) -> BTreeMap<String, String> {
        lock(&self.apps).clone()
    }
}

impl ErrorSink for Diagnostics {
    fn record_error(&self, code: &str, tool: Option<&str>, message: &str, at_ms: i64) {
        Diagnostics::record_error(self, code, tool, message, at_ms);
    }
}

impl ConnObserver for Diagnostics {
    fn said_hello(&self, _conn_id: u64, session: &Session, build_id: &str) {
        if let CallerKind::Ui(app) = &session.caller {
            lock(&self.apps).insert(app.clone(), build_id.to_string());
        }
    }

    fn closed(&self, _conn_id: u64) {}
}

const RECENT_DESCRIPTION: &str = "Returns the core's recent errors grouped by code, the code \
    whose last error is newest first: for each, the code, how many of the core's last 1,000 \
    error outcomes had it, when the first and the last of them were (RFC 3339 UTC), the last \
    one's message and the tool it came from. `limit` is 1-200 (default 50).";

const BUNDLE_DESCRIPTION: &str = "Writes a diagnostics bundle, one zip in the suite's \
    diagnostics folder, and returns its path, its size in bytes, the files in it and how many \
    values were redacted. It holds the component versions, the redacted suite.toml, the recent \
    errors, a hardware snapshot, the engines' status and environment, and the newest 5 MiB of \
    each process's newest log; with job_id, that job's summary and its tool arguments \
    (untrusted). Secrets, Claude Code profile labels, e-mail addresses and Credential Manager \
    names are redacted, the user-profile folder becomes %USERPROFILE% and the computer name \
    <machine>. It never holds a workspace file, and nothing is sent anywhere: the bundle \
    leaves this PC only when you hand it on.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RecentArgs {
    /// 1–200 (default 50).
    #[serde(default)]
    #[schemars(range(min = 1, max = 200))]
    limit: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct RecentResult {
    errors: Vec<ErrorGroup>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BundleArgs {
    /// A job (`j_<16 hex>`) whose summary and tool arguments the bundle adds.
    #[serde(default)]
    job_id: Option<String>,
}

/// Registers `diagnostics.recent` and `diagnostics.bundle` on `reg`.
pub fn register_diagnostics_tools(reg: &mut Registry, diagnostics: &Arc<Diagnostics>) {
    let d = diagnostics.clone();
    reg.register(typed_tool(
        "diagnostics.recent",
        RECENT_DESCRIPTION,
        EffectClass::Read,
        DIAGNOSTICS_MODULE,
        move |ctx, args: RecentArgs| {
            let d = d.clone();
            Box::pin(async move {
                let limit = args.limit.unwrap_or(DEFAULT_RECENT);
                if !(1..=MAX_RECENT).contains(&limit) {
                    return Err(ErrorBody::new(
                        &codes::PARAMS_INVALID,
                        format!("limit is 1-{MAX_RECENT}, not {limit}"),
                        ctx.trace_id.clone(),
                    ));
                }
                let errors = d.recent(limit);
                let summary = format!("{} error code(s)", errors.len());
                Ok((RecentResult { errors }, summary))
            })
        },
    ));
    let d = diagnostics.clone();
    reg.register(typed_tool(
        "diagnostics.bundle",
        BUNDLE_DESCRIPTION,
        EffectClass::Write,
        DIAGNOSTICS_MODULE,
        move |ctx, args: BundleArgs| {
            let d = d.clone();
            Box::pin(async move {
                let job = args.job_id.map(JobId);
                let result = d.bundle(ctx, job.as_ref()).await?;
                let summary = format!(
                    "diagnostics bundle {} ({} files, {} redactions)",
                    result.path,
                    result.files.len(),
                    result.redactions
                );
                Ok((result, summary))
            })
        },
    ));
}
