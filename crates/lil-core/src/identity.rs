//! Caller identity (ARCH-045–048, ARCH-050): what a connection may claim to be, checked
//! against the client process (infrastructure, not a CORE-014 module).
//!
//! The declared `caller_kind` is a claim. The core keeps it only when the client process backs
//! it up:
//! - `ui:<app>`: the image is that app's executable ([`APP_EXES`]) in a trusted app folder;
//!   on a test channel also `ui:test` from [`TEST_APP_EXE`] (QA-066), never elsewhere;
//! - `mcp`: the image is [`MCP_EXE`] in a trusted MCP folder and its direct parent process is
//!   `claude.exe`, with no shell in between ([`claude_is_direct_parent`]; ARCH-047 as Appendix
//!   F clarifies it);
//! - `assistant`: the client presented an engine token that validates. A token that does not
//!   validate is refused, never downgraded (ARCH-048, ARCH-050).
//!
//! Every other claim resolves to `cli`. Folder paths compare by
//! `lil_platform::paths::folder_key`: canonical when they exist, without a `\\?\` prefix,
//! case-insensitive.

use std::path::{Path, PathBuf};

use lil_platform::paths::folder_key;
use lil_platform::process::{ProcessInfo, ProcessInspector};
use lil_platform::{Channel, Env};
use lil_protocol::codes::{CALLER_ENGINE_TOKEN_INVALID, PARAMS_INVALID};
use lil_protocol::{CallerKind, ErrorBody};

/// The executables of the suite's apps: (file name, app). A `ui:<app>` claim needs that file.
pub const APP_EXES: &[(&str, &str)] = &[("lil-image.exe", "image")];

/// The test app (file name, app): `ui:test`, accepted only by a core on a test channel
/// ([`TrustedDirs::test_app`]).
pub const TEST_APP_EXE: (&str, &str) = ("lil-test-app.exe", "test");

/// The MCP server's executable. An `mcp` claim needs it.
pub const MCP_EXE: &str = "lil-mcp.exe";

/// The Claude Code executable, required as the MCP server's direct parent.
const CLAUDE_EXE: &str = "claude.exe";

/// The folders whose executables are trusted (ARCH-046, ARCH-047).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedDirs {
    pub app_dirs: Vec<PathBuf>,
    pub mcp_dirs: Vec<PathBuf>,
    /// [`TEST_APP_EXE`] in an app folder is `ui:test`: set by [`TrustedDirs::for_env`] on test
    /// channels only.
    pub test_app: bool,
}

/// Whether a core on `channel` accepts the test app: test channels only.
pub fn trusts_test_app(channel: &Channel) -> bool {
    matches!(channel, Channel::Test(_))
}

impl TrustedDirs {
    /// Stable (installed): apps from the install folder; the MCP server from the install
    /// folder or `local_dir\bin`. Dev and test: both from the folder of the running core. The
    /// test app is trusted on test channels only.
    pub fn for_env(env: &Env, own_exe_dir: &Path) -> TrustedDirs {
        // Only the stable channel has an install folder (`Env::install_dir`).
        let dirs = match env.install_dir() {
            Some(install_dir) => TrustedDirs::installed(&install_dir, &env.local_dir()),
            None => TrustedDirs::own_dir(own_exe_dir),
        };
        TrustedDirs {
            test_app: trusts_test_app(env.channel()),
            ..dirs
        }
    }

    /// The stable layout: `app_dirs = [install_dir]`, `mcp_dirs = [install_dir, local_dir\bin]`.
    pub fn installed(install_dir: &Path, local_dir: &Path) -> TrustedDirs {
        TrustedDirs {
            app_dirs: vec![install_dir.to_path_buf()],
            mcp_dirs: vec![install_dir.to_path_buf(), local_dir.join("bin")],
            test_app: false,
        }
    }

    /// The dev and test layout: both lists are `[own_exe_dir]`.
    pub fn own_dir(own_exe_dir: &Path) -> TrustedDirs {
        TrustedDirs {
            app_dirs: vec![own_exe_dir.to_path_buf()],
            mcp_dirs: vec![own_exe_dir.to_path_buf()],
            test_app: false,
        }
    }
}

/// What a valid engine token stands for (ARCH-030, ARCH-048).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSession {
    /// The workspace id the engine session works in.
    pub workspace: String,
    pub profile_id: Option<String>,
}

/// Checks engine tokens. Plan 3 issues them; until then [`NoEngineTokens`] refuses all.
pub trait EngineTokens: Send + Sync {
    /// The session `token` stands for, if it is valid for the process `client_pid`.
    fn validate(&self, token: &str, client_pid: u32) -> Option<EngineSession>;
}

