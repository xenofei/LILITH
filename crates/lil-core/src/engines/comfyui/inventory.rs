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
//! - **Families** (CORE-164; Appendix G "Model inventory report"). A `.safetensors`, `.sft`
//!   or `.gguf` file's family is read from its header (`lil_parse`, within its limits) in the
//!   same open as its hash, and cached with it (`model_hashes.family`, `family_read`): it is
//!   read again only when the file's identity changed, or when the last read couldn't open or
//!   read the file. Pickle formats (`.ckpt`, `.pt`, `.pth`, `.bin`) are hashed, never parsed.
//!   Family text is the outside world's (ARCH-060).
//! - **Result.** `structured` is `{environment, models}` ([`super::environment::EnvCapture`]);
//!   the capture is also kept in the suite `state.db` ([`super::environment::store_capture`]).
//!   The inventory is kept for the graph checks and `engines.status` while the engine stays
//!   ready, and the newest one of the core run for `models.report` ([`models_report`]).

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use lil_parse::{Family, HeaderError};
use lil_platform::fs::{FileIdentity, Probe};
use lil_protocol::{ErrorBody, codes};
use rusqlite::params;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::client::hex;
use super::environment;
use super::graph::is_model_file;
use crate::engines::{ENGINES_MODULE, Engines};
use crate::jobs::{
    EngineKind, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobSpec, Lane, Priority,
    cancelled_error,
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
    /// The model family its header names (untrusted, ARCH-060); `None` when it names none, for
    /// a format without a header, or when it couldn't be read.
    pub family: Option<String>,
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

/// A cached row: size, mtime, FileId, SHA-256, family and whether the family was read.
type Row = (u64, i64, String, String, Option<String>, bool);

/// A family read: `Some(family)` once the header gave its answer (a header that is malformed
/// or over the limits answers `None`), `None` when the file couldn't be opened or read, so it is
/// read again next time.
type FamilyRead = Option<Option<String>>;

/// A row to write, by its path.
type Fresh = (String, Row);

/// The extensions whose header names a family (CORE-164); every other model format is a pickle
/// or unknown, and is never parsed.
fn has_header(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["safetensors", "sft", "gguf"]
            .iter()
            .any(|h| e.eq_ignore_ascii_case(h))
    })
}

