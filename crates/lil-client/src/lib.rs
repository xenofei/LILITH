//! The client side of the core pipe (ARCH-110): `lil`, the apps and `lil-mcp` use it to reach
//! `lil-core`.
//!
//! [`Client::connect`] finds the channel's core, or starts one on demand as a detached
//! process (ARCH-004, ARCH-018, ARCH-019). It verifies the process serving the pipe before
//! saying anything to it (ARCH-027), then says `core/hello` (ARCH-029). A core with an older
//! major protocol is drained and replaced (unless the client may not start a core); a newer
//! one is left running and this client reports itself incompatible (ARCH-023, ARCH-014). The
//! client then calls tools, minting idempotency keys (ARCH-110), and follows events
//! (ARCH-032). [`drain_and_wait`] is `lil core stop`. [`resolve_workspace`] is the `cli`
//! workspace chain (ARCH-030); a core that does not know the workspace it names is
//! [`ClientError::UnknownWorkspace`], naming the folder. [`blocking`] wraps all of it for
//! synchronous callers such as `lil`.
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
pub mod doctor;
mod workspace;

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use lil_platform::Env;
use lil_platform::process::current_exe_dir;
use lil_protocol::{
    BUILD_ID, CallerKind, ErrorBody, NO_WORKSPACE, PROTOCOL, ProtocolVersion, RpcError,
};

pub use conn::EventStream;
pub use connect::{Client, DrainOutcome, drain_and_wait};
pub use workspace::{
    ResolvedWorkspace, WorkspaceOrigin, WorkspaceSource, resolve_workspace, resolve_workspace_id,
};

/// The start of an `Untrusted` reason for a core of the current user whose executable is in
/// another folder: the one case `lil core stop` can clear (`drain_and_wait` asks only cores of
/// the current user that it could inspect).
pub(crate) const WRONG_FOLDER: &str = "its executable is not in the folder this client trusts";

/// The core executable's file name, next to the client's own executable.
pub const CORE_EXE: &str = "lil-core.exe";

/// Test channels only: the protocol `core/hello` says instead of this build's (`"M.m"`), to
/// test how a client meets an incompatible core (QA-066).
pub const ENV_TEST_CLIENT_PROTOCOL: &str = "LIL_TEST_CLIENT_PROTOCOL";

/// This build's protocol, or on a test channel the one `LIL_TEST_CLIENT_PROTOCOL` names (a
/// value that is not `"M.m"` is ignored).
fn protocol_for(
    channel: &lil_platform::Channel,
    var: &dyn Fn(&str) -> Option<String>,
) -> ProtocolVersion {
    match channel {
        lil_platform::Channel::Test(_) => var(ENV_TEST_CLIENT_PROTOCOL)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(PROTOCOL),
        _ => PROTOCOL,
    }
}

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
    /// Where `workspace` came from, when the `cli` chain named it: a core that does not know
    /// it is then [`ClientError::UnknownWorkspace`], naming the folder, not the id.
    pub workspace_origin: Option<WorkspaceOrigin>,
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
    /// - `core_exe` is `lil-core.exe` in this executable's folder;
    /// - `server_dirs` is the install folder on the stable channel, else this executable's
    ///   folder (ARCH-027);
    /// - `protocol` and `build_id` are this build's (on a test channel `protocol` may come
    ///   from `LIL_TEST_CLIENT_PROTOCOL`); no workspace (`"none"`), interactive,
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
        let protocol = protocol_for(env.channel(), &|name| std::env::var(name).ok());
        Ok(ClientConfig {
            env,
            caller_kind,
            non_interactive: false,
            workspace: NO_WORKSPACE.to_string(),
            workspace_origin: None,
            start: StartPolicy::StartIfNeeded,
            core_exe: own_dir.join(CORE_EXE),
            server_dirs,
            protocol,
            start_timeout: Duration::from_secs(10),
            start_hard_cap: Duration::from_secs(60),
            build_id: BUILD_ID.to_string(),
            spawn_env: Vec::new(),
        })
    }

    /// Names `ws` in `core/hello`: its id, and where it came from.
    pub fn set_workspace(&mut self, ws: ResolvedWorkspace) {
        self.workspace = ws.id;
        self.workspace_origin = ws.origin;
    }

    /// `lil-mcp`'s configuration: [`ClientConfig::for_env`] for this process's channel
    /// (`Env::from_process`) as an `mcp` caller, naming the workspace the `mcp` chain finds
    /// from `cwd`, the folder Claude Code started the server in (ARCH-030): the workspace
    /// containing it, else the default workspace in `suite.toml`. When that chain fails (an
    /// unreadable `suite.toml`, a default workspace whose folder is gone), the session has no
    /// workspace, and the second value says why, for the caller to report. On a test channel,
    /// `protocol` may come from `LIL_TEST_CLIENT_PROTOCOL`.
    ///
    /// # Errors
    /// [`ClientError::Io`] when this process's channel folders or executable folder cannot be
    /// found.
    pub fn for_mcp(
        cwd: &std::path::Path,
    ) -> Result<(ClientConfig, Option<ClientError>), ClientError> {
        let env = Env::from_process().map_err(|e| {
            ClientError::Io(io::Error::other(format!(
                "Cannot tell which LILITH folders to use: {e}"
            )))
        })?;
        let mut cfg = ClientConfig::for_env(env, CallerKind::Mcp).map_err(ClientError::Io)?;
        match resolve_workspace(&cfg.env, None, cwd) {
            Ok(ws) => {
                cfg.set_workspace(ws);
                Ok((cfg, None))
            }
            Err(e) => Ok((cfg, Some(e))),
        }
    }
}

