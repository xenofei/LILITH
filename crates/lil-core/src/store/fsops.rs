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

/// Writes `bytes` to the new file `path`, whose parent folders are made: the data goes to
/// `<parent>\<temp_name>` (flushed to disk, never an existing file) and is moved to `path`
/// without replacing anything (errors 80 and 183 when a file is there). The temp file is
/// removed again when the move fails.
pub fn write_new_file(
    path: &Path,
    temp_name: &str,
    bytes: &[u8],
    fp: &FailPoints,
) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the file has no folder"))?;
    ensure_dir(dir)?;
    let temp = write_temp(dir, temp_name, bytes, fp)?;
    if let Err(e) = move_new(&temp, path) {
        let _ = remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

/// Writes `bytes` as the file `path`, directly in one of the suite's own cache folders: the
/// `.lilref` cache ([`Env::lilref_dir`], APP-021) or the diagnostics folder
/// ([`Env::diagnostics_dir`], ARCH-119). The folder is made; the data goes to a new temp file
/// beside the target (flushed to disk), which is moved to the target's name, or replaces the
/// file there (`ReplaceFileW`, its backup then removed). Whatever fails, the temp file is
/// removed again, and a file `ReplaceFileW` had already moved to the backup name is moved
/// back when nothing took its place. Temp and backup files of this function older than a day
/// (a crash's leftovers) are removed from the folder first.
///
/// Refused with `PermissionDenied` before anything is written or made: a relative path, a
/// path with a `.` or `..` part, a path whose folder isn't one of the two (compared with
/// `folder_key`, ignoring case), and a file name Windows would read as something else: one
/// with `:` (a stream), `*?"<>|` or a control character, one ending in `.` or a space, or a
/// reserved device name (`CON`, `NUL`, `COM1`, … with any extension). So nothing is written
/// anywhere else (Appendix G).
///
/// [`Env::lilref_dir`]: lil_platform::Env::lilref_dir
/// [`Env::diagnostics_dir`]: lil_platform::Env::diagnostics_dir
pub fn write_suite_cache_file(
    env: &lil_platform::Env,
    path: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    let (_, temp) = cache_temp(env, path, bytes)?;
    let placed = match move_new(&temp, path) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // `.<name>.<pid>-<n>.bak`, beside the temp file.
            let backup = temp.with_extension("bak");
            let replaced = replace(path, &temp, &backup);
            settle_replace(path, &backup, replaced, move_new)
        }
        other => other,
    };
    if placed.is_err() {
        let _ = remove_file(&temp);
    }
    placed
}

/// How many names [`write_new_suite_cache_file`] tries before it gives up: `<name>`, then
/// `<stem>-2.<ext>` and on ("until one is free"; the bound only stops a folder that refuses
/// every move with `AlreadyExists` from looping for ever).
pub const NEW_CACHE_NAMES: u32 = 1_000_000;

/// [`write_suite_cache_file`], but never replacing a file: the temp file is moved (without
/// `MOVEFILE_REPLACE_EXISTING`) to `path`, or, when a file has that name or `skip` says it
/// was handed out already, to `<stem>-2.<ext>`, `<stem>-3.<ext>` and on until a move takes;
/// no name is checked for a file before the move, so two writers never take the same one.
/// Returns the name it took (a diagnostics bundle made in the same second as another,
/// Appendix G "Diagnostics").
///
/// # Errors
/// As [`write_suite_cache_file`]; `AlreadyExists` past [`NEW_CACHE_NAMES`] names.
pub fn write_new_suite_cache_file(
    env: &lil_platform::Env,
    path: &Path,
    bytes: &[u8],
    skip: &dyn Fn(&Path) -> bool,
) -> std::io::Result<PathBuf> {
    let (dir, temp) = cache_temp(env, path, bytes)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name.as_str(), ""),
    };
    for k in 1..=NEW_CACHE_NAMES {
        let target = if k == 1 {
            path.to_path_buf()
        } else {
            dir.join(format!("{stem}-{k}{ext}"))
        };
        if skip(&target) {
            continue;
        }
        match move_new(&temp, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                let _ = remove_file(&temp);
                return Err(e);
            }
        }
    }
    let _ = remove_file(&temp);
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "{} and its {} numbered names are all taken",
            path.display(),
            NEW_CACHE_NAMES - 1
        ),
    ))
}

