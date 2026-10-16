//! File helpers for tests.

use std::fmt::Write as _;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The SHA-256 of a file's bytes as 64 lowercase hex characters.
///
/// # Panics
/// If the file cannot be read.
pub fn sha256_file(path: &Path) -> String {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("cannot read {} to hash it: {e}", path.display()));
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-031
    fn sha256_file_matches_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
