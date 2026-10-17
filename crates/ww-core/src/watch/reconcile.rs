//! What the watcher decides about one observed file (ARCH-079, ARCH-085, CORE-034), and how an
//! event's path becomes a workspace path. Pure: no file, database or clock is touched here, so
//! the simulated QA-046 variant (Task 14) runs the same code.
//!
//! [`decide`]'s rows are checked in order and the first match wins:
//!
//! | Observed | Recorded | git busy | Action |
//! |---|---|---|---|
//! | any | any | yes | `DeferUntilGitDone` (decided by the rescan after git) |
//! | `CloudOnly` | any | no | `Reindex` (state `cloud_only`, never read) |
//! | `BadManifest` | any | no | `NeedsRepair` (never rewritten) |
//! | `Removed` | Candidate, Sealed or Manifest, and `expected` | no | `Missing` (row and last SHA-256 kept) |
//! | `Removed` | anything else, or none | no | `Reindex` (row removed) |
//! | `Changed` | Manifest | no | `Reindex` (a valid manifest is re-parsed) |
//! | `Changed` | Work, Other or none | no | `Reindex` |
//! | `Changed`, `lfs_oid == Some(recorded)` | Candidate/Sealed | no | `NotFetched` |
//! | `Changed`, `lfs_oid` is `Some` and differs from recorded | Candidate/Sealed | no | `HumanEdit` (the pointer's `oid` as `sha256`) |
//! | `Changed`, `sha256 == recorded`, or `sha256` in `pending` | Candidate/Sealed | no | `Reindex` (the core's own bytes) |
//! | `Changed`, `sha256 == last_human_edit` | Candidate/Sealed | no | `Reindex` (already recorded) |
//! | `Changed`, any other `sha256` | Candidate/Sealed | no | `HumanEdit` (the file is never touched) |
//!
//! Two readings the table leaves open:
//! - A [`Recorded`] whose `sha256` is empty (a pre-2b run file recorded without a hash, and no
//!   generated hash) has nothing to compare a change with, so a `Changed` file is `Reindex`.
//! - An LFS pointer whose `oid` is the latest human edit's hash is `NotFetched`: that human
//!   edit is what the pointer stands for, so it is recorded already and never pushed twice.

use std::io;
use std::path::{Path, PathBuf};

use crate::index::{FileKind, asset_of};
use crate::store::WsPath;

/// What the suite knows about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// Bare hex: the manifest's `hashes` entry, else the run's file record, else the last
    /// generated hash; empty when none records one.
    pub sha256: String,
    pub kind: FileKind,
    /// The asset's manifest.
    pub manifest: Option<WsPath>,
    /// Still expected on disk: a manifest, a file in `hashes`, or a file of an open run without
    /// `trashed_utc` (Task 10's `missing` rule).
    pub expected: bool,
    /// The old and new hashes of a pending store replace (Task 6).
    pub pending: Vec<String>,
    /// The latest `human_edits[path].sha256`.
    pub last_human_edit: Option<String>,
}

/// One file as a stable read saw it. Unstable or locked reads are parked by the index and never
/// become an `Observed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    Changed {
        path: WsPath,
        /// Bare hex of the bytes (for a pointer, its `oid`).
        sha256: String,
        /// The `oid` when the file is a git-lfs pointer (ARCH-085).
        lfs_oid: Option<String>,
    },
    CloudOnly(WsPath),
    Removed(WsPath),
    /// A manifest that doesn't parse, or disagrees with its folder.
    BadManifest(WsPath),
}

impl Observed {
    /// The path observed.
    pub fn path(&self) -> &WsPath {
        match self {
            Observed::Changed { path, .. }
            | Observed::CloudOnly(path)
            | Observed::Removed(path)
            | Observed::BadManifest(path) => path,
        }
    }
}

/// What the watcher does about an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Reindex(WsPath),
    /// Push `{path, sha256, previous_sha256, date}` to the manifest's `human_edits`; the file is
    /// never touched (CORE-034).
    HumanEdit {
        manifest: WsPath,
        path: WsPath,
        sha256: String,
        previous: String,
    },
    NeedsRepair(WsPath),
    NotFetched(WsPath),
    Missing(WsPath),
    /// Git is working on the workspace: the rescan after it decides (ARCH-083).
    DeferUntilGitDone(WsPath),
}

