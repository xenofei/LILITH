//! The store's write path (ARCH-074, ARCH-078, CORE-040, QA-011; Appendix E "Store write
//! algorithm"): the hash check, the flushed temp file, the pending row, `ReplaceFileW` with a
//! backup that is checked afterwards, the `move_new`-only fallbacks, the retries, and the
//! recovery that resolves what a crash left behind.
//!
//! Blocking file calls run on `spawn_blocking` threads and the retry pauses are
//! `tokio::time::sleep`, so paused-time tests drive the 10 s budget. The store reads file data
//! only through `probe.open_read`, after the attribute-only `probe.identity` has ruled out a
//! cloud placeholder, so it never downloads one.
//!
//! Names beside a target `<dir>\<name>`: the temp file `.<name>.ww-tmp-<pid>-<n>`, the
//! backups `.<name>.ww-bak-<pid>-<n>` and `.<name>.ww-bak-<pid>-<n+1>`, and a visible conflict
//! copy `<name>.ww-conflict-<YYYYMMDDTHHMMSSZ>`. No rename ever replaces an existing file.
//!
//! A fail point's error stands for the error of the file call that follows it. Where that call
//! is the rest of a swap (after `store.after_replace`, between the fallback's renames, in the
//! middle of a put-back), the call stops there and keeps its pending row, as a crash would, and
//! the next `recover()` resolves the files.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::future::Future;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use ww_platform::fs::{FileIdentity, Probe, RetryClass, classify};
use ww_platform::{Clock, IdSource};
use ww_protocol::ErrorBody;
use ww_protocol::codes::{
    MANIFEST_NEEDS_REPAIR, STATUS_INVALID_TRANSITION, STORE_BUSY, STORE_DISK_FULL, STORE_GIT_BUSY,
    STORE_HASH_CONFLICT, STORE_IO, STORE_LOCKED, STORE_PATH_INVALID, TRASH_RESTORE_CONFLICT,
    TRASH_UNKNOWN, VERSION_EXISTS, WORKSPACE_READ_ONLY, YAML_UNSUPPORTED_EDIT,
};
use ww_schema::{EditError, Manifest, YamlEdit, YamlEditError, sha256_hex};

use super::failpoints::FailPoints;
use super::fsops;
use super::paths::{WsPath, writable};
use crate::state::{Pending, StateDb, StateError};
use crate::workspace::Access;

/// The first pause of a retry.
pub const RETRY_FIRST: Duration = Duration::from_millis(10);
/// Pauses double up to this.
pub const RETRY_CAP: Duration = Duration::from_millis(500);
/// Sharing violations are retried for this long in all, then the call is `store.busy`.
pub const RETRY_BUDGET: Duration = Duration::from_secs(10);
/// An edit that meets an outside change re-reads and re-applies at most this many times.
pub const EDIT_RETRIES: usize = 3;
/// A git-lfs pointer file is smaller than this (git-lfs spec).
pub const LFS_POINTER_MAX: usize = 1024;

/// What a full write expects to find at the target (write step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// No file.
    New,
    /// The file's last generated hash, or either hash of its pending row. With neither, a
    /// missing file is written as `New` and an existing one is a conflict.
    Generated,
    /// This SHA-256 (64 lowercase hex). An LFS pointer matches by its `oid`.
    Hash(String),
}

/// A finished write: the new SHA-256 and the identity the file has now (the replacement's
/// file id, not the old target's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: WsPath,
    pub sha256: String,
    pub identity: FileIdentity,
}

/// Why a store call failed; [`StoreError::to_error`] gives the Appendix D error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The target changed outside the suite, or (`cloud_only`) is a cloud-only placeholder,
    /// which is never read (ruling Q11). `backup` names the backup that holds the outside
    /// version when a put-back couldn't finish; the next `recover()` puts it back. `kept` names
    /// the visible `<name>.ww-conflict-<utc>` copies of outside versions the call set aside
    /// (a newer save the put-back met at the target, or the backup beside an outside file that
    /// took the freed name).
    HashConflict {
        path: WsPath,
        backup: Option<WsPath>,
        cloud_only: bool,
        kept: Vec<WsPath>,
    },
    /// Held by another program past the 10 s retry budget.
    Busy(WsPath),
    PathInvalid {
        path: String,
        why: String,
    },
    DiskFull(WsPath),
    Io {
        path: String,
        why: String,
    },
    Locked(WsPath),
    GitBusy,
    ReadOnly,
    NeedsRepair(WsPath),
    YamlUnsupported(String),
    VersionExists(String),
    /// A status change the status machine refuses (CORE-048), with why.
    InvalidTransition(String),
    /// No trash entry has this `entry_id`.
    TrashUnknown(String),
    /// A trash entry's original path is taken, so it was not restored.
    RestoreConflict(WsPath),
}

impl StoreError {
    /// A refused manifest edit (`Manifest::push` on a value that isn't a list, ruling Q5): the
    /// manifest needs repair and is not written.
    pub fn manifest_edit(rel: &WsPath, e: &EditError) -> StoreError {
        tracing::warn!(path = %rel, why = %e, "manifest edit refused");
        StoreError::NeedsRepair(rel.clone())
    }

    pub(super) fn io(path: &WsPath, e: &io::Error) -> StoreError {
        match classify(e) {
            RetryClass::DiskFull => StoreError::DiskFull(path.clone()),
            RetryClass::SharingViolation => StoreError::Busy(path.clone()),
            _ => StoreError::Io {
                path: path.as_str().to_string(),
                why: e.to_string(),
            },
        }
    }

    pub(super) fn state(path: &WsPath, e: &StateError) -> StoreError {
        StoreError::Io {
            path: path.as_str().to_string(),
            why: format!("the workspace state database failed: {e}"),
        }
    }

    fn conflict(path: &WsPath) -> StoreError {
        StoreError::HashConflict {
            path: path.clone(),
            backup: None,
            cloud_only: false,
            kept: Vec::new(),
        }
    }

    fn held_backup(path: &WsPath, backup: WsPath) -> StoreError {
        StoreError::HashConflict {
            path: path.clone(),
            backup: Some(backup),
            cloud_only: false,
            kept: Vec::new(),
        }
    }

    pub(super) fn cloud_only(path: &WsPath) -> StoreError {
        StoreError::HashConflict {
            path: path.clone(),
            backup: None,
            cloud_only: true,
            kept: Vec::new(),
        }
    }

