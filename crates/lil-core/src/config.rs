//! What a core needs to run: its `Env`, the clock and ids, the process inspector, who it
//! trusts, and the timing knobs (infrastructure, not a CORE-014 module).

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lil_platform::fs::{Probe, SystemProbe};
use lil_platform::process::{ProcessInspector, SystemInspector, current_exe_dir};
use lil_platform::{Channel, Clock, Env, IdSource, RandomIds, SystemClock};

use crate::identity::{EngineTokens, NoEngineTokens, TrustedDirs};
use crate::store::FailPoints;
use crate::watch::WatchConfig;

/// A core with no connections, calls or busy blockers exits after this long (ARCH-020).
const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(600);
/// A connection that has not said hello after this long is closed.
const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(30);
/// A confirmation expires this long after it was created (CORE-115, owner question 6).
const DEFAULT_CONFIRM_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// Test channels only: the fake hardware script the sampler reads instead of the real sources
/// (`lil_testkit::FakeHwScript`, QA-066).
pub const ENV_TEST_FAKE_HW: &str = "LIL_TEST_FAKE_HW";
/// Test channels only: a confirmation's lifetime in seconds instead of 30 minutes.
pub const ENV_TEST_CONFIRM_LIFETIME_SECS: &str = "LIL_TEST_CONFIRM_LIFETIME_SECS";

/// The plan-3 test knobs from `var`: `(LIL_TEST_FAKE_HW, LIL_TEST_CONFIRM_LIFETIME_SECS)`, both
/// `None` off a test channel; a lifetime that does not parse is `None`.
fn test_knobs(
    channel: &Channel,
    var: &dyn Fn(&str) -> Option<String>,
) -> (Option<PathBuf>, Option<Duration>) {
    if !matches!(channel, Channel::Test(_)) {
        return (None, None);
    }
    let fake_hw = var(ENV_TEST_FAKE_HW)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let lifetime = var(ENV_TEST_CONFIRM_LIFETIME_SECS)
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_secs);
    (fake_hw, lifetime)
}

/// A fault an in-process test injects into the creation of pipe instances
/// ([`CoreConfig::pipe_instance_fault`]).
#[doc(hidden)]
pub type PipeInstanceFault = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

/// A step of watching the test parent ([`CoreConfig::parent_watch_fault`]).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentWatchStep {
    /// Before the parent is opened (`process::open`).
    Open,
    /// Before the wait for its exit is registered (`ProcessHandle::wait`).
    Wait,
}

/// A fault an in-process test injects into watching the test parent
/// ([`CoreConfig::parent_watch_fault`]).
#[doc(hidden)]
pub type ParentWatchFault = Arc<dyn Fn(ParentWatchStep) -> io::Result<()> + Send + Sync>;

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
    /// 600 s, or `LIL_TEST_IDLE_SECS` on a test channel.
    pub idle_after: Duration,
    /// 30 s.
    pub hello_timeout: Duration,
    /// How long a confirmation stays answerable: 30 minutes, or
    /// `LIL_TEST_CONFIRM_LIFETIME_SECS` on a test channel.
    pub confirm_lifetime: Duration,
    /// Test channels only: the fake hardware script `LIL_TEST_FAKE_HW` names, read instead of
    /// NVML, PDH and `sysinfo`.
    pub fake_hw: Option<PathBuf>,
    /// The channel is `test-*`: the test tools are registered.
    pub test_tools: bool,
    /// Test channels only: the core exits 0 once this process is gone.
    pub test_parent_pid: Option<u32>,
    /// The fail points `LIL_TEST_FAILPOINTS` arms (test channels of non-stable builds only;
    /// QA-066), passed to every workspace store.
    pub failpoints: Arc<FailPoints>,
    /// The file facts of the workspace stores (`SystemProbe`); in-process tests fake volumes
    /// and placeholders with their own.
    pub probe: Arc<dyn Probe>,
    /// The workspace watchers' buffer, batch window and git quiet time
    /// (`WatchConfig::default()`: 64 KiB, 200 ms, 2 s).
    pub watch: WatchConfig,
    /// Added to the environment of every `git` the core runs (`Store::git`): empty in
    /// production, `TestEnv::git_env()` for in-process test cores, so their `git` runs read
    /// only the test profile's config, never the system or user one (QA-038). A spawned core
    /// needs nothing here: it inherits the variables from its parent.
    pub git_env: Vec<(OsString, OsString)>,
    /// In-process tests only (`None` from [`CoreConfig::for_env`]): runs before each pipe
    /// instance after the claimed first one is created, and an `Err` is that instance's
    /// creation error. The core then stops as after a fatal error (exit 4).
    #[doc(hidden)]
    pub pipe_instance_fault: Option<PipeInstanceFault>,
    /// In-process tests only (`None` from [`CoreConfig::for_env`]): runs before each step of
    /// watching the test parent, and an `Err` is that step's error. `NotFound` at `Open` means
    /// the parent is gone (exit 0); any other error stops the core as after a fatal error
    /// (exit 4).
    #[doc(hidden)]
    pub parent_watch_fault: Option<ParentWatchFault>,
    /// The crate's own unit tests only (compiled out of every build): a request with this
    /// method panics in the router, to test how the transport survives a panicking handler.
    #[cfg(test)]
    pub panic_on_method: Option<&'static str>,
}