/// The canonical manifest of the asset folder `path` is in:
/// `projects/<p>/assets/<a>/<a>.manifest.json`.
pub(crate) fn manifest_of(path: &WsPath) -> Option<WsPath> {
    let (project, asset) = asset_of(path.as_str())?;
    Some(WsPath::from_disk(format!(
        "projects/{project}/assets/{asset}/{asset}.manifest.json"
    )))
}

/// The module table, row by row.
pub fn decide(obs: &Observed, recorded: Option<&Recorded>, git_busy: bool) -> Action {
    let path = obs.path().clone();
    if git_busy {
        return Action::DeferUntilGitDone(path);
    }
    let (sha256, lfs_oid) = match obs {
        Observed::CloudOnly(_) => return Action::Reindex(path),
        Observed::BadManifest(_) => return Action::NeedsRepair(path),
        Observed::Removed(_) => {
            return match recorded {
                Some(r)
                    if r.expected
                        && matches!(
                            r.kind,
                            FileKind::Candidate | FileKind::Sealed | FileKind::Manifest
                        ) =>
                {
                    Action::Missing(path)
                }
                _ => Action::Reindex(path),
            };
        }
        Observed::Changed {
            sha256, lfs_oid, ..
        } => (sha256, lfs_oid),
    };
    let Some(r) = recorded else {
        return Action::Reindex(path);
    };
    if !matches!(r.kind, FileKind::Candidate | FileKind::Sealed) || r.sha256.is_empty() {
        return Action::Reindex(path);
    }
    let human_edit = |sha256: &str| Action::HumanEdit {
        manifest: r
            .manifest
            .clone()
            .or_else(|| manifest_of(&path))
            .unwrap_or_else(|| path.clone()),
        path: path.clone(),
        sha256: sha256.to_string(),
        previous: r
            .last_human_edit
            .clone()
            .unwrap_or_else(|| r.sha256.clone()),
    };
    if let Some(oid) = lfs_oid {
        return if *oid == r.sha256 || r.last_human_edit.as_deref() == Some(oid.as_str()) {
            Action::NotFetched(path)
        } else {
            human_edit(oid)
        };
    }
    if *sha256 == r.sha256 || r.pending.iter().any(|p| p == sha256) {
        return Action::Reindex(path);
    }
    if r.last_human_edit.as_deref() == Some(sha256.as_str()) {
        return Action::Reindex(path);
    }
    human_edit(sha256)
}

/// `parts` joined with `/` as a workspace path, if it is one.
fn ws_path(parts: &[String]) -> Option<WsPath> {
    if parts.is_empty() {
        return None;
    }
    WsPath::parse(&parts.join("/")).ok()
}

