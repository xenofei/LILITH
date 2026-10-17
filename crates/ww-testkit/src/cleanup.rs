//! Removing a [`crate::TestEnv`]'s temporary folder when it drops.
//!
//! `tempfile::TempDir` calls `std::fs::remove_dir_all` and discards the error. That call stops
//! at the first entry it cannot delete. On Windows this is usually a file that another handle
//! holds open without delete sharing, which is how SQLite opens its databases: for example a
//! `state.db` that a core which has just closed its pipe, or a value the test drops after the
//! `TestEnv`, still has open. The whole folder then stays in `%TEMP%`, and nothing says so.
//!
//! [`remove_tree`] instead:
//! - never follows a junction or symbolic link: it removes the link itself and leaves the
//!   target alone;
//! - clears the read-only attribute (sealed version files) only on entries it reached by
//!   walking down from the root without following links, so only inside the test's own folder;
//! - keeps going past an entry it cannot delete, then walks the tree again after a short pause,
//!   for up to [`RETRY_FOR`], while every failure is a sharing, lock or delete-pending error;
//! - returns what it could not remove, which [`report_leftovers`] writes to stderr and appends
//!   to [`leftovers_log`].

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::windows::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long [`remove_tree`] keeps retrying entries that another handle still holds.
pub(crate) const RETRY_FOR: Duration = Duration::from_secs(2);
/// The first pause between passes; it doubles up to [`MAX_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(10);
const MAX_PAUSE: Duration = Duration::from_millis(200);
/// How many leftover entries a report names.
const REPORT_LIMIT: usize = 20;

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;
const ERROR_DIR_NOT_EMPTY: i32 = 145;

/// An entry [`remove_tree`] could not remove, with the last error it got.
pub(crate) type Leftover = (PathBuf, io::Error);

/// Removes `root` and everything under it, without following links out of it (see the module
/// documentation). `Ok` once `root` is gone or was never there; otherwise the entries left and
/// their last errors. Never panics.
pub(crate) fn remove_tree(root: &Path, retry_for: Duration) -> Result<(), Vec<Leftover>> {
    let deadline = Instant::now() + retry_for;
    let mut pause = FIRST_PAUSE;
    loop {
        let mut left = Vec::new();
        remove_entry(root, root, &mut left);
        if left.is_empty() {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline || !left.iter().all(|(_, e)| held_by_another_handle(e)) {
            return Err(left);
        }
        std::thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(MAX_PAUSE);
    }
}

/// Errors that go away once another handle closes: a sharing or lock violation, access denied
/// (a file whose delete is still pending), or a folder that is not empty yet because a child's
/// delete is still pending.
fn held_by_another_handle(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(
            ERROR_ACCESS_DENIED
                | ERROR_SHARING_VIOLATION
                | ERROR_LOCK_VIOLATION
                | ERROR_DIR_NOT_EMPTY
        )
    )
}

/// Removes `path`, which lies under `root`, and records in `left` what could not be removed.
fn remove_entry(root: &Path, path: &Path, left: &mut Vec<Leftover>) {
    // Every path here is `root` plus names read from folders that are not links, so this
    // holds; the check keeps a future change from ever touching anything outside the root.
    if !path.starts_with(root) {
        left.push((
            path.to_path_buf(),
            io::Error::other("outside the test folder; not touched"),
        ));
        return;
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            left.push((path.to_path_buf(), e));
            return;
        }
    };
    let attributes = meta.file_attributes();
    let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let result = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        // A junction or symbolic link: remove the link itself, never what it points to. Its
        // attributes are left alone too, since setting them would follow the link.
        if is_dir {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
    } else {
        if attributes & FILE_ATTRIBUTE_READONLY != 0 {
            // Inside the root and not a link (checked above), so this clears only the test's
            // own sealed files and folders. A failure shows up as the delete's error below.
            let _ = clear_read_only(path, &meta);
        }
        if is_dir {
            let before = left.len();
            match fs::read_dir(path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => remove_entry(root, &entry.path(), left),
                            Err(e) => left.push((path.to_path_buf(), e)),
                        }
                    }
                }
                Err(e) => left.push((path.to_path_buf(), e)),
            }
            if left.len() > before {
                // Something below is still there, so this folder is too; the entries below
                // are what the report names.
                return;
            }
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
    };
    match result {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => left.push((path.to_path_buf(), e)),
    }
}

