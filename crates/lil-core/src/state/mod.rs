//! The core's SQLite databases (CORE-004, CORE-005, QA-012, QA-062): the suite `state.db` at
//! `Env::local_dir()\state.db` and one per workspace at `Env::workspace_local_dir(ws)\state.db`.
//!
//! **One actor per database.** Each [`StateDb`] owns a single `rusqlite::Connection` on its
//! own named thread (`lil-state-suite`, `lil-state-ws-<id>`); callers send it jobs and await the
//! replies. If the actor panics, `on_fatal("state")` is called once and every queued and later
//! call returns [`StateError::ActorGone`].
//!
//! **Bounded queue.** At most [`STATE_QUEUE`] jobs wait for an actor. A caller that finds the
//! queue full awaits room (an async wait, never a blocked tokio worker); the actor takes jobs
//! with `blocking_recv` on its own thread. The actor never waits for the runtime, so a full
//! queue always drains, and [`StateDb::close`] (which waits for the queued jobs) always ends.
//!
//! **Check before write.** An existing file with a hot rollback journal beside it (a crash
//! mid-write) is refused before SQLite sees it ([`StateError::HotJournal`]): any open would roll
//! the journal back. Otherwise the file is first opened read-only, `immutable=1` when there is
//! no `-wal` file and no other core can be writing it (never for a read-only workspace open,
//! whose lock holder may be live), so a file that is then refused never gets fresh `-wal` and
//! `-shm` files:
//! `PRAGMA quick_check` must say `ok` and `PRAGMA user_version` must not exceed the migrations
//! this build knows.
//! Only then is it opened read-write (WAL, `synchronous=FULL`, `busy_timeout=5000`,
//! `foreign_keys=ON`) and migrated, each migration in its own `BEGIN IMMEDIATE`. A newer or
//! damaged file is never modified, and `lil-core` never deletes, renames or truncates a
//! `state.db` (owner decision 5).
//!
//! **Stale rows.** On a read-write open, `running` idempotency rows (calls that died with an
//! earlier core) are deleted. For the suite database that is safe: it is per channel, and only
//! the core that owns the channel's pipe opens it (startup order: pipe, log, state). A
//! workspace database on another volume than the local folder,
//! `<vol>:\.lilith-local\<hash>\state.db`, is not per channel: the cores of every channel
//! that open that workspace share it (ARCH-097). So a workspace database is opened read-write
//! (and its rows released) only by the core that holds the workspace's ARCH-028 `core.lock`
//! ([`Access::ReadWrite`]); every other core opens it [`Access::ReadOnly`]: a read-only
//! connection that never migrates the file or releases a row. When there is no `state.db` on
//! disk yet (another channel's local folder), a read-only open gets an in-memory database with
//! the migrations applied, and nothing is created on disk.
//! A workspace database also empties its `hold_markers` on that open (APP-104: holds live in
//! memory, so none outlives its core).
//! `done` rows older than 30 days are pruned by `idem_begin`, against the caller's `now`: the
//! same clock that stamped the rows.
//!
//! **Workspace databases** are opened on first use, on a blocking thread
//! (`spawn_blocking`), each behind its own once-cell: opening one workspace's database never
//! holds up calls for another. An open whose thread starts after [`StateRegistry::close`] is
//! refused before it touches the file, and `close` waits for the opens already past that check.

mod migrations;

pub use migrations::{SUITE_MIGRATIONS, WORKSPACE_MIGRATIONS};

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use lil_platform::Env;
use lil_platform::sqlite_files;
use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension as _, TransactionBehavior, params,
};
use tokio::sync::{OnceCell, mpsc, oneshot};

use crate::store::fsops;
use crate::store::paths::path_key;
pub use crate::store::{Access, Pending};
use crate::store::{RowsFuture, StoreRows};

/// Called once when a database actor dies, with `"state"`.
pub type OnFatal = Arc<dyn Fn(&str) + Send + Sync>;

/// The jobs that may wait for one database actor; a caller past it awaits room.
pub const STATE_QUEUE: usize = 1024;

/// The [`StateDb::path`] of an in-memory database.
pub const IN_MEMORY: &str = ":memory:";

/// `PRAGMA busy_timeout`.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
/// `done` idempotency rows older than this are pruned by `idem_begin`.
const DONE_KEPT_SECS: i64 = 30 * 24 * 60 * 60;

/// Which kind of database: it decides the migrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateScope {
    Suite,
    Workspace,
}

impl StateScope {
    fn migrations(self) -> &'static [&'static str] {
        match self {
            StateScope::Suite => SUITE_MIGRATIONS,
            StateScope::Workspace => WORKSPACE_MIGRATIONS,
        }
    }

    fn thread_name(self) -> &'static str {
        match self {
            StateScope::Suite => "lil-state-suite",
            StateScope::Workspace => "lil-state-ws",
        }
    }
}

/// Why a database could not be opened or used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// `user_version` is greater than the migrations this build knows (QA-062).
    TooNew { found: i64, known: i64 },
    /// SQLite reports the file damaged or not a database.
    Corrupt(String),
    /// A hot rollback journal (`<file>-journal`, non-empty and not finished) lies beside the
    /// file: a crash mid-write. Nothing was opened, so nothing was changed; only a read-write
    /// open, which rolls the journal back, would change the file (`lil doctor` offers it).
    HotJournal,
    /// Anything else.
    Io(String),
    /// The database's actor has stopped (it panicked).
    ActorGone,
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::TooNew { found, known } => write!(
                f,
                "the database is from a newer version (schema {found}; this build knows {known})"
            ),
            StateError::Corrupt(why) => write!(f, "the database is damaged: {why}"),
            StateError::HotJournal => f.write_str(
                "the database was left mid-write by a crash (a hot rollback journal lies beside \
                 it); nothing was changed",
            ),
            StateError::Io(why) => f.write_str(why),
            StateError::ActorGone => f.write_str("the database actor has stopped"),
        }
    }
}

impl std::error::Error for StateError {}