/// The checks of [`write_suite_cache_file`], then `bytes` in a new temp file beside `path`
/// (the folder made, a day's leftovers swept first): the folder and the temp file.
fn cache_temp<'p>(
    env: &lil_platform::Env,
    path: &'p Path,
    bytes: &[u8],
) -> std::io::Result<(&'p Path, PathBuf)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let (dir, name) = check_cache_path(env, path)?;
    ensure_dir(dir)?;
    sweep_leftovers(dir);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let stem = format!(".{}.{}-{n}", name.to_string_lossy(), std::process::id());
    let temp = write_temp(dir, &format!("{stem}.tmp"), bytes, &FailPoints::none())?;
    Ok((dir, temp))
}

/// `path`'s folder and file name, when it is a file directly in the `.lilref` cache or the
/// diagnostics folder with a name Windows reads as a plain file ([`write_suite_cache_file`]).
fn check_cache_path<'p>(
    env: &lil_platform::Env,
    path: &'p Path,
) -> std::io::Result<(&'p Path, &'p std::ffi::OsStr)> {
    let refuse = |why: &str| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a file of the suite's .lilref or diagnostics folder: {why}",
                path.display()
            ),
        )
    };
    if !path.is_absolute() {
        return Err(refuse("the path is relative"));
    }
    // Read from the text: `Path::components` drops a `.` in the middle.
    if path
        .to_string_lossy()
        .split(['\\', '/'])
        .any(|part| part == "." || part == "..")
    {
        return Err(refuse("its name holds `.` or `..`"));
    }
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(refuse("it names no file"));
    };
    if let Some(why) = name
        .to_str()
        .map_or(Some("its name is not Unicode"), odd_file_name)
    {
        return Err(refuse(why));
    }
    let key = lil_platform::paths::folder_key(dir);
    if ![env.lilref_dir(), env.diagnostics_dir()]
        .iter()
        .any(|allowed| lil_platform::paths::folder_key(allowed) == key)
    {
        return Err(refuse("its folder is another one"));
    }
    Ok((dir, name))
}

/// The newest `max` bytes of `path` (all of it when it is shorter), cut so they start at a
/// line: the diagnostics bundle's logs (Appendix G "Diagnostics"). `File::open` shares
/// reading, writing and deleting, so a process still writing the file goes on.
pub fn read_tail(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(long_path(path))?;
    let len = file.metadata()?.len();
    if len <= max {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    // One byte more, to see whether the tail already starts at a line.
    let from = len - max - 1;
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    let cut = bytes
        .iter()
        .position(|b| *b == b'\n')
        .map_or(bytes.len(), |nl| nl + 1);
    bytes.drain(..cut);
    Ok(bytes)
}

/// After `ReplaceFileW` of `path` with `backup` gave `replaced`: the backup (the old file) is
/// removed only once the new file is in place. When the replace failed halfway (the old file
/// already at the backup name, nothing at `path`), `restore` moves it back; if that fails too,
/// the backup is kept, the only copy of the old file (review N3).
fn settle_replace(
    path: &Path,
    backup: &Path,
    replaced: std::io::Result<()>,
    restore: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match &replaced {
        Ok(()) => {
            let _ = remove_file(backup);
        }
        Err(_) if !path.exists() && backup.exists() => {
            if let Err(e) = restore(backup, path) {
                tracing::warn!(backup = %backup.display(), error = %e, "the old file could not be put back; it is kept at its backup name");
            }
        }
        Err(_) => {}
    }
    replaced
}

/// Why `name` can't be a file name [`write_suite_cache_file`] writes, if it can't.
fn odd_file_name(name: &str) -> Option<&'static str> {
    const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Some("its name holds a character Windows reads otherwise");
    }
    if name.ends_with(['.', ' ']) {
        return Some("its name ends in `.` or a space");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    // `COM0`–`COM9` and `LPT0`–`LPT9`, and the superscript `COM¹`–`COM³` and `LPT¹`–`LPT³`
    // (review N2).
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            let mut digits = n.chars();
            matches!(
                (digits.next(), digits.next()),
                (Some('0'..='9' | '\u{B9}' | '\u{B2}' | '\u{B3}'), None)
            )
        })
    };
    if RESERVED.contains(&stem.as_str()) || numbered("COM") || numbered("LPT") {
        return Some("its name is a reserved device name");
    }
    None
}

