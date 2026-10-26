//! `diagnostics.bundle` (ARCH-121, QA-084; P4a-R19; Appendix G "Diagnostics"): one zip,
//! `<local dir>\diagnostics\lilith-diagnostics-<YYYYMMDDTHHMMSSZ>.zip` (or `-2`, `-3` … when a
//! bundle of that second is there: it is never replaced), holding
//!
//! - `manifest.json`: `{bundle: 1, created, build_id, channel, protocol}`;
//! - `versions.json`: the core, each app that said hello, ComfyUI, the Qt pair and
//!   KDDockWidgets (`null`: protocol 1.2 carries no such version) and the driver;
//! - `suite.toml`, redacted ([`Redactor::toml`]), left out when there is none;
//! - `errors.json`: `diagnostics.recent` with limit 200;
//! - `hardware.json`: the telemetry `Snapshot`;
//! - `engines.json`: `{status: engines.status, environment: engines.environment}`;
//! - `logs/<file>`: the newest [`LOG_TAIL_BYTES`] of each process's newest
//!   `<process>.<YYYY-MM-DD>.log`, cut at a line start and redacted line by line, at most
//!   [`LOGS_TOTAL_BYTES`] in all (`lil-core`'s first, then by name);
//! - with a job, `job.json`: `{job, tool, arguments, untrusted: true}`.
//!
//! Never a workspace file. Every entry is redacted ([`super::redact`]); `redactions` counts
//! the replacements. The zip is built in memory and written by the store's cache writer
//! (`fsops::write_new_suite_cache_file`: a temp file in the diagnostics folder, then a move
//! that never replaces), so nothing is written anywhere else. No path is handed out twice: a
//! taken name, or one this core handed out already, is skipped for the next number. Then the
//! folder keeps the newest [`KEEP_BUNDLES`] bundles, ordered by time stamp, then number (as a
//! number); a bundle whose file was made less than [`YOUNG_BUNDLE`] ago (by the file system's
//! creation time, not its name's time stamp) is never removed, nor the one created most
//! recently before the new one, whatever the clocks say, so a burst keeps every bundle of it
//! and no bundle removes the one made just before it. A log whose
//! file name a rule changes is named `logs/redacted-<n>.<date>.log`. Nothing is sent anywhere
//! (ARCH-122).

use std::collections::{BTreeMap, HashSet};
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lil_platform::Env;
use lil_protocol::{BUILD_ID, ErrorBody, PROTOCOL, codes, schema_hash};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use super::redact::{Compiled, Redactor, profile_values};
use super::{Diagnostics, MAX_MESSAGE_CHARS, MAX_RECENT, cut_message};
use crate::engines::comfyui::environment;
use crate::jobs::JobId;
use crate::store::fsops;
use crate::tools::CallCtx;

/// How much of each process's newest log goes in.
pub const LOG_TAIL_BYTES: u64 = 5 * 1024 * 1024;
/// How much of the logs goes in, in all.
pub const LOGS_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
/// How many bundles the diagnostics folder keeps, beyond those made less than
/// [`YOUNG_BUNDLE`] ago.
pub const KEEP_BUNDLES: usize = 10;
/// A bundle made less than this long ago (by its file's creation time) is never removed.
pub const YOUNG_BUNDLE: Duration = Duration::from_secs(60);
/// A bundle's name before its time stamp.
const BUNDLE_PREFIX: &str = "lilith-diagnostics-";
/// The manifest's `bundle` version.
const BUNDLE_VERSION: u32 = 1;

/// `diagnostics.bundle`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct BundleResult {
    /// The zip.
    pub path: String,
    pub bytes: u64,
    /// The zip's entries, in order (`logs/<file>` for the logs).
    pub files: Vec<String>,
    /// How many values were replaced.
    pub redactions: u32,
}

