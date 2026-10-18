//! `suite.toml` writes (CORE-007, ARCH-090; owner decision 1): the core is the file's only
//! writer. `lil workspace init|use` reach it through the tools `workspace.create` and
//! `workspace.set_default`.
//!
//! An edit is targeted (`lil_schema::suite_edit::register`, through `toml_edit`), so comments,
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
//! 6. the backup is removed only if its hash equals step 1's (hashed and deleted through one
//!    unshared handle, the store's `remove_if_hash`). Otherwise an outside edit landed between
//!    steps 4 and 5: it is put back with `ReplaceFileW`, and the second backup is removed only
//!    if it holds this core's bytes. Anything else in it is a newer outside save, which the
//!    store's `set_aside` keeps beside the file as `suite.toml.ww-conflict-<utc>`.
//!
//! The fail points are the store's at the same steps: `store.replace` just before the replace,
//! `store.after_replace` just after it, and `store.put_back` after the put-back's replace.
//!
//! An outside change re-reads and re-applies once; a second one is `store.hash_conflict`, and
//! so is a put-back that kept a newer save (`data.kept` names the copy). Sharing violations
//! are retried under the store's budget (`store.busy` after 10 s). A failure after the
//! replace is `store.io` naming the backup, which then holds the replaced version and stays.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lil_platform::Env;
use lil_platform::fs::{RetryClass, classify};
use lil_platform::paths::folder_key;
use lil_schema::{sha256_hex, suite_edit};

use super::failpoints::FailPoints;
use super::fsops;
use super::paths::WsPath;
use super::write::{Backoff, StoreError, remove_if_hash, remove_temp, set_aside};

/// The name `suite.toml` errors carry as their path.
const LABEL: &str = "suite.toml";

/// Keeps the `<pid>-<n>` names of concurrent writes apart; steps by 2, so `n + 1` (the
/// put-back's second backup) never collides with another write's `n`.
static NEXT_N: AtomicU64 = AtomicU64::new(1);

/// How one swap attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Swap {
    Done,
    /// The file changed outside this core since it was read; nothing of the change was lost.
    Changed,
    /// The put-back met a newer outside save, which was kept at this visible copy.
    Kept(PathBuf),
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
            // Two outside saves met this write: the first is back under the name, the newer
            // one is kept beside it. Neither is overwritten by a retry.
            Swap::Kept(copy) => {
                let kept = copy
                    .file_name()
                    .and_then(|n| WsPath::parse(&n.to_string_lossy()).ok())
                    .into_iter()
                    .collect();
                return Err(StoreError::HashConflict {
                    path: label(),
                    backup: None,
                    cloud_only: false,
                    kept,
                });
            }
        }
    }
}

/// Why a swap attempt failed.
#[derive(Debug)]
enum SwapError {
    /// Before `suite.toml` changed: nothing to clean up; sharing violations are retried.
    Before(io::Error),
    /// After `ReplaceFileW` moved a version of `suite.toml` into `backup`: that file may hold
    /// the earlier version or an outside one, so it stays and the error names it.
    After { backup: PathBuf, error: io::Error },
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
        match swap_once(file, expected, bytes, fp).await {
            Ok(swap) => return Ok(swap),
            Err(SwapError::Before(e)) if retryable(&e) => {
                if !backoff.wait().await {
                    tracing::warn!(file = %file.display(), error = %e, "suite.toml stayed held past the retry budget");
                    return Err(StoreError::Busy(label()));
                }
            }
            Err(SwapError::Before(e)) => return Err(error(file, &e)),
            Err(SwapError::After { backup, error }) => {
                tracing::warn!(file = %file.display(), backup = %backup.display(), %error, "a suite.toml swap stopped after the replace; the backup stays");
                return Err(StoreError::Io {
                    path: backup.display().to_string(),
                    why: format!(
                        "writing suite.toml stopped after its replace ({error}); this file \
                         keeps the version of suite.toml that was replaced, which may hold \
                         another program's change, so compare it with suite.toml before you \
                         retry"
                    ),
                });
            }
        }
    }
}

/// Sharing violations and errors 1175/1176 leave every file under its own name: retried.
fn retryable(e: &io::Error) -> bool {
    matches!(
        classify(e),
        RetryClass::SharingViolation | RetryClass::ReplaceRetry
    )
}

/// `op`, retried while [`retryable`], under the store's budget.
async fn retrying<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut backoff = Backoff::new();
    loop {
        match op() {
            Err(e) if retryable(&e) && backoff.wait().await => {}
            other => return other,
        }
    }
}