impl CoreConfig {
    /// The system clock, random ids, the real process inspector, the trusted folders of
    /// `env` (the running executable's folder on dev and test channels) and no engine tokens,
    /// with the `LIL_TEST_*` overrides from the process environment (read through
    /// `env.test_overrides`, so they apply on test channels only), `LIL_TEST_FAKE_HW` and
    /// `LIL_TEST_CONFIRM_LIFETIME_SECS` (test channels only too), the fail points of
    /// `LIL_TEST_FAILPOINTS` (`FailPoints::from_env`), and the real probe.
    pub fn for_env(env: Env) -> io::Result<CoreConfig> {
        let overrides = env.test_overrides(&|name| std::env::var(name).ok());
        let (fake_hw, confirm_lifetime) =
            test_knobs(env.channel(), &|name| std::env::var(name).ok());
        let test_tools = matches!(env.channel(), Channel::Test(_));
        let trusted = TrustedDirs::for_env(&env, &current_exe_dir()?);
        let failpoints = FailPoints::from_env(&env, &|name| std::env::var(name).ok());
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
            confirm_lifetime: confirm_lifetime.unwrap_or(DEFAULT_CONFIRM_LIFETIME),
            fake_hw,
            test_tools,
            test_parent_pid: overrides.parent_pid,
            failpoints,
            probe: Arc::new(SystemProbe),
            watch: WatchConfig::default(),
            git_env: Vec::new(),
            pipe_instance_fault: None,
            parent_watch_fault: None,
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
    /// The suite `state.db` is damaged; nothing was changed.
    StateDamaged(PathBuf),
    /// The suite `state.db` is from a newer version; nothing was changed.
    StateTooNew(PathBuf),
    /// The suite `state.db` has a hot rollback journal (a crash mid-write); nothing was
    /// changed.
    StateHotJournal(PathBuf),
}

impl CoreExit {
    /// The exit code of `lil-core` (Appendix D): 3, 2, 5, 6 and 7. Code 4 (stopped after a fatal
    /// error: a state actor that died, a pipe instance after the first that could not be
    /// created, a test parent that cannot be watched) is a shutdown's, from
    /// `ShutdownReport::exit_code`.
    pub fn code(&self) -> u8 {
        match self {
            CoreExit::AlreadyRunning => 3,
            CoreExit::Startup(_) => 2,
            CoreExit::StateDamaged(_) => 5,
            CoreExit::StateTooNew(_) => 6,
            CoreExit::StateHotJournal(_) => 7,
        }
    }
}

impl std::fmt::Display for CoreExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreExit::AlreadyRunning => f.write_str("another core already owns the pipe"),
            CoreExit::Startup(why) => write!(f, "the core could not start: {why}"),
            CoreExit::StateDamaged(path) => {
                write!(f, "the state database {} is damaged", path.display())
            }
            CoreExit::StateTooNew(path) => write!(
                f,
                "the state database {} is from a newer version",
                path.display()
            ),
            CoreExit::StateHotJournal(path) => write!(
                f,
                "the state database {} was left mid-write by a crash",
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
            CoreExit::StateDamaged(path.clone()),
            CoreExit::StateTooNew(path.clone()),
            CoreExit::StateHotJournal(path),
        ]
        .iter()
        .map(CoreExit::code)
        .collect();
        assert_eq!(codes, [3, 2, 5, 6, 7]);
    }

    /// `LIL_TEST_FAKE_HW` and `LIL_TEST_CONFIRM_LIFETIME_SECS` count on test channels only; a
    /// value that does not parse is ignored.
    #[test] // spec: QA-066, CORE-115
    fn test_knobs_are_read_on_test_channels_only() {
        let set = |name: &str| match name {
            ENV_TEST_FAKE_HW => Some(r"C:\t\hw.json".to_string()),
            ENV_TEST_CONFIRM_LIFETIME_SECS => Some("1".to_string()),
            _ => None,
        };
        let test = Channel::Test("ab12cd34".into());
        assert_eq!(
            test_knobs(&test, &set),
            (
                Some(PathBuf::from(r"C:\t\hw.json")),
                Some(Duration::from_secs(1))
            )
        );
        let unparsable =
            |name: &str| (name == ENV_TEST_CONFIRM_LIFETIME_SECS).then(|| "soon".into());
        assert_eq!(test_knobs(&test, &unparsable), (None, None));
        for channel in [Channel::Dev, Channel::Stable] {
            assert_eq!(test_knobs(&channel, &set), (None, None), "{channel}");
        }
    }
}