/// The model files below `models_dir`, with their hashes and families (module docs): a file
/// whose path, size, mtime and FileId match its cached row is not read, nor one whose volume and
/// FileId, size and mtime match another row (the same file reached through another link),
/// unless its family is still to be read; the others are opened once through `probe`, for
/// their header and their hash, and the cache is updated. Rows of files gone from `models_dir`
/// are dropped, but not those below a folder that could not be read. `cancelled` is asked
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
                let mut st = conn.prepare(
                    "SELECT path, size, mtime, file_id, sha256, family, family_read \
                     FROM model_hashes",
                )?;
                let rows = st.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        (
                            r.get::<_, i64>(1)?.cast_unsigned(),
                            r.get::<_, i64>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, Option<String>>(5)?,
                            r.get::<_, i64>(6)? != 0,
                        ),
                    ))
                })?;
                rows.collect()
            };
            run().map_err(|e| StateError::Io(e.to_string()))
        })
        .await
        .map_err(|e| ScanError::State(e.to_string()))?;
    let by_identity: HashMap<(String, u64, i64), &Row> = cached
        .values()
        .filter(|row| !row.2.is_empty())
        .map(|row| ((row.2.clone(), row.0, row.1), row))
        .collect();
    let mut files = Vec::with_capacity(walked.files.len());
    let mut fresh: Vec<Fresh> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for f in walked.files {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let key = f.abs.display().to_string();
        seen.insert(key.clone());
        let file_id = identity_key(&f.id).unwrap_or_default();
        let own = cached.get(&key);
        let known: Option<&Row> = match own {
            Some(row) if row.0 == f.id.size && row.1 == f.id.last_write && row.2 == file_id => {
                Some(row)
            }
            _ if !file_id.is_empty() => by_identity
                .get(&(file_id.clone(), f.id.size, f.id.last_write))
                .copied(),
            _ => None,
        };
        let (sha256, family) = if f.id.is_cloud_placeholder() {
            (None, None)
        } else {
            // A file whose hash isn't known, or whose family is still to be read, is opened
            // once for both; any other isn't opened.
            let read = if known.is_none_or(|row| !row.5) {
                let need_hash = known.is_none();
                let (abs, p, stop) = (f.abs.clone(), probe.clone(), cancelled.clone());
                match tokio::task::spawn_blocking(move || {
                    read_model(p.as_ref(), &abs, need_hash, stop.as_ref())
                })
                .await
                {
                    Ok(Ok(read)) => Some(read),
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(ScanError::Cancelled);
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(file = %f.abs.display(), error = %e, "a model file could not be read");
                        None
                    }
                    Err(_) => None,
                }
            } else {
                None
            };
            let (family_read, read_sha) = read.unwrap_or((None, None));
            let sha = known.map(|row| row.3.clone()).or(read_sha);
            // The family just read, else the cached one when it was read.
            let (family, was_read) = match (family_read, known) {
                (Some(family), _) => (family, true),
                (None, Some(row)) if row.5 => (row.4.clone(), true),
                _ => (None, false),
            };
            if let Some(sha) = &sha {
                let row: Row = (
                    f.id.size,
                    f.id.last_write,
                    file_id,
                    sha.clone(),
                    family.clone(),
                    was_read,
                );
                if own != Some(&row) {
                    fresh.push((key.clone(), row));
                }
            }
            (sha, family)
        };
        files.push(ModelFile {
            path: f.rel,
            size: f.id.size,
            sha256,
            family,
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
                for (path, (size, mtime, file_id, sha256, family, read)) in &fresh {
                    tx.execute(
                        "INSERT INTO model_hashes \
                         (path, size, mtime, file_id, sha256, family, family_read) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(path) DO UPDATE SET \
                         size = ?2, mtime = ?3, file_id = ?4, sha256 = ?5, family = ?6, \
                         family_read = ?7",
                        params![
                            path,
                            size.cast_signed(),
                            mtime,
                            file_id,
                            sha256,
                            family,
                            i64::from(*read)
                        ],
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
/// logged and left out. Each folder is walked once, by its volume and FileId (else its resolved
/// path), so a linked folder
/// that loops, or two links to one folder, list each file once.
fn walk(dir: &Path, probe: &dyn Probe) -> Walked {
    let mut out = Walked::default();
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(dir.to_path_buf(), String::new(), 0)];
    while let Some((at, rel, depth)) = stack.pop() {
        // The folder itself, past any link: its volume and FileId (re-review N3), else its
        // resolved path.
        let resolved = std::fs::canonicalize(&at).unwrap_or_else(|_| at.clone());
        let key = probe
            .identity(&resolved)
            .ok()
            .and_then(|id| identity_key(&id))
            .unwrap_or_else(|| resolved.display().to_string().to_lowercase());
        if !visited.insert(key) {
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

/// `<volume serial>-<FileId>` (hex): what identifies a file or folder across links, `None`
/// without a FileId.
fn identity_key(id: &FileIdentity) -> Option<String> {
    id.file_id
        .map(|f| format!("{:016x}-{}", id.volume_serial, hex(&f)))
}

/// Opens `path` once through `probe` and reads its family (a header format only, never a
/// pickle: [`has_header`]) and, with `hash`, then its SHA-256. A file of another format that
/// needs no hash isn't opened: its family is `None`, read. `cancelled` is asked before each
/// chunk of the hash, and ends it with [`std::io::ErrorKind::Interrupted`].
///
/// # Errors
/// The file can't be opened, or its hash can't be read.
fn read_model(
    probe: &dyn Probe,
    path: &Path,
    hash: bool,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> std::io::Result<(FamilyRead, Option<String>)> {
    let header = has_header(path);
    if !header && !hash {
        return Ok((Some(None), None));
    }
    let mut file = probe.open_read(path)?;
    let family = if header {
        read_family(&mut file, path)
    } else {
        Some(None)
    };
    let sha = if hash {
        file.seek(std::io::SeekFrom::Start(0))?;
        Some(hash_open(&mut file, cancelled)?)
    } else {
        None
    };
    Ok((family, sha))
}

/// The family in the header of the open `file` (`path` names its format): see [`FamilyRead`].
fn read_family(file: &mut std::fs::File, path: &Path) -> FamilyRead {
    let gguf = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gguf"));
    let read = if gguf {
        // Its walk makes many small reads.
        lil_parse::gguf_family(&mut std::io::BufReader::new(&mut *file))
    } else {
        lil_parse::safetensors_family(file)
    };
    match read {
        Ok(Family::Known(family)) => Some(Some(family)),
        Ok(Family::Unknown) | Err(HeaderError::Malformed(_)) => Some(None),
        // A file shorter than its header says is malformed, not unreadable.
        Err(HeaderError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => Some(None),
        Err(HeaderError::Io(e)) => {
            tracing::warn!(file = %path.display(), error = %e, "a model file's header could not be read");
            None
        }
    }
}

/// The SHA-256 of the rest of `file`; `cancelled` is asked before each chunk, and ends it with
/// [`std::io::ErrorKind::Interrupted`].
fn hash_open(
    file: &mut std::fs::File,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> std::io::Result<String> {
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
        // Kept for `engines.environment` and `lil doctor`; one that can't be kept is logged,
        // and the inventory goes on.
        let kept = serde_json::to_value(&environment).unwrap_or(Value::Null);
        if let Err(e) = environment::store_capture(
            &comfy.suite_state(),
            EngineKind::ComfyUi.wire(),
            &kept,
            comfy.now_ms(),
        )
        .await
        {
            tracing::warn!(error = %e, "the engine environment capture could not be kept");
        }
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

/// `models.report`'s result (CORE-164; Appendix G "Model inventory report").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ModelsReport {
    /// `[engines.comfyui] root`.
    pub root: String,
    /// When the inventory was refreshed (RFC 3339 UTC); `null` before the first one.
    pub refreshed: Option<String>,
    pub models: Vec<ModelReportEntry>,
    pub missing: Vec<MissingModel>,
}

/// One model file of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ModelReportEntry {
    /// Below `<root>\models`, with `\` (untrusted).
    pub path: String,
    pub size: u64,
    /// `null` for a file never read (a cloud placeholder).
    pub sha256: Option<String>,
    /// From the header (untrusted); `null` when none could be read.
    pub family: Option<String>,
    pub states: Vec<ModelState>,
    /// The `registry/models.yaml` entry with its SHA-256 (trusted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// Always true: `path` and `family` are outside text (ARCH-060).
    pub untrusted: bool,
}

/// The inventory glossary's states of a file (CORE-164).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    /// No `registry/models.yaml` entry has its SHA-256.
    Orphan,
    /// Another file has the same SHA-256.
    Duplicate,
    /// Its header names no family.
    UnknownFamily,
}

/// A `registry/models.yaml` SHA-256 that no file has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MissingModel {
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub sha256: String,
}

/// The report's entries for `files` against `registry` (Appendix G): each file's states, and
/// the registry hashes no file has, each once per entry.
pub fn report_entries(
    files: &[ModelFile],
    registry: &[lil_schema::engine_files::RegistryModel],
) -> (Vec<ModelReportEntry>, Vec<MissingModel>) {
    let by_hash: HashMap<&str, &lil_schema::engine_files::RegistryModel> = registry
        .iter()
        .flat_map(|m| m.sha256.iter().map(move |h| (h.as_str(), m)))
        .collect();
    let mut copies: HashMap<&str, usize> = HashMap::new();
    for sha in files.iter().filter_map(|f| f.sha256.as_deref()) {
        *copies.entry(sha).or_default() += 1;
    }
    let models = files
        .iter()
        .map(|f| {
            let sha = f.sha256.as_deref();
            let entry = sha.and_then(|h| by_hash.get(h));
            let mut states = Vec::new();
            if entry.is_none() {
                states.push(ModelState::Orphan);
            }
            if sha.is_some_and(|h| copies.get(h).is_some_and(|n| *n > 1)) {
                states.push(ModelState::Duplicate);
            }
            if f.family.is_none() {
                states.push(ModelState::UnknownFamily);
            }
            ModelReportEntry {
                path: f.path.clone(),
                size: f.size,
                sha256: f.sha256.clone(),
                family: f.family.clone(),
                states,
                model_id: entry.map(|m| m.id.clone()),
                model_name: entry.and_then(|m| m.name.clone()),
                untrusted: true,
            }
        })
        .collect();
    let mut missing: Vec<MissingModel> = Vec::new();
    for m in registry {
        for sha in &m.sha256 {
            let listed = missing
                .iter()
                .any(|x| x.model_id == m.id && x.sha256 == *sha);
            if !copies.contains_key(sha.as_str()) && !listed {
                missing.push(MissingModel {
                    model_id: m.id.clone(),
                    model_name: m.name.clone(),
                    sha256: sha.clone(),
                });
            }
        }
    }
    (models, missing)
}

/// `models.report` (CORE-164; Appendix G): the newest inventory of this core run with the
/// glossary's states, matched against the default workspace's `registry/models.yaml` as it is
/// now. Before the first inventory, `refreshed` is `null` and both lists are empty. Nothing is
/// moved or deleted.
///
/// # Errors
/// `engine.not_configured` without `[engines.comfyui] root`.
pub async fn models_report(engines: &Engines) -> Result<ModelsReport, ErrorBody> {
    let comfy = engines.comfy();
    let root = comfy.config().and_then(|c| c.root.clone()).ok_or_else(|| {
        ErrorBody::new(
            &codes::ENGINE_NOT_CONFIGURED,
            "The model report reads ComfyUI's models folder: set [engines.comfyui] root in \
             suite.toml.",
            "",
        )
    })?;
    let root = root.display().to_string();
    let Some(inventory) = comfy.last_inventory() else {
        return Ok(ModelsReport {
            root,
            refreshed: None,
            models: Vec::new(),
            missing: Vec::new(),
        });
    };
    let workspace = comfy.default_workspace();
    let registry = tokio::task::spawn_blocking(move || read_registry(workspace.as_deref()))
        .await
        .unwrap_or_default();
    let (models, missing) = report_entries(&inventory.files, &registry);
    Ok(ModelsReport {
        root,
        refreshed: Some(inventory.refreshed.clone()),
        models,
        missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(volume_serial: u64, file_id: Option<[u8; 16]>) -> FileIdentity {
        FileIdentity {
            volume_serial,
            file_id,
            size: 1,
            last_write: 1,
            change_time: None,
            attributes: 0,
        }
    }

    /// The cached-hash and folder identity is the volume serial plus the FileId (final-3a M1):
    /// one FileId on two volumes is two files, so a hash is never reused across volumes, and
    /// a file without a FileId has no identity.
    #[test] // spec: CORE-130
    fn identity_is_the_volume_serial_and_the_file_id() {
        let f = Some([7u8; 16]);
        assert_eq!(identity_key(&id(1, f)), identity_key(&id(1, f)));
        assert_ne!(identity_key(&id(1, f)), identity_key(&id(2, f)));
        assert_ne!(
            identity_key(&id(1, f)),
            identity_key(&id(1, Some([8u8; 16])))
        );
        assert_eq!(identity_key(&id(1, None)), None);
    }
}
