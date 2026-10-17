//! Reconcile (CORE-041, ARCH-079, ARCH-089, CORE-038, CORE-047): the walk, the per-file
//! decision, stable-snapshot hashing, manifests, `missing` files, the cold rebuild, the store's
//! change events and the background verification.
//!
//! **The walk** covers the workspace without `.git/` (at any depth), the root's `.worldwright/`
//! and `.trash/`, and the store's temp (`.<name>.ww-tmp-*`), backup (`.<name>.ww-bak-*`) and
//! conflict (`*.ww-conflict-*`) names. It never follows a link: a symbolic link, junction or
//! mount point (a name-surrogate reparse point) is skipped, file or folder.
//!
//! **Per file:**
//! 1. `probe.identity` (attribute-only).
//! 2. A cloud placeholder is `cloud_only` and is never opened for reading.
//! 3. Where the volume has stable ids (local NTFS or ReFS outside a sync root), the file is
//!    unchanged when size, last write, FileId and ChangeTime all match its row. Elsewhere (and
//!    when the file reports no FileId or ChangeTime) only size and last write are compared,
//!    and a file whose last write is within 2 s of the previous scan's start is hashed anyway.
//!    File ids only detect changes in place; the store's post-move identity is taken as is.
//! 4. Anything else is hashed through `probe.open_read`, and the identity is taken again: the
//!    digest counts only when size, last write and ChangeTime are unchanged. An unstable read,
//!    or a read that fails with error 32 or 33, parks the path: its row is kept as it was.
//!
//! **Manifests** (`projects/<p>/assets/<a>/<name>.manifest.json`) are parsed into `assets`,
//! `edges`, `assets_fts` and `expected`. The folder is authoritative: one that doesn't parse,
//! or whose `asset_id` or `project` disagrees with its folder, is `needs_repair` and its asset
//! row is flagged; the manifest is never rewritten. With [`ReconcileOrder::ManifestsFirst`]
//! every manifest is looked at, and its results written, before any other file.
//!
//! **Removed files:** a file the suite still expects on disk (a manifest, a file in its
//! manifest's `hashes`, a file of an `open` run without `trashed_utc`) keeps its row and last
//! SHA-256 as `missing`; every other row of a removed file is deleted.
//!
//! Rows are written compare-and-set: a reconcile's result for a file is dropped when the row
//! changed after the reconcile read it (a store write in between wins).

use std::collections::{HashMap, HashSet};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use ww_platform::Clock;
use ww_platform::fs::{FileIdentity, Probe};
use ww_schema::{LinkRef, Manifest};

use super::{
    FileKind, FileRow, FileState, Index, IndexError, Mismatch, PARKED_MAX, ParkedPath,
    ReconcileOrder, ReconcileReport, Scope, asset_of, kind_of, lock, sql,
};
use crate::store::{ChangeKind, StoreEvent, WsPath, lfs_pointer_oid, path_key};

/// A cold rebuild clears the index and trusts manifest hashes; a warm reconcile compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Warm,
    Cold,
}

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// FILETIME units per second.
const TICKS: i64 = 10_000_000;
/// Seconds from 1601-01-01 to 1970-01-01.
const EPOCH_GAP: i64 = 11_644_473_600;
/// FAT write times have 2-second resolution.
const RECENT: i64 = 2 * TICKS;
/// The largest LFS pointer (git-lfs spec).
const LFS_POINTER_MAX: usize = 1024;

fn filetime_now(clock: &dyn Clock) -> i64 {
    let since = clock
        .utc_now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ticks = i64::try_from(since.as_nanos() / 100).unwrap_or(i64::MAX);
    ticks.saturating_add(EPOCH_GAP * TICKS)
}

/// A FILETIME as Unix seconds.
fn unix_secs(filetime: i64) -> i64 {
    filetime.div_euclid(TICKS) - EPOCH_GAP
}

async fn blocking<T, F>(f: F) -> Result<T, IndexError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| IndexError::Io(format!("a blocking index call stopped: {e}")))
}

// ---------------------------------------------------------------------------------------------
// Rows.
// ---------------------------------------------------------------------------------------------

/// The columns [`db_file`] reads, in order.
pub(super) const FILE_COLUMNS: &str = "key, path, size, last_write, change_time, file_id, \
    volume_serial, attributes, sha256, state, verified";

/// A `files` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DbFile {
    pub(super) key: String,
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) last_write: i64,
    pub(super) change_time: Option<i64>,
    pub(super) file_id: Option<[u8; 16]>,
    pub(super) volume_serial: u64,
    pub(super) attributes: u32,
    pub(super) sha256: Option<String>,
    pub(super) state: FileState,
    pub(super) verified: bool,
}

impl DbFile {
    fn new(
        path: &WsPath,
        id: &FileIdentity,
        sha256: Option<String>,
        state: FileState,
        verified: bool,
    ) -> DbFile {
        DbFile {
            key: path_key(path.as_str()),
            path: path.as_str().to_string(),
            size: id.size,
            last_write: id.last_write,
            change_time: id.change_time,
            file_id: id.file_id,
            volume_serial: id.volume_serial,
            attributes: id.attributes,
            sha256,
            state,
            verified,
        }
    }

    fn identity(&self) -> FileIdentity {
        FileIdentity {
            volume_serial: self.volume_serial,
            file_id: self.file_id,
            size: self.size,
            last_write: self.last_write,
            change_time: self.change_time,
            attributes: self.attributes,
        }
    }

    pub(super) fn into_row(self) -> FileRow {
        FileRow {
            identity: Some(self.identity()),
            path: WsPath::from_disk(self.path),
            size: self.size,
            sha256: self.sha256,
            state: self.state,
        }
    }

    /// What the compare-and-set compares.
    fn version(&self) -> (Option<&str>, u64, i64, Option<i64>, FileState) {
        (
            self.sha256.as_deref(),
            self.size,
            self.last_write,
            self.change_time,
            self.state,
        )
    }
}

/// A row of [`FILE_COLUMNS`].
pub(super) fn db_file(r: &rusqlite::Row<'_>) -> rusqlite::Result<DbFile> {
    let file_id: Option<Vec<u8>> = r.get(5)?;
    let size: i64 = r.get(2)?;
    let serial: i64 = r.get(6)?;
    let attributes: i64 = r.get(7)?;
    let state: String = r.get(9)?;
    Ok(DbFile {
        key: r.get(0)?,
        path: r.get(1)?,
        size: size.cast_unsigned(),
        last_write: r.get(3)?,
        change_time: r.get(4)?,
        file_id: file_id.and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok()),
        volume_serial: serial.cast_unsigned(),
        attributes: u32::try_from(attributes).unwrap_or(0),
        sha256: r.get(8)?,
        state: FileState::parse(&state),
        verified: r.get::<_, i64>(10)? != 0,
    })
}

