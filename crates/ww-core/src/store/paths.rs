//! Workspace-relative paths (CORE-040) and the paths the store never writes (ARCH-082,
//! CORE-026).

use std::fmt;
use std::path::{Path, PathBuf};

use ww_schema::{Version, check_rel_path};

use super::write::StoreError;

/// A checked workspace-relative path with `/` separators (CORE-040: at most 180 characters,
/// no reserved names, no bad components, never outside the workspace).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WsPath(String);

impl WsPath {
    /// Checks `rel` with `ww_schema::check_rel_path`; a refused path is `store.path_invalid`.
    pub fn parse(rel: &str) -> Result<WsPath, StoreError> {
        check_rel_path(rel).map_err(|problem| StoreError::PathInvalid {
            path: rel.to_string(),
            why: problem.to_string(),
        })?;
        Ok(WsPath(rel.to_string()))
    }

    /// A path the store built itself from names on disk (a temp or backup file beside a
    /// target, a conflict copy): its components are already valid, but it may be longer than
    /// the budget, which applies to the paths callers ask for.
    pub(crate) fn from_disk(rel: String) -> WsPath {
        WsPath(rel)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The absolute path under `root`, with `\` separators.
    pub fn abs(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        for component in self.0.split('/') {
            path.push(component);
        }
        path
    }

    /// `self/segment` (`segment` may hold `/`), checked as a whole.
    pub fn join(&self, segment: &str) -> Result<WsPath, StoreError> {
        WsPath::parse(&format!("{}/{segment}", self.0))
    }

    /// The last component.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// The folder part, or `None` at the workspace root.
    pub fn parent(&self) -> Option<&str> {
        self.0.rsplit_once('/').map(|(dir, _)| dir)
    }

    /// The case-folded form: Windows names are case-insensitive, so two spellings of one
    /// file share one key.
    pub(crate) fn key(&self) -> String {
        self.0.to_lowercase()
    }
}

impl fmt::Display for WsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `v<major.minor.patch>`, the name of a sealed version folder (the `v` in either case, since
/// Windows names are case-insensitive).
fn is_version_folder(name: &str) -> bool {
    name.strip_prefix(['v', 'V'])
        .is_some_and(|rest| rest.parse::<Version>().is_ok())
}

/// Refuses the paths the store never writes, compared without regard to case:
/// - `registry/ledger.csv`, which only the user edits (ARCH-082);
/// - anything in a sealed version folder, `projects/<p>/assets/<a>/v<semver>/…` (CORE-026),
///   and a file named like that folder.
pub fn writable(rel: &WsPath) -> Result<(), StoreError> {
    let parts: Vec<String> = rel.as_str().split('/').map(str::to_lowercase).collect();
    let refuse = |why: &str| {
        Err(StoreError::PathInvalid {
            path: rel.as_str().to_string(),
            why: why.to_string(),
        })
    };
    if parts.len() == 2 && parts[0] == "registry" && parts[1] == "ledger.csv" {
        return refuse("registry/ledger.csv is the user's own file; the suite never writes it");
    }
    if parts.len() >= 5
        && parts[0] == "projects"
        && parts[2] == "assets"
        && is_version_folder(&parts[4])
    {
        return refuse("a sealed version folder is never written; start a new version instead");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-040
    fn parse_join_and_abs() {
        let p = WsPath::parse("notes/a.txt").unwrap();
        assert_eq!(p.as_str(), "notes/a.txt");
        assert_eq!(p.file_name(), "a.txt");
        assert_eq!(p.parent(), Some("notes"));
        assert_eq!(WsPath::parse("a.txt").unwrap().parent(), None);
        assert_eq!(
            p.abs(Path::new(r"C:\ws")),
            PathBuf::from(r"C:\ws\notes\a.txt")
        );
        let dir = WsPath::parse("notes").unwrap();
        assert_eq!(dir.join("b/c.txt").unwrap().as_str(), "notes/b/c.txt");
        assert!(dir.join("../x").is_err());
        for bad in ["", "/x", "C:/x", r"a\b", "a/../b", "con.txt", "a/b."] {
            assert!(
                matches!(WsPath::parse(bad), Err(StoreError::PathInvalid { .. })),
                "{bad}"
            );
        }
    }

    #[test] // spec: ARCH-082, CORE-026
    fn ledger_and_sealed_folders_are_not_writable() {
        for refused in [
            "registry/ledger.csv",
            "Registry/LEDGER.CSV",
            "projects/p/assets/a/v1.0.0/x.png",
            "projects/p/assets/a/V2.10.3/deep/x.png",
            "projects/p/assets/a/v1.0.0",
        ] {
            let p = WsPath::parse(refused).unwrap();
            assert!(writable(&p).is_err(), "{refused}");
        }
        for allowed in [
            "registry/ledger.csv.bak",
            "registry/other/ledger.csv",
            "projects/p/assets/a/a.manifest.json",
            "projects/p/assets/a/candidates/r_1/v1.0.0/x.png",
            "projects/p/assets/a/v01.0.0/x.png",
            "projects/p/assets/a/v1.0/x.png",
            "projects/p/notes/v1.0.0/x.png",
        ] {
            let p = WsPath::parse(allowed).unwrap();
            assert!(writable(&p).is_ok(), "{allowed}");
        }
    }
}
