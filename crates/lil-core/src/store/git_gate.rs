//! The git gate (ARCH-083; Appendix E write step 2): store writes wait while git works on the
//! workspace, for up to [`GIT_WAIT`], then fail with `store.git_busy`.
//!
//! Git counts as busy while the watcher's flag is set ([`GitGate::set_busy`]: `index.lock`
//! appeared or `HEAD` moved, until git has been quiet and the post-git rescan has run) **or**
//! while `<ws>\.git\index.lock` exists. The gate checks that file itself with one
//! `GetFileAttributesW`, so a write never races git during the watcher's batch window, or when
//! no watcher runs at all (CORE-010: the store owns the gate, the watcher only sets its flag).
//!
//! **The git folder** is resolved once, when the gate is made (the workspace's open): `.git`
//! is usually a folder, but in a linked worktree, a submodule or a repository made with
//! `--separate-git-dir` it is a file holding `gitdir: <path>` (absolute, or relative to the
//! workspace folder). The gate then checks `<that folder>\index.lock`, and the watcher watches
//! that folder's `index.lock` and `HEAD` ([`GitGate::git_dir`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::write::StoreError;

/// How long a store write waits for git ("queues its writes for up to 60 s", ARCH-083).
pub const GIT_WAIT: Duration = Duration::from_secs(60);
/// How often a waiting write looks again (tokio time).
pub const GIT_POLL: Duration = Duration::from_millis(250);

/// Whether git is working on the workspace.
#[derive(Debug)]
pub struct GitGate {
    busy: AtomicBool,
    /// The repository's git folder ([`git_dir_of`]).
    dir: PathBuf,
    /// `<git folder>\index.lock`.
    lock: PathBuf,
}

/// The longest `.git` file read: a `gitdir:` line is one path.
const GITDIR_FILE_MAX: u64 = 64 * 1024;

/// The git folder of the workspace folder `root`: `<root>\.git`, unless that is a file whose
/// first `gitdir: <path>` line names another folder (a linked worktree, a submodule,
/// `--separate-git-dir`); a relative path is relative to `root`, and `.` and `..` are
/// resolved. A `.git` file that can't be read, or holds no such line, leaves `<root>\.git`
/// (logged).
pub fn git_dir_of(root: &Path) -> PathBuf {
    let dot = root.join(".git");
    let is_file = std::fs::symlink_metadata(lil_platform::fs::long_path(&dot))
        .is_ok_and(|m| m.is_file() && m.len() <= GITDIR_FILE_MAX);
    if !is_file {
        return dot;
    }
    let text = match std::fs::read_to_string(lil_platform::fs::long_path(&dot)) {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!(path = %dot.display(), error = %e, "the .git file can't be read; git is watched at .git");
            return dot;
        }
    };
    let Some(target) = text
        .lines()
        .find_map(|l| l.trim_start_matches('\u{feff}').strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        tracing::warn!(path = %dot.display(), "the .git file holds no gitdir line; git is watched at .git");
        return dot;
    };
    let named = PathBuf::from(target.replace('/', "\\"));
    let joined = if named.is_absolute() {
        named
    } else {
        root.join(named)
    };
    let mut resolved = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

impl GitGate {
    /// The gate of the workspace folder `root`, not busy, at the git folder [`git_dir_of`]
    /// resolves.
    pub fn new(root: &Path) -> GitGate {
        let dir = git_dir_of(root);
        GitGate {
            busy: AtomicBool::new(false),
            lock: dir.join("index.lock"),
            dir,
        }
    }

    /// The repository's git folder, whose `index.lock` the gate checks.
    pub fn git_dir(&self) -> &Path {
        &self.dir
    }