/// The components of a relative path, `\` or `/` separated.
fn components(rel: &str) -> Vec<String> {
    rel.split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

/// The components of `long`, a relative path `resolve` returned.
fn path_components(long: &Path) -> Vec<String> {
    long.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The workspace path of an event's relative path `rel` (`\` or `/` separated).
///
/// A component containing `~` may be an 8.3 short name ("it is unspecified which one",
/// Microsoft Learn): the path is resolved with `resolve` (`fs::long_name`, given and returning
/// paths relative to the workspace). When it can't be resolved (the file is gone), or isn't a
/// workspace path, the nearest parent that maps to one is the `Err`, for a reconcile of that
/// folder. When not even the first component maps, the `Err` is that first component as it
/// came; the watcher then rescans the whole workspace.
pub fn map_notify_path(
    rel: &str,
    resolve: &dyn Fn(&Path) -> io::Result<PathBuf>,
) -> Result<WsPath, WsPath> {
    let parts = components(rel);
    let short = |parts: &[String]| parts.iter().any(|c| c.contains('~'));
    let long = |parts: &[String]| -> Option<Vec<String>> {
        if short(parts) {
            let joined: PathBuf = parts.iter().collect();
            resolve(&joined).ok().map(|l| path_components(&l))
        } else {
            Some(parts.to_vec())
        }
    };
    if let Some(found) = long(&parts).as_deref().and_then(ws_path) {
        return Ok(found);
    }
    for k in (1..parts.len()).rev() {
        if let Some(parent) = long(&parts[..k]).as_deref().and_then(ws_path) {
            return Err(parent);
        }
    }
    Err(WsPath::from_disk(
        parts.first().cloned().unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(rel: &str) -> WsPath {
        WsPath::parse(rel).unwrap()
    }

    const SEALED: &str = "projects/sketchbook/assets/crane/v1.0.0/a.png";
    const CANDIDATE: &str = "projects/sketchbook/assets/crane/candidates/r_20270101_0800/c.png";
    const MANIFEST: &str = "projects/sketchbook/assets/crane/crane.manifest.json";
    const OLD: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const NEW: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const EDIT: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const PEND: &str = "4444444444444444444444444444444444444444444444444444444444444444";

    fn rec(kind: FileKind) -> Recorded {
        Recorded {
            sha256: OLD.to_string(),
            kind,
            manifest: Some(p(MANIFEST)),
            expected: true,
            pending: Vec::new(),
            last_human_edit: None,
        }
    }

    fn changed(path: &str, sha: &str) -> Observed {
        Observed::Changed {
            path: p(path),
            sha256: sha.to_string(),
            lfs_oid: None,
        }
    }

    fn pointer(path: &str, oid: &str) -> Observed {
        Observed::Changed {
            path: p(path),
            sha256: oid.to_string(),
            lfs_oid: Some(oid.to_string()),
        }
    }

    fn human(path: &str, sha: &str, previous: &str) -> Action {
        Action::HumanEdit {
            manifest: p(MANIFEST),
            path: p(path),
            sha256: sha.to_string(),
            previous: previous.to_string(),
        }
    }

    #[test] // spec: ARCH-079, ARCH-083, ARCH-085, CORE-034
    fn decide_follows_the_table() {
        let sealed = rec(FileKind::Sealed);
        // Row 1: git busy defers everything.
        for obs in [
            changed(SEALED, NEW),
            Observed::Removed(p(SEALED)),
            Observed::BadManifest(p(MANIFEST)),
            Observed::CloudOnly(p(SEALED)),
        ] {
            assert_eq!(
                decide(&obs, Some(&sealed), true),
                Action::DeferUntilGitDone(obs.path().clone())
            );
        }
        // Row 2: a placeholder is never read.
        assert_eq!(
            decide(&Observed::CloudOnly(p(SEALED)), Some(&sealed), false),
            Action::Reindex(p(SEALED))
        );
        // Row 3.
        assert_eq!(
            decide(&Observed::BadManifest(p(MANIFEST)), None, false),
            Action::NeedsRepair(p(MANIFEST))
        );
        // Rows 4 and 5: a removed file the suite still expects is missing; anything else goes.
        for kind in [FileKind::Candidate, FileKind::Sealed, FileKind::Manifest] {
            assert_eq!(
                decide(&Observed::Removed(p(SEALED)), Some(&rec(kind)), false),
                Action::Missing(p(SEALED)),
                "{kind:?}"
            );
        }
        let not_expected = Recorded {
            expected: false,
            ..rec(FileKind::Candidate)
        };
        assert_eq!(
            decide(&Observed::Removed(p(CANDIDATE)), Some(&not_expected), false),
            Action::Reindex(p(CANDIDATE)),
            "a file of a selected, rejected or trashed run"
        );
        for kind in [FileKind::Work, FileKind::Other] {
            assert_eq!(
                decide(&Observed::Removed(p(SEALED)), Some(&rec(kind)), false),
                Action::Reindex(p(SEALED))
            );
        }
        assert_eq!(
            decide(&Observed::Removed(p(SEALED)), None, false),
            Action::Reindex(p(SEALED))
        );
        // Row 6: outside manifest edits are allowed.
        assert_eq!(
            decide(
                &changed(MANIFEST, NEW),
                Some(&rec(FileKind::Manifest)),
                false
            ),
            Action::Reindex(p(MANIFEST))
        );
        // Row 7.
        for kind in [FileKind::Work, FileKind::Other] {
            assert_eq!(
                decide(&changed(SEALED, NEW), Some(&rec(kind)), false),
                Action::Reindex(p(SEALED))
            );
        }
        assert_eq!(
            decide(&changed(SEALED, NEW), None, false),
            Action::Reindex(p(SEALED))
        );
        // Row 8: a pointer to the recorded content isn't fetched.
        assert_eq!(
            decide(&pointer(SEALED, OLD), Some(&sealed), false),
            Action::NotFetched(p(SEALED))
        );
        // Row 9: a pointer to other content is a human edit, recorded by its oid.
        assert_eq!(
            decide(&pointer(SEALED, NEW), Some(&sealed), false),
            human(SEALED, NEW, OLD)
        );
        // Row 10: the recorded bytes, or either hash of a pending replace.
        assert_eq!(
            decide(&changed(SEALED, OLD), Some(&sealed), false),
            Action::Reindex(p(SEALED))
        );
        let pending = Recorded {
            pending: vec![OLD.to_string(), PEND.to_string()],
            ..rec(FileKind::Candidate)
        };
        assert_eq!(
            decide(&changed(CANDIDATE, PEND), Some(&pending), false),
            Action::Reindex(p(CANDIDATE))
        );
        // Row 11: a human edit already recorded is never pushed twice.
        let edited = Recorded {
            last_human_edit: Some(EDIT.to_string()),
            ..rec(FileKind::Sealed)
        };
        assert_eq!(
            decide(&changed(SEALED, EDIT), Some(&edited), false),
            Action::Reindex(p(SEALED))
        );
        assert_eq!(
            decide(&pointer(SEALED, EDIT), Some(&edited), false),
            Action::NotFetched(p(SEALED)),
            "a pointer to the recorded human edit"
        );
        // Row 12: any other content is a human edit; a later one follows the earlier.
        assert_eq!(
            decide(&changed(SEALED, NEW), Some(&sealed), false),
            human(SEALED, NEW, OLD)
        );
        assert_eq!(
            decide(
                &changed(CANDIDATE, NEW),
                Some(&rec(FileKind::Candidate)),
                false
            ),
            human(CANDIDATE, NEW, OLD)
        );
        assert_eq!(
            decide(&changed(SEALED, NEW), Some(&edited), false),
            human(SEALED, NEW, EDIT)
        );
        // Nothing recorded to compare with: no human edit.
        let unhashed = Recorded {
            sha256: String::new(),
            ..rec(FileKind::Candidate)
        };
        assert_eq!(
            decide(&changed(CANDIDATE, NEW), Some(&unhashed), false),
            Action::Reindex(p(CANDIDATE))
        );
        // Without a known manifest, the asset folder's canonical one.
        let no_manifest = Recorded {
            manifest: None,
            ..rec(FileKind::Sealed)
        };
        assert_eq!(
            decide(&changed(SEALED, NEW), Some(&no_manifest), false),
            human(SEALED, NEW, OLD)
        );
    }

    #[test] // spec: ARCH-079
    fn short_name_component_is_resolved_or_parent_reconciled() {
        let resolve = |rel: &Path| -> io::Result<PathBuf> {
            let text = rel.to_string_lossy();
            if text.contains("CHR_PA~1") {
                return Err(io::Error::from_raw_os_error(2));
            }
            Ok(PathBuf::from(text.replace("ASSETS~1", "assets")))
        };
        assert_eq!(
            map_notify_path(r"projects\sketchbook\ASSETS~1\crane\a.png", &resolve),
            Ok(p("projects/sketchbook/assets/crane/a.png"))
        );
        // Gone: the nearest parent that maps.
        assert_eq!(
            map_notify_path(
                r"projects\sketchbook\ASSETS~1\CHR_PA~1\v1.0.0\a.png",
                &resolve
            ),
            Err(p("projects/sketchbook/assets"))
        );
        // A long path never calls the resolver.
        let never = |_: &Path| -> io::Result<PathBuf> { panic!("resolved a long path") };
        assert_eq!(
            map_notify_path(r"notes\Ünïcödé space.txt", &never),
            Ok(p("notes/Ünïcödé space.txt"))
        );
        // A name that isn't a workspace path (a reserved name) gives its folder.
        assert_eq!(map_notify_path(r"notes\CON", &never), Err(p("notes")));
        // Not even the first component maps: it comes back as it is (the caller rescans).
        assert_eq!(
            map_notify_path(r"CHR_PA~1\x", &resolve)
                .unwrap_err()
                .as_str(),
            "CHR_PA~1"
        );
    }
}
