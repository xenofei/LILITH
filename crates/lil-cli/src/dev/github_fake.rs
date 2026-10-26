//! In-memory [`GitHubApi`] for tests. Script it with the setters, then pass `&fake` wherever a
//! `&dyn GitHubApi` is expected.

use super::github::{CheckState, CommitPull, GitHubApi, MergeOutcome, PrInfo, Review, WorkflowRun};
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
    /// (workflow file, run) pairs `workflow_runs_as_app` filters like GitHub does.
    workflow_runs: Vec<(String, WorkflowRun)>,
    /// (workflow, event, head_sha) of every `workflow_runs_as_app` call.
    run_queries: Vec<(String, String, String)>,
    workflow_runs_error: Option<String>,
    /// Run ids `rerun_as_app` accepted.
    reruns: Vec<u64>,
    rerun_error: Option<String>,
    /// The slug `app_slug` answers with; `None` means the App's slug, [`DEFAULT_APP_SLUG`].
    /// Reviews posted as the App carry `<slug>[bot]`.
    app_slug: Option<String>,
    /// The JWTs `app_slug` was asked with (test JWTs only).
    app_jwts: Vec<String>,
    app_slug_error: Option<String>,
    /// The error `dismiss_approvals_as_app` answers with.
    dismiss_error: Option<String>,
    /// The order of the App calls: `app`, `post_review`, `workflow_runs`, `rerun <id>`, plus
    /// anything a test adds with [`FakeGitHub::log_event`].
    events: Vec<String>,
}