fn upsert_file(tx: &Transaction<'_>, row: &DbFile) -> rusqlite::Result<()> {
    let (project, asset) = match asset_of(&row.path) {
        Some((p, a)) => (Some(p), Some(a)),
        None => (None, None),
    };
    tx.prepare_cached(
        "INSERT OR REPLACE INTO files (key, path, project, asset_id, size, last_write,
             change_time, file_id, volume_serial, attributes, sha256, state, verified)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?
    .execute(params![
        row.key,
        row.path,
        project,
        asset,
        row.size.cast_signed(),
        row.last_write,
        row.change_time,
        row.file_id.map(|f| f.to_vec()),
        row.volume_serial.cast_signed(),
        i64::from(row.attributes),
        row.sha256,
        row.state.as_str(),
        i64::from(row.verified),
    ])?;
    Ok(())
}

fn current(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<Option<DbFile>> {
    tx.prepare_cached(&format!("SELECT {FILE_COLUMNS} FROM files WHERE key = ?1"))?
        .query_row(params![key], db_file)
        .optional()
}

/// Whether the row of `key` is still what the reconcile read (`old`, or none).
fn unchanged_since(
    tx: &Transaction<'_>,
    key: &str,
    old: Option<&DbFile>,
) -> rusqlite::Result<bool> {
    let now = current(tx, key)?;
    Ok(match (old, now.as_ref()) {
        (None, None) => true,
        (Some(a), Some(b)) => a.version() == b.version(),
        _ => false,
    })
}

/// Rows whose key is `prefix` or under the folder `prefix`.
const UNDER: &str = "(key = ?1 OR (key > ?1 || '/' AND key < ?1 || '0'))";

/// The rows in `scope`, by key.
fn load_rows(conn: &Connection, scope: &Scope) -> rusqlite::Result<HashMap<String, DbFile>> {
    let mut rows = HashMap::new();
    match scope {
        Scope::All => {
            let mut stmt = conn.prepare(&format!("SELECT {FILE_COLUMNS} FROM files"))?;
            for row in stmt.query_map([], db_file)? {
                let row = row?;
                rows.insert(row.key.clone(), row);
            }
        }
        Scope::Paths(paths) => {
            let mut stmt =
                conn.prepare(&format!("SELECT {FILE_COLUMNS} FROM files WHERE {UNDER}"))?;
            for path in paths {
                for row in stmt.query_map(params![path_key(path.as_str())], db_file)? {
                    let row = row?;
                    rows.insert(row.key.clone(), row);
                }
            }
        }
    }
    Ok(rows)
}

/// The manifest keys of the asset rows.
fn load_manifests(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT manifest FROM assets")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.map(|m| m.map(|m| path_key(&m))).collect()
}

// ---------------------------------------------------------------------------------------------
// The walk.
// ---------------------------------------------------------------------------------------------

/// A file the walk found.
#[derive(Debug, Clone)]
pub(super) struct Walked {
    rel: WsPath,
    key: String,
    abs: PathBuf,
    kind: FileKind,
}

fn walked(root: &Path, rel: String) -> Walked {
    let rel = WsPath::from_disk(rel);
    Walked {
        key: path_key(rel.as_str()),
        abs: rel.abs(root),
        kind: kind_of(&rel),
        rel,
    }
}

fn skip_folder(name: &str, at_root: bool) -> bool {
    name.eq_ignore_ascii_case(".git")
        || (at_root
            && (name.eq_ignore_ascii_case(".worldwright") || name.eq_ignore_ascii_case(".trash")))
}

fn skip_file(name: &str) -> bool {
    (name.starts_with('.') && (name.contains(".ww-tmp-") || name.contains(".ww-bak-")))
        || name.contains(".ww-conflict-")
}

/// Whether a scoped path is one the walk never enters.
fn skipped_path(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    parts
        .iter()
        .enumerate()
        .any(|(i, name)| skip_folder(name, i == 0))
        || parts.last().is_some_and(|name| skip_file(name))
}

/// Every file under `dir` (`rel` is its workspace-relative path, empty at the root).
fn walk_dir(
    root: &Path,
    dir: &Path,
    rel: &str,
    closed: &AtomicBool,
    out: &mut Vec<Walked>,
) -> io::Result<()> {
    let mut folders = vec![(dir.to_path_buf(), rel.to_string())];
    while let Some((dir, rel)) = folders.pop() {
        if closed.load(Ordering::Relaxed) {
            return Err(io::Error::other("the index closed"));
        }
        let entries = match std::fs::read_dir(ww_platform::fs::long_path(&dir)) {
            Ok(entries) => entries,
            Err(e) if rel.is_empty() || e.kind() == io::ErrorKind::NotFound => {
                if rel.is_empty() && e.kind() != io::ErrorKind::NotFound {
                    return Err(e);
                }
                continue;
            }
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "the index can't list a folder");
                continue;
            }
        };
        let mut names: Vec<(String, std::fs::FileType)> = Vec::new();
        for entry in entries.flatten() {
            if let Ok(kind) = entry.file_type() {
                names.push((entry.file_name().to_string_lossy().into_owned(), kind));
            }
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, kind) in names {
            // A link (symbolic link, junction, mount point) is never followed or indexed.
            if kind.is_symlink() {
                continue;
            }
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if kind.is_dir() {
                if !skip_folder(&name, rel.is_empty()) {
                    folders.push((dir.join(&name), child));
                }
            } else if !skip_file(&name) {
                out.push(walked(root, child));
            }
        }
    }
    Ok(())
}

/// The files of `scope`, and the key prefixes inside which a row the walk didn't see is gone
/// (`None`: every row).
fn walk(
    root: &Path,
    scope: &Scope,
    closed: &AtomicBool,
) -> io::Result<(Vec<Walked>, Option<Vec<String>>)> {
    let mut out = Vec::new();
    match scope {
        Scope::All => {
            walk_dir(root, root, "", closed, &mut out)?;
            Ok((out, None))
        }
        Scope::Paths(paths) => {
            let mut prefixes = Vec::new();
            let mut seen = HashSet::new();
            for path in paths {
                if skipped_path(path.as_str()) {
                    continue;
                }
                prefixes.push(path_key(path.as_str()));
                let abs = path.abs(root);
                match std::fs::symlink_metadata(ww_platform::fs::long_path(&abs)) {
                    Ok(meta) if meta.file_type().is_symlink() => {}
                    Ok(meta) if meta.is_dir() => {
                        walk_dir(root, &abs, path.as_str(), closed, &mut out)?;
                    }
                    Ok(_) => out.push(walked(root, path.as_str().to_string())),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => {
                        tracing::warn!(path = %path, error = %e, "the index can't look at a path");
                    }
                }
            }
            out.retain(|w| seen.insert(w.key.clone()));
            Ok((out, Some(prefixes)))
        }
    }
}

