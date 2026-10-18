//! The core's only `git` runner (CORE-040, ARCH-085): `workspace.doctor` reads and writes the
//! workspace repository's own config through [`Store::git`], so a repository-config write is
//! the store's, like every other workspace write (ARCH-002, the intent of ARCH-112).
//!
//! Each run is `run_hidden` (no console window, stdin empty, output captured) of the `git` on
//! `PATH`, in the workspace root, with the caller's git environment added (`CoreConfig.git_env`:
//! empty in production, the test profile in in-process tests, QA-038) and the variables that
//! would point `git` at another repository removed (`GIT_REPO_VARS`), for at most 30 s. A
//! timeout kills `git` itself, not processes it started.

use std::ffi::{OsStr, OsString};
use std::io;
use std::time::Duration;

use lil_platform::process::{Captured, GIT_REPO_VARS, find_on_path, run_hidden_without};

use super::write::{Store, StoreError, blocking};

/// How long one `git` run may take before it is killed.
pub const GIT_RUN_LIMIT: Duration = Duration::from_secs(30);

impl Store {
    /// Runs `git <args>` in the workspace root with `git_env` added to its environment, and
    /// returns what it printed and its exit code (a non-zero code is the caller's to read:
    /// `git config --get` exits 1 for an unset key).
    ///
    /// # Errors
    /// `store.io` when `git` isn't on `PATH`, can't be started, or runs past 30 s.
    pub async fn git(
        &self,
        args: &[&str],
        git_env: &[(OsString, OsString)],
    ) -> Result<Captured, StoreError> {
        let shown = format!("git {}", args.join(" "));
        let root = self.root().to_path_buf();
        let owned: Vec<OsString> = args.iter().map(OsString::from).collect();
        let env = git_env.to_vec();
        let run = blocking(move || {
            let git = find_on_path("git")
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git is not on PATH"))?;
            let args: Vec<&OsStr> = owned.iter().map(OsString::as_os_str).collect();
            run_hidden_without(&git, &args, &root, &env, GIT_REPO_VARS, GIT_RUN_LIMIT)
        })
        .await;
        let captured = run.map_err(|e| StoreError::Io {
            path: ".git".to_string(),
            why: format!("`{shown}` could not run: {e}"),
        })?;
        if captured.code.is_none() {
            tracing::warn!(command = %shown, "git ran past its time limit and was stopped");
            return Err(StoreError::Io {
                path: ".git".to_string(),
                why: format!(
                    "`{shown}` did not finish within {} s and was stopped",
                    GIT_RUN_LIMIT.as_secs()
                ),
            });
        }
        Ok(captured)
    }
}
