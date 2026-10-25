//! Environment capture (Appendix F "ComfyUI client"; CORE-130): `/system_stats`, the NVML
//! driver version, and the revision of ComfyUI and of each `custom_nodes\*` folder: its git
//! commit (`HEAD`, as `git rev-parse HEAD` gives it) and its `pyproject.toml` `[project]
//! version`.
//!
//! The commit is read from the repository's files, not by running git: `.git` (a folder, or a
//! file `gitdir: <path>` as a worktree or submodule has), its `HEAD` (a commit id, or `ref:
//! <name>`), then that ref's loose file or its `packed-refs` line. A repository it can't read
//! has no commit; nothing here fails the capture.
//!
//! **Stored captures** (plan 4a; Appendix G "Engine environments"): a capture whose environment
//! differs from the newest stored one is kept in the suite `state.db` table `engine_captures`,
//! the newest [`KEEP_CAPTURES`] per engine ([`store_capture`]); `engines.environment` ([`environment`]) shows the newest two and what
//! changed between them ([`changes`]). The suite records environments and never restores one
//! (CORE-130; CORE-166 is Later).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lil_protocol::{ErrorBody, codes};
use rusqlite::params;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::engines::Engines;
use crate::jobs::EngineKind;
use crate::state::{StateDb, StateError};

/// How many captures are kept per engine.
pub const KEEP_CAPTURES: u32 = 10;

/// What the environment was when the engine became ready.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvCapture {
    pub system_stats: Value,
    /// The NVML driver version, when there is a GPU reading.
    pub driver: Option<String>,
    /// ComfyUI's commit, else its `pyproject.toml` version.
    pub comfyui: Option<String>,
    /// `(folder name, git commit, pyproject version)` of each `custom_nodes\*` folder, sorted
    /// by name.
    #[serde(serialize_with = "nodes_as_objects")]
    pub nodes: Vec<(String, Option<String>, Option<String>)>,
    /// RFC 3339 UTC.
    pub captured: String,
}

fn nodes_as_objects<S: serde::Serializer>(
    nodes: &[(String, Option<String>, Option<String>)],
    s: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq as _;
    let mut seq = s.serialize_seq(Some(nodes.len()))?;
    for (name, git_rev, version) in nodes {
        seq.serialize_element(&serde_json::json!({
            "name": name, "git_rev": git_rev, "version": version,
        }))?;
    }
    seq.end()
}

/// The capture for an engine whose folder is `root` (none known: no revisions). Reads files
/// only: call it off the runtime's threads.
pub fn capture(
    mut system_stats: Value,
    driver: Option<String>,
    root: Option<&Path>,
    captured: String,
) -> EnvCapture {
    if let Some(argv) = system_stats
        .get_mut("system")
        .and_then(|s| s.get_mut("argv"))
        .and_then(Value::as_array_mut)
    {
        blank_secret_args(argv);
        if let Some(root) = root {
            relative_to_root(argv, root);
        }
    }
    let comfyui = root.and_then(|r| {
        let (rev, version) = revision(r);
        rev.or(version)
    });
    let mut nodes: Vec<(String, Option<String>, Option<String>)> = root
        .and_then(|r| std::fs::read_dir(r.join("custom_nodes")).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let (rev, version) = revision(&e.path());
            (e.file_name().to_string_lossy().into_owned(), rev, version)
        })
        // ComfyUI-Manager's `.disabled` (and any dot folder) holds no node of its own.
        .filter(|(name, _, _)| name != "__pycache__" && !name.starts_with('.'))
        .collect();
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    EnvCapture {
        system_stats,
        driver,
        comfyui,
        nodes,
        captured,
    }
}

/// Each `argv` entry that is a path inside `root` (or `--flag=<such a path>`) is written relative
/// to `root` (`.` for `root` itself), so a stored or served capture holds no absolute path into
/// the ComfyUI folder (plan 4a ruling, 2026-10-24). Other entries are kept.
pub fn relative_to_root(argv: &mut [Value], root: &Path) {
    let base = root.display().to_string().replace('/', "\\");
    let base = base.trim_end_matches('\\');
    if base.is_empty() {
        return;
    }
    let inside = |path: &str| -> Option<String> {
        let path = path.replace('/', "\\");
        let head = path.get(..base.len())?;
        if !head.eq_ignore_ascii_case(base) {
            return None;
        }
        let rest = &path[base.len()..];
        if rest.is_empty() {
            return Some(".".to_string());
        }
        rest.strip_prefix('\\')
            .filter(|r| !r.is_empty())
            .map(str::to_string)
    };
    for arg in argv.iter_mut() {
        let Some(text) = arg.as_str() else { continue };
        let relative = match text.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => {
                inside(value).map(|v| format!("{flag}={v}"))
            }
            _ => inside(text),
        };
        if let Some(relative) = relative {
            *arg = Value::from(relative);
        }
    }
}

