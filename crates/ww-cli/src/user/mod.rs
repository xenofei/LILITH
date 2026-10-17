//! The `ww` user commands over `ww-client` (ARCH-016, ARCH-018, ARCH-030): `status`, `call`,
//! `events --follow`, `core stop`, `workspace init|use`, `notify` and `doctor`. Spec Appendix D, "The `ww` user
//! commands", and Appendix E, "The `ww` commands this plan adds", are the contract for their
//! flags, output and exit codes.
//!
//! Every command builds its [`ClientConfig`] for the process's channel as a `cli` caller
//! ([`config`]). `status`, `call` and `events` name the workspace the `cli` chain resolves
//! (ARCH-030); `core stop` and `workspace init|use` name none, and `doctor` names the default
//! workspace it read itself, so a broken workspace registry never stops them.
//!
//! A workspace found from the current folder that is not registered in `suite.toml` stops
//! `call` and `events` with a plain message naming the folder; `status` warns once on stderr
//! and shows the status without a workspace ([`connect`]).
//!
//! A [`ClientError`] (about 250 bytes) never crosses a function boundary here: each command
//! maps it where it occurs to its plain message on stderr and exit code 2 ([`cannot_run`]).

mod call;
mod core;
mod doctor;
mod events;
mod notify;
mod status;
mod workspace;

use std::fmt::Display;
use std::io::{self, Write as _};
use std::path::PathBuf;

use ww_client::blocking::BlockingClient;
use ww_client::{
    ClientConfig, ClientError, ResolvedWorkspace, StartPolicy, WorkspaceOrigin, WorkspaceSource,
    resolve_workspace,
};
use ww_platform::Env;
use ww_protocol::{CallerKind, NO_WORKSPACE};

/// The call or command succeeded.
pub const EXIT_OK: i32 = 0;
/// The call returned an `error` result.
pub const EXIT_ERROR_RESULT: i32 = 1;
/// `ww` could not run the command (usage, connection, untrusted core, start failure); the
/// reason is on stderr.
pub const EXIT_CANNOT_RUN: i32 = 2;
/// `ww status --no-start` found no core.
pub const EXIT_NOT_RUNNING: i32 = 3;
/// The call returned `pending_confirmation`.
pub const EXIT_PENDING: i32 = 4;

#[derive(Debug, clap::Args)]
pub struct GlobalArgs {
    /// The workspace folder to use (it must contain worldwright.yaml).
    #[arg(long, global = true, value_name = "PATH")]
    pub workspace: Option<PathBuf>,
    /// Never wait for a person: a call that needs confirming fails instead.
    #[arg(long, global = true)]
    pub non_interactive: bool,
    /// Never start the core: without one, `status` exits 3 and every other command does
    /// nothing and exits 0.
    #[arg(long, global = true)]
    pub no_start: bool,
}

