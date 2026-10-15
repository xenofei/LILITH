//! The core's SQLite databases (CORE-004, CORE-005, QA-012, QA-062): the suite `state.db` at
//! `Env::local_dir()\state.db` and one per workspace at `Env::workspace_local_dir(ws)\state.db`.
//!
//! **One actor per database.** Each [`StateDb`] owns a single `rusqlite::Connection` on its
//! own named thread (`ww-state-suite`, `ww-state-ws-<id>`); callers send it jobs and await the
//! replies. If the actor panics, `on_fatal("state")` is called once and every queued and later
//! call returns [`StateError::ActorGone`].
//!
//! **Check before write.** An existing file is first opened read-only: `PRAGMA quick_check`
//! must say `ok` and `PRAGMA user_version` must not exceed the migrations this build knows.
//! Only then is it opened read-write (WAL, `synchronous=FULL`, `busy_timeout=5000`,
//! `foreign_keys=ON`) and migrated, each migration in its own `BEGIN IMMEDIATE`. A newer or
//! damaged file is never modified, and `ww-core` never deletes, renames or truncates a
//! `state.db` (owner decision 5).
//!
//! **Stale rows.** On open, `running` idempotency rows (calls that died with an earlier core)
//! are deleted. That is safe because only the core that owns the channel's pipe opens its
//! databases (startup order: pipe, log, state). `done` rows older than 30 days are pruned by
//! `idem_begin`, against the caller's `now`: the same clock that stamped the rows.
//!
//! **Workspace databases** are opened on first use, on a blocking thread
//! (`spawn_blocking`), each behind its own once-cell: opening one workspace's database never
//! holds up calls for another.

mod migrations;

pub use migrations::{SUITE_MIGRATIONS, WORKSPACE_MIGRATIONS};

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension as _, TransactionBehavior, params,
};
use tokio::sync::{OnceCell, oneshot};
use ww_platform::Env;

use crate::store::fsops;
use crate::workspace::WorkspaceRef;

/// Called once when a database actor dies, with `"state"`.
pub type OnFatal = Arc<dyn Fn(&str) + Send + Sync>;

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
            StateScope::Suite => "ww-state-suite",
            StateScope::Workspace => "ww-state-ws",
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
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// One `state.db`: a handle to its actor thread. Dropping it stops the actor and closes the
/// connection.
pub struct StateDb {
    path: PathBuf,
    schema_version: i64,
    jobs: Option<mpsc::Sender<Job>>,
    actor: Option<JoinHandle<()>>,
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
            check_existing(path, known)?;
        }
        let mut conn = open_read_write(path)?;
        let schema_version = migrate(&mut conn, migrations).map_err(|e| match e {
            Migrate::State(e) => e,
            Migrate::Sql(e) => sql_error(&e),
        })?;
        release_running(&mut conn).map_err(|e| sql_error(&e))?;

        let (jobs, inbox) = mpsc::channel::<Job>();
        let actor = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_actor(conn, &inbox, &on_fatal))
            .map_err(|e| StateError::Io(format!("cannot start the database actor: {e}")))?;
        Ok(StateDb {
            path: path.to_path_buf(),
            schema_version,
            jobs: Some(jobs),
            actor: Some(actor),
        })
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
    /// `now_utc_secs` are pruned, so rows and pruning use the caller's clock.
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
    /// it is.
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
            changed.map(drop).map_err(|e| sql_error(&e))
        })
        .await
    }

    /// Makes the actor panic. Used only by the test-channel tool `test.state_panic` (QA-012).
    pub fn panic_actor(&self) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Box::new(|_| {
                panic!("the state actor was asked to panic (test.state_panic)")
            }));
        }
    }

    /// Runs `f` on the actor's connection.
    async fn call<T, F>(&self, f: F) -> Result<T, StateError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StateError> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let _ = reply.send(f(conn));
        });
        self.jobs
            .as_ref()
            .ok_or(StateError::ActorGone)?
            .send(job)
            .map_err(|_| StateError::ActorGone)?;
        answer.await.map_err(|_| StateError::ActorGone)?
    }
}

