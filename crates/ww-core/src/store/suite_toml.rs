//! `suite.toml` writes (CORE-007, ARCH-090; owner decision 1): the core is the file's only
//! writer. `ww workspace init|use` reach it through the tools `workspace.create` and
//! `workspace.set_default`.
//!
//! An edit is targeted (`ww_schema::suite_edit::register`, through `toml_edit`), so comments,
//! key order and unknown keys and tables survive. The write is the store's swap, applied to a
//! file outside any workspace:
//! 1. read the file (no file is an empty text) and take its SHA-256;
//! 2. `register`; an unchanged text is not written;
//! 3. the fail point `store.suite_toml`;
//! 4. hash the file again: a change since step 1 is an outside change;
//! 5. write `.suite.toml.ww-tmp-<pid>-<n>` beside it (flushed before it is closed) and swap it
//!    in: `ReplaceFileW` with the backup `.suite.toml.ww-bak-<pid>-<n>` for an existing file,
//!    `move_new` for a file that doesn't exist yet (`ReplaceFileW` needs an existing target).
//!    Errors 80 and 183 from `move_new` mean another writer created the file meanwhile, an
//!    outside change;
//! 6. hash the backup: equal to step 1's hash, it is removed. Otherwise an outside edit landed
//!    between steps 4 and 5: it is put back with `ReplaceFileW` (the second backup, which then
//!    holds this core's bytes, is removed), and that is an outside change too.
//!
//! An outside change re-reads and re-applies once; a second one is `store.hash_conflict`.
//! Sharing violations are retried under the store's budget (`store.busy` after 10 s).

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use ww_platform::Env;
use ww_platform::fs::{RetryClass, classify};
use ww_platform::paths::folder_key;
use ww_schema::{sha256_hex, suite_edit};

use super::failpoints::FailPoints;
use super::fsops;
use super::paths::WsPath;
use super::write::{Backoff, StoreError};

/// The name `suite.toml` errors carry as their path.
const LABEL: &str = "suite.toml";

/// Keeps the `<pid>-<n>` names of concurrent writes apart; steps by 2, so `n + 1` (the
/// put-back's second backup) never collides with another write's `n`.
static NEXT_N: AtomicU64 = AtomicU64::new(1);

/// How one swap attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Swap {
    Done,
    /// The file changed outside this core since it was read; nothing of the change was lost.
    Changed,
}

/// Registers the workspace folder `path` in `env`'s `suite.toml` unless it is registered
/// already (folders compared with `folder_key`, ruling Q6: the stored spelling is kept), and
/// with `make_default` makes it `default_workspace`. Returns whether the file was written.
///
/// # Errors
/// - `store.path_invalid`: `path` isn't valid Unicode;
/// - `store.io`: the file can't be read or written, isn't UTF-8 or isn't TOML the editor can
///   change (it is left as it is);
/// - `store.hash_conflict`: it changed outside the core twice while this ran;
/// - `store.busy`, `store.disk_full`.
pub async fn register_workspace(
    env: &Env,
    path: &Path,
    make_default: bool,
    fp: &FailPoints,
) -> Result<bool, StoreError> {
    let file = env.suite_toml();
    let spelling = path.to_str().ok_or_else(|| StoreError::PathInvalid {
        path: path.display().to_string(),
        why: "a workspace path must be valid Unicode".to_string(),
    })?;
    let key = |p: &str| folder_key(Path::new(p));
    let mut changed_once = false;
    loop {
        let read = match std::fs::read(&file) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(error(&file, &e)),
        };
        let text = match &read {
            None => "",
            Some(bytes) => std::str::from_utf8(bytes).map_err(|_| StoreError::Io {
                path: file.display().to_string(),
                why: "suite.toml isn't UTF-8 text, so it was left as it is".to_string(),
            })?,
        };
        let new = suite_edit::register(text, spelling, make_default, &key).map_err(|why| {
            StoreError::Io {
                path: file.display().to_string(),
                why: format!("{why}; it was left as it is"),
            }
        })?;
        if read.is_some() && new == text {
            return Ok(false);
        }
        fp.hit("store.suite_toml").map_err(|e| error(&file, &e))?;
        let expected = read.as_deref().map(sha256_hex);
        match swap(&file, expected.as_deref(), new.as_bytes(), fp).await? {
            Swap::Done => {
                tracing::info!(file = %file.display(), workspace = spelling, make_default, "suite.toml written");
                return Ok(true);
            }
            Swap::Changed if !changed_once => {
                changed_once = true;
                tracing::info!(file = %file.display(), "suite.toml changed outside the core; re-reading");
            }
            Swap::Changed => {
                tracing::warn!(file = %file.display(), "suite.toml changed outside the core again; not written");
                return Err(StoreError::HashConflict {
                    path: label(),
                    backup: None,
                    cloud_only: false,
                    kept: Vec::new(),
                });
            }
        }
    }
}

