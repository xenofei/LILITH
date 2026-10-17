//! The `index.db` schema and how it is opened (ARCH-089, CORE-005).
//!
//! `index.db` is disposable: a `user_version` other than [`INDEX_SCHEMA_VERSION`], a failed
//! `quick_check`, or a file that isn't a database means the file is closed, removed with its
//! `-wal` and `-shm` (through `store::fsops::remove_file`), and created afresh; the caller then
//! runs a cold rebuild. WAL, `synchronous=NORMAL`.
//!
//! Tables: `files` (one row per file, keyed by the path's case-folded key), `assets` (one row
//! per asset folder), `edges` (manifest inputs), `assets_fts` (FTS5 over each asset's text), and
//! two of the index's own: `expected` (the hash each manifest records for a file, and whether
//! the suite still expects that file on disk) and `meta` (the last scan's start).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::{INDEX_SCHEMA_VERSION, IndexError};
use crate::store::fsops;

/// `PRAGMA busy_timeout`.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// The schema at [`INDEX_SCHEMA_VERSION`].
const SCHEMA: &str = "
CREATE TABLE files (
    key TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    project TEXT,
    asset_id TEXT,
    size INTEGER NOT NULL,
    last_write INTEGER NOT NULL,
    change_time INTEGER,
    file_id BLOB,
    volume_serial INTEGER NOT NULL,
    attributes INTEGER NOT NULL,
    sha256 TEXT,
    state TEXT NOT NULL,
    verified INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX files_asset ON files(project, asset_id);
CREATE INDEX files_unverified ON files(verified) WHERE verified = 0;
CREATE TABLE assets (
    id INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    asset_type TEXT,
    status TEXT,
    version TEXT,
    approved_version TEXT,
    manifest TEXT NOT NULL,
    creation_uuid TEXT,
    updated INTEGER NOT NULL,
    needs_repair INTEGER NOT NULL,
    UNIQUE (project, asset_id)
);
CREATE INDEX assets_by_status ON assets(COALESCE(status, ''), project, asset_id);
CREATE INDEX assets_by_type ON assets(COALESCE(asset_type, ''), project, asset_id);
CREATE INDEX assets_by_updated ON assets(updated, project, asset_id);
CREATE TABLE edges (
    from_project TEXT NOT NULL,
    from_asset TEXT NOT NULL,
    to_project TEXT NOT NULL,
    to_asset TEXT NOT NULL,
    to_version TEXT,
    kind TEXT NOT NULL
);
CREATE INDEX edges_from ON edges(from_project, from_asset);
CREATE INDEX edges_to ON edges(to_project, to_asset);
CREATE VIRTUAL TABLE assets_fts USING fts5(body);
CREATE TABLE expected (
    key TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    project TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    sha256 TEXT,
    tracked INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX expected_asset ON expected(project, asset_id);
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value INTEGER
) WITHOUT ROWID;
";

fn sql(e: &rusqlite::Error) -> IndexError {
    IndexError::Sql(e.to_string())
}

/// `busy_timeout`, then WAL (file databases only) and `synchronous=NORMAL`.
fn configure(conn: &Connection, wal: bool) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    if wal {
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    conn.pragma_update(None, "synchronous", "NORMAL")
}

/// Creates the tables and sets `user_version`.
fn create(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", INDEX_SCHEMA_VERSION)
}

/// Opens `index.db` at `path`, creating its folder. Returns the connection and whether it is
/// new (or was rebuilt): then the first reconcile is a cold rebuild.
///
/// # Errors
/// [`IndexError::Io`] when the folder can't be created or a damaged file can't be removed;
/// [`IndexError::Sql`] when the new database can't be created.
pub(super) fn open_file(path: &Path) -> Result<(Connection, bool), IndexError> {
    if let Some(parent) = path.parent() {
        fsops::ensure_dir(parent)
            .map_err(|e| IndexError::Io(format!("cannot create {}: {e}", parent.display())))?;
    }
    let exists = path
        .try_exists()
        .map_err(|e| IndexError::Io(format!("cannot look at {}: {e}", path.display())))?;
    if exists {
        match open_existing(path) {
            Ok(conn) => return Ok((conn, false)),
            Err(why) => {
                tracing::warn!(path = %path.display(), why = %why, "index.db is removed and rebuilt");
                remove_db(path)?;
            }
        }
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| sql(&e))?;
    configure(&conn, true).map_err(|e| sql(&e))?;
    create(&conn).map_err(|e| sql(&e))?;
    Ok((conn, true))
}

/// The same schema over SQLite `:memory:` (a read-only workspace, ARCH-028).
pub(super) fn open_memory() -> Result<Connection, IndexError> {
    let conn = Connection::open_in_memory().map_err(|e| sql(&e))?;
    configure(&conn, false).map_err(|e| sql(&e))?;
    create(&conn).map_err(|e| sql(&e))?;
    Ok(conn)
}

/// An existing `index.db`, checked: `user_version` must be [`INDEX_SCHEMA_VERSION`],
/// `quick_check` must say `ok`, and every table must be there. `Err` says why not.
fn open_existing(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version != INDEX_SCHEMA_VERSION {
        return Err(format!(
            "user_version {version}, not {INDEX_SCHEMA_VERSION}"
        ));
    }
    let report = conn
        .prepare("PRAGMA quick_check")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| e.to_string())?;
    if report != ["ok"] {
        return Err(format!("quick_check: {}", report.join("; ")));
    }
    for table in ["files", "assets", "edges", "assets_fts", "expected", "meta"] {
        conn.prepare(&format!("SELECT * FROM {table} LIMIT 0"))
            .map_err(|e| format!("table {table}: {e}"))?;
    }
    configure(&conn, true).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// `path` with `suffix` appended to its file name (`index.db-wal`).
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Removes `index.db` and its `-wal` and `-shm` (the index is disposable; CORE-110 allows the
/// index rebuild's delete). A file that is already gone is fine.
fn remove_db(path: &Path) -> Result<(), IndexError> {
    for file in [
        path.to_path_buf(),
        beside(path, "-wal"),
        beside(path, "-shm"),
    ] {
        match fsops::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(IndexError::Io(format!(
                    "cannot remove {}: {e}",
                    file.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-089
    fn memory_index_has_the_schema_and_version() {
        let conn = open_memory().unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, INDEX_SCHEMA_VERSION);
        for table in ["files", "assets", "edges", "assets_fts", "expected", "meta"] {
            conn.prepare(&format!("SELECT * FROM {table} LIMIT 0"))
                .unwrap_or_else(|e| panic!("{table}: {e}"));
        }
        assert_eq!(
            beside(Path::new(r"C:\x\index.db"), "-wal"),
            PathBuf::from(r"C:\x\index.db-wal")
        );
    }
}