fn in_scope(key: &str, prefixes: Option<&[String]>) -> bool {
    match prefixes {
        None => true,
        Some(prefixes) => prefixes.iter().any(|p| {
            key == p
                || (key.len() > p.len()
                    && key.starts_with(p.as_str())
                    && key.as_bytes()[p.len()] == b'/')
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// Reading files.
// ---------------------------------------------------------------------------------------------

/// A stable read: the digest, the LFS `oid` when the file is a pointer, the bytes when asked
/// for, and the identity after the read.
struct Read {
    sha256: String,
    lfs: Option<String>,
    bytes: Option<Vec<u8>>,
    after: FileIdentity,
}

enum ReadFail {
    /// Unstable, or held (32/33): parked.
    Parked,
    Gone,
    Other(io::Error),
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Reads `abs` through `probe.open_read` and takes its identity again: the digest counts only
/// when size and last write are what `before` says, and ChangeTime too whenever both values
/// are present and the volume isn't FAT (`change_time`; ruling Q21: FAT has no ChangeTime to
/// trust).
fn read_stable(
    probe: &dyn Probe,
    abs: &Path,
    before: &FileIdentity,
    keep: bool,
    change_time: bool,
) -> Result<Read, ReadFail> {
    let fail = |e: io::Error| match e.raw_os_error() {
        Some(32 | 33) => ReadFail::Parked,
        _ if e.kind() == io::ErrorKind::NotFound => ReadFail::Gone,
        _ => ReadFail::Other(e),
    };
    let mut file = probe.open_read(abs).map_err(fail)?;
    let mut hasher = Sha256::new();
    let mut head = Vec::new();
    let mut bytes = keep.then(Vec::new);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(fail)?;
        if n == 0 {
            break;
        }
        if head.len() < LFS_POINTER_MAX {
            let take = n.min(LFS_POINTER_MAX - head.len());
            head.extend_from_slice(&buf[..take]);
        }
        if let Some(bytes) = bytes.as_mut() {
            bytes.extend_from_slice(&buf[..n]);
        }
        hasher.update(&buf[..n]);
    }
    drop(file);
    let after = probe.identity(abs).map_err(fail)?;
    if after.size != before.size
        || after.last_write != before.last_write
        || (change_time
            && after.change_time.is_some()
            && before.change_time.is_some()
            && after.change_time != before.change_time)
    {
        return Err(ReadFail::Parked);
    }
    Ok(Read {
        sha256: hex(&hasher.finalize()),
        lfs: lfs_pointer_oid(&head),
        bytes,
        after,
    })
}

// ---------------------------------------------------------------------------------------------
// The per-file decision.
// ---------------------------------------------------------------------------------------------

/// What the walk's look at one file found.
#[derive(Debug)]
enum Outcome {
    Unchanged,
    /// A row to write: its content was hashed (`hashed`), and is new or different (`changed`).
    /// A manifest's bytes come along for the parse.
    Row {
        row: DbFile,
        hashed: bool,
        changed: bool,
        bytes: Option<Vec<u8>>,
    },
    /// A cloud placeholder: a row to write when it differs from the old one.
    Cloud(Option<DbFile>),
    Parked,
    Gone,
    /// An error, logged: the row stays as it is.
    Skip,
}

/// What every decision of one run shares.
struct Judge {
    probe: Arc<dyn Probe>,
    stable: bool,
    /// A FAT or exFAT volume: no ChangeTime to compare after a read.
    fat: bool,
    prev_scan: Option<i64>,
    rows: Arc<HashMap<String, DbFile>>,
    /// Manifest keys that have an asset row.
    known: Arc<HashSet<String>>,
    /// Cold rebuild: the hashes manifests record, by key, taken on trust (not read).
    trusted: Arc<HashMap<String, String>>,
    /// Warm reconcile: the hashes manifests record, by key, read after the manifests were
    /// re-parsed. A hashed file whose content differs stays unverified, so `verify_pending`
    /// reports it (ruling Q20).
    recorded: Arc<HashMap<String, String>>,
}

impl Judge {
    fn same(&self, old: &DbFile, id: &FileIdentity) -> bool {
        if old.size != id.size || old.last_write != id.last_write {
            return false;
        }
        if self.stable && id.file_id.is_some() && id.change_time.is_some() {
            old.file_id == id.file_id
                && old.change_time == id.change_time
                && old.volume_serial == id.volume_serial
        } else {
            // Without stable ids, a file written within 2 s of the previous scan's start may
            // have changed inside the same FAT time tick.
            !self
                .prev_scan
                .is_some_and(|start| id.last_write >= start - RECENT)
        }
    }

    fn decide(&self, w: &Walked) -> Outcome {
        let old = self.rows.get(&w.key);
        let id = match self.probe.identity(&w.abs) {
            Ok(id) => id,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Outcome::Gone,
            Err(e) if matches!(e.raw_os_error(), Some(32 | 33)) => return Outcome::Parked,
            Err(e) => {
                tracing::warn!(path = %w.rel, error = %e, "the index can't look at a file");
                return Outcome::Skip;
            }
        };
        if id.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Outcome::Skip;
        }
        if id.is_cloud_placeholder() {
            if old.is_some_and(|o| o.state == FileState::CloudOnly && self.same(o, &id)) {
                return Outcome::Cloud(None);
            }
            let sha = old
                .and_then(|o| o.sha256.clone())
                .or_else(|| self.trusted.get(&w.key).cloned());
            let verified = old.is_some_and(|o| o.verified);
            return Outcome::Cloud(Some(DbFile::new(
                &w.rel,
                &id,
                sha,
                FileState::CloudOnly,
                verified,
            )));
        }
        let manifest = w.kind == FileKind::Manifest;
        let look = match old {
            None => true,
            Some(o) => {
                !self.same(o, &id)
                    || matches!(o.state, FileState::Missing | FileState::CloudOnly)
                    || (manifest && !self.known.contains(&w.key))
            }
        };
        if !look {
            return Outcome::Unchanged;
        }
        if !manifest && let Some(sha) = self.trusted.get(&w.key) {
            // Cold rebuild: the manifest's hash, unverified until `verify_pending` reads it.
            return Outcome::Row {
                row: DbFile::new(&w.rel, &id, Some(sha.clone()), FileState::Ok, false),
                hashed: false,
                changed: true,
                bytes: None,
            };
        }
        match read_stable(self.probe.as_ref(), &w.abs, &id, manifest, !self.fat) {
            Ok(read) => {
                let (sha, state) = match read.lfs {
                    Some(oid) => (oid, FileState::NotFetched),
                    None => {
                        let keep = old
                            .filter(|o| {
                                o.sha256.as_deref() == Some(read.sha256.as_str())
                                    && matches!(
                                        o.state,
                                        FileState::HumanEdit | FileState::NeedsRepair
                                    )
                            })
                            .map_or(FileState::Ok, |o| o.state);
                        (read.sha256, keep)
                    }
                };
                let changed = old.is_none_or(|o| o.sha256.as_deref() != Some(sha.as_str()));
                // Content its manifest doesn't record (an edit made outside, perhaps while no
                // core ran) stays unverified for `verify_pending` to report.
                let verified = self.recorded.get(&w.key).is_none_or(|want| *want == sha);
                Outcome::Row {
                    row: DbFile::new(&w.rel, &read.after, Some(sha), state, verified),
                    hashed: true,
                    changed,
                    bytes: read.bytes,
                }
            }
            Err(ReadFail::Parked) => Outcome::Parked,
            Err(ReadFail::Gone) => Outcome::Gone,
            Err(ReadFail::Other(e)) => {
                tracing::warn!(path = %w.rel, error = %e, "the index can't read a file");
                Outcome::Skip
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Manifests.
// ---------------------------------------------------------------------------------------------

/// A manifest input or pin.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    to_project: String,
    to_asset: String,
    to_version: Option<String>,
    kind: &'static str,
}

/// A file a manifest records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Expected {
    key: String,
    path: String,
    sha256: Option<String>,
    /// The suite still expects it on disk (in `hashes`, or a file of an `open` run).
    tracked: bool,
}

/// What a manifest says, read leniently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Fields {
    asset_type: Option<String>,
    status: Option<String>,
    version: Option<String>,
    approved_version: Option<String>,
    creation_uuid: Option<String>,
    edges: Vec<Edge>,
    expected: Vec<Expected>,
    body: String,
}

/// A parsed manifest, keyed by its folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Parsed {
    project: String,
    asset_id: String,
    manifest: WsPath,
    updated: i64,
    /// `None`: it doesn't parse (the asset row keeps its last values).
    fields: Option<Fields>,
    needs_repair: bool,
}

fn string(map: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Parses the manifest `rel` (in the folder of `(project, asset_id)`) from `bytes`.
fn parse_manifest(rel: &WsPath, bytes: &[u8], last_write: i64) -> Option<Parsed> {
    let (project, asset_id) = asset_of(rel.as_str())?;
    let (project, asset_id) = (project.to_string(), asset_id.to_string());
    let updated = unix_secs(last_write);
    let parsed = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| Manifest::parse(text).ok());
    let Some((manifest, _warnings)) = parsed else {
        return Some(Parsed {
            project,
            asset_id,
            manifest: rel.clone(),
            updated,
            fields: None,
            needs_repair: true,
        });
    };
    let map = manifest.as_map();
    let view = manifest.view();
    // The folder name is authoritative (CORE-047): a manifest that names another asset or
    // project (a folder copied or moved outside the suite) needs repair.
    let disagrees = |key: &str, folder: &str| match map.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => s != folder,
        Some(_) => true,
    };
    let canonical = format!("{asset_id}.manifest.json");
    let needs_repair = disagrees("asset_id", &asset_id)
        || disagrees("project", &project)
        || !rel.file_name().eq_ignore_ascii_case(&canonical);

    let mut edges = Vec::new();
    let mut link = |reference: &str, kind: &'static str| {
        let Ok(parsed) = reference.parse::<LinkRef>() else {
            return;
        };
        let (to_project, to_asset, to_version) = match parsed {
            LinkRef::Local { asset, version } => {
                (project.clone(), asset, Some(version.to_string()))
            }
            LinkRef::Project {
                slug,
                asset,
                version,
            } => (slug, asset, Some(version.to_string())),
            LinkRef::Library { item, version } => {
                ("lib".to_string(), item, Some(version.to_string()))
            }
            LinkRef::Style(_) => return,
        };
        edges.push(Edge {
            to_project,
            to_asset,
            to_version,
            kind,
        });
    };
    for input in &view.inputs {
        link(&input.r#ref, "input");
    }
    for pin in &view.pinned_inputs {
        link(&pin.r#ref, "pinned");
    }

    let base = format!("projects/{project}/assets/{asset_id}");
    let mut expected: HashMap<String, Expected> = HashMap::new();
    for (inside, sha) in &view.hashes {
        let path = format!("{base}/{inside}");
        expected.insert(
            path_key(&path),
            Expected {
                key: path_key(&path),
                path,
                sha256: Some(sha.clone()),
                tracked: true,
            },
        );
    }
    for run in &view.candidate_runs {
        let open = run.status.as_deref() == Some("open") && run.trashed_utc.is_none();
        for file in &run.files {
            let path = format!("{base}/{}", file.path);
            let key = path_key(&path);
            if expected.contains_key(&key) {
                continue;
            }
            expected.insert(
                key.clone(),
                Expected {
                    key,
                    path,
                    sha256: file.sha256.clone(),
                    tracked: open,
                },
            );
        }
    }
    let mut expected: Vec<Expected> = expected.into_values().collect();
    expected.sort_by(|a, b| a.key.cmp(&b.key));

    let mut body = vec![asset_id.clone(), project.clone()];
    for key in ["type", "status", "name", "title", "notes", "description"] {
        if let Some(text) = string(map, key) {
            body.push(text);
        }
    }
    if let Some(Value::Array(tags)) = map.get("tags") {
        body.extend(tags.iter().filter_map(Value::as_str).map(str::to_string));
    }
    Some(Parsed {
        project,
        asset_id,
        manifest: rel.clone(),
        updated,
        fields: Some(Fields {
            asset_type: string(map, "type"),
            status: string(map, "status"),
            version: string(map, "version"),
            approved_version: string(map, "approved_version"),
            creation_uuid: view.creation_uuid.clone(),
            edges,
            expected,
            body: body.join(" "),
        }),
        needs_repair,
    })
}

/// Whether the manifest `rel` is the one its folder uses: `<asset_id>.manifest.json`, or any
/// `*.manifest.json` when that one doesn't exist.
fn is_folder_manifest(root: &Path, rel: &WsPath) -> bool {
    let Some((_, asset_id)) = asset_of(rel.as_str()) else {
        return false;
    };
    let canonical = format!("{asset_id}.manifest.json");
    if rel.file_name().eq_ignore_ascii_case(&canonical) {
        return true;
    }
    let beside = rel.abs(root).with_file_name(canonical);
    ww_platform::fs::attributes(&beside).is_err()
}

fn upsert_asset(tx: &Transaction<'_>, parsed: &Parsed) -> rusqlite::Result<()> {
    let Some(fields) = &parsed.fields else {
        // It doesn't parse: flag the row and keep its last values, edges and expected files.
        tx.prepare_cached(
            "INSERT INTO assets (project, asset_id, manifest, updated, needs_repair)
             VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT (project, asset_id) DO UPDATE SET
                 manifest = excluded.manifest, updated = excluded.updated, needs_repair = 1",
        )?
        .execute(params![
            parsed.project,
            parsed.asset_id,
            parsed.manifest.as_str(),
            parsed.updated
        ])?;
        return Ok(());
    };
    let id: i64 = tx
        .prepare_cached(
            "INSERT INTO assets (project, asset_id, asset_type, status, version, approved_version,
                 manifest, creation_uuid, updated, needs_repair)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (project, asset_id) DO UPDATE SET
                 asset_type = excluded.asset_type, status = excluded.status,
                 version = excluded.version, approved_version = excluded.approved_version,
                 manifest = excluded.manifest, creation_uuid = excluded.creation_uuid,
                 updated = excluded.updated, needs_repair = excluded.needs_repair
             RETURNING id",
        )?
        .query_row(
            params![
                parsed.project,
                parsed.asset_id,
                fields.asset_type,
                fields.status,
                fields.version,
                fields.approved_version,
                parsed.manifest.as_str(),
                fields.creation_uuid,
                parsed.updated,
                i64::from(parsed.needs_repair),
            ],
            |r| r.get(0),
        )?;
    tx.prepare_cached("DELETE FROM assets_fts WHERE rowid = ?1")?
        .execute(params![id])?;
    tx.prepare_cached("INSERT INTO assets_fts (rowid, body) VALUES (?1, ?2)")?
        .execute(params![id, fields.body])?;
    tx.prepare_cached("DELETE FROM edges WHERE from_project = ?1 AND from_asset = ?2")?
        .execute(params![parsed.project, parsed.asset_id])?;
    for edge in &fields.edges {
        tx.prepare_cached(
            "INSERT INTO edges (from_project, from_asset, to_project, to_asset, to_version, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            parsed.project,
            parsed.asset_id,
            edge.to_project,
            edge.to_asset,
            edge.to_version,
            edge.kind
        ])?;
    }
    tx.prepare_cached("DELETE FROM expected WHERE project = ?1 AND asset_id = ?2")?
        .execute(params![parsed.project, parsed.asset_id])?;
    for e in &fields.expected {
        tx.prepare_cached(
            "INSERT OR REPLACE INTO expected (key, path, project, asset_id, sha256, tracked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            e.key,
            e.path,
            parsed.project,
            parsed.asset_id,
            e.sha256,
            i64::from(e.tracked)
        ])?;
    }
    Ok(())
}

