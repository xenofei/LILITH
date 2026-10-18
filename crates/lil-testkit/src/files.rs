//! File helpers for tests.
//!
//! [`write_file`] is how `lil-core` tests create fixture files: `crates/lil-core/src` may not call
//! `std::fs` writers outside `store/fsops.rs` (QA-024, `single_write_allowance.rs`), and its unit
//! tests live there too.

use std::fmt::Write as _;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The SHA-256 of `bytes` as 64 lowercase hex characters.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// The SHA-256 of a file's bytes as 64 lowercase hex characters.
///
/// # Panics
/// If the file cannot be read.
pub fn sha256_file(path: &Path) -> String {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("cannot read {} to hash it: {e}", path.display()));
    sha256_bytes(&bytes)
}

/// Writes `bytes` to `path`, creating its parent folders, replacing any existing file.
///
/// # Panics
/// If a folder or the file cannot be written.
pub fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-031
    fn sha256_file_matches_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_file(&path), abc);
        assert_eq!(sha256_bytes(b"abc"), abc);
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test] // spec: QA-024
    fn write_file_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("Ünïcödé space")
            .join("two")
            .join("three.txt");
        write_file(&path, b"first");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_file(&path, b"second");
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        // An existing parent is fine too.
        let beside = path.with_file_name("beside.bin");
        write_file(&beside, &[0, 13, 10, 255]);
        assert_eq!(std::fs::read(&beside).unwrap(), [0, 13, 10, 255]);
        // A parent that is a file is a panic, not a silent success.
        let under_file = path.join("x.txt");
        assert!(std::panic::catch_unwind(|| write_file(&under_file, b"x")).is_err());
    }
}