/// Steps 4–6, retrying sharing violations (and errors 1175/1176, which leave both files under
/// their own names) under the store's budget.
async fn swap(
    file: &Path,
    expected: Option<&str>,
    bytes: &[u8],
    fp: &FailPoints,
) -> Result<Swap, StoreError> {
    let mut backoff = Backoff::new();
    loop {
        match swap_once(file, expected, bytes, fp) {
            Ok(swap) => return Ok(swap),
            Err(e)
                if matches!(
                    classify(&e),
                    RetryClass::SharingViolation | RetryClass::ReplaceRetry
                ) =>
            {
                if !backoff.wait().await {
                    tracing::warn!(file = %file.display(), error = %e, "suite.toml stayed held past the retry budget");
                    return Err(StoreError::Busy(label()));
                }
            }
            Err(e) => return Err(error(file, &e)),
        }
    }
}

/// One attempt at steps 4–6. Small and quick: `suite.toml` is a few lines.
fn swap_once(
    file: &Path,
    expected: Option<&str>,
    bytes: &[u8],
    fp: &FailPoints,
) -> io::Result<Swap> {
    let dir = file
        .parent()
        .ok_or_else(|| io::Error::other("suite.toml has no folder"))?;
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::other("suite.toml has no file name"))?;
    fsops::ensure_dir(dir)?;
    // Step 4.
    if hash_of(file)?.as_deref() != expected {
        return Ok(Swap::Changed);
    }
    // Step 5.
    let (pid, n) = (std::process::id(), NEXT_N.fetch_add(2, Ordering::Relaxed));
    let b1 = dir.join(format!(".{name}.ww-bak-{pid}-{n}"));
    let b2 = dir.join(format!(".{name}.ww-bak-{pid}-{}", n + 1));
    let temp = fsops::write_temp(dir, &format!(".{name}.ww-tmp-{pid}-{n}"), bytes, fp)?;
    let placed = place(file, &temp, &b1, &b2, expected);
    if temp.exists() {
        let _ = fsops::remove_file(&temp);
    }
    placed
}

/// Moves `temp` onto `file` and checks the backup (steps 5 and 6).
fn place(
    file: &Path,
    temp: &Path,
    b1: &Path,
    b2: &Path,
    expected: Option<&str>,
) -> io::Result<Swap> {
    let Some(expected) = expected else {
        return match fsops::move_new(temp, file) {
            Ok(()) => Ok(Swap::Done),
            Err(e) if classify(&e) == RetryClass::TargetExists => Ok(Swap::Changed),
            Err(e) => Err(e),
        };
    };
    match fsops::replace(file, temp, b1) {
        Ok(()) => {}
        // The old file is under the backup name and the new one still under its temp name.
        Err(e) if classify(&e) == RetryClass::ReplacementNotMoved => fsops::move_new(temp, file)?,
        Err(e) => return Err(e),
    }
    // Step 6.
    if hash_of(b1)?.as_deref() == Some(expected) {
        fsops::remove_file(b1)?;
        return Ok(Swap::Done);
    }
    // An outside edit landed between the check and the replace: it goes back under its name.
    fsops::replace(file, b1, b2)?;
    fsops::remove_file(b2)?;
    Ok(Swap::Changed)
}

/// The SHA-256 of the file at `path`, `None` when there is none.
fn hash_of(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn label() -> WsPath {
    WsPath::parse(LABEL).expect("`suite.toml` is a valid relative path")
}

/// The store error for an I/O error on `suite.toml`.
fn error(file: &Path, e: &io::Error) -> StoreError {
    match classify(e) {
        RetryClass::SharingViolation => StoreError::Busy(label()),
        RetryClass::DiskFull => StoreError::DiskFull(label()),
        _ => StoreError::Io {
            path: file.display().to_string(),
            why: e.to_string(),
        },
    }
}
