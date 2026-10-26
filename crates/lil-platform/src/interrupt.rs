//! Ctrl+C and Ctrl+Break for a console command that must clean up before it exits
//! (`lil dev desktop-tests` closes its Windows Sandbox and sweeps the host run's leftovers;
//! `lil dev merge` withdraws an approval it posted).
//!
//! [`catch_interrupts`] installs a console control handler that only records the event and
//! tells Windows it was handled, so the process keeps running; the command polls
//! [`interrupted`] (or [`interrupt_count`]) at its next wait and then cleans up. Children in the
//! same console still get the event themselves (a running `cargo nextest` stops its tests).
//! [`say_on_repeat`] sets a line the handler itself prints on the second and every later event,
//! so a command busy in a blocking call still answers at once. Closing the console window, a
//! logoff and a shutdown are not caught: Windows ends the process a few seconds later anyway.

use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows::core::BOOL;

static COUNT: AtomicU32 = AtomicU32::new(0);
static REPEAT_NOTE: OnceLock<String> = OnceLock::new();

/// Records Ctrl+C and Ctrl+Break from now on, instead of ending the process. Installing it
/// twice is harmless.
pub fn catch_interrupts() -> std::io::Result<()> {
    // SAFETY: `on_control` is a plain function that stays valid for the process's life.
    unsafe { SetConsoleCtrlHandler(Some(on_control), true) }
        .map_err(|e| crate::process::io_error(&e))
}

/// Whether Ctrl+C or Ctrl+Break came since [`catch_interrupts`].
pub fn interrupted() -> bool {
    interrupt_count() > 0
}

/// How many times Ctrl+C or Ctrl+Break came since [`catch_interrupts`].
pub fn interrupt_count() -> u32 {
    COUNT.load(Ordering::SeqCst)
}

/// The line the handler prints to stderr on the second and every later Ctrl+C. Only the first
/// call sets it.
pub fn say_on_repeat(note: String) {
    let _ = REPEAT_NOTE.set(note);
}

unsafe extern "system" fn on_control(ctrl: u32) -> BOOL {
    if ctrl == CTRL_C_EVENT || ctrl == CTRL_BREAK_EVENT {
        let before = COUNT.fetch_add(1, Ordering::SeqCst);
        if before >= 1
            && let Some(note) = REPEAT_NOTE.get()
        {
            let _ = writeln!(std::io::stderr(), "{note}");
        }
        BOOL(1)
    } else {
        BOOL(0)
    }
}