/// Removes the asset rows whose manifest is `key` or under it, with their edges, text and
/// expected files (the store moved the folder or the manifest away).
fn delete_assets_under(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare_cached("SELECT id, project, asset_id, manifest FROM assets")?;
    let doomed: Vec<(i64, String, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?))
        })?
        .filter_map(Result::ok)
        .filter(|(_, _, _, manifest)| in_scope(&path_key(manifest), Some(&[key.to_string()])))
        .map(|(id, p, a, _)| (id, p, a))
        .collect();
    drop(stmt);
    for (id, project, asset_id) in doomed {
        tx.execute("DELETE FROM assets WHERE id = ?1", params![id])?;
        tx.execute("DELETE FROM assets_fts WHERE rowid = ?1", params![id])?;
        tx.execute(
            "DELETE FROM edges WHERE from_project = ?1 AND from_asset = ?2",
            params![project, asset_id],
        )?;
        tx.execute(
            "DELETE FROM expected WHERE project = ?1 AND asset_id = ?2",
            params![project, asset_id],
        )?;
    }
    Ok(())
}

/// Whether the suite still expects the file of `key` on disk.
fn tracked(tx: &Transaction<'_>, key: &str, path: &str) -> rusqlite::Result<bool> {
    if kind_of(&WsPath::from_disk(path.to_string())) == FileKind::Manifest {
        return Ok(true);
    }
    Ok(tx
        .prepare_cached("SELECT tracked FROM expected WHERE key = ?1")?
        .query_row(params![key], |r| r.get::<_, i64>(0))
        .optional()?
        .is_some_and(|t| t != 0))
}

