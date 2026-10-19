//! Workspace-relative paths (CORE-040), the paths the store never writes (ARCH-082,
//! CORE-026) and the ones the trash never touches (ARCH-058), each also checked by its long
//! form so an 8.3 short name can't get past them.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use lil_platform::names::{LEGACY_WORKSPACE_DIR, is_workspace_dir};
use lil_schema::{Version, check_rel_path};

use super::trash::TRASH_DIR;
use super::write::StoreError;

/// A checked workspace-relative path with `/` separators (CORE-040: at most 180 characters,
/// no reserved names, no bad components, never outside the workspace).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WsPath(String);

impl WsPath {
    /// Checks `rel` with `lil_schema::check_rel_path`; a refused path is `store.path_invalid`.
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

    /// The case-folded form ([`path_key`]): Windows names are case-insensitive, so two
    /// spellings of one file share one key.
    pub(crate) fn key(&self) -> String {
        path_key(&self.0)
    }
}

impl fmt::Display for WsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The case-folded key of a workspace-relative path: `lil_platform::paths::upcase_name`, the
/// system uppercase table Windows compares file names by (one UTF-16 unit to one unit), so two
/// spellings share a key exactly when they name one file (`ᾳ`/`ᾼ` do, `ς`/`σ` don't). The path
/// locks and the `generated` and `generated_pending` rows use this one folding.
pub fn path_key(rel: &str) -> String {
    lil_platform::paths::upcase_name(rel)
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
///   and a file named like that folder;
/// - the root's workspace data folder from before the rename and anything in it (R4: only
///   read; it stays reserved after the rename's cleanup).
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
    if parts[0].eq_ignore_ascii_case(LEGACY_WORKSPACE_DIR) {
        return refuse(
            "the workspace data folder from before the rename is only read; the suite never writes in it",
        );
    }
    Ok(())
}

/// The long form of `rel` under `root`, workspace-relative: `resolve` (`Probe::long_form` in
/// the store) expands every 8.3 short component of the part that exists, so `LILITH~1/x` comes
/// back as `.lilith/x`. A resolver error, or a long form that isn't under the long form of
/// `root`, is `store.path_invalid` (fail closed).
pub(crate) fn long_rel(
    root: &Path,
    rel: &WsPath,
    resolve: &dyn Fn(&Path) -> io::Result<PathBuf>,
) -> Result<WsPath, StoreError> {
    let refuse = |why: String| StoreError::PathInvalid {
        path: rel.as_str().to_string(),
        why,
    };
    let long =
        |p: &Path| resolve(p).map_err(|e| refuse(format!("its long form can't be read: {e}")));
    let (long_root, long_abs) = (long(root)?, long(&rel.abs(root))?);
    let (lr, la) = (
        long_root
            .to_string_lossy()
            .trim_end_matches('\\')
            .to_string(),
        long_abs.to_string_lossy().into_owned(),
    );
    let inside_root = la.len() > lr.len()
        && la.is_char_boundary(lr.len())
        && path_key(&la[..lr.len()]) == path_key(&lr)
        && la[lr.len()..].starts_with('\\');
    if !inside_root {
        return Err(refuse(
            "its long form is not under the workspace folder".to_string(),
        ));
    }
    let parts: Vec<String> = Path::new(&la[lr.len()..])
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Ok(WsPath::from_disk(parts.join("/")))
}

/// `store.path_invalid` for the paths the trash never touches, compared without regard to case:
/// the trash itself, either workspace data folder ([`is_workspace_dir`]), `.git` and the ledger.
/// `what` finishes the reason ("moved to the trash", "restored to").
fn refuse_reserved(rel: &WsPath, what: &str) -> Result<(), StoreError> {
    let first = rel.as_str().split('/').next().unwrap_or_default();
    let refused = is_workspace_dir(first)
        || [TRASH_DIR, ".git"]
            .iter()
            .any(|area| first.eq_ignore_ascii_case(area))
        || path_key(rel.as_str()) == path_key("registry/ledger.csv");
    if refused {
        return Err(StoreError::PathInvalid {
            path: rel.as_str().to_string(),
            why: format!(
                "the trash, the suite's own folders, .git and the ledger are never {what}"
            ),
        });
    }
    Ok(())
}