/// Whether `name` is one of [`write_suite_cache_file`]'s temp or backup names:
/// `.<name>.<pid>-<n>.tmp` or `.bak`, with `<pid>` and `<n>` decimal (review N4).
fn own_leftover(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix('.')
        .and_then(|n| n.strip_suffix(".tmp").or_else(|| n.strip_suffix(".bak")))
    else {
        return false;
    };
    let Some((target, counter)) = rest.rsplit_once('.') else {
        return false;
    };
    let decimal = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    !target.is_empty()
        && counter
            .split_once('-')
            .is_some_and(|(pid, n)| decimal(pid) && decimal(n))
}

/// Removes [`write_suite_cache_file`]'s temp and backup files ([`own_leftover`]) older than a
/// day from `dir`: what a crash between writing and moving left.
#[allow(clippy::disallowed_methods)]
fn sweep_leftovers(dir: &Path) {
    const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !own_leftover(&name) {
            continue;
        }
        let old = entry
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > DAY);
        if old {
            let _ = remove_file(&entry.path());
        }
    }
}

/// A new file written in pieces (an engine output streamed into a staging handle or a job
/// cache, Appendix F "ComfyUI client"): [`create_streamed`] creates it (never an existing
/// file), [`StreamedFile::write`] appends, [`StreamedFile::finish`] flushes it with
/// `FlushFileBuffers` and closes it. One dropped before it is finished is removed again.
#[derive(Debug)]
pub struct StreamedFile {
    file: Option<std::fs::File>,
    path: PathBuf,
}

/// Creates `path` for [`StreamedFile`]; an existing file there is an error (`AlreadyExists`).
#[allow(clippy::disallowed_methods)]
pub fn create_streamed(path: &Path) -> std::io::Result<StreamedFile> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(long_path(path))?;
    Ok(StreamedFile {
        file: Some(file),
        path: path.to_path_buf(),
    })
}

impl StreamedFile {
    /// Appends `bytes`.
    ///
    /// # Errors
    /// The write's.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match &mut self.file {
            Some(file) => file.write_all(bytes),
            None => Err(std::io::Error::other("the file is finished")),
        }
    }

    /// Flushes the file to disk and closes it; it stays.
    ///
    /// # Errors
    /// The flush's (the file is then removed).
    #[allow(clippy::disallowed_methods)]
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        let file = self
            .file
            .take()
            .ok_or_else(|| std::io::Error::other("the file is finished"))?;
        let flushed = lil_platform::fs::flush(&file);
        drop(file);
        if let Err(e) = flushed {
            let _ = std::fs::remove_file(long_path(&self.path));
            return Err(e);
        }
        Ok(std::mem::take(&mut self.path))
    }
}

impl Drop for StreamedFile {
    #[allow(clippy::disallowed_methods)]
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            drop(file);
            let _ = std::fs::remove_file(long_path(&self.path));
        }
    }
}

/// Removes `file`, a copy the core itself just staged directly in `dir` (an engine output
/// streamed or copied there) that failed its check, so no unverified copy is left: a temp
/// clean-up (CORE-110). Anything not directly in `dir` is refused (`PermissionDenied`).
pub fn discard_unverified(dir: &Path, file: &Path) -> std::io::Result<()> {
    if file.parent() != Some(dir) || file.file_name().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a staged copy in {}",
                file.display(),
                dir.display()
            ),
        ));
    }
    remove_file(file)
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

/// Deletes `file`, a ComfyUI-side copy of an output or an upload (Appendix F "ComfyUI
/// client"), through [`remove_file`]. It is refused (`PermissionDenied`) unless `file` is a
/// file (or a link, which is removed, never followed) below `<root>\output` or `<root>\input`,
/// named without `.` or `..` and reached through no link or junction below that folder, so a
/// clean-up can never delete anything else. Names compare case-insensitively, as Windows does.
pub fn remove_engine_file(root: &Path, file: &Path) -> std::io::Result<()> {
    check_engine_file(root, file)?;
    remove_file(file)
}

