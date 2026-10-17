//! Which workspace a `ww` call names (ARCH-030, the `cli` chain): the explicit `--workspace`
//! path, else the workspace containing the current folder, else the default workspace of
//! `suite.toml`, else none. A workspace is named by its id: the 16-hex hash of its canonical
//! path (`Env::workspace_hash`), the same id the core computes.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use ww_platform::Env;
use ww_protocol::NO_WORKSPACE;
use ww_schema::SuiteToml;

use crate::ClientError;

/// The file that makes a folder a Worldwright workspace.
pub const WORKSPACE_MANIFEST: &str = "worldwright.yaml";

/// Which step of the `cli` chain named the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSource {
    /// `--workspace <PATH>`.
    Flag,
    /// The current folder, or a folder above it, holds `worldwright.yaml`.
    CurrentFolder,
    /// `default_workspace` in `suite.toml`.
    Default,
}

impl fmt::Display for WorkspaceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WorkspaceSource::Flag => "named by --workspace",
            WorkspaceSource::CurrentFolder => "found from the current folder",
            WorkspaceSource::Default => "the default workspace in suite.toml",
        })
    }
}

/// Where a named workspace came from: its folder as the chain found it, and the step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOrigin {
    pub path: PathBuf,
    pub source: WorkspaceSource,
}

/// What the `cli` chain resolved: the id said in `core/hello`, and where it came from
/// (`None` for `"none"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspace {
    /// A 16-hex workspace id, or `"none"`.
    pub id: String,
    pub origin: Option<WorkspaceOrigin>,
}

/// The workspace id for a `cli` caller (ARCH-030): [`resolve_workspace`] without its origin.
///
/// # Errors
/// As [`resolve_workspace`].
pub fn resolve_workspace_id(
    env: &Env,
    explicit: Option<&Path>,
    cwd: &Path,
) -> Result<String, ClientError> {
    resolve_workspace(env, explicit, cwd).map(|ws| ws.id)
}

/// The workspace for a `cli` caller (ARCH-030), with where it came from:
/// 1. `explicit` (relative to `cwd` when relative), which must contain `worldwright.yaml`;
/// 2. else the nearest folder from `cwd` upwards that contains `worldwright.yaml`;
/// 3. else `default_workspace` in the channel's `suite.toml`;
/// 4. else `"none"`.
///
/// # Errors
/// [`ClientError::Io`] with a plain message when the explicit path is not a workspace, when
/// `suite.toml` cannot be read, or when the default workspace's folder does not exist.
pub fn resolve_workspace(
    env: &Env,
    explicit: Option<&Path>,
    cwd: &Path,
) -> Result<ResolvedWorkspace, ClientError> {
    let found = |path: &Path, source: WorkspaceSource| -> Result<ResolvedWorkspace, ClientError> {
        Ok(ResolvedWorkspace {
            id: id_of(path)?,
            origin: Some(WorkspaceOrigin {
                path: path.to_path_buf(),
                source,
            }),
        })
    };
    let none = || ResolvedWorkspace {
        id: NO_WORKSPACE.to_string(),
        origin: None,
    };
    if let Some(explicit) = explicit {
        let path = cwd.join(explicit);
        if !path.join(WORKSPACE_MANIFEST).is_file() {
            return Err(plain(
                io::ErrorKind::NotFound,
                format!(
                    "{} is not a Worldwright workspace: it has no {WORKSPACE_MANIFEST}.",
                    path.display()
                ),
            ));
        }
        return found(&path, WorkspaceSource::Flag);
    }
    if let Some(dir) = cwd
        .ancestors()
        .find(|dir| dir.join(WORKSPACE_MANIFEST).is_file())
    {
        return found(dir, WorkspaceSource::CurrentFolder);
    }
    let file = env.suite_toml();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(none()),
        Err(e) => {
            return Err(plain(
                e.kind(),
                format!(
                    "The workspace registry {} cannot be read: {e}",
                    file.display()
                ),
            ));
        }
    };
    let suite = SuiteToml::parse(&text).map_err(|e| {
        plain(
            io::ErrorKind::InvalidData,
            format!(
                "The workspace registry {} is not valid: {e}",
                file.display()
            ),
        )
    })?;
    match suite.default_workspace.as_deref().filter(|p| !p.is_empty()) {
        Some(default) => {
            let path = Path::new(default);
            std::fs::canonicalize(path)
                .map(|canonical| ResolvedWorkspace {
                    id: Env::workspace_hash(&canonical),
                    origin: Some(WorkspaceOrigin {
                        path: path.to_path_buf(),
                        source: WorkspaceSource::Default,
                    }),
                })
                .map_err(|e| {
                    plain(
                        e.kind(),
                        format!(
                            "The default workspace {} named in {} cannot be opened: {e}. Pass --workspace, or change default_workspace there.",
                            path.display(),
                            file.display()
                        ),
                    )
                })
        }
        None => Ok(none()),
    }
}

