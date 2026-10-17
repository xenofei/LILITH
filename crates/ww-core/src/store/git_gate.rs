//! The git gate (ARCH-083; Appendix E write step 2): store writes wait while git works on the
//! workspace, for up to [`GIT_WAIT`], then fail with `store.git_busy`.
//!
//! Git counts as busy while the watcher's flag is set ([`GitGate::set_busy`]: `index.lock`
//! appeared or `HEAD` moved, until git has been quiet and the post-git rescan has run) **or**
//! while `<ws>\.git\index.lock` exists. The gate checks that file itself with one
//! `GetFileAttributesW`, so a write never races git during the watcher's batch window, or when
//! no watcher runs at all (CORE-010: the store owns the gate, the watcher only sets its flag).

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
    /// `<ws>\.git\index.lock`.
    lock: PathBuf,
}

impl GitGate {
    /// The gate of the workspace folder `root`, not busy.
    pub fn new(root: &Path) -> GitGate {
        GitGate {
            busy: AtomicBool::new(false),
            lock: root.join(".git").join("index.lock"),
        }
    }

    /// The watcher's flag: set when git starts, cleared after the post-git rescan.
    pub fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
    }

    /// The watcher's flag is set, or `<ws>\.git\index.lock` exists (`fs::attributes`; any
    /// answer but "not found" counts as there, so an unreadable lock still holds writes back).
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst) || self.lock_exists()
    }

    /// Whether `<ws>\.git\index.lock` exists.
    pub fn lock_exists(&self) -> bool {
        match ww_platform::fs::attributes(&self.lock) {
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

    use ww_platform::fs::SystemProbe;
    use ww_testkit::files::write_file;
    use ww_testkit::{FakeClock, SeqIds, TestEnv};

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