/// A fresh id `<prefix>_<16 hex>`: `ik` for an idempotency key, `t` for a trace id. The keys
/// [`Client::call_tool`] mints are these.
pub fn mint_id(prefix: &str) -> String {
    use lil_platform::IdSource as _;
    lil_platform::RandomIds.id(prefix)
}

/// Why a core this client started could not start, from its exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFailure {
    /// Exit code 5: the suite `state.db` is damaged; nothing was changed.
    StateDamaged,
    /// Exit code 6: the suite `state.db` is from a newer version; nothing was changed.
    StateTooNew,
    /// Exit code 7: the suite `state.db` has a hot rollback journal (a crash mid-write);
    /// nothing was changed.
    StateHotJournal,
    /// Any other exit code; the core's log may say why (a core that fails before its log
    /// starts writes none).
    Other,
}

impl StartFailure {
    /// The failure an exit code of `lil-core` means.
    pub fn from_exit_code(code: Option<u32>) -> StartFailure {
        match code {
            Some(5) => StartFailure::StateDamaged,
            Some(6) => StartFailure::StateTooNew,
            Some(7) => StartFailure::StateHotJournal,
            _ => StartFailure::Other,
        }
    }
}

/// What went wrong reaching or using the core. [`fmt::Display`] is the plain-language message
/// `lil` prints.
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
    /// The core answered with an error result.
    Refused(ErrorBody),
    /// The core refused the hello with `workspace.unknown` for the workspace the `cli` chain
    /// found ([`ClientConfig::workspace_origin`]): most often a folder with `lilith.yaml`
    /// that is not registered in `suite.toml`. `default` is the default workspace `suite.toml`
    /// names, the one the core serves (`None` when none is set or it can't be read).
    UnknownWorkspace {
        origin: WorkspaceOrigin,
        default: Option<PathBuf>,
        error: ErrorBody,
    },
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
            ClientError::NotRunning => f.write_str("The LILITH core is not running."),
            ClientError::Untrusted { pid, image, reason } => {
                if image.as_os_str().is_empty() {
                    write!(f, "The LILITH core pipe is served by process {pid}")?;
                } else {
                    write!(
                        f,
                        "The LILITH core pipe is served by {} (pid {pid})",
                        image.display()
                    )?;
                }
                write!(f, ", which this client does not trust: {reason}.")?;
                // Only a same-user core in another folder can be asked to drain.
                if reason.starts_with(WRONG_FOLDER) {
                    f.write_str(" `lil core stop` can ask it to finish.")?;
                }
                Ok(())
            }
            ClientError::Incompatible { core_protocol } if core_protocol.is_empty() => f
                .write_str(
                    "The running LILITH core speaks a protocol version this client cannot use (it did not say which). Update the older of the two; the core was left running.",
                ),
            ClientError::Incompatible { core_protocol } => write!(
                f,
                "The running LILITH core speaks protocol {core_protocol}, which this client cannot use. Update the older of the two; the core was left running."
            ),
            ClientError::StartFailed {
                kind: StartFailure::StateDamaged,
                state_db,
                ..
            } => write!(
                f,
                "The core can't start: its state database at {} is damaged. Nothing was changed. `lil doctor` can set it aside.",
                state_db.display()
            ),
            ClientError::StartFailed {
                kind: StartFailure::StateHotJournal,
                state_db,
                ..
            } => write!(
                f,
                "The core can't start: its state database at {} was left mid-write by a crash. Nothing was changed. `lil doctor` can finish the recovery.",
                state_db.display()
            ),
            ClientError::StartFailed {
                kind: StartFailure::StateTooNew,
                state_db,
                ..
            } => write!(
                f,
                "The core can't start: its state database at {} is from a newer version of LILITH. Nothing was changed.",
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
                write!(
                    f,
                    ". Its log folder {} may say why.",
                    logs_dir.display()
                )
            }
            ClientError::Timeout => f.write_str("The LILITH core did not answer in time."),
            ClientError::Refused(body) => f.write_str(&body.message),
            ClientError::UnknownWorkspace {
                origin:
                    WorkspaceOrigin {
                        path,
                        source: WorkspaceSource::Default,
                    },
                ..
            } => write!(
                f,
                "The workspace {} ({}) is not one the core can use. Change default_workspace there, or pass --workspace.",
                path.display(),
                WorkspaceSource::Default
            ),
            // A workspace named by --workspace is never told to become the default: it is told
            // which workspace the core serves.
            ClientError::UnknownWorkspace {
                origin:
                    WorkspaceOrigin {
                        path,
                        source: WorkspaceSource::Flag,
                    },
                default,
                ..
            } => {
                write!(
                    f,
                    "The workspace \"{}\" ({}) is not registered in suite.toml, so the core cannot use it. ",
                    path.display(),
                    WorkspaceSource::Flag
                )?;
                match default {
                    Some(default) => write!(
                        f,
                        "The core serves only the default workspace, \"{}\": run the command without --workspace, or from inside that workspace.",
                        default.display()
                    ),
                    None => f.write_str(
                        "The core serves only the default workspace, and none is set: run the command without --workspace.",
                    ),
                }
            }
            ClientError::UnknownWorkspace {
                origin: WorkspaceOrigin { path, source },
                ..
            } => write!(
                f,
                "The workspace \"{}\" ({source}) is not registered in suite.toml, so the core cannot use it. To use it, make it the default with `lil workspace use \"{}\"`.",
                path.display(),
                path.display()
            ),
            ClientError::Protocol(e) => write!(
                f,
                "The LILITH core answered with a protocol error ({}): {}",
                e.code, e.message
            ),
            ClientError::Io(e) => write!(f, "{e}"),
            ClientError::Closed => f.write_str("The LILITH core closed the connection."),
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
        let env = lil_testkit::TestEnv::new();
        let cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        let own = current_exe_dir().unwrap();
        assert_eq!(cfg.core_exe, own.join("lil-core.exe"));
        assert_eq!(cfg.server_dirs, vec![own]);
        assert_eq!(cfg.protocol, PROTOCOL);
        assert_eq!(cfg.build_id, BUILD_ID);
        assert_eq!(cfg.workspace, "none");
        assert_eq!(cfg.start, StartPolicy::StartIfNeeded);
        assert_eq!(cfg.start_timeout, Duration::from_secs(10));
        assert_eq!(cfg.start_hard_cap, Duration::from_secs(60));
        assert!(cfg.spawn_env.is_empty());
    }

    /// `LIL_TEST_CLIENT_PROTOCOL` replaces the protocol said in `core/hello` on test channels
    /// only; a value that is not `"M.m"` is ignored.
    #[test] // spec: ARCH-014, QA-066
    fn client_protocol_override_is_test_channels_only() {
        let test = lil_platform::Channel::Test("ab12cd34".into());
        let set = |name: &str| (name == ENV_TEST_CLIENT_PROTOCOL).then(|| "0.9".to_string());
        let older = ProtocolVersion { major: 0, minor: 9 };
        assert_eq!(protocol_for(&test, &set), older);
        for channel in [lil_platform::Channel::Dev, lil_platform::Channel::Stable] {
            assert_eq!(protocol_for(&channel, &set), PROTOCOL, "{channel}");
        }
        let garbage = |name: &str| (name == ENV_TEST_CLIENT_PROTOCOL).then(|| "one".to_string());
        assert_eq!(protocol_for(&test, &garbage), PROTOCOL);
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
        assert_eq!(
            StartFailure::from_exit_code(Some(7)),
            StartFailure::StateHotJournal
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
            r"The core can't start: its state database at C:\t\local\state.db is damaged. Nothing was changed. `lil doctor` can set it aside."
        );
        assert_eq!(
            failed(Some(7), StartFailure::StateHotJournal).to_string(),
            r"The core can't start: its state database at C:\t\local\state.db was left mid-write by a crash. Nothing was changed. `lil doctor` can finish the recovery."
        );
        assert_eq!(
            failed(Some(6), StartFailure::StateTooNew).to_string(),
            r"The core can't start: its state database at C:\t\local\state.db is from a newer version of LILITH. Nothing was changed."
        );
        let other = failed(Some(2), StartFailure::Other).to_string();
        assert!(other.contains(r"C:\t\local\logs"), "{other}");
        assert!(other.contains("exit code 2"), "{other}");
        // A core can fail before its log exists: the folder only "may" say why.
        assert!(other.contains("may say why"), "{other}");
    }

    fn unknown(source: WorkspaceSource, default: Option<&str>) -> ClientError {
        ClientError::UnknownWorkspace {
            origin: WorkspaceOrigin {
                path: PathBuf::from(r"C:\ws\Ünïcödé space"),
                source,
            },
            default: default.map(PathBuf::from),
            error: ErrorBody::new(
                &lil_protocol::codes::WORKSPACE_UNKNOWN,
                "The workspace 0123456789abcdef is not one this core knows.",
                "t_1",
            ),
        }
    }

    #[test] // spec: ARCH-030
    fn unknown_workspace_names_the_folder_and_where_it_came_from() {
        for (source, said) in [
            (WorkspaceSource::Flag, "--workspace"),
            (WorkspaceSource::CurrentFolder, "current folder"),
        ] {
            let text = unknown(source, None).to_string();
            assert!(text.contains(r"C:\ws\Ünïcödé space"), "{text}");
            assert!(text.contains(said), "{text}");
            assert!(text.contains("not registered in suite.toml"), "{text}");
            assert!(!text.contains("0123456789abcdef"), "{text}");
            assert!(!text.contains("plan 2b"), "{text}");
        }
        let text = unknown(WorkspaceSource::CurrentFolder, None).to_string();
        assert!(
            text.contains(r#"`lil workspace use "C:\ws\Ünïcödé space"`"#),
            "{text}"
        );
        let text = unknown(WorkspaceSource::Default, None).to_string();
        assert!(text.contains(r"C:\ws\Ünïcödé space"), "{text}");
        assert!(text.contains("default_workspace"), "{text}");
    }

    #[test] // spec: ARCH-090, QA-003
    fn unregistered_hint_quotes_the_path_and_flag_source_is_not_told_to_switch() {
        // Found from the current folder: the hint to make it the default quotes the path.
        let text = unknown(WorkspaceSource::CurrentFolder, Some(r"D:\main ws")).to_string();
        assert!(text.contains(r#""C:\ws\Ünïcödé space""#), "{text}");
        assert!(
            text.ends_with(r#"`lil workspace use "C:\ws\Ünïcödé space"`."#),
            "{text}"
        );
        assert!(!text.contains("(plan 2b)"), "{text}");

        // Named by --workspace: never told to become the default; told which workspace the
        // core serves, and to run without --workspace or from inside it.
        let text = unknown(WorkspaceSource::Flag, Some(r"D:\main ws")).to_string();
        assert!(!text.contains("lil workspace use"), "{text}");
        assert!(text.contains(r#""C:\ws\Ünïcödé space""#), "{text}");
        assert!(
            text.contains(r#"serves only the default workspace, "D:\main ws""#),
            "{text}"
        );
        assert!(text.contains("without --workspace"), "{text}");
        assert!(text.contains("from inside"), "{text}");
        let text = unknown(WorkspaceSource::Flag, None).to_string();
        assert!(!text.contains("lil workspace use"), "{text}");
        assert!(text.contains("none is set"), "{text}");
        assert!(text.contains("without --workspace"), "{text}");
    }

    #[test] // spec: ARCH-014, ARCH-027
    fn messages_stay_plain_when_details_are_missing() {
        // A refusal without data.core_protocol.
        let unknown = ClientError::Incompatible {
            core_protocol: String::new(),
        }
        .to_string();
        assert!(!unknown.contains("protocol ,"), "{unknown}");
        assert!(unknown.contains("did not say which"), "{unknown}");

        // `lil core stop` is offered only for a same-user core in another folder: the only
        // kind `drain_and_wait` asks.
        let untrusted = |reason: String| ClientError::Untrusted {
            pid: 77,
            image: PathBuf::from(r"C:\other bin\lil-core.exe"),
            reason,
        };
        let folder = untrusted(format!(r"{WRONG_FOLDER} (C:\bin)")).to_string();
        assert!(folder.contains("lil core stop"), "{folder}");
        assert!(folder.contains(r"C:\other bin\lil-core.exe"), "{folder}");
        let user = untrusted("it runs as another user".to_string()).to_string();
        assert!(!user.contains("lil core stop"), "{user}");
        assert!(user.contains("77"), "{user}");
    }

    #[test]
    fn messages_name_the_core_lilith() {
        for (err, want) in [
            (ClientError::NotRunning, "The LILITH core is not running."),
            (
                ClientError::Timeout,
                "The LILITH core did not answer in time.",
            ),
            (
                ClientError::Closed,
                "The LILITH core closed the connection.",
            ),
        ] {
            assert_eq!(err.to_string(), want);
        }
        let incompatible = ClientError::Incompatible {
            core_protocol: "9.0".to_string(),
        }
        .to_string();
        assert!(
            incompatible.starts_with("The running LILITH core speaks protocol 9.0"),
            "{incompatible}"
        );
        let untrusted = ClientError::Untrusted {
            pid: 5,
            image: PathBuf::new(),
            reason: "it runs as another user".to_string(),
        }
        .to_string();
        assert!(
            untrusted.starts_with("The LILITH core pipe is served by process 5"),
            "{untrusted}"
        );
    }
}