    /// The watcher's flag: set when git starts, cleared after the post-git rescan.
    pub fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
    }

    /// The watcher's flag is set, or `<git folder>\index.lock` exists (`fs::attributes`; any
    /// answer but "not found" counts as there, so an unreadable lock still holds writes back).
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst) || self.lock_exists()
    }

    /// Whether `<git folder>\index.lock` exists.
    pub fn lock_exists(&self) -> bool {
        match lil_platform::fs::attributes(&self.lock) {
            Ok(_) => true,
            Err(e) => !matches!(e.raw_os_error(), Some(2 | 3)),
        }
    }

    /// Returns once git isn't busy, looking every [`GIT_POLL`] (tokio time); with or without a
    /// watcher.
    ///
    /// # Errors
    /// [`StoreError::GitBusy`] when git is still busy after `budget`.
    pub async fn wait_clear(&self, budget: Duration) -> Result<(), StoreError> {
        if !self.is_busy() {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + budget;
        tracing::info!("git is working on the workspace; the write waits");
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(StoreError::GitBusy);
            }
            tokio::time::sleep(GIT_POLL.min(deadline - now)).await;
            if !self.is_busy() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lil_platform::fs::SystemProbe;
    use lil_testkit::files::write_file;
    use lil_testkit::{FakeClock, SeqIds, TestEnv};

    use super::*;
    use crate::state::{StateDb, StateScope};
    use crate::store::{Expect, FailPoints, Store, WsPath, fsops};

    #[tokio::test(start_paused = true)] // spec: ARCH-083
    async fn git_gate_waits_up_to_60_s_then_git_busy() {
        let env = TestEnv::new();
        let gate = Arc::new(GitGate::new(env.root()));
        assert!(!gate.is_busy());
        gate.wait_clear(GIT_WAIT)
            .await
            .expect("a free gate passes at once");

        gate.set_busy(true);
        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.wait_clear(GIT_WAIT).await })
        };
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert!(!waiting.is_finished(), "still waiting after 59 s");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(waiting.await.unwrap(), Err(StoreError::GitBusy));

        // Cleared within the budget: the wait ends then.
        let start = tokio::time::Instant::now();
        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.wait_clear(GIT_WAIT).await })
        };
        tokio::time::sleep(Duration::from_secs(5)).await;
        gate.set_busy(false);
        assert_eq!(waiting.await.unwrap(), Ok(()));
        assert!(start.elapsed() < Duration::from_secs(6));
    }

    #[test] // spec: ARCH-083
    fn a_gitdir_file_points_the_gate_at_the_real_git_folder() {
        // Minor 1 of the plan-2b review: a linked worktree, a submodule or
        // `--separate-git-dir` has a `.git` file, and git's `index.lock` is in the folder it
        // names.
        let env = TestEnv::new();
        let root = env.make_workspace("worktree");
        let real = env
            .root()
            .join("repo")
            .join(".git")
            .join("worktrees")
            .join("ws");
        write_file(&real.join("HEAD"), b"ref: refs/heads/main\n");
        write_file(
            &root.join(".git"),
            format!("gitdir: {}\n", real.display()).as_bytes(),
        );
        let gate = GitGate::new(&root);
        assert_eq!(gate.git_dir(), real);
        assert!(!gate.is_busy());
        write_file(&real.join("index.lock"), b"");
        assert!(gate.is_busy(), "the real index.lock holds writes back");
        // `<root>\.git\index.lock` can't exist (`.git` is a file), and the gate never looks
        // there.
        assert!(!root.join(".git").is_dir());

        // A relative gitdir (a submodule's `../.git/modules/<name>`), with `/` separators.
        let sub = env.root().join("super").join("lib");
        let modules = env
            .root()
            .join("super")
            .join(".git")
            .join("modules")
            .join("lib");
        write_file(&modules.join("HEAD"), b"ref: refs/heads/main\n");
        write_file(&sub.join(".git"), b"gitdir: ../.git/modules/lib\r\n");
        assert_eq!(git_dir_of(&sub), modules);

        // A plain `.git` folder, or none, is used as it is; so is a `.git` file without a
        // gitdir line.
        let plain = env.make_workspace("plain");
        assert_eq!(git_dir_of(&plain), plain.join(".git"));
        write_file(&plain.join(".git"), b"not a gitdir file\n");
        assert_eq!(git_dir_of(&plain), plain.join(".git"));
    }

    #[tokio::test(start_paused = true)] // spec: ARCH-083
    async fn git_gate_sees_index_lock_without_the_watcher() {
        let env = TestEnv::new();
        let root = env.make_workspace("gate");
        let state = Arc::new(
            StateDb::open(
                &env.root().join("local").join("state.db"),
                StateScope::Workspace,
                Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
            )
            .unwrap(),
        );
        let store = Arc::new(Store::new(
            root.clone(),
            state,
            FailPoints::none(),
            FakeClock::new(),
            Arc::new(SeqIds::new()),
            Arc::new(SystemProbe),
        ));
        let lock = root.join(".git").join("index.lock");
        write_file(&lock, b"");
        // No watcher sets the flag: the file alone holds writes back.
        assert!(store.git_gate().is_busy());
        let rel = WsPath::parse("notes/after.txt").unwrap();
        let waiting = {
            let (store, rel) = (store.clone(), rel.clone());
            tokio::spawn(async move { store.write(&rel, b"after".to_vec(), Expect::New).await })
        };
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert!(
            !waiting.is_finished(),
            "a write waits while index.lock exists"
        );
        assert!(!rel.abs(&root).exists(), "nothing is written meanwhile");
        // Git ends a command by renaming `index.lock` onto `index`.
        fsops::move_new(&lock, &root.join(".git").join("index")).unwrap();
        assert!(!store.git_gate().is_busy());
        let written = waiting
            .await
            .unwrap()
            .expect("the write goes on once the lock is gone");
        assert_eq!(written.path, rel);
        assert_eq!(std::fs::read(rel.abs(&root)).unwrap(), b"after");
    }
}
