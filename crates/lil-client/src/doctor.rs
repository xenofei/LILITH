//! What `lil doctor` does before, and beside, the core's own checks (CORE-004, CORE-040; owner
//! decision 5): the suite `state.db`, the default workspace's `state.db` in its local folder,
//! `suite.toml`, and the one repository setting that only the person may change.
//!
//! `lil` may touch the suite's own local folders, but it never writes workspace files
//! (ARCH-002): [`set_aside_db`] and [`finish_recovery_at`] work on a `state.db` in the channel's
//! local folder or a workspace's local folder (`Env::workspace_local_dir`), never in the
//! workspace itself, and [`set_repo_autocrlf_false`] lets `git` itself write `.git\config`,
//! which isn't a store-managed workspace file. The SQLite file helpers live in
//! `lil_platform::sqlite_files`, shared with the core.
//!
//! - [`inspect_suite_state`] changes nothing and creates nothing: a database without a `-wal`
//!   file is opened `immutable=1`, so no `-wal` or `-shm` appears beside it.
//! - [`set_aside`] renames `state.db` and its `-wal`, `-shm` and `-journal` files to
//!   `<name>.damaged-<YYYYMMDDTHHMMSSZ>` in the same folder. Nothing is ever deleted or
//!   overwritten.
//!   Side files left without their database ([`StateFileState::Leftovers`]) are moved the same
//!   way, so a fresh database never takes an old WAL for its own.
//! - [`core_running`] tells whether a core serves the channel; nothing is offered while one does.
//! - [`finish_recovery`] opens the database read-write once, so SQLite rolls a hot journal back,
//!   then inspects it again.
//! - [`inspect_suite_toml`] reads `suite.toml` itself and never resolves the `cli` workspace
//!   chain, so a stale default never stops the command that reports it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lil_platform::Env;
use lil_platform::pipe::{ConnectError, open_client};
use lil_platform::process::{GIT_REPO_VARS, find_on_path, run_hidden_without};
use lil_platform::sqlite_files::{self, has_hot_journal, open_untouched, orphan_sidecars};
use lil_schema::SuiteToml;
/// The schemas [`inspect_state_db`] compares against, for callers without `lil-schema`.
pub use lil_schema::{SUITE_STATE_SCHEMA, WORKSPACE_STATE_SCHEMA};
use rusqlite::{Connection, OpenFlags};

/// What the suite `state.db` looks like from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFileState {
    /// No file yet: the core creates it when it starts.
    Missing,
    /// It passes SQLite's `quick_check` at a schema this build knows.
    Healthy,
    /// SQLite reports it damaged, or it can't be read as a database; why.
    Damaged(String),
    /// Its schema (`user_version`) is newer than this build's.
    TooNew { found: i64, known: i64 },
    /// A hot rollback journal lies beside it: a crash mid-write.
    HotJournal,
    /// No database, but some of its `-wal`, `-shm` or `-journal` files are left: a fresh
    /// database created beside them could take an old WAL for its own, so they are set aside
    /// first (only moved, never deleted).
    Leftovers(Vec<PathBuf>),
}

/// The suite `state.db` (`Env::suite_state_db`), inspected without changing or creating
/// anything.
pub fn inspect_suite_state(env: &Env) -> StateFileState {
    // A running core may be writing it: then never `immutable=1`.
    inspect_state_db(
        &env.suite_state_db(),
        !core_running(env),
        SUITE_STATE_SCHEMA,
    )
}

/// The `state.db` of the workspace at `ws`: `Env::workspace_local_dir(ws)\state.db`.
///
/// # Errors
/// When the workspace folder can't be resolved.
pub fn workspace_state_db(env: &Env, ws: &Path) -> io::Result<PathBuf> {
    env.workspace_local_dir(ws)
        .map(|dir| dir.join("state.db"))
        .map_err(|e| io::Error::other(e.to_string()))
}

