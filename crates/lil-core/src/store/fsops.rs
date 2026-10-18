//! The file-write home of `lil-core` (ARCH-112, QA-024): the only file in the crate with
//! `#[allow(clippy::disallowed_methods)]`, one per function.
//!
//! These are thin, blocking wrappers. The write algorithm (hash check, pending rows, retries,
//! put-backs) is `store::write`, which calls them on blocking threads. Paths go to Windows
//! through `lil_platform::fs::long_path`, so long absolute paths work without the
//! `LongPathsEnabled` opt-in.

use std::fs::OpenOptions;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use lil_platform::fs::{LockedFile, long_path};

use super::failpoints::FailPoints;

/// Writes `bytes` to `path` atomically: the data goes to `<path>.tmp-<pid>`, is flushed to disk
/// with `sync_all`, and is then renamed over the target.
///
/// A plan-2 stub kept for this crate's unit tests' fixture files, so it exists only in test
/// builds. It replaces an existing target without any hash check, so nothing outside tests
/// may use it: workspace files go through `Store::write`.
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
pub fn write_file_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp_name);

    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();

    if result.is_err() {
        // Best effort: do not leave the temp file behind when the write or rename failed.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Creates `path` and any missing parents; an existing folder is fine.
#[allow(clippy::disallowed_methods)]
pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(long_path(path))
}

/// Creates the folder `path`, whose parent exists; an existing file or folder there is an
/// error (`AlreadyExists`), so a new staging handle is never one that is already in use.
#[allow(clippy::disallowed_methods)]
pub fn create_dir_new(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(long_path(path))
}

/// Removes the folder `path` only if it is empty (`RemoveDirectoryW` refuses one that isn't,
/// with error 145), so no content is ever deleted (CORE-110). A link is never followed.
#[allow(clippy::disallowed_methods)]
pub fn remove_empty_dir(path: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(long_path(path))
}

/// Write step 4: creates `dir\name` (never an existing file), writes `bytes` and flushes them
/// with `FlushFileBuffers` before closing. The fail point `store.write_temp` comes after the
/// create, and its error stands for the write's. On any error after the create, the file is
/// removed again.
#[allow(clippy::disallowed_methods)]
pub fn write_temp(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    fp: &FailPoints,
) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(long_path(&path))?;
    let written = (|| {
        fp.hit("store.write_temp")?;
        file.write_all(bytes)?;
        lil_platform::fs::flush(&file)
    })();
    drop(file);
    if let Err(e) = written {
        let _ = std::fs::remove_file(long_path(&path));
        return Err(e);
    }
    Ok(path)
}

/// Flushes the existing file `path` to disk with `FlushFileBuffers`, through a handle opened
/// for writing that writes nothing (never creates or truncates): the copy-forward's fsync of
/// its temp file (plan decision D3).
#[allow(clippy::disallowed_methods)]
pub fn flush_file(path: &Path) -> std::io::Result<()> {
    let file = OpenOptions::new().write(true).open(long_path(path))?;
    lil_platform::fs::flush(&file)
}

/// `ReplaceFileW(target, temp, backup)` without a write-through flag (see `lil_platform::fs`).
#[allow(clippy::disallowed_methods)]
pub fn replace(target: &Path, temp: &Path, backup: &Path) -> std::io::Result<()> {
    lil_platform::fs::replace_file(target, temp, backup)
}

/// `MoveFileExW(MOVEFILE_WRITE_THROUGH)`: the only move. An existing `to` is never replaced
/// (errors 80 and 183); there is no replace-existing variant.
#[allow(clippy::disallowed_methods)]
pub fn move_new(from: &Path, to: &Path) -> std::io::Result<()> {
    lil_platform::fs::move_new(from, to)
}

/// Deletes a file, clearing `FILE_ATTRIBUTE_READONLY` first (`DeleteFile` fails with
/// `ERROR_ACCESS_DENIED` on a read-only file).
#[allow(clippy::disallowed_methods)]
pub fn remove_file(path: &Path) -> std::io::Result<()> {
    clear_readonly(path)?;
    std::fs::remove_file(long_path(path))
}

/// Deletes a folder tree, clearing `FILE_ATTRIBUTE_READONLY` on every file and folder in it
/// first. Links (symbolic links, junctions) are removed, never followed.
#[allow(clippy::disallowed_methods)]
pub fn remove_dir_all(path: &Path) -> std::io::Result<()> {
    fn clear(path: &Path) -> std::io::Result<()> {
        clear_readonly(path)?;
        let meta = std::fs::symlink_metadata(long_path(path))?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            for entry in std::fs::read_dir(long_path(path))? {
                clear(&path.join(entry?.file_name()))?;
            }
        }
        Ok(())
    }
    clear(path)?;
    std::fs::remove_dir_all(long_path(path))
}

