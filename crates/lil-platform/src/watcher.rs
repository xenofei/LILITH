//! The workspace watcher's file-system half (ARCH-079, ARCH-083): `ReadDirectoryChangesW` on
//! its own thread.
//!
//! Facts it relies on (Microsoft Learn, `ReadDirectoryChangesW`, re-checked 2026-10-16):
//! - The plain form is used, not `ReadDirectoryChangesExW`, which is documented for NTFS only.
//!   The watcher re-reads every changed path through `probe.identity` anyway, so the extended
//!   information would add nothing.
//! - The first call allocates a buffer of the call's size for the handle. Changes between calls
//!   are added to it and returned by the next call; when it overflows, the call succeeds with
//!   zero bytes and its contents are discarded. A completion with `ERROR_NOTIFY_ENUM_DIR` means
//!   the same. Both are [`DirChange::Overflow`]: the caller rescans.
//! - Over the network the call fails with `ERROR_INVALID_PARAMETER` when the buffer is larger
//!   than 64 KiB, so a larger buffer is refused up front. A redirector or file system that
//!   can't watch fails with `ERROR_INVALID_FUNCTION`.
//! - The buffer must be DWORD-aligned; `FILE_NOTIFY_INFORMATION` records follow each other by
//!   `NextEntryOffset`, and `FileName` is not NUL-terminated. A name may be the long or the
//!   8.3 short form ("it is unspecified which one"): the caller resolves it.
//! - Changes to the watched folder itself are not reported. When the folder is removed, or its
//!   volume goes away, the pending read completes with an error (error 5 for a removed folder,
//!   observed on Windows 11 NTFS, 2026-10-16): [`DirChange::Failed`], and the thread ends. A
//!   rename of the watched folder completes nothing: the handle follows the folder and its
//!   changes keep coming under the new name, so the caller checks the folder's path itself.
//!
//! The folder is opened with `FILE_LIST_DIRECTORY`, sharing read, write and delete, with
//! `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED`, so the watch never blocks a rename or a
//! delete in the tree. Each read is overlapped and completed with `GetOverlappedResult`;
//! [`DirWatcher::stop`] cancels it with `CancelIoEx`, which completes it with
//! `ERROR_OPERATION_ABORTED` (never reported as a failure).
//!
//! [`DirWatcher::hold`] keeps the thread from issuing its next read until
//! [`DirWatcher::release`]: changes meanwhile collect in the system's buffer for the handle, and
//! a burst larger than it arrives as one [`DirChange::Overflow`]. The workspace watcher holds
//! while it handles a batch, so a burst becomes one rescan instead of an unbounded queue.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows::Win32::Foundation::{ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE, FILE_NOTIFY_CHANGE_ATTRIBUTES,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::CreateEventW;

use crate::fs::long_path;
use crate::process::io_error;

/// The largest buffer a network folder accepts, and the largest [`DirWatcher::start`] takes.
pub const MAX_BUFFER_BYTES: u32 = 64 * 1024;

/// `ERROR_INVALID_PARAMETER`.
const INVALID_PARAMETER: i32 = 87;

/// One change below the watched folder. Paths are relative to it, as Windows reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirChange {
    Added(PathBuf),
    Removed(PathBuf),
    Modified(PathBuf),
    RenamedFrom(PathBuf),
    RenamedTo(PathBuf),
    /// Changes were lost (the buffer overflowed, or `ERROR_NOTIFY_ENUM_DIR`): rescan.
    Overflow,
    /// A read or completion error other than `ERROR_OPERATION_ABORTED` (our own cancel): the
    /// watched folder was removed, or its volume went away. The thread ends after sending it.
    /// (A rename of the watched folder is no error: see the module docs.)
    Failed(i32),
}

/// What the thread and the watcher share.
#[derive(Debug, Default)]
struct Control {
    /// The folder handle, while the thread runs; only the thread closes it.
    dir: Option<File>,
    stopped: bool,
    held: bool,
}

#[derive(Debug)]
struct Shared {
    control: Mutex<Control>,
    changed: Condvar,
}