/// The value of every flag whose name looks like a secret's (the scrubbed environment's
/// words, ruling P3-R19: `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`,
/// `AUTH`) becomes `"…"`, in either form (`--api-key x`, `--api-key=x`): a launcher may pass
/// one, and the capture lands in a job's result.
pub fn blank_secret_args(argv: &mut [Value]) {
    const WORDS: [&str; 7] = [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ];
    let secret = |flag: &str| {
        let name = flag.trim_start_matches('-').to_ascii_uppercase();
        flag.starts_with('-') && WORDS.iter().any(|w| name.contains(w))
    };
    let mut blank_next = false;
    for arg in argv.iter_mut() {
        let Some(text) = arg.as_str().map(str::to_string) else {
            blank_next = false;
            continue;
        };
        // A flag that takes no value is followed by another flag, which is kept.
        if blank_next && !text.starts_with('-') {
            *arg = Value::from("…");
            blank_next = false;
            continue;
        }
        blank_next = false;
        match text.split_once('=') {
            Some((flag, _)) if secret(flag) => *arg = Value::from(format!("{flag}=…")),
            None if secret(&text) => blank_next = true,
            _ => {}
        }
    }
}

/// `(git commit, pyproject version)` of the folder `dir`.
pub fn revision(dir: &Path) -> (Option<String>, Option<String>) {
    let version = std::fs::read_to_string(dir.join("pyproject.toml"))
        .ok()
        .and_then(|t| lil_schema::engine_files::pyproject_version(&t));
    (git_head(dir), version)
}

/// Whether `s` is a full commit id (40 hex, or 64 for SHA-256 repositories).
fn is_commit(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit `HEAD` names in the repository at `dir` (module docs).
fn git_head(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if target.is_absolute() {
            target
        } else {
            dir.join(target)
        }
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(name) = head.strip_prefix("ref:").map(str::trim) else {
        return is_commit(head).then(|| head.to_ascii_lowercase());
    };
    // A worktree keeps its refs in the main repository (`commondir`).
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| git_dir.join(c.trim()))
        .unwrap_or_else(|| git_dir.clone());
    for base in [&git_dir, &common] {
        if let Ok(id) = std::fs::read_to_string(base.join(name)) {
            let id = id.trim();
            if is_commit(id) {
                return Some(id.to_ascii_lowercase());
            }
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (id, r) = line.split_once(' ')?;
        (r.trim() == name && is_commit(id)).then(|| id.to_ascii_lowercase())
    })
}

/// Keeps `capture` of `engine` taken at `at_ms` (Unix milliseconds; one more than the engine's
/// newest when that is not earlier), then drops all but the newest [`KEEP_CAPTURES`] of it;
/// whether it was kept. A capture whose environment is the newest stored one's (no
/// [`changes`] between them) isn't kept: the stored ones are the last different environments
/// (plan 4a ruling, 2026-10-24).
///
/// # Errors
/// The suite `state.db` can't be written.
pub async fn store_capture(
    db: &StateDb,
    engine: &str,
    capture: &Value,
    at_ms: i64,
) -> Result<bool, StateError> {
    let (engine, text, new) = (engine.to_string(), capture.to_string(), capture.clone());
    db.call(move |conn| {
        let mut run = || -> rusqlite::Result<bool> {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let newest: Option<(i64, String)> = tx
                .query_row(
                    "SELECT captured_utc, capture FROM engine_captures WHERE engine = ?1 \
                     ORDER BY captured_utc DESC LIMIT 1",
                    [&engine],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            // The same environment, unless a known driver arrives after an unknown one: that is
            // stored too (never written into the stored row), so a later driver change is
            // compared with it (plan 4a ruling, 2026-10-24, fix round 2).
            let same = newest.as_ref().is_some_and(|(_, stored)| {
                serde_json::from_str::<Value>(stored).is_ok_and(|old| {
                    let learns_driver =
                        text_of(&old, "driver").is_none() && text_of(&new, "driver").is_some();
                    changes(&new, &old).is_empty() && !learns_driver
                })
            });
            if same {
                return Ok(false);
            }
            let at = newest.map_or(at_ms, |(n, _)| at_ms.max(n.saturating_add(1)));
            tx.execute(
                "INSERT INTO engine_captures (engine, captured_utc, capture) VALUES (?1, ?2, ?3)",
                params![engine, at, text],
            )?;
            tx.execute(
                "DELETE FROM engine_captures WHERE engine = ?1 AND captured_utc NOT IN \
                 (SELECT captured_utc FROM engine_captures WHERE engine = ?1 \
                  ORDER BY captured_utc DESC LIMIT ?2)",
                params![engine, KEEP_CAPTURES],
            )?;
            tx.commit()?;
            Ok(true)
        };
        run().map_err(|e| StateError::Io(e.to_string()))
    })
    .await
}

/// The newest `limit` captures of `engine`, newest first. A capture that isn't JSON (the file
/// was edited by hand) is left out.
///
/// # Errors
/// The suite `state.db` can't be read.
pub async fn recent(db: &StateDb, engine: &str, limit: u32) -> Result<Vec<Value>, StateError> {
    let engine = engine.to_string();
    let texts: Vec<String> = db
        .call(move |conn| {
            let run = || -> rusqlite::Result<Vec<String>> {
                let mut st = conn.prepare(
                    "SELECT capture FROM engine_captures WHERE engine = ?1 \
                     ORDER BY captured_utc DESC LIMIT ?2",
                )?;
                let rows = st.query_map(params![engine, limit], |r| r.get(0))?;
                rows.collect()
            };
            run().map_err(|e| StateError::Io(e.to_string()))
        })
        .await?;
    Ok(texts
        .iter()
        .filter_map(|text| serde_json::from_str(text).ok())
        .collect())
}

/// `engines.environment`'s result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct EnvironmentView {
    /// The newest two captures ([`EnvCapture`]), newest first.
    pub captures: Vec<Value>,
    /// What changed from the older to the newer ([`changes`]).
    pub changes: Vec<EnvChange>,
}