/// The reviewer App's slug (`reviewer_app_slug`).
pub const DEFAULT_APP_SLUG: &str = "lilith-reviewer";

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
                merged: false,
                title: format!("PR {number}'s title"),
                author: "an-author".into(),
                head_ref: "feature".into(),
            },
        );
    }

    /// Sets the PR's title, author and head branch (shown in the merge flow's summary).
    pub fn set_pr_details(&self, number: u64, title: &str, author: &str, head_ref: &str) {
        if let Some(pr) = self.lock().prs.get_mut(&number) {
            pr.title = title.into();
            pr.author = author.into();
            pr.head_ref = head_ref.into();
        }
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

    /// Marks the PR merged and closed (as GitHub shows a merge, for example one whose merge
    /// call failed after GitHub had merged).
    pub fn set_merged(&self, number: u64) {
        let mut st = self.lock();
        let pr = st.prs.get_mut(&number).expect("unknown PR");
        pr.state = "closed".into();
        pr.merged = true;
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

    /// Makes `dismiss_approvals_as_app` fail with `message`.
    pub fn set_dismiss_error(&self, message: &str) {
        self.lock().dismiss_error = Some(message.into());
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

    /// Adds a run of `workflow` (a file name such as `ci-guard.yml`).
    pub fn add_workflow_run(&self, workflow: &str, run: WorkflowRun) {
        self.lock().workflow_runs.push((workflow.into(), run));
    }

    /// Changes run `id`'s status and conclusion (a run finishing while `lil dev approve` waits).
    pub fn set_run_status(&self, id: u64, status: &str, conclusion: Option<&str>) {
        for (_, run) in self
            .lock()
            .workflow_runs
            .iter_mut()
            .filter(|(_, r)| r.id == id)
        {
            run.status = status.into();
            run.conclusion = conclusion.map(str::to_owned);
        }
    }

    /// Makes `workflow_runs_as_app` answer with an error.
    pub fn fail_workflow_runs(&self, message: &str) {
        self.lock().workflow_runs_error = Some(message.into());
    }

    /// Makes `rerun_as_app` answer with an error (for example GitHub's 403).
    pub fn fail_rerun(&self, message: &str) {
        self.lock().rerun_error = Some(message.into());
    }

    /// Makes `app_slug` answer `slug`, and reviews posted as the App carry `<slug>[bot]` (the
    /// App after a rename).
    pub fn set_app_slug(&self, slug: &str) {
        self.lock().app_slug = Some(slug.into());
    }

    /// Makes `app_slug` answer with an error.
    pub fn fail_app_slug(&self, message: &str) {
        self.lock().app_slug_error = Some(message.into());
    }

    /// The JWTs `app_slug` was asked with, in order.
    pub fn app_jwts(&self) -> Vec<String> {
        self.lock().app_jwts.clone()
    }

    pub fn run_queries(&self) -> Vec<(String, String, String)> {
        self.lock().run_queries.clone()
    }

    pub fn reruns(&self) -> Vec<u64> {
        self.lock().reruns.clone()
    }

    /// Appends `event` to the call log ([`FakeGitHub::events`]).
    pub fn log_event(&self, event: &str) {
        self.lock().events.push(event.into());
    }

    pub fn events(&self) -> Vec<String> {
        self.lock().events.clone()
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
        let pr = st.prs.get_mut(&number).expect("checked above");
        pr.state = "closed".into();
        pr.merged = true;
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
        st.events.push("post_review".into());
        st.posted.push(PostedReview {
            number,
            sha: sha.into(),
            body: body.into(),
        });
        let login = format!(
            "{}[bot]",
            st.app_slug.as_deref().unwrap_or(DEFAULT_APP_SLUG)
        );
        st.reviews.entry(number).or_default().push(Review {
            login,
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

    fn workflow_runs_as_app(
        &self,
        workflow: &str,
        event: &str,
        head_sha: &str,
        _installation_token: &str,
    ) -> Result<Vec<WorkflowRun>> {
        let mut st = self.lock();
        st.events.push("workflow_runs".into());
        st.run_queries
            .push((workflow.into(), event.into(), head_sha.into()));
        if let Some(msg) = &st.workflow_runs_error {
            return Err(anyhow!("fake: {msg}"));
        }
        // Like GitHub: the runs of that workflow file for that event and commit, newest first.
        let mut runs: Vec<WorkflowRun> = st
            .workflow_runs
            .iter()
            .filter(|(w, r)| w == workflow && r.event == event && r.head_sha == head_sha)
            .map(|(_, r)| r.clone())
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.id));
        Ok(runs)
    }

    fn rerun_as_app(&self, run_id: u64, _installation_token: &str) -> Result<()> {
        let mut st = self.lock();
        st.events.push(format!("rerun {run_id}"));
        if let Some(msg) = &st.rerun_error {
            return Err(anyhow!("fake: {msg}"));
        }
        let Some((_, run)) = st.workflow_runs.iter_mut().find(|(_, r)| r.id == run_id) else {
            return Err(anyhow!("fake: HTTP 404: no workflow run {run_id}"));
        };
        // GitHub refuses to re-run a run that hasn't finished.
        if run.status != "completed" {
            return Err(anyhow!("fake: HTTP 403: This workflow is already running"));
        }
        run.status = "queued".into();
        run.conclusion = None;
        st.reruns.push(run_id);
        Ok(())
    }

    fn app_slug(&self, jwt: &str) -> Result<String> {
        let mut st = self.lock();
        st.events.push("app".into());
        st.app_jwts.push(jwt.into());
        if let Some(msg) = &st.app_slug_error {
            return Err(anyhow!("fake: {msg}"));
        }
        Ok(st
            .app_slug
            .clone()
            .unwrap_or_else(|| DEFAULT_APP_SLUG.into()))
    }

    fn dismiss_approvals_as_app(
        &self,
        number: u64,
        sha: &str,
        login: &str,
        _message: &str,
        _installation_token: &str,
    ) -> Result<usize> {
        let mut st = self.lock();
        st.events.push("dismiss".into());
        if let Some(msg) = &st.dismiss_error {
            return Err(anyhow!("fake: {msg}"));
        }
        let mut n = 0;
        for r in st.reviews.entry(number).or_default().iter_mut() {
            if r.login == login && r.commit_id == sha && r.state == "APPROVED" {
                r.state = "DISMISSED".into();
                n += 1;
            }
        }
        Ok(n)
    }
}
