//! Names of the process environment a core or client reads (ARCH-094, ARCH-095, QA-038).
//!
//! | Variable | Meaning |
//! |---|---|
//! | `WW_BUILD_CHANNEL` (compile time) | `stable` only in release packaging; default `dev` |
//! | `WW_BUILD_ID` (compile time) | build id suffix; `build_id` is `<CARGO_PKG_VERSION>+<WW_BUILD_ID or "local">` |
//! | `WW_CHANNEL` | a dev build accepts `dev` or `test-<id>`; a stable build refuses anything but `stable` |
//! | `WW_TEST_ROOT` | test channels only: `<root>\{roaming,local,profile}` |
//! | `WW_TEST_PARENT_PID` | test channels only |
//! | `WW_TEST_IDLE_SECS` | test channels only |

/// Runtime channel request.
pub const ENV_CHANNEL: &str = "WW_CHANNEL";
/// Test channels only: the folder that holds `roaming`, `local` and `profile`.
pub const ENV_TEST_ROOT: &str = "WW_TEST_ROOT";
/// Test channels only: the test process the core exits with.
pub const ENV_TEST_PARENT_PID: &str = "WW_TEST_PARENT_PID";
/// Test channels only: the idle timeout in seconds.
pub const ENV_TEST_IDLE_SECS: &str = "WW_TEST_IDLE_SECS";

/// The channel this binary was built for: `stable` in release packaging, else `dev`.
pub const BUILD_CHANNEL: &str = match option_env!("WW_BUILD_CHANNEL") {
    Some(channel) => channel,
    None => "dev",
};

/// The user SID that test channels put in pipe names, so test processes agree on the name
/// without querying the token. It is a well-formed SID that belongs to no account.
pub const TEST_PIPE_SID: &str = "S-1-5-21-1000-2000-3000-1001";

/// Test-only knobs; every field is `None` outside a test channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TestOverrides {
    pub parent_pid: Option<u32>,
    pub idle_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-095
    fn constants_match_the_protocol_appendix() {
        assert_eq!(ENV_CHANNEL, "WW_CHANNEL");
        assert_eq!(ENV_TEST_ROOT, "WW_TEST_ROOT");
        assert_eq!(ENV_TEST_PARENT_PID, "WW_TEST_PARENT_PID");
        assert_eq!(ENV_TEST_IDLE_SECS, "WW_TEST_IDLE_SECS");
        assert_eq!(TEST_PIPE_SID, "S-1-5-21-1000-2000-3000-1001");
        assert!(matches!(BUILD_CHANNEL, "dev" | "stable"));
        assert_eq!(
            TestOverrides::default(),
            TestOverrides {
                parent_pid: None,
                idle_secs: None
            }
        );
    }
}