// ---------------------------------------------------------------------------------------------
// One run.
// ---------------------------------------------------------------------------------------------

/// The results of looking at a list of files.
#[derive(Default)]
struct Looked {
    rows: Vec<(DbFile, Option<DbFile>)>,
    parsed: Vec<Parsed>,
    /// Keys the walk saw that are still there (or parked).
    present: HashSet<String>,
}

/// Looks at `files` on a blocking thread and folds the outcomes into `report`.
async fn look(
    index: &Index,
    judge: Arc<Judge>,
    files: Vec<Walked>,
    report: &mut ReconcileReport,
) -> Result<Looked, IndexError> {
    let closed = index.closed.clone();
    let root = index.root.clone();
    let decided = blocking(move || {
        let mut out = Vec::with_capacity(files.len());
        for w in files {
            if closed.load(Ordering::Relaxed) {
                return Err(IndexError::ActorGone);
            }
            let outcome = judge.decide(&w);
            let parsed = match &outcome {
                Outcome::Row {
                    row,
                    bytes: Some(bytes),
                    ..
                } if is_folder_manifest(&root, &w.rel) => {
                    parse_manifest(&w.rel, bytes, row.last_write)
                }
                _ => None,
            };
            let old = judge.rows.get(&w.key).cloned();
            out.push((w, outcome, parsed, old));
        }
        Ok(out)
    })
    .await??;

    let mut looked = Looked::default();
    for (w, outcome, parsed, old) in decided {
        match outcome {
            Outcome::Unchanged => {
                report.unchanged += 1;
                looked.present.insert(w.key.clone());
                lock(&index.parked).remove(&w.key);
            }
            Outcome::Row {
                mut row,
                hashed,
                changed,
                ..
            } => {
                looked.present.insert(w.key.clone());
                lock(&index.parked).remove(&w.key);
                if hashed {
                    report.hashed += 1;
                }
                if changed {
                    report.changed.push(w.rel.clone());
                }
                if let Some(parsed) = parsed {
                    if parsed.needs_repair {
                        row.state = FileState::NeedsRepair;
                        report.needs_repair.push(w.rel.clone());
                    } else if row.state == FileState::NeedsRepair {
                        row.state = FileState::Ok;
                    }
                    looked.parsed.push(parsed);
                }
                looked.rows.push((row, old));
            }
            Outcome::Cloud(row) => {
                looked.present.insert(w.key.clone());
                report.cloud_only.push(w.rel.clone());
                if let Some(row) = row {
                    looked.rows.push((row, old));
                } else {
                    report.unchanged += 1;
                }
            }
            Outcome::Parked => {
                looked.present.insert(w.key.clone());
                report.parked.push(w.rel.clone());
                let mut parked = lock(&index.parked);
                if parked.contains_key(&w.key) || parked.len() < PARKED_MAX {
                    parked.entry(w.key.clone()).or_insert(ParkedPath {
                        path: w.rel.clone(),
                        retries: 0,
                    });
                } else {
                    tracing::warn!(path = %w.rel, "too many parked files; the next rescan looks at this one");
                }
            }
            Outcome::Skip => {
                looked.present.insert(w.key.clone());
            }
            Outcome::Gone => {}
        }
    }
    Ok(looked)
}

/// Writes `rows` (compare-and-set) and the asset rows of the manifests among them whose row
/// was written.
fn write_rows(
    tx: &Transaction<'_>,
    rows: &[(DbFile, Option<DbFile>)],
    parsed: &[Parsed],
    cas: bool,
) -> rusqlite::Result<()> {
    let mut written = HashSet::new();
    for (row, old) in rows {
        if cas && !unchanged_since(tx, &row.key, old.as_ref())? {
            tracing::debug!(path = %row.path, "a row changed during the reconcile; the newer one stays");
            continue;
        }
        upsert_file(tx, row)?;
        written.insert(row.key.clone());
    }
    for p in parsed {
        if written.contains(&path_key(p.manifest.as_str())) {
            upsert_asset(tx, p)?;
        }
    }
    Ok(())
}

/// What the workspace volume says: whether its FileId and ChangeTime can be trusted, and
/// whether it is FAT or exFAT. Unreadable facts count as no stable ids, and not FAT (CORE-041
/// clarification; ruling Q21).
async fn volume_facts(index: &Index) -> Result<(bool, bool), IndexError> {
    let (probe, root) = (index.probe.clone(), index.root.clone());
    Ok(match blocking(move || probe.volume_info(&root)).await? {
        Ok(volume) => (volume.stable_ids(), is_fat(&volume.fs_name)),
        Err(e) => {
            tracing::debug!(error = %e, "the volume facts can't be read; file ids aren't trusted");
            (false, false)
        }
    })
}