/// What the bundle holds besides the files it reads itself.
struct Parts {
    unix_secs: i64,
    entries: Vec<(&'static str, Value)>,
}

impl Diagnostics {
    /// Writes the bundle (module docs), with job `job`'s summary and tool arguments when
    /// given.
    ///
    /// # Errors
    /// `job.unknown` for a job of another scope or none (nothing is written);
    /// `state.unavailable`; `store.io` when the zip can't be written.
    pub async fn bundle(
        &self,
        ctx: &CallCtx,
        job: Option<&JobId>,
    ) -> Result<BundleResult, ErrorBody> {
        let job_entry = match job {
            Some(id) => {
                let (summary, origin) = ctx.jobs.get_with_origin(&ctx.job_caller(), id).await?;
                Some(json!({
                    "job": summary,
                    "tool": origin.as_ref().map(|o| o.tool.clone()),
                    "arguments": origin.map(|o| o.arguments),
                    "untrusted": true,
                }))
            }
            None => None,
        };
        let now = self
            .clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let unix_ms = i64::try_from(now.as_millis()).unwrap_or(i64::MAX);
        let unix_secs = unix_ms.div_euclid(1000);
        let engines = self
            .sources
            .engines
            .as_ref()
            .and_then(|slot| slot.get().cloned());
        let (status, environment) = match &engines {
            Some(engines) => {
                let status = engines.status().await;
                let environment = match environment::environment(engines).await {
                    Ok(view) => serde_json::to_value(view).unwrap_or(Value::Null),
                    Err(e) => json!({ "error": e.message }),
                };
                (
                    serde_json::to_value(status).unwrap_or(Value::Null),
                    environment,
                )
            }
            None => (json!([]), Value::Null),
        };
        let snapshot = self.sources.telemetry.as_ref().map(|t| t.snapshot());
        let comfyui = status
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|s| s["version"].as_str().map(str::to_string))
            .or_else(|| newest_capture(&environment, "comfyui"));
        let driver = snapshot
            .as_ref()
            .and_then(|s| s.gpu.as_ref().map(|g| g.driver.clone()))
            .or_else(|| newest_capture(&environment, "driver"));
        let apps: Vec<Value> = self
            .apps()
            .into_iter()
            .map(|(app, build_id)| json!({"app": app, "build_id": build_id}))
            .collect();
        let mut entries = vec![
            (
                "manifest.json",
                json!({
                    "bundle": BUNDLE_VERSION,
                    "created": crate::telemetry::format_ts(unix_ms),
                    "build_id": BUILD_ID,
                    "channel": self.env.channel().to_string(),
                    "protocol": PROTOCOL.to_string(),
                }),
            ),
            (
                "versions.json",
                json!({
                    "core": {"build_id": BUILD_ID, "protocol": PROTOCOL.to_string(),
                             "schema_hash": schema_hash()},
                    "apps": apps,
                    "comfyui": comfyui,
                    "qt": null,
                    "qt_webengine": null,
                    "kddockwidgets": null,
                    "driver": driver,
                }),
            ),
            (
                "errors.json",
                json!({ "errors": self.recent_uncut(MAX_RECENT) }),
            ),
            (
                "hardware.json",
                snapshot.map_or(Value::Null, |s| {
                    serde_json::to_value(s).unwrap_or(Value::Null)
                }),
            ),
            (
                "engines.json",
                json!({ "status": status, "environment": environment }),
            ),
        ];
        if let Some(job) = job_entry {
            entries.push(("job.json", job));
        }
        let parts = Parts { unix_secs, entries };
        let env = self.env.clone();
        let handed = self.handed.clone();
        let trace_id = ctx.trace_id.clone();
        let written = tokio::task::spawn_blocking(move || write_bundle(&env, parts, &handed))
            .await
            .map_err(|e| {
                ErrorBody::new(
                    &codes::CORE_BUG,
                    format!("the diagnostics bundle stopped: {e}. This is a bug in LILITH."),
                    trace_id.clone(),
                )
            })?;
        let result = written.map_err(|e| {
            ErrorBody::new(
                &codes::STORE_IO,
                format!("The diagnostics bundle could not be written: {e}"),
                trace_id,
            )
        })?;
        tracing::info!(
            files = result.files.len(),
            bytes = result.bytes,
            redactions = result.redactions,
            "diagnostics bundle written"
        );
        Ok(result)
    }
}

/// `key` of the newest engine capture in an `engines.environment` view, if known.
fn newest_capture(environment: &Value, key: &str) -> Option<String> {
    environment["captures"]
        .get(0)
        .and_then(|c| c[key].as_str())
        .map(str::to_string)
}