/// Plan 2: no engine runs, so every token is invalid.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEngineTokens;

impl EngineTokens for NoEngineTokens {
    fn validate(&self, _token: &str, _client_pid: u32) -> Option<EngineSession> {
        None
    }
}

/// The caller kind the core grants, and the engine session behind an `assistant` caller.
/// `inspector` reads the client's parent for an `mcp` claim.
///
/// # Errors
/// - `params.invalid`: an engine token with a declared kind other than `assistant`;
/// - `caller.engine_token_invalid`: an engine token that does not validate.
pub fn resolve_caller(
    declared: &CallerKind,
    engine_token: Option<&str>,
    client: &ProcessInfo,
    inspector: &dyn ProcessInspector,
    trusted: &TrustedDirs,
    tokens: &dyn EngineTokens,
    trace_id: &str,
) -> Result<(CallerKind, Option<EngineSession>), ErrorBody> {
    if let Some(token) = engine_token {
        if *declared != CallerKind::Assistant {
            return Err(ErrorBody::new(
                &PARAMS_INVALID,
                format!(
                    "An engine token is only for caller_kind \"assistant\", not \"{declared}\"."
                ),
                trace_id,
            ));
        }
        // Never downgraded: a caller that presents a token is an engine or nothing.
        return match tokens.validate(token, client.pid) {
            Some(session) => Ok((CallerKind::Assistant, Some(session))),
            None => Err(ErrorBody::new(
                &CALLER_ENGINE_TOKEN_INVALID,
                "The engine token is not valid for this process.",
                trace_id,
            )),
        };
    }
    let verified = match declared {
        CallerKind::Ui(app) => {
            let test_app = trusted.test_app.then_some(TEST_APP_EXE);
            APP_EXES
                .iter()
                .copied()
                .chain(test_app)
                .any(|(exe, a)| a == app && runs(client, exe, &trusted.app_dirs))
        }
        CallerKind::Mcp => {
            runs(client, MCP_EXE, &trusted.mcp_dirs) && claude_is_direct_parent(inspector, client)
        }
        CallerKind::Assistant | CallerKind::Cli => false,
    };
    let kind = if verified {
        declared.clone()
    } else {
        CallerKind::Cli
    };
    Ok((kind, None))
}

/// Whether `client`'s image is a file named `exe` directly inside one of `dirs`.
fn runs(client: &ProcessInfo, exe: &str, dirs: &[PathBuf]) -> bool {
    let (Some(name), Some(dir)) = (client.image.file_name(), client.image.parent()) else {
        return false;
    };
    let dir = folder_key(dir);
    !dir.is_empty()
        && name.to_string_lossy().to_lowercase() == exe.to_lowercase()
        && dirs.iter().any(|d| folder_key(d) == dir)
}