/// Why `file` is not one [`remove_engine_file`] may delete.
fn check_engine_file(root: &Path, file: &Path) -> std::io::Result<()> {
    use std::path::Component;
    let refuse = |why: &str| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a ComfyUI output or upload under {}: {why}",
                file.display(),
                root.display()
            ),
        ))
    };
    let key = |c: Component<'_>| c.as_os_str().to_string_lossy().to_lowercase();
    let mut base = root.components();
    let mut rest = file.components();
    for want in base.by_ref() {
        match rest.next() {
            Some(got) if key(got) == key(want) => {}
            _ => return refuse("it is outside the ComfyUI folder"),
        }
    }
    let rest: Vec<Component<'_>> = rest.collect();
    if rest.iter().any(|c| !matches!(c, Component::Normal(_))) {
        return refuse("its name holds `.` or `..`");
    }
    let Some((first, below)) = rest.split_first() else {
        return refuse("it is the ComfyUI folder itself");
    };
    if !matches!(key(*first).as_str(), "output" | "input") || below.is_empty() {
        return refuse(r"only files below output\ and input\ are removed");
    }
    let mut at = root.join(first.as_os_str());
    for (i, part) in below.iter().enumerate() {
        at.push(part.as_os_str());
        let meta = std::fs::symlink_metadata(long_path(&at))?;
        let last = i + 1 == below.len();
        if !last && (meta.file_type().is_symlink() || !meta.is_dir()) {
            return refuse("a folder on its way is a link or not a folder");
        }
        if last && meta.is_dir() {
            return refuse("it is a folder");
        }
    }
    Ok(())
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

/// Sets a damaged or foreign-version `telemetry.db` aside: it and its `-wal`, `-shm` and
/// `-journal` files are renamed to `<name>.damaged-<utc>` (`lil_platform::sqlite_files::set_aside`),
/// and nothing is deleted or replaced. For the disposable `telemetry.db` only (CORE-004): a
/// `state.db` is set aside by `lil doctor` alone, after a yes.
#[allow(clippy::disallowed_methods)]
pub fn set_aside_db(db: &Path, utc: &str) -> std::io::Result<Vec<PathBuf>> {
    lil_platform::sqlite_files::set_aside(db, utc)
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

    /// Review N3: the old file at the backup name is removed only once the new file is in
    /// place; a failed replace puts it back, and when that fails too it is kept.
    #[test] // spec: APP-021, ARCH-119
    #[allow(clippy::disallowed_methods)] // plants the old file at its backup name
    fn a_failed_restore_keeps_the_old_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, backup) = (
            dir.path().join("a.lilref"),
            dir.path().join(".a.lilref.1-1.bak"),
        );
        let failed = || Err(std::io::Error::other("replace failed"));
        let cannot = |_: &Path, _: &Path| Err(std::io::Error::other("restore failed"));

        // The restore fails: the backup stays, the only copy.
        std::fs::write(&backup, b"old").unwrap();
        assert!(settle_replace(&path, &backup, failed(), cannot).is_err());
        assert_eq!(std::fs::read(&backup).unwrap(), b"old");
        assert!(!path.exists());

        // The restore works: the old file is back at its name.
        assert!(settle_replace(&path, &backup, failed(), move_new).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(!backup.exists());

        // The new file is in place: the backup goes.
        std::fs::write(&backup, b"old").unwrap();
        settle_replace(&path, &backup, Ok(()), cannot).unwrap();
        assert!(!backup.exists());
    }

    #[test] // spec: APP-021, ARCH-119
    fn only_the_writers_own_leftovers_are_swept() {
        for own in [
            ".a.lilref.12-3.tmp",
            ".a.lilref.12-3.bak",
            ".bundle.zip.1-99.tmp",
        ] {
            assert!(own_leftover(own), "{own}");
        }
        for other in [
            ".a.tmp",
            ".a.lilref.tmp",
            ".a.lilref.12-x.tmp",
            ".a.lilref.12.tmp",
            "..1-2.tmp",
            "a.lilref.1-2.tmp",
            ".a.lilref.1-2.txt",
            ".task12-temp.tmp",
        ] {
            assert!(!own_leftover(other), "{other}");
        }
    }

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