fn lock(m: &Mutex<Control>) -> MutexGuard<'_, Control> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running `ReadDirectoryChangesW` watch of a folder and its subtree, reporting file and
/// folder names, sizes, last writes and attributes. Dropping it stops it.
#[derive(Debug)]
pub struct DirWatcher {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// The changes the watch reports.
fn filter() -> FILE_NOTIFY_CHANGE {
    FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_ATTRIBUTES
}

fn raw(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

impl DirWatcher {
    /// Starts watching `root` and its subtree with a `buffer_bytes` buffer (rounded up to a
    /// multiple of 4). The first read is issued before this returns, so its error is this
    /// call's.
    ///
    /// # Errors
    /// - `InvalidInput` (raw code 87) for a buffer of 0 or over [`MAX_BUFFER_BYTES`];
    /// - the open's error (the folder is missing, say);
    /// - the first read's error: `ERROR_INVALID_FUNCTION` (1) where the redirector or file
    ///   system can't watch, `ERROR_INVALID_PARAMETER` (87) for a buffer a network folder
    ///   refuses.
    pub fn start(
        root: &Path,
        buffer_bytes: u32,
    ) -> io::Result<(DirWatcher, UnboundedReceiver<DirChange>)> {
        if buffer_bytes == 0 || buffer_bytes > MAX_BUFFER_BYTES {
            return Err(io::Error::from_raw_os_error(INVALID_PARAMETER));
        }
        let dir = OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY.0)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
            .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED).0)
            .open(long_path(root))?;
        // SAFETY: plain call; the event handle is owned by the `OwnedHandle` built from it.
        let event = unsafe { CreateEventW(None, true, false, None) }.map_err(|e| io_error(&e))?;
        // SAFETY: `event` is a fresh handle nothing else owns.
        let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
        let shared = Arc::new(Shared {
            control: Mutex::new(Control {
                dir: Some(dir),
                stopped: false,
                held: false,
            }),
            changed: Condvar::new(),
        });
        let (tx, rx) = unbounded_channel();
        let (first_tx, first_rx) = std::sync::mpsc::channel::<io::Result<()>>();
        let words = buffer_bytes.div_ceil(4) as usize;
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("lil-dir-watcher".to_string())
            .spawn(move || run(&thread_shared, &event, words, &tx, first_tx))?;
        match first_rx.recv() {
            Ok(Ok(())) => Ok((
                DirWatcher {
                    shared,
                    thread: Mutex::new(Some(thread)),
                },
                rx,
            )),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other(
                    "the watcher thread ended before its first read",
                ))
            }
        }
    }

    /// Stops the watch: the pending read is cancelled (`CancelIoEx`), the thread ends without
    /// a [`DirChange::Failed`], and the channel closes. Stopping again does nothing.
    pub fn stop(&self) {
        {
            let mut control = lock(&self.shared.control);
            control.stopped = true;
            if let Some(dir) = control.dir.as_ref() {
                // SAFETY: the handle is open while it is in `control` (only the thread closes
                // it, under this lock). A failure means no read was pending.
                let _ = unsafe { CancelIoEx(raw(dir), None) };
            }
        }
        self.shared.changed.notify_all();
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }

    /// Keeps the thread from issuing its next read until [`DirWatcher::release`]: changes
    /// meanwhile collect in the system's buffer for the folder, and overflow it as one
    /// [`DirChange::Overflow`] when there are more than it holds.
    pub fn hold(&self) {
        lock(&self.shared.control).held = true;
    }

    /// Lets the thread read again after [`DirWatcher::hold`].
    pub fn release(&self) {
        lock(&self.shared.control).held = false;
        self.shared.changed.notify_all();
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Issues one overlapped read into `buffer`, unless the watch was stopped (then `None`). It
/// waits while the watch is held. The read is issued under the control lock, so a stop either
/// comes first (no read) or cancels it.
fn issue(
    shared: &Shared,
    buffer: &mut [u32],
    overlapped: &mut OVERLAPPED,
) -> Option<windows::core::Result<()>> {
    let mut control = lock(&shared.control);
    while control.held && !control.stopped {
        control = shared
            .changed
            .wait(control)
            .unwrap_or_else(PoisonError::into_inner);
    }
    if control.stopped {
        return None;
    }
    let dir = control.dir.as_ref()?;
    let len = u32::try_from(std::mem::size_of_val(buffer)).unwrap_or(MAX_BUFFER_BYTES);
    // SAFETY: the handle is open (it is in `control`); `buffer` and `overlapped` outlive the
    // read, since the thread waits for its completion (`GetOverlappedResult` with bWait) before
    // it touches them again or returns.
    Some(unsafe {
        ReadDirectoryChangesW(
            raw(dir),
            buffer.as_mut_ptr().cast(),
            len,
            true,
            filter(),
            None,
            Some(std::ptr::from_mut(overlapped)),
            None,
        )
    })
}

/// The thread: read, wait, report, until stopped or failed. The first read's outcome goes to
/// `first`.
fn run(
    shared: &Shared,
    event: &OwnedHandle,
    words: usize,
    tx: &UnboundedSender<DirChange>,
    first: std::sync::mpsc::Sender<io::Result<()>>,
) {
    let mut buffer = vec![0u32; words];
    let mut first = Some(first);
    loop {
        let mut overlapped = OVERLAPPED {
            hEvent: HANDLE(event.as_raw_handle()),
            ..OVERLAPPED::default()
        };
        let issued = issue(shared, &mut buffer, &mut overlapped);
        match issued {
            None => break,
            Some(Err(e)) => {
                let e = io_error(&e);
                match first.take() {
                    Some(first) => {
                        let _ = first.send(Err(e));
                    }
                    None => {
                        let _ = tx.send(DirChange::Failed(e.raw_os_error().unwrap_or(-1)));
                    }
                }
                break;
            }
            Some(Ok(())) => {
                if let Some(first) = first.take() {
                    let _ = first.send(Ok(()));
                }
            }
        }
        let mut bytes = 0u32;
        let done = {
            let control = lock(&shared.control);
            let Some(dir) = control.dir.as_ref().map(raw) else {
                break;
            };
            drop(control);
            // SAFETY: the handle stays open (only this thread closes it); `overlapped` is the
            // one the read was issued with and lives until this returns.
            unsafe { GetOverlappedResult(dir, &raw const overlapped, &raw mut bytes, true) }
        };
        let change = match done {
            Err(e) => {
                let e = io_error(&e);
                match e.raw_os_error() {
                    Some(code) if code == ERROR_OPERATION_ABORTED.0.cast_signed() => break,
                    Some(code) if code == ERROR_NOTIFY_ENUM_DIR.0.cast_signed() => {
                        Some(DirChange::Overflow)
                    }
                    code => {
                        let _ = tx.send(DirChange::Failed(code.unwrap_or(-1)));
                        break;
                    }
                }
            }
            Ok(()) if bytes == 0 => Some(DirChange::Overflow),
            Ok(()) => None,
        };
        if lock(&shared.control).stopped {
            break;
        }
        let sent = match change {
            Some(change) => tx.send(change).is_ok(),
            None => parse(&buffer, bytes as usize)
                .into_iter()
                .all(|c| tx.send(c).is_ok()),
        };
        if !sent {
            // Nobody listens any more.
            break;
        }
    }
    // Only the thread closes the folder handle, under the lock a stop takes.
    drop(lock(&shared.control).dir.take());
}

/// The `FILE_NOTIFY_INFORMATION` records in the first `len` bytes of `buffer`.
fn parse(buffer: &[u32], len: usize) -> Vec<DirChange> {
    // SAFETY: a `u32` slice viewed as bytes: same memory, alignment 1 ≤ 4.
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), std::mem::size_of_val(buffer))
    };
    let len = len.min(bytes.len());
    let mut out = Vec::new();
    let mut at = 0usize;
    // NextEntryOffset (4), Action (4), FileNameLength (4), FileName (UTF-16).
    while at + 12 <= len {
        let word =
            |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let next = word(at) as usize;
        let action = word(at + 4);
        let name_len = word(at + 8) as usize;
        let name_at = at + 12;
        if name_at + name_len > len {
            break;
        }
        let units: Vec<u16> = bytes[name_at..name_at + name_len]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let path = PathBuf::from(std::ffi::OsString::from_wide(&units));
        let change = match action {
            a if a == FILE_ACTION_ADDED.0 => Some(DirChange::Added(path)),
            a if a == FILE_ACTION_REMOVED.0 => Some(DirChange::Removed(path)),
            a if a == FILE_ACTION_MODIFIED.0 => Some(DirChange::Modified(path)),
            a if a == FILE_ACTION_RENAMED_OLD_NAME.0 => Some(DirChange::RenamedFrom(path)),
            a if a == FILE_ACTION_RENAMED_NEW_NAME.0 => Some(DirChange::RenamedTo(path)),
            _ => None,
        };
        out.extend(change);
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One record of `action` for `name`, padded to a DWORD boundary, as Windows lays it out.
    fn record(action: u32, name: &str, last: bool) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let size = (12 + units.len()).div_ceil(4) * 4;
        let mut out = Vec::new();
        out.extend(
            u32::try_from(if last { 0 } else { size })
                .unwrap()
                .to_le_bytes(),
        );
        out.extend(action.to_le_bytes());
        out.extend(u32::try_from(units.len()).unwrap().to_le_bytes());
        out.extend(units);
        out.resize(size, 0);
        out
    }

    #[test] // spec: ARCH-079
    fn records_parse_into_changes() {
        let mut bytes = record(1, "a.txt", false);
        bytes.extend(record(4, r"dir\Ünïcödé space.png", false));
        bytes.extend(record(5, r"dir\b.png", false));
        bytes.extend(record(2, "c", false));
        bytes.extend(record(3, r"PROJEC~1\x", true));
        let len = bytes.len();
        bytes.resize(len.div_ceil(4) * 4, 0);
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        assert_eq!(
            parse(&words, len),
            [
                DirChange::Added("a.txt".into()),
                DirChange::RenamedFrom(r"dir\Ünïcödé space.png".into()),
                DirChange::RenamedTo(r"dir\b.png".into()),
                DirChange::Removed("c".into()),
                DirChange::Modified(r"PROJEC~1\x".into()),
            ]
        );
        // A record cut short by `len` is dropped, never read past the end.
        assert_eq!(parse(&words, 14), []);
    }
}
