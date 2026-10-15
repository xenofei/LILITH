//! Caller identity (ARCH-045–048, ARCH-050): what a connection may claim to be, checked
//! against the client process (infrastructure, not a CORE-014 module).
//!
//! The declared `caller_kind` is a claim. The core keeps it only when the client process backs
//! it up:
//! - `ui:<app>`: the image is that app's executable ([`APP_EXES`]) in a trusted app folder;
//! - `mcp`: the image is [`MCP_EXE`] in a trusted MCP folder;
//! - `assistant`: the client presented an engine token that validates. A token that does not
//!   validate is refused, never downgraded (ARCH-048, ARCH-050).
//!
//! Every other claim resolves to `cli`. Folder paths compare case-insensitively after a
//! `\\?\` prefix is stripped.

use std::path::{Path, PathBuf};

use ww_platform::Env;
use ww_platform::process::{ProcessInfo, ProcessInspector};
use ww_protocol::codes::{CALLER_ENGINE_TOKEN_INVALID, PARAMS_INVALID};
use ww_protocol::{CallerKind, ErrorBody};

/// The executables of the suite's apps: (file name, app). A `ui:<app>` claim needs that file.
pub const APP_EXES: &[(&str, &str)] = &[("ww-image.exe", "image")];

/// The MCP server's executable. An `mcp` claim needs it.
pub const MCP_EXE: &str = "ww-mcp.exe";

/// The Claude Code executable, expected as the MCP server's parent or grandparent.
const CLAUDE_EXE: &str = "claude.exe";

/// The folders whose executables are trusted (ARCH-046, ARCH-047).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedDirs {
    pub app_dirs: Vec<PathBuf>,
    pub mcp_dirs: Vec<PathBuf>,
}

impl TrustedDirs {
    /// Stable (installed): apps from the install folder; the MCP server from the install
    /// folder or `local_dir\bin`. Dev and test: both from the folder of the running core.
    pub fn for_env(env: &Env, own_exe_dir: &Path) -> TrustedDirs {
        // Only the stable channel has an install folder (`Env::install_dir`).
        match env.install_dir() {
            Some(install_dir) => TrustedDirs::installed(&install_dir, &env.local_dir()),
            None => TrustedDirs::own_dir(own_exe_dir),
        }
    }

    /// The stable layout: `app_dirs = [install_dir]`, `mcp_dirs = [install_dir, local_dir\bin]`.
    pub fn installed(install_dir: &Path, local_dir: &Path) -> TrustedDirs {
        TrustedDirs {
            app_dirs: vec![install_dir.to_path_buf()],
            mcp_dirs: vec![install_dir.to_path_buf(), local_dir.join("bin")],
        }
    }

    /// The dev and test layout: both lists are `[own_exe_dir]`.
    pub fn own_dir(own_exe_dir: &Path) -> TrustedDirs {
        TrustedDirs {
            app_dirs: vec![own_exe_dir.to_path_buf()],
            mcp_dirs: vec![own_exe_dir.to_path_buf()],
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
///
/// # Errors
/// - `params.invalid`: an engine token with a declared kind other than `assistant`;
/// - `caller.engine_token_invalid`: an engine token that does not validate.
#[allow(
    clippy::result_large_err,
    reason = "the plan fixes this signature; ErrorBody is the wire error result, built once per call"
)]
pub fn resolve_caller(
    declared: &CallerKind,
    engine_token: Option<&str>,
    client: &ProcessInfo,
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
        CallerKind::Ui(app) => APP_EXES
            .iter()
            .any(|(exe, a)| a == app && runs(client, exe, &trusted.app_dirs)),
        CallerKind::Mcp => runs(client, MCP_EXE, &trusted.mcp_dirs),
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
    let dir = normalise(dir);
    !dir.is_empty()
        && name.to_string_lossy().to_lowercase() == exe.to_lowercase()
        && dirs.iter().any(|d| normalise(d) == dir)
}

/// A folder's spelling for comparison: no `\\?\` (`\\?\UNC\` becomes `\\`), `\` separators,
/// no trailing separator, lowercase.
fn normalise(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', r"\");
    let text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    };
    text.trim_end_matches('\\').to_lowercase()
}

