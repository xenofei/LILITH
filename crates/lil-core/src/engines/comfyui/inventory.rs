//! The model inventory and the environment capture (CORE-130, ARCH-060; Appendix F "ComfyUI
//! client"): a `cpu`-lane job of kind `comfy.inventory`, which the engines submit each time
//! ComfyUI becomes ready (the lane runs one job at a time).
//!
//! - **Files.** Every file below `<root>\models\` with a model extension
//!   ([`super::graph::MODEL_EXTENSIONS`]), linked folders followed (to [`MAX_DEPTH`] levels).
//! - **Hashes.** Each file's SHA-256 is cached in the suite `state.db` (`model_hashes`) by its
//!   path, size, mtime and FileId: a file whose three facts are unchanged is never read again
//!   ([`scan`]). A cloud placeholder is never read (that would download it): it is listed
//!   without a hash.
//! - **Names.** A file whose hash equals a `files[].sha256` of the default workspace's
//!   `registry/models.yaml` is `{model_id, model_name, size, sha256}` (the entry's `id` and
//!   `name`, or its `id` twice; trusted, the file name left out); any other is `{name, size,
//!   sha256, untrusted: true}`, its file name being the outside world's (ARCH-060).
//! - **Result.** `structured` is `{environment, models}` ([`super::environment::EnvCapture`]);
//!   the inventory is kept for the graph checks and `engines.status` while the engine stays
//!   ready.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use lil_platform::fs::{FileIdentity, Probe};
use lil_protocol::{ErrorBody, codes};
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::client::hex;
use super::environment;
use super::graph::is_model_file;
use crate::engines::{ENGINES_MODULE, Engines};
use crate::jobs::{
    JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobSpec, Lane, Priority, cancelled_error,
};
use crate::state::{StateDb, StateError};

/// The job kind.
pub const INVENTORY_KIND: &str = "comfy.inventory";
/// How deep below `models\` the walk goes (a linked folder that loops ends there).
pub const MAX_DEPTH: usize = 12;
/// The read size while hashing.
const CHUNK: usize = 1024 * 1024;

/// One model file of the inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    /// Below `<root>\models`, with `\` (`checkpoints\sub\a.safetensors`).
    pub path: String,
    pub size: u64,
    /// `None` for a cloud placeholder (never read).
    pub sha256: Option<String>,
}

/// The inventory kept while the engine stays ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    pub files: Vec<ModelFile>,
    /// RFC 3339 UTC.
    pub refreshed: String,
    /// ComfyUI's command line (`system_stats.system.argv`): where it looks for models.
    pub argv: Vec<String>,
}

impl Inventory {
    /// The files' paths (what the model check matches references against).
    pub fn paths(&self) -> Vec<String> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }
}

/// The `JobSpec` of an inventory for the engine's readiness `epoch`.
pub fn inventory_spec(epoch: u64) -> JobSpec {
    JobSpec {
        kind: INVENTORY_KIND,
        lane: Lane::Cpu,
        // A core-submitted job is `cli`, so `agent` (Appendix F "Jobs").
        priority: Priority::Agent,
        module: ENGINES_MODULE.to_string(),
        args: json!({ "epoch": epoch }),
        project: None,
        asset_id: None,
        doc_id: None,
        expected_secs: 0,
        expected_output_bytes: 0,
        vram_need_mib: None,
        engine: None,
        group: None,
        external_side_effects: false,
    }
}

/// Why a scan stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    Cancelled,
    /// The cache could not be read or written.
    State(String),
}

/// One file found by the walk.
struct Found {
    abs: PathBuf,
    rel: String,
    id: FileIdentity,
}

/// What a walk found: the model files, and the folders it could not read (their cached rows
/// are kept: the folder may be back next time).
#[derive(Default)]
struct Walked {
    files: Vec<Found>,
    unreadable: Vec<String>,
}

/// Asked between files and between the chunks of a file being hashed.
pub type Cancelled = Arc<dyn Fn() -> bool + Send + Sync>;

/// A cached row: size, mtime, FileId and SHA-256.
type Row = (u64, i64, String, String);