/// The trash's reserved-path check ([`refuse_reserved`]) on `rel` as given and on its long form
/// ([`long_rel`] with `resolve`), so an 8.3 short name (`LILITH~1`, `GIT~1`) is refused like
/// the long name. A resolver error refuses the path (fail closed). Returns the long form, for
/// the caller's other checks.
pub(crate) fn reserved_on_disk(
    root: &Path,
    rel: &WsPath,
    what: &str,
    resolve: &dyn Fn(&Path) -> io::Result<PathBuf>,
) -> Result<WsPath, StoreError> {
    refuse_reserved(rel, what)?;
    let long = long_rel(root, rel, resolve)?;
    refuse_reserved(&long, what).map_err(|e| match e {
        StoreError::PathInvalid { why, .. } => StoreError::PathInvalid {
            path: rel.as_str().to_string(),
            why,
        },
        other => other,
    })?;
    Ok(long)
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

    #[test] // spec: CORE-040
    fn path_keys_fold_case_like_windows_names() {
        assert_eq!(path_key("notes/Ärger.txt"), path_key("NOTES/ärger.TXT"));
        assert_eq!(path_key("a/b"), "A/B");
        assert_eq!(
            path_key("\u{1fb3}"),
            path_key("\u{1fbc}"),
            "one name on NTFS"
        );
        assert_ne!(
            path_key("\u{3c2}"),
            path_key("\u{3c3}"),
            "two names on NTFS"
        );
        assert_ne!(path_key("a.txt"), path_key("b.txt"));
        let p = WsPath::parse("Notes/Ä.txt").unwrap();
        assert_eq!(p.key(), path_key("notes/ä.TXT"));
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
        // The legacy workspace data folder, in any case (R4).
        for refused in [
            LEGACY_WORKSPACE_DIR.to_string(),
            format!("{LEGACY_WORKSPACE_DIR}/state.json"),
            format!("{}/x", LEGACY_WORKSPACE_DIR.to_uppercase()),
        ] {
            let p = WsPath::parse(&refused).unwrap();
            assert!(writable(&p).is_err(), "{refused}");
        }
        let deeper = WsPath::parse(&format!("notes/{LEGACY_WORKSPACE_DIR}/x")).unwrap();
        assert!(writable(&deeper).is_ok());
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

    /// The reserved-name check runs on the long form a resolver gives, so it holds on a
    /// volume with 8.3 names off too: the stub maps short names the way `GetLongPathNameW`
    /// would. A resolver that fails refuses the path (fail closed).
    #[test] // spec: ARCH-058, CORE-040
    fn reserved_check_runs_on_the_long_name() {
        let ws = Path::new(r"C:\ws");
        let legacy = format!(r"{LEGACY_WORKSPACE_DIR}\x");
        let map: Vec<(&str, String)> = vec![
            (r"LILITH~1\x", r".lilith\x".to_string()),
            (r"WORLDW~1\x", legacy),
            (r"GIT~1\x", r".git\x".to_string()),
            (r"TRASH~1\x", r".trash\x".to_string()),
            (r"NOTES~1\x", r"notes.lilith\x".to_string()),
        ];
        let resolve = |p: &Path| -> io::Result<PathBuf> {
            if p == ws {
                return Ok(ws.to_path_buf());
            }
            let inside = p.strip_prefix(ws).map_err(io::Error::other)?;
            map.iter()
                .find(|(short, _)| inside == Path::new(short))
                .map(|(_, long)| ws.join(long))
                .ok_or_else(|| io::Error::other("not in the stub"))
        };
        for short in ["LILITH~1/x", "WORLDW~1/x", "GIT~1/x", "TRASH~1/x"] {
            let got = reserved_on_disk(ws, &WsPath::parse(short).unwrap(), "moved", &resolve);
            assert!(
                matches!(got, Err(StoreError::PathInvalid { .. })),
                "{short}: {got:?}"
            );
        }
        let notes = reserved_on_disk(ws, &WsPath::parse("NOTES~1/x").unwrap(), "moved", &resolve);
        assert_eq!(
            notes.map(|long| long.as_str().to_string()),
            Ok("notes.lilith/x".to_string())
        );
        let failing = |_: &Path| -> io::Result<PathBuf> { Err(io::Error::from_raw_os_error(5)) };
        let closed = reserved_on_disk(ws, &WsPath::parse("notes/x").unwrap(), "moved", &failing);
        assert!(
            matches!(closed, Err(StoreError::PathInvalid { .. })),
            "{closed:?}"
        );
    }
}