/// The database at `path` (the suite one, or a workspace's), inspected without changing or
/// creating anything; `quiet`: no process may be writing it; `known`: the schema this build
/// knows (`SUITE_STATE_SCHEMA` or `WORKSPACE_STATE_SCHEMA`).
pub fn inspect_state_db(path: &Path, quiet: bool, known: i64) -> StateFileState {
    match path.try_exists() {
        Ok(false) => {
            return match orphan_sidecars(path) {
                Ok(left) if left.is_empty() => StateFileState::Missing,
                Ok(left) => StateFileState::Leftovers(left),
                Err(e) => {
                    StateFileState::Damaged(format!("its side files can't be looked at: {e}"))
                }
            };
        }
        Ok(true) => {}
        Err(e) => return StateFileState::Damaged(format!("it can't be looked at: {e}")),
    }
    if path.is_dir() {
        return StateFileState::Damaged("it is a folder, not a file".to_string());
    }
    match has_hot_journal(path) {
        Ok(true) => return StateFileState::HotJournal,
        Ok(false) => {}
        Err(e) => {
            return StateFileState::Damaged(format!("its rollback journal can't be read: {e}"));
        }
    }
    let conn = match open_untouched(path, quiet) {
        Ok(conn) => conn,
        Err(e) => return StateFileState::Damaged(e.to_string()),
    };
    let checked = (|| -> rusqlite::Result<(Vec<String>, i64)> {
        let report = conn
            .prepare("PRAGMA quick_check")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let found = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok((report, found))
    })();
    drop(conn);
    match checked {
        Err(e) => StateFileState::Damaged(e.to_string()),
        Ok((report, _)) if report != ["ok"] => StateFileState::Damaged(report.join("; ")),
        Ok((_, found)) if found > known => StateFileState::TooNew { found, known },
        Ok((_, found)) if found < 0 => {
            StateFileState::Damaged(format!("user_version {found} is not a schema version"))
        }
        Ok(_) => StateFileState::Healthy,
    }
}

/// Moves the suite `state.db` (when there is one; [`StateFileState::Leftovers`] has none), then
/// each of its `-wal`, `-shm` and `-journal` files that exists, to
/// `<name>.damaged-<YYYYMMDDTHHMMSSZ>` (the UTC time `now_utc`, in Unix seconds) in the same
/// folder, and returns the new paths. Nothing is deleted, and no existing file is replaced. The
/// core then starts with a fresh database (owner decision 5).
///
/// # Errors
/// As [`set_aside_db`].
pub fn set_aside(env: &Env, now_utc: i64) -> io::Result<Vec<PathBuf>> {
    set_aside_db(&env.suite_state_db(), now_utc)
}

/// [`set_aside`] of the database at `db` (the suite one, or a workspace's).
///
/// # Errors
/// When there is nothing to set aside, or a file can't be moved. A file another program holds
/// open (Windows error 32) says so; the error names what was already moved, so nothing is lost
/// track of.
pub fn set_aside_db(db: &Path, now_utc: i64) -> io::Result<Vec<PathBuf>> {
    sqlite_files::set_aside(db, &lil_schema::time::compact(now_utc))
}

/// The suffix [`set_aside`] gives the files at `now_utc`: `.damaged-<YYYYMMDDTHHMMSSZ>`.
pub fn set_aside_suffix(now_utc: i64) -> String {
    sqlite_files::set_aside_suffix(&lil_schema::time::compact(now_utc))
}

/// Opens the suite `state.db` read-write once, so SQLite rolls a hot journal back (the change
/// the core itself refuses to make), then inspects it again.
///
/// # Errors
/// When the database can't be opened or read.
pub fn finish_recovery(env: &Env) -> io::Result<StateFileState> {
    finish_recovery_at(&env.suite_state_db(), SUITE_STATE_SCHEMA)
}

/// [`finish_recovery`] of the database at `path`, whose schema this build knows as `known`.
///
/// # Errors
/// When the database can't be opened or read.
pub fn finish_recovery_at(path: &Path, known: i64) -> io::Result<StateFileState> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)?;
    // The first read takes the lock that makes SQLite roll the journal back.
    let rolled_back = conn
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(drop);
    conn.close().map_err(|(_, e)| io::Error::other(e))?;
    if let Err(e) = rolled_back {
        return Ok(StateFileState::Damaged(e.to_string()));
    }
    Ok(inspect_state_db(path, true, known))
}

/// What `suite.toml` says about the default workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuiteTomlState {
    /// No `suite.toml`: no workspace is set up yet.
    Missing,
    /// It can't be read or doesn't parse; why.
    Unreadable(String),
    /// It names no default workspace.
    NoDefault,
    /// Its default workspace's folder doesn't exist (the path as written there).
    DefaultMissing(PathBuf),
    /// The default workspace, as written there, and its id (the hash of its canonical path).
    Ok { default: PathBuf, id: String },
}

