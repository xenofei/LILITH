//! The `ww-core` binary. A Windows-subsystem executable: starting it opens no console window
//! (ARCH-005).
//!
//! Startup order: the process environment, then the pipe claim (a core that loses it exits 3
//! having touched nothing, not even its log), then the log, then the suite `state.db` (a newer
//! one exits 6, a damaged one 5, one with a hot rollback journal 7, any other failure 2;
//! nothing on disk is changed and the pipe is released first), then serving until the core shuts down (ARCH-021). Startup failures
//! exit with [`CoreExit::code`]; a shutdown exits with its report's code: 4 after a fatal
//! error, else 0.

#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::process::ExitCode;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use ww_core::store::fsops;
use ww_core::{CoreConfig, CoreExit, claim_pipe, start_claimed};
use ww_platform::Env;

/// Daily log files kept (ARCH-117).
const LOG_FILES_KEPT: usize = 14;

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

/// JSON lines to `logs_dir\core.<date>.log`, rotated daily, 14 files kept, and a panic hook
/// that logs the message and location before the default hook runs (ARCH-117, ARCH-119).
fn init_logs(env: &Env) -> Result<(), CoreExit> {
    let dir = env.logs_dir();
    let startup = |what: &str, e: &dyn std::fmt::Display| {
        CoreExit::Startup(format!("cannot {what} {}: {e}", dir.display()))
    };
    fsops::ensure_dir(&dir).map_err(|e| startup("create the log folder", &e))?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("core")
        .filename_suffix("log")
        .max_log_files(LOG_FILES_KEPT)
        .build(&dir)
        .map_err(|e| startup("open the log in", &e))?;
    tracing_subscriber::fmt()
        .json()
        .with_writer(appender)
        .try_init()
        .map_err(|e| startup("start logging to", &e))?;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let message = info.payload_as_str().unwrap_or("(no message)");
        tracing::error!(%location, message, "panic");
        default_hook(info);
    }));
    Ok(())
}
