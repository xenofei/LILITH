//! The client side of the core pipe (ARCH-110): `ww`, the apps and `ww-mcp` use it to reach
//! `ww-core`.
//!
//! [`Client::connect`] finds the channel's core, or starts one on demand as a detached
//! process (ARCH-004, ARCH-018, ARCH-019). It verifies the process serving the pipe before
//! saying anything to it (ARCH-027), then says `core/hello` (ARCH-029). A core with an older
//! major protocol is drained and replaced; a newer one is left running and this client reports
//! itself incompatible (ARCH-023, ARCH-014). The client then calls tools, minting idempotency
//! keys (ARCH-110), and follows events (ARCH-032). [`drain_and_wait`] is `ww core stop`.
//! [`resolve_workspace_id`] is the `cli` workspace chain (ARCH-030). [`blocking`] wraps all of
//! it for synchronous callers such as `ww`.
//!
//! Deferred to plan 4: reconnecting and resubscribing after a core restart (APP-034/035;
//! `HelloResult::instance_id` already tells a client that a core restarted), and QA-026's
//! GUI-thread check in [`blocking`].

// `ClientError::Refused` carries the core's `ErrorBody` by value, as the interface says; it is
// larger than clippy's default for an `Err` variant.
#![allow(clippy::result_large_err)]

pub mod blocking;
mod conn;
mod connect;
mod workspace;

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ww_platform::Env;
use ww_platform::process::current_exe_dir;
use ww_protocol::{
    BUILD_ID, CallerKind, ErrorBody, NO_WORKSPACE, PROTOCOL, ProtocolVersion, RpcError,
};

pub use conn::EventStream;
pub use connect::{Client, DrainOutcome, drain_and_wait};
pub use workspace::{WORKSPACE_MANIFEST, resolve_workspace_id};

/// The core executable's file name, next to the client's own executable.
pub const CORE_EXE: &str = "ww-core.exe";

/// Whether [`Client::connect`] may start a core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartPolicy {
    /// Start one when none is running (ARCH-018).
    StartIfNeeded,
    /// Never start one: no core is [`ClientError::NotRunning`] (`--no-start`).
    NoStart,
}

/// How to reach the core.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub env: Env,
    pub caller_kind: CallerKind,
    pub non_interactive: bool,
    /// A 16-hex workspace id, or `"none"` ([`resolve_workspace_id`]).
    pub workspace: String,
    pub start: StartPolicy,
    /// The core executable to start.
    pub core_exe: PathBuf,
    /// The folders a trusted core's executable may be in (ARCH-027).
    pub server_dirs: Vec<PathBuf>,
    /// The protocol said in `core/hello`.
    pub protocol: ProtocolVersion,
    /// How long finding or starting the core may take (10 s).
    pub start_timeout: Duration,
    /// While a core this client started is still starting (a virus scanner reading a new
    /// executable, say), the start may take up to this long instead (60 s).
    pub start_hard_cap: Duration,
    pub build_id: String,
    /// Added to the started core's environment: empty in production, `TestEnv::child_env()`
    /// in tests.
    pub spawn_env: Vec<(OsString, OsString)>,
}

impl ClientConfig {
    /// The configuration for `env`:
    /// - `core_exe` is `ww-core.exe` in this executable's folder;
    /// - `server_dirs` is the install folder on the stable channel, else this executable's
    ///   folder (ARCH-027);
    /// - `protocol` and `build_id` are this build's; no workspace (`"none"`), interactive,
    ///   [`StartPolicy::StartIfNeeded`], 10 s to start (60 s at most), nothing added to the
    ///   core's environment.
    ///
    /// # Errors
    /// When this executable's folder cannot be found.
    pub fn for_env(env: Env, caller_kind: CallerKind) -> io::Result<ClientConfig> {
        let own_dir = current_exe_dir()?;
        let server_dirs = match env.install_dir() {
            Some(install_dir) => vec![install_dir],
            None => vec![own_dir.clone()],
        };
        Ok(ClientConfig {
            env,
            caller_kind,
            non_interactive: false,
            workspace: NO_WORKSPACE.to_string(),
            start: StartPolicy::StartIfNeeded,
            core_exe: own_dir.join(CORE_EXE),
            server_dirs,
            protocol: PROTOCOL,
            start_timeout: Duration::from_secs(10),
            start_hard_cap: Duration::from_secs(60),
            build_id: BUILD_ID.to_string(),
            spawn_env: Vec::new(),
        })
    }
}

/// Why a core this client started could not start, from its exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFailure {
    /// Exit code 5: the suite `state.db` is damaged; nothing was changed.
    StateDamaged,
    /// Exit code 6: the suite `state.db` is from a newer version; nothing was changed.
    StateTooNew,
    /// Any other exit code; the core's log says why.
    Other,
}

impl StartFailure {
    /// The failure an exit code of `ww-core` means.
    pub fn from_exit_code(code: Option<u32>) -> StartFailure {
        match code {
            Some(5) => StartFailure::StateDamaged,
            Some(6) => StartFailure::StateTooNew,
            _ => StartFailure::Other,
        }
    }
}

