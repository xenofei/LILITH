//! The suite's names in one place: the new names, and every legacy name the code still reads
//! for one release (R4). PR-D removes the `LEGACY_*` constants.
//!
//! Code that needs a legacy name uses one of these constants, so the legacy spellings appear
//! only here and on the lines that use them. The one exception is the reference URI scheme,
//! which lives in `lil-schema` (it depends on no other crate, QA-027).

/// The product name, used for the data folders, the registry root and the credential prefixes.
pub const APP_NAME: &str = "LILITH";
/// The product name before the rename.
pub const LEGACY_APP_NAME: &str = "Worldwright";

/// The home folder below `%USERPROFILE%` (owner tooling: reviewer key, privacy denylist).
pub const HOME_DIR: &str = ".lilith";
/// The home folder before the rename.
pub const LEGACY_HOME_DIR: &str = ".worldwright";

/// The per-volume folder for workspace-local data when the workspace is not on the system
/// volume.
pub const LOCAL_DIR: &str = ".lilith-local";
/// The per-volume local-data folder before the rename.
pub const LEGACY_LOCAL_DIR: &str = ".worldwright-local";

/// The owner-tooling credential prefix (the agent token): every use.
pub const DEV_CREDENTIAL_PREFIX: &str = "LILITH-dev/";
/// The owner-tooling credential prefix before the rename: read and delete only.
pub const LEGACY_DEV_CREDENTIAL_PREFIX: &str = "Worldwright-dev/";

/// The environment variable that holds the privacy denylist (CI).
pub const DENYLIST_ENV: &str = "LIL_PRIVACY_DENYLIST";
/// The denylist variable before the rename.
pub const LEGACY_DENYLIST_ENV: &str = "WW_PRIVACY_DENYLIST";

/// The workspace file at a workspace's root (map row 6).
pub const WORKSPACE_FILE: &str = "lilith.yaml";
/// The workspace file before the rename: found when there is no [`WORKSPACE_FILE`], never
/// renamed or written (R4).
pub const LEGACY_WORKSPACE_FILE: &str = "worldwright.yaml";

/// The workspace data folder at a workspace's root: `state.json`, `staging\`, `fence\` and
/// `core.lock` (map row 7).
pub const WORKSPACE_DIR: &str = ".lilith";
/// The workspace data folder before the rename. Its `state.json` is read when the new one is
/// missing, its staging leftovers are listed as orphans, and its `core.lock` is taken (D4);
/// nothing else in it is written, and the folder itself is never created (R4).
pub const LEGACY_WORKSPACE_DIR: &str = ".worldwright";

/// The marker of a temp file beside its target: `.<name>.lil-tmp-<pid>-<n>` (map row 22).
pub const TMP_MARKER: &str = ".lil-tmp-";
/// The temp-file marker before the rename: recognised by recovery and the index, never written.
pub const LEGACY_TMP_MARKER: &str = ".ww-tmp-";
/// The marker of a backup beside its target: `.<name>.lil-bak-<pid>-<n>`.
pub const BAK_MARKER: &str = ".lil-bak-";
/// The backup marker before the rename: recognised by recovery and the index, never written.
pub const LEGACY_BAK_MARKER: &str = ".ww-bak-";
/// The marker of a visible conflict copy: `<name>.lil-conflict-<utc>`.
pub const CONFLICT_MARKER: &str = ".lil-conflict-";
/// The conflict-copy marker before the rename: such copies are recognised and left alone.
pub const LEGACY_CONFLICT_MARKER: &str = ".ww-conflict-";

/// The workspace file in `dir`: [`WORKSPACE_FILE`] if it is a file, else
/// [`LEGACY_WORKSPACE_FILE`] if that is a file, else `None` (R4: the legacy file is only read).
#[must_use]
pub fn workspace_file_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    [WORKSPACE_FILE, LEGACY_WORKSPACE_FILE]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// Whether a root-folder name is a workspace data folder, [`WORKSPACE_DIR`] or
/// [`LEGACY_WORKSPACE_DIR`], without regard to case (Windows compares names that way).
#[must_use]
pub fn is_workspace_dir(name: &str) -> bool {
    name.eq_ignore_ascii_case(WORKSPACE_DIR) || name.eq_ignore_ascii_case(LEGACY_WORKSPACE_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_file_prefers_lilith_yaml() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(workspace_file_in(dir.path()), None);
        std::fs::write(dir.path().join(LEGACY_WORKSPACE_FILE), "name: a\n").unwrap();
        assert_eq!(
            workspace_file_in(dir.path()),
            Some(dir.path().join(LEGACY_WORKSPACE_FILE))
        );
        std::fs::write(dir.path().join(WORKSPACE_FILE), "name: a\n").unwrap();
        assert_eq!(
            workspace_file_in(dir.path()),
            Some(dir.path().join(WORKSPACE_FILE))
        );
        // A folder by the new name is not a workspace file: the legacy file is found.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir(other.path().join(WORKSPACE_FILE)).unwrap();
        std::fs::write(other.path().join(LEGACY_WORKSPACE_FILE), "name: b\n").unwrap();
        assert_eq!(
            workspace_file_in(other.path()),
            Some(other.path().join(LEGACY_WORKSPACE_FILE))
        );
    }

    #[test]
    fn is_workspace_dir_covers_both_names_in_any_case() {
        for name in [
            WORKSPACE_DIR.to_string(),
            LEGACY_WORKSPACE_DIR.to_string(),
            WORKSPACE_DIR.to_uppercase(),
            LEGACY_WORKSPACE_DIR.to_uppercase(),
            ".LiLiTh".to_string(),
        ] {
            assert!(is_workspace_dir(&name), "{name}");
        }
        for name in [".lilith-local", "lilith", ".trash", ".git", ".lilithx"] {
            assert!(!is_workspace_dir(name), "{name}");
        }
    }

    #[test]
    fn markers_are_lil_and_legacy_markers_are_ww() {
        assert_eq!(TMP_MARKER.len(), 9);
        for (new, old) in [
            (TMP_MARKER, LEGACY_TMP_MARKER),
            (BAK_MARKER, LEGACY_BAK_MARKER),
            (CONFLICT_MARKER, LEGACY_CONFLICT_MARKER),
        ] {
            assert!(new.starts_with(".lil-") && new.ends_with('-'), "{new}");
            let tail = |m: &str| m[m.find('-').unwrap()..].to_string();
            assert_eq!(tail(new), tail(old), "{new} {old}");
        }
    }
}