    /// The Appendix D error: `store.hash_conflict` (with `data.backup`, the backup's
    /// workspace-relative path, when a put-back couldn't finish), `store.busy`,
    /// `store.path_invalid`, `store.disk_full`, `store.io`, `store.locked`, `store.git_busy`,
    /// `workspace.read_only`, `manifest.needs_repair`, `yaml.unsupported_edit`,
    /// `version.exists`, `status.invalid_transition`, `trash.unknown` (with `data.entry_id`)
    /// and `trash.restore_conflict` (with `data.path`, the occupied original path).
    pub fn to_error(&self, trace_id: &str) -> ErrorBody {
        let (spec, data) = match self {
            StoreError::HashConflict {
                path,
                backup,
                cloud_only,
                kept,
            } => {
                let mut data = json!({ "path": path.as_str(), "cloud_only": cloud_only });
                if let Some(backup) = backup {
                    data["backup"] = Value::from(backup.as_str());
                }
                if !kept.is_empty() {
                    data["kept"] = kept.iter().map(|k| Value::from(k.as_str())).collect();
                }
                (&STORE_HASH_CONFLICT, Some(data))
            }
            StoreError::Busy(path) => (&STORE_BUSY, Some(json!({ "path": path.as_str() }))),
            StoreError::PathInvalid { path, why } => (
                &STORE_PATH_INVALID,
                Some(json!({ "path": path, "why": why })),
            ),
            StoreError::DiskFull(path) => {
                (&STORE_DISK_FULL, Some(json!({ "path": path.as_str() })))
            }
            StoreError::Io { path, .. } => (&STORE_IO, Some(json!({ "path": path }))),
            StoreError::Locked(path) => (&STORE_LOCKED, Some(json!({ "path": path.as_str() }))),
            StoreError::GitBusy => (&STORE_GIT_BUSY, None),
            StoreError::ReadOnly => (&WORKSPACE_READ_ONLY, None),
            StoreError::NeedsRepair(path) => (
                &MANIFEST_NEEDS_REPAIR,
                Some(json!({ "path": path.as_str() })),
            ),
            StoreError::YamlUnsupported(_) => (&YAML_UNSUPPORTED_EDIT, None),
            StoreError::VersionExists(v) => (&VERSION_EXISTS, Some(json!({ "version": v }))),
            StoreError::InvalidTransition(_) => (&STATUS_INVALID_TRANSITION, None),
            StoreError::TrashUnknown(id) => (&TRASH_UNKNOWN, Some(json!({ "entry_id": id }))),
            StoreError::RestoreConflict(path) => (
                &TRASH_RESTORE_CONFLICT,
                Some(json!({ "path": path.as_str() })),
            ),
        };
        let mut body = ErrorBody::new(spec, self.to_string(), trace_id);
        body.data = data;
        body
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::HashConflict {
                cloud_only: true,
                path,
                ..
            } => write!(
                f,
                "`{path}` is a cloud-only file, which the suite never reads or overwrites: make \
                 it available offline first"
            ),
            StoreError::HashConflict {
                path, backup, kept, ..
            } => {
                write!(
                    f,
                    "`{path}` changed outside Worldwright, so it was not overwritten"
                )?;
                if !kept.is_empty() {
                    let names: Vec<String> = kept.iter().map(|k| format!("`{k}`")).collect();
                    write!(
                        f,
                        "; outside versions are kept beside it as {}",
                        names.join(" and ")
                    )?;
                }
                if let Some(backup) = backup {
                    write!(
                        f,
                        "; the outside version is kept in `{backup}` and is put back when the \
                         workspace is next opened"
                    )?;
                }
                Ok(())
            }
            StoreError::Busy(path) => write!(
                f,
                "`{path}` is held open by another program; close it and try again"
            ),
            StoreError::PathInvalid { path, why } => write!(f, "`{path}` can't be written: {why}"),
            StoreError::DiskFull(path) => write!(f, "the disk is full; `{path}` was not changed"),
            StoreError::Io { path, why } => write!(f, "`{path}`: {why}"),
            StoreError::Locked(path) => write!(f, "`{path}` is locked in its manifest"),
            StoreError::GitBusy => f.write_str(
                "git is still working on the workspace after 60 s; try again when it has finished",
            ),
            StoreError::ReadOnly => f.write_str(
                "another Worldwright core serves this workspace, so it is read-only here",
            ),
            StoreError::NeedsRepair(path) => write!(
                f,
                "`{path}` needs repair: it can't be edited as a manifest, so it was left as it is"
            ),
            StoreError::YamlUnsupported(why) => write!(f, "this YAML edit isn't supported: {why}"),
            StoreError::VersionExists(v) => write!(f, "version {v} already exists"),
            StoreError::InvalidTransition(why) => f.write_str(why),
            StoreError::TrashUnknown(id) => write!(
                f,
                "there is no trash entry `{id}`; trash.list lists the entries there are"
            ),
            StoreError::RestoreConflict(path) => write!(
                f,
                "`{path}` is taken, so the trash entry was not restored; move or rename what is \
                 there first, then restore it again"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

/// What a store call did to a path, for the index (CORE-010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Written,
    Created,
    Removed,
    MovedFrom(WsPath),
}

/// One change, with the hash and identity the store knows (the index records the identity the
/// store reports after each write and move).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreEvent {
    pub path: WsPath,
    pub kind: ChangeKind,
    pub sha256: Option<String>,
    pub identity: Option<FileIdentity>,
}

/// The index step (Task 10): the store emits, the index consumes (CORE-010).
pub trait ChangeSink: Send + Sync {
    fn applied<'a>(
        &'a self,
        events: &'a [StoreEvent],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// The `oid` of a git-lfs pointer (`version https://git-lfs.github.com/spec/v1`, then
/// `oid sha256:<64 hex>`): the SHA-256 of the real content, which an LFS-tracked file is
/// compared by (ARCH-078). `None` for anything else.
pub fn lfs_pointer_oid(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= LFS_POINTER_MAX {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "version https://git-lfs.github.com/spec/v1" {
        return None;
    }
    lines
        .filter_map(|line| line.strip_prefix("oid sha256:"))
        .find(|hex| hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .map(str::to_string)
}

/// What recovery did (Appendix E "Write order" recovery rules for temps, backups, pending
/// rows and staging).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub temps_removed: Vec<WsPath>,
    pub backups_restored: Vec<WsPath>,
    pub backups_removed: Vec<WsPath>,
    /// `<name>.ww-conflict-<utc>` copies made (the core's bytes set aside beside an outside
    /// edit, or a backup nothing else accounts for).
    pub conflicts_kept: Vec<WsPath>,
    pub pending_resolved: Vec<WsPath>,
    /// Empty staging handles of a dead core, removed.
    pub staging_removed: Vec<WsPath>,
    /// Non-empty staging handles and `seal-*` folders of a dead core: kept, and orphans.
    pub staging_kept: Vec<WsPath>,
}

// ---------------------------------------------------------------------------------------------
// Error steps (pure).
// ---------------------------------------------------------------------------------------------

/// Where in the write path a file call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Step 6's `ReplaceFileW` over an existing target.
    Replace,
    /// Step 6's `move_new` of the temp file onto a new target's name.
    MoveNew,
    /// The fallback's first rename, target → backup.
    FallbackFirst,
    /// A `move_new` onto a target name that was just freed: the fallback's second rename and
    /// the move after 1177.
    FreedName,
}

/// What the store does next about a failed call (ARCH-074 error handling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Pause (10 ms doubling to 500 ms, 10 s in all) and call again; past the budget,
    /// `store.busy`.
    Retry,
    /// 1177: the old target is under the backup name and the temp under its own; `move_new`
    /// moves the temp onto the freed name, then step 7 runs.
    MoveReplacement,
    /// `ReplaceFileW` is unsupported here: two `move_new` renames, target → backup, then
    /// temp → target.
    TwoRenames,
    /// An outside writer created the new target after step 3: the temp file is removed, the
    /// pending row cleared, `store.hash_conflict`.
    Conflict,
    /// An outside writer took the freed name in the gap: that file is kept, the backup is moved
    /// aside to `<name>.ww-conflict-<utc>`, the temp removed, `store.hash_conflict`.
    KeepOutsideFile,
    /// `store.disk_full` at once.
    DiskFull,
    /// `store.io`.
    Fail,
}

/// The step for an error of `class` in `phase`: 32/33/1224 and (for `ReplaceFileW`) 1175/1176
/// are retried; 1177 moves the replacement with `move_new`; 1/50 fall back to two renames;
/// 80/183 are an outside file (a conflict, or a kept file on a freed name); 112/39 are disk
/// full; anything else fails.
pub fn next_step(class: RetryClass, phase: Phase) -> Step {
    match (class, phase) {
        (RetryClass::SharingViolation, _) | (RetryClass::ReplaceRetry, Phase::Replace) => {
            Step::Retry
        }
        (RetryClass::ReplacementNotMoved, Phase::Replace) => Step::MoveReplacement,
        (RetryClass::Unsupported, Phase::Replace) => Step::TwoRenames,
        (RetryClass::TargetExists, Phase::MoveNew) => Step::Conflict,
        (RetryClass::TargetExists, Phase::FreedName) => Step::KeepOutsideFile,
        (RetryClass::DiskFull, _) => Step::DiskFull,
        _ => Step::Fail,
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------------------------

/// The retry pauses of one call: 10 ms doubling to 500 ms, 10 s in all, on tokio's clock.
pub(super) struct Backoff {
    start: tokio::time::Instant,
    next: Duration,
}

impl Backoff {
    pub(super) fn new() -> Backoff {
        Backoff {
            start: tokio::time::Instant::now(),
            next: RETRY_FIRST,
        }
    }

    /// Pauses before the next attempt; `false` once the budget is spent.
    pub(super) async fn wait(&mut self) -> bool {
        let spent = self.start.elapsed();
        if spent >= RETRY_BUDGET {
            return false;
        }
        tokio::time::sleep(self.next.min(RETRY_BUDGET - spent)).await;
        self.next = (self.next * 2).min(RETRY_CAP);
        true
    }
}

/// A file's SHA-256 and, for a small file, its LFS pointer `oid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileHash {
    pub(super) sha256: String,
    lfs_oid: Option<String>,
}

impl FileHash {
    /// Equal to `expected` by its SHA-256, or as an LFS pointer by its `oid`.
    fn matches(&self, expected: &str) -> bool {
        self.sha256 == expected || self.lfs_oid.as_deref() == Some(expected)
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Hashes an open file in 64 KiB pieces, keeping the start for the LFS check.
pub(super) fn hash_file(file: File) -> io::Result<FileHash> {
    hash_reader(file)
}

/// Deletes the backup `path` only if its SHA-256 is `sha256`, hashing and deleting it through
/// one handle that shares nothing (`fsops::remove_if`), so no writer slips into it between the
/// check and the delete. Returns whether it was deleted. Shared by the store's backup checks
/// and `suite.toml` writes.
pub(super) fn remove_if_hash(path: &Path, sha256: &str) -> io::Result<bool> {
    fsops::remove_if(path, &|file| Ok(hash_reader(file)?.sha256 == sha256))
}

/// Removes a temp file the caller wrote itself (`.<name>.ww-tmp-<pid>-<n>`, the core's own
/// bytes, never content anyone else has). Shared by `suite.toml` writes, so the hard deletes
/// stay where CORE-110 allows them.
pub(super) fn remove_temp(path: &Path) -> io::Result<()> {
    fsops::remove_file(path)
}

/// Moves `from` aside to a free `<dir>\<name>.ww-conflict-<stamp>` (a `-<k>` suffix when that
/// name is taken) with `move_new`, and returns the new path. Shared by the store and
/// `suite.toml` writes.
pub(super) fn set_aside(from: &Path, dir: &Path, name: &str, stamp: &str) -> io::Result<PathBuf> {
    for k in 1..=100 {
        let conflict = if k == 1 {
            format!("{name}.ww-conflict-{stamp}")
        } else {
            format!("{name}.ww-conflict-{stamp}-{k}")
        };
        let to = dir.join(conflict);
        match fsops::move_new(from, &to) {
            Ok(()) => return Ok(to),
            Err(e) if classify(&e) == RetryClass::TargetExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no free conflict name beside the target"))
}

/// [`hash_file`] over any reader (an exclusive handle's `&File`).
fn hash_reader(mut file: impl io::Read) -> io::Result<FileHash> {
    let mut hasher = Sha256::new();
    let mut head = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if head.len() < LFS_POINTER_MAX {
            let take = n.min(LFS_POINTER_MAX - head.len());
            head.extend_from_slice(&buf[..take]);
        }
        hasher.update(&buf[..n]);
    }
    Ok(FileHash {
        sha256: hex(&hasher.finalize()),
        lfs_oid: lfs_pointer_oid(&head),
    })
}

fn read_all(mut file: File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// What is at a path, found with the attribute-only `identity` before any data is read.
pub(super) enum Seen<T> {
    Missing,
    /// A cloud-only placeholder: never read.
    Placeholder,
    /// A symbolic link, junction or mount point (a name-surrogate reparse point): never read
    /// or written through.
    Link,
    Folder,
    Found(T),
}

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_ATTRIBUTE_READONLY: u32 = 0x1;

/// The `store.path_invalid` of a link at `rel`.
pub(super) fn link_refused(rel: &WsPath) -> StoreError {
    StoreError::PathInvalid {
        path: rel.as_str().to_string(),
        why: "it is a symbolic link or junction; the suite never reads or writes through a \
              link, so replace it with the file itself"
            .to_string(),
    }
}

/// `probe.identity` first; only a local file is opened, through `probe.open_read`.
fn see<T>(probe: &dyn Probe, path: &Path, read: fn(File) -> io::Result<T>) -> io::Result<Seen<T>> {
    let id = match probe.identity(path) {
        Ok(id) => id,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Seen::Missing),
        Err(e) => return Err(e),
    };
    if id.is_cloud_placeholder() {
        return Ok(Seen::Placeholder);
    }
    // A reparse point that names another file (a link) is never followed. Other reparse
    // points (a hydrated cloud file, a deduplicated file) hold their own data.
    if id.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && probe
            .reparse_tag(path)?
            .is_some_and(ww_platform::fs::is_name_surrogate)
    {
        return Ok(Seen::Link);
    }
    if id.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Ok(Seen::Folder);
    }
    match probe.open_read(path) {
        Ok(file) => read(file).map(Seen::Found),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Seen::Missing),
        Err(e) => Err(e),
    }
}

/// Whether a path exists, by its attributes (no handle, so a held file still counts).
pub(super) fn exists(path: &Path) -> bool {
    ww_platform::fs::attributes(path).is_ok()
}

/// Runs a blocking call on the blocking pool; a panic in it is an I/O error.
pub(super) async fn blocking<T, F>(f: F) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| {
        Err(io::Error::other(format!(
            "a blocking file call stopped: {e}"
        )))
    })
}

/// The pid of this process, and the counter that keeps `<pid>-<n>` names apart. It steps by 2,
/// so `n + 1` (the second backup) never collides with another write's `n`.
static NEXT_N: AtomicU64 = AtomicU64::new(1);

/// The names beside one target for one write.
#[derive(Debug, Clone)]
struct Names {
    dir: PathBuf,
    name: String,
    temp_name: String,
    temp: PathBuf,
    b1: PathBuf,
    b2: PathBuf,
    /// The SHA-256 of the bytes this write puts in (set after the pick).
    new_sha256: String,
}

/// The longest file name, in UTF-16 units, that Windows accepts.
const NAME_UNITS_MAX: usize = 255;
/// Room for `.` + `.ww-tmp-` + a pid + `-` + a counter.
const NAME_SUFFIX_UNITS: usize = 1 + 8 + 10 + 1 + 20;

impl Names {
    /// Fresh names in `dir` whose temp and both backups don't exist yet.
    fn pick(dir: &Path, name: &str) -> io::Result<Names> {
        let pid = std::process::id();
        for _ in 0..100 {
            let n = NEXT_N.fetch_add(2, Ordering::Relaxed);
            let temp_name = format!(".{name}.ww-tmp-{pid}-{n}");
            let names = Names {
                dir: dir.to_path_buf(),
                name: name.to_string(),
                temp: dir.join(&temp_name),
                temp_name,
                b1: dir.join(format!(".{name}.ww-bak-{pid}-{n}")),
                b2: dir.join(format!(".{name}.ww-bak-{pid}-{}", n + 1)),
                new_sha256: String::new(),
            };
            if !exists(&names.temp) && !exists(&names.b1) && !exists(&names.b2) {
                return Ok(names);
            }
        }
        Err(io::Error::other("no free temporary name beside the target"))
    }
}

/// Which leftover a dot-file beside a target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeftoverKind {
    Temp,
    Backup,
}