/// Clears `FILE_ATTRIBUTE_READONLY` on `path`, which must not be a link.
fn clear_read_only(path: &Path, meta: &fs::Metadata) -> io::Result<()> {
    let mut permissions = meta.permissions();
    // Windows only: this clears FILE_ATTRIBUTE_READONLY. The Unix mode bits the lint warns
    // about do not exist here.
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

/// The file [`report_leftovers`] appends to: `%TEMP%\ww-testenv-leftovers.log`, beside the
/// `ww-testenv-*` folders it describes.
pub fn leftovers_log() -> PathBuf {
    std::env::temp_dir().join("ww-testenv-leftovers.log")
}

/// Says that `root` could not be fully removed and what is left: on stderr, and appended to
/// [`leftovers_log`] with the time, the process and the test (the current thread's name, which
/// the test harness sets to the test's name). Never panics.
pub(crate) fn report_leftovers(root: &Path, left: &[Leftover]) {
    let test = std::thread::current()
        .name()
        .unwrap_or("<unnamed thread>")
        .to_string();
    let when = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut text = format!(
        "ww-testkit: TestEnv could not remove {} (unix time {when}, pid {}, test `{test}`); \
         {} entries left:\n",
        root.display(),
        std::process::id(),
        left.len()
    );
    for (path, e) in left.iter().take(REPORT_LIMIT) {
        text.push_str(&format!("  {}: {e}\n", path.display()));
    }
    if left.len() > REPORT_LIMIT {
        text.push_str(&format!("  ... and {} more\n", left.len() - REPORT_LIMIT));
    }
    let _ = io::stderr().write_all(text.as_bytes());
    // One write per report, so reports from tests running in parallel do not interleave.
    if let Ok(mut log) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(leftovers_log())
    {
        let _ = log.write_all(text.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;

    /// Opens `path` the way SQLite does: read and write sharing, no delete sharing.
    fn hold_like_sqlite(path: &Path) -> fs::File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .unwrap()
    }

    #[test] // spec: QA-040
    fn a_file_held_past_the_budget_is_reported_and_its_siblings_still_go() {
        let dir = tempfile::Builder::new()
            .prefix("ww-testkit-cleanup-")
            .tempdir()
            .unwrap();
        let root = dir.path().join("root");
        for name in ["a", "b", "c"] {
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("f.txt"), name).unwrap();
        }
        // "a" sorts first: std's remove_dir_all would stop here and leave "b" and "c" too.
        let held = root.join("a").join("f.txt");
        let handle = hold_like_sqlite(&held);

        let left = remove_tree(&root, Duration::from_millis(100)).unwrap_err();

        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].0, held);
        assert_eq!(left[0].1.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
        assert!(!root.join("b").exists() && !root.join("c").exists());
        drop(handle);
        remove_tree(&root, Duration::ZERO).unwrap();
        assert!(!root.exists());
    }

    #[test] // spec: QA-040
    fn a_file_released_within_the_budget_is_removed() {
        let dir = tempfile::Builder::new()
            .prefix("ww-testkit-cleanup-")
            .tempdir()
            .unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let held = root.join("state.db");
        fs::write(&held, "db").unwrap();
        let handle = hold_like_sqlite(&held);
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(handle);
        });

        let result = remove_tree(&root, Duration::from_secs(10));

        release.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(!root.exists());
    }

    #[test] // spec: QA-040
    fn a_missing_root_is_already_removed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(remove_tree(&dir.path().join("never-made"), Duration::ZERO).is_ok());
    }
}