/// The model files below `models_dir`, with their hashes (module docs): a file whose path,
/// size, mtime and FileId match its cached row is not read, nor one whose FileId, size and
/// mtime match another row (the same file reached through another link); the others are
/// hashed through `probe` and the cache is updated. Rows of files gone from `models_dir` are
/// dropped, but not those below a folder that could not be read. `cancelled` is asked
/// between files and every chunk of a hash. A missing `models_dir` has no files.
///
/// # Errors
/// [`ScanError`].
pub async fn scan(
    models_dir: &Path,
    probe: Arc<dyn Probe>,
    db: Arc<StateDb>,
    cancelled: Cancelled,
) -> Result<Vec<ModelFile>, ScanError> {
    let dir = models_dir.to_path_buf();
    let walk_probe = probe.clone();
    let walked = tokio::task::spawn_blocking(move || walk(&dir, walk_probe.as_ref()))
        .await
        .unwrap_or_default();
    let prefix = format!("{}\\", models_dir.display()).to_lowercase();
    let cached: HashMap<String, Row> = db
        .call(move |conn| {
            let run = || -> rusqlite::Result<HashMap<String, Row>> {
                let mut st =
                    conn.prepare("SELECT path, size, mtime, file_id, sha256 FROM model_hashes")?;
                let rows = st.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        (
                            r.get::<_, i64>(1)?.cast_unsigned(),
                            r.get::<_, i64>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                        ),
                    ))
                })?;
                rows.collect()
            };
            run().map_err(|e| StateError::Io(e.to_string()))
        })
        .await
        .map_err(|e| ScanError::State(e.to_string()))?;
    let by_identity: HashMap<(String, u64, i64), String> = cached
        .values()
        .filter(|(_, _, id, _)| !id.is_empty())
        .map(|(size, mtime, id, sha)| ((id.clone(), *size, *mtime), sha.clone()))
        .collect();
    let mut files = Vec::with_capacity(walked.files.len());
    let mut fresh: Vec<(String, u64, i64, String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for f in walked.files {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let key = f.abs.display().to_string();
        seen.insert(key.clone());
        let file_id = f.id.file_id.map(|b| hex(&b)).unwrap_or_default();
        let known = match cached.get(&key) {
            Some((size, mtime, id, sha))
                if *size == f.id.size && *mtime == f.id.last_write && *id == file_id =>
            {
                Some(sha.clone())
            }
            _ if !file_id.is_empty() => by_identity
                .get(&(file_id.clone(), f.id.size, f.id.last_write))
                .cloned(),
            _ => None,
        };
        let sha256 = if f.id.is_cloud_placeholder() {
            None
        } else if let Some(sha) = known {
            if !cached.contains_key(&key) {
                fresh.push((
                    key.clone(),
                    f.id.size,
                    f.id.last_write,
                    file_id,
                    sha.clone(),
                ));
            }
            Some(sha)
        } else {
            let (abs, p, stop) = (f.abs.clone(), probe.clone(), cancelled.clone());
            match tokio::task::spawn_blocking(move || hash_file(p.as_ref(), &abs, stop.as_ref()))
                .await
            {
                Ok(Ok(sha)) => {
                    fresh.push((
                        key.clone(),
                        f.id.size,
                        f.id.last_write,
                        file_id,
                        sha.clone(),
                    ));
                    Some(sha)
                }
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::Interrupted => {
                    return Err(ScanError::Cancelled);
                }
                Ok(Err(e)) => {
                    tracing::warn!(file = %f.abs.display(), error = %e, "a model file could not be hashed");
                    None
                }
                Err(_) => None,
            }
        };
        files.push(ModelFile {
            path: f.rel,
            size: f.id.size,
            sha256,
        });
    }
    let kept: Vec<String> = walked.unreadable.iter().map(|d| d.to_lowercase()).collect();
    let gone: Vec<String> = cached
        .keys()
        .filter(|k| {
            let lower = k.to_lowercase();
            lower.starts_with(&prefix)
                && !seen.contains(*k)
                && !kept.iter().any(|d| lower.starts_with(&format!("{d}\\")))
        })
        .cloned()
        .collect();
    if !fresh.is_empty() || !gone.is_empty() {
        db.call(move |conn| {
            let mut run = || -> rusqlite::Result<()> {
                let tx = conn.transaction()?;
                for (path, size, mtime, file_id, sha256) in &fresh {
                    tx.execute(
                        "INSERT INTO model_hashes (path, size, mtime, file_id, sha256) \
                         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(path) DO UPDATE SET \
                         size = ?2, mtime = ?3, file_id = ?4, sha256 = ?5",
                        params![path, size.cast_signed(), mtime, file_id, sha256],
                    )?;
                }
                for path in &gone {
                    tx.execute("DELETE FROM model_hashes WHERE path = ?1", [path])?;
                }
                tx.commit()
            };
            run().map_err(|e| StateError::Io(e.to_string()))
        })
        .await
        .map_err(|e| ScanError::State(e.to_string()))?;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// The paths below `models_dir` of its model files (as [`ModelFile::path`]), found by walking
/// it again with no hashing: the model check looks here before it refuses a reference the
/// inventory doesn't hold (a model added while ComfyUI runs). Reads folders only: call it off
/// the runtime's threads.
pub fn list_paths(models_dir: &Path, probe: &dyn Probe) -> Vec<String> {
    walk(models_dir, probe)
        .files
        .into_iter()
        .map(|f| f.rel)
        .collect()
}

/// Every model file below `dir` (module docs), with its identity; unreadable entries are
/// logged and left out. Each folder is walked once, by its resolved path, so a linked folder
/// that loops, or two links to one folder, list each file once.
fn walk(dir: &Path, probe: &dyn Probe) -> Walked {
    let mut out = Walked::default();
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(dir.to_path_buf(), String::new(), 0)];
    while let Some((at, rel, depth)) = stack.pop() {
        let resolved = std::fs::canonicalize(&at).map_or_else(
            |_| at.display().to_string().to_lowercase(),
            |p| p.display().to_string().to_lowercase(),
        );
        if !visited.insert(resolved) {
            continue;
        }
        let Ok(entries) = probe.read_dir(&at) else {
            if depth > 0 {
                out.unreadable.push(at.display().to_string());
            }
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = at.join(&name);
            let rel_path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}\\{name}")
            };
            // Follows a linked folder (model folders are often junctions to other drives).
            if path.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, rel_path, depth + 1));
                }
                continue;
            }
            if !is_model_file(&name) {
                continue;
            }
            match probe.identity(&path) {
                Ok(id) => out.files.push(Found {
                    abs: path,
                    rel: rel_path,
                    id,
                }),
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "a model file can't be read")
                }
            }
        }
    }
    out
}

