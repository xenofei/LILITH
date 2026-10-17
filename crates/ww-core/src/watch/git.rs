//! Which event paths the watcher looks at, and the git pause (ARCH-079, ARCH-083).
//!
//! Of `.worldwright\` only `fence\<name>` is watched (the test fence), and `.trash\` not at
//! all. Of `.git\` (at the root) only `index.lock` and `HEAD`; every other `.git` folder, at
//! any depth, is ignored, as the index ignores it.
//!
//! Git counts as working from the moment `index.lock` appears or `HEAD` changes, until
//! `index.lock` is gone and `HEAD` has stayed unchanged for `git_quiet` (rebase and pull take
//! and release `index.lock` between their steps). Then the post-git rescan runs.

use tokio::time::Instant;

use crate::store::WsPath;

/// What an event path is to the watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Class {
    /// `.worldwright/fence/<name>`.
    Fence(String),
    /// `.git/index.lock`.
    GitLock,
    /// `.git/HEAD`.
    GitHead,
    /// Not watched: the rest of `.worldwright/` and `.git/`, `.trash/`, `.git` folders deeper
    /// down.
    Ignored,
    /// A workspace file or folder.
    Path,
}

/// [`Class`] of `path` (names compare without regard to case).
pub(crate) fn classify(path: &WsPath) -> Class {
    let parts: Vec<&str> = path.as_str().split('/').collect();
    let first = parts[0];
    if first.eq_ignore_ascii_case(".worldwright") {
        return match parts.as_slice() {
            [_, fence, name] if fence.eq_ignore_ascii_case("fence") => {
                Class::Fence((*name).to_string())
            }
            _ => Class::Ignored,
        };
    }
    if first.eq_ignore_ascii_case(".trash") {
        return Class::Ignored;
    }
    if first.eq_ignore_ascii_case(".git") {
        return match parts.as_slice() {
            [_, name] if name.eq_ignore_ascii_case("index.lock") => Class::GitLock,
            [_, name] if name.eq_ignore_ascii_case("HEAD") => Class::GitHead,
            _ => Class::Ignored,
        };
    }
    if parts.iter().any(|p| p.eq_ignore_ascii_case(".git")) {
        return Class::Ignored;
    }
    Class::Path
}

/// The git pause as the watcher tracks it.
#[derive(Debug, Default)]
pub(crate) struct GitState {
    /// Git is working: the gate's flag is set and paths wait for the post-git rescan.
    pub(crate) busy: bool,
    /// The last `index.lock` or `HEAD` event (or the last look that found `index.lock`).
    pub(crate) last: Option<Instant>,
    /// Paths decided by the rescan after git, by key.
    pub(crate) deferred: std::collections::BTreeMap<String, WsPath>,
    /// What the index's verification found while git worked: the file's content then, which
    /// the post-git rescan may not see change (its row still trusts the old hash).
    pub(crate) observed: Vec<super::Observed>,
    /// Events were lost while git worked: the post-git rescan also counts as the overflow's.
    pub(crate) overflowed: bool,
}

impl GitState {
    /// Git did something now. Returns whether this starts a pause.
    pub(crate) fn activity(&mut self, now: Instant) -> bool {
        self.last = Some(now);
        let started = !self.busy;
        self.busy = true;
        started
    }

    /// When the pause may end, if one is on: `quiet` after the last activity.
    pub(crate) fn quiet_at(&self, quiet: std::time::Duration) -> Option<Instant> {
        if !self.busy {
            return None;
        }
        Some(self.last.map_or_else(Instant::now, |last| last + quiet))
    }

    /// Takes the deferred paths, the verification's observations and the overflow mark, and
    /// ends the pause.
    pub(crate) fn finish(&mut self) -> (Vec<WsPath>, Vec<super::Observed>, bool) {
        self.busy = false;
        self.last = None;
        let deferred = std::mem::take(&mut self.deferred).into_values().collect();
        (
            deferred,
            std::mem::take(&mut self.observed),
            std::mem::take(&mut self.overflowed),
        )
    }

    pub(crate) fn defer(&mut self, path: WsPath) {
        self.deferred
            .insert(crate::store::path_key(path.as_str()), path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(rel: &str) -> WsPath {
        WsPath::parse(rel).unwrap()
    }

    #[test] // spec: ARCH-079, ARCH-083
    fn only_fences_index_lock_and_head_are_watched_inside_private_folders() {
        assert_eq!(
            classify(&p(".worldwright/fence/12-3")),
            Class::Fence("12-3".into())
        );
        assert_eq!(
            classify(&p(".Worldwright/FENCE/12-3")),
            Class::Fence("12-3".into())
        );
        for ignored in [
            ".worldwright",
            ".worldwright/fence",
            ".worldwright/staging/sh_0000000000000001/a.png",
            ".worldwright/core.lock",
            ".worldwright/fence/a/b",
            ".trash/2027-01-01/te_0000000000000001/payload/a.txt",
            ".git/index",
            ".git/objects/ab/cdef",
            ".git/HEAD.lock",
            "projects/p/.git/index.lock",
            "projects/p/.git/HEAD",
        ] {
            assert_eq!(classify(&p(ignored)), Class::Ignored, "{ignored}");
        }
        assert_eq!(classify(&p(".git/index.lock")), Class::GitLock);
        assert_eq!(classify(&p(".GIT/head")), Class::GitHead);
        assert_eq!(classify(&p("projects/p/assets/a/work/x.psd")), Class::Path);
        assert_eq!(classify(&p("notes/.gitignore")), Class::Path);
    }

    #[tokio::test(start_paused = true)] // spec: ARCH-083
    async fn git_pause_ends_after_quiet() {
        let quiet = std::time::Duration::from_secs(2);
        let mut git = GitState::default();
        assert_eq!(git.quiet_at(quiet), None);
        let t0 = Instant::now();
        assert!(git.activity(t0), "the first activity starts a pause");
        assert!(!git.activity(t0), "later ones extend it");
        assert_eq!(git.quiet_at(quiet), Some(t0 + quiet));
        git.defer(p("notes/a.txt"));
        git.defer(p("NOTES/A.TXT"));
        git.overflowed = true;
        let (deferred, observed, overflowed) = git.finish();
        assert_eq!(deferred.len(), 1, "one path per key");
        assert!(observed.is_empty());
        assert!(overflowed);
        assert!(!git.busy);
        assert_eq!(git.quiet_at(quiet), None);
    }
}