/// Best effort (ARCH-047): whether the MCP server `client` runs under Claude Code, that is,
/// its parent or grandparent is `claude.exe`. `false` when that cannot be told.
pub fn claude_ancestor(inspector: &dyn ProcessInspector, client: &ProcessInfo) -> bool {
    let mut next = client.parent_pid;
    for _ in 0..2 {
        let Some(pid) = next else {
            return false;
        };
        let Ok(parent) = inspector.inspect(pid) else {
            return false;
        };
        let is_claude = parent
            .image
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(CLAUDE_EXE));
        if is_claude {
            return true;
        }
        next = parent.parent_pid;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;

    use ww_protocol::ErrorCategory;
    use ww_testkit::TestEnv;

    use super::*;

    const TRUSTED: &str = r"C:\Program Files\Ünïcödé space\bin";

    fn trusted() -> TrustedDirs {
        TrustedDirs {
            app_dirs: vec![PathBuf::from(TRUSTED)],
            mcp_dirs: vec![PathBuf::from(TRUSTED)],
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

    #[allow(
        clippy::result_large_err,
        reason = "returns what resolve_caller returns"
    )]
    fn resolve(
        declared: &str,
        token: Option<&str>,
        client: &ProcessInfo,
    ) -> Result<(CallerKind, Option<EngineSession>), ErrorBody> {
        resolve_caller(
            &declared.parse().unwrap(),
            token,
            client,
            &trusted(),
            &NoEngineTokens,
            "t_1",
        )
    }

    fn kind(declared: &str, image: &str) -> CallerKind {
        resolve(declared, None, &process(image, None)).unwrap().0
    }

    #[test] // spec: ARCH-045, ARCH-046
    fn declared_ui_from_app_exe_in_trusted_dir_is_ui() {
        let image = format!(r"{TRUSTED}\ww-image.exe");
        assert_eq!(kind("ui:image", &image), CallerKind::Ui("image".into()));
    }

    #[test] // spec: ARCH-045, ARCH-046
    fn declared_ui_from_elsewhere_is_cli() {
        assert_eq!(kind("ui:image", r"C:\Temp\ww-image.exe"), CallerKind::Cli);
        // A subfolder of a trusted folder is not trusted.
        let nested = format!(r"{TRUSTED}\sub\ww-image.exe");
        assert_eq!(kind("ui:image", &nested), CallerKind::Cli);
    }

    #[test] // spec: ARCH-045, ARCH-046
    fn declared_ui_with_wrong_exe_is_cli() {
        // Another exe in the trusted folder, or an app the exe is not.
        assert_eq!(
            kind("ui:image", &format!(r"{TRUSTED}\ww.exe")),
            CallerKind::Cli
        );
        assert_eq!(
            kind("ui:sketch", &format!(r"{TRUSTED}\ww-image.exe")),
            CallerKind::Cli
        );
        assert_eq!(
            kind("ui:image", &format!(r"{TRUSTED}\ww-mcp.exe")),
            CallerKind::Cli
        );
        assert_eq!(kind("ui:image", ""), CallerKind::Cli);
    }

    #[test] // spec: ARCH-047
    fn mcp_from_trusted_dir_is_mcp_without_claude_parent() {
        // No parent at all: still `mcp` (the Claude Code check is best effort).
        let image = format!(r"{TRUSTED}\ww-mcp.exe");
        assert_eq!(kind("mcp", &image), CallerKind::Mcp);
        let client = process(&image, Some(7));
        assert_eq!(resolve("mcp", None, &client).unwrap().0, CallerKind::Mcp);
    }

    #[test] // spec: ARCH-047
    fn mcp_from_untrusted_dir_is_cli() {
        assert_eq!(kind("mcp", r"C:\Temp\ww-mcp.exe"), CallerKind::Cli);
        assert_eq!(
            kind("mcp", &format!(r"{TRUSTED}\ww-image.exe")),
            CallerKind::Cli
        );
    }

    #[test] // spec: ARCH-048, ARCH-050
    fn any_engine_token_is_refused_in_plan_2() {
        // Even from a trusted image: a token that does not validate is refused, never
        // downgraded to `cli`.
        for image in [format!(r"{TRUSTED}\ww-mcp.exe"), r"C:\Temp\x.exe".into()] {
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
            let image = format!(r"{TRUSTED}\ww-image.exe");
            let err = resolve(declared, Some("t"), &process(&image, None)).unwrap_err();
            assert_eq!(err.code, "params.invalid", "{declared}");
        }
    }

    #[test] // spec: ARCH-048
    fn assistant_without_token_is_cli() {
        let image = format!(r"{TRUSTED}\ww-mcp.exe");
        assert_eq!(
            resolve("assistant", None, &process(&image, None)).unwrap(),
            (CallerKind::Cli, None)
        );
        assert_eq!(kind("cli", &image), CallerKind::Cli);
    }

    #[test] // spec: ARCH-046
    fn dir_comparison_ignores_case_and_verbatim_prefix() {
        let upper = format!(r"\\?\{}\WW-IMAGE.EXE", TRUSTED.to_uppercase());
        assert_eq!(kind("ui:image", &upper), CallerKind::Ui("image".into()));
        let slashes = format!("{}/ww-mcp.exe", TRUSTED.to_lowercase().replace('\\', "/"));
        assert_eq!(kind("mcp", &slashes), CallerKind::Mcp);
        // A trusted folder written with a trailing separator or a verbatim prefix.
        let dirs = TrustedDirs {
            app_dirs: vec![PathBuf::from(format!(r"\\?\{TRUSTED}\"))],
            mcp_dirs: Vec::new(),
        };
        let client = process(&format!(r"{TRUSTED}\ww-image.exe"), None);
        let ui: CallerKind = "ui:image".parse().unwrap();
        let resolved = resolve_caller(&ui, None, &client, &dirs, &NoEngineTokens, "t");
        assert_eq!(resolved.unwrap().0, CallerKind::Ui("image".into()));
        let mcp = resolve_caller(&CallerKind::Mcp, None, &client, &dirs, &NoEngineTokens, "t");
        assert_eq!(mcp.unwrap().0, CallerKind::Cli);
    }

    #[test] // spec: ARCH-046, ARCH-047
    fn stable_trusts_install_dir_and_bin_for_mcp() {
        let install = PathBuf::from(r"C:\L\WorldwrightSuite\current");
        let local = PathBuf::from(r"C:\L\Worldwright");
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
        // The dev channel has no install folder either, so it takes the same branch.
        assert_eq!(TrustedDirs::own_dir(&own), dirs);
    }

    /// A process table for [`claude_ancestor`].
    #[derive(Debug)]
    struct Table(HashMap<u32, ProcessInfo>);

    impl ProcessInspector for Table {
        fn inspect(&self, pid: u32) -> io::Result<ProcessInfo> {
            self.0
                .get(&pid)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gone"))
        }
    }

    #[test] // spec: ARCH-047
    fn claude_parent_or_grandparent_is_found() {
        let row = |pid, image: &str, parent| ProcessInfo {
            pid,
            ..process(image, parent)
        };
        let table = Table(HashMap::from([
            (10, row(10, r"C:\tools\Claude.exe", None)),
            (11, row(11, r"C:\Windows\cmd.exe", Some(10))),
            (12, row(12, r"C:\Windows\cmd.exe", Some(11))),
        ]));
        let mcp = |parent| process(r"C:\x\ww-mcp.exe", parent);
        assert!(claude_ancestor(&table, &mcp(Some(10))));
        assert!(claude_ancestor(&table, &mcp(Some(11))));
        assert!(!claude_ancestor(&table, &mcp(Some(12))));
        assert!(!claude_ancestor(&table, &mcp(Some(99))));
        assert!(!claude_ancestor(&table, &mcp(None)));
    }
}