/// Clears `FILE_ATTRIBUTE_READONLY` (nothing when it is clear); a missing path is left to the
/// call that follows to report.
#[allow(clippy::disallowed_methods)]
fn clear_readonly(path: &Path) -> std::io::Result<()> {
    match lil_platform::fs::set_readonly(path, false) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Opens `path` with no sharing (`lil_platform::fs::open_exclusive`), lets `decide` read it
/// through that handle, and deletes it through the same handle when `decide` says so; returns
/// whether it was deleted. Nobody else can open the file between the read and the delete, and
/// an existing handle on it (a writer still holding it) makes the open fail with error 32.
#[allow(clippy::disallowed_methods)]
pub fn remove_if(
    path: &Path,
    decide: &dyn Fn(&std::fs::File) -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    let held = lil_platform::fs::open_exclusive(path)?;
    if decide(held.file())? {
        held.delete()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Sets or clears `FILE_ATTRIBUTE_READONLY`.
#[allow(clippy::disallowed_methods)]
pub fn set_readonly(path: &Path, on: bool) -> std::io::Result<()> {
    lil_platform::fs::set_readonly(path, on)
}

/// `CopyFile2(COPY_FILE_FAIL_IF_EXISTS)`: never overwrites `to`.
#[allow(clippy::disallowed_methods)]
pub fn copy_new(from: &Path, to: &Path) -> std::io::Result<u64> {
    lil_platform::fs::copy_new(from, to)
}

/// Opens or creates the lock file `path` and takes its exclusive lock; `None` at once when
/// another handle holds it (`core.lock`, ARCH-028).
#[allow(clippy::disallowed_methods)]
pub fn lock_exclusive(path: &Path) -> std::io::Result<Option<LockedFile>> {
    lil_platform::fs::try_lock_exclusive(path)
}

/// Appends one line to the log `path` (created if missing) under the append lock
/// (`try_lock_append`, ARCH-081). `None` at once when another holder has the lock: the caller
/// polls under its retry budget. With the lock held the file is read, a torn last line is
/// closed with `\n`, `make_line` gets the text as it then is (so the seq comes from what is
/// really there), and the line goes out in one `write_all`. Returns the line.
#[allow(clippy::disallowed_methods)]
pub fn append_locked(
    path: &Path,
    make_line: &dyn Fn(&str) -> String,
) -> std::io::Result<Option<String>> {
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(long_path(path))?;
    let Some(lock) = lil_platform::fs::try_lock_append(&file)? else {
        return Ok(None);
    };
    let mut raw = Vec::new();
    (&file).seek(SeekFrom::Start(0))?;
    (&file).read_to_end(&mut raw)?;
    let mut text = String::from_utf8_lossy(&raw).into_owned();
    let mut out = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        out.push('\n');
        text.push('\n');
    }
    let line = make_line(&text);
    out.push_str(&line);
    (&file).write_all(out.as_bytes())?;
    drop(lock);
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-112
    fn ensure_dir_creates_nested_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b c");
        ensure_dir(&nested).expect("first call");
        ensure_dir(&nested).expect("an existing folder is fine");
        assert!(nested.is_dir());
    }

    #[test]
    fn atomic_write_replaces_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("data.txt");

        write_file_atomic(&target, b"a").expect("first write");
        write_file_atomic(&target, b"b").expect("second write");

        assert_eq!(std::fs::read(&target).expect("read back"), b"b");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test] // spec: ARCH-074, QA-011
    fn write_temp_never_reuses_a_name_and_cleans_up_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let fp = FailPoints::none();
        let temp = write_temp(dir.path(), ".a.lil-tmp-1-1", b"x", &fp).unwrap();
        assert_eq!(std::fs::read(&temp).unwrap(), b"x");
        let again = write_temp(dir.path(), ".a.lil-tmp-1-1", b"y", &fp).unwrap_err();
        assert_eq!(again.raw_os_error(), Some(80), "{again:?}");
        assert_eq!(std::fs::read(&temp).unwrap(), b"x");

        let full = FailPoints::parse("store.write_temp=disk_full").unwrap();
        let err = write_temp(dir.path(), ".a.lil-tmp-1-3", b"z", &full).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(112));
        assert!(!dir.path().join(".a.lil-tmp-1-3").exists());
    }

    #[test] // spec: ARCH-074
    fn move_new_never_replaces_and_remove_clears_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let fp = FailPoints::none();
        let a = write_temp(dir.path(), "a", b"a", &fp).unwrap();
        let b = write_temp(dir.path(), "b", b"b", &fp).unwrap();
        let err = move_new(&a, &b).unwrap_err();
        assert!(matches!(err.raw_os_error(), Some(80 | 183)), "{err:?}");
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        set_readonly(&b, true).unwrap();
        remove_file(&b).unwrap();
        assert!(!b.exists());

        let tree = dir.path().join("t");
        ensure_dir(&tree.join("sub")).unwrap();
        let sealed = write_temp(&tree.join("sub"), "x.png", b"x", &fp).unwrap();
        set_readonly(&sealed, true).unwrap();
        set_readonly(&tree.join("sub"), true).unwrap();
        remove_dir_all(&tree).unwrap();
        assert!(!tree.exists());
    }

    #[test] // spec: ARCH-074
    fn remove_if_deletes_only_what_it_decided_on() {
        let dir = tempfile::tempdir().unwrap();
        let fp = FailPoints::none();
        let path = write_temp(dir.path(), ".a.lil-bak-1-1", b"old", &fp).unwrap();
        let read = |f: &std::fs::File| {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut &*f, &mut s)?;
            Ok::<_, std::io::Error>(s)
        };
        assert!(!remove_if(&path, &|f| Ok(read(f)? == "other")).unwrap());
        assert!(path.exists());
        assert!(remove_if(&path, &|f| Ok(read(f)? == "old")).unwrap());
        assert!(!path.exists());
        let gone = remove_if(&path, &|_| Ok(true)).unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
    }

    #[test] // spec: ARCH-081
    fn append_locked_closes_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("jobs.jsonl");
        let fp = FailPoints::none();
        let torn = write_temp(dir.path(), "jobs.jsonl", b"{\"a\":1}\n{\"b\"", &fp).unwrap();
        assert_eq!(torn, log);
        let seen = std::sync::Mutex::new(String::new());
        let line = append_locked(&log, &|text| {
            *seen.lock().unwrap() = text.to_string();
            "{\"c\":3}\n".to_string()
        })
        .unwrap()
        .unwrap();
        assert_eq!(line, "{\"c\":3}\n");
        assert_eq!(*seen.lock().unwrap(), "{\"a\":1}\n{\"b\"\n");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "{\"a\":1}\n{\"b\"\n{\"c\":3}\n"
        );
    }
}
