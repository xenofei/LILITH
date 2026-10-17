//! What `ww doctor` does before, and beside, the core's own checks (CORE-004, CORE-040; owner
//! decision 5): the suite `state.db`, `suite.toml`, and the one repository setting that only
//! the person may change.
//!
//! `ww` may touch the suite's own local folder, but it never writes workspace files (ARCH-002):
//! [`set_aside`] and [`finish_recovery`] work on the channel's `state.db` only, and
//! [`set_repo_autocrlf_false`] lets `git` itself write `.git\config`, which isn't a
//! store-managed workspace file.
//!
//! - [`inspect_suite_state`] changes nothing and creates nothing: a database without a `-wal`
//!   file is opened `immutable=1`, so no `-wal` or `-shm` appears beside it.
//! - [`set_aside`] renames `state.db` and its `-wal`, `-shm` and `-journal` files to
//!   `<name>.damaged-<YYYYMMDDTHHMMSSZ>` in the same folder. Nothing is ever deleted or
//!   overwritten.
//! - [`finish_recovery`] opens the database read-write once, so SQLite rolls a hot journal back,
//!   then inspects it again.
//! - [`inspect_suite_toml`] reads `suite.toml` itself and never resolves the `cli` workspace
//!   chain, so a stale default never stops the command that reports it.

use std::io;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use ww_platform::Env;
use ww_platform::fs::move_new;
use ww_platform::process::{GIT_REPO_VARS, find_on_path, run_hidden_without};
use ww_schema::{SUITE_STATE_SCHEMA, SuiteToml};

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
}

/// The suite `state.db` (`Env::suite_state_db`), inspected without changing or creating
/// anything.
pub fn inspect_suite_state(env: &Env) -> StateFileState {
    inspect_db(&env.suite_state_db())
}

/// [`inspect_suite_state`] of the database at `path`.
fn inspect_db(path: &Path) -> StateFileState {
    match path.try_exists() {
        Ok(false) => return StateFileState::Missing,
        Ok(true) => {}
        Err(e) => return StateFileState::Damaged(format!("it can't be looked at: {e}")),
    }
    if path.is_dir() {
        return StateFileState::Damaged("it is a folder, not a file".to_string());
    }
    if has_hot_journal(path) {
        return StateFileState::HotJournal;
    }
    let conn = match open_untouched(path) {
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
        Ok((_, found)) if found > SUITE_STATE_SCHEMA => StateFileState::TooNew {
            found,
            known: SUITE_STATE_SCHEMA,
        },
        Ok((_, found)) if found < 0 => {
            StateFileState::Damaged(format!("user_version {found} is not a schema version"))
        }
        Ok(_) => StateFileState::Healthy,
    }
}

/// The side files SQLite keeps beside a database, in the order [`set_aside`] moves them after
/// the database itself.
const SIDE_FILES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// `path` with `suffix` added to its file name (`state.db-wal`).
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Moves the suite `state.db`, then each of its `-wal`, `-shm` and `-journal` files that
/// exists, to `<name>.damaged-<YYYYMMDDTHHMMSSZ>` (the UTC time `now_utc`, in Unix seconds) in
/// the same folder, and returns the new paths. Nothing is deleted, and no existing file is
/// replaced. The core then starts with a fresh database (owner decision 5).
///
/// # Errors
/// When a file can't be moved. A file another program holds open (Windows error 32) says so;
/// the error names what was already moved, so nothing is lost track of.
pub fn set_aside(env: &Env, now_utc: i64) -> io::Result<Vec<PathBuf>> {
    set_aside_at(&env.suite_state_db(), now_utc)
}

/// The suffix [`set_aside`] gives the files at `now_utc`: `.damaged-<YYYYMMDDTHHMMSSZ>`.
pub fn set_aside_suffix(now_utc: i64) -> String {
    format!(".damaged-{}", ww_schema::time::compact(now_utc))
}