/// The default workspace `env`'s `suite.toml` names, as written there; `None` when there is
/// no file, it doesn't parse, or it names none. For messages only.
pub(crate) fn registered_default(env: &Env) -> Option<PathBuf> {
    let text = std::fs::read_to_string(env.suite_toml()).ok()?;
    let suite = SuiteToml::parse(&text).ok()?;
    suite
        .default_workspace
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// The id of the workspace folder `path`.
fn id_of(path: &Path) -> Result<String, ClientError> {
    let canonical = std::fs::canonicalize(path).map_err(|e| {
        plain(
            e.kind(),
            format!(
                "The workspace folder {} cannot be opened: {e}",
                path.display()
            ),
        )
    })?;
    Ok(Env::workspace_hash(&canonical))
}

fn plain(kind: io::ErrorKind, message: String) -> ClientError {
    ClientError::Io(io::Error::new(kind, message))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ww_testkit::TestEnv;

    use super::*;

    fn id(path: &Path) -> String {
        Env::workspace_hash(&std::fs::canonicalize(path).expect("canonicalize"))
    }

    fn mkdir(path: PathBuf) -> PathBuf {
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test] // spec: ARCH-030
    fn workspace_chain_explicit_then_cwd_then_default_then_none() {
        let env = TestEnv::with_label("Ünïcödé space");
        let outside = mkdir(env.root().join("outside"));

        // 4. Nothing applies: no workspace.
        assert_eq!(
            resolve_workspace_id(&env.env, None, &outside).unwrap(),
            "none"
        );
        // A registry without a default is still no workspace.
        let listed = env.make_workspace("listed");
        env.register_workspace(&listed, false);
        assert_eq!(
            resolve_workspace_id(&env.env, None, &outside).unwrap(),
            "none"
        );

        // 3. The default workspace of suite.toml.
        let default = env.make_workspace("Ünïcödé default");
        env.register_workspace(&default, true);
        assert_eq!(
            resolve_workspace_id(&env.env, None, &outside).unwrap(),
            id(&default)
        );

        // 2. The workspace containing the current folder beats the default, from any depth.
        let here = env.make_workspace("here");
        let deep = mkdir(here.join("a b").join("c"));
        assert_eq!(
            resolve_workspace_id(&env.env, None, &deep).unwrap(),
            id(&here)
        );
        assert_eq!(
            resolve_workspace_id(&env.env, None, &here).unwrap(),
            id(&here)
        );

        // 1. An explicit path beats both; a relative one is taken from the current folder.
        let explicit = env.make_workspace("explicit");
        assert_eq!(
            resolve_workspace_id(&env.env, Some(&explicit), &deep).unwrap(),
            id(&explicit)
        );
        assert_eq!(
            resolve_workspace_id(&env.env, Some(Path::new(r"..\ws-explicit")), &outside).unwrap(),
            id(&explicit)
        );
        assert_eq!(id(&explicit).len(), 16);
    }

    #[test] // spec: ARCH-030
    fn the_chain_says_where_the_workspace_came_from() {
        let env = TestEnv::new();
        let outside = mkdir(env.root().join("outside"));
        let origin = |explicit: Option<&Path>, cwd: &Path| {
            resolve_workspace(&env.env, explicit, cwd)
                .unwrap()
                .origin
                .map(|o| (o.source, o.path))
        };
        assert_eq!(origin(None, &outside), None);
        let default = env.make_workspace("default");
        env.register_workspace(&default, true);
        assert_eq!(
            origin(None, &outside),
            Some((WorkspaceSource::Default, default.clone()))
        );
        let here = env.make_workspace("here");
        let deep = mkdir(here.join("sub"));
        assert_eq!(
            origin(None, &deep),
            Some((WorkspaceSource::CurrentFolder, here.clone()))
        );
        assert_eq!(
            origin(Some(&default), &deep),
            Some((WorkspaceSource::Flag, default.clone()))
        );
        assert_eq!(
            resolve_workspace(&env.env, None, &deep).unwrap().id,
            id(&here)
        );
    }

    #[test] // spec: ARCH-030
    fn a_stale_or_unreadable_registry_is_an_error_naming_suite_toml() {
        let env = TestEnv::with_label("Ünïcödé space");
        let outside = mkdir(env.root().join("outside"));

        // A default workspace whose folder is gone: never "none", never another workspace.
        let gone = env.make_workspace("gone");
        env.register_workspace(&gone, true);
        std::fs::remove_dir_all(&gone).unwrap();
        let text = resolve_workspace_id(&env.env, None, &outside)
            .expect_err("a stale default workspace resolved")
            .to_string();
        assert!(text.contains("suite.toml"), "{text}");
        assert!(text.contains(&gone.display().to_string()), "{text}");

        // A registry that is not TOML.
        std::fs::write(env.env.suite_toml(), "default_workspace = [").unwrap();
        let text = resolve_workspace_id(&env.env, None, &outside)
            .expect_err("an invalid suite.toml resolved")
            .to_string();
        assert!(text.contains("suite.toml"), "{text}");
    }

    #[test] // spec: ARCH-030
    fn explicit_path_without_worldwright_yaml_is_an_error() {
        let env = TestEnv::new();
        let ws = env.make_workspace("real");
        env.register_workspace(&ws, true);
        let plain = tempfile::Builder::new()
            .prefix("ww-client-plain-")
            .tempdir()
            .unwrap();

        let err = resolve_workspace_id(&env.env, Some(plain.path()), &ws)
            .expect_err("a folder without worldwright.yaml was accepted");
        let text = err.to_string();
        assert!(text.contains(&plain.path().display().to_string()), "{text}");
        assert!(text.contains("worldwright.yaml"), "{text}");

        // A missing folder is an error too, never a fallback to another workspace.
        let missing = plain.path().join("gone");
        assert!(resolve_workspace_id(&env.env, Some(&missing), &ws).is_err());
    }
}