/// `FAT`, `FAT12`, `FAT16`, `FAT32` or `exFAT`, in any case.
fn is_fat(fs_name: &str) -> bool {
    let fs_name = fs_name.to_ascii_lowercase();
    fs_name.starts_with("fat") || fs_name == "exfat"
}

/// The hashes the manifests record for the files in `scope`, by key.
fn load_recorded(conn: &Connection, scope: &Scope) -> rusqlite::Result<HashMap<String, String>> {
    let pair = |r: &rusqlite::Row<'_>| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?));
    let mut out = HashMap::new();
    match scope {
        Scope::All => {
            let mut stmt =
                conn.prepare("SELECT key, sha256 FROM expected WHERE sha256 IS NOT NULL")?;
            for row in stmt.query_map([], pair)? {
                let (key, sha) = row?;
                out.insert(key, sha);
            }
        }
        Scope::Paths(paths) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT key, sha256 FROM expected WHERE sha256 IS NOT NULL AND {UNDER}"
            ))?;
            for path in paths {
                for row in stmt.query_map(params![path_key(path.as_str())], pair)? {
                    let (key, sha) = row?;
                    out.insert(key, sha);
                }
            }
        }
    }
    Ok(out)
}

/// The store's changes `apply_events` wrote: replayed in a cold rebuild's own transaction
/// when they landed while it ran, so the rebuild never loses or reverts one.
#[derive(Debug, Clone, Default)]
pub(super) struct StoreChanges {
    removed: Vec<String>,
    rows: Vec<DbFile>,
    parsed: Vec<Parsed>,
}

/// While it lives, a cold rebuild is running: `apply_events` logs what it writes. Dropping it
/// (the rebuild ended, either way) stops the log.
struct ColdLog<'a>(&'a Mutex<Option<Vec<StoreChanges>>>);

impl Drop for ColdLog<'_> {
    fn drop(&mut self) {
        *lock(self.0) = None;
    }
}

/// Runs one reconcile or cold rebuild under the scan lock. `scan` holds the previous whole
/// scan's start, and gets this one's when this one covers the whole workspace: a scoped run
/// (a parked re-check, a path rescan) never moves it, so the 2-second rule still covers files
/// written just before the last whole scan.
pub(super) async fn run(
    index: &Index,
    scan: &mut Option<i64>,
    scope: Scope,
    order: ReconcileOrder,
    mode: Mode,
) -> Result<ReconcileReport, IndexError> {
    let started = filetime_now(index.clock.as_ref());
    let whole = scope == Scope::All;
    let mut report = ReconcileReport::default();
    let (stable, fat) = volume_facts(index).await?;
    let _cold = (mode == Mode::Cold).then(|| {
        *lock(&index.cold_log) = Some(Vec::new());
        ColdLog(&index.cold_log)
    });

    let (rows, known) = match mode {
        Mode::Cold => (HashMap::new(), HashSet::new()),
        Mode::Warm => {
            let scope = scope.clone();
            index
                .db
                .call(move |conn| {
                    let rows = load_rows(conn, &scope).map_err(|e| sql(&e))?;
                    let known = load_manifests(conn).map_err(|e| sql(&e))?;
                    Ok((rows, known))
                })
                .await?
        }
    };
    let rows = Arc::new(rows);

    let (files, prefixes) = {
        let (root, scope, closed) = (index.root.clone(), scope.clone(), index.closed.clone());
        blocking(move || walk(&root, &scope, &closed))
            .await?
            .map_err(|e| IndexError::Io(format!("the workspace can't be listed: {e}")))?
    };

    let mut judge = Judge {
        probe: index.probe.clone(),
        stable,
        fat,
        prev_scan: if mode == Mode::Cold { None } else { *scan },
        rows: rows.clone(),
        known: Arc::new(known),
        trusted: Arc::new(HashMap::new()),
        recorded: Arc::new(HashMap::new()),
    };

    let (first, rest): (Vec<Walked>, Vec<Walked>) = match order {
        ReconcileOrder::ManifestsFirst => files
            .into_iter()
            .partition(|w| w.kind == FileKind::Manifest),
        ReconcileOrder::AsWalked => (Vec::new(), files),
    };

    // Manifests first: looked at, and (warm) written, before any other file.
    let mut manifests = look(index, Arc::new(clone_judge(&judge)), first, &mut report).await?;
    match mode {
        Mode::Warm => {
            let (rows, parsed) = (
                std::mem::take(&mut manifests.rows),
                std::mem::take(&mut manifests.parsed),
            );
            let scope = scope.clone();
            let recorded = index
                .db
                .call(move |conn| {
                    if !rows.is_empty() {
                        let tx = conn.transaction().map_err(|e| sql(&e))?;
                        write_rows(&tx, &rows, &parsed, true).map_err(|e| sql(&e))?;
                        tx.commit().map_err(|e| sql(&e))?;
                    }
                    load_recorded(conn, &scope).map_err(|e| sql(&e))
                })
                .await?;
            judge.recorded = Arc::new(recorded);
        }
        Mode::Cold => {
            let mut trusted = HashMap::new();
            for p in &manifests.parsed {
                for e in p.fields.iter().flat_map(|f| &f.expected) {
                    if let Some(sha) = &e.sha256 {
                        trusted.insert(e.key.clone(), sha.clone());
                    }
                }
            }
            judge.trusted = Arc::new(trusted);
        }
    }

    let others = look(index, Arc::new(judge), rest, &mut report).await?;

    let mut present = manifests.present;
    present.extend(others.present);
    let gone: Vec<DbFile> = rows
        .values()
        .filter(|r| in_scope(&r.key, prefixes.as_deref()) && !present.contains(&r.key))
        .cloned()
        .collect();

    let mut rows_all = manifests.rows;
    let mut parsed_all = manifests.parsed;
    rows_all.extend(others.rows);
    parsed_all.extend(others.parsed);
    let cold_log = index.cold_log.clone();
    let removals = index
        .db
        .call(move |conn| {
            let tx = conn.transaction().map_err(|e| sql(&e))?;
            // Taken on the actor, so every store change written before this job is in it; a
            // later one is written after the rebuild commits.
            let replay = match mode {
                Mode::Cold => lock(&cold_log).take().unwrap_or_default(),
                Mode::Warm => Vec::new(),
            };
            let done = finish(
                &tx,
                mode,
                &rows_all,
                &parsed_all,
                &gone,
                &replay,
                whole.then_some(started),
            )
            .map_err(|e| sql(&e))?;
            tx.commit().map_err(|e| sql(&e))?;
            Ok(done)
        })
        .await?;
    (report.removed, report.missing) = removals;
    report.removed.sort();
    report.missing.sort();
    report.changed.sort();
    report.parked.sort();
    report.cloud_only.sort();
    report.needs_repair.sort();
    if whole {
        *scan = Some(started);
    }
    Ok(report)
}