/// Whether the MCP server `client` was started by Claude Code itself (ARCH-047, Appendix F):
/// its direct parent's image is `claude.exe`, and that parent started before `client`, so a
/// process that took the pid of an exited parent does not count (review M1). A grandparent
/// never counts, so a shell between them makes this `false`, as does a parent that is gone or
/// cannot be inspected, or a start time that cannot be read.
///
/// The parent is recognised by its file name only: a program renamed `claude.exe` passes.
/// That is the documented limit (Appendix F), and the reason `mcp_confirmation` stays
/// `"core"` until there is a stronger parent check.
pub fn claude_is_direct_parent(inspector: &dyn ProcessInspector, client: &ProcessInfo) -> bool {
    let Some(pid) = client.parent_pid else {
        return false;
    };
    let named_claude = inspector.inspect(pid).is_ok_and(|parent| {
        parent
            .image
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(CLAUDE_EXE))
    });
    let started_first = match (inspector.started(pid), inspector.started(client.pid)) {
        (Ok(parent), Ok(child)) => parent < child,
        _ => false,
    };
    named_claude && started_first
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;

    use lil_protocol::ErrorCategory;
    use lil_testkit::TestEnv;

    use super::*;

    const TRUSTED: &str = r"C:\Program Files\Ünïcödé space\bin";

    fn trusted() -> TrustedDirs {
        TrustedDirs {
            app_dirs: vec![PathBuf::from(TRUSTED)],
            mcp_dirs: vec![PathBuf::from(TRUSTED)],
            test_app: false,
        }
    }

    fn process(image: &str, parent_pid: Option<u32>) -> ProcessInfo {
        ProcessInfo {
            pid: 4242,
            image: PathBuf::from(image),
            user_sid: "S-1-5-21-1000-2000-3000-1001".into(),
            parent_pid,
        }
    }

    /// A process table for the parent check, with each process's start time.
    #[derive(Debug)]
    struct Table(HashMap<u32, ProcessInfo>, HashMap<u32, u64>);

    impl ProcessInspector for Table {
        fn inspect(&self, pid: u32) -> io::Result<ProcessInfo> {
            self.0
                .get(&pid)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gone"))
        }

        fn started(&self, pid: u32) -> io::Result<u64> {
            self.1
                .get(&pid)
                .copied()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no start time"))
        }
    }

    /// `claude.exe` in the process table.
    const CLAUDE_PID: u32 = 10;

    /// Claude Code (10) and, under it, the shells an agent's shell tool could run: `cmd.exe`
    /// (11), `bash.exe` (12) and `pwsh.exe` (13), each started before the client (4242).
    fn table() -> Table {
        let row = |pid, image: &str, parent| ProcessInfo {
            pid,
            ..process(image, parent)
        };
        Table(
            HashMap::from([
                (CLAUDE_PID, row(CLAUDE_PID, r"C:\tools\Claude.exe", None)),
                (
                    11,
                    row(11, r"C:\Windows\System32\cmd.exe", Some(CLAUDE_PID)),
                ),
                (
                    12,
                    row(12, r"C:\Program Files\Git\bin\bash.exe", Some(CLAUDE_PID)),
                ),
                (
                    13,
                    row(
                        13,
                        r"C:\Program Files\PowerShell\7\pwsh.exe",
                        Some(CLAUDE_PID),
                    ),
                ),
            ]),
            HashMap::from([
                (CLAUDE_PID, 100),
                (11, 200),
                (12, 200),
                (13, 200),
                (4242, 1000),
            ]),
        )
    }

    fn resolve(
        declared: &str,
        token: Option<&str>,
        client: &ProcessInfo,
    ) -> Result<(CallerKind, Option<EngineSession>), ErrorBody> {
        resolve_caller(
            &declared.parse().unwrap(),
            token,
            client,
            &table(),
            &trusted(),
            &NoEngineTokens,
            "t_1",
        )
    }

    /// The kind `image` gets for `declared`, started by Claude Code.
    fn kind(declared: &str, image: &str) -> CallerKind {
        resolve(declared, None, &process(image, Some(CLAUDE_PID)))
            .unwrap()
            .0
    }

    #[test] // spec: ARCH-045, ARCH-046
    fn declared_ui_from_app_exe_in_trusted_dir_is_ui() {
        let image = format!(r"{TRUSTED}\lil-image.exe");
        assert_eq!(kind("ui:image", &image), CallerKind::Ui("image".into()));
    }

    #[test] // spec: ARCH-045, ARCH-046
    fn declared_ui_from_elsewhere_is_cli() {
        assert_eq!(kind("ui:image", r"C:\Temp\lil-image.exe"), CallerKind::Cli);
        // A subfolder of a trusted folder is not trusted.
        let nested = format!(r"{TRUSTED}\sub\lil-image.exe");
        assert_eq!(kind("ui:image", &nested), CallerKind::Cli);
    }

    #[test] // spec: ARCH-045, ARCH-046
    fn declared_ui_with_wrong_exe_is_cli() {
        // Another exe in the trusted folder, or an app the exe is not.
        assert_eq!(
            kind("ui:image", &format!(r"{TRUSTED}\lil.exe")),
            CallerKind::Cli
        );
        assert_eq!(
            kind("ui:sketch", &format!(r"{TRUSTED}\lil-image.exe")),
            CallerKind::Cli
        );
        assert_eq!(
            kind("ui:image", &format!(r"{TRUSTED}\lil-mcp.exe")),
            CallerKind::Cli
        );
        assert_eq!(kind("ui:image", ""), CallerKind::Cli);
    }

    #[test] // spec: ARCH-047, ARCH-052
    fn mcp_without_claude_as_direct_parent_is_cli() {
        let image = format!(r"{TRUSTED}\lil-mcp.exe");
        let under = |parent| resolve("mcp", None, &process(&image, parent)).unwrap().0;
        assert_eq!(under(Some(CLAUDE_PID)), CallerKind::Mcp);
        // A shell between Claude Code and the server: cmd.exe, bash.exe, pwsh.exe.
        for shell in [11, 12, 13] {
            assert_eq!(under(Some(shell)), CallerKind::Cli, "shell {shell}");
        }
        // No parent, or one that is gone.
        assert_eq!(under(None), CallerKind::Cli);
        assert_eq!(under(Some(99)), CallerKind::Cli);
    }

    #[test] // spec: ARCH-047
    fn mcp_from_untrusted_dir_is_cli() {
        assert_eq!(kind("mcp", r"C:\Temp\lil-mcp.exe"), CallerKind::Cli);
        assert_eq!(
            kind("mcp", &format!(r"{TRUSTED}\lil-image.exe")),
            CallerKind::Cli
        );
    }

    #[test] // spec: ARCH-048, ARCH-050
    fn any_engine_token_is_refused_in_plan_2() {
        // Even from a trusted image: a token that does not validate is refused, never
        // downgraded to `cli`.
        for image in [format!(r"{TRUSTED}\lil-mcp.exe"), r"C:\Temp\x.exe".into()] {
            let err = resolve("assistant", Some("forged"), &process(&image, None)).unwrap_err();
            assert_eq!(err.code, "caller.engine_token_invalid");
            assert_eq!(err.category, ErrorCategory::Declined);
            assert_eq!(err.trace_id, "t_1");
            assert!(!err.message.contains("forged"), "{}", err.message);
        }
        assert_eq!(NoEngineTokens.validate("anything", 1), None);
    }

    #[test] // spec: ARCH-048
    fn valid_engine_token_is_assistant() {
        struct OneToken;
        impl EngineTokens for OneToken {
            fn validate(&self, token: &str, client_pid: u32) -> Option<EngineSession> {
                (token == "good" && client_pid == 4242).then(|| EngineSession {
                    workspace: "0123456789abcdef".into(),
                    profile_id: Some("p1".into()),
                })
            }
        }
        let client = process(r"C:\Temp\engine.exe", None);
        let (kind, session) = resolve_caller(
            &CallerKind::Assistant,
            Some("good"),
            &client,
            &table(),
            &trusted(),
            &OneToken,
            "t",
        )
        .unwrap();
        assert_eq!(kind, CallerKind::Assistant);
        assert_eq!(session.unwrap().workspace, "0123456789abcdef");
        let other_pid = ProcessInfo { pid: 1, ..client };
        let err = resolve_caller(
            &CallerKind::Assistant,
            Some("good"),
            &other_pid,
            &table(),
            &trusted(),
            &OneToken,
            "t",
        )
        .unwrap_err();
        assert_eq!(err.code, "caller.engine_token_invalid");
    }

    #[test] // spec: ARCH-048
    fn token_with_non_assistant_kind_is_invalid_params() {
        for declared in ["cli", "mcp", "ui:image"] {
            let image = format!(r"{TRUSTED}\lil-image.exe");
            let err = resolve(declared, Some("t"), &process(&image, None)).unwrap_err();
            assert_eq!(err.code, "params.invalid", "{declared}");
        }
    }

    #[test] // spec: ARCH-048
    fn assistant_without_token_is_cli() {
        let image = format!(r"{TRUSTED}\lil-mcp.exe");
        assert_eq!(
            resolve("assistant", None, &process(&image, None)).unwrap(),
            (CallerKind::Cli, None)
        );
        assert_eq!(kind("cli", &image), CallerKind::Cli);
    }

    #[test] // spec: ARCH-046
    fn dir_comparison_ignores_case_and_verbatim_prefix() {
        let upper = format!(r"\\?\{}\LIL-IMAGE.EXE", TRUSTED.to_uppercase());
        assert_eq!(kind("ui:image", &upper), CallerKind::Ui("image".into()));
        let slashes = format!("{}/lil-mcp.exe", TRUSTED.to_lowercase().replace('\\', "/"));
        assert_eq!(kind("mcp", &slashes), CallerKind::Mcp);
        // A trusted folder written with a trailing separator or a verbatim prefix.
        let dirs = TrustedDirs {
            app_dirs: vec![PathBuf::from(format!(r"\\?\{TRUSTED}\"))],
            mcp_dirs: Vec::new(),
            test_app: false,
        };
        let client = process(&format!(r"{TRUSTED}\lil-image.exe"), None);
        let ui: CallerKind = "ui:image".parse().unwrap();
        let resolved = resolve_caller(&ui, None, &client, &table(), &dirs, &NoEngineTokens, "t");
        assert_eq!(resolved.unwrap().0, CallerKind::Ui("image".into()));
        let mcp = resolve_caller(
            &CallerKind::Mcp,
            None,
            &client,
            &table(),
            &dirs,
            &NoEngineTokens,
            "t",
        );
        assert_eq!(mcp.unwrap().0, CallerKind::Cli);
    }

    #[test] // spec: ARCH-046, ARCH-047
    fn stable_trusts_install_dir_and_bin_for_mcp() {
        let install = PathBuf::from(r"C:\L\LILITHSuite\current");
        let local = PathBuf::from(r"C:\L\LILITH");
        let dirs = TrustedDirs::installed(&install, &local);
        assert_eq!(dirs.app_dirs, vec![install.clone()]);
        assert_eq!(dirs.mcp_dirs, vec![install, local.join("bin")]);
    }

    #[test] // spec: ARCH-046, ARCH-047
    fn dev_and_test_trust_own_dir() {
        let own = PathBuf::from(r"C:\build\Ünïcödé space\debug");
        let env = TestEnv::new();
        let dirs = TrustedDirs::for_env(&env.env, &own);
        assert_eq!(dirs.app_dirs, vec![own.clone()]);
        assert_eq!(dirs.mcp_dirs, vec![own.clone()]);
        // The dev channel has no install folder either, so it takes the same branch (where
        // only a test channel trusts the test app).
        assert_eq!(
            TrustedDirs {
                test_app: true,
                ..TrustedDirs::own_dir(&own)
            },
            dirs
        );
    }

    /// `lil-test-app.exe` in a trusted app folder is `ui:test`, only where the test app is
    /// trusted, which is a test channel's core only.
    #[test] // spec: ARCH-045, ARCH-046, QA-066
    fn test_app_is_ui_test_only_on_test_channels() {
        let ui_test: CallerKind = "ui:test".parse().unwrap();
        let ui_image: CallerKind = "ui:image".parse().unwrap();
        let app = process(&format!(r"{TRUSTED}\lil-test-app.exe"), None);
        let with = TrustedDirs {
            test_app: true,
            ..trusted()
        };
        let kind = |dirs: &TrustedDirs, declared: &CallerKind, client: &ProcessInfo| {
            resolve_caller(declared, None, client, &table(), dirs, &NoEngineTokens, "t")
                .unwrap()
                .0
        };
        assert_eq!(kind(&with, &ui_test, &app), ui_test);
        assert_eq!(kind(&trusted(), &ui_test, &app), CallerKind::Cli);
        // Only that executable, only as `ui:test`, only from a trusted folder.
        assert_eq!(kind(&with, &ui_image, &app), CallerKind::Cli);
        let image_exe = process(&format!(r"{TRUSTED}\lil-image.exe"), None);
        assert_eq!(kind(&with, &ui_test, &image_exe), CallerKind::Cli);
        let elsewhere = process(r"C:\Temp\lil-test-app.exe", None);
        assert_eq!(kind(&with, &ui_test, &elsewhere), CallerKind::Cli);

        assert!(trusts_test_app(&Channel::Test("ab12cd34".into())));
        assert!(!trusts_test_app(&Channel::Dev));
        assert!(!trusts_test_app(&Channel::Stable));
        let own = PathBuf::from(r"C:\build\debug");
        assert!(TrustedDirs::for_env(&TestEnv::new().env, &own).test_app);
        assert!(!TrustedDirs::own_dir(&own).test_app);
        assert!(!TrustedDirs::installed(&own, &own).test_app);
    }

    #[test] // spec: ARCH-047
    fn only_a_direct_claude_parent_counts() {
        let table = table();
        let mcp = |parent| process(r"C:\x\lil-mcp.exe", parent);
        assert!(claude_is_direct_parent(&table, &mcp(Some(CLAUDE_PID))));
        // Claude Code as the grandparent, through any shell, does not count.
        for shell in [11, 12, 13] {
            assert!(
                !claude_is_direct_parent(&table, &mcp(Some(shell))),
                "{shell}"
            );
        }
        assert!(!claude_is_direct_parent(&table, &mcp(Some(99))));
        assert!(!claude_is_direct_parent(&table, &mcp(None)));
    }

    /// Review M1: a `claude.exe` whose pid the real parent left behind is not the parent. The
    /// parent must have started before the client, and both start times must be known.
    #[test] // spec: ARCH-047
    fn a_reused_parent_pid_does_not_count() {
        let mcp = process(r"C:\x\lil-mcp.exe", Some(CLAUDE_PID));
        let mut later = table();
        later.1.insert(CLAUDE_PID, 5000);
        assert!(
            !claude_is_direct_parent(&later, &mcp),
            "started after its child"
        );
        let mut unknown = table();
        unknown.1.remove(&CLAUDE_PID);
        assert!(
            !claude_is_direct_parent(&unknown, &mcp),
            "parent start unknown"
        );
        let mut client_unknown = table();
        client_unknown.1.remove(&4242);
        assert!(
            !claude_is_direct_parent(&client_unknown, &mcp),
            "client start unknown"
        );
        assert!(claude_is_direct_parent(&table(), &mcp));
    }
}