/// Builds, redacts and writes the zip (blocking: it reads the logs and `suite.toml`).
fn write_bundle(
    env: &Env,
    parts: Parts,
    handed: &Mutex<HashSet<PathBuf>>,
) -> std::io::Result<BundleResult> {
    let suite_toml = match std::fs::read_to_string(env.suite_toml_to_read()) {
        Ok(text) => Some(text),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, "suite.toml can't be read; the bundle leaves it out");
            }
            None
        }
    };
    let profiles = |key: &str| {
        suite_toml
            .as_deref()
            .map(|doc| profile_values(doc, key))
            .unwrap_or_default()
    };
    let redactor = Redactor {
        labels: profiles("label"),
        profile_dir: Some(env.user_profile().display().to_string()),
        machine: env.computer_name(),
        machine_names: env.dns_host_name().into_iter().collect(),
        names: profiles("config_dir"),
    }
    .compile();
    let mut redactions = 0u32;
    let mut entries: Vec<(String, String)> = Vec::new();
    for (name, mut value) in parts.entries {
        if name == "errors.json" {
            // Each message redacted whole, then cut: a cut first could leave a fragment no
            // rule knows.
            for group in value["errors"].as_array_mut().into_iter().flatten() {
                if let Some(message) = group["last_message"].as_str() {
                    let (message, n) = redactor.text(message);
                    redactions += n;
                    group["last_message"] = json!(cut_message(&message, MAX_MESSAGE_CHARS));
                }
            }
        }
        let text = serde_json::to_string_pretty(&value).map_err(std::io::Error::other)?;
        let (text, n) = redactor.text(&text);
        redactions += n;
        entries.push((name.to_string(), text));
        if name == "manifest.json"
            && let Some(doc) = &suite_toml
        {
            let (text, n) = redactor.toml(doc);
            redactions += n;
            entries.push(("suite.toml".to_string(), text));
        }
    }
    let mut budget = LOGS_TOTAL_BYTES;
    let mut renamed = 0u32;
    for path in newest_logs(&env.logs_dir()) {
        let Some(file) = path.file_name().map(|f| f.to_string_lossy().into_owned()) else {
            continue;
        };
        if budget == 0 {
            tracing::warn!(log = %file, "the bundle's logs are full; it leaves this one out");
            continue;
        }
        let bytes = match fsops::read_tail(&path, LOG_TAIL_BYTES.min(budget)) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(log = %file, error = %e, "a log can't be read; the bundle leaves it out");
                continue;
            }
        };
        budget = budget.saturating_sub(bytes.len() as u64);
        let (text, n) = redact_lines(&redactor, &String::from_utf8_lossy(&bytes));
        redactions += n;
        // The file name too (a process name can hold the account name).
        let (shown, m) = redactor.text(&file);
        let name = if shown == file {
            format!("logs/{file}")
        } else {
            redactions += m;
            renamed += 1;
            let date = file
                .strip_suffix(".log")
                .and_then(|stem| stem.rsplit_once('.'))
                .map_or("unknown", |(_, date)| date);
            format!("logs/redacted-{renamed}.{date}.log")
        };
        entries.push((name, text));
    }

    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip_time(parts.unix_secs));
    for (name, text) in &entries {
        zip.start_file(name.as_str(), options)
            .map_err(std::io::Error::other)?;
        zip.write_all(text.as_bytes())?;
    }
    let bytes = zip.finish().map_err(std::io::Error::other)?.into_inner();
    let path = env.diagnostics_dir().join(format!(
        "{BUNDLE_PREFIX}{}.zip",
        lil_schema::time::compact(parts.unix_secs)
    ));
    // A path handed out once is never handed out again (compared lower-cased).
    let key = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    let set = || {
        handed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let path = fsops::write_new_suite_cache_file(env, &path, &bytes, &|candidate| {
        set().contains(&key(candidate))
    })?;
    set().insert(key(&path));
    prune_bundles(env, &path);
    Ok(BundleResult {
        path: path.display().to_string(),
        bytes: bytes.len() as u64,
        files: entries.into_iter().map(|(name, _)| name).collect(),
        redactions,
    })
}

/// `name`'s time stamp and number, when it is a bundle's name
/// (`lilith-diagnostics-<YYYYMMDDTHHMMSSZ>[-<n>].zip`; no suffix is number 1).
fn bundle_order(name: &str) -> Option<(String, u64)> {
    let stem = name.strip_prefix(BUNDLE_PREFIX)?.strip_suffix(".zip")?;
    let (stamp, number) = match stem.split_once('-') {
        Some((stamp, k)) => (stamp, k.parse().ok()?),
        None => (stem, 1),
    };
    let b = stamp.as_bytes();
    let stamped = b.len() == 16
        && b.iter().enumerate().all(|(i, c)| match i {
            8 => *c == b'T',
            15 => *c == b'Z',
            _ => c.is_ascii_digit(),
        });
    stamped.then(|| (stamp.to_string(), number))
}

