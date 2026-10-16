//! What a core needs to run: its `Env`, the clock and ids, the process inspector, who it
//! trusts, and the timing knobs (infrastructure, not a CORE-014 module).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ww_platform::process::{ProcessInspector, SystemInspector, current_exe_dir};
use ww_platform::{Channel, Clock, Env, IdSource, RandomIds, SystemClock};

use crate::identity::{EngineTokens, NoEngineTokens, TrustedDirs};

/// A core with no connections, calls or busy blockers exits after this long (ARCH-020).
const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(600);
/// A connection that has not said hello after this long is closed.
const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(30);

/// The core's configuration.
#[derive(Clone)]
pub struct CoreConfig {
    pub env: Env,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdSource>,
    pub inspector: Arc<dyn ProcessInspector>,
    /// The folders whose app and MCP executables are trusted (ARCH-046, ARCH-047).
    pub trusted: TrustedDirs,
    /// Checks engine tokens; plan 2 refuses every token (ARCH-048).
    pub engine_tokens: Arc<dyn EngineTokens>,
    /// 600 s, or `WW_TEST_IDLE_SECS` on a test channel.
    pub idle_after: Duration,
    /// 30 s.
    pub hello_timeout: Duration,
    /// The channel is `test-*`: the test tools are registered.
    pub test_tools: bool,
    /// Test channels only: the core exits 0 once this process is gone.
    pub test_parent_pid: Option<u32>,
    /// The crate's own unit tests only (compiled out of every build): a request with this
    /// method panics in the router, to test how the transport survives a panicking handler.
    #[cfg(test)]
    pub panic_on_method: Option<&'static str>,
}

impl CoreConfig {
    /// The system clock, random ids, the real process inspector, the trusted folders of
    /// `env` (the running executable's folder on dev and test channels) and no engine tokens,
    /// with the `WW_TEST_*` overrides from the process environment (read through
    /// `env.test_overrides`, so they apply on test channels only).
    pub fn for_env(env: Env) -> io::Result<CoreConfig> {
        let overrides = env.test_overrides(&|name| std::env::var(name).ok());
        let test_tools = matches!(env.channel(), Channel::Test(_));
        let trusted = TrustedDirs::for_env(&env, &current_exe_dir()?);
        Ok(CoreConfig {
            env,
            clock: Arc::new(SystemClock::new()),
            ids: Arc::new(RandomIds),
            inspector: Arc::new(SystemInspector),
            trusted,
            engine_tokens: Arc::new(NoEngineTokens),
            idle_after: overrides
                .idle_secs
                .map_or(DEFAULT_IDLE_AFTER, Duration::from_secs),
            hello_timeout: DEFAULT_HELLO_TIMEOUT,
            test_tools,
            test_parent_pid: overrides.parent_pid,
            #[cfg(test)]
            panic_on_method: None,
        })
    }
}

/// Why a core stopped or could not start. [`CoreExit::code`] is the process exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreExit {
    /// Another core already owns the channel's pipe.
    AlreadyRunning,
    /// Startup failed for another reason.
    Startup(String),
    /// Stopped after a fatal error.
    Fatal(String),
    /// The suite `state.db` is damaged; nothing was changed.
    StateDamaged(PathBuf),
    /// The suite `state.db` is from a newer version; nothing was changed.
    StateTooNew(PathBuf),
}

impl CoreExit {
    /// The exit code of `ww-core` (Appendix D): 3, 2, 4, 5 and 6.
    pub fn code(&self) -> u8 {
        match self {
            CoreExit::AlreadyRunning => 3,
            CoreExit::Startup(_) => 2,
            CoreExit::Fatal(_) => 4,
            CoreExit::StateDamaged(_) => 5,
            CoreExit::StateTooNew(_) => 6,
        }
    }
}

impl std::fmt::Display for CoreExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreExit::AlreadyRunning => f.write_str("another core already owns the pipe"),
            CoreExit::Startup(why) => write!(f, "the core could not start: {why}"),
            CoreExit::Fatal(why) => write!(f, "the core stopped after a fatal error: {why}"),
            CoreExit::StateDamaged(path) => {
                write!(f, "the state database {} is damaged", path.display())
            }
            CoreExit::StateTooNew(path) => write!(
                f,
                "the state database {} is from a newer version",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CoreExit {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-026
    fn exit_codes_match_the_protocol_appendix() {
        let path = PathBuf::from("state.db");
        let codes: Vec<u8> = [
            CoreExit::AlreadyRunning,
            CoreExit::Startup(String::new()),
            CoreExit::Fatal(String::new()),
            CoreExit::StateDamaged(path.clone()),
            CoreExit::StateTooNew(path),
        ]
        .iter()
        .map(CoreExit::code)
        .collect();
        assert_eq!(codes, [3, 2, 4, 5, 6]);
    }
}