impl Drop for StateDb {
    /// Stops the actor and waits for it to close the connection.
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(actor) = self.actor.take()
            && actor.thread().id() != std::thread::current().id()
        {
            let _ = actor.join();
        }
    }
}

/// The actor: runs jobs in order until every sender is gone. A panicking job ends it:
/// `on_fatal("state")` once, then the inbox is dropped, so every queued and later call gets
/// [`StateError::ActorGone`].
fn run_actor(mut conn: Connection, inbox: &mpsc::Receiver<Job>, on_fatal: &OnFatal) {
    while let Ok(job) = inbox.recv() {
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

/// Step 2 of the open order: an existing file, read-only. `quick_check` must say `ok`, and
/// `user_version` must be between 0 and `known`.
fn check_existing(path: &Path, known: i64) -> Result<(), StateError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| sql_error(&e))?;
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

/// Deletes the `running` rows of calls that died with an earlier core.
fn release_running(conn: &mut Connection) -> rusqlite::Result<()> {
    let released = conn.execute("DELETE FROM idempotency WHERE state = 'running'", [])?;
    if released > 0 {
        tracing::info!(
            released,
            "running idempotency rows of an earlier core released"
        );
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
        "DELETE FROM idempotency WHERE state = 'done' AND created_utc < ?1",
        params![now_utc_secs.saturating_sub(DONE_KEPT_SECS)],
    )?;
    if pruned > 0 {
        tracing::info!(pruned, "idempotency rows older than 30 days pruned");
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
    suite: StateDb,
    /// One cell per workspace id: the map's lock is held only to find the cell.
    workspaces: Mutex<HashMap<String, Arc<OnceCell<Arc<StateDb>>>>>,
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
            suite,
            workspaces: Mutex::new(HashMap::new()),
        })
    }

    /// The suite database.
    pub fn suite(&self) -> &StateDb {
        &self.suite
    }

    /// The database of `ws` at `Env::workspace_local_dir(ws)\state.db` (CORE-005), opened on
    /// first use and then cached: one actor per file (QA-012).
    ///
    /// The open (folder, `quick_check`, migrations) runs on a blocking thread behind the
    /// workspace's own once-cell, so concurrent callers for the same workspace wait for one
    /// open and other workspaces are not held up. A failed open is retried by the next call.
    ///
    /// # Errors
    /// As [`StateDb::open`]; [`StateError::Io`] if the workspace's local folder cannot be
    /// worked out or the blocking open did not finish.
    pub async fn for_workspace(&self, ws: &WorkspaceRef) -> Result<Arc<StateDb>, StateError> {
        let cell = self
            .workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(ws.id.clone())
            .or_default()
            .clone();
        let db = cell
            .get_or_try_init(|| {
                let (env, on_fatal, ws) = (self.env.clone(), self.on_fatal.clone(), ws.clone());
                async move {
                    tokio::task::spawn_blocking(move || {
                        let dir = env
                            .workspace_local_dir_for_canonical(&ws.path)
                            .map_err(|e| StateError::Io(e.to_string()))?;
                        StateDb::open_named(
                            &dir.join("state.db"),
                            StateScope::Workspace,
                            format!("ww-state-ws-{}", ws.id),
                            on_fatal,
                        )
                        .map(Arc::new)
                    })
                    .await
                    .map_err(|e| {
                        StateError::Io(format!("opening the workspace database stopped: {e}"))
                    })?
                }
            })
            .await?;
        Ok(db.clone())
    }
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
            db.panic_actor();
            db.idem_begin("cli|none", "k", "h", 0).await
        });
        assert_eq!(pending, Err(StateError::ActorGone));
        db.panic_actor();
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
}
