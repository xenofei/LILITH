//! `HandleHolder`: a file held open by "someone else", as Defender, a preview pane or an
//! image viewer holds it (QA-066, ARCH-074). The store's sharing-violation retries and the
//! watcher's parking of locked files are tested against it.

use std::fs::File;
use std::path::{Path, PathBuf};

use lil_platform::fs::{Share, open_shared};

/// An open read handle on a file, with exactly the sharing given to [`HandleHolder::hold`].
/// The handle closes on [`HandleHolder::release`] or on drop.
#[derive(Debug)]
pub struct HandleHolder {
    _file: File,
    path: PathBuf,
    share: Share,
}

impl HandleHolder {
    /// Opens the existing file `path` for reading, allowing others only `share`.
    ///
    /// # Panics
    /// If the file cannot be opened that way.
    pub fn hold(path: &Path, share: Share) -> HandleHolder {
        let file = open_shared(path, share)
            .unwrap_or_else(|e| panic!("cannot hold {} ({share:?}): {e}", path.display()));
        HandleHolder {
            _file: file,
            path: path.to_path_buf(),
            share,
        }
    }

    /// The held file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sharing the handle allows others.
    pub fn share(&self) -> Share {
        self.share
    }

    /// Closes the handle.
    pub fn release(self) {
        drop(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::fs::{RetryClass, classify, replace_file};

    #[test] // spec: QA-066
    fn handle_holder_blocks_replace_until_released() {
        let dir = tempfile::Builder::new()
            .prefix("lil-handles-Ünïcödé space-")
            .tempdir()
            .unwrap();
        let target = dir.path().join("target.txt");
        let replacement = dir.path().join(".target.txt.lil-tmp-1-1");
        let backup = dir.path().join(".target.txt.lil-bak-1-1");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&replacement, b"new").unwrap();

        for share in [Share::Read, Share::None, Share::ReadWrite] {
            let held = HandleHolder::hold(&target, share);
            assert_eq!(held.path(), target);
            assert_eq!(held.share(), share);
            let err = replace_file(&target, &replacement, &backup)
                .expect_err("a replace while another handle holds the target");
            // Observed on the plan-2b test machine (Windows 11, 2026-10-16): every share mode
            // gives error 32, ERROR_SHARING_VIOLATION, which the store retries.
            assert_eq!(err.raw_os_error(), Some(32), "{share:?}: {err:?}");
            assert_eq!(classify(&err), RetryClass::SharingViolation, "{share:?}");
            held.release();
            // Nothing moved: the old content, and the replacement still under its own name
            // (read after the release, since `Share::None` refuses even readers).
            assert_eq!(std::fs::read(&target).unwrap(), b"old");
            assert_eq!(std::fs::read(&replacement).unwrap(), b"new");
            assert!(!backup.exists());
        }

        replace_file(&target, &replacement, &backup).expect("the replace after release");
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(&backup).unwrap(), b"old");
        assert!(!replacement.exists());
    }
}