/// `.<target>.ww-tmp-<pid>-<n>` or `.<target>.ww-bak-<pid>-<n>` → (kind, target, pid, n).
fn parse_leftover(file_name: &str) -> Option<(LeftoverKind, String, u32, u64)> {
    let rest = file_name.strip_prefix('.')?;
    // The marker that comes last names the kind: a target may itself hold the other marker.
    let (i, marker, kind) = [
        (".ww-tmp-", LeftoverKind::Temp),
        (".ww-bak-", LeftoverKind::Backup),
    ]
    .into_iter()
    .filter_map(|(marker, kind)| rest.rfind(marker).map(|i| (i, marker, kind)))
    .max_by_key(|(i, _, _)| *i)?;
    let target = &rest[..i];
    let (pid, n) = rest[i + marker.len()..].split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if target.is_empty() || !digits(pid) || !digits(n) {
        return None;
    }
    Some((kind, target.to_string(), pid.parse().ok()?, n.parse().ok()?))
}

/// A temp or backup file found by the recovery scan.
#[derive(Debug, Clone)]
struct Leftover {
    path: PathBuf,
    kind: LeftoverKind,
    target: String,
    pid: u32,
    n: u64,
}

/// Every temp and backup file under `root`, skipping `.git`, the root's `.trash` and links.
/// A folder that can't be listed is logged and skipped.
fn scan_leftovers(root: &Path) -> io::Result<Vec<Leftover>> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(dir) = folders.pop() {
        let entries = match std::fs::read_dir(ww_platform::fs::long_path(&dir)) {
            Ok(entries) => entries,
            Err(e) if dir == root => return Err(e),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "recovery can't list a folder");
                continue;
            }
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = dir.join(&name);
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                let skip = name.eq_ignore_ascii_case(".git")
                    || (dir == root && name.eq_ignore_ascii_case(".trash"));
                if !skip {
                    folders.push(path);
                }
            } else if let Some((kind, target, pid, n)) = parse_leftover(&name) {
                found.push(Leftover {
                    path,
                    kind,
                    target,
                    pid,
                    n,
                });
            }
        }
    }
    found.sort_by(|a, b| (&a.path, a.pid, a.n).cmp(&(&b.path, b.pid, b.n)));
    Ok(found)
}

/// The backups beside `dir\name`, oldest `<pid>-<n>` first.
fn backups_of(dir: &Path, name: &str) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(ww_platform::fs::long_path(dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let wanted = super::paths::path_key(name);
    let mut found: Vec<(u32, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            match parse_leftover(&file_name) {
                Some((LeftoverKind::Backup, target, pid, n))
                    if super::paths::path_key(&target) == wanted =>
                {
                    Some((pid, n, dir.join(file_name)))
                }
                _ => None,
            }
        })
        .collect();
    found.sort();
    Ok(found.into_iter().map(|(_, _, p)| p).collect())
}

/// Whether process `pid` still runs. A process that can't be opened for another reason than
/// "not found" counts as running, so its temp files are kept.
fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    match ww_platform::process::open(pid) {
        Ok(process) => !matches!(process.exit_code(), Ok(Some(_))),
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}

pub(super) type PathLock = tokio::sync::Mutex<()>;

/// An edit: the current bytes (`None`: no file) → the new bytes and a value for the caller.
type EditFn<'a, T> = dyn Fn(Option<&[u8]>) -> Result<(Vec<u8>, T), StoreError> + Send + Sync + 'a;

pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a write's step 3 accepts at the target.
struct Want {
    hashes: Vec<String>,
    /// A missing file is acceptable.
    absent_ok: bool,
}