/// The SHA-256 of `path`'s data, read through `probe`; `cancelled` is asked before each
/// chunk, and ends it with [`std::io::ErrorKind::Interrupted`].
fn hash_file(
    probe: &dyn Probe,
    path: &Path,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> std::io::Result<String> {
    let mut file = probe.open_read(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "the hash was cancelled",
            ));
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex(&hash.finalize()))
}
/// The `structured.models` entries of `files`, named from `registry` where the hash matches
/// (module docs).
pub fn model_entries(
    files: &[ModelFile],
    registry: &[lil_schema::engine_files::RegistryModel],
) -> Vec<Value> {
    let by_hash: HashMap<&str, &lil_schema::engine_files::RegistryModel> = registry
        .iter()
        .flat_map(|m| m.sha256.iter().map(move |h| (h.as_str(), m)))
        .collect();
    files
        .iter()
        .map(|f| match f.sha256.as_deref().and_then(|h| by_hash.get(h)) {
            Some(m) => json!({
                "model_id": m.id,
                "model_name": m.name.as_deref().unwrap_or(&m.id),
                "size": f.size,
                "sha256": f.sha256,
            }),
            None => {
                let name = f.path.rsplit('\\').next().unwrap_or(&f.path);
                let mut entry = json!({"name": name, "size": f.size, "untrusted": true});
                if let Some(sha) = &f.sha256 {
                    entry["sha256"] = Value::from(sha.as_str());
                }
                entry
            }
        })
        .collect()
}

