//! The index (CORE-014; ARCH-079, ARCH-089, CORE-005, CORE-041, CORE-047): a disposable view of
//! the workspace in `index.db`, which never holds anything only it knows.
//!
//! - [`schema`]: the tables, and the open that removes and recreates a damaged `index.db`;
//! - [`reconcile`]: the walk, the per-file decision (size, last write, FileId, ChangeTime),
//!   stable-snapshot hashing, parked reads, manifests parsed into `assets` and `edges`,
//!   `missing` files, the cold rebuild and the store's change events;
//! - [`query`]: `assets.query`'s keyset paging and its opaque cursor.
//!
//! One SQLite connection lives on its own thread (`ww-index-<id>`); callers send it jobs and
//! await the answer, as `state` does. Walking and hashing run on blocking threads, never on
//! that thread, so reads go on while a reconcile works. Reconciles, rebuilds and verification
//! run one at a time.
//!
//! The index reads file data only through `probe.open_read`, after the attribute-only
//! `probe.identity` has ruled out a cloud placeholder, so it never downloads one. It never
//! decides that a file is a human edit (CORE-010): [`Index::verify_pending`] reports files
//! whose bytes differ from their manifest's hash, and the watcher decides.

mod query;
mod reconcile;
mod schema;

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{mpsc, oneshot, watch};
use ww_platform::fs::{FileIdentity, Probe};
use ww_platform::{Clock, Env, SystemClock};
use ww_protocol::ErrorBody;
use ww_protocol::codes::STORE_IO;

use crate::store::{ChangeSink, StoreEvent, WsPath, path_key};

pub use query::{
    AssetPage, AssetQuery, Keyset, SortDir, SortField, decode_cursor, encode_cursor, query_hash,
};

/// `PRAGMA user_version` of `index.db`.
pub const INDEX_SCHEMA_VERSION: i64 = 1;

/// Paths parked after an unstable or locked read, at most (beyond that, the next rescan picks
/// them up).
pub const PARKED_MAX: usize = 4096;
/// A parked path still unstable after this many re-checks is logged and left for the next
/// rescan.
pub const PARKED_RETRIES: u32 = 5;
/// The jobs that may wait for the index actor.
const QUEUE: usize = 1024;

/// Why an index call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    Sql(String),
    Io(String),
    /// The index's actor has stopped (the workspace closed, or a job panicked).
    ActorGone,
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Sql(why) => write!(f, "the index database failed: {why}"),
            IndexError::Io(why) => f.write_str(why),
            IndexError::ActorGone => f.write_str("the index has stopped"),
        }
    }
}

impl std::error::Error for IndexError {}

impl IndexError {
    /// `store.io` (transient): the index is disposable, so a retry can succeed.
    pub fn to_error(&self, trace_id: &str) -> ErrorBody {
        ErrorBody::new(
            &STORE_IO,
            format!(
                "The workspace index can't answer ({self}); retry with the same idempotency key."
            ),
            trace_id,
        )
    }
}

fn sql(e: &rusqlite::Error) -> IndexError {
    IndexError::Sql(e.to_string())
}

/// What a reconcile looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    /// These files, or these folders with their whole subtrees.
    Paths(Vec<WsPath>),
}

/// A file's state in the index; [`FileState::as_str`] is its wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileState {
    Ok,
    /// A git-lfs pointer whose content isn't here (ARCH-085).
    NotFetched,
    /// A cloud placeholder, never read.
    CloudOnly,
    HumanEdit,
    NeedsRepair,
    /// Expected on disk and gone; its last SHA-256 is kept (ARCH-079, CORE-038).
    Missing,
}

impl FileState {
    /// `ok`, `not_fetched`, `cloud_only`, `human_edit`, `needs_repair` or `missing`.
    pub fn as_str(self) -> &'static str {
        match self {
            FileState::Ok => "ok",
            FileState::NotFetched => "not_fetched",
            FileState::CloudOnly => "cloud_only",
            FileState::HumanEdit => "human_edit",
            FileState::NeedsRepair => "needs_repair",
            FileState::Missing => "missing",
        }
    }

    fn parse(s: &str) -> FileState {
        match s {
            "not_fetched" => FileState::NotFetched,
            "cloud_only" => FileState::CloudOnly,
            "human_edit" => FileState::HumanEdit,
            "needs_repair" => FileState::NeedsRepair,
            "missing" => FileState::Missing,
            _ => FileState::Ok,
        }
    }
}

