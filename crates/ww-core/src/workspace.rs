//! Which workspace a session works in (ARCH-030 on the core side, ARCH-090, CORE-007):
//! infrastructure, not a CORE-014 module.
//!
//! A hello names a workspace by its id, the 16-hex hash of its canonical path
//! (`Env::workspace_hash`), or says `"none"`. The core serves only the default workspace of
//! `suite.toml` (owner decision 2): another registered workspace is `workspace.unsupported`,
//! with the way to make it the default; anything else is `workspace.unknown`.
//!
//! Before a workspace is used, the harness guard sees its path (`check_workspace`), so a core
//! on a test channel never opens a workspace registered in a real `suite.toml` (QA-040).

use std::path::{Path, PathBuf};

use ww_platform::{Env, check_workspace};
use ww_protocol::codes::{WORKSPACE_UNKNOWN, WORKSPACE_UNSUPPORTED};
use ww_protocol::{ErrorBody, Fix, NO_WORKSPACE, UiAction};
use ww_schema::SuiteToml;

/// The UI action that makes another workspace the default (plan 2b's `ww workspace use`).
pub const SET_DEFAULT_ACTION: &str = "workspace.set_default";

/// A resolved workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    /// 16 lowercase hex: the hash of the canonical path.
    pub id: String,
    /// The canonical path, without a `\\?\` prefix.
    pub path: PathBuf,
}

/// Resolves the `workspace` of a hello: `"none"` is `None`; the id of the default workspace
/// is that workspace.
///
/// # Errors
/// - `workspace.unsupported`: the id of another registered workspace; the message names the
///   default workspace and how to switch;
/// - `workspace.unknown`: any other id, including a registered workspace whose folder is gone.
///
/// # Panics
/// When the harness guard is armed and the workspace is registered in a real `suite.toml`
/// (QA-040).
pub fn resolve_workspace(
    env: &Env,
    requested: &str,
    trace_id: &str,
) -> Result<Option<WorkspaceRef>, ErrorBody> {
    if requested == NO_WORKSPACE {
        return Ok(None);
    }
    let unknown = |why: &str| {
        ErrorBody::new(
            &WORKSPACE_UNKNOWN,
            format!("The workspace {requested} is not one this core knows: {why}."),
            trace_id,
        )
    };
    let file = env.suite_toml();
    let suite = match std::fs::read_to_string(&file) {
        Ok(text) => match SuiteToml::parse(&text) {
            Ok(suite) => suite,
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "suite.toml is not valid");
                return Err(unknown(
                    "the workspace registry (suite.toml) cannot be read",
                ));
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(unknown("no workspace is registered"));
        }
        Err(e) => {
            tracing::warn!(file = %file.display(), error = %e, "cannot read suite.toml");
            return Err(unknown(
                "the workspace registry (suite.toml) cannot be read",
            ));
        }
    };

    let default = suite.default_workspace.as_deref().filter(|p| !p.is_empty());
    if let Some(default) = default
        && let Some((id, canonical)) = id_of(Path::new(default))
        && id == requested
    {
        // The guard sees the path as registered and as resolved (it compares text).
        check_workspace(Path::new(default));
        check_workspace(&canonical);
        return Ok(Some(WorkspaceRef {
            id,
            path: canonical,
        }));
    }

    let other = suite
        .workspaces
        .iter()
        .map(|w| w.path.as_str())
        .filter(|p| !p.is_empty() && Some(*p) != default)
        .find(|p| id_of(Path::new(p)).is_some_and(|(id, _)| id == requested));
    match other {
        Some(path) => Err(unsupported(default, path, trace_id)),
        None => Err(unknown(
            "it is not a registered workspace, or its folder no longer exists",
        )),
    }
}

/// `workspace.unsupported` for the registered workspace at `requested` (owner decision 2).
fn unsupported(default: Option<&str>, requested: &str, trace_id: &str) -> ErrorBody {
    let serves = match default {
        Some(default) => format!("This core serves only the default workspace, {default}."),
        None => "This core serves only the default workspace, and none is set.".to_string(),
    };
    ErrorBody {
        fix: Some(Fix {
            ui_action: UiAction {
                id: SET_DEFAULT_ACTION.to_string(),
                label: format!("Use {requested} as the default workspace"),
            },
            agent_hint: None,
        }),
        ..ErrorBody::new(
            &WORKSPACE_UNSUPPORTED,
            format!(
                "{serves} To use {requested} instead, make it the default with `ww workspace use {requested}`."
            ),
            trace_id,
        )
    }
}

/// The id of the workspace at `path` and its canonical path without `\\?\`, or `None` if its
/// folder does not exist.
fn id_of(path: &Path) -> Option<(String, PathBuf)> {
    let canonical = std::fs::canonicalize(path).ok()?;
    if !canonical.is_dir() {
        return None;
    }
    let id = Env::workspace_hash(&canonical);
    let text = canonical.to_string_lossy();
    let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        canonical.clone()
    };
    Some((id, plain))
}