/// What went wrong reaching or using the core. [`fmt::Display`] is the plain-language message
/// `ww` prints.
#[derive(Debug)]
pub enum ClientError {
    /// No core runs and [`StartPolicy::NoStart`] forbids starting one.
    NotRunning,
    /// The process serving the pipe is not a trusted core (ARCH-027).
    Untrusted {
        pid: u32,
        image: PathBuf,
        reason: String,
    },
    /// The core speaks another major protocol version and was left running.
    Incompatible {
        core_protocol: String,
    },
    /// A core this client started exited.
    StartFailed {
        exit_code: Option<u32>,
        kind: StartFailure,
        /// The suite `state.db` (`Env::suite_state_db`).
        state_db: PathBuf,
        logs_dir: PathBuf,
    },
    /// The core did not appear or answer in time.
    Timeout,
    /// The core answered with an error result (a `core/hello` refused for an unknown
    /// workspace, say).
    Refused(ErrorBody),
    /// The core answered with a JSON-RPC protocol fault, or with something that is not the
    /// protocol.
    Protocol(RpcError),
    Io(io::Error),
    /// The core closed the connection.
    Closed,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::NotRunning => f.write_str("The Worldwright core is not running."),
            ClientError::Untrusted { pid, image, reason } => write!(
                f,
                "The Worldwright core pipe is served by {} (pid {pid}), which this client does not trust: {reason}. `ww core stop` can ask it to finish.",
                image.display()
            ),
            ClientError::Incompatible { core_protocol } => write!(
                f,
                "The running Worldwright core speaks protocol {core_protocol}, which this client cannot use. Update the older of the two; the core was left running."
            ),
            ClientError::StartFailed {
                kind: StartFailure::StateDamaged,
                state_db,
                ..
            } => write!(
                f,
                "The core can't start: its state database at {} is damaged. Nothing was changed. `ww doctor` (plan 2b) can set it aside.",
                state_db.display()
            ),
            ClientError::StartFailed {
                kind: StartFailure::StateTooNew,
                state_db,
                ..
            } => write!(
                f,
                "The core can't start: its state database at {} is from a newer version of Worldwright. Nothing was changed.",
                state_db.display()
            ),
            ClientError::StartFailed {
                exit_code,
                kind: StartFailure::Other,
                logs_dir,
                ..
            } => {
                f.write_str("The core can't start")?;
                if let Some(code) = exit_code {
                    write!(f, " (exit code {code})")?;
                }
                write!(f, ". Its log in {} says why.", logs_dir.display())
            }
            ClientError::Timeout => f.write_str("The Worldwright core did not answer in time."),
            ClientError::Refused(body) => f.write_str(&body.message),
            ClientError::Protocol(e) => write!(
                f,
                "The Worldwright core answered with a protocol error ({}): {}",
                e.code, e.message
            ),
            ClientError::Io(e) => write!(f, "{e}"),
            ClientError::Closed => f.write_str("The Worldwright core closed the connection."),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-027
    fn for_env_uses_own_folder_off_the_stable_channel() {
        let env = ww_testkit::TestEnv::new();
        let cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        let own = current_exe_dir().unwrap();
        assert_eq!(cfg.core_exe, own.join("ww-core.exe"));
        assert_eq!(cfg.server_dirs, vec![own]);
        assert_eq!(cfg.protocol, PROTOCOL);
        assert_eq!(cfg.build_id, BUILD_ID);
        assert_eq!(cfg.workspace, "none");
        assert_eq!(cfg.start, StartPolicy::StartIfNeeded);
        assert_eq!(cfg.start_timeout, Duration::from_secs(10));
        assert_eq!(cfg.start_hard_cap, Duration::from_secs(60));
        assert!(cfg.spawn_env.is_empty());
    }

    #[test] // spec: CORE-004
    fn start_failures_follow_the_core_exit_codes() {
        assert_eq!(
            StartFailure::from_exit_code(Some(5)),
            StartFailure::StateDamaged
        );
        assert_eq!(
            StartFailure::from_exit_code(Some(6)),
            StartFailure::StateTooNew
        );
        for other in [Some(2), Some(4), Some(0), None] {
            assert_eq!(StartFailure::from_exit_code(other), StartFailure::Other);
        }
        let failed = |exit_code, kind| ClientError::StartFailed {
            exit_code,
            kind,
            state_db: PathBuf::from(r"C:\t\local\state.db"),
            logs_dir: PathBuf::from(r"C:\t\local\logs"),
        };
        assert_eq!(
            failed(Some(5), StartFailure::StateDamaged).to_string(),
            r"The core can't start: its state database at C:\t\local\state.db is damaged. Nothing was changed. `ww doctor` (plan 2b) can set it aside."
        );
        assert_eq!(
            failed(Some(6), StartFailure::StateTooNew).to_string(),
            r"The core can't start: its state database at C:\t\local\state.db is from a newer version of Worldwright. Nothing was changed."
        );
        let other = failed(Some(2), StartFailure::Other).to_string();
        assert!(other.contains(r"C:\t\local\logs"), "{other}");
        assert!(other.contains("exit code 2"), "{other}");
    }
}