#[derive(Debug, clap::Subcommand)]
pub enum UserCommand {
    /// Show whether the core is running, and its pipe, protocol, build and workspace.
    Status {
        /// Print `{"running":true,"hello":{...}}` or `{"running":false}`.
        #[arg(long)]
        json: bool,
    },
    /// Call a core tool: ww call <tool> [<JSON object> | -]. Prints the result as JSON.
    Call {
        /// The tool's name, for example `test.echo`.
        tool: String,
        /// The arguments as a JSON object (default `{}`); `-` reads them from stdin.
        args: Option<String>,
        /// Retry an earlier call: the tool runs once per key.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Print events as JSON lines until the core goes away.
    Events {
        /// Keep printing events as they come (required).
        #[arg(long, required = true)]
        follow: bool,
        /// A topic to follow; repeat for more.
        #[arg(long = "topic", required = true)]
        topics: Vec<String>,
        /// Replay a topic's recent events after this sequence number; repeat per topic.
        #[arg(long = "since", value_name = "TOPIC=SEQ")]
        since: Vec<String>,
    },
    /// Manage the core process.
    #[command(subcommand)]
    Core(CoreCommand),
    /// Create workspaces and choose the default one.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Tell the core's watcher that files changed (for folders it can't watch). Prints nothing
    /// on success.
    Notify {
        /// The changed files or folders, inside the workspace.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Check the suite's state database, the workspace registry and the default workspace, and
    /// repair what is safe to repair. Anything persistent beyond that is done only after you
    /// type y.
    Doctor,
}

#[derive(Debug, clap::Subcommand)]
pub enum WorkspaceCommand {
    /// Create a workspace (or register an existing one) and make it the default when none is set.
    Init {
        /// The workspace folder; it is created when it doesn't exist.
        path: PathBuf,
        /// The workspace's name (default: the folder's name).
        #[arg(long)]
        name: Option<String>,
        /// Make it the default workspace even when one is set.
        #[arg(long)]
        default: bool,
    },
    /// Make a workspace the default one the core serves.
    Use {
        /// A workspace folder (it holds worldwright.yaml).
        path: PathBuf,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum CoreCommand {
    /// Ask the core to finish its work and exit, then wait for it.
    Stop {
        /// How long to wait for the core to exit.
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
    },
}

/// Runs a user command and returns `ww`'s exit code.
pub fn run(cmd: UserCommand, global: &GlobalArgs) -> i32 {
    let result = match cmd {
        UserCommand::Status { json } => status::run(global, json),
        UserCommand::Call {
            tool,
            args,
            idempotency_key,
        } => call::run(global, &tool, args.as_deref(), idempotency_key),
        UserCommand::Events {
            follow: _,
            topics,
            since,
        } => events::run(global, topics, &since),
        UserCommand::Core(CoreCommand::Stop { timeout_secs }) => core::stop(global, timeout_secs),
        UserCommand::Workspace(WorkspaceCommand::Init {
            path,
            name,
            default,
        }) => workspace::init(global, &path, name.as_deref(), default),
        UserCommand::Workspace(WorkspaceCommand::Use { path }) => {
            workspace::use_default(global, &path)
        }
        UserCommand::Notify { paths } => notify::run(global, &paths),
        UserCommand::Doctor => doctor::run(global),
    };
    match result {
        Ok(code) | Err(Exit(code)) => code,
    }
}

/// A command that ended early with this exit code; its message is already on stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exit(pub(crate) i32);

/// Prints `message` on stderr: exit code 2.
pub(crate) fn cannot_run(message: impl Display) -> Exit {
    eprintln!("{message}");
    Exit(EXIT_CANNOT_RUN)
}

/// Writes `text` and a line end to stdout and flushes it.
pub(crate) fn print_line(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{text}")?;
    out.flush()
}

/// The configuration for this process's channel (`Env::from_process`) as a `cli` caller:
/// non-interactive with `--non-interactive`, never starting a core with `--no-start` or when
/// `start` says so, and no workspace (`"none"`).
pub(crate) fn config(global: &GlobalArgs, start: StartPolicy) -> Result<ClientConfig, Exit> {
    let env = Env::from_process()
        .map_err(|e| cannot_run(format!("Cannot tell which Worldwright folders to use: {e}")))?;
    let mut cfg = ClientConfig::for_env(env, CallerKind::Cli)
        .map_err(|e| cannot_run(format!("Cannot find the folder ww runs from: {e}")))?;
    cfg.non_interactive = global.non_interactive;
    cfg.start = if global.no_start {
        StartPolicy::NoStart
    } else {
        start
    };
    Ok(cfg)
}

/// What [`connect`] does when the core does not know a workspace found from the current
/// folder (one not registered in `suite.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownCwdWorkspace {
    /// The command cannot run (exit 2) and says why: `call`, `events`.
    Fail,
    /// One warning line on stderr naming the folder, then no workspace: `status`.
    WarnAndUseNone,
}

/// Connects for `status`, `call` and `events`: the [`config`] with the workspace the `cli`
/// chain resolves (ARCH-030). `None` when no core runs and `--no-start` forbids starting one.
///
/// With `--no-start`, a workspace that cannot be resolved (a stale `default_workspace` in
/// `suite.toml`, say) is reported only when a core runs: with none, the command stays a silent
/// no-op (ARCH-018). The probe for a running core reports its own failures (an untrusted
/// core, say) rather than hiding them behind the workspace message.
///
/// A workspace the core does not know is named by its folder and where it came from
/// ([`ClientError::UnknownWorkspace`]); one found from the current folder is handled as
/// `unknown_cwd` says.
pub(crate) fn connect(
    global: &GlobalArgs,
    unknown_cwd: UnknownCwdWorkspace,
) -> Result<Option<BlockingClient>, Exit> {
    let mut cfg = config(global, StartPolicy::StartIfNeeded)?;
    let cwd = std::env::current_dir()
        .map_err(|e| cannot_run(format!("Cannot read the current folder: {e}")))?;
    match resolve_workspace(&cfg.env, global.workspace.as_deref(), &cwd) {
        Ok(ws) => cfg.set_workspace(ws),
        Err(e) => {
            if cfg.start == StartPolicy::NoStart {
                cfg.set_workspace(no_workspace());
                match BlockingClient::connect(&cfg) {
                    Err(ClientError::NotRunning) => return Ok(None),
                    // A core runs: the workspace message stands.
                    Ok(_) => {}
                    Err(probe) => return Err(cannot_run(probe)),
                }
            }
            return Err(cannot_run(e));
        }
    }
    match BlockingClient::connect(&cfg) {
        Ok(client) => Ok(Some(client)),
        Err(ClientError::NotRunning) => Ok(None),
        Err(ClientError::UnknownWorkspace {
            origin:
                WorkspaceOrigin {
                    path,
                    source: WorkspaceSource::CurrentFolder,
                },
            ..
        }) if unknown_cwd == UnknownCwdWorkspace::WarnAndUseNone => {
            eprintln!(
                "warning: the workspace {} (found from the current folder) is not registered in suite.toml; showing the status without a workspace.",
                path.display()
            );
            cfg.set_workspace(no_workspace());
            match BlockingClient::connect(&cfg) {
                Ok(client) => Ok(Some(client)),
                Err(ClientError::NotRunning) => Ok(None),
                Err(e) => Err(cannot_run(e)),
            }
        }
        Err(e) => Err(cannot_run(e)),
    }
}

/// `"none"`: no workspace.
fn no_workspace() -> ResolvedWorkspace {
    ResolvedWorkspace {
        id: NO_WORKSPACE.to_string(),
        origin: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser as _;

    use super::*;
    use crate::{Cli, Command};

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn user(cli: Cli) -> UserCommand {
        match cli.command {
            Command::User(cmd) => cmd,
            Command::Dev(_) => panic!("parsed as a dev command"),
        }
    }

    #[test] // spec: ARCH-016
    fn user_commands_parse() {
        assert!(matches!(
            user(parse(&["ww", "status"])),
            UserCommand::Status { json: false }
        ));
        assert!(matches!(
            user(parse(&["ww", "status", "--json"])),
            UserCommand::Status { json: true }
        ));

        match user(parse(&["ww", "call", "test.echo"])) {
            UserCommand::Call {
                tool,
                args,
                idempotency_key,
            } => {
                assert_eq!(tool, "test.echo");
                assert_eq!(args, None);
                assert_eq!(idempotency_key, None);
            }
            _ => panic!("not a call"),
        }
        match user(parse(&[
            "ww",
            "call",
            "test.write",
            "{\"label\":\"Ünïcödé space\"}",
            "--idempotency-key",
            "k1",
        ])) {
            UserCommand::Call {
                tool,
                args,
                idempotency_key,
            } => {
                assert_eq!(tool, "test.write");
                assert_eq!(args.as_deref(), Some("{\"label\":\"Ünïcödé space\"}"));
                assert_eq!(idempotency_key.as_deref(), Some("k1"));
            }
            _ => panic!("not a call"),
        }
        assert!(matches!(
            user(parse(&["ww", "call", "test.echo", "-"])),
            UserCommand::Call { args: Some(a), .. } if a == "-"
        ));

        match user(parse(&[
            "ww", "events", "--follow", "--topic", "test:a", "--topic", "test:b", "--since",
            "test:a=0",
        ])) {
            UserCommand::Events {
                follow,
                topics,
                since,
            } => {
                assert!(follow);
                assert_eq!(topics, ["test:a", "test:b"]);
                assert_eq!(since, ["test:a=0"]);
            }
            _ => panic!("not events"),
        }

        assert!(matches!(
            user(parse(&["ww", "core", "stop"])),
            UserCommand::Core(CoreCommand::Stop { timeout_secs: 60 })
        ));
        assert!(matches!(
            user(parse(&["ww", "core", "stop", "--timeout-secs", "5"])),
            UserCommand::Core(CoreCommand::Stop { timeout_secs: 5 })
        ));

        // No subcommand, an unknown one, or `core` alone is a usage error.
        for bad in [
            &["ww"][..],
            &["ww", "nonsense"],
            &["ww", "core"],
            &["ww", "call"],
        ] {
            assert!(Cli::try_parse_from(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test] // spec: ARCH-003, ARCH-090
    fn workspace_init_and_use_parse() {
        match user(parse(&["ww", "workspace", "init", r"C:\ws\Ünïcödé space"])) {
            UserCommand::Workspace(WorkspaceCommand::Init {
                path,
                name,
                default,
            }) => {
                assert_eq!(path, PathBuf::from(r"C:\ws\Ünïcödé space"));
                assert_eq!(name, None);
                assert!(!default);
            }
            other => panic!("not workspace init: {other:?}"),
        }
        match user(parse(&[
            "ww",
            "workspace",
            "init",
            "rel ws",
            "--name",
            "Ünïcödé space",
            "--default",
        ])) {
            UserCommand::Workspace(WorkspaceCommand::Init {
                path,
                name,
                default,
            }) => {
                assert_eq!(path, PathBuf::from("rel ws"));
                assert_eq!(name.as_deref(), Some("Ünïcödé space"));
                assert!(default);
            }
            other => panic!("not workspace init: {other:?}"),
        }
        match user(parse(&[
            "ww",
            "--no-start",
            "workspace",
            "use",
            r"D:\other",
        ])) {
            UserCommand::Workspace(WorkspaceCommand::Use { path }) => {
                assert_eq!(path, PathBuf::from(r"D:\other"));
            }
            other => panic!("not workspace use: {other:?}"),
        }
        for bad in [
            &["ww", "workspace"][..],
            &["ww", "workspace", "init"],
            &["ww", "workspace", "use"],
            &["ww", "workspace", "use", "a", "b"],
            &["ww", "workspace", "drop", "a"],
        ] {
            assert!(Cli::try_parse_from(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test] // spec: ARCH-016, ARCH-073
    fn notify_parses() {
        match user(parse(&[
            "ww",
            "notify",
            r"C:\ws\Ünïcödé space\a.png",
            "rel/b.png",
        ])) {
            UserCommand::Notify { paths } => assert_eq!(
                paths,
                [
                    PathBuf::from(r"C:\ws\Ünïcödé space\a.png"),
                    PathBuf::from("rel/b.png")
                ]
            ),
            other => panic!("not notify: {other:?}"),
        }
        assert!(matches!(
            user(parse(&["ww", "--no-start", "notify", "a.png"])),
            UserCommand::Notify { .. }
        ));
        assert!(
            Cli::try_parse_from(["ww", "notify"]).is_err(),
            "notify needs a path"
        );
    }

    #[test] // spec: CORE-004, CORE-040
    fn doctor_parses() {
        assert!(matches!(
            user(parse(&["ww", "doctor"])),
            UserCommand::Doctor
        ));
        let cli = parse(&["ww", "--non-interactive", "doctor"]);
        assert!(cli.global.non_interactive);
        assert!(matches!(user(cli), UserCommand::Doctor));
        assert!(
            Cli::try_parse_from(["ww", "doctor", "extra"]).is_err(),
            "doctor takes no argument"
        );
    }

    #[test] // spec: ARCH-016
    fn events_needs_follow_and_a_topic() {
        assert!(Cli::try_parse_from(["ww", "events", "--topic", "test:a"]).is_err());
        assert!(Cli::try_parse_from(["ww", "events", "--follow"]).is_err());
        assert!(Cli::try_parse_from(["ww", "events"]).is_err());
        assert!(matches!(
            user(parse(&["ww", "events", "--follow", "--topic", "test:a"])),
            UserCommand::Events { follow: true, .. }
        ));
    }

    #[test] // spec: ARCH-016, ARCH-018, ARCH-030
    fn global_flags_work_before_and_after_the_subcommand() {
        let ws = Path::new(r"C:\ws\Ünïcödé space");
        for args in [
            &[
                "ww",
                "--workspace",
                r"C:\ws\Ünïcödé space",
                "--non-interactive",
                "--no-start",
                "status",
            ][..],
            &[
                "ww",
                "status",
                "--workspace",
                r"C:\ws\Ünïcödé space",
                "--non-interactive",
                "--no-start",
            ],
            &[
                "ww",
                "--no-start",
                "call",
                "test.echo",
                "--non-interactive",
                "--workspace",
                r"C:\ws\Ünïcödé space",
            ],
            &[
                "ww",
                "core",
                "stop",
                "--no-start",
                "--non-interactive",
                "--workspace",
                r"C:\ws\Ünïcödé space",
            ],
            &[
                "ww",
                "--non-interactive",
                "events",
                "--follow",
                "--topic",
                "test:a",
                "--no-start",
                "--workspace",
                r"C:\ws\Ünïcödé space",
            ],
        ] {
            let cli = parse(args);
            assert_eq!(cli.global.workspace.as_deref(), Some(ws), "{args:?}");
            assert!(cli.global.non_interactive, "{args:?}");
            assert!(cli.global.no_start, "{args:?}");
            assert!(matches!(cli.command, Command::User(_)), "{args:?}");
        }

        let plain = parse(&["ww", "status"]);
        assert_eq!(plain.global.workspace, None);
        assert!(!plain.global.non_interactive);
        assert!(!plain.global.no_start);
    }

    #[test] // spec: ARCH-016
    fn dev_commands_still_parse() {
        let cli = parse(&["ww", "dev", "merge", "3"]);
        assert!(matches!(
            cli.command,
            Command::Dev(crate::dev::DevCommand::Merge { pr: 3 })
        ));
        assert!(matches!(
            parse(&["ww", "dev", "spec-tests"]).command,
            Command::Dev(crate::dev::DevCommand::SpecTests)
        ));
    }
}