#[cfg(test)]
mod tests {
    use ww_protocol::{ErrorCategory, RetryPolicy};
    use ww_testkit::TestEnv;

    use super::*;

    fn id(path: &Path) -> String {
        Env::workspace_hash(&std::fs::canonicalize(path).expect("canonicalize"))
    }

    #[test] // spec: ARCH-030
    fn none_means_no_workspace() {
        let env = TestEnv::new();
        // No suite.toml at all.
        assert_eq!(resolve_workspace(&env.env, "none", "t"), Ok(None));
        let ws = env.make_workspace("alpha");
        env.register_workspace(&ws, true);
        assert_eq!(resolve_workspace(&env.env, "none", "t"), Ok(None));
    }

    #[test] // spec: ARCH-030, ARCH-090
    fn default_workspace_id_resolves() {
        let env = TestEnv::with_label("Ünïcödé space");
        let other = env.make_workspace("other");
        let ws = env.make_workspace("Ünïcödé space");
        env.register_workspace(&other, false);
        env.register_workspace(&ws, true);
        let found = resolve_workspace(&env.env, &id(&ws), "t")
            .expect("the default workspace resolves")
            .expect("a workspace");
        assert_eq!(found.id, id(&ws));
        assert_eq!(found.id.len(), 16);
        assert!(
            !found.path.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            found.path.display()
        );
        assert_eq!(
            std::fs::canonicalize(&found.path).unwrap(),
            std::fs::canonicalize(&ws).unwrap()
        );
    }

    #[test] // spec: ARCH-030
    fn unregistered_id_is_unknown() {
        let env = TestEnv::new();
        let ws = env.make_workspace("alpha");
        let check = |requested: &str| {
            let err = resolve_workspace(&env.env, requested, "t_9").unwrap_err();
            assert_eq!(err.code, "workspace.unknown", "{requested}");
            assert_eq!(err.category, ErrorCategory::UserFixable);
            assert_eq!(err.retry, RetryPolicy::AfterResolve);
            assert_eq!(err.trace_id, "t_9");
        };
        // No suite.toml yet.
        check(&id(&ws));
        // Registered: a workspace that exists but is not listed, and ids that match nothing.
        let listed = env.make_workspace("listed");
        env.register_workspace(&listed, true);
        check(&id(&ws));
        check("0123456789abcdef");
        check("not-an-id");
        check("");
    }

    #[test] // spec: ARCH-090
    fn second_workspace_is_refused_naming_the_default() {
        let env = TestEnv::with_label("Ünïcödé space");
        let default = env.make_workspace("default");
        let second = env.make_workspace("second");
        env.register_workspace(&default, true);
        env.register_workspace(&second, false);
        let err = resolve_workspace(&env.env, &id(&second), "t").unwrap_err();
        assert_eq!(err.code, "workspace.unsupported");
        assert_eq!(err.category, ErrorCategory::UserFixable);
        let (default, second) = (default.display(), second.display());
        assert!(
            err.message.contains(&default.to_string()),
            "{}",
            err.message
        );
        assert!(err.message.contains("ww workspace use"), "{}", err.message);
        assert!(
            err.message
                .contains(&format!("`ww workspace use {second}`")),
            "{}",
            err.message
        );
        let fix = err.fix.expect("a fix");
        assert_eq!(fix.ui_action.id, "workspace.set_default");
        assert_eq!(
            fix.ui_action.label,
            format!("Use {second} as the default workspace")
        );
        assert!(fix.agent_hint.is_none());
    }

    #[test] // spec: ARCH-090
    fn second_workspace_without_a_default_is_refused() {
        let env = TestEnv::new();
        let second = env.make_workspace("second");
        env.register_workspace(&second, false);
        let err = resolve_workspace(&env.env, &id(&second), "t").unwrap_err();
        assert_eq!(err.code, "workspace.unsupported");
        assert!(err.message.contains("ww workspace use"), "{}", err.message);
    }

    #[test] // spec: ARCH-030
    fn vanished_workspace_is_unknown() {
        let env = TestEnv::new();
        // Folders removed by dropping their `TempDir` (ww-core code itself never deletes).
        let default = tempfile::tempdir_in(env.root()).unwrap();
        let second = tempfile::tempdir_in(env.root()).unwrap();
        env.register_workspace(default.path(), true);
        env.register_workspace(second.path(), false);
        let (default_id, second_id) = (id(default.path()), id(second.path()));
        assert!(resolve_workspace(&env.env, &default_id, "t").is_ok());
        drop(default);
        drop(second);
        for gone in [default_id, second_id] {
            let err = resolve_workspace(&env.env, &gone, "t").unwrap_err();
            assert_eq!(err.code, "workspace.unknown");
        }
    }
}