/// What a change is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeWhat {
    Comfyui,
    Node,
    Driver,
}

/// How it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Changed,
    Added,
    Removed,
}

/// One change between two captures. A node's `name` is its `custom_nodes` folder and its
/// `from`/`to` are `<version> (<commit>)`, or whichever of the two is known (untrusted text,
/// as the folder's files say).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EnvChange {
    pub what: ChangeWhat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub change: ChangeKind,
}

/// The change of ComfyUI's revision or the driver (`None`: not known), `None` when there is
/// none.
fn change_of(what: ChangeWhat, from: Option<String>, to: Option<String>) -> Option<EnvChange> {
    let change = match (&from, &to) {
        (None, None) => return None,
        (Some(_), None) => ChangeKind::Removed,
        (None, Some(_)) => ChangeKind::Added,
        (Some(a), Some(b)) if a == b => return None,
        (Some(_), Some(_)) => ChangeKind::Changed,
    };
    Some(EnvChange {
        what,
        name: None,
        from,
        to,
        change,
    })
}

/// A top-level text of a capture; `None` when it is `null` or missing.
fn text_of(capture: &Value, key: &str) -> Option<String> {
    capture.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Each node of a capture by its folder name, with its `<version> (<commit>)` text.
fn nodes_of(capture: &Value) -> BTreeMap<String, Option<String>> {
    capture["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let name = n["name"].as_str()?.to_string();
            let text = match (n["version"].as_str(), n["git_rev"].as_str()) {
                (Some(v), Some(g)) => Some(format!("{v} ({g})")),
                (Some(v), None) => Some(v.to_string()),
                (None, Some(g)) => Some(g.to_string()),
                (None, None) => None,
            };
            Some((name, text))
        })
        .collect()
}

/// What changed from `older` to `newer` (two [`EnvCapture`]s as JSON): ComfyUI's revision,
/// then each node by name, then the driver.
pub fn changes(newer: &Value, older: &Value) -> Vec<EnvChange> {
    let mut out = Vec::new();
    out.extend(change_of(
        ChangeWhat::Comfyui,
        text_of(older, "comfyui"),
        text_of(newer, "comfyui"),
    ));
    let (old_nodes, new_nodes) = (nodes_of(older), nodes_of(newer));
    let mut names: Vec<&String> = old_nodes.keys().chain(new_nodes.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let (from, to) = (old_nodes.get(name).cloned(), new_nodes.get(name).cloned());
        // A node present on one side only is added or removed, revision known or not.
        let change = match (&from, &to) {
            (Some(_), None) => Some(ChangeKind::Removed),
            (None, Some(_)) => Some(ChangeKind::Added),
            (Some(a), Some(b)) if a != b => Some(ChangeKind::Changed),
            _ => None,
        };
        if let Some(change) = change {
            out.push(EnvChange {
                what: ChangeWhat::Node,
                name: Some(name.clone()),
                from: from.flatten(),
                to: to.flatten(),
                change,
            });
        }
    }
    // A driver that isn't known (no GPU reading at the capture) is never a difference.
    if let (Some(from), Some(to)) = (text_of(older, "driver"), text_of(newer, "driver")) {
        out.extend(change_of(ChangeWhat::Driver, Some(from), Some(to)));
    }
    out
}

/// `engines.environment` (CORE-130; Appendix G): ComfyUI's newest two stored captures, newest
/// first, and what changed between them (none with fewer than two).
///
/// # Errors
/// `state.unavailable` when the suite `state.db` can't be read.
pub async fn environment(engines: &Engines) -> Result<EnvironmentView, ErrorBody> {
    let db = engines.comfy().suite_state();
    let captures = recent(&db, EngineKind::ComfyUi.wire(), 2)
        .await
        .map_err(|e| {
            ErrorBody::new(
                &codes::STATE_UNAVAILABLE,
                format!("the engine captures can't be read: {e}"),
                "",
            )
        })?;
    let changes = match captures.as_slice() {
        [newer, older] => changes(newer, older),
        _ => Vec::new(),
    };
    Ok(EnvironmentView { captures, changes })
}
