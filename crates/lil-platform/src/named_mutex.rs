//! A named mutex as a single-run lock (Task 30: one `lil dev desktop-tests --host` at a time per
//! user session, whatever checkout it runs from).
//!
//! [`try_lock`] opens or creates the mutex and takes it without waiting. Windows releases a
//! mutex whose owner ended without releasing it (a crash, a kill), and the next taker sees
//! `WAIT_ABANDONED`: that counts as taken, and [`Taken::Abandoned`] says so, so no stale-lock
//! logic is needed. Ownership belongs to the thread that took it: the guard must be dropped on
//! that thread, and the same thread taking it again succeeds (Windows mutexes are recursive).

use std::io;

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::HSTRING;

/// The mutex, held; released and closed when dropped.
#[derive(Debug)]
pub struct MutexGuard {
    handle: HANDLE,
}

// SAFETY: a mutex handle may move between threads; the guard only releases it on drop (and
// the release fails harmlessly on a thread that doesn't own it).
unsafe impl Send for MutexGuard {}

impl Drop for MutexGuard {
    fn drop(&mut self) {
        // SAFETY: the handle is the mutex this guard took and still owns; it is released once and
        // closed once.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// What [`try_lock`] found.
#[derive(Debug)]
pub enum Taken {
    /// Taken.
    Acquired(MutexGuard),
    /// Taken, after its last owner ended without releasing it.
    Abandoned(MutexGuard),
    /// Another thread or process holds it.
    Busy,
}

/// Opens or creates the named mutex `name` (for example `Local\LILITH-desktop-host-run`, per
/// session) and takes it without waiting.
pub fn try_lock(name: &str) -> io::Result<Taken> {
    // SAFETY: plain call with a NUL-terminated name; the handle is owned by the guard below or
    // closed here.
    let handle = unsafe { CreateMutexW(None, false, &HSTRING::from(name)) }
        .map_err(|e| crate::process::io_error(&e))?;
    // SAFETY: the handle was just opened.
    let waited = unsafe { WaitForSingleObject(handle, 0) };
    let guard = MutexGuard { handle };
    match waited {
        WAIT_OBJECT_0 => Ok(Taken::Acquired(guard)),
        WAIT_ABANDONED => Ok(Taken::Abandoned(guard)),
        WAIT_TIMEOUT => {
            // Not ours: close without releasing.
            let handle = guard.handle;
            std::mem::forget(guard);
            // SAFETY: the handle is open and owned by nothing else.
            let _ = unsafe { CloseHandle(handle) };
            Ok(Taken::Busy)
        }
        _ => {
            let e = io::Error::last_os_error();
            let handle = guard.handle;
            std::mem::forget(guard);
            // SAFETY: as above.
            let _ = unsafe { CloseHandle(handle) };
            Err(e)
        }
    }
}