/// The run's last transaction: a cold rebuild clears the tables, writes every row (asset rows
/// with their `expected` files, then the files), then replays the store's changes that landed
/// while it ran (`replay`); a warm one writes its rows compare-and-set and settles the rows of
/// files that are gone (`missing` when the suite still expects them, else deleted). Then the
/// whole scan's start, if it was one. Returns (removed, missing).
fn finish(
    tx: &Transaction<'_>,
    mode: Mode,
    rows: &[(DbFile, Option<DbFile>)],
    parsed: &[Parsed],
    gone: &[DbFile],
    replay: &[StoreChanges],
    scan_start: Option<i64>,
) -> rusqlite::Result<(Vec<WsPath>, Vec<WsPath>)> {
    let (mut removed, mut missing) = (Vec::new(), Vec::new());
    if mode == Mode::Cold {
        tx.execute_batch(
            "DELETE FROM files; DELETE FROM assets; DELETE FROM assets_fts;
             DELETE FROM edges; DELETE FROM expected;",
        )?;
        write_rows(tx, rows, parsed, false)?;
        for changes in replay {
            store_changes(tx, &changes.removed, &changes.rows, &changes.parsed)?;
        }
    } else {
        write_rows(tx, rows, parsed, true)?;
        for old in gone {
            if !unchanged_since(tx, &old.key, Some(old))? {
                continue;
            }
            if tracked(tx, &old.key, &old.path)? {
                if old.state != FileState::Missing {
                    tx.execute(
                        "UPDATE files SET state = 'missing' WHERE key = ?1",
                        params![old.key],
                    )?;
                    missing.push(WsPath::from_disk(old.path.clone()));
                }
            } else {
                tx.execute("DELETE FROM files WHERE key = ?1", params![old.key])?;
                removed.push(WsPath::from_disk(old.path.clone()));
            }
        }
    }
    if let Some(started) = scan_start {
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('scan_start', ?1)",
            params![started],
        )?;
    }
    Ok((removed, missing))
}

/// The store's changes in one transaction: removals (with everything under them), then rows,
/// then asset rows.
fn store_changes(
    tx: &Transaction<'_>,
    removed: &[String],
    rows: &[DbFile],
    parsed: &[Parsed],
) -> rusqlite::Result<()> {
    for key in removed {
        tx.execute(&format!("DELETE FROM files WHERE {UNDER}"), params![key])?;
        delete_assets_under(tx, key)?;
    }
    for row in rows {
        upsert_file(tx, row)?;
    }
    for p in parsed {
        upsert_asset(tx, p)?;
    }
    Ok(())
}

fn clone_judge(j: &Judge) -> Judge {
    Judge {
        probe: j.probe.clone(),
        stable: j.stable,
        fat: j.fat,
        prev_scan: j.prev_scan,
        rows: j.rows.clone(),
        known: j.known.clone(),
        trusted: j.trusted.clone(),
        recorded: j.recorded.clone(),
    }
}

// ---------------------------------------------------------------------------------------------
// The store's change events (ChangeSink).
// ---------------------------------------------------------------------------------------------

