//! A sealed version's content hash (CORE-047; Appendix E "Refs").
//!
//! The SHA-256 of a text with one line per file in `v<version>/`, except `source/**` and
//! `manifest.snapshot.json`. The lines are sorted by `/`-separated relative path in byte order,
//! and each reads `<sha256 hex>  <relative path>\n`.

use sha2::{Digest, Sha256};

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// SHA-256 of `bytes`, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The content hash of a version. `files` holds `(path relative to v<version>/ with '/',
/// sha256 hex)` pairs in any order.
pub fn version_content_hash(files: &[(String, String)]) -> String {
    let mut lines: Vec<(&str, &str)> = files
        .iter()
        .filter(|(rel, _)| !rel.starts_with("source/") && rel != "manifest.snapshot.json")
        .map(|(rel, sha)| (rel.as_str(), sha.as_str()))
        .collect();
    lines.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = Sha256::new();
    for (rel, sha) in lines {
        hasher.update(format!("{sha}  {rel}\n").as_bytes());
    }
    hex(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0eaf94256e9ad75cf113dceeb38e3c2730ae35d2888f625f6418165b41f743d3";
    const B: &str = "5cd3c91a2cdc2ee698eabfe804b94b30db24f3dccb166a26eb06718830bc6b52";

    #[test] // spec: CORE-047
    fn content_hash_follows_the_contract() {
        // Built by hand: sorted by path in byte order ("B.png" < "a.png"), two spaces.
        let text = format!("{B}  B.png\n{A}  a.png\n");
        let want = hex(&Sha256::digest(text.as_bytes()));

        let files = vec![
            ("a.png".to_string(), A.to_string()),
            ("source/a.png".to_string(), B.to_string()),
            ("manifest.snapshot.json".to_string(), B.to_string()),
            ("B.png".to_string(), B.to_string()),
        ];
        assert_eq!(version_content_hash(&files), want);

        // Order of the input doesn't matter; ignored files don't count.
        let mut reversed = files.clone();
        reversed.reverse();
        assert_eq!(version_content_hash(&reversed), want);
        assert_eq!(
            version_content_hash(&[
                ("B.png".to_string(), B.to_string()),
                ("a.png".to_string(), A.to_string()),
            ]),
            want
        );
        // A file merely named `source` does count.
        assert_ne!(
            version_content_hash(&[
                ("B.png".to_string(), B.to_string()),
                ("a.png".to_string(), A.to_string()),
                ("source".to_string(), A.to_string()),
            ]),
            want
        );
        // The empty version hashes the empty text.
        assert_eq!(version_content_hash(&[]), sha256_hex(b""));
        assert_eq!(want.len(), 64);
    }
}
