//! Names of the process environment a core or client reads (ARCH-094, ARCH-095, QA-038).
//!
//! | Variable | Meaning |
//! |---|---|
//! | `LIL_BUILD_CHANNEL` (compile time) | `stable` only in release packaging; default `dev` |
//! | `LIL_BUILD_ID` (compile time) | build id suffix; `build_id` is `<CARGO_PKG_VERSION>+<LIL_BUILD_ID or "local">` |
//! | `LIL_CHANNEL` | a dev build accepts `dev` or `test-<id>`; a stable build refuses anything but `stable` |
//! | `LIL_TEST_ROOT` | test channels only: `<root>\{roaming,local,profile}` |
//! | `LIL_TEST_PARENT_PID` | test channels only |
//! | `LIL_TEST_IDLE_SECS` | test channels only |

/// Runtime channel request.
pub const ENV_CHANNEL: &str = "LIL_CHANNEL";
/// Test channels only: the folder that holds `roaming`, `local` and `profile`.
pub const ENV_TEST_ROOT: &str = "LIL_TEST_ROOT";
/// Test channels only: the test process the core exits with.
pub const ENV_TEST_PARENT_PID: &str = "LIL_TEST_PARENT_PID";
/// Test channels only: the idle timeout in seconds.
pub const ENV_TEST_IDLE_SECS: &str = "LIL_TEST_IDLE_SECS";

/// The channel this binary was built for: `stable` in release packaging, else `dev`.
pub const BUILD_CHANNEL: &str = match option_env!("LIL_BUILD_CHANNEL") {
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
        assert_eq!(ENV_CHANNEL, "LIL_CHANNEL");
        assert_eq!(ENV_TEST_ROOT, "LIL_TEST_ROOT");
        assert_eq!(ENV_TEST_PARENT_PID, "LIL_TEST_PARENT_PID");
        assert_eq!(ENV_TEST_IDLE_SECS, "LIL_TEST_IDLE_SECS");
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

    /// Every variable the suite reads or sets at build time is `LIL_*` (map row 12): the
    /// runtime names above, the compile-time channel read here, and the build id and protocol
    /// suffix of `lil-protocol`'s build script. No name with the old prefix is left in either file.
    #[test] // spec: ARCH-094, ARCH-095
    fn env_vars_are_lil_prefixed() {
        for name in [
            ENV_CHANNEL,
            ENV_TEST_ROOT,
            ENV_TEST_PARENT_PID,
            ENV_TEST_IDLE_SECS,
        ] {
            assert!(name.starts_with("LIL_"), "{name}");
        }
        let this = include_str!("process_env.rs");
        assert!(
            this.contains(concat!("option_env!(\"", "LIL_BUILD_CHANNEL", "\")")),
            "the compile-time channel is LIL_BUILD_CHANNEL"
        );
        let build = include_str!("../../lil-protocol/build.rs");
        for want in [
            concat!("rerun-if-env-changed=", "LIL_BUILD_ID"),
            concat!("env::var(\"", "LIL_BUILD_ID", "\")"),
            concat!("rustc-env=", "LIL_PROTOCOL_BUILD_SUFFIX="),
        ] {
            assert!(build.contains(want), "lil-protocol/build.rs lacks {want}");
        }
        let old = ["W", "W_"].concat();
        for (file, text) in [("process_env.rs", this), ("lil-protocol/build.rs", build)] {
            assert!(!text.contains(&old), "{file} still names an old variable");
        }
    }
}
