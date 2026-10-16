//! Which workspace a `ww` call names (ARCH-030, the `cli` chain): the explicit `--workspace`
//! path, else the workspace containing the current folder, else the default workspace of
//! `suite.toml`, else none. A workspace is named by its id: the 16-hex hash of its canonical
//! path (`Env::workspace_hash`), the same id the core computes.

use std::io;
use std::path::Path;

use ww_platform::Env;
use ww_protocol::NO_WORKSPACE;
use ww_schema::SuiteToml;

use crate::ClientError;

/// The file that makes a folder a Worldwright workspace.
pub const WORKSPACE_MANIFEST: &str = "worldwright.yaml";

/// The workspace id for a `cli` caller (ARCH-030):
/// 1. `explicit` (relative to `cwd` when relative), which must contain `worldwright.yaml`;
/// 2. else the nearest folder from `cwd` upwards that contains `worldwright.yaml`;
/// 3. else `default_workspace` in the channel's `suite.toml`;
/// 4. else `"none"`.
///
/// # Errors
/// [`ClientError::Io`] with a plain message when the explicit path is not a workspace, when
/// `suite.toml` cannot be read, or when the default workspace's folder does not exist.
pub fn resolve_workspace_id(
    env: &Env,
    explicit: Option<&Path>,
    cwd: &Path,
) -> Result<String, ClientError> {
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
        return id_of(&path);
    }
    if let Some(dir) = cwd
        .ancestors()
        .find(|dir| dir.join(WORKSPACE_MANIFEST).is_file())
    {
        return id_of(dir);
    }
    let file = env.suite_toml();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(NO_WORKSPACE.to_string()),
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
        Some(default) => id_of(Path::new(default)),
        None => Ok(NO_WORKSPACE.to_string()),
    }
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
