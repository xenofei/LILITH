//! The `lil-core` binary. A Windows-subsystem executable: starting it opens no console window
//! (ARCH-005).
//!
//! Startup order: the process environment, then the pipe claim (a core that loses it exits 3
//! having touched nothing, not even its log), then the log, then the suite `state.db` (a newer
//! one exits 6, a damaged one 5, one with a hot rollback journal 7, any other failure 2;
//! nothing on disk is changed and the pipe is released first), then serving until the core
//! shuts down (ARCH-021). Startup failures exit with [`CoreExit::code`]; a shutdown exits with
//! its report's code: 4 after a fatal error (a state actor that died, a pipe instance that
//! could not be created, a test parent that cannot be watched), else 0.

#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::process::ExitCode;

use lil_core::{CoreConfig, CoreExit, claim_pipe, start_claimed};
use lil_platform::Env;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(exit) => {
            tracing::error!(code = exit.code(), reason = %exit, "core exiting");
            ExitCode::from(exit.code())
        }
    }
}

fn run() -> Result<u8, CoreExit> {
    let env = Env::from_process().map_err(|e| CoreExit::Startup(e.to_string()))?;
    let cfg = CoreConfig::for_env(env).map_err(|e| CoreExit::Startup(e.to_string()))?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| CoreExit::Startup(format!("cannot build the runtime: {e}")))?;
    rt.block_on(async {
        let listener = claim_pipe(&cfg.env.pipe_name())?;
        init_logs(&cfg.env)?;
        let report = start_claimed(cfg, listener).await?.wait().await;
        Ok(report.exit_code())
    })
}

/// The shared log setup (ARCH-117, ARCH-119; P4a-R20): JSON lines to
/// `logs_dir\lil-core.<YYYY-MM-DD>.log`, a file a day, 14 kept, a day's file stopped at 64 MiB
/// with one `log_capped` line, and a panic hook that logs the panic's text (`panic`) and
/// location before the default hook runs. The hook's line is logged in the panicking task's
/// span, so a tool's panic carries the call's `trace_id`.
fn init_logs(env: &Env) -> Result<(), CoreExit> {
    // The log of before plan 4a, `core.<date>.log`, goes once it is 14 days old.
    lil_platform::logs::prune_retired(&env.logs_dir(), "core");
    // The guard holds nothing: logging lasts for the process.
    lil_platform::logs::init(env, "lil-core")
        .map(drop)
        .map_err(|e| {
            CoreExit::Startup(format!(
                "cannot start logging to {}: {e}",
                env.logs_dir().display()
            ))
        })
}
