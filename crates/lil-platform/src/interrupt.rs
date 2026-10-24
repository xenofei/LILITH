//! Ctrl+C and Ctrl+Break for a console command that must clean up before it exits
//! (`lil dev desktop-tests` closes its Windows Sandbox and sweeps the host run's leftovers).
//!
//! [`catch_interrupts`] installs a console control handler that only records the event and
//! tells Windows it was handled, so the process keeps running; the command polls
//! [`interrupted`] at its next wait and then cleans up. Children in the same console still get
//! the event themselves (a running `cargo nextest` stops its tests). Closing the console window,
//! a logoff and a shutdown are not caught: Windows ends the process a few seconds later anyway.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows::core::BOOL;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Records Ctrl+C and Ctrl+Break from now on, instead of ending the process. Installing it
/// twice is harmless.
pub fn catch_interrupts() -> std::io::Result<()> {
    // SAFETY: `on_control` is a plain function that stays valid for the process's life.
    unsafe { SetConsoleCtrlHandler(Some(on_control), true) }
        .map_err(|e| crate::process::io_error(&e))
}

/// Whether Ctrl+C or Ctrl+Break came since [`catch_interrupts`].
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

unsafe extern "system" fn on_control(ctrl: u32) -> BOOL {
    if ctrl == CTRL_C_EVENT || ctrl == CTRL_BREAK_EVENT {
        INTERRUPTED.store(true, Ordering::SeqCst);
        BOOL(1)
    } else {
        BOOL(0)
    }
}
