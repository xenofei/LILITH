//! A SQLite database's files, looked at and set aside from outside (CORE-004, owner decision 5):
//! the one home of these helpers for `lil doctor` (`lil-client`) and the core's state databases
//! (`lil-core`), so a fix to one is a fix to both (plan-2b Minor 6).
//!
//! - [`beside`] names a side file: `state.db` → `state.db-wal`.
//! - [`has_hot_journal`] and [`orphan_sidecars`] read metadata and at most 8 bytes.
//! - [`open_untouched`] opens read-only, `immutable=1` only while the file is quiet, so a file
//!   that is then refused never gets fresh `-wal` and `-shm` files.
//! - [`set_aside`] renames a database and its side files to `<name>.damaged-<utc>`; it never
//!   deletes or overwrites anything. Only `lil doctor` calls it, after the person typed `y`:
//!   `lil-core` never renames a `state.db` (its `clippy.toml` forbids the call).

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// The side files SQLite keeps beside a database, in the order [`set_aside`] moves them after
/// the database itself.
pub const SIDE_FILES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// `db` with `suffix` added to its file name (`state.db-wal`).
pub fn beside(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Whether `<db>-journal` is a hot rollback journal: it exists, is not empty, and its header was
/// not zeroed (the persistent journal mode zeroes a finished journal's header instead of
/// deleting it). Only metadata and the first 8 bytes are read.
///
/// # Errors
/// When the journal exists but can't be opened or read (a caller fails closed on it).
pub fn has_hot_journal(db: &Path) -> io::Result<bool> {
    let mut file = match std::fs::File::open(beside(db, "-journal")) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut head = [0_u8; 8];
    let mut read = 0;
    while read < head.len() {
        match file.read(&mut head[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read > 0 && head[..read].iter().any(|&b| b != 0))
}

/// The `-wal`, `-shm` and `-journal` files beside `db` when `db` itself is missing, in
/// [`SIDE_FILES`] order; none when `db` exists. A fresh database created beside them could take
/// an old write-ahead log for its own.
///
/// # Errors
/// When whether `db` or a side file exists can't be read.
pub fn orphan_sidecars(db: &Path) -> io::Result<Vec<PathBuf>> {
    if db.try_exists()? {
        return Ok(Vec::new());
    }
    let mut left = Vec::new();
    for suffix in SIDE_FILES {
        let side = beside(db, suffix);
        if side.try_exists()? {
            left.push(side);
        }
    }
    Ok(left)
}

/// `db` as a SQLite URI (`file:///C:/a%20b/state.db?<query>`), every byte outside the
/// unreserved set percent-encoded; `None` for a path that isn't a plain drive path.
pub fn sqlite_uri(db: &Path, query: &str) -> Option<String> {
    let text = db.to_str()?;
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
    uri.push('?');
    uri.push_str(query);
    Some(uri)
}

/// A read-only connection to an existing database that leaves no trace. `quiet`: no process may
/// be writing the file. On a quiet file without a `-wal` it is opened `immutable=1`, so SQLite
/// creates no `-wal` or `-shm` beside it. With a `-wal` file the WAL must be read, and a file
/// another process may be writing must not be read as immutable (SQLite would skip its locks and
/// could see a half-written page): both are plain read-only opens.
///
/// # Errors
/// SQLite's error.
pub fn open_untouched(db: &Path, quiet: bool) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if quiet
        && !beside(db, "-wal").exists()
        && let Some(uri) = sqlite_uri(db, "immutable=1")
    {
        return Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI);
    }
    Connection::open_with_flags(db, flags)
}

/// The suffix [`set_aside`] gives the files: `.damaged-<utc>` (`utc` as
/// `YYYYMMDDTHHMMSSZ`).
pub fn set_aside_suffix(utc: &str) -> String {
    format!(".damaged-{utc}")
}

/// Moves `db` (when there is one) and then each of its `-wal`, `-shm` and `-journal` files that
/// exists to `<name>.damaged-<utc>` in the same folder, and returns the new paths. Nothing is
/// deleted, and no existing file is replaced (`move_new`).
///
/// # Errors
/// When there is nothing to set aside, or a file can't be moved. A file another program holds
/// open (Windows error 32) says so; the error names what was already moved, so nothing is lost
/// track of.
pub fn set_aside(db: &Path, utc: &str) -> io::Result<Vec<PathBuf>> {
    let suffix = set_aside_suffix(utc);
    let mut files = Vec::new();
    for path in std::iter::once(db.to_path_buf()).chain(SIDE_FILES.iter().map(|s| beside(db, s))) {
        if path.try_exists()? {
            files.push(path);
        }
    }
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("there is no {} and none of its side files", db.display()),
        ));
    }
    let mut moved: Vec<PathBuf> = Vec::new();
    for from in files {
        let to = beside(&from, &suffix);
        if let Err(e) = crate::fs::move_new(&from, &to) {
            let why = if e.raw_os_error() == Some(32) {
                "another program holds it open; close the LILITH apps (and any program \
                 that may have it open), then run lil doctor again"
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
                        .map(|p| p.display().to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-004
    fn sqlite_uris_percent_encode_the_path() {
        assert_eq!(
            sqlite_uri(Path::new(r"C:\t\Ünïcödé space\a#b?.db"), "immutable=1").as_deref(),
            Some("file:///C:/t/%C3%9Cn%C3%AFc%C3%B6d%C3%A9%20space/a%23b%3F.db?immutable=1")
        );
        assert_eq!(
            sqlite_uri(Path::new(r"\\?\D:\x\state.db"), "immutable=1").as_deref(),
            Some("file:///D:/x/state.db?immutable=1")
        );
        assert_eq!(sqlite_uri(Path::new(r"\\server\share\state.db"), "q"), None);
        assert_eq!(sqlite_uri(Path::new("relative.db"), "q"), None);
    }

    #[test] // spec: CORE-004
    fn side_file_names_are_beside_the_database() {
        let db = Path::new(r"C:\t\local\state.db");
        assert_eq!(
            beside(db, "-wal"),
            PathBuf::from(r"C:\t\local\state.db-wal")
        );
        assert_eq!(
            beside(&beside(db, "-shm"), &set_aside_suffix("20261017T010203Z")),
            PathBuf::from(r"C:\t\local\state.db-shm.damaged-20261017T010203Z")
        );
    }

    #[test] // spec: CORE-004
    fn a_zeroed_or_empty_journal_is_not_hot() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let journal = dir.path().join("state.db-journal");
        assert!(!has_hot_journal(&db).unwrap(), "no journal");
        std::fs::write(&journal, b"").unwrap();
        assert!(!has_hot_journal(&db).unwrap(), "an empty journal");
        std::fs::write(&journal, [0_u8; 512]).unwrap();
        assert!(
            !has_hot_journal(&db).unwrap(),
            "a journal whose header was zeroed"
        );
        std::fs::write(&journal, [0xd9, 0xd5, 0x05, 0xf9, 0, 0]).unwrap();
        assert!(has_hot_journal(&db).unwrap(), "a journal with a header");
    }

    #[test] // spec: CORE-004
    fn orphan_sidecars_are_only_those_without_their_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        assert!(orphan_sidecars(&db).unwrap().is_empty());
        std::fs::write(beside(&db, "-shm"), b"shm").unwrap();
        std::fs::write(beside(&db, "-wal"), b"wal").unwrap();
        assert_eq!(
            orphan_sidecars(&db).unwrap(),
            [beside(&db, "-wal"), beside(&db, "-shm")]
        );
        std::fs::write(&db, b"db").unwrap();
        assert!(
            orphan_sidecars(&db).unwrap().is_empty(),
            "the database is there"
        );
    }

    #[test] // spec: CORE-004
    fn set_aside_moves_the_database_and_its_side_files_and_replaces_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        std::fs::write(&db, b"db").unwrap();
        std::fs::write(beside(&db, "-wal"), b"wal").unwrap();
        let moved = set_aside(&db, "20261018T010203Z").unwrap();
        assert_eq!(
            moved,
            [
                dir.path().join("state.db.damaged-20261018T010203Z"),
                dir.path().join("state.db-wal.damaged-20261018T010203Z"),
            ]
        );
        assert!(!db.exists() && !beside(&db, "-wal").exists());
        assert_eq!(std::fs::read(&moved[1]).unwrap(), b"wal");

        // The same second again: the names are taken, so nothing is replaced.
        std::fs::write(&db, b"second").unwrap();
        let err = set_aside(&db, "20261018T010203Z").unwrap_err();
        assert!(err.to_string().contains("Nothing was moved"), "{err}");
        assert_eq!(std::fs::read(&db).unwrap(), b"second");
        assert_eq!(std::fs::read(&moved[0]).unwrap(), b"db");

        let none = dir.path().join("none.db");
        assert_eq!(
            set_aside(&none, "20261018T010203Z").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// Every `.rs` file under `dir`.
    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test] // spec: CORE-004
    fn sqlite_helpers_have_one_home() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut files = Vec::new();
        for krate in ["lil-client", "lil-core"] {
            sources(&crates.join(krate).join("src"), &mut files);
        }
        assert!(!files.is_empty());
        let copies: Vec<String> = files
            .iter()
            .filter(|f| {
                let text = std::fs::read_to_string(f).unwrap();
                [
                    "fn has_hot_journal",
                    "fn open_untouched",
                    "fn immutable_uri",
                    "fn sqlite_uri",
                ]
                .iter()
                .any(|copy| text.contains(copy))
            })
            .map(|f| f.display().to_string())
            .collect();
        assert!(
            copies.is_empty(),
            "lil_platform::sqlite_files is their one home; copies in {copies:?}"
        );
    }
}