impl Want {
    fn accepts(&self, seen: Option<&FileHash>) -> bool {
        match seen {
            None => self.absent_ok,
            Some(h) => self.hashes.iter().any(|want| h.matches(want)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The store.
// ---------------------------------------------------------------------------------------------

/// The only file-write home of a workspace (ARCH-002, ARCH-112): every write goes through the
/// algorithm above.
pub struct Store {
    root: PathBuf,
    state: Arc<StateDb>,
    fp: Arc<FailPoints>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    probe: Arc<dyn Probe>,
    sink: Mutex<Option<Arc<dyn ChangeSink>>>,
    /// One async lock per path (case-folded), so writes to one file never interleave.
    paths: Mutex<HashMap<String, Weak<PathLock>>>,
    /// [`Access::ReadOnly`]: another core holds `core.lock`; nothing is written or resolved.
    access: Access,
    /// The `<machine>` of the append logs (ARCH-081).
    machine: String,
    /// The staging handles this store issued and hasn't released: never swept or listed.
    pub(super) live_handles: Mutex<HashSet<String>>,
    /// One async lock per asset (`<project>/<asset_id>`, case-folded), held from a sign-off's
    /// seal to the end of its Record stage ([`Store::version_lock`]).
    pub(super) version_locks: Mutex<HashMap<String, Weak<PathLock>>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// A read-write store over the workspace folder `root` and its `state.db`. `probe` gives
    /// the placeholder check of step 3 and the identities after each write. Its log machine is
    /// `unknown` until [`Store::with_machine`].
    pub fn new(
        root: PathBuf,
        state: Arc<StateDb>,
        failpoints: Arc<FailPoints>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        probe: Arc<dyn Probe>,
    ) -> Store {
        Store {
            root,
            state,
            fp: failpoints,
            clock,
            ids,
            probe,
            sink: Mutex::new(None),
            paths: Mutex::new(HashMap::new()),
            access: Access::ReadWrite,
            machine: "unknown".to_string(),
            live_handles: Mutex::new(HashSet::new()),
            version_locks: Mutex::new(HashMap::new()),
        }
    }

    /// This store with `access`: a [`Access::ReadOnly`] store refuses every write
    /// (`workspace.read_only`) and never resolves what a stopped write left (ARCH-028).
    #[must_use]
    pub fn with_access(mut self, access: Access) -> Store {
        self.access = access;
        self
    }

    /// This store with `machine` as the `<machine>` of its append logs (`Env::machine_id`).
    #[must_use]
    pub fn with_machine(mut self, machine: &str) -> Store {
        self.machine = machine.to_string();
        self
    }

    pub fn access(&self) -> Access {
        self.access
    }

    pub fn machine(&self) -> &str {
        &self.machine
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The workspace `state.db`.
    pub fn state(&self) -> &Arc<StateDb> {
        &self.state
    }

    /// The fail points (an engine-like writer of staging files passes them to
    /// `fsops::write_temp`).
    pub fn failpoints(&self) -> &Arc<FailPoints> {
        &self.fp
    }

    pub fn probe(&self) -> &Arc<dyn Probe> {
        &self.probe
    }

    /// `workspace.read_only` on a read-only store.
    pub(super) fn writable_store(&self) -> Result<(), StoreError> {
        match self.access {
            Access::ReadWrite => Ok(()),
            Access::ReadOnly => Err(StoreError::ReadOnly),
        }
    }

    pub fn ids(&self) -> &Arc<dyn IdSource> {
        &self.ids
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The index step that hears every change (Task 10).
    pub fn set_sink(&self, sink: Arc<dyn ChangeSink>) {
        *lock(&self.sink) = Some(sink);
    }

    pub(super) async fn emit(&self, events: Vec<StoreEvent>) {
        if events.is_empty() {
            return;
        }
        let sink = lock(&self.sink).clone();
        if let Some(sink) = sink {
            sink.applied(&events).await;
        }
    }

    /// The store clock's UTC time, in Unix seconds.
    pub(crate) fn now_utc(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    /// The workspace-relative form of a path under the root.
    pub(super) fn rel_of(&self, path: &Path) -> WsPath {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        WsPath::from_disk(parts.join("/"))
    }

    pub(super) async fn lock_path(&self, rel: &WsPath) -> tokio::sync::OwnedMutexGuard<()> {
        let held = {
            let mut paths = lock(&self.paths);
            paths.retain(|_, w| w.strong_count() > 0);
            let key = rel.key();
            match paths.get(&key).and_then(Weak::upgrade) {
                Some(held) => held,
                None => {
                    let held = Arc::new(PathLock::new(()));
                    paths.insert(key, Arc::downgrade(&held));
                    held
                }
            }
        };
        held.lock_owned().await
    }

    /// One blocking file call, after the fail point `point` (whose error stands for the
    /// call's).
    pub(super) async fn fs<T, F>(&self, point: Option<&'static str>, f: F) -> io::Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> io::Result<T> + Send + 'static,
    {
        let fp = self.fp.clone();
        blocking(move || {
            if let Some(point) = point {
                fp.hit(point)?;
            }
            f()
        })
        .await
    }

    /// A blocking file call retried on sharing violations under the budget. Past it the last
    /// error (a sharing violation) is returned.
    pub(super) async fn fs_retry<T, F>(&self, point: Option<&'static str>, f: F) -> io::Result<T>
    where
        T: Send + 'static,
        F: Fn() -> io::Result<T> + Send + Sync + 'static,
    {
        let f = Arc::new(f);
        let mut backoff = Backoff::new();
        loop {
            let call = f.clone();
            match self.fs(point, move || call()).await {
                Err(e) if classify(&e) == RetryClass::SharingViolation && backoff.wait().await => {}
                done => return done,
            }
        }
    }

    /// What is at `path`, hashed, with sharing violations retried.
    pub(super) async fn see_hash(&self, path: &Path) -> io::Result<Seen<FileHash>> {
        let (probe, path) = (self.probe.clone(), path.to_path_buf());
        self.fs_retry(None, move || see(probe.as_ref(), &path, hash_file))
            .await
    }

    /// The hash of a local file, `None` when it is missing, a folder or a placeholder.
    async fn hash_of(&self, path: &Path) -> io::Result<Option<FileHash>> {
        Ok(match self.see_hash(path).await? {
            Seen::Found(h) => Some(h),
            _ => None,
        })
    }

    async fn remove(&self, path: &Path) -> io::Result<()> {
        let path = path.to_path_buf();
        match self.fs_retry(None, move || fsops::remove_file(&path)).await {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            done => done,
        }
    }

    async fn path_exists(&self, path: &Path) -> bool {
        let path = path.to_path_buf();
        blocking(move || Ok(exists(&path))).await.unwrap_or(false)
    }

    /// Moves `from` aside to a free `<dir>\<name>.ww-conflict-<YYYYMMDDTHHMMSSZ>` (a `-<k>`
    /// suffix when that name is taken) with `move_new`.
    async fn move_aside(&self, from: &Path, dir: &Path, name: &str) -> io::Result<WsPath> {
        let stamp = ww_schema::time::compact(self.now_utc());
        let (from, dir, name) = (from.to_path_buf(), dir.to_path_buf(), name.to_string());
        let to = self
            .fs_retry(None, move || set_aside(&from, &dir, &name, &stamp))
            .await?;
        Ok(self.rel_of(&to))
    }

    /// Ends a failed write before the target changed: the temp file is removed and the pending
    /// row cleared.
    async fn abandon(&self, rel: &WsPath, names: &Names) {
        if let Err(e) = self.remove(&names.temp).await {
            tracing::warn!(path = %rel, error = %e, "a temp file could not be removed");
        }
        if let Err(e) = self.state.finish_pending(rel.as_str(), None).await {
            tracing::warn!(path = %rel, error = %e, "a pending row could not be cleared");
        }
    }

    /// Ends a failed write in the middle of a swap: the temp file (the core's own bytes) is
    /// removed, and the pending row and any backup stay for recovery (the next read of the
    /// path, or `recover()`). Returns the call's error, which names the backup when one holds
    /// the earlier content.
    async fn stop(&self, rel: &WsPath, names: &Names, error: StoreError) -> StoreError {
        if let Err(e) = self.remove(&names.temp).await {
            tracing::warn!(path = %rel, error = %e, "a temp file could not be removed");
        }
        tracing::warn!(path = %rel, "a write stopped mid-swap; recovery resolves it");
        if !self.path_exists(&names.b1).await {
            return error;
        }
        let why = match &error {
            StoreError::Io { why, .. } => why.clone(),
            other => other.to_string(),
        };
        StoreError::Io {
            path: rel.as_str().to_string(),
            why: format!(
                "{why}; the earlier content is kept in `{}` until recovery puts it back",
                self.rel_of(&names.b1)
            ),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Reads.
    // -----------------------------------------------------------------------------------------

    /// The bytes of `rel`, `None` when there is no file. A cloud-only placeholder is never
    /// read: `store.io`, saying to make it available offline first.
    pub async fn read(&self, rel: &WsPath) -> Result<Option<Vec<u8>>, StoreError> {
        match self.see_data(rel).await? {
            Seen::Missing => Ok(None),
            Seen::Found(bytes) => Ok(Some(bytes)),
            Seen::Placeholder => Err(StoreError::Io {
                path: rel.as_str().to_string(),
                why: "it is a cloud-only file, which the suite never downloads: make it \
                      available offline first"
                    .to_string(),
            }),
            Seen::Link => Err(link_refused(rel)),
            Seen::Folder => Err(StoreError::Io {
                path: rel.as_str().to_string(),
                why: "it is a folder".to_string(),
            }),
        }
    }

    /// The bytes an edit starts from: `None` for no file; a placeholder is a conflict.
    async fn read_for_edit(&self, rel: &WsPath) -> Result<Option<Vec<u8>>, StoreError> {
        match self.see_data(rel).await? {
            Seen::Missing => Ok(None),
            Seen::Found(bytes) => Ok(Some(bytes)),
            Seen::Placeholder => Err(StoreError::cloud_only(rel)),
            Seen::Link => Err(link_refused(rel)),
            Seen::Folder => Err(StoreError::PathInvalid {
                path: rel.as_str().to_string(),
                why: "it is a folder".to_string(),
            }),
        }
    }

    /// What is at `rel`, with its data (ruling Q12): a missing target with a pending row is
    /// a swap that stopped part-way, so the row is resolved under the path lock and the path
    /// read again. It is never "not found" while a backup holds the content: if the row can't
    /// be resolved the read fails with `store.io` naming the backup.
    async fn see_data(&self, rel: &WsPath) -> Result<Seen<Vec<u8>>, StoreError> {
        let seen = self.see_bytes(rel).await?;
        if !matches!(seen, Seen::Missing) {
            return Ok(seen);
        }
        let pending = self
            .state
            .pending(rel.as_str())
            .await
            .map_err(|e| StoreError::state(rel, &e))?;
        if pending.is_none() {
            return Ok(seen);
        }
        if self.access == Access::ReadOnly {
            // Only the core that holds `core.lock` moves files or clears rows: this one names
            // the backup that holds the content.
            return Err(self
                .held_in_backup(
                    rel,
                    "another Worldwright core serves this workspace, and it puts it back",
                )
                .await);
        }
        let _held = self.lock_path(rel).await;
        let pending = self
            .state
            .pending(rel.as_str())
            .await
            .map_err(|e| StoreError::state(rel, &e))?;
        if let Some(row) = pending
            && let Err(e) = self.resolve_row(&row, &mut RecoveryReport::default()).await
        {
            return Err(self.held_in_backup(rel, &e.to_string()).await);
        }
        self.see_bytes(rel).await
    }

    /// `store.io` for a missing `rel` whose content a stopped write left in a backup, naming
    /// the backup, with `why` it wasn't put back.
    async fn held_in_backup(&self, rel: &WsPath, why: &str) -> StoreError {
        let abs = rel.abs(&self.root);
        let dir = abs.parent().unwrap_or(&self.root).to_path_buf();
        let name = rel.file_name().to_string();
        let backups = blocking(move || backups_of(&dir, &name))
            .await
            .unwrap_or_default();
        let named = backups.first().map_or_else(
            || "a backup beside it".to_string(),
            |b| format!("`{}`", self.rel_of(b)),
        );
        StoreError::Io {
            path: rel.as_str().to_string(),
            why: format!(
                "a write stopped part-way and its content is in {named} until recovery puts it \
                 back: {why}"
            ),
        }
    }

    async fn see_bytes(&self, rel: &WsPath) -> Result<Seen<Vec<u8>>, StoreError> {
        let (probe, path) = (self.probe.clone(), rel.abs(&self.root));
        self.fs_retry(None, move || see(probe.as_ref(), &path, read_all))
            .await
            .map_err(|e| StoreError::io(rel, &e))
    }

    /// Whether `sha256` is the core's own for `rel`: its generated hash, or either hash of its
    /// pending row (so the watcher never records the core's own write as a human edit).
    pub async fn accepts_as_own(&self, rel: &WsPath, sha256: &str) -> Result<bool, StoreError> {
        let path = rel.as_str();
        let generated = self
            .state
            .generated(path)
            .await
            .map_err(|e| StoreError::state(rel, &e))?;
        if generated.as_deref() == Some(sha256) {
            return Ok(true);
        }
        let pending = self
            .state
            .pending(path)
            .await
            .map_err(|e| StoreError::state(rel, &e))?;
        Ok(pending
            .is_some_and(|p| p.new_sha256 == sha256 || p.old_sha256.as_deref() == Some(sha256)))
    }

    // -----------------------------------------------------------------------------------------
    // Writes.
    // -----------------------------------------------------------------------------------------

    /// A full write of `bytes` to `rel`, checked against `expect` (Appendix E steps 1–8).
    pub async fn write(
        &self,
        rel: &WsPath,
        bytes: Vec<u8>,
        expect: Expect,
    ) -> Result<Written, StoreError> {
        let (written, kind) = self.write_quiet(rel, bytes, expect).await?;
        self.emit(vec![event(&written, kind)]).await;
        Ok(written)
    }

    /// [`Store::edit_manifest`]: reads the manifest (an empty one when there is no file), runs
    /// `f` on its ordered map, and writes it with `Expect::Hash` of what was read. An outside
    /// change re-reads and re-applies, at most 3 times. A manifest that isn't a JSON object,
    /// or isn't UTF-8, is `manifest.needs_repair` and is never written.
    pub async fn edit_manifest(
        &self,
        rel: &WsPath,
        f: &(dyn Fn(&mut Manifest) -> Result<(), StoreError> + Send + Sync),
    ) -> Result<(Manifest, Written), StoreError> {
        let (manifest, written, kind) = self.edit_manifest_quiet(rel, f).await?;
        if let Some(kind) = kind {
            self.emit(vec![event(&written, kind)]).await;
        }
        Ok((manifest, written))
    }

    /// Targeted YAML edits that keep comments (`ww_schema::yaml_edit`), hash-checked like a
    /// manifest edit. A form the editor can't change is `yaml.unsupported_edit`.
    pub async fn edit_yaml(&self, rel: &WsPath, edits: &[YamlEdit]) -> Result<Written, StoreError> {
        let (written, kind) = self.edit_yaml_quiet(rel, edits).await?;
        if let Some(kind) = kind {
            self.emit(vec![event(&written, kind)]).await;
        }
        Ok(written)
    }

    /// A text edit (`progress.md`), hash-checked like a manifest edit. No file is `""`.
    pub async fn edit_text(
        &self,
        rel: &WsPath,
        f: &(dyn Fn(&str) -> Result<String, StoreError> + Send + Sync),
    ) -> Result<Written, StoreError> {
        let (written, kind) = self.edit_text_quiet(rel, f).await?;
        if let Some(kind) = kind {
            self.emit(vec![event(&written, kind)]).await;
        }
        Ok(written)
    }

    /// [`Store::edit_manifest`] without telling the sink (for an operation that reports its
    /// changes itself, in the ARCH-075 order). `None` kind: nothing changed, nothing written.
    pub(crate) async fn edit_manifest_quiet(
        &self,
        rel: &WsPath,
        f: &(dyn Fn(&mut Manifest) -> Result<(), StoreError> + Send + Sync),
    ) -> Result<(Manifest, Written, Option<ChangeKind>), StoreError> {
        self.edit_with(rel, &|current| {
            let mut manifest = match current {
                None => Manifest::parse("{}").expect("an empty object parses").0,
                Some(bytes) => {
                    let text = std::str::from_utf8(bytes)
                        .map_err(|_| StoreError::NeedsRepair(rel.clone()))?;
                    match Manifest::parse(text) {
                        Ok((manifest, _warnings)) => manifest,
                        Err(e) => {
                            tracing::warn!(path = %rel, error = %e, "manifest doesn't parse");
                            return Err(StoreError::NeedsRepair(rel.clone()));
                        }
                    }
                }
            };
            f(&mut manifest)?;
            Ok((manifest.to_text().into_bytes(), manifest))
        })
        .await
    }

    /// [`Store::edit_yaml`] without telling the sink.
    pub(crate) async fn edit_yaml_quiet(
        &self,
        rel: &WsPath,
        edits: &[YamlEdit],
    ) -> Result<(Written, Option<ChangeKind>), StoreError> {
        let (_, written, kind) = self
            .edit_with(rel, &|current| {
                let text = match current {
                    None => "",
                    Some(bytes) => std::str::from_utf8(bytes).map_err(|_| {
                        StoreError::YamlUnsupported("the file isn't UTF-8 text".to_string())
                    })?,
                };
                let edited = ww_schema::yaml_edit::apply(text, edits).map_err(|e| match e {
                    YamlEditError::Unsupported(m) => StoreError::YamlUnsupported(m),
                    YamlEditError::Invalid(m) => {
                        StoreError::YamlUnsupported(format!("the file isn't valid YAML: {m}"))
                    }
                })?;
                Ok((edited.into_bytes(), ()))
            })
            .await?;
        Ok((written, kind))
    }

    /// [`Store::edit_text`] without telling the sink.
    pub(crate) async fn edit_text_quiet(
        &self,
        rel: &WsPath,
        f: &(dyn Fn(&str) -> Result<String, StoreError> + Send + Sync),
    ) -> Result<(Written, Option<ChangeKind>), StoreError> {
        let (_, written, kind) = self
            .edit_with(rel, &|current| {
                let text = match current {
                    None => "",
                    Some(bytes) => std::str::from_utf8(bytes).map_err(|_| StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: "the file isn't UTF-8 text".to_string(),
                    })?,
                };
                Ok((f(text)?.into_bytes(), ()))
            })
            .await?;
        Ok((written, kind))
    }

    /// Read → `apply` → write with `Expect::Hash` of what was read (`New` for no file). An
    /// outside change between the read and the write re-reads and re-applies, at most
    /// [`EDIT_RETRIES`] times. Unchanged bytes are not written.
    async fn edit_with<T: Send>(
        &self,
        rel: &WsPath,
        apply: &EditFn<'_, T>,
    ) -> Result<(T, Written, Option<ChangeKind>), StoreError> {
        self.writable_store()?;
        writable(rel)?;
        self.check_locks(rel).await?;
        let mut attempt = 0;
        loop {
            let current = self.read_for_edit(rel).await?;
            let (bytes, value) = apply(current.as_deref())?;
            if current.as_deref() == Some(bytes.as_slice()) {
                let identity = self.identity(rel).await?;
                let written = Written {
                    path: rel.clone(),
                    sha256: sha256_hex(&bytes),
                    identity,
                };
                return Ok((value, written, None));
            }
            let expect = match &current {
                None => Expect::New,
                Some(read) => Expect::Hash(sha256_hex(read)),
            };
            match self.write_quiet(rel, bytes, expect).await {
                Ok((written, kind)) => return Ok((value, written, Some(kind))),
                Err(StoreError::HashConflict {
                    backup: None,
                    cloud_only: false,
                    kept,
                    ..
                }) if kept.is_empty() && attempt < EDIT_RETRIES => {
                    attempt += 1;
                    tracing::info!(path = %rel, attempt, "outside change during an edit; re-applying");
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub(super) async fn identity(&self, rel: &WsPath) -> Result<FileIdentity, StoreError> {
        let (probe, path) = (self.probe.clone(), rel.abs(&self.root));
        self.fs_retry(None, move || probe.identity(&path))
            .await
            .map_err(|e| StoreError::io(rel, &e))
    }

    /// Step 1's check against the long form of the path: the deepest existing part of it goes
    /// through `probe.long_name`, so an 8.3 short name (`PROJEC~1`, or one set with
    /// `SetFileShortName`) never reaches a sealed folder or the ledger. A lookup that fails, or
    /// a long form that isn't under the workspace root, refuses the write (fail closed).
    pub(super) async fn writable_on_disk(&self, rel: &WsPath) -> Result<(), StoreError> {
        let refuse = |why: String| StoreError::PathInvalid {
            path: rel.as_str().to_string(),
            why,
        };
        let (root, abs, probe) = (self.root.clone(), rel.abs(&self.root), self.probe.clone());
        let long = blocking(move || {
            let existing = abs
                .ancestors()
                .take_while(|p| p.starts_with(&root))
                .find(|p| exists(p))
                .map(Path::to_path_buf)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "the workspace is missing")
                })?;
            let tail = abs
                .strip_prefix(&existing)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let long_root = probe.long_name(&root)?;
            let long_existing = probe.long_name(&existing)?;
            let (lr, le) = (
                long_root
                    .to_string_lossy()
                    .trim_end_matches('\\')
                    .to_string(),
                long_existing.to_string_lossy().into_owned(),
            );
            let inside_root = le.len() >= lr.len()
                && le.is_char_boundary(lr.len())
                && le[..lr.len()].to_lowercase() == lr.to_lowercase()
                && (le.len() == lr.len() || le[lr.len()..].starts_with('\\'));
            if !inside_root {
                return Ok(None);
            }
            let inside = PathBuf::from(&le[lr.len()..]).join(tail);
            let parts: Vec<String> = inside
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect();
            Ok(Some(parts.join("/")))
        })
        .await;
        match long {
            Ok(Some(long)) => writable(&WsPath::from_disk(long)),
            Ok(None) => Err(refuse(
                "its long form is not under the workspace folder".to_string(),
            )),
            Err(e) => Err(refuse(format!("its long form can't be read: {e}"))),
        }
    }

    /// CORE-035: `store.locked` for a path under an asset folder,
    /// `projects/<p>/assets/<a>/<rest>`, whose manifest lists `<rest>` (or a folder above it)
    /// in `locks`. Entries are relative to the asset folder (I5) and compare without regard to
    /// case. The manifest itself is never locked by its own list, an asset without a manifest
    /// locks nothing, and one whose manifest doesn't parse is logged and locks nothing; a
    /// manifest that can't be read refuses the write with that read's error (fail closed).
    pub(super) async fn check_locks(&self, rel: &WsPath) -> Result<(), StoreError> {
        let parts: Vec<&str> = rel.as_str().split('/').collect();
        if parts.len() < 5
            || !parts[0].eq_ignore_ascii_case("projects")
            || !parts[2].eq_ignore_ascii_case("assets")
        {
            return Ok(());
        }
        let asset_id = parts[3];
        let inside = super::paths::path_key(&parts[4..].join("/"));
        if inside == super::paths::path_key(&format!("{asset_id}.manifest.json")) {
            return Ok(());
        }
        let manifest = WsPath::from_disk(format!(
            "{}/{}/{}/{asset_id}/{asset_id}.manifest.json",
            parts[0], parts[1], parts[2]
        ));
        let Some(bytes) = self.read(&manifest).await? else {
            return Ok(());
        };
        let Some((m, _)) = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| Manifest::parse(t).ok())
        else {
            tracing::warn!(manifest = %manifest, "a manifest that doesn't parse; its locks aren't known");
            return Ok(());
        };
        let locked = m.view().locks.iter().any(|entry| {
            let entry = super::paths::path_key(entry.trim_end_matches('/'));
            !entry.is_empty()
                && (inside == entry
                    || (inside.starts_with(&entry) && inside[entry.len()..].starts_with('/')))
        });
        if locked {
            return Err(StoreError::Locked(rel.clone()));
        }
        Ok(())
    }

    /// What step 3 accepts for `expect`.
    async fn want(&self, rel: &WsPath, expect: &Expect) -> Result<Want, StoreError> {
        Ok(match expect {
            Expect::New => Want {
                hashes: Vec::new(),
                absent_ok: true,
            },
            Expect::Hash(h) => Want {
                hashes: vec![h.clone()],
                absent_ok: false,
            },
            Expect::Generated => {
                let path = rel.as_str();
                let generated = self
                    .state
                    .generated(path)
                    .await
                    .map_err(|e| StoreError::state(rel, &e))?;
                let pending = self
                    .state
                    .pending(path)
                    .await
                    .map_err(|e| StoreError::state(rel, &e))?;
                if generated.is_none() && pending.is_none() {
                    Want {
                        hashes: Vec::new(),
                        absent_ok: true,
                    }
                } else {
                    let mut hashes: Vec<String> = generated.into_iter().collect();
                    let mut absent_ok = false;
                    if let Some(p) = pending {
                        hashes.push(p.new_sha256);
                        match p.old_sha256 {
                            Some(old) => hashes.push(old),
                            None => absent_ok = true,
                        }
                    }
                    Want { hashes, absent_ok }
                }
            }
        })
    }

    /// Steps 1–8 without telling the sink; returns what the write was to the index.
    pub(crate) async fn write_quiet(
        &self,
        rel: &WsPath,
        bytes: Vec<u8>,
        expect: Expect,
    ) -> Result<(Written, ChangeKind), StoreError> {
        self.writable_store()?;
        // Step 1.
        writable(rel)?;
        self.check_locks(rel).await?;
        self.writable_on_disk(rel).await?;
        let name = rel.file_name().to_string();
        if name.encode_utf16().count() + NAME_SUFFIX_UNITS > NAME_UNITS_MAX {
            return Err(StoreError::PathInvalid {
                path: rel.as_str().to_string(),
                why: "the file name is too long for the store's temporary names".to_string(),
            });
        }
        let _held = self.lock_path(rel).await;
        // A replace that a failed call left pending is resolved first, so its backup is never
        // left behind without its row.
        let pending = self
            .state
            .pending(rel.as_str())
            .await
            .map_err(|e| StoreError::state(rel, &e))?;
        if let Some(row) = pending {
            self.resolve_row(&row, &mut RecoveryReport::default())
                .await?;
        }
        // Step 2, the git gate (ARCH-083), comes with the watcher.

        // Step 3.
        let abs = rel.abs(&self.root);
        let want = self.want(rel, &expect).await?;
        let seen = match self.see_hash(&abs).await {
            Ok(Seen::Missing) => None,
            Ok(Seen::Found(h)) => Some(h),
            Ok(Seen::Placeholder) => return Err(StoreError::cloud_only(rel)),
            Ok(Seen::Link) => return Err(link_refused(rel)),
            Ok(Seen::Folder) => {
                return Err(StoreError::PathInvalid {
                    path: rel.as_str().to_string(),
                    why: "it is a folder".to_string(),
                });
            }
            Err(e) => return Err(StoreError::io(rel, &e)),
        };
        if !want.accepts(seen.as_ref()) {
            return Err(StoreError::conflict(rel));
        }
        if seen.is_some() && self.identity(rel).await?.attributes & FILE_ATTRIBUTE_READONLY != 0 {
            // ReplaceFileW refuses a read-only target and the two-rename fallback would put a
            // writable file in its place, so neither path writes it.
            return Err(StoreError::Io {
                path: rel.as_str().to_string(),
                why: "it is read-only, so the suite doesn't write it; clear its read-only \
                      attribute first"
                    .to_string(),
            });
        }
        let new_sha = sha256_hex(&bytes);
        if seen.as_ref().is_some_and(|h| h.sha256 == new_sha) {
            // The file already holds these bytes: nothing to swap, only the hash to record.
            self.state
                .set_generated(rel.as_str(), &new_sha, self.now_utc())
                .await
                .map_err(|e| StoreError::state(rel, &e))?;
            let identity = self.identity(rel).await?;
            return Ok((
                Written {
                    path: rel.clone(),
                    sha256: new_sha,
                    identity,
                },
                ChangeKind::Written,
            ));
        }

        // Step 4.
        let dir = abs
            .parent()
            .expect("a workspace path has a folder")
            .to_path_buf();
        let mut names = {
            let (dir, name) = (dir.clone(), name.clone());
            blocking(move || {
                fsops::ensure_dir(&dir)?;
                Names::pick(&dir, &name)
            })
            .await
            .map_err(|e| StoreError::io(rel, &e))?
        };
        names.new_sha256 = new_sha.clone();
        {
            let (dir, temp_name, fp) =
                (names.dir.clone(), names.temp_name.clone(), self.fp.clone());
            blocking(move || fsops::write_temp(&dir, &temp_name, &bytes, &fp))
                .await
                .map_err(|e| StoreError::io(rel, &e))?;
        }

        // Step 5.
        let now = self.now_utc();
        let row = Pending {
            path: rel.as_str().to_string(),
            old_sha256: seen.as_ref().map(|h| h.sha256.clone()),
            new_sha256: new_sha.clone(),
            started_utc: now,
        };
        if let Err(e) = self.state.set_pending(&row).await {
            let _ = self.remove(&names.temp).await;
            return Err(StoreError::state(rel, &e));
        }

        // Steps 6 and 7.
        let (kind, clear_row) = match &seen {
            None => {
                self.swap_new(rel, &abs, &names).await?;
                (ChangeKind::Created, true)
            }
            Some(old) => (
                ChangeKind::Written,
                self.swap_existing(rel, &abs, &names, old).await?,
            ),
        };

        // Step 8.
        let finished = if clear_row {
            self.state
                .finish_pending(rel.as_str(), Some((&new_sha, now)))
                .await
        } else {
            self.state.set_generated(rel.as_str(), &new_sha, now).await
        };
        finished.map_err(|e| StoreError::state(rel, &e))?;
        let identity = self.identity(rel).await?;
        Ok((
            Written {
                path: rel.clone(),
                sha256: new_sha,
                identity,
            },
            kind,
        ))
    }

    /// Step 6 for a new target: `move_new` of the temp file, never over a file that appeared.
    async fn swap_new(&self, rel: &WsPath, abs: &Path, names: &Names) -> Result<(), StoreError> {
        let mut backoff = Backoff::new();
        loop {
            let (temp, target) = (names.temp.clone(), abs.to_path_buf());
            let Err(e) = self
                .fs(Some("store.replace"), move || {
                    fsops::move_new(&temp, &target)
                })
                .await
            else {
                break;
            };
            let step = next_step(classify(&e), Phase::MoveNew);
            if step == Step::Retry && backoff.wait().await {
                continue;
            }
            self.abandon(rel, names).await;
            return Err(match step {
                Step::Retry => StoreError::Busy(rel.clone()),
                Step::Conflict => StoreError::conflict(rel),
                _ => StoreError::io(rel, &e),
            });
        }
        if let Err(e) = self.fs(Some("store.after_replace"), || Ok(())).await {
            return Err(self.stop(rel, names, StoreError::io(rel, &e)).await);
        }
        Ok(())
    }

    /// Steps 6 and 7 for an existing target. Returns whether the pending row can be cleared
    /// (false when the old backup could not be removed: recovery removes it later).
    async fn swap_existing(
        &self,
        rel: &WsPath,
        abs: &Path,
        names: &Names,
        old: &FileHash,
    ) -> Result<bool, StoreError> {
        let mut fallback = false;
        let mut backoff = Backoff::new();
        loop {
            let (target, temp, b1) = (abs.to_path_buf(), names.temp.clone(), names.b1.clone());
            let Err(e) = self
                .fs(Some("store.replace"), move || {
                    fsops::replace(&target, &temp, &b1)
                })
                .await
            else {
                break;
            };
            if e.kind() == io::ErrorKind::NotFound && !self.path_exists(abs).await {
                // The target was deleted outside after step 3.
                self.abandon(rel, names).await;
                return Err(StoreError::conflict(rel));
            }
            let step = next_step(classify(&e), Phase::Replace);
            if step == Step::Retry && backoff.wait().await {
                continue;
            }
            match step {
                Step::MoveReplacement => {
                    self.move_onto_freed(rel, abs, names, None).await?;
                    break;
                }
                Step::TwoRenames => {
                    fallback = true;
                    self.two_renames(rel, abs, names).await?;
                    break;
                }
                Step::Retry => {
                    self.abandon(rel, names).await;
                    return Err(StoreError::Busy(rel.clone()));
                }
                Step::DiskFull => {
                    self.abandon(rel, names).await;
                    return Err(StoreError::DiskFull(rel.clone()));
                }
                _ => {
                    let untouched = self.path_exists(&names.temp).await
                        && self.path_exists(abs).await
                        && !self.path_exists(&names.b1).await;
                    if untouched {
                        self.abandon(rel, names).await;
                        return Err(StoreError::io(rel, &e));
                    }
                    return Err(self.stop(rel, names, StoreError::io(rel, &e)).await);
                }
            }
        }
        if let Err(e) = self.fs(Some("store.after_replace"), || Ok(())).await {
            return Err(self.stop(rel, names, StoreError::io(rel, &e)).await);
        }
        self.check_backup(rel, abs, names, old, fallback).await
    }

    /// The fallback where `ReplaceFileW` is unsupported: target → backup, then (after the
    /// fail point `store.between_renames`) temp → target, both with `move_new`.
    async fn two_renames(&self, rel: &WsPath, abs: &Path, names: &Names) -> Result<(), StoreError> {
        let mut backoff = Backoff::new();
        loop {
            let (target, b1) = (abs.to_path_buf(), names.b1.clone());
            let Err(e) = self.fs(None, move || fsops::move_new(&target, &b1)).await else {
                break;
            };
            if e.kind() == io::ErrorKind::NotFound && !self.path_exists(abs).await {
                self.abandon(rel, names).await;
                return Err(StoreError::conflict(rel));
            }
            let step = next_step(classify(&e), Phase::FallbackFirst);
            if step == Step::Retry && backoff.wait().await {
                continue;
            }
            self.abandon(rel, names).await;
            return Err(match step {
                Step::Retry => StoreError::Busy(rel.clone()),
                Step::DiskFull => StoreError::DiskFull(rel.clone()),
                _ => StoreError::io(rel, &e),
            });
        }
        self.move_onto_freed(rel, abs, names, Some("store.between_renames"))
            .await
    }

    /// `move_new` of the temp file onto the target name that was just freed (the fallback's
    /// second rename, or the move after 1177). A file that an outside writer put there in the
    /// gap is kept: the backup is moved aside, the temp removed, `store.hash_conflict`.
    async fn move_onto_freed(
        &self,
        rel: &WsPath,
        abs: &Path,
        names: &Names,
        point: Option<&'static str>,
    ) -> Result<(), StoreError> {
        let mut backoff = Backoff::new();
        loop {
            let (temp, target) = (names.temp.clone(), abs.to_path_buf());
            let Err(e) = self
                .fs(point, move || fsops::move_new(&temp, &target))
                .await
            else {
                return Ok(());
            };
            let step = next_step(classify(&e), Phase::FreedName);
            if step == Step::Retry && backoff.wait().await {
                continue;
            }
            if step == Step::KeepOutsideFile {
                let _ = self.remove(&names.temp).await;
                return Err(
                    match self.move_aside(&names.b1, &names.dir, &names.name).await {
                        Ok(kept) => {
                            tracing::warn!(path = %rel, kept = %kept, "an outside file took the target name; the old content was set aside");
                            if let Err(e) = self.state.finish_pending(rel.as_str(), None).await {
                                tracing::warn!(path = %rel, error = %e, "a pending row could not be cleared");
                            }
                            StoreError::HashConflict {
                                path: rel.clone(),
                                backup: None,
                                cloud_only: false,
                                kept: vec![kept],
                            }
                        }
                        Err(_) => StoreError::held_backup(rel, self.rel_of(&names.b1)),
                    },
                );
            }
            let error = match step {
                Step::Retry => StoreError::Busy(rel.clone()),
                Step::DiskFull => StoreError::DiskFull(rel.clone()),
                _ => StoreError::io(rel, &e),
            };
            return Err(self.stop(rel, names, error).await);
        }
    }

    /// Step 7: the backup holds what the target was just before the swap. Equal to what step 3
    /// checked → removed. Anything else is an outside edit that landed between steps 3 and 6,
    /// and it is put back.
    async fn check_backup(
        &self,
        rel: &WsPath,
        abs: &Path,
        names: &Names,
        old: &FileHash,
        fallback: bool,
    ) -> Result<bool, StoreError> {
        // Hashed and removed through one handle that shares nothing, so no writer can slip into
        // the backup between the check and the delete.
        let (b1, old_sha) = (names.b1.clone(), old.sha256.clone());
        let removed = self
            .fs_retry(None, move || remove_if_hash(&b1, &old_sha))
            .await;
        match removed {
            Ok(true) => Ok(true),
            Ok(false) => Err(self.put_back(rel, abs, names, fallback).await),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::warn!(path = %rel, "the backup of a replace vanished before it was checked");
                Ok(true)
            }
            Err(e) => {
                // Unchecked: the backup may hold an outside edit. It stays, with the row, and
                // the error names it.
                tracing::warn!(path = %rel, error = %e, "the backup could not be checked; recovery checks it");
                Err(StoreError::held_backup(rel, self.rel_of(&names.b1)))
            }
        }
    }

    /// Puts the outside version (in the first backup) back under the target's name and
    /// discards the core's new bytes; returns the call's `store.hash_conflict`. A put-back
    /// that can't finish keeps the pending row and names the backup in `data.backup`.
    async fn put_back(
        &self,
        rel: &WsPath,
        abs: &Path,
        names: &Names,
        fallback: bool,
    ) -> StoreError {
        let held = || StoreError::held_backup(rel, self.rel_of(&names.b1));
        tracing::warn!(path = %rel, "an outside edit landed during a write; putting it back");
        if !fallback {
            let mut backoff = Backoff::new();
            loop {
                let (target, b1, b2) = (abs.to_path_buf(), names.b1.clone(), names.b2.clone());
                let Err(e) = self
                    .fs(None, move || fsops::replace(&target, &b1, &b2))
                    .await
                else {
                    // The outside version is at the target, the core's bytes in the second
                    // backup.
                    return self
                        .finish_put_back(rel, names, Some("store.put_back"))
                        .await;
                };
                let class = classify(&e);
                if matches!(
                    class,
                    RetryClass::SharingViolation | RetryClass::ReplaceRetry
                ) {
                    if backoff.wait().await {
                        continue;
                    }
                    return held();
                }
                match class {
                    // The core's bytes went to the second backup; the target name is free.
                    RetryClass::ReplacementNotMoved => {
                        return self
                            .restore_backup(rel, abs, names, Some("store.put_back"))
                            .await;
                    }
                    RetryClass::Unsupported => break,
                    _ => {
                        tracing::warn!(path = %rel, error = %e, "the put-back failed; recovery finishes it");
                        return held();
                    }
                }
            }
        }
        // The fallback put-back: target (the core's bytes) → second backup, then first backup →
        // target.
        let (target, b2) = (abs.to_path_buf(), names.b2.clone());
        match self
            .fs_retry(None, move || fsops::move_new(&target, &b2))
            .await
        {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound && !self.path_exists(abs).await => {}
            Err(e) => {
                tracing::warn!(path = %rel, error = %e, "the put-back failed; recovery finishes it");
                return held();
            }
        }
        self.restore_backup(rel, abs, names, Some("store.put_back"))
            .await
    }

    /// The rest of a put-back once the target name is free: first backup → target with
    /// `move_new`. Another outside file there is kept, and the first backup set aside.
    async fn restore_backup(
        &self,
        rel: &WsPath,
        abs: &Path,
        names: &Names,
        point: Option<&'static str>,
    ) -> StoreError {
        let held = || StoreError::held_backup(rel, self.rel_of(&names.b1));
        let (b1, target) = (names.b1.clone(), abs.to_path_buf());
        match self
            .fs_retry(point, move || fsops::move_new(&b1, &target))
            .await
        {
            Ok(()) => self.finish_put_back(rel, names, None).await,
            Err(e) if classify(&e) == RetryClass::TargetExists => {
                match self.move_aside(&names.b1, &names.dir, &names.name).await {
                    Ok(aside) => match self.finish_put_back(rel, names, None).await {
                        // The first outside edit's copy is named beside any newer one kept.
                        StoreError::HashConflict {
                            path,
                            backup,
                            cloud_only,
                            mut kept,
                        } => {
                            kept.insert(0, aside);
                            StoreError::HashConflict {
                                path,
                                backup,
                                cloud_only,
                                kept,
                            }
                        }
                        other => other,
                    },
                    Err(_) => held(),
                }
            }
            Err(e) => {
                tracing::warn!(path = %rel, error = %e, "the put-back stopped; recovery finishes it");
                held()
            }
        }
    }

    /// The outside version is in place. The second backup holds whatever the target held when
    /// the put-back moved it: the core's new bytes, which are removed, or a newer outside save
    /// (the user saved again during the put-back), which is moved aside to a visible
    /// `<name>.ww-conflict-<utc>` copy and named in `data.kept`. The pending row is then cleared;
    /// if the second backup can't be handled it stays, with the row, for recovery.
    async fn finish_put_back(
        &self,
        rel: &WsPath,
        names: &Names,
        point: Option<&'static str>,
    ) -> StoreError {
        let (b2, new_sha) = (names.b2.clone(), names.new_sha256.clone());
        let removed = self
            .fs_retry(point, move || remove_if_hash(&b2, &new_sha))
            .await;
        let kept = match removed {
            Ok(true) => Vec::new(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Ok(false) => match self.move_aside(&names.b2, &names.dir, &names.name).await {
                Ok(kept) => {
                    tracing::warn!(path = %rel, kept = %kept, "a newer outside save met the put-back; kept beside the target");
                    vec![kept]
                }
                Err(e) => {
                    tracing::warn!(path = %rel, error = %e, "a newer outside save stays in the backup until recovery");
                    return StoreError::held_backup(rel, self.rel_of(&names.b2));
                }
            },
            Err(e) => {
                tracing::warn!(path = %rel, error = %e, "the second backup stays until recovery");
                return StoreError::held_backup(rel, self.rel_of(&names.b2));
            }
        };
        if let Err(e) = self.state.finish_pending(rel.as_str(), None).await {
            tracing::warn!(path = %rel, error = %e, "a pending row could not be cleared");
        }
        StoreError::HashConflict {
            path: rel.clone(),
            backup: None,
            cloud_only: false,
            kept,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Recovery.
    // -----------------------------------------------------------------------------------------

    /// Run by a read-write core at workspace open (Appendix E recovery rules):
    /// - temp files of processes that no longer run are removed;
    /// - each pending row is resolved (its backups by the five rules), then the row cleared;
    /// - a backup left without a row is restored when its target is missing; otherwise it is
    ///   removed when it equals the target or the generated hash, and else moved aside to a
    ///   visible conflict copy, so an outside edit is never left only in a dot-file.
    ///
    /// Then the staging handles are swept ([`Store::sweep_staging`]): under `core.lock` every
    /// handle is a dead core's, so empty ones are removed and the rest are kept as orphans.
    ///
    /// No content is deleted that isn't the core's own or a duplicate. A read-only store
    /// recovers nothing (`workspace.read_only`).
    pub async fn recover(&self) -> Result<RecoveryReport, StoreError> {
        self.writable_store()?;
        let mut report = RecoveryReport::default();
        let root_rel = WsPath::from_disk(String::new());
        let root = self.root.clone();
        let leftovers = blocking(move || scan_leftovers(&root))
            .await
            .map_err(|e| StoreError::io(&root_rel, &e))?;

        for temp in leftovers.iter().filter(|l| l.kind == LeftoverKind::Temp) {
            let pid = temp.pid;
            if blocking(move || Ok(pid_alive(pid))).await.unwrap_or(true) {
                continue;
            }
            match self.remove(&temp.path).await {
                Ok(()) => report.temps_removed.push(self.rel_of(&temp.path)),
                Err(e) => {
                    tracing::warn!(path = %temp.path.display(), error = %e, "a stale temp file stays")
                }
            }
        }

        let rows = self
            .state
            .pendings()
            .await
            .map_err(|e| StoreError::state(&root_rel, &e))?;
        for row in &rows {
            let rel = WsPath::from_disk(row.path.clone());
            let _held = self.lock_path(&rel).await;
            // The row as it is now, under the lock: a write may have resolved or replaced it.
            let Some(row) = self
                .state
                .pending(&row.path)
                .await
                .map_err(|e| StoreError::state(&rel, &e))?
            else {
                continue;
            };
            if let Err(e) = self.resolve_row(&row, &mut report).await {
                tracing::warn!(path = %rel, error = %e, "a pending replace stays for the next open");
            }
        }

        // A row that couldn't be resolved keeps its backups for the next open.
        let kept: Vec<String> = self
            .state
            .pendings()
            .await
            .map_err(|e| StoreError::state(&root_rel, &e))?
            .into_iter()
            .map(|row| super::paths::path_key(&row.path))
            .collect();
        for backup in leftovers.iter().filter(|l| l.kind == LeftoverKind::Backup) {
            if !self.path_exists(&backup.path).await {
                continue;
            }
            let dir = backup.path.parent().unwrap_or(&self.root).to_path_buf();
            let target = dir.join(&backup.target);
            let rel = self.rel_of(&target);
            if kept.contains(&rel.key()) {
                continue;
            }
            let _held = self.lock_path(&rel).await;
            if let Err(e) = self
                .resolve_loose_backup(&backup.path, &target, &dir, &backup.target, &mut report)
                .await
            {
                tracing::warn!(path = %rel, error = %e, "a backup stays for the next open");
            }
        }

        let mut events: Vec<StoreEvent> = Vec::new();
        for path in report.backups_restored.iter().chain(&report.conflicts_kept) {
            events.push(StoreEvent {
                path: path.clone(),
                kind: ChangeKind::Written,
                sha256: None,
                identity: None,
            });
        }
        self.emit(events).await;

        let (removed, kept) = self.sweep().await?;
        report.staging_removed = removed;
        report.staging_kept = kept;
        Ok(report)
    }

    /// The five rules for one pending row; the row is cleared at the end.
    async fn resolve_row(
        &self,
        row: &Pending,
        report: &mut RecoveryReport,
    ) -> Result<(), StoreError> {
        let rel = WsPath::from_disk(row.path.clone());
        let abs = rel.abs(&self.root);
        let dir = abs.parent().unwrap_or(&self.root).to_path_buf();
        let name = rel.file_name().to_string();
        let backups = {
            let (dir, name) = (dir.clone(), name.clone());
            blocking(move || backups_of(&dir, &name))
                .await
                .map_err(|e| StoreError::io(&rel, &e))?
        };

        // 1. The core's own new bytes in a backup are removed (but never the last copy of the
        // content while the target is missing).
        let mut hashed: Vec<(PathBuf, FileHash)> = Vec::new();
        for backup in backups {
            if let Some(h) = self
                .hash_of(&backup)
                .await
                .map_err(|e| StoreError::io(&rel, &e))?
            {
                hashed.push((backup, h));
            }
        }
        let keep_one = !self.path_exists(&abs).await
            && !hashed.is_empty()
            && hashed.iter().all(|(_, h)| h.sha256 == row.new_sha256);
        let mut left: Vec<(PathBuf, FileHash)> = Vec::new();
        for (i, (backup, h)) in hashed.into_iter().enumerate() {
            if h.sha256 == row.new_sha256 && !(keep_one && i == 0) {
                self.remove(&backup)
                    .await
                    .map_err(|e| StoreError::io(&rel, &e))?;
                report.backups_removed.push(self.rel_of(&backup));
            } else {
                left.push((backup, h));
            }
        }

        // 2. Target missing: the remaining backup (old content or an outside edit) goes back.
        if !left.is_empty() && !self.path_exists(&abs).await {
            let (backup, h) = left.remove(0);
            let (from, to) = (backup.clone(), abs.clone());
            match self
                .fs_retry(None, move || fsops::move_new(&from, &to))
                .await
            {
                Ok(()) => report.backups_restored.push(self.rel_of(&backup)),
                Err(e) if classify(&e) == RetryClass::TargetExists => left.insert(0, (backup, h)),
                Err(e) => return Err(StoreError::io(&rel, &e)),
            }
        }

        // 3 and 4. Both present.
        if !left.is_empty() {
            let generated = self
                .state
                .generated(rel.as_str())
                .await
                .map_err(|e| StoreError::state(&rel, &e))?;
            for (backup, h) in left {
                let ours = row.old_sha256.as_deref() == Some(h.sha256.as_str())
                    || generated.as_deref() == Some(h.sha256.as_str());
                if ours {
                    self.remove(&backup)
                        .await
                        .map_err(|e| StoreError::io(&rel, &e))?;
                    report.backups_removed.push(self.rel_of(&backup));
                    continue;
                }
                // An outside edit: the core's bytes are set aside, the edit goes back.
                if self.path_exists(&abs).await {
                    let kept = self
                        .move_aside(&abs, &dir, &name)
                        .await
                        .map_err(|e| StoreError::io(&rel, &e))?;
                    report.conflicts_kept.push(kept);
                }
                let (from, to) = (backup.clone(), abs.clone());
                self.fs_retry(None, move || fsops::move_new(&from, &to))
                    .await
                    .map_err(|e| StoreError::io(&rel, &e))?;
                report.backups_restored.push(self.rel_of(&backup));
            }
        }

        // 5. The target is the core's new content: it becomes the generated hash.
        let target = self
            .hash_of(&abs)
            .await
            .map_err(|e| StoreError::io(&rel, &e))?;
        let now = self.now_utc();
        let generated = target
            .filter(|h| h.sha256 == row.new_sha256)
            .map(|_| (row.new_sha256.as_str(), now));
        self.state
            .finish_pending(rel.as_str(), generated)
            .await
            .map_err(|e| StoreError::state(&rel, &e))?;
        report.pending_resolved.push(rel);
        Ok(())
    }

    /// A backup that no pending row accounts for.
    async fn resolve_loose_backup(
        &self,
        backup: &Path,
        target: &Path,
        dir: &Path,
        name: &str,
        report: &mut RecoveryReport,
    ) -> Result<(), StoreError> {
        let rel = self.rel_of(target);
        if !self.path_exists(target).await {
            let (from, to) = (backup.to_path_buf(), target.to_path_buf());
            match self
                .fs_retry(None, move || fsops::move_new(&from, &to))
                .await
            {
                Ok(()) => {
                    report.backups_restored.push(self.rel_of(backup));
                    return Ok(());
                }
                Err(e) if classify(&e) == RetryClass::TargetExists => {}
                Err(e) => return Err(StoreError::io(&rel, &e)),
            }
        }
        let io = |e: io::Error| StoreError::io(&rel, &e);
        let Some(h) = self.hash_of(backup).await.map_err(io)? else {
            return Ok(());
        };
        let current = self.hash_of(target).await.map_err(io)?;
        let generated = self
            .state
            .generated(rel.as_str())
            .await
            .map_err(|e| StoreError::state(&rel, &e))?;
        if current.is_some_and(|c| c.sha256 == h.sha256) || generated.as_deref() == Some(&h.sha256)
        {
            self.remove(backup).await.map_err(io)?;
            report.backups_removed.push(self.rel_of(backup));
        } else {
            let kept = self.move_aside(backup, dir, name).await.map_err(io)?;
            report.conflicts_kept.push(kept);
        }
        Ok(())
    }
}

pub(super) fn event(written: &Written, kind: ChangeKind) -> StoreEvent {
    StoreEvent {
        path: written.path.clone(),
        kind,
        sha256: Some(written.sha256.clone()),
        identity: Some(written.identity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-074
    fn leftover_names_parse_back() {
        assert_eq!(
            parse_leftover(".a.txt.ww-tmp-12-7"),
            Some((LeftoverKind::Temp, "a.txt".to_string(), 12, 7))
        );
        assert_eq!(
            parse_leftover("..hidden.ww-bak-3-4"),
            Some((LeftoverKind::Backup, ".hidden".to_string(), 3, 4))
        );
        assert_eq!(
            parse_leftover(".x.ww-tmp-1.ww-bak-2-3"),
            Some((LeftoverKind::Backup, "x.ww-tmp-1".to_string(), 2, 3))
        );
        for no in [
            "a.txt.ww-tmp-1-2",
            ".ww-tmp-1-2",
            ".a.ww-tmp-1",
            ".a.ww-tmp-x-2",
            ".a.ww-bak-1-2b",
            "a.txt.ww-conflict-20261016T120000Z",
        ] {
            assert_eq!(parse_leftover(no), None, "{no}");
        }
    }

    #[test] // spec: ARCH-078
    fn lfs_oid_needs_the_version_line_and_64_hex() {
        let oid = "a".repeat(64);
        let pointer =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 12\n");
        assert_eq!(lfs_pointer_oid(pointer.as_bytes()), Some(oid.clone()));
        let crlf = pointer.replace('\n', "\r\n");
        assert_eq!(lfs_pointer_oid(crlf.as_bytes()), Some(oid.clone()));
        assert_eq!(
            lfs_pointer_oid(format!("oid sha256:{oid}\n").as_bytes()),
            None
        );
        let short = "version https://git-lfs.github.com/spec/v1\noid sha256:abc\n";
        assert_eq!(lfs_pointer_oid(short.as_bytes()), None);
        let mut big = pointer.into_bytes();
        big.resize(LFS_POINTER_MAX, b'\n');
        assert_eq!(lfs_pointer_oid(&big), None);
    }

    #[tokio::test(start_paused = true)] // spec: ARCH-074
    async fn backoff_doubles_to_the_cap_within_the_budget() {
        let start = tokio::time::Instant::now();
        let mut backoff = Backoff::new();
        let mut pauses = Vec::new();
        let mut last = start;
        while backoff.wait().await {
            let now = tokio::time::Instant::now();
            pauses.push(now - last);
            last = now;
        }
        assert_eq!(
            pauses[..7],
            [10, 20, 40, 80, 160, 320, 500].map(Duration::from_millis)
        );
        assert!(pauses.iter().all(|p| *p <= RETRY_CAP));
        assert_eq!(last - start, RETRY_BUDGET);
    }
}