/// Whether the file at `path` was made less than [`YOUNG_BUNDLE`] ago, by the file system's
/// creation time (real time, whatever the name's time stamp says); a time that can't be read
/// counts as young.
fn is_young(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.created())
        .map_or(true, |created| {
            SystemTime::now()
                .duration_since(created)
                .map_or(true, |age| age < YOUNG_BUNDLE)
        })
}

/// The file system's creation time of the file at `path`, if it can be read.
fn created(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.created()).ok()
}

/// Keeps the newest [`KEEP_BUNDLES`] bundles of the diagnostics folder after the bundle `new`
/// was written: while there are more, the oldest is removed (by time stamp, then number), but
/// never one made less than [`YOUNG_BUNDLE`] ago ([`is_young`]), and never the one created most
/// recently before `new` (by creation time, whatever the clocks and the names say), so no
/// bundle removes the one made just before it. Other files are never touched; one that can't
/// be removed is left.
fn prune_bundles(env: &Env, new: &Path) {
    let dir = env.diagnostics_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut bundles: Vec<((String, u64), String)> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| bundle_order(&name).map(|order| (order, name)))
        .collect();
    bundles.sort();
    let new_name = new.file_name().map(|n| n.to_string_lossy().into_owned());
    let new_created = created(new).unwrap_or_else(SystemTime::now);
    let previous = bundles
        .iter()
        .filter(|(_, name)| Some(name) != new_name.as_ref())
        .filter_map(|(_, name)| created(&dir.join(name)).map(|at| (at, name)))
        .filter(|(at, _)| *at <= new_created)
        .max()
        .map(|(_, name)| name.clone());
    let mut excess = bundles.len().saturating_sub(KEEP_BUNDLES);
    for (_, name) in &bundles {
        if excess == 0 {
            break;
        }
        let path = dir.join(name);
        if Some(name) == previous.as_ref() || is_young(&path) {
            continue;
        }
        match fsops::remove_suite_cache_file(env, &path) {
            Ok(()) => excess -= 1,
            Err(e) => {
                tracing::warn!(bundle = %name, error = %e, "an old diagnostics bundle could not be removed");
            }
        }
    }
}

/// `text` redacted line by line.
fn redact_lines(redactor: &Compiled, text: &str) -> (String, u32) {
    let mut out = String::with_capacity(text.len());
    let mut n = 0;
    for line in text.split_inclusive('\n') {
        let (body, end) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        let (redacted, m) = redactor.text(body);
        n += m;
        out.push_str(&redacted);
        out.push_str(end);
    }
    (out, n)
}

/// The newest `<process>.<YYYY-MM-DD>.log` of each process in `dir`, by process name.
fn newest_logs(dir: &Path) -> Vec<PathBuf> {
    let mut newest: BTreeMap<String, String> = BTreeMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some((process, date)) = name
            .strip_suffix(".log")
            .and_then(|stem| stem.rsplit_once('.'))
        else {
            continue;
        };
        let valid = !process.is_empty()
            && process
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && is_date(date);
        if valid && newest.get(process).is_none_or(|d| d.as_str() < date) {
            newest.insert(process.to_string(), date.to_string());
        }
    }
    // The core's own log first, then by name.
    let mut logs: Vec<(bool, String, String)> = newest
        .into_iter()
        .map(|(process, date)| (process != "lil-core", process, date))
        .collect();
    logs.sort();
    logs.into_iter()
        .map(|(_, process, date)| dir.join(format!("{process}.{date}.log")))
        .collect()
}

/// `YYYY-MM-DD` with digits.
fn is_date(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// The zip entries' time: `unix_secs` in UTC (the zip format has no zone), or the format's
/// earliest time if it can't hold it.
fn zip_time(unix_secs: i64) -> zip::DateTime {
    let rfc = lil_schema::time::rfc3339(unix_secs);
    let num = |range: std::ops::Range<usize>| rfc[range].parse::<u16>().unwrap_or(0);
    zip::DateTime::from_date_and_time(
        num(0..4),
        num(5..7) as u8,
        num(8..10) as u8,
        num(11..13) as u8,
        num(14..16) as u8,
        num(17..19) as u8,
    )
    .unwrap_or_default()
}
