//! The thumbnail cache's files (Appendix H "Thumbnails"; APP-038, ARCH-097): in the
//! per-workspace local data folder, `<local dir>\thumbs\<sha256[0..2]>\<sha256>.<renderer>@<version>.<px>.png`,
//! keyed by the SHA-256 of the bytes a render read (P4b-R36). A `.t<ms>` segment before
//! `.png` is reserved for a video's poster time; plan 4b writes none, and [`parse_entry`]
//! reads it. Writes go through `fsops::write_workspace_local_file`, removals through
//! `fsops::remove_workspace_local_file`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The cache folder's name in the local data folder.
pub const THUMBS_DIR: &str = "thumbs";
/// Clean-up keeps an entry whose hash nothing live holds while it was written within this.
pub const KEEP_UNTOUCHED: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The cache folder of the local data folder `local_dir`.
pub fn thumbs_dir(local_dir: &Path) -> PathBuf {
    local_dir.join(THUMBS_DIR)
}

/// `<sha256>.<renderer>@<version>.<px>.png`.
pub fn entry_name(sha256: &str, renderer: &str, version: u32, px: u32) -> String {
    format!("{sha256}.{renderer}@{version}.{px}.png")
}

/// The cache file of `sha256` drawn by `renderer@version` at `px`.
pub fn entry_path(
    local_dir: &Path,
    sha256: &str,
    renderer: &str,
    version: u32,
    px: u32,
) -> PathBuf {
    thumbs_dir(local_dir)
        .join(sha256.get(..2).unwrap_or_default())
        .join(entry_name(sha256, renderer, version, px))
}

/// Whether `s` is a SHA-256 in bare lowercase hex.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The SHA-256 of a cache file's name, when the name is one of the cache's:
/// `<sha256>.<renderer>@<version>.<px>[.t<ms>].png`.
pub fn parse_entry(name: &str) -> Option<&str> {
    let rest = name.strip_suffix(".png")?;
    let mut parts = rest.split('.');
    let sha = parts.next()?;
    let renderer = parts.next()?;
    let px = parts.next()?;
    let poster = parts.next();
    if parts.next().is_some() || !is_sha256(sha) {
        return None;
    }
    let (id, version) = renderer.split_once('@')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let ok = !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_lowercase())
        && digits(version)
        && digits(px)
        && poster.is_none_or(|t| t.strip_prefix('t').is_some_and(digits));
    ok.then_some(sha)
}

/// One cache file found by [`list_entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub sha256: String,
    /// Its last write.
    pub modified: SystemTime,
}

/// Every cache file under `<local dir>\thumbs\<shard>\` whose name [`parse_entry`] reads and
/// whose shard is its hash's first two characters. Links are never followed, and anything
/// else (temp files, other names) is left out.
pub fn list_entries(local_dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let Ok(shards) = std::fs::read_dir(thumbs_dir(local_dir)) else {
        return out;
    };
    for shard in shards.flatten() {
        let is_dir = shard
            .file_type()
            .is_ok_and(|t| t.is_dir() && !t.is_symlink());
        if !is_dir {
            continue;
        }
        let shard_name = shard.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(file.path()) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(sha) = parse_entry(&name) else {
                continue;
            };
            if sha[..2] != shard_name {
                continue;
            }
            let Ok(modified) = meta.modified() else {
                continue;
            };
            out.push(Entry {
                path: file.path(),
                sha256: sha.to_string(),
                modified,
            });
        }
    }
    out
}