/// Applies the store's changes: written files take the store's hash and identity as they are;
/// a manifest is parsed; a removal (always the suite's own move, never `missing`) deletes the
/// rows of the path and everything under it, and the asset rows whose manifest went with it.
/// An event without an identity reconciles its path.
pub(super) async fn apply_events(index: &Index, events: &[StoreEvent]) -> Result<(), IndexError> {
    let mut rows: Vec<DbFile> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut rescan: Vec<WsPath> = Vec::new();
    for e in events {
        if let ChangeKind::MovedFrom(old) = &e.kind {
            removed.push(path_key(old.as_str()));
        }
        match (&e.kind, &e.identity) {
            (ChangeKind::Removed, _) => removed.push(path_key(e.path.as_str())),
            (_, Some(id)) if id.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 => {
                let verified = e.sha256.is_some();
                rows.push(DbFile::new(
                    &e.path,
                    id,
                    e.sha256.clone(),
                    FileState::Ok,
                    verified,
                ));
            }
            _ => rescan.push(e.path.clone()),
        }
    }
    // Manifests the store wrote are read once more to parse them (never hashed again).
    let root = index.root.clone();
    let probe = index.probe.clone();
    let manifests: Vec<(WsPath, i64)> = rows
        .iter()
        .map(|r| WsPath::from_disk(r.path.clone()))
        .zip(rows.iter().map(|r| r.last_write))
        .filter(|(p, _)| kind_of(p) == FileKind::Manifest)
        .collect();
    let parsed: Vec<Parsed> = if manifests.is_empty() {
        Vec::new()
    } else {
        blocking(move || {
            manifests
                .into_iter()
                .filter(|(rel, _)| is_folder_manifest(&root, rel))
                .filter_map(|(rel, last_write)| {
                    let mut file = probe.open_read(&rel.abs(&root)).ok()?;
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes).ok()?;
                    parse_manifest(&rel, &bytes, last_write)
                })
                .collect()
        })
        .await?
    };
    for p in &parsed {
        let key = path_key(p.manifest.as_str());
        if let Some(row) = rows.iter_mut().find(|r| r.key == key) {
            row.state = if p.needs_repair {
                FileState::NeedsRepair
            } else {
                FileState::Ok
            };
        }
    }
    if !rows.is_empty() || !removed.is_empty() {
        let changes = StoreChanges {
            removed,
            rows,
            parsed,
        };
        // Logged before it is written: a cold rebuild that commits after this write replays it,
        // and one that commits before it is followed by it.
        if let Some(log) = lock(&index.cold_log).as_mut() {
            log.push(changes.clone());
        }
        index
            .db
            .call(move |conn| {
                let tx = conn.transaction().map_err(|e| sql(&e))?;
                store_changes(&tx, &changes.removed, &changes.rows, &changes.parsed)
                    .map_err(|e| sql(&e))?;
                tx.commit().map_err(|e| sql(&e))
            })
            .await?;
    }
    if !rescan.is_empty() {
        index
            .reconcile_ordered(Scope::Paths(rescan), ReconcileOrder::AsWalked)
            .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Verification.
// ---------------------------------------------------------------------------------------------

/// [`Index::verify_pending`], under the scan lock. It examines at most `max_files` unverified
/// rows, in key order from where the last call stopped (wrapping round at the end), so a file
/// already reported (skipped until it changes) never starves the others; every examined row
/// counts, read or not.
pub(super) async fn verify(index: &Index, max_files: usize) -> Result<Vec<Mismatch>, IndexError> {
    if max_files == 0 {
        return Ok(Vec::new());
    }
    let from = lock(&index.verify_from).clone();
    let limit = i64::try_from(max_files).unwrap_or(i64::MAX);
    let pending: Vec<(DbFile, Option<String>)> = index
        .db
        .call(move |conn| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {}, e.sha256 FROM files f LEFT JOIN expected e ON e.key = f.key
                     WHERE f.verified = 0 AND f.state NOT IN ('cloud_only', 'missing')
                       AND f.key > ?1
                     ORDER BY f.key LIMIT ?2",
                    FILE_COLUMNS
                        .split(", ")
                        .map(|c| format!("f.{c}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .map_err(|e| sql(&e))?;
            stmt.query_map(params![from, limit], |r| {
                Ok((db_file(r)?, r.get::<_, Option<String>>(11)?))
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|e| sql(&e))
        })
        .await?;
    // A short batch reached the end: the next call starts from the first row again.
    *lock(&index.verify_from) = if pending.len() < max_files {
        String::new()
    } else {
        pending
            .last()
            .map(|(row, _)| row.key.clone())
            .unwrap_or_default()
    };
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let (_, fat) = volume_facts(index).await?;
    // A file already reported is hashed again only once it changes.
    let mismatched = lock(&index.mismatched).clone();

    let (probe, root, closed) = (
        index.probe.clone(),
        index.root.clone(),
        index.closed.clone(),
    );
    let checked = blocking(move || {
        let mut out = Vec::new();
        for (row, recorded) in pending {
            if closed.load(Ordering::Relaxed) {
                break;
            }
            let rel = WsPath::from_disk(row.path.clone());
            let abs = rel.abs(&root);
            let Ok(id) = probe.identity(&abs) else {
                continue;
            };
            if id.is_cloud_placeholder()
                || mismatched.get(&row.key) == Some(&(id.size, id.last_write, id.change_time))
            {
                continue;
            }
            match read_stable(probe.as_ref(), &abs, &id, false, !fat) {
                Ok(read) => out.push((row, recorded, read)),
                Err(ReadFail::Other(e)) => {
                    tracing::debug!(path = %rel, error = %e, "a file can't be verified now");
                }
                Err(_) => {}
            }
        }
        out
    })
    .await?;

    let mut mismatches = Vec::new();
    let mut updates: Vec<(DbFile, Option<DbFile>)> = Vec::new();
    for (row, recorded, read) in checked {
        // The manifest's hash first (ruling Q20: a row whose content differs from it stays
        // unverified until the watcher decides); else the hash the row was given.
        let trusted = recorded.or_else(|| row.sha256.clone());
        let matches = |want: &str| read.sha256 == want || read.lfs.as_deref() == Some(want);
        match trusted.as_deref() {
            Some(want) if !matches(want) => {
                lock(&index.mismatched).insert(
                    row.key.clone(),
                    (
                        read.after.size,
                        read.after.last_write,
                        read.after.change_time,
                    ),
                );
                mismatches.push(Mismatch {
                    path: WsPath::from_disk(row.path.clone()),
                    sha256: read.lfs.clone().unwrap_or_else(|| read.sha256.clone()),
                    recorded: want.to_string(),
                    lfs: read.lfs.is_some(),
                    identity: read.after,
                });
            }
            _ => {
                let (sha, state) = match (&read.lfs, trusted.as_deref()) {
                    (Some(oid), Some(want)) if oid == want => (oid.clone(), FileState::NotFetched),
                    (Some(oid), None) => (oid.clone(), FileState::NotFetched),
                    _ => (read.sha256.clone(), row.state),
                };
                let new = DbFile::new(
                    &WsPath::from_disk(row.path.clone()),
                    &read.after,
                    Some(sha),
                    state,
                    true,
                );
                updates.push((new, Some(row)));
            }
        }
    }
    if !updates.is_empty() {
        index
            .db
            .call(move |conn| {
                let tx = conn.transaction().map_err(|e| sql(&e))?;
                write_rows(&tx, &updates, &[], true).map_err(|e| sql(&e))?;
                tx.commit().map_err(|e| sql(&e))
            })
            .await?;
    }
    Ok(mismatches)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(rel: &str) -> WsPath {
        WsPath::parse(rel).unwrap()
    }

    #[test] // spec: CORE-041
    fn skips_store_names_and_private_folders() {
        assert!(skip_folder(".git", false));
        assert!(skip_folder(".GIT", true));
        assert!(skip_folder(".worldwright", true));
        assert!(!skip_folder(".worldwright", false));
        assert!(skip_folder(".Trash", true));
        assert!(skip_file(".a.png.ww-tmp-1-2"));
        assert!(skip_file(".a.png.ww-bak-1-2"));
        assert!(skip_file("a.png.ww-conflict-20270101T000000Z"));
        assert!(!skip_file("a.png"));
        assert!(skipped_path(".trash/x"));
        assert!(skipped_path("projects/.git/x"));
        assert!(!skipped_path("projects/p/x"));
    }

    #[test] // spec: CORE-041
    fn scope_prefixes_match_whole_components() {
        let prefixes = vec![path_key("notes/a")];
        assert!(in_scope(&path_key("notes/a"), Some(&prefixes)));
        assert!(in_scope(&path_key("notes/a/b.txt"), Some(&prefixes)));
        assert!(!in_scope(&path_key("notes/ab.txt"), Some(&prefixes)));
        assert!(in_scope("anything", None));
    }

    #[test] // spec: CORE-047, ARCH-079
    fn manifest_folder_is_authoritative() {
        let m = br#"{"asset_id":"crane","project":"p","type":"image","status":"candidate",
            "inputs":[{"ref":"lamp@1.0.0"},{"ref":"style@2"},{"ref":"bad"}],
            "hashes":{"v1.0.0/a.png":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            "candidate_runs":[{"run_id":"r_20270101_0800","status":"open","files":[{"path":"candidates/r_20270101_0800/c.png","role":"candidate"}]},
                              {"run_id":"r_20270102_0800","status":"open","trashed_utc":"2027-01-03T00:00:00Z","files":[{"path":"candidates/r_20270102_0800/t.png","role":"candidate"}]}]}"#;
        let ok = parse_manifest(&p("projects/p/assets/crane/crane.manifest.json"), m, 0).unwrap();
        assert!(!ok.needs_repair);
        let fields = ok.fields.unwrap();
        assert_eq!(
            fields.edges.len(),
            1,
            "style and unparsable links are no edges"
        );
        assert_eq!(fields.edges[0].to_asset, "lamp");
        let tracked: Vec<(&str, bool)> = fields
            .expected
            .iter()
            .map(|e| (e.path.rsplit('/').next().unwrap(), e.tracked))
            .collect();
        assert!(tracked.contains(&("a.png", true)));
        assert!(tracked.contains(&("c.png", true)));
        assert!(
            tracked.contains(&("t.png", false)),
            "a trashed run isn't expected"
        );

        let copied =
            parse_manifest(&p("projects/p/assets/crane2/crane.manifest.json"), m, 0).unwrap();
        assert!(copied.needs_repair);
        let moved =
            parse_manifest(&p("projects/q/assets/crane/crane.manifest.json"), m, 0).unwrap();
        assert!(moved.needs_repair);
        let broken =
            parse_manifest(&p("projects/p/assets/crane/crane.manifest.json"), b"{", 0).unwrap();
        assert!(broken.needs_repair && broken.fields.is_none());
        let legacy =
            parse_manifest(&p("projects/p/assets/crane/crane.manifest.json"), b"{}", 0).unwrap();
        assert!(
            !legacy.needs_repair,
            "a manifest without asset_id or project doesn't disagree"
        );
    }

    #[test] // spec: CORE-041
    fn filetimes_convert_to_unix_seconds() {
        assert_eq!(
            unix_secs((1_800_000_000 + EPOCH_GAP) * TICKS + 9_999_999),
            1_800_000_000
        );
        assert_eq!(unix_secs(EPOCH_GAP * TICKS), 0);
    }
}