fn set_aside_at(db: &Path, now_utc: i64) -> io::Result<Vec<PathBuf>> {
    let suffix = set_aside_suffix(now_utc);
    let mut moved = Vec::new();
    let files = std::iter::once(db.to_path_buf()).chain(SIDE_FILES.iter().map(|s| beside(db, s)));
    for (n, from) in files.enumerate() {
        // The database must exist; its side files only when they do.
        if n > 0 && !from.exists() {
            continue;
        }
        let to = beside(&from, &suffix);
        if let Err(e) = move_new(&from, &to) {
            let why = if e.raw_os_error() == Some(32) {
                "another program holds it open; close the Worldwright apps (and any program \
                 that may have it open), then run ww doctor again"
                    .to_string()
            } else {
                e.to_string()
            };
            let already = if moved.is_empty() {
                "Nothing was moved.".to_string()
            } else {
                format!(
                    "Already set aside: {}.",
                    moved
                        .iter()
                        .map(|p: &PathBuf| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            return Err(io::Error::new(
                e.kind(),
                format!("{} can't be set aside: {why}. {already}", from.display()),
            ));
        }
        moved.push(to);
    }
    Ok(moved)
}

/// Opens the suite `state.db` read-write once, so SQLite rolls a hot journal back (the change
/// the core itself refuses to make), then inspects it again.
///
/// # Errors
/// When the database can't be opened or read.
pub fn finish_recovery(env: &Env) -> io::Result<StateFileState> {
    let path = env.suite_state_db();
    let conn = Connection::open_with_flags(
        &path,
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
    Ok(inspect_db(&path))
}

/// Whether `<path>-journal` is a hot rollback journal: not empty, and its header not zeroed (the
/// persistent journal mode zeroes a finished journal's header instead of deleting it).
fn has_hot_journal(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(beside(path, "-journal")) else {
        return false;
    };
    let mut head = [0_u8; 8];
    let mut read = 0;
    while read < head.len() {
        match file.read(&mut head[read..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    read > 0 && head[..read].iter().any(|&b| b != 0)
}

/// A read-only connection that leaves no trace: `immutable=1` when there is no `-wal` file (no
/// `-wal` or `-shm` is created), a plain read-only open when the WAL must be read.
fn open_untouched(path: &Path) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if !beside(path, "-wal").exists()
        && let Some(uri) = immutable_uri(path)
    {
        return Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI);
    }
    Connection::open_with_flags(path, flags)
}

/// `file:///C:/…?immutable=1`, every byte outside the unreserved set percent-encoded; `None`
/// for a path that isn't a plain drive path.
fn immutable_uri(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    let bytes = text.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return None;
    }
    let mut uri = String::from("file:///");
    for &b in bytes {
        match b {
            b'\\' | b'/' => uri.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' => {
                uri.push(char::from(b));
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri.push_str("?immutable=1");
    Some(uri)
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
    let file = env.suite_toml();
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

/// How long the `git config` run may take.
const GIT_LIMIT: Duration = Duration::from_secs(30);

/// `git -C <ws> config --local core.autocrlf false`, through `run_hidden` (no console window),
/// with the variables that would point `git` elsewhere removed. `ww doctor` calls it only after
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-004
    fn immutable_uris_percent_encode_the_path() {
        assert_eq!(
            immutable_uri(Path::new(r"C:\t\Ünïcödé space\state.db")).as_deref(),
            Some("file:///C:/t/%C3%9Cn%C3%AFc%C3%B6d%C3%A9%20space/state.db?immutable=1")
        );
        assert_eq!(immutable_uri(Path::new(r"\\server\share\state.db")), None);
    }

    #[test] // spec: CORE-004
    fn side_file_names_are_beside_the_database() {
        let db = Path::new(r"C:\t\local\state.db");
        assert_eq!(
            beside(db, "-wal"),
            PathBuf::from(r"C:\t\local\state.db-wal")
        );
        assert_eq!(
            beside(&beside(db, "-shm"), ".damaged-20261017T010203Z"),
            PathBuf::from(r"C:\t\local\state.db-shm.damaged-20261017T010203Z")
        );
    }
}
