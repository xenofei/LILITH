//! The store: the only module in `ww-core` allowed to write files (ARCH-112, QA-024).

use std::io::Write as _;
use std::path::Path;

/// Writes `bytes` to `path` atomically: the data goes to `<path>.tmp-<pid>`, is flushed to disk
/// with `sync_all`, and is then renamed over the target.
///
/// This is the single file-write allowance in `ww-core` (ARCH-112); it is a stub that plan 2
/// completes (directory fsync, crash-recovery of stale temp files, failure injection).
///
/// On Windows, `std::fs::rename` replaces an existing target (MoveFileEx with
/// replace-existing).
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

#[cfg(test)]
mod tests {
    use super::write_file_atomic;

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
}
