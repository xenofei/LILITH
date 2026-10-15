//! In-memory [`GitHubApi`] for tests. Script it with the setters, then pass `&fake` wherever a
//! `&dyn GitHubApi` is expected.

use super::github::{CheckState, CommitPull, GitHubApi, MergeOutcome, PrInfo, Review};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::sync::Mutex;

/// A review the code under test posted through `post_review_as_app`. The installation token is
/// deliberately not recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedReview {
    pub number: u64,
    pub sha: String,
    pub body: String,
}

#[derive(Default)]
struct State {
    prs: HashMap<u64, PrInfo>,
    /// (sha, check name) -> state
    checks: HashMap<(String, String), CheckState>,
    reviews: HashMap<u64, Vec<Review>>,
    not_mergeable: HashMap<u64, String>,
    posted: Vec<PostedReview>,
    /// commit sha -> the PRs GitHub associates with it
    commit_pulls: HashMap<String, Vec<CommitPull>>,
    /// commit sha -> the error `commit_pulls` answers with
    commit_pulls_errors: HashMap<String, String>,
}

#[derive(Default)]
pub struct FakeGitHub {
    state: Mutex<State>,
}

impl FakeGitHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("fake github state poisoned")
    }

    /// Adds an open, non-draft PR.
    pub fn add_pr(&self, number: u64, head_sha: &str, base_ref: &str) {
        self.lock().prs.insert(
            number,
            PrInfo {
                number,
                head_sha: head_sha.into(),
                base_ref: base_ref.into(),
                state: "open".into(),
                draft: false,
            },
        );
    }

    /// Moves the PR head (a push after the check or review was done).
    pub fn set_head(&self, number: u64, head_sha: &str) {
        self.lock()
            .prs
            .get_mut(&number)
            .expect("unknown PR")
            .head_sha = head_sha.into();
    }

    /// Sets GitHub's `state` (`open` or `closed`).
    pub fn set_state(&self, number: u64, state: &str) {
        self.lock().prs.get_mut(&number).expect("unknown PR").state = state.into();
    }

    pub fn set_draft(&self, number: u64, draft: bool) {
        self.lock().prs.get_mut(&number).expect("unknown PR").draft = draft;
    }

    pub fn set_check(&self, sha: &str, name: &str, state: CheckState) {
        self.lock().checks.insert((sha.into(), name.into()), state);
    }

    pub fn add_review(&self, number: u64, login: &str, state: &str, commit_id: &str) {
        self.lock().reviews.entry(number).or_default().push(Review {
            login: login.into(),
            state: state.into(),
            commit_id: commit_id.into(),
        });
    }

    /// Makes `merge` answer `NotMergeable(message)` (GitHub's 405) while the head still matches.
    pub fn set_not_mergeable(&self, number: u64, message: &str) {
        self.lock().not_mergeable.insert(number, message.into());
    }

    /// Associates PR `number` (with its final head and base) with commit `sha`.
    pub fn add_commit_pull(
        &self,
        sha: &str,
        number: u64,
        head_sha: &str,
        base_ref: &str,
        merged: bool,
    ) {
        self.lock()
            .commit_pulls
            .entry(sha.into())
            .or_default()
            .push(CommitPull {
                number,
                head_sha: head_sha.into(),
                base_ref: base_ref.into(),
                merged,
            });
    }

    /// Makes `commit_pulls(sha)` answer with an error.
    pub fn fail_commit_pulls(&self, sha: &str, message: &str) {
        self.lock()
            .commit_pulls_errors
            .insert(sha.into(), message.into());
    }

    pub fn posted_reviews(&self) -> Vec<PostedReview> {
        self.lock().posted.clone()
    }
}

impl GitHubApi for FakeGitHub {
    fn pr(&self, number: u64) -> Result<PrInfo> {
        self.lock()
            .prs
            .get(&number)
            .cloned()
            .ok_or_else(|| anyhow!("fake: no PR #{number}"))
    }

    fn check(&self, sha: &str, name: &str) -> Result<CheckState> {
        Ok(self
            .lock()
            .checks
            .get(&(sha.into(), name.into()))
            .cloned()
            .unwrap_or(CheckState::Missing))
    }

    fn reviews(&self, number: u64) -> Result<Vec<Review>> {
        Ok(self
            .lock()
            .reviews
            .get(&number)
            .cloned()
            .unwrap_or_default())
    }

    fn merge(&self, number: u64, expected_head_sha: &str) -> Result<MergeOutcome> {
        let mut st = self.lock();
        let head = st
            .prs
            .get(&number)
            .ok_or_else(|| anyhow!("fake: no PR #{number}"))?
            .head_sha
            .clone();
        let state = st.prs[&number].state.clone();
        if state != "open" {
            return Ok(MergeOutcome::NotMergeable(state));
        }
        if head != expected_head_sha {
            return Ok(MergeOutcome::HeadMoved);
        }
        if let Some(msg) = st.not_mergeable.get(&number) {
            return Ok(MergeOutcome::NotMergeable(msg.clone()));
        }
        st.prs.get_mut(&number).expect("checked above").state = "closed".into();
        Ok(MergeOutcome::Merged {
            sha: format!("merge-of-{head}"),
        })
    }

    fn post_review_as_app(
        &self,
        number: u64,
        sha: &str,
        body: &str,
        _installation_token: &str,
    ) -> Result<()> {
        let mut st = self.lock();
        if !st.prs.contains_key(&number) {
            return Err(anyhow!("fake: no PR #{number}"));
        }
        st.posted.push(PostedReview {
            number,
            sha: sha.into(),
            body: body.into(),
        });
        st.reviews.entry(number).or_default().push(Review {
            login: "worldwright-reviewer[bot]".into(),
            state: "APPROVED".into(),
            commit_id: sha.into(),
        });
        Ok(())
    }

    fn commit_pulls(&self, sha: &str) -> Result<Vec<CommitPull>> {
        let st = self.lock();
        if let Some(msg) = st.commit_pulls_errors.get(sha) {
            return Err(anyhow!("fake: {msg}"));
        }
        Ok(st.commit_pulls.get(sha).cloned().unwrap_or_default())
    }
}