/// What a file is, from its path alone (`watch::reconcile` uses this one too, CORE-010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// `projects/<p>/assets/<a>/candidates/<run>/…`
    Candidate,
    /// `projects/<p>/assets/<a>/v<semver>/…`
    Sealed,
    /// `projects/<p>/assets/<a>/work/…`
    Work,
    /// `projects/<p>/assets/<a>/<name>.manifest.json`
    Manifest,
    Other,
}

/// The asset folder a path is in: `projects/<p>/assets/<a>/…` → `(p, a)`.
pub(crate) fn asset_of(path: &str) -> Option<(&str, &str)> {
    let mut parts = path.split('/');
    let (Some(projects), Some(project), Some(assets), Some(asset), Some(_)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    (projects.eq_ignore_ascii_case("projects") && assets.eq_ignore_ascii_case("assets"))
        .then_some((project, asset))
}

/// [`FileKind`] of `path`, from its components alone (names compare without regard to case).
pub fn kind_of(path: &WsPath) -> FileKind {
    if asset_of(path.as_str()).is_none() {
        return FileKind::Other;
    }
    let parts: Vec<&str> = path.as_str().split('/').collect();
    let fifth = parts[4];
    if parts.len() == 5 {
        return if fifth.to_ascii_lowercase().ends_with(".manifest.json") {
            FileKind::Manifest
        } else {
            FileKind::Other
        };
    }
    if fifth.eq_ignore_ascii_case("candidates") && parts.len() >= 7 {
        FileKind::Candidate
    } else if fifth.eq_ignore_ascii_case("work") {
        FileKind::Work
    } else if fifth
        .strip_prefix(['v', 'V'])
        .is_some_and(|v| v.parse::<ww_schema::Version>().is_ok())
    {
        FileKind::Sealed
    } else {
        FileKind::Other
    }
}

/// One file as the index knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub path: WsPath,
    pub size: u64,
    /// Bare hex: of the bytes, or the manifest's recorded hash until verified; for an LFS
    /// pointer, its `oid`.
    pub sha256: Option<String>,
    pub state: FileState,
    pub identity: Option<FileIdentity>,
}

/// One asset, keyed by its folder (`projects/<project>/assets/<asset_id>/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRow {
    pub project: String,
    pub asset_id: String,
    pub asset_type: Option<String>,
    pub status: Option<String>,
    pub version: Option<String>,
    pub approved_version: Option<String>,
    pub manifest: WsPath,
    pub creation_uuid: Option<String>,
    /// The manifest's last write as indexed, in Unix seconds.
    pub updated: i64,
    /// The manifest doesn't parse, or its `asset_id` or `project` disagrees with the folder.
    pub needs_repair: bool,
}

/// What a reconcile did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub unchanged: usize,
    /// Files whose bytes were read and hashed.
    pub hashed: usize,
    /// Files whose content is new or changed.
    pub changed: Vec<WsPath>,
    /// Rows deleted: files the suite no longer expects that are gone.
    pub removed: Vec<WsPath>,
    /// Expected files that are gone, now `missing`.
    pub missing: Vec<WsPath>,
    pub cloud_only: Vec<WsPath>,
    /// Manifests that don't parse or disagree with their folder.
    pub needs_repair: Vec<WsPath>,
    /// Unstable or locked reads, re-checked later.
    pub parked: Vec<WsPath>,
}

/// A file whose bytes differ from the hash its manifest records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub path: WsPath,
    /// What the file holds now (bare hex; an LFS pointer's `oid`).
    pub sha256: String,
    /// What the manifest records.
    pub recorded: String,
}

/// In which order a reconcile looks at files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOrder {
    AsWalked,
    /// Every manifest is re-parsed before any other file is looked at, so candidate and sealed
    /// files are judged against the new manifests.
    ManifestsFirst,
}

