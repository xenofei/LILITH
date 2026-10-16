//! `ww core stop [--timeout-secs <n>]` (ARCH-023): asks the core to finish its work and exit,
//! then waits for it. Never starts a core, and names no workspace, so a broken workspace
//! registry cannot stop it. Prints `core stopped` or `core not running`; with `--no-start` and
//! no core it prints nothing (ruling P14).

use std::time::Duration;

use ww_client::blocking;
use ww_client::{ClientError, DrainOutcome, StartPolicy};

use super::{EXIT_OK, Exit, GlobalArgs, cannot_run, config, print_line};

/// The longest wait `--timeout-secs` can ask for: a day. Larger values are taken as this one,
/// so a deadline never overflows the clock.
const MAX_WAIT_SECS: u64 = 24 * 60 * 60;

pub(super) fn stop(global: &GlobalArgs, timeout_secs: u64) -> Result<i32, Exit> {
    let cfg = config(global, StartPolicy::NoStart)?;
    let within = Duration::from_secs(timeout_secs.min(MAX_WAIT_SECS));
    match blocking::drain(&cfg, within) {
        Ok(DrainOutcome::Stopped) => {
            let _ = print_line("core stopped");
        }
        Ok(DrainOutcome::NotRunning) => {
            if !global.no_start {
                let _ = print_line("core not running");
            }
        }
        Err(ClientError::Timeout) => {
            return Err(cannot_run(format!(
                "The Worldwright core did not stop within {timeout_secs} s; it may still be finishing its work. Run `ww core stop` again to keep waiting."
            )));
        }
        Err(e) => return Err(cannot_run(e)),
    }
    Ok(EXIT_OK)
}