/// The channel's `suite.toml`, read with [`SuiteToml`]; never through the `cli` workspace
/// chain, so a stale default is reported rather than refused.
pub fn inspect_suite_toml(env: &Env) -> SuiteTomlState {
    let file = env.suite_toml_to_read();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return SuiteTomlState::Missing,
        Err(e) => {
            return SuiteTomlState::Unreadable(format!("{} can't be read: {e}", file.display()));
        }
    };
    let suite = match SuiteToml::parse(&text) {
        Ok(suite) => suite,
        Err(e) => {
            return SuiteTomlState::Unreadable(format!("{} is not valid: {e}", file.display()));
        }
    };
    let Some(default) = suite.default_workspace.filter(|p| !p.is_empty()) else {
        return SuiteTomlState::NoDefault;
    };
    let default = PathBuf::from(default);
    match std::fs::canonicalize(&default) {
        Ok(canonical) if canonical.is_dir() => SuiteTomlState::Ok {
            id: Env::workspace_hash(&canonical),
            default,
        },
        _ => SuiteTomlState::DefaultMissing(default),
    }
}

/// The `[engines.comfyui] pass_env` entries this channel's `suite.toml` lists that the engine
/// scrub refuses even though they are listed, or that can never name a variable (ruling P3-R31):
/// `(entry, why)`. Empty when the file is missing, unreadable, has no such table, or every entry
/// is let through. `lil doctor` reports each one, so a refused name is never dropped silently.
pub fn comfy_pass_env_problems(env: &Env) -> Vec<(String, String)> {
    comfy_pass_env_split(env).1
}

/// The `pass_env` entries that are let through but look like a key or token that isn't a hub
/// token (ruling P3-R31b): `(entry, why)`. `lil doctor` warns about each, without refusing it.
pub fn comfy_pass_env_warnings(env: &Env) -> Vec<(String, String)> {
    comfy_pass_env_split(env)
        .0
        .into_iter()
        .filter_map(|e| lil_schema::pass_env::warning(&e).map(|why| (e, why)))
        .collect()
}

fn comfy_pass_env_split(env: &Env) -> (Vec<String>, Vec<(String, String)>) {
    let Ok(text) = std::fs::read_to_string(env.suite_toml_to_read()) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(suite) = SuiteToml::parse(&text) else {
        return (Vec::new(), Vec::new());
    };
    lil_schema::pass_env::split(&suite)
}

/// Whether a core serves this channel's pipe now (it holds the suite `state.db` open). Any
/// answer but "no such pipe" counts as running, so a set-aside is never offered while one may
/// be using the files.
///
/// It opens a real client connection to the pipe and drops it at once, without a hello. That is
/// harmless: the core ends a connection that closes before its hello like any other, and no
/// core is started by it.
pub fn core_running(env: &Env) -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return true;
    };
    let pipe = env.pipe_name();
    rt.block_on(async { !matches!(open_client(&pipe), Err(ConnectError::NotFound)) })
}

/// How long the `git config` run may take.
const GIT_LIMIT: Duration = Duration::from_secs(30);

/// `git -C <ws> config --local core.autocrlf false`, through `run_hidden` (no console window),
/// with the variables that would point `git` elsewhere removed. `lil doctor` calls it only after
/// the person typed `y`; git itself writes the repository's `.git\config`.
///
/// # Errors
/// When `ws` has no `.git`, `git` isn't on `PATH` or doesn't finish in 30 s, or `git` fails.
pub fn set_repo_autocrlf_false(ws: &Path) -> io::Result<()> {
    if !ws.join(".git").exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a git repository", ws.display()),
        ));
    }
    let git = find_on_path("git")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git is not on PATH"))?;
    let args = [
        "-C".as_ref(),
        ws.as_os_str(),
        "config".as_ref(),
        "--local".as_ref(),
        "core.autocrlf".as_ref(),
        "false".as_ref(),
    ];
    let out = run_hidden_without(&git, &args, ws, &[], GIT_REPO_VARS, GIT_LIMIT)?;
    match out.code {
        Some(0) => Ok(()),
        Some(code) => Err(io::Error::other(format!(
            "`git config --local core.autocrlf false` failed (exit code {code}): {}",
            out.stderr.trim()
        ))),
        None => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "`git config --local core.autocrlf false` did not finish in 30 s",
        )),
    }
}