/// One attempt at steps 4–6. Small and quick: `suite.toml` is a few lines.
async fn swap_once(
    file: &Path,
    expected: Option<&str>,
    bytes: &[u8],
    fp: &FailPoints,
) -> Result<Swap, SwapError> {
    let before = SwapError::Before;
    let dir = file
        .parent()
        .ok_or_else(|| before(io::Error::other("suite.toml has no folder")))?;
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| before(io::Error::other("suite.toml has no file name")))?;
    fsops::ensure_dir(dir).map_err(before)?;
    // Step 4.
    if hash_of(file).map_err(before)?.as_deref() != expected {
        return Ok(Swap::Changed);
    }
    // Step 5.
    let (pid, n) = (std::process::id(), NEXT_N.fetch_add(2, Ordering::Relaxed));
    let names = Names {
        dir,
        name: &name,
        b1: dir.join(format!(".{name}.ww-bak-{pid}-{n}")),
        b2: dir.join(format!(".{name}.ww-bak-{pid}-{}", n + 1)),
    };
    let temp =
        fsops::write_temp(dir, &format!(".{name}.ww-tmp-{pid}-{n}"), bytes, fp).map_err(before)?;
    let placed = place(file, &temp, &names, expected, &sha256_hex(bytes), fp).await;
    if temp.exists() {
        let _ = remove_temp(&temp);
    }
    placed
}

/// The names beside `suite.toml` for one attempt.
struct Names<'a> {
    dir: &'a Path,
    name: &'a str,
    b1: PathBuf,
    b2: PathBuf,
}

/// Moves `temp` onto `file` and checks the backup (steps 5 and 6). The fail points
/// `store.replace` (just before the replace), `store.after_replace` (just after it) and
/// `store.put_back` (after the put-back's replace) are the store's, at the same steps.
async fn place(
    file: &Path,
    temp: &Path,
    names: &Names<'_>,
    expected: Option<&str>,
    new_sha256: &str,
    fp: &FailPoints,
) -> Result<Swap, SwapError> {
    let Some(expected) = expected else {
        return match fsops::move_new(temp, file) {
            Ok(()) => Ok(Swap::Done),
            Err(e) if classify(&e) == RetryClass::TargetExists => Ok(Swap::Changed),
            Err(e) => Err(SwapError::Before(e)),
        };
    };
    let (b1, b2) = (&names.b1, &names.b2);
    let after = |backup: &Path| {
        let backup = backup.to_path_buf();
        move |error| SwapError::After { backup, error }
    };
    fp.hit("store.replace").map_err(SwapError::Before)?;
    match fsops::replace(file, temp, b1) {
        Ok(()) => {}
        // The old file is under the backup name and the new one still under its temp name.
        Err(e) if classify(&e) == RetryClass::ReplacementNotMoved => {
            fsops::move_new(temp, file).map_err(after(b1))?;
        }
        Err(e) => return Err(SwapError::Before(e)),
    }
    fp.hit("store.after_replace").map_err(after(b1))?;
    // Step 6: the backup is removed only if it is exactly what step 4 checked.
    match retrying(|| remove_if_hash(b1, expected)).await {
        Ok(true) => return Ok(Swap::Done),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            tracing::warn!(backup = %b1.display(), "the suite.toml backup vanished before it was checked");
            return Ok(Swap::Done);
        }
        Err(e) => return Err(after(b1)(e)),
        Ok(false) => {}
    }
    // An outside edit landed between the check and the replace: it goes back under its name,
    // and the target (this core's bytes, or a newer outside save) goes to the second backup.
    tracing::warn!(file = %file.display(), "an outside edit landed during a suite.toml write; putting it back");
    match retrying(|| fsops::replace(file, b1, b2)).await {
        Ok(()) => {}
        // The target went to the second backup; the outside edit is still in the first.
        Err(e) if classify(&e) == RetryClass::ReplacementNotMoved => {
            fsops::move_new(b1, file).map_err(after(b1))?;
        }
        Err(e) => return Err(after(b1)(e)),
    }
    fp.hit("store.put_back").map_err(after(b2))?;
    // The second backup is deleted only if it holds this core's bytes; anything else is a
    // newer outside save, kept beside the file (Task 6's put-back rule).
    match retrying(|| remove_if_hash(b2, new_sha256)).await {
        Ok(true) => Ok(Swap::Changed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Swap::Changed),
        Ok(false) => {
            let stamp = lil_schema::time::compact(now_secs());
            let kept = retrying(|| set_aside(b2, names.dir, names.name, &stamp))
                .await
                .map_err(after(b2))?;
            tracing::warn!(kept = %kept.display(), "a newer outside save of suite.toml met the put-back; kept beside it");
            Ok(Swap::Kept(kept))
        }
        Err(e) => Err(after(b2)(e)),
    }
}

/// Seconds since the Unix epoch, UTC.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
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