// ---------------------------------------------------------------------------------------------
// The actor.
// ---------------------------------------------------------------------------------------------

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// The connection's thread.
struct Db {
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    actor: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Db {
    fn start(mut conn: Connection, name: String) -> Result<Db, IndexError> {
        let (jobs, mut inbox) = mpsc::channel::<Job>(QUEUE);
        let actor = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                while let Some(job) = inbox.blocking_recv() {
                    if std::panic::catch_unwind(AssertUnwindSafe(|| job(&mut conn))).is_err() {
                        tracing::error!("the index actor panicked; the index is closed");
                        return;
                    }
                }
            })
            .map_err(|e| IndexError::Io(format!("cannot start the index actor: {e}")))?;
        Ok(Db {
            jobs: Mutex::new(Some(jobs)),
            actor: Mutex::new(Some(actor)),
        })
    }

    async fn call<T, F>(&self, f: F) -> Result<T, IndexError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, IndexError> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let _ = reply.send(f(conn));
        });
        let jobs = lock(&self.jobs).clone().ok_or(IndexError::ActorGone)?;
        jobs.send(job).await.map_err(|_| IndexError::ActorGone)?;
        drop(jobs);
        answer.await.map_err(|_| IndexError::ActorGone)?
    }

    fn close(&self) {
        drop(lock(&self.jobs).take());
        let actor = lock(&self.actor).take();
        if let Some(actor) = actor
            && actor.thread().id() != std::thread::current().id()
        {
            let _ = actor.join();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The index.
// ---------------------------------------------------------------------------------------------

/// A file's (size, last write, ChangeTime) as erify_pending saw it.
pub(super) type Seen = (u64, i64, Option<i64>);

/// A parked path and how often it was re-checked.
#[derive(Debug, Clone)]
struct ParkedPath {
    path: WsPath,
    retries: u32,
}

/// A workspace's index: one connection on its own thread.
pub struct Index {
    root: PathBuf,
    probe: Arc<dyn Probe>,
    clock: Arc<dyn Clock>,
    db: Db,
    path: Option<PathBuf>,
    /// The first reconcile is a cold rebuild (a new, rebuilt or in-memory index).
    cold: bool,
    ready: watch::Sender<bool>,
    /// Held by each reconcile, rebuild and verification: the last scan's start (FILETIME).
    scan: tokio::sync::Mutex<Option<i64>>,
    parked: Mutex<BTreeMap<String, ParkedPath>>,
    /// Files `verify_pending` found different, with the (size, last write, ChangeTime) it saw:
    /// not hashed again until one of them changes.
    mismatched: Mutex<HashMap<String, Seen>>,
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("root", &self.root)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Index {
    /// Opens `index.db` at `path` for the workspace folder `root` (ARCH-089). A damaged or
    /// mismatched file is removed and created afresh; then [`Index::needs_rebuild`] is true.
    ///
    /// # Errors
    /// [`IndexError::Io`] or [`IndexError::Sql`] when the file can't be opened or created.
    pub fn open(path: &Path, root: &Path, probe: Arc<dyn Probe>) -> Result<Index, IndexError> {
        let (conn, fresh) = schema::open_file(path)?;
        Index::start(conn, Some(path.to_path_buf()), fresh, root, probe)
    }

    /// The same schema over SQLite `:memory:`, with no file at all: a read-only workspace's
    /// index (ARCH-028, ARCH-097), cold-rebuilt at open.
    ///
    /// # Errors
    /// [`IndexError::Sql`] when SQLite can't create it.
    pub fn open_in_memory(root: &Path, probe: Arc<dyn Probe>) -> Result<Index, IndexError> {
        let conn = schema::open_memory()?;
        Index::start(conn, None, true, root, probe)
    }

    fn start(
        conn: Connection,
        path: Option<PathBuf>,
        cold: bool,
        root: &Path,
        probe: Arc<dyn Probe>,
    ) -> Result<Index, IndexError> {
        let scan_start: Option<i64> = conn
            .query_row("SELECT value FROM meta WHERE key = 'scan_start'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| sql(&e))?
            .flatten();
        let db = Db::start(conn, format!("ww-index-{}", Env::workspace_hash(root)))?;
        Ok(Index {
            root: root.to_path_buf(),
            probe,
            clock: Arc::new(SystemClock::new()),
            db,
            path,
            cold,
            ready: watch::Sender::new(false),
            scan: tokio::sync::Mutex::new(scan_start),
            parked: Mutex::new(BTreeMap::new()),
            mismatched: Mutex::new(HashMap::new()),
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// This index with `clock` as the clock of its scan start times (the 2-second rule), which
    /// are compared with file times: the system clock unless a test fakes both.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Index {
        self.clock = clock;
        self
    }

    /// The `index.db` file; `None` for an in-memory index.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the first reconcile must be a cold rebuild: the index is new, was rebuilt at
    /// open, or lives in memory.
    pub fn needs_rebuild(&self) -> bool {
        self.cold
    }

    /// Starts the first reconcile in the background, or the cold rebuild when
    /// [`Index::needs_rebuild`]; [`Index::ready`] resolves when it finishes, also when it
    /// failed (then the error is logged and the next reconcile tries again).
    pub fn start_first(self: &Arc<Self>) {
        let index = self.clone();
        tokio::spawn(async move {
            let done = if index.cold {
                index.rebuild().await
            } else {
                index.reconcile(Scope::All).await
            };
            match done {
                Ok(report) => tracing::info!(
                    root = %index.root.display(),
                    cold = index.cold,
                    hashed = report.hashed,
                    unchanged = report.unchanged,
                    "the first reconcile finished"
                ),
                Err(e) => {
                    tracing::warn!(root = %index.root.display(), error = %e, "the first reconcile failed")
                }
            }
            index.ready.send_replace(true);
        });
    }

    /// Resolves once the first reconcile (of the whole workspace) or cold rebuild has
    /// finished.
    pub async fn ready(&self) {
        let mut ready = self.ready.subscribe();
        let _ = ready.wait_for(|r| *r).await;
    }

    /// Stops the actor once its queued jobs have run (a blocking call); later calls are
    /// [`IndexError::ActorGone`] and a running walk stops early.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.db.close();
    }

    /// [`Index::reconcile_ordered`] with [`ReconcileOrder::ManifestsFirst`].
    ///
    /// # Errors
    /// [`IndexError`] when the database fails or the workspace can't be listed.
    pub async fn reconcile(&self, scope: Scope) -> Result<ReconcileReport, IndexError> {
        self.reconcile_ordered(scope, ReconcileOrder::ManifestsFirst)
            .await
    }

    /// Reconciles the files in `scope` with the rows (CORE-041; see [`reconcile`]).
    ///
    /// # Errors
    /// [`IndexError`] when the database fails or the workspace can't be listed.
    pub async fn reconcile_ordered(
        &self,
        scope: Scope,
        order: ReconcileOrder,
    ) -> Result<ReconcileReport, IndexError> {
        let whole = scope == Scope::All;
        let mut scan = self.scan.lock().await;
        let done = reconcile::run(self, &mut scan, scope, order, reconcile::Mode::Warm).await;
        if whole && done.is_ok() {
            self.ready.send_replace(true);
        }
        done
    }

    /// Clears the index and builds it again from the workspace: manifests are parsed, the
    /// hashes they record are trusted (unverified, for [`Index::verify_pending`]), and only
    /// files no manifest records are hashed.
    ///
    /// # Errors
    /// [`IndexError`] when the database fails or the workspace can't be listed.
    pub async fn rebuild(&self) -> Result<ReconcileReport, IndexError> {
        let mut scan = self.scan.lock().await;
        let done = reconcile::run(
            self,
            &mut scan,
            Scope::All,
            ReconcileOrder::ManifestsFirst,
            reconcile::Mode::Cold,
        )
        .await;
        if done.is_ok() {
            self.ready.send_replace(true);
        }
        done
    }

    /// Re-checks the parked paths. One still unstable after [`PARKED_RETRIES`] re-checks is
    /// logged and dropped from the list; the next rescan picks it up.
    ///
    /// # Errors
    /// [`IndexError`] when the database fails.
    pub async fn retry_parked(&self) -> Result<ReconcileReport, IndexError> {
        let paths: Vec<WsPath> = lock(&self.parked)
            .values()
            .map(|p| p.path.clone())
            .collect();
        if paths.is_empty() {
            return Ok(ReconcileReport::default());
        }
        let report = self
            .reconcile_ordered(Scope::Paths(paths.clone()), ReconcileOrder::AsWalked)
            .await?;
        let mut parked = lock(&self.parked);
        for path in paths {
            let key = path_key(path.as_str());
            if let Some(entry) = parked.get_mut(&key) {
                entry.retries += 1;
                if entry.retries >= PARKED_RETRIES {
                    tracing::warn!(path = %path, "a file stayed unstable through every re-check; the next rescan looks at it again");
                    parked.remove(&key);
                }
            }
        }
        Ok(report)
    }

    /// Hashes up to `max_files` unverified rows (hashes trusted from manifests, or rows the
    /// store reported without one) and returns the files whose bytes differ from the recorded
    /// hash, leaving them unverified. The others become verified.
    ///
    /// # Errors
    /// [`IndexError`] when the database fails.
    pub async fn verify_pending(&self, max_files: usize) -> Result<Vec<Mismatch>, IndexError> {
        let _scan = self.scan.lock().await;
        reconcile::verify(self, max_files).await
    }

    /// One page of assets, and the exact total, read in one transaction.
    ///
    /// # Errors
    /// [`IndexError::Sql`] when the database fails.
    pub async fn assets(&self, q: AssetQuery) -> Result<AssetPage, IndexError> {
        self.db
            .call(move |conn| query::page(conn, &q).map_err(|e| sql(&e)))
            .await
    }

    /// The asset in the folder `projects/<project>/assets/<asset_id>/`.
    ///
    /// # Errors
    /// [`IndexError::Sql`] when the database fails.
    pub async fn asset(
        &self,
        project: &str,
        asset_id: &str,
    ) -> Result<Option<AssetRow>, IndexError> {
        let (project, asset_id) = (project.to_string(), asset_id.to_string());
        self.db
            .call(move |conn| {
                conn.query_row(
                    &format!(
                        "SELECT {} FROM assets WHERE project = ?1 AND asset_id = ?2",
                        query::ASSET_COLUMNS
                    ),
                    params![project, asset_id],
                    query::asset_row,
                )
                .optional()
                .map_err(|e| sql(&e))
            })
            .await
    }

    /// The files of an asset folder, by path.
    ///
    /// # Errors
    /// [`IndexError::Sql`] when the database fails.
    pub async fn files_of(
        &self,
        project: &str,
        asset_id: &str,
    ) -> Result<Vec<FileRow>, IndexError> {
        let (project, asset_id) = (project.to_string(), asset_id.to_string());
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT {} FROM files WHERE project = ?1 AND asset_id = ?2 ORDER BY path",
                        reconcile::FILE_COLUMNS
                    ))
                    .map_err(|e| sql(&e))?;
                let rows = stmt
                    .query_map(params![project, asset_id], reconcile::db_file)
                    .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                    .map_err(|e| sql(&e))?;
                Ok(rows.into_iter().map(reconcile::DbFile::into_row).collect())
            })
            .await
    }

    /// The row of one file.
    ///
    /// # Errors
    /// [`IndexError::Sql`] when the database fails.
    pub async fn files_of_path(&self, path: &WsPath) -> Result<Option<FileRow>, IndexError> {
        let key = path_key(path.as_str());
        self.db
            .call(move |conn| {
                conn.query_row(
                    &format!(
                        "SELECT {} FROM files WHERE key = ?1",
                        reconcile::FILE_COLUMNS
                    ),
                    params![key],
                    reconcile::db_file,
                )
                .optional()
                .map(|row| row.map(reconcile::DbFile::into_row))
                .map_err(|e| sql(&e))
            })
            .await
    }

    /// The SHA-256 the asset's manifest records for `path` (`hashes`, or a run's
    /// `files[].sha256`), bare hex.
    ///
    /// # Errors
    /// [`IndexError::Sql`] when the database fails.
    pub async fn recorded_hash(&self, path: &WsPath) -> Result<Option<String>, IndexError> {
        let key = path_key(path.as_str());
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT sha256 FROM expected WHERE key = ?1",
                    params![key],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map(Option::flatten)
                .map_err(|e| sql(&e))
            })
            .await
    }

    /// The assets whose manifest `inputs` or `pinned_inputs` name this one, as
    /// `(project, asset_id)`, sorted.
    ///
    /// # Errors
    /// [`IndexError::Sql`] when the database fails.
    pub async fn dependents(
        &self,
        project: &str,
        asset_id: &str,
    ) -> Result<Vec<(String, String)>, IndexError> {
        let (project, asset_id) = (project.to_string(), asset_id.to_string());
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT DISTINCT from_project, from_asset FROM edges
                         WHERE to_project = ?1 AND to_asset = ?2
                         ORDER BY from_project, from_asset",
                    )
                    .map_err(|e| sql(&e))?;
                stmt.query_map(params![project, asset_id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                    .map_err(|e| sql(&e))
            })
            .await
    }

    /// Sets a file's state (the watcher records human edits and repairs this way).
    ///
    /// # Errors
    /// [`IndexError::Sql`] when the database fails.
    pub async fn set_state(&self, path: &WsPath, state: FileState) -> Result<(), IndexError> {
        let key = path_key(path.as_str());
        lock(&self.mismatched).remove(&key);
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE files SET state = ?2 WHERE key = ?1",
                    params![key, state.as_str()],
                )
                .map(drop)
                .map_err(|e| sql(&e))
            })
            .await
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        self.close();
    }
}

impl ChangeSink for Index {
    /// Takes the store's `sha256` and `identity` as they are, without hashing the file again;
    /// a manifest is read once more to parse it. An event without an identity (a restore, a
    /// recovery) reconciles that path.
    fn applied<'a>(
        &'a self,
        events: &'a [StoreEvent],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Err(e) = reconcile::apply_events(self, events).await {
                tracing::warn!(error = %e, "the index missed store changes; the next reconcile catches up");
            }
        })
    }
}