/// SQLite's damage codes become [`StateError::Corrupt`], anything else [`StateError::Io`].
fn sql_error(e: &rusqlite::Error) -> StateError {
    match e.sqlite_error_code() {
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
            StateError::Corrupt(e.to_string())
        }
        _ => StateError::Io(e.to_string()),
    }
}

/// The prune of `idem_begin`: `done` rows created before `?1`. Migration 2's
/// `idempotency_prune` index serves it.
pub const PRUNE_DONE_SQL: &str =
    "DELETE FROM idempotency WHERE state = 'done' AND created_utc < ?1";

/// The confirmations' part of that prune: answered, expired and cancelled confirmations
/// created before `?1` (the `confirmations_state` index serves it).
pub const PRUNE_CONFIRMATIONS_SQL: &str =
    "DELETE FROM confirmations WHERE state <> 'pending' AND created_utc < ?1";

/// A resolved workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    /// 16 lowercase hex: the hash of the canonical path.
    pub id: String,
    /// The canonical path, without a `\\?\` prefix.
    pub path: PathBuf,
}

/// The store's rows in a workspace `state.db` (the store defines the trait, so it names no
/// `state` type: CORE-010).
impl StoreRows for StateDb {
    fn pending<'a>(&'a self, path: &'a str) -> RowsFuture<'a, Option<Pending>> {
        Box::pin(async move {
            StateDb::pending(self, path)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn pendings(&self) -> RowsFuture<'_, Vec<Pending>> {
        Box::pin(async move { StateDb::pendings(self).await.map_err(|e| e.to_string()) })
    }

    fn set_pending<'a>(&'a self, p: &'a Pending) -> RowsFuture<'a, ()> {
        Box::pin(async move {
            StateDb::set_pending(self, p)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn finish_pending<'a>(
        &'a self,
        path: &'a str,
        generated: Option<(&'a str, i64)>,
    ) -> RowsFuture<'a, ()> {
        Box::pin(async move {
            StateDb::finish_pending(self, path, generated)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn generated<'a>(&'a self, path: &'a str) -> RowsFuture<'a, Option<String>> {
        Box::pin(async move {
            StateDb::generated(self, path)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn set_generated<'a>(&'a self, path: &'a str, sha256: &'a str, utc: i64) -> RowsFuture<'a, ()> {
        Box::pin(async move {
            StateDb::set_generated(self, path, sha256, utc)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

/// A row of `hold_markers` (APP-104): `doc_id` was held by `holder` since `since_utc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldMarker {
    pub doc_id: String,
    pub path: String,
    /// The app (`ui:<app>`) or `core`.
    pub holder: String,
    pub since_utc: i64,
    /// Tells this hold's row from a later hold's on the same document.
    pub token: u64,
}

fn pending_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Pending> {
    Ok(Pending {
        path: r.get(0)?,
        old_sha256: r.get(1)?,
        new_sha256: r.get(2)?,
        started_utc: r.get(3)?,
    })
}

/// What `idem_begin` found (CORE-016).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdemBegin {
    /// No row: one in state `running` was inserted; run the call.
    Fresh,
    /// The same arguments finished before: the stored outcome JSON.
    Replay(String),
    /// The same arguments are running now.
    InProgress,
    /// The key was used with other arguments.
    Conflict,
}

/// The connection's settings, read through the actor (ruling P8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDiagnostics {
    /// `PRAGMA journal_mode`, lowercase.
    pub journal_mode: String,
    /// `PRAGMA synchronous`: 2 is FULL.
    pub synchronous: i64,
    pub busy_timeout_ms: i64,
    pub user_version: i64,
    /// The connection can't write (`sqlite3_db_readonly`): a read-only workspace open.
    pub read_only: bool,
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// One `state.db`: a handle to its actor thread. [`StateDb::close`], or dropping it, stops
/// the actor and closes the connection.
pub struct StateDb {
    path: PathBuf,
    schema_version: i64,
    /// `None` once closed.
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    actor: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for StateDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateDb")
            .field("path", &self.path)
            .field("schema_version", &self.schema_version)
            .finish_non_exhaustive()
    }
}

impl StateDb {
    /// Opens (creating it and its folder if needed), checks and migrates the database at
    /// `path`, then starts its actor. A newer or damaged existing file is left as it is.
    ///
    /// # Errors
    /// [`StateError::TooNew`], [`StateError::Corrupt`], or [`StateError::Io`] for anything
    /// else (the folder cannot be created, the file cannot be opened, ...).
    pub fn open(path: &Path, scope: StateScope, on_fatal: OnFatal) -> Result<StateDb, StateError> {
        StateDb::open_named(path, scope, scope.thread_name().to_string(), on_fatal)
    }

    fn open_named(
        path: &Path,
        scope: StateScope,
        thread_name: String,
        on_fatal: OnFatal,
    ) -> Result<StateDb, StateError> {
        let migrations = scope.migrations();
        let known = i64::try_from(migrations.len()).expect("a handful of migrations");
        if let Some(parent) = path.parent() {
            fsops::ensure_dir(parent)
                .map_err(|e| StateError::Io(format!("cannot create {}: {e}", parent.display())))?;
        }
        let exists = path
            .try_exists()
            .map_err(|e| StateError::Io(format!("cannot look at {}: {e}", path.display())))?;
        if exists {
            refuse_hot_journal(path)?;
            // Only this core may write the file (the suite's pipe owner, or the holder of the
            // workspace's `core.lock`), so no writer is live: `immutable=1` is safe here.
            check_existing(path, known, Quiet::Yes)?;
        }
        let mut conn = open_read_write(path)?;
        let schema_version = migrate(&mut conn, migrations).map_err(Migrate::into_state)?;
        release_running(&mut conn, scope).map_err(|e| sql_error(&e))?;
        StateDb::start(
            conn,
            path.to_path_buf(),
            schema_version,
            thread_name,
            on_fatal,
        )
    }

    /// Opens the database at `path` for a core that doesn't hold the workspace's `core.lock`
    /// (ARCH-028): an existing file is checked as [`StateDb::open`] checks it, then opened with
    /// a read-only connection. It is never migrated and no row is released. With no file (or
    /// one at an older schema, which only the lock holder may migrate) an in-memory database
    /// with the migrations applied stands in, and nothing is created on disk.
    ///
    /// # Errors
    /// [`StateError::TooNew`], [`StateError::Corrupt`] or [`StateError::Io`], as for
    /// [`StateDb::open`].
    pub fn open_read_only(
        path: &Path,
        scope: StateScope,
        on_fatal: OnFatal,
    ) -> Result<StateDb, StateError> {
        StateDb::open_read_only_named(path, scope, scope.thread_name().to_string(), on_fatal)
    }

    fn open_read_only_named(
        path: &Path,
        scope: StateScope,
        thread_name: String,
        on_fatal: OnFatal,
    ) -> Result<StateDb, StateError> {
        let migrations = scope.migrations();
        let known = i64::try_from(migrations.len()).expect("a handful of migrations");
        let exists = path
            .try_exists()
            .map_err(|e| StateError::Io(format!("cannot look at {}: {e}", path.display())))?;
        if !exists {
            tracing::info!(path = %path.display(), "no state.db for a read-only workspace; an in-memory one stands in");
            return StateDb::open_in_memory(scope, thread_name, on_fatal);
        }
        refuse_hot_journal(path)?;
        // Another core holds `core.lock` and may be writing right now: `immutable=1` would read
        // a torn view, so this check is a plain read-only open.
        check_existing(path, known, Quiet::No)?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| sql_error(&e))?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| sql_error(&e))?;
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|e| sql_error(&e))?;
        if version < known {
            tracing::info!(
                path = %path.display(),
                version,
                known,
                "a read-only core can't migrate this state.db; an in-memory one stands in"
            );
            drop(conn);
            return StateDb::open_in_memory(scope, thread_name, on_fatal);
        }
        StateDb::start(conn, path.to_path_buf(), version, thread_name, on_fatal)
    }

    /// An in-memory database with `scope`'s migrations applied; its [`StateDb::path`] is
    /// [`IN_MEMORY`].
    fn open_in_memory(
        scope: StateScope,
        thread_name: String,
        on_fatal: OnFatal,
    ) -> Result<StateDb, StateError> {
        let mut conn = Connection::open_in_memory().map_err(|e| sql_error(&e))?;
        let schema_version = migrate(&mut conn, scope.migrations()).map_err(Migrate::into_state)?;
        StateDb::start(
            conn,
            PathBuf::from(IN_MEMORY),
            schema_version,
            thread_name,
            on_fatal,
        )
    }

    /// Starts the actor that owns `conn`.
    fn start(
        conn: Connection,
        path: PathBuf,
        schema_version: i64,
        thread_name: String,
        on_fatal: OnFatal,
    ) -> Result<StateDb, StateError> {
        let (jobs, mut inbox) = mpsc::channel::<Job>(STATE_QUEUE);
        let actor = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_actor(conn, &mut inbox, &on_fatal))
            .map_err(|e| StateError::Io(format!("cannot start the database actor: {e}")))?;
        Ok(StateDb {
            path,
            schema_version,
            jobs: Mutex::new(Some(jobs)),
            actor: Mutex::new(Some(actor)),
        })
    }

    /// Stops the actor once the jobs already queued have run, and waits for it to close the
    /// connection (a blocking call). A call already waiting for room in a full queue still
    /// gets its job in and run; every later call returns [`StateError::ActorGone`]. Closing
    /// again does nothing.
    pub fn close(&self) {
        drop(lock(&self.jobs).take());
        let actor = lock(&self.actor).take();
        if let Some(actor) = actor
            && actor.thread().id() != std::thread::current().id()
        {
            let _ = actor.join();
        }
    }

    /// Whether [`StateDb::close`] has run.
    pub fn is_closed(&self) -> bool {
        lock(&self.jobs).is_none()
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `user_version` after the migrations ran: the number of migrations applied.
    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }

    /// The connection's journal mode, synchronous level, busy timeout and `user_version`.
    pub async fn diagnostics(&self) -> Result<StateDiagnostics, StateError> {
        self.call(|conn| {
            let read = || -> rusqlite::Result<StateDiagnostics> {
                let journal_mode: String =
                    conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
                Ok(StateDiagnostics {
                    journal_mode: journal_mode.to_ascii_lowercase(),
                    synchronous: conn.pragma_query_value(None, "synchronous", |r| r.get(0))?,
                    busy_timeout_ms: conn.pragma_query_value(None, "busy_timeout", |r| r.get(0))?,
                    user_version: conn.pragma_query_value(None, "user_version", |r| r.get(0))?,
                    read_only: conn.is_readonly(rusqlite::MAIN_DB)?,
                })
            };
            read().map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Looks up `(scope_key, key)`: no row → a `running` row is inserted ([`IdemBegin::Fresh`]);
    /// the same `args_hash` and `done` → [`IdemBegin::Replay`]; the same hash and `running` →
    /// [`IdemBegin::InProgress`]; another hash → [`IdemBegin::Conflict`].
    ///
    /// First, in the same transaction, `done` rows created more than 30 days before
    /// `now_utc_secs` are pruned, so rows and pruning use the caller's clock, and so are the
    /// confirmations of that age that are no longer pending (their history).
    pub async fn idem_begin(
        &self,
        scope_key: &str,
        key: &str,
        args_hash: &str,
        now_utc_secs: i64,
    ) -> Result<IdemBegin, StateError> {
        let (scope_key, key, args_hash) = (
            scope_key.to_string(),
            key.to_string(),
            args_hash.to_string(),
        );
        self.call(move |conn| {
            idem_begin(conn, &scope_key, &key, &args_hash, now_utc_secs).map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Ends the `running` row `(scope_key, key)`: `Some(outcome)` stores it as `done`; `None`
    /// deletes the row, so the key can be used again. A row that is not `running` is left as
    /// it is, and the call is [`StateError::Io`] ("no idempotency row …"), logged: the claim it
    /// ends was lost (another core released it), so the outcome was not stored.
    pub async fn idem_finish(
        &self,
        scope_key: &str,
        key: &str,
        outcome_json: Option<String>,
    ) -> Result<(), StateError> {
        let (scope_key, key) = (scope_key.to_string(), key.to_string());
        self.call(move |conn| {
            let changed = match outcome_json {
                Some(outcome) => conn.execute(
                    "UPDATE idempotency SET state = 'done', outcome = ?3
                     WHERE scope = ?1 AND key = ?2 AND state = 'running'",
                    params![scope_key, key, outcome],
                ),
                None => conn.execute(
                    "DELETE FROM idempotency
                     WHERE scope = ?1 AND key = ?2 AND state = 'running'",
                    params![scope_key, key],
                ),
            };
            match changed.map_err(|e| sql_error(&e))? {
                0 => {
                    tracing::error!(scope = %scope_key, key = %key, "no running idempotency row to finish");
                    Err(StateError::Io(format!(
                        "no idempotency row ({scope_key}, {key}) is running, so its outcome was \
                         not recorded"
                    )))
                }
                _ => Ok(()),
            }
        })
        .await
    }

    /// Takes the `done` row `(scope_key, key)` back for another run (CORE-091, Appendix F: a
    /// retry with the token of an answered confirmation): when its arguments hash is
    /// `args_hash` and its stored outcome is a `pending_confirmation`, the row becomes
    /// `running` again, with no outcome, and this returns `true`. Anything else (no such row,
    /// another hash, a row already taken back by a concurrent retry, any other outcome) changes
    /// nothing and returns `false`. One statement, so of two concurrent retries exactly one
    /// gets the row. The caller then finishes it like a fresh claim (`idem_finish`).
    pub async fn idem_reclaim(
        &self,
        scope_key: &str,
        key: &str,
        args_hash: &str,
    ) -> Result<bool, StateError> {
        let (scope_key, key, args_hash) = (
            scope_key.to_string(),
            key.to_string(),
            args_hash.to_string(),
        );
        self.call(move |conn| {
            conn.execute(
                "UPDATE idempotency SET state = 'running', outcome = NULL
                 WHERE scope = ?1 AND key = ?2 AND args_hash = ?3 AND state = 'done'
                   AND json_valid(outcome)
                   AND json_extract(outcome, '$.status') = 'pending_confirmation'",
                params![scope_key, key, args_hash],
            )
            .map(|changed| changed == 1)
            .map_err(|e| sql_error(&e))
        })
        .await
    }

    /// The last hash the store wrote to `path` (workspace databases; ARCH-078).
    pub async fn generated(&self, path: &str) -> Result<Option<String>, StateError> {
        let path = path.to_string();
        self.call(move |conn| {
            conn.query_row(
                "SELECT sha256 FROM generated WHERE key = ?1",
                params![path_key(&path)],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Records `sha256` as the last hash the store wrote to `path`, at `utc` (Unix seconds).
    pub async fn set_generated(
        &self,
        path: &str,
        sha256: &str,
        utc: i64,
    ) -> Result<(), StateError> {
        let (path, sha256) = (path.to_string(), sha256.to_string());
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO generated (key, path, sha256, written_utc) VALUES (?4, ?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET path = ?1, sha256 = ?2, written_utc = ?3",
                params![path, sha256, utc, path_key(&path)],
            )
            .map(drop)
            .map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Records (or replaces) the pending replace of `p.path` (write step 5).
    pub async fn set_pending(&self, p: &Pending) -> Result<(), StateError> {
        let p = p.clone();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO generated_pending (key, path, old_sha256, new_sha256, started_utc)
                 VALUES (?5, ?1, ?2, ?3, ?4)
                 ON CONFLICT(key) DO UPDATE SET path = ?1, old_sha256 = ?2, new_sha256 = ?3,
                     started_utc = ?4",
                params![
                    p.path,
                    p.old_sha256,
                    p.new_sha256,
                    p.started_utc,
                    path_key(&p.path)
                ],
            )
            .map(drop)
            .map_err(|e| sql_error(&e))
        })
        .await
    }

    /// The pending replace of `path`, if one is recorded.
    pub async fn pending(&self, path: &str) -> Result<Option<Pending>, StateError> {
        let path = path.to_string();
        self.call(move |conn| {
            conn.query_row(
                "SELECT path, old_sha256, new_sha256, started_utc FROM generated_pending
                 WHERE key = ?1",
                params![path_key(&path)],
                pending_row,
            )
            .optional()
            .map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Every pending replace, by path.
    pub async fn pendings(&self) -> Result<Vec<Pending>, StateError> {
        self.call(|conn| {
            let read = || -> rusqlite::Result<Vec<Pending>> {
                conn.prepare(
                    "SELECT path, old_sha256, new_sha256, started_utc FROM generated_pending
                     ORDER BY path",
                )?
                .query_map([], pending_row)?
                .collect()
            };
            read().map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Write step 8, in one transaction: clears the pending row of `path` and, when
    /// `generated` is given, records `(sha256, utc)` as its last generated hash.
    pub async fn finish_pending(
        &self,
        path: &str,
        generated: Option<(&str, i64)>,
    ) -> Result<(), StateError> {
        let path = path.to_string();
        let generated = generated.map(|(sha, utc)| (sha.to_string(), utc));
        self.call(move |conn| {
            let run = || -> rusqlite::Result<()> {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let key = path_key(&path);
                tx.execute("DELETE FROM generated_pending WHERE key = ?1", params![key])?;
                if let Some((sha, utc)) = generated {
                    tx.execute(
                        "INSERT INTO generated (key, path, sha256, written_utc)
                         VALUES (?4, ?1, ?2, ?3)
                         ON CONFLICT(key) DO UPDATE SET path = ?1, sha256 = ?2, written_utc = ?3",
                        params![path, sha, utc, key],
                    )?;
                }
                tx.commit()
            };
            run().map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Records that `m.doc_id` is held (replacing an earlier marker of the document).
    pub async fn set_hold_marker(&self, m: &HoldMarker) -> Result<(), StateError> {
        let m = m.clone();
        self.call(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO hold_markers (doc_id, path, holder, since_utc, token)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    m.doc_id,
                    m.path,
                    m.holder,
                    m.since_utc,
                    i64::try_from(m.token).unwrap_or(i64::MAX)
                ],
            )
            .map(drop)
            .map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Removes the marker of `doc_id` if it is hold `token`'s: the marker of a later hold on
    /// the same document stays.
    pub async fn clear_hold_marker(&self, doc_id: &str, token: u64) -> Result<(), StateError> {
        let doc_id = doc_id.to_string();
        let token = i64::try_from(token).unwrap_or(i64::MAX);
        self.call(move |conn| {
            conn.execute(
                "DELETE FROM hold_markers WHERE doc_id = ?1 AND token = ?2",
                params![doc_id, token],
            )
            .map(drop)
            .map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Every hold marker, by document id.
    pub async fn hold_markers(&self) -> Result<Vec<HoldMarker>, StateError> {
        self.call(|conn| {
            let read = || -> rusqlite::Result<Vec<HoldMarker>> {
                conn.prepare(
                    "SELECT doc_id, path, holder, since_utc, token FROM hold_markers ORDER BY doc_id",
                )?
                .query_map([], |r| {
                    Ok(HoldMarker {
                        doc_id: r.get(0)?,
                        path: r.get(1)?,
                        holder: r.get(2)?,
                        since_utc: r.get(3)?,
                        token: u64::try_from(r.get::<_, i64>(4)?).unwrap_or_default(),
                    })
                })?
                .collect()
            };
            read().map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Makes the actor panic. Used only by the test-channel tool `test.state_panic` (QA-012).
    pub async fn panic_actor(&self) {
        let Some(jobs) = lock(&self.jobs).clone() else {
            return;
        };
        let _ = jobs
            .send(Box::new(|_| {
                panic!("the state actor was asked to panic (test.state_panic)")
            }))
            .await;
    }

    /// Runs `f` on the actor's connection (the `jobs` module keeps its rows through it).
    pub(crate) async fn call<T, F>(&self, f: F) -> Result<T, StateError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StateError> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let _ = reply.send(f(conn));
        });
        // A clone, so the lock is not held across the wait for room; it is dropped as soon as
        // the job is in, so it never keeps a closed actor running.
        let jobs = lock(&self.jobs).clone().ok_or(StateError::ActorGone)?;
        jobs.send(job).await.map_err(|_| StateError::ActorGone)?;
        drop(jobs);
        answer.await.map_err(|_| StateError::ActorGone)?
    }
}

impl Drop for StateDb {
    /// Stops the actor and waits for it to close the connection.
    fn drop(&mut self) {
        self.close();
    }
}

/// Locks `m`; its value (an `Option` taken at most once) stays usable after a panic.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The actor: runs jobs in order until every sender is gone. A panicking job ends it:
/// `on_fatal("state")` once, then the inbox is dropped, so every queued and later call gets
/// [`StateError::ActorGone`] (a caller waiting for room too).
fn run_actor(mut conn: Connection, inbox: &mut mpsc::Receiver<Job>, on_fatal: &OnFatal) {
    while let Some(job) = inbox.blocking_recv() {
        if std::panic::catch_unwind(AssertUnwindSafe(|| job(&mut conn))).is_err() {
            tracing::error!(
                thread = std::thread::current().name().unwrap_or("?"),
                "the state actor panicked; its database is closed"
            );
            on_fatal("state");
            return;
        }
    }
}

/// Step 1 of the open order: an existing file with a hot rollback journal beside it is
/// refused before SQLite sees it, since any open would roll the journal back.
/// A journal that can't be read is refused too (fail closed).
fn refuse_hot_journal(path: &Path) -> Result<(), StateError> {
    match sqlite_files::has_hot_journal(path) {
        Ok(false) => Ok(()),
        Ok(true) => Err(StateError::HotJournal),
        Err(e) => Err(StateError::Io(format!(
            "the rollback journal beside {} can't be read: {e}",
            path.display()
        ))),
    }
}

/// Whether a database file may have a live writer while it is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quiet {
    /// No other process may write it: `immutable=1` is safe.
    Yes,
    /// Another core may be writing it: never `immutable=1`.
    No,
}

/// Step 2 of the open order: an existing file, read-only (`sqlite_files::open_untouched`).
/// `quick_check` must say `ok`, and `user_version` must be between 0 and `known`.
fn check_existing(path: &Path, known: i64, quiet: Quiet) -> Result<(), StateError> {
    let conn =
        sqlite_files::open_untouched(path, quiet == Quiet::Yes).map_err(|e| sql_error(&e))?;
    let checked = (|| -> rusqlite::Result<(Vec<String>, i64)> {
        let report = conn
            .prepare("PRAGMA quick_check")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let found = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok((report, found))
    })();
    conn.close().map_err(|(_, e)| sql_error(&e))?;
    let (report, found) = checked.map_err(|e| sql_error(&e))?;
    if report != ["ok"] {
        return Err(StateError::Corrupt(report.join("; ")));
    }
    if found > known {
        return Err(StateError::TooNew { found, known });
    }
    if found < 0 {
        return Err(StateError::Corrupt(format!(
            "user_version {found} is not a schema version"
        )));
    }
    Ok(())
}

/// Step 3: read-write (created if new), WAL, `synchronous=FULL`, `busy_timeout=5000`,
/// `foreign_keys=ON`.
fn open_read_write(path: &Path) -> Result<Connection, StateError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| sql_error(&e))?;
    let configure = || -> rusqlite::Result<String> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(mode)
    };
    let mode = configure().map_err(|e| sql_error(&e))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StateError::Io(format!(
            "{} stays in journal mode {mode}, not wal",
            path.display()
        )));
    }
    Ok(conn)
}

enum Migrate {
    State(StateError),
    Sql(rusqlite::Error),
}

impl Migrate {
    fn into_state(self) -> StateError {
        match self {
            Migrate::State(e) => e,
            Migrate::Sql(e) => sql_error(&e),
        }
    }
}

impl From<rusqlite::Error> for Migrate {
    fn from(e: rusqlite::Error) -> Migrate {
        Migrate::Sql(e)
    }
}

/// Step 4: applies the missing migrations, each in its own `BEGIN IMMEDIATE` that re-reads
/// `user_version`. Returns the final `user_version`.
fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<i64, Migrate> {
    let known = i64::try_from(migrations.len()).expect("a handful of migrations");
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > known {
            return Err(Migrate::State(StateError::TooNew {
                found: version,
                known,
            }));
        }
        let Ok(next) = usize::try_from(version) else {
            return Err(Migrate::State(StateError::Corrupt(format!(
                "user_version {version} is not a schema version"
            ))));
        };
        let Some(migration) = migrations.get(next) else {
            // Up to date; the transaction wrote nothing and rolls back.
            return Ok(version);
        };
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
        tracing::info!(version = version + 1, "state.db migrated");
    }
}

/// Deletes the `running` rows of calls that died with an earlier core and, in a workspace
/// database, the hold markers of its holds (APP-104: holds live in memory, so none outlives
/// its core).
fn release_running(conn: &mut Connection, scope: StateScope) -> rusqlite::Result<()> {
    let released = conn.execute("DELETE FROM idempotency WHERE state = 'running'", [])?;
    if released > 0 {
        tracing::info!(
            released,
            "running idempotency rows of an earlier core released"
        );
    }
    if scope == StateScope::Workspace {
        let cleared = conn.execute("DELETE FROM hold_markers", [])?;
        if cleared > 0 {
            tracing::info!(cleared, "hold markers of an earlier core cleared");
        }
    }
    Ok(())
}

fn idem_begin(
    conn: &mut Connection,
    scope_key: &str,
    key: &str,
    args_hash: &str,
    now_utc_secs: i64,
) -> rusqlite::Result<IdemBegin> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pruned = tx.execute(
        PRUNE_DONE_SQL,
        params![now_utc_secs.saturating_sub(DONE_KEPT_SECS)],
    )?;
    if pruned > 0 {
        tracing::info!(pruned, "idempotency rows older than 30 days pruned");
    }
    // The confirmations' history goes with them; a pending one is kept (it expires first).
    let pruned = tx.execute(
        PRUNE_CONFIRMATIONS_SQL,
        params![now_utc_secs.saturating_sub(DONE_KEPT_SECS)],
    )?;
    if pruned > 0 {
        tracing::info!(pruned, "confirmations older than 30 days pruned");
    }
    let row: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT args_hash, state, outcome FROM idempotency WHERE scope = ?1 AND key = ?2",
            params![scope_key, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let begin = match row {
        None => {
            tx.execute(
                "INSERT INTO idempotency (scope, key, args_hash, state, created_utc)
                 VALUES (?1, ?2, ?3, 'running', ?4)",
                params![scope_key, key, args_hash, now_utc_secs],
            )?;
            IdemBegin::Fresh
        }
        Some((hash, _, _)) if hash != args_hash => IdemBegin::Conflict,
        Some((_, state, outcome)) if state == "done" => {
            IdemBegin::Replay(outcome.unwrap_or_default())
        }
        Some(_) => IdemBegin::InProgress,
    };
    tx.commit()?;
    Ok(begin)
}

/// The suite database, opened at start, and the workspace databases, opened on first use.
pub struct StateRegistry {
    env: Env,
    on_fatal: OnFatal,
    suite: Arc<StateDb>,
    /// The workspace databases and the closed flag, under one lock.
    workspaces: Arc<Mutex<Workspaces>>,
    /// Held shared by each blocking workspace open from its closed check until its database
    /// is open; [`StateRegistry::close`] takes it exclusively once it has set the flag, so no
    /// open is still migrating or releasing rows when `close` returns.
    opening: Arc<RwLock<()>>,
}

/// One cell per workspace id and access: the lock is held only to find a cell or to close.
#[derive(Default)]
struct Workspaces {
    cells: HashMap<(String, Access), Arc<OnceCell<Arc<StateDb>>>>,
    /// Set by [`StateRegistry::close`]: no database is opened any more.
    closed: bool,
}

impl std::fmt::Debug for StateRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateRegistry")
            .field("suite", &self.suite)
            .finish_non_exhaustive()
    }
}

impl StateRegistry {
    /// Opens the suite `state.db` at `env.suite_state_db()` (CORE-005).
    ///
    /// # Errors
    /// As [`StateDb::open`].
    pub fn open(env: &Env, on_fatal: OnFatal) -> Result<StateRegistry, StateError> {
        let suite = StateDb::open_named(
            &env.suite_state_db(),
            StateScope::Suite,
            StateScope::Suite.thread_name().to_string(),
            on_fatal.clone(),
        )?;
        Ok(StateRegistry {
            env: env.clone(),
            on_fatal,
            suite: Arc::new(suite),
            workspaces: Arc::new(Mutex::new(Workspaces::default())),
            opening: Arc::new(RwLock::new(())),
        })
    }

    /// Closes every database: the suite one and each workspace one opened so far, after the
    /// jobs already queued on them have run (a blocking call). Later calls on them return
    /// [`StateError::ActorGone`], and no workspace database is opened any more. The core
    /// calls it at the end of its shutdown, before it releases its pipe, so a new core that
    /// claims the pipe never finds this one still writing (ARCH-021).
    pub fn close(&self) {
        // The flag and the cells are read under one lock: see `for_workspace`.
        let cells: Vec<_> = {
            let mut workspaces = lock(&self.workspaces);
            workspaces.closed = true;
            workspaces.cells.values().cloned().collect()
        };
        // Waits for the opens that passed their check before the flag was set: they finish
        // (migrations, stale rows) now, not after this returns. Later ones see the flag.
        drop(self.opening.write().unwrap_or_else(PoisonError::into_inner));
        self.suite.close();
        for cell in cells {
            if let Some(db) = cell.get() {
                db.close();
            }
        }
    }

    /// The suite database.
    pub fn suite(&self) -> &StateDb {
        &self.suite
    }

    /// The suite database, shared: a job without a workspace holds it (`JobCtx::state`).
    pub fn suite_db(&self) -> Arc<StateDb> {
        self.suite.clone()
    }

    /// The database of `ws` at `Env::workspace_local_dir(ws)\state.db` (CORE-005), opened on
    /// first use and then cached per access: one actor per file and access (QA-012).
    ///
    /// [`Access::ReadWrite`] (the caller holds `core.lock`) checks, migrates and releases the
    /// stale `running` rows; [`Access::ReadOnly`] is [`StateDb::open_read_only`], which changes
    /// nothing and creates nothing (ARCH-028).
    ///
    /// The open (folder, `quick_check`, migrations) runs on a blocking thread behind the
    /// workspace's own once-cell, so concurrent callers for the same workspace wait for one
    /// open and other workspaces are not held up. A failed open is retried by the next call.
    ///
    /// # Errors
    /// As [`StateDb::open`]; [`StateError::Io`] if the workspace's local folder cannot be
    /// worked out or the blocking open did not finish; [`StateError::ActorGone`] once the
    /// registry is closed.
    pub async fn for_workspace(
        &self,
        ws: &WorkspaceRef,
        access: Access,
    ) -> Result<Arc<StateDb>, StateError> {
        let cell = {
            let mut workspaces = lock(&self.workspaces);
            if workspaces.closed {
                return Err(StateError::ActorGone);
            }
            workspaces
                .cells
                .entry((ws.id.clone(), access))
                .or_default()
                .clone()
        };
        let db = cell
            .get_or_try_init(|| {
                let (env, on_fatal, ws) = (self.env.clone(), self.on_fatal.clone(), ws.clone());
                let (workspaces, opening) = (self.workspaces.clone(), self.opening.clone());
                async move {
                    tokio::task::spawn_blocking(move || {
                        open_workspace_db(&env, on_fatal, &ws, access, &workspaces, &opening)
                    })
                    .await
                    .map_err(|e| {
                        StateError::Io(format!("opening the workspace database stopped: {e}"))
                    })?
                }
            })
            .await?;
        // A database opened while `close` ran. The cell was filled before this takes the lock,
        // and `close` sets the flag and copies the cells under the same lock: if `close` came
        // first this sees the flag and closes the database; otherwise `close` finds it in its
        // cell. Either way none stays open.
        let closed = lock(&self.workspaces).closed;
        if closed {
            db.close();
            return Err(StateError::ActorGone);
        }
        Ok(db.clone())
    }

    /// The read-write database of the workspace `id`, if it is open (a call in the workspace
    /// opened it): the confirmations of that scope live there.
    pub fn workspace_db(&self, id: &str) -> Option<Arc<StateDb>> {
        let workspaces = lock(&self.workspaces);
        if workspaces.closed {
            return None;
        }
        workspaces
            .cells
            .get(&(id.to_string(), Access::ReadWrite))
            .and_then(|cell| cell.get().cloned())
    }

    /// The writable databases open now: the suite one and each read-write workspace one,
    /// with the workspace id (`"none"` for the suite).
    pub fn writable_dbs(&self) -> Vec<(String, Arc<StateDb>)> {
        let mut dbs = vec![(lil_protocol::NO_WORKSPACE.to_string(), self.suite.clone())];
        let workspaces = lock(&self.workspaces);
        if workspaces.closed {
            return dbs;
        }
        for ((id, access), cell) in &workspaces.cells {
            if *access == Access::ReadWrite
                && let Some(db) = cell.get()
            {
                dbs.push((id.clone(), db.clone()));
            }
        }
        dbs
    }

    /// Closes the databases of the workspace `id` (each access), after their queued jobs, and
    /// forgets them, so a later [`StateRegistry::for_workspace`] opens the file again (a
    /// blocking call). The workspace hub calls it when it closes a workspace, before it lets
    /// go of `core.lock`.
    pub fn close_workspace(&self, id: &str) {
        let cells: Vec<_> = {
            let mut workspaces = lock(&self.workspaces);
            [Access::ReadWrite, Access::ReadOnly]
                .into_iter()
                .filter_map(|access| workspaces.cells.remove(&(id.to_string(), access)))
                .collect()
        };
        for cell in cells {
            if let Some(db) = cell.get() {
                db.close();
            }
        }
    }
}

/// The blocking part of [`StateRegistry::for_workspace`]: opens `ws`'s database. Its thread
/// may run only after [`StateRegistry::close`] has returned, so it checks the closed flag
/// first, under the workspaces lock as `close` sets it, and is then refused before it creates,
/// migrates or releases anything. It holds `opening` shared until the database is open.
fn open_workspace_db(
    env: &Env,
    on_fatal: OnFatal,
    ws: &WorkspaceRef,
    access: Access,
    workspaces: &Mutex<Workspaces>,
    opening: &RwLock<()>,
) -> Result<Arc<StateDb>, StateError> {
    let _opening = opening.read().unwrap_or_else(PoisonError::into_inner);
    if lock(workspaces).closed {
        return Err(StateError::ActorGone);
    }
    let dir = env
        .workspace_local_dir_for_canonical(&ws.path)
        .map_err(|e| StateError::Io(e.to_string()))?;
    let path = dir.join("state.db");
    let thread = format!("lil-state-ws-{}", ws.id);
    match access {
        Access::ReadWrite => StateDb::open_named(&path, StateScope::Workspace, thread, on_fatal),
        Access::ReadOnly => {
            StateDb::open_read_only_named(&path, StateScope::Workspace, thread, on_fatal)
        }
    }
    .map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test] // spec: QA-012
    fn actor_panic_reports_fatal_once() {
        let dir = tempfile::tempdir().unwrap();
        let reports = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = reports.clone();
        let on_fatal: OnFatal = Arc::new(move |what: &str| seen.lock().unwrap().push(what.into()));
        let db = StateDb::open(&dir.path().join("state.db"), StateScope::Suite, on_fatal).unwrap();
        let rt = runtime();

        // A call queued behind the panic, and calls made after it, all fail with ActorGone.
        let pending = rt.block_on(async {
            db.panic_actor().await;
            db.idem_begin("cli|none", "k", "h", 0).await
        });
        assert_eq!(pending, Err(StateError::ActorGone));
        rt.block_on(db.panic_actor());
        for _ in 0..3 {
            assert_eq!(
                rt.block_on(db.idem_finish("cli|none", "k", None)),
                Err(StateError::ActorGone)
            );
        }
        assert_eq!(rt.block_on(db.diagnostics()), Err(StateError::ActorGone));
        drop(db);
        assert_eq!(*reports.lock().unwrap(), ["state"]);
    }

    #[test] // spec: CORE-004
    fn fts5_and_json1_are_available() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let on_fatal: OnFatal = Arc::new(move |_: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let db = StateDb::open(&dir.path().join("state.db"), StateScope::Suite, on_fatal).unwrap();
        let json = runtime()
            .block_on(db.call(|conn| {
                let run = || -> rusqlite::Result<String> {
                    conn.execute_batch(
                        "CREATE VIRTUAL TABLE probe USING fts5(body);
                         INSERT INTO probe (body) VALUES ('Ünïcödé space');",
                    )?;
                    let hits: i64 = conn.query_row(
                        "SELECT count(*) FROM probe WHERE probe MATCH 'space'",
                        [],
                        |r| r.get(0),
                    )?;
                    assert_eq!(hits, 1);
                    conn.query_row("SELECT json('{}')", [], |r| r.get(0))
                };
                run().map_err(|e| sql_error(&e))
            }))
            .unwrap();
        assert_eq!(json, "{}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A database whose actor is held inside a job until the returned sender is used, with
    /// [`STATE_QUEUE`] more jobs queued behind it (each counts itself in the returned counter).
    fn held_and_full(db: &StateDb) -> (std::sync::mpsc::Sender<()>, Arc<AtomicUsize>) {
        let jobs = lock(&db.jobs).clone().expect("the database is open");
        assert_eq!(jobs.max_capacity(), STATE_QUEUE);
        let (started_tx, started) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel::<()>();
        jobs.try_send(Box::new(move |_| {
            started_tx.send(()).unwrap();
            let _ = held.recv();
        }))
        .unwrap_or_else(|_| panic!("the empty queue refused a job"));
        started.recv().expect("the actor took the holding job");
        let ran = Arc::new(AtomicUsize::new(0));
        for n in 0..STATE_QUEUE {
            let ran = ran.clone();
            jobs.try_send(Box::new(move |_| {
                ran.fetch_add(1, Ordering::SeqCst);
            }))
            .unwrap_or_else(|_| panic!("the queue was full after {n} jobs"));
        }
        let full = jobs.try_send(Box::new(|_| {}));
        assert!(
            matches!(full, Err(mpsc::error::TrySendError::Full(_))),
            "the queue takes more than {STATE_QUEUE} jobs"
        );
        (release, ran)
    }

    #[test] // spec: QA-012
    fn state_queue_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let on_fatal: OnFatal = Arc::new(|_: &str| {});
        let db = StateDb::open(&dir.path().join("state.db"), StateScope::Suite, on_fatal).unwrap();
        let (release, ran) = held_and_full(&db);

        // A caller with a full queue waits for room; it neither fails nor blocks its thread.
        runtime().block_on(async {
            let mut call = std::pin::pin!(db.diagnostics());
            tokio::select! {
                biased;
                answer = &mut call => panic!("answered with a full queue: {answer:?}"),
                () = std::future::ready(()) => {}
            }
            release.send(()).unwrap();
            let diagnostics = call.await.expect("answered once there was room");
            assert_eq!(diagnostics.journal_mode, "wal");
        });
        assert_eq!(ran.load(Ordering::SeqCst), STATE_QUEUE);
    }

    #[test] // spec: ARCH-021, CORE-016
    fn a_workspace_open_that_runs_after_close_changes_nothing() {
        let env = lil_testkit::TestEnv::new();
        let reg = StateRegistry::open(&env.env, Arc::new(|_: &str| {})).unwrap();
        let folder = std::fs::canonicalize(env.make_workspace("late")).unwrap();
        let text = folder.to_string_lossy().into_owned();
        let ws = WorkspaceRef {
            id: Env::workspace_hash(&folder),
            path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
        };
        let db = env
            .env
            .workspace_local_dir_for_canonical(&ws.path)
            .unwrap()
            .join("state.db");
        reg.close();

        // The blocking open as it runs when its thread starts only after `close` returned:
        // refused before it creates, migrates or releases anything (no timing involved).
        let late = open_workspace_db(
            &reg.env,
            reg.on_fatal.clone(),
            &ws,
            Access::ReadWrite,
            &reg.workspaces,
            &reg.opening,
        );
        assert!(matches!(late, Err(StateError::ActorGone)), "{late:?}");
        assert!(!db.exists(), "{} was created after close", db.display());
    }

    #[test] // spec: CORE-004, QA-062
    fn suite_schema_constant_matches_the_migrations() {
        assert_eq!(
            i64::try_from(SUITE_MIGRATIONS.len()).unwrap(),
            lil_schema::SUITE_STATE_SCHEMA,
            "lil doctor tells a newer suite state.db apart by this constant"
        );
        assert_eq!(
            i64::try_from(WORKSPACE_MIGRATIONS.len()).unwrap(),
            lil_schema::WORKSPACE_STATE_SCHEMA,
            "lil doctor tells a newer workspace state.db apart by this constant"
        );
    }

    #[test] // spec: ARCH-021, QA-012
    fn close_drains_a_full_queue() {
        let dir = tempfile::tempdir().unwrap();
        let on_fatal: OnFatal = Arc::new(|_: &str| {});
        let db = Arc::new(
            StateDb::open(&dir.path().join("state.db"), StateScope::Suite, on_fatal).unwrap(),
        );
        let (release, ran) = held_and_full(&db);

        // `close` waits for the queued jobs; with the queue full it still ends.
        let closing = std::thread::spawn({
            let db = db.clone();
            move || db.close()
        });
        release.send(()).unwrap();
        closing.join().expect("close ended");
        assert_eq!(ran.load(Ordering::SeqCst), STATE_QUEUE);
        assert!(db.is_closed());
        assert_eq!(
            runtime().block_on(db.diagnostics()),
            Err(StateError::ActorGone)
        );
    }
}