/// The entries of `<ws>\registry\models.yaml`; none when there is no file. A file that can't
/// be read or parsed is logged and matches nothing.
fn read_registry(ws: Option<&Path>) -> Vec<lil_schema::engine_files::RegistryModel> {
    let Some(ws) = ws else {
        return Vec::new();
    };
    let path = ws.join("registry").join("models.yaml");
    match std::fs::read_to_string(&path) {
        Ok(text) => lil_schema::engine_files::read_models_yaml(&text).unwrap_or_else(|e| {
            tracing::warn!(file = %path.display(), error = %e, "registry/models.yaml can't be read; no model file is named from it");
            Vec::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(file = %path.display(), error = %e, "registry/models.yaml can't be read");
            Vec::new()
        }
    }
}

/// Runs `comfy.inventory` jobs (module docs).
pub struct InventoryHandler {
    engines: Weak<Engines>,
    probe: Arc<dyn Probe>,
}

impl std::fmt::Debug for InventoryHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InventoryHandler").finish_non_exhaustive()
    }
}

impl InventoryHandler {
    /// A handler for `engines` (held weakly), reading model files through `probe`.
    pub fn new(engines: Weak<Engines>, probe: Arc<dyn Probe>) -> InventoryHandler {
        InventoryHandler { engines, probe }
    }

    async fn attempt(&self, job: &JobCtx) -> Result<JobDone, JobFailure> {
        let trace = |mut e: ErrorBody| {
            e.trace_id.clone_from(&job.trace_id);
            e
        };
        let Some(engines) = self.engines.upgrade() else {
            return Err(JobFailure::Final(trace(ErrorBody::new(
                &codes::CORE_DRAINING,
                "The core is shutting down.",
                "",
            ))));
        };
        let comfy = engines.comfy().clone();
        // The readiness epoch it inventories is the one when it starts (review M5): one that
        // waited in the queue across a reconnect still counts for the engine now.
        let epoch = comfy.ready_epoch();
        let Some(client) = comfy.client().cloned() else {
            return Err(JobFailure::Final(trace(ErrorBody::new(
                &codes::ENGINE_NOT_CONFIGURED,
                "ComfyUI is not configured.",
                "",
            ))));
        };
        let stats = client.system_stats().await.map_err(|e| {
            JobFailure::Retryable(trace(ErrorBody::new(
                &codes::COMFYUI_UNREACHABLE,
                format!("ComfyUI did not answer /system_stats: {e}"),
                "",
            )))
        })?;
        let argv: Vec<String> = stats["system"]["argv"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let root = comfy.config().and_then(|c| c.root.clone());
        let driver = comfy.driver();
        let captured = comfy.now_rfc3339();
        let workspace = comfy.default_workspace();
        let (env_root, when) = (root.clone(), captured.clone());
        let (environment, registry) = tokio::task::spawn_blocking(move || {
            (
                environment::capture(stats, driver, env_root.as_deref(), when),
                read_registry(workspace.as_deref()),
            )
        })
        .await
        .map_err(|e| {
            JobFailure::Final(trace(ErrorBody::new(
                &codes::CORE_BUG,
                format!("the environment capture did not run: {e}"),
                "",
            )))
        })?;
        let files = match &root {
            Some(root) => {
                let cancel = job.cancel.clone();
                scan(
                    &root.join("models"),
                    self.probe.clone(),
                    comfy.suite_state(),
                    Arc::new(move || *cancel.borrow()),
                )
                .await
                .map_err(|e| match e {
                    ScanError::Cancelled => JobFailure::Final(cancelled_error(&job.trace_id)),
                    ScanError::State(why) => JobFailure::Retryable(trace(ErrorBody::new(
                        &codes::STATE_UNAVAILABLE,
                        format!("the model hashes can't be read or kept: {why}"),
                        "",
                    ))),
                })?
            }
            None => Vec::new(),
        };
        let models = model_entries(&files, &registry);
        if root.is_some() {
            comfy.set_inventory(
                epoch,
                Inventory {
                    files,
                    refreshed: captured,
                    argv,
                },
            );
        }
        Ok(JobDone {
            structured: json!({"environment": environment, "models": models}),
            artifacts: Vec::new(),
            record: None,
            windows: Vec::new(),
        })
    }
}

impl JobHandler for InventoryHandler {
    fn kind(&self) -> &'static str {
        INVENTORY_KIND
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(self.attempt(job))
    }

    fn cancel_in_engine<'a>(&'a self, _job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}
