//! `lil dev guard` and `lil dev merge` (QA-021, QA-033, QA-034).
//!
//! [`merge_conditions`] is the decision. It takes the GitHub client and the guard verdict as
//! parameters, so it is tested with the fake and an injected verdict, without git or network.
//! [`run_guard`] and [`run_merge`] are the thin CLI wiring around it.
//!
//! **Settings** (`dev/merge-guard.toml`) are only ever read as committed on `origin/main`, never
//! from the working tree (ruling R12). [`preflight`] reads them from the local `origin/main`
//! remote-tracking ref for the hooks check, before any network call; [`merge_in_repo`] fetches
//! `origin/main` and re-reads them, and those fresh settings supply the required checks, the
//! reviewer slug, the owner, the repo and the base branch.
//!
//! **Origin** must be `github.com/<owner>/<repo>` from the committed settings before anything is
//! fetched (ruling R13, [`origin_matches`]).
//!
//! **Fail closed.** Every error (GitHub, git, the guard verdict, the settings) is a refusal with
//! a plain-language reason. An error is never read as success, `Missing` or "still pending".
//!
//! **The base the guard judges against** is `origin/<base_branch>` (`origin/main`), fetched from
//! GitHub just before the settings are re-read; the PR's head (`refs/pull/<n>/head`) is fetched
//! immediately before the verdict.
//! That ref is the tip of the PR's base branch as GitHub sees it, which is what the merge is made
//! onto; the local `main` can be stale or carry unpushed commits. The PR's `base_ref` must equal
//! `base_branch` (otherwise the merge is refused), so `origin/<base_ref>` and
//! `origin/<base_branch>` are the same ref. The guard diffs `base...head` (from the merge-base),
//! so later commits on `main` are not counted as the PR's changes.
//!
//! **Approval** counts only when the latest review that the reviewer App left on the checked
//! head commit is `APPROVED`. The App's login is `<reviewer_app_slug>[bot]`
//! ([`MergeGuardConfig::reviewer_login`]). GitHub lists reviews oldest first, so the latest is
//! the last one in the list; a later `DISMISSED`, `CHANGES_REQUESTED`, `COMMENTED` or `PENDING`
//! review by the App on that commit cancels an earlier approval. No other login counts, a
//! person's included.
//!
//! **Your authentication** (QA-036 as amended 2026-10-23 and 2026-10-26, P4a-R38): `lil dev
//! merge` and `lil dev merge-flow` are the same flow (`merge_flow.rs`): they read the PR, ask you
//! (Windows Hello when enrolled, then the reviewer key's passphrase), post the reviewer App's
//! approval of the head you were shown, wait for CI and only then call [`merge_pr`]. Once every
//! other condition held, [`merge_pr`] asks its [`MergeGate`] last, just before GitHub is asked
//! to merge; the flow's gate lets through only the PR and head you authenticated for. A refusal
//! merges nothing.
//!
//! **The protected-test check on a PR** ([`run_guard`] with `--pr`, CI's `ci-guard.yml`; owner
//! decision 2026-10-26): it passes only when the reviewer App approved the PR's head commit,
//! whether or not protected files changed ([`pr_approval`]). The ruleset on `main` requires that
//! check, so GitHub refuses a merge you didn't authenticate.

use super::agent_access::GIT_CREDENTIAL_HELPER;
use super::github::{
    BASE_BRANCH, CheckState, GitHubApi, MergeOutcome, RestClient, Review, github_token,
};
use super::guard::{self, Verdict};
use super::merge_flow::{MergeGate, MergeSubject};
use super::root;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

/// Repo-relative path of the merge-guard settings.
pub const CONFIG_PATH: &str = "dev/merge-guard.toml";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeGuardConfig {
    pub owner: String,
    pub repo: String,
    pub base_branch: String,
    pub reviewer_app_slug: String,
    pub required_checks: Vec<String>,
    pub hooks_path: String,
}

/// Letters, digits and `-._/`, not starting with `-` or `/` and without `..`: safe in a
/// refspec, a URL path and a git argument.
fn is_plain_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(['-', '/'])
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '/'))
}

impl MergeGuardConfig {
    /// Parses and validates the settings. Unknown keys, empty values, an empty check list and
    /// names that are unsafe in git or URL arguments are errors, so a damaged file cannot
    /// weaken the guard.
    pub fn parse(text: &str) -> Result<Self> {
        let cfg: MergeGuardConfig =
            toml::from_str(text).context("parsing the merge-guard settings")?;
        for (key, value) in [
            ("owner", &cfg.owner),
            ("repo", &cfg.repo),
            ("base_branch", &cfg.base_branch),
            ("reviewer_app_slug", &cfg.reviewer_app_slug),
            ("hooks_path", &cfg.hooks_path),
        ] {
            if !is_plain_name(value) {
                bail!("merge-guard settings: `{key}` = {value:?} is empty or not a plain name");
            }
        }
        if cfg.required_checks.is_empty() {
            bail!(
                "merge-guard settings: `required_checks` is empty; a merge needs at least one CI check"
            );
        }
        if let Some(bad) = cfg.required_checks.iter().find(|c| c.trim().is_empty()) {
            bail!("merge-guard settings: `required_checks` has an empty name {bad:?}");
        }
        Ok(cfg)
    }

    /// The login GitHub shows on the reviewer App's reviews: `<reviewer_app_slug>[bot]`. A
    /// person's login never ends in `[bot]`.
    pub fn reviewer_login(&self) -> String {
        format!("{}[bot]", self.reviewer_app_slug)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `core.hooksPath` is not `expected`; `found` is what git reported (`None` when unset).
    HooksNotInstalled {
        expected: String,
        found: Option<String>,
    },
    /// The repository's local `credential.helper` entries aren't exactly the reset and the
    /// `lil dev git-credential` helper, or another helper entry comes after them. `found` is
    /// the local entries; `extra_keys` names the helper keys read after the reset (their
    /// values aren't shown).
    CredentialHelperNotInstalled {
        found: Vec<String>,
        extra_keys: Vec<String>,
    },
    /// `state` is `missing`, `pending` or GitHub's conclusion (`failure`, `cancelled`, ...).
    CheckNotGreen {
        name: String,
        state: String,
        sha: String,
    },
    /// One plain-language line per guard blocker.
    ProtectedNeedsApproval(Vec<String>),
    PrNotOpen {
        state: String,
    },
    PrIsDraft,
    WrongBase {
        expected: String,
        found: String,
    },
    HeadMoved,
    NotMergeable(String),
    /// Something could not be checked (a GitHub, git or settings error).
    CouldNotCheck(String),
    /// The merge call itself errored, so whether GitHub merged is unknown.
    MergeCallFailed(String),
    /// `origin` is not the GitHub repo named in the committed settings (ruling R13).
    WrongOrigin {
        url: String,
        expected: String,
    },
    /// Your authentication for the merge didn't succeed (QA-036, P4a-R38).
    NotAuthenticated(String),
}

/// `true` when the latest review on commit `sha` by `login` (compared exactly) is `APPROVED`.
pub fn app_approved(reviews: &[Review], login: &str, sha: &str) -> bool {
    reviews
        .iter()
        .rfind(|r| r.commit_id == sha && r.login == login)
        .is_some_and(|r| r.state == "APPROVED")
}

pub(crate) fn is_commit_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Collapses whitespace, including newlines in error chains, onto one line.
pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn could_not(what: &str, e: &anyhow::Error) -> Refusal {
    Refusal::CouldNotCheck(one_line(&format!("{what}: {e:#}")))
}

pub(crate) fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// The blocker lines of a verdict, without `render_plain`'s "Needs your approval: " prefix.
pub(crate) fn blocker_lines(v: &Verdict) -> Vec<String> {
    let only_blockers = Verdict {
        needs_approval: v.needs_approval.clone(),
        warnings: Vec::new(),
    };
    guard::render_plain(&only_blockers)
        .lines()
        .map(|l| {
            l.strip_prefix("Needs your approval: ")
                .unwrap_or(l)
                .to_string()
        })
        .collect()
}

/// `Some` refusal unless `core.hooksPath` is exactly `cfg.hooks_path` (surrounding whitespace
/// ignored).
fn hooks_refusal(cfg: &MergeGuardConfig, found: Option<&str>) -> Option<Refusal> {
    (found.map(str::trim) != Some(cfg.hooks_path.as_str())).then(|| Refusal::HooksNotInstalled {
        expected: cfg.hooks_path.clone(),
        found: found.map(str::to_string),
    })
}

/// `Some` refusal unless the repository's local `credential.helper` entries are exactly an empty
/// entry (which drops inherited helpers such as Git Credential Manager) and then
/// [`GIT_CREDENTIAL_HELPER`], and no helper entry git reads comes after them.
///
/// `local` is `git config --local --get-all credential.helper`; `effective` is every
/// `credential.*helper` entry git reads, URL-specific ones included, as `(key, value)` in the
/// order git applies them.
pub(crate) fn credential_helper_refusal(
    local: &[String],
    effective: &[(String, String)],
) -> Option<Refusal> {
    let want = ["", GIT_CREDENTIAL_HELPER];
    let local_ok = local.iter().map(String::as_str).eq(want);
    let tail_ok = effective.len() >= 2
        && effective[effective.len() - 2..]
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .eq(want.map(|v| ("credential.helper", v)));
    if local_ok && tail_ok {
        return None;
    }
    // The helper entries read after the last reset, other than ours.
    let extra_keys = effective
        .iter()
        .rposition(|(k, v)| k == "credential.helper" && v.is_empty())
        .map(|reset| {
            effective[reset + 1..]
                .iter()
                .filter(|(k, v)| !(k == "credential.helper" && v == GIT_CREDENTIAL_HELPER))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    Some(Refusal::CredentialHelperNotInstalled {
        found: local.to_vec(),
        extra_keys,
    })
}

/// Decides whether PR `pr` may be merged. On success it returns the head SHA every condition
/// was checked against; the merge must be pinned to it.
///
/// Order: the hooks (local, before any GitHub call); then the PR (open, not a draft, into
/// `base_branch`); then every required check on the head SHA; then the guard verdict for
/// `origin/<base_branch>`...head, with the reviewer App's approval of that SHA when protected
/// files changed. Hook and PR problems stop at once; the rest are all collected and reported.
pub fn merge_conditions(
    cfg: &MergeGuardConfig,
    gh: &dyn GitHubApi,
    pr: u64,
    verdict_for: &dyn Fn(&str, &str) -> Result<guard::Verdict>,
    hooks_path_configured: Option<String>,
) -> Result<String, Vec<Refusal>> {
    if let Some(r) = hooks_refusal(cfg, hooks_path_configured.as_deref()) {
        return Err(vec![r]);
    }

    let info = gh.pr(pr).map_err(|e| {
        vec![could_not(
            &format!("reading PR #{pr} from GitHub failed"),
            &e,
        )]
    })?;
    if info.state != "open" {
        return Err(vec![Refusal::PrNotOpen { state: info.state }]);
    }
    if info.draft {
        return Err(vec![Refusal::PrIsDraft]);
    }
    if info.base_ref != cfg.base_branch {
        return Err(vec![Refusal::WrongBase {
            expected: cfg.base_branch.clone(),
            found: info.base_ref,
        }]);
    }
    let head = info.head_sha;
    if !is_commit_sha(&head) {
        return Err(vec![Refusal::CouldNotCheck(format!(
            "GitHub reported a head commit for PR #{pr} that is not a commit SHA: {head:?}"
        ))]);
    }

    let mut refusals = Vec::new();
    for name in &cfg.required_checks {
        let state = match gh.check(&head, name) {
            Ok(CheckState::Success) => continue,
            Ok(CheckState::Pending | CheckState::Rerunning) => "pending".to_string(),
            Ok(CheckState::Missing) => "missing".to_string(),
            Ok(CheckState::Failure(conclusion)) => conclusion,
            Err(e) => {
                refusals.push(could_not(
                    &format!("reading CI check {name} on {} failed", short(&head)),
                    &e,
                ));
                continue;
            }
        };
        refusals.push(Refusal::CheckNotGreen {
            name: name.clone(),
            state,
            sha: head.clone(),
        });
    }

    // The full ref: a local branch named `origin/main` must not stand in for it.
    let base = remote_base_ref(&cfg.base_branch);
    match verdict_for(&base, &head) {
        Err(e) => refusals.push(could_not(
            &format!(
                "the test-diff guard could not judge {} against {base}",
                short(&head)
            ),
            &e,
        )),
        Ok(v) if !v.needs_approval.is_empty() => match gh.reviews(pr) {
            Err(e) => refusals.push(could_not(
                &format!("reading the reviews of PR #{pr} failed"),
                &e,
            )),
            Ok(reviews) if app_approved(&reviews, &cfg.reviewer_login(), &head) => {}
            Ok(_) => refusals.push(Refusal::ProtectedNeedsApproval(blocker_lines(&v))),
        },
        Ok(_) => {}
    }

    if refusals.is_empty() {
        Ok(head)
    } else {
        Err(refusals)
    }
}

/// Checks [`merge_conditions`], then asks `gate` (the flow's: only the PR and head you
/// authenticated for; QA-036, P4a-R38), then asks GitHub to merge pinned to the checked SHA.
/// Returns the merge commit's SHA.
pub fn merge_pr(
    cfg: &MergeGuardConfig,
    gh: &dyn GitHubApi,
    pr: u64,
    verdict_for: &dyn Fn(&str, &str) -> Result<Verdict>,
    hooks_path_configured: Option<String>,
    gate: &dyn MergeGate,
) -> Result<String, Vec<Refusal>> {
    let head = merge_conditions(cfg, gh, pr, verdict_for, hooks_path_configured)?;
    // What the gate shows of the PR (re-review m5): its title and author, escaped, from a read
    // at the checked head; nothing is decided on them.
    let (title, author) = match gh.pr(pr) {
        Ok(info) if info.head_sha == head => (
            guard::escape_controls(&info.title),
            guard::escape_controls(&info.author),
        ),
        _ => (String::new(), String::new()),
    };
    let subject = MergeSubject {
        pr,
        head: &head,
        title: &title,
        author: &author,
        base_branch: &cfg.base_branch,
    };
    gate.authorize(&subject)
        .map_err(|e| vec![Refusal::NotAuthenticated(one_line(&e))])?;
    match gh.merge(pr, &head) {
        Ok(MergeOutcome::Merged { sha }) => Ok(sha),
        Ok(MergeOutcome::HeadMoved) => Err(vec![Refusal::HeadMoved]),
        Ok(MergeOutcome::NotMergeable(message)) => Err(vec![Refusal::NotMergeable(message)]),
        Err(e) => Err(vec![Refusal::MergeCallFailed(one_line(&format!("{e:#}")))]),
    }
}

fn explain_one(r: &Refusal) -> String {
    match r {
        Refusal::HooksNotInstalled { expected, found } => match found {
            None => format!(
                "The git hooks aren't installed: core.hooksPath isn't set. Run `lil dev install-hooks` (it sets `git config core.hooksPath {expected}`), then try again."
            ),
            Some(f) => format!(
                "The git hooks aren't installed: core.hooksPath is {f:?}, not {expected}. Run `lil dev install-hooks` (it sets `git config core.hooksPath {expected}`), then try again."
            ),
        },
        Refusal::CredentialHelperNotInstalled { found, extra_keys } => {
            let extra = if extra_keys.is_empty() {
                String::new()
            } else {
                format!(
                    " Other helper entries are read after them: {}.",
                    extra_keys.join(", ")
                )
            };
            format!(
                "The agent credential helper isn't installed: this repository's local credential.helper entries must be an empty entry and then `{GIT_CREDENTIAL_HELPER}`, with no other helper after them (local entries found: {found:?}).{extra} Run `lil dev install-hooks`, then try again."
            )
        }
        Refusal::CheckNotGreen { name, state, sha } => match state.as_str() {
            "missing" => format!(
                "CI check {name} hasn't reported for {} (the PR's head commit) yet.",
                short(sha)
            ),
            "pending" => format!(
                "CI check {name} is still running on {}; wait for it to finish.",
                short(sha)
            ),
            other => format!(
                "CI check {name} finished as {other}, not success, on {}.",
                short(sha)
            ),
        },
        Refusal::ProtectedNeedsApproval(lines) => format!(
            "Protected files changed and the reviewer app hasn't approved this exact commit: {}",
            lines.join(" ")
        ),
        Refusal::PrNotOpen { state } => {
            format!("The PR is {state}, not open, so there is nothing to merge.")
        }
        Refusal::PrIsDraft => {
            "The PR is a draft. Mark it ready for review, then try again.".to_string()
        }
        Refusal::WrongBase { expected, found } => format!(
            "The PR targets {found}, not {expected}. lil dev merge only merges into {expected}."
        ),
        Refusal::HeadMoved => "The PR's head moved after the checks passed, so GitHub refused the pinned merge. Nothing was merged; run lil dev merge again to check the new commit.".to_string(),
        Refusal::NotMergeable(message) => format!("GitHub won't merge the PR: {message}"),
        Refusal::CouldNotCheck(message) => {
            format!("Couldn't finish the checks, so the merge is refused: {message}")
        }
        Refusal::WrongOrigin { url, expected } => format!(
            "origin is {}, expected {expected}. Point origin at that GitHub repo, then try again.",
            redact_url(url)
        ),
        Refusal::NotAuthenticated(message) => format!("Not authenticated: {message}"),
        Refusal::MergeCallFailed(message) => format!(
            "The merge request to GitHub failed ({message}). It may or may not have merged: look at the PR on GitHub before trying again."
        ),
    }
}

/// Plain language, one line per refusal, with control and invisible characters escaped.
pub fn explain(refusals: &[Refusal]) -> String {
    refusals
        .iter()
        .map(|r| guard::escape_controls(&one_line(&explain_one(r))))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------------------------
// CLI wiring. `preflight`, `check_origin` and `merge_in_repo` are tested against temp repos with
// a local bare origin and the fake GitHub. `run_merge` and `run_guard` call GitHub for real and
// are exercised live in task 15.
// ---------------------------------------------------------------------------------------------

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    root::git_command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("could not run git")
}

/// `git config core.hooksPath`: `Ok(None)` when unset, an error when git itself fails.
fn configured_hooks_path(repo: &Path) -> Result<Option<String>> {
    let out = git(repo, &["config", "--get", "core.hooksPath"])?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => bail!(
            "git config core.hooksPath failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// Runs `git config` and returns its stdout, or `None` when git reports no match (exit 1).
fn git_config_out(repo: &Path, args: &[&str]) -> Result<Option<Vec<u8>>> {
    let out = git(repo, args)?;
    match out.status.code() {
        Some(0) => Ok(Some(out.stdout)),
        Some(1) => Ok(None),
        _ => bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// `git config --local --get-all credential.helper`, in order. Read with `--null`, so an empty
/// entry shows as one.
pub(crate) fn local_credential_helpers(repo: &Path) -> Result<Vec<String>> {
    let out = git_config_out(
        repo,
        &[
            "config",
            "--local",
            "--null",
            "--get-all",
            "credential.helper",
        ],
    )?;
    Ok(String::from_utf8_lossy(&out.unwrap_or_default())
        .split_terminator('\0')
        .map(str::to_string)
        .collect())
}

/// Every `credential.*helper` entry git reads (all scopes, URL-specific keys included), as
/// `(key, value)` in config order.
pub(crate) fn effective_credential_helpers(repo: &Path) -> Result<Vec<(String, String)>> {
    let out = git_config_out(
        repo,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^credential\..*helper$",
        ],
    )?;
    Ok(String::from_utf8_lossy(&out.unwrap_or_default())
        .split_terminator('\0')
        .map(|entry| match entry.split_once('\n') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (entry.to_string(), String::new()),
        })
        .collect())
}

/// `refs/remotes/origin/<branch>`, the remote-tracking ref, always spelled out in full: git's
/// short name `origin/<branch>` resolves to a local branch `refs/heads/origin/<branch>` first.
pub(crate) fn remote_base_ref(branch: &str) -> String {
    format!("refs/remotes/origin/{branch}")
}

/// `refs/remotes/origin/main`: the only place the merge-guard settings are read from.
pub(crate) fn settings_ref() -> String {
    remote_base_ref(BASE_BRANCH)
}

/// The merge-guard settings as committed on `origin/main`, read from the local remote-tracking
/// ref (no fetch happens here). The working tree is never read, so an uncommitted edit cannot
/// drop a check, swap the reviewer or move the hooks path (ruling R12).
pub(crate) fn committed_config(repo: &Path) -> Result<MergeGuardConfig, Refusal> {
    let unreadable = |detail: &str| {
        Refusal::CouldNotCheck(one_line(&format!(
            "can't read {CONFIG_PATH} from origin/{BASE_BRANCH}; run `git fetch origin` ({detail})"
        )))
    };
    let text = match guard::file_at(repo, &settings_ref(), CONFIG_PATH) {
        Ok(Some(text)) => text,
        Ok(None) => return Err(unreadable("the file isn't committed there")),
        Err(e) => return Err(unreadable(&format!("{e:#}"))),
    };
    let cfg = MergeGuardConfig::parse(&text).map_err(|e| {
        could_not(
            &format!("{CONFIG_PATH} on origin/{BASE_BRANCH} is unusable"),
            &e,
        )
    })?;
    if cfg.base_branch != BASE_BRANCH {
        return Err(Refusal::CouldNotCheck(format!(
            "{CONFIG_PATH} on origin/{BASE_BRANCH} names the base branch {:?}; lil dev merge only merges into {BASE_BRANCH}",
            cfg.base_branch
        )));
    }
    Ok(cfg)
}

/// The local steps, before any network call: the committed settings, then the hooks (QA-034),
/// then the agent credential helper (QA-079).
/// Returns the settings and the configured hooks path.
pub(crate) fn preflight(repo: &Path) -> Result<(MergeGuardConfig, Option<String>), Vec<Refusal>> {
    let cfg = committed_config(repo).map_err(|r| vec![r])?;
    let hooks = configured_hooks_path(repo)
        .map_err(|e| vec![could_not("reading core.hooksPath failed", &e)])?;
    if let Some(r) = hooks_refusal(&cfg, hooks.as_deref()) {
        return Err(vec![r]);
    }
    let local = local_credential_helpers(repo)
        .map_err(|e| vec![could_not("reading credential.helper failed", &e)])?;
    let effective = effective_credential_helpers(repo)
        .map_err(|e| vec![could_not("reading the credential helpers failed", &e)])?;
    if let Some(r) = credential_helper_refusal(&local, &effective) {
        return Err(vec![r]);
    }
    Ok((cfg, hooks))
}

/// Whether `url` names `github.com/<owner>/<repo>` (ruling R13). Accepted forms are
/// `https://github.com/O/R`, `git@github.com:O/R` and `ssh://git@github.com/O/R`, each with an
/// optional `.git` and an optional trailing `/`. The scheme, host, owner and repo compare
/// case-insensitively. Anything else, including local paths and URLs with credentials, is no
/// match.
pub fn origin_matches(url: &str, owner: &str, repo: &str) -> bool {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let Some(path) = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|prefix| lower.starts_with(prefix).then(|| &url[prefix.len()..])) else {
        return false;
    };
    let path = path.strip_suffix('/').unwrap_or(path);
    let path = path.strip_suffix(".git").unwrap_or(path);
    let Some((o, r)) = path.split_once('/') else {
        return false;
    };
    !o.is_empty()
        && !r.is_empty()
        && !r.contains('/')
        && o.eq_ignore_ascii_case(owner)
        && r.eq_ignore_ascii_case(repo)
}

/// Replaces any `user:password@` in a `scheme://` URL with `***@`, for messages.
fn redact_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        if let Some((_, host)) = authority.rsplit_once('@') {
            return format!("{scheme}://***@{host}{path}");
        }
    }
    url.to_string()
}

/// Refuses unless `origin` is the GitHub repo the committed settings name.
pub(crate) fn check_origin(repo: &Path, cfg: &MergeGuardConfig) -> Result<(), Refusal> {
    let out = git(repo, &["remote", "get-url", "origin"])
        .map_err(|e| could_not("reading origin's URL failed", &e))?;
    if !out.status.success() {
        return Err(Refusal::CouldNotCheck(one_line(&format!(
            "there is no usable `origin` remote: {}",
            String::from_utf8_lossy(&out.stderr)
        ))));
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if origin_matches(&url, &cfg.owner, &cfg.repo) {
        Ok(())
    } else {
        Err(Refusal::WrongOrigin {
            url,
            expected: format!("github.com/{}/{}", cfg.owner, cfg.repo),
        })
    }
}

/// `git fetch --no-tags --quiet origin <refspecs>`.
pub(crate) fn fetch(repo: &Path, refspecs: &[&str]) -> Result<()> {
    let mut args = vec!["fetch", "--no-tags", "--quiet", "origin"];
    args.extend_from_slice(refspecs);
    let out = git(repo, &args)?;
    if !out.status.success() {
        bail!(
            "git fetch from origin failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Everything after [`preflight`] and the origin check: fetches `origin/main`, re-reads the
/// committed settings from it, builds the GitHub client from those settings with `github`, and
/// runs [`merge_pr`] with `gate`. The verdict closure fetches `refs/pull/<pr>/head` and judges
/// `origin/main...<head sha>`.
///
/// `checked` is the settings `origin` was verified against; fresh settings naming a different
/// owner or repo are refused.
pub(crate) fn merge_in_repo<'g>(
    repo: &Path,
    pr: u64,
    checked: &MergeGuardConfig,
    hooks_path_configured: Option<String>,
    github: &dyn Fn(&MergeGuardConfig) -> Result<Box<dyn GitHubApi + 'g>>,
    gate: &dyn MergeGate,
) -> Result<String, Vec<Refusal>> {
    let base_spec = format!("+refs/heads/{BASE_BRANCH}:{}", settings_ref());
    fetch(repo, &[&base_spec]).map_err(|e| {
        vec![could_not(
            &format!("fetching origin/{BASE_BRANCH} failed"),
            &e,
        )]
    })?;
    let cfg = committed_config(repo).map_err(|r| vec![r])?;
    if !cfg.owner.eq_ignore_ascii_case(&checked.owner)
        || !cfg.repo.eq_ignore_ascii_case(&checked.repo)
    {
        return Err(vec![Refusal::CouldNotCheck(format!(
            "the freshly fetched {CONFIG_PATH} names github.com/{}/{}, but origin was checked against github.com/{}/{}",
            cfg.owner, cfg.repo, checked.owner, checked.repo
        ))]);
    }
    let gh = github(&cfg).map_err(|e| vec![could_not("no GitHub access", &e)])?;
    let pr_spec = format!("refs/pull/{pr}/head");
    let verdict_for = |base: &str, head: &str| -> Result<Verdict> {
        fetch(repo, &[&pr_spec])?;
        guard::verdict(repo, base, head)
    };
    merge_pr(
        &cfg,
        gh.as_ref(),
        pr,
        &verdict_for,
        hooks_path_configured,
        gate,
    )
}

/// `lil dev merge <pr> [--passphrase]`: exit 0 when merged, 1 when refused. It is the merge flow
/// without an agent's summary or audit (`reviewer::run_merge_alone`): it reads the PR, asks for
/// your authentication, posts the reviewer App's approval of that head, waits for CI and merges
/// that head (QA-036). With `passphrase_only`, Windows Hello is skipped and the passphrase is
/// asked at the terminal.
pub fn run_merge(pr: u64, passphrase_only: bool) -> i32 {
    super::reviewer::run_merge_alone(pr, passphrase_only)
}

/// CI's protected-test check on PR `pr` (`lil dev guard --pr`; QA-021 as amended, owner decision
/// 2026-10-26): it passes only when the reviewer App's latest review of `head` on that PR is an
/// approval, whether or not protected files changed. An approval of an older head, or by any
/// other login, doesn't count; neither does one that couldn't be read. `Ok` is the line printed
/// when it passes, `Err` the line when it fails.
pub(crate) fn pr_approval(
    gh: &dyn GitHubApi,
    cfg: &MergeGuardConfig,
    pr: u64,
    head: &str,
) -> std::result::Result<String, String> {
    let login = cfg.reviewer_login();
    match gh.reviews(pr) {
        Ok(reviews) if app_approved(&reviews, &login, head) => Ok(format!(
            "Approved: {login} approved {} on PR #{pr}.",
            short(head)
        )),
        Ok(_) => Err(format!(
            "Not approved: {login} hasn't approved {} on PR #{pr}. Every merge into {} needs that approval; `lil dev merge` and `lil dev merge-flow` post it after the owner's authentication.",
            short(head),
            cfg.base_branch
        )),
        Err(e) => Err(format!(
            "lil dev guard: couldn't read the approval, so it doesn't count: {}",
            one_line(&format!("{e:#}"))
        )),
    }
}

/// `lil dev guard --base <ref> --head <ref> [--pr <n>]`: prints the verdict. Without `--pr`: exit
/// 0 when nothing needs approval, 1 on blockers. With `--pr` (CI's protected-test check): exit 0
/// only when the reviewer App approved the head commit on that PR, protected files or not
/// ([`pr_approval`]), else 1 (including when the approval could not be read). 2 when the guard
/// could not run.
pub fn run_guard(base: &str, head: &str, pr: Option<u64>) -> i32 {
    let repo = match root::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lil dev guard: the guard could not run: {e}");
            return 2;
        }
    };
    // Pin both sides once: the verdict and the approval are judged on these exact commits.
    let judged = guard::resolve_commit(&repo, base).and_then(|b| {
        let h = guard::resolve_commit(&repo, head)?;
        Ok((guard::verdict(&repo, &b, &h)?, h))
    });
    let (v, head_sha) = match judged {
        Ok(x) => x,
        Err(e) => {
            eprintln!(
                "lil dev guard: the guard could not run: {}",
                one_line(&format!("{e:#}"))
            );
            return 2;
        }
    };
    println!("{}", guard::render_plain(&v));
    let approval = |pr: u64| -> std::result::Result<String, String> {
        let unread = |why: String| {
            format!("lil dev guard: couldn't read the approval, so it doesn't count: {why}")
        };
        // The reviewer slug comes from the committed settings, never the working tree (R12).
        let cfg = committed_config(&repo).map_err(|r| unread(explain(&[r])))?;
        let client = github_token()
            .and_then(|token| RestClient::new(&cfg.owner, &cfg.repo, &token))
            .map_err(|e| unread(one_line(&format!("{e:#}"))))?;
        pr_approval(&client, &cfg, pr, &head_sha)
    };
    let (code, line) = guard_exit(&v, pr, &approval);
    match line {
        Some(line) if code == 0 => println!("{line}"),
        Some(line) => eprintln!("{line}"),
        None => {}
    }
    code
}

/// `lil dev guard`'s exit code once the verdict is known, and the line about the approval when
/// one was read. Without a PR: 0 when nothing needs approval, 1 otherwise. With a PR (CI's
/// protected-test-check; owner decision 2026-10-26): whatever the verdict, 0 only when
/// `approval` (the reviewer App's approval of the PR's head, [`pr_approval`]) passes, else 1.
pub(crate) fn guard_exit(
    v: &Verdict,
    pr: Option<u64>,
    approval: &dyn Fn(u64) -> std::result::Result<String, String>,
) -> (i32, Option<String>) {
    let Some(pr) = pr else {
        return (i32::from(!v.needs_approval.is_empty()), None);
    };
    match approval(pr) {
        Ok(line) => (0, Some(line)),
        Err(line) => (1, Some(line)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::github::{CheckState, MergeOutcome, PrInfo};
    use crate::dev::github_fake::FakeGitHub;
    use crate::dev::guard::{Blocker, Warning};
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::process::Command;

    const PR: u64 = 42;
    const HEAD: &str = "1234567890abcdef1234567890abcdef12345678";
    const OLD: &str = "fedcba0987654321fedcba0987654321fedcba09";
    const NEW: &str = "0000000000000000000000000000000000000abc";
    const BOT: &str = "lilith-reviewer[bot]";

    fn cfg() -> MergeGuardConfig {
        MergeGuardConfig {
            owner: "xenofei".into(),
            repo: "LILITH".into(),
            base_branch: "main".into(),
            reviewer_app_slug: "lilith-reviewer".into(),
            required_checks: vec![
                "every-push".into(),
                "pr-ready".into(),
                "protected-test-check".into(),
                "privacy-check".into(),
            ],
            hooks_path: "tools/git-hooks".into(),
        }
    }

    fn hooks() -> Option<String> {
        Some("tools/git-hooks".into())
    }

    /// An open PR at `HEAD` with every required check green on `HEAD`.
    fn green() -> FakeGitHub {
        let fake = FakeGitHub::new();
        fake.add_pr(PR, HEAD, "main");
        for name in cfg().required_checks {
            fake.set_check(HEAD, &name, CheckState::Success);
        }
        fake
    }

    fn clean(_: &str, _: &str) -> Result<Verdict> {
        Ok(Verdict::default())
    }

    /// The gate for the tests that aren't about authentication: it lets every merge through.
    struct Allow;

    impl MergeGate for Allow {
        fn authorize(&self, _: &MergeSubject) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    /// A gate that records what it was asked and answers `answer`.
    struct Gate {
        asked: RefCell<Vec<(u64, String)>>,
        answer: std::result::Result<(), String>,
    }

    impl MergeGate for Gate {
        fn authorize(&self, s: &MergeSubject) -> std::result::Result<(), String> {
            self.asked.borrow_mut().push((s.pr, s.head.to_string()));
            self.answer.clone()
        }
    }

    fn protected(_: &str, _: &str) -> Result<Verdict> {
        Ok(Verdict {
            needs_approval: vec![Blocker::ProtectedChanged {
                path: "crates/lil-spec-tests/tests/smoke.rs".into(),
                kind: "modified".into(),
            }],
            warnings: vec![],
        })
    }

    /// Wraps the fake: can fail one method with an error, or move the PR head just before the
    /// merge call (a push that lands after the conditions were checked).
    struct Scripted<'a> {
        inner: &'a FakeGitHub,
        fail: Option<&'static str>,
        move_head_on_merge_to: Option<&'static str>,
    }

    impl<'a> Scripted<'a> {
        fn failing(inner: &'a FakeGitHub, method: &'static str) -> Self {
            Self {
                inner,
                fail: Some(method),
                move_head_on_merge_to: None,
            }
        }
        fn err(&self, method: &str) -> Result<()> {
            if self.fail == Some(method) {
                return Err(anyhow!("HTTP 502 from GitHub ({method})"));
            }
            Ok(())
        }
    }

    impl GitHubApi for Scripted<'_> {
        fn pr(&self, number: u64) -> Result<PrInfo> {
            self.err("pr")?;
            self.inner.pr(number)
        }
        fn check(&self, sha: &str, name: &str) -> Result<CheckState> {
            self.err("check")?;
            self.inner.check(sha, name)
        }
        fn reviews(&self, number: u64) -> Result<Vec<Review>> {
            self.err("reviews")?;
            self.inner.reviews(number)
        }
        fn merge(&self, number: u64, expected_head_sha: &str) -> Result<MergeOutcome> {
            self.err("merge")?;
            if let Some(new) = self.move_head_on_merge_to {
                self.inner.set_head(number, new);
            }
            self.inner.merge(number, expected_head_sha)
        }
        fn post_review_as_app(&self, _: u64, _: &str, _: &str, _: &str) -> Result<()> {
            panic!("merge must never post reviews")
        }
        fn commit_pulls(&self, sha: &str) -> Result<Vec<crate::dev::github::CommitPull>> {
            self.err("commit_pulls")?;
            self.inner.commit_pulls(sha)
        }
    }

    /// Any call is a test failure: proves nothing touched GitHub.
    struct NoNetwork;

    impl GitHubApi for NoNetwork {
        fn pr(&self, _: u64) -> Result<PrInfo> {
            panic!("network call: pr")
        }
        fn check(&self, _: &str, _: &str) -> Result<CheckState> {
            panic!("network call: check")
        }
        fn reviews(&self, _: u64) -> Result<Vec<Review>> {
            panic!("network call: reviews")
        }
        fn merge(&self, _: u64, _: &str) -> Result<MergeOutcome> {
            panic!("network call: merge")
        }
        fn post_review_as_app(&self, _: u64, _: &str, _: &str, _: &str) -> Result<()> {
            panic!("network call: post_review_as_app")
        }
        fn commit_pulls(&self, _: &str) -> Result<Vec<crate::dev::github::CommitPull>> {
            panic!("network call: commit_pulls")
        }
    }

    fn refusals(r: Result<String, Vec<Refusal>>) -> Vec<Refusal> {
        match r {
            Ok(sha) => panic!("expected a refusal, got a pass at {sha}"),
            Err(v) => {
                assert!(!v.is_empty(), "a refusal must carry at least one reason");
                v
            }
        }
    }

    // ---- the brief's tests ----------------------------------------------------------------

    #[test]
    fn all_green_merges_at_checked_sha() {
        let fake = green();
        assert_eq!(
            merge_conditions(&cfg(), &fake, PR, &clean, hooks()),
            Ok(HEAD.to_string())
        );
        assert_eq!(
            merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow),
            Ok(format!("merge-of-{HEAD}"))
        );
        assert_eq!(fake.pr(PR).unwrap().state, "closed");
    }

    #[test]
    fn missing_check_refuses() {
        let fake = green();
        fake.set_check(HEAD, "pr-ready", CheckState::Missing);
        let r = refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow));
        assert_eq!(
            r,
            vec![Refusal::CheckNotGreen {
                name: "pr-ready".into(),
                state: "missing".into(),
                sha: HEAD.into(),
            }]
        );
        let msg = explain(&r);
        assert!(
            msg.contains("pr-ready hasn't reported for 1234567"),
            "{msg}"
        );
        assert_eq!(fake.pr(PR).unwrap().state, "open", "nothing merged");
    }

    #[test]
    fn pending_check_refuses() {
        let fake = green();
        fake.set_check(HEAD, "every-push", CheckState::Pending);
        let r = refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow));
        assert_eq!(
            r,
            vec![Refusal::CheckNotGreen {
                name: "every-push".into(),
                state: "pending".into(),
                sha: HEAD.into(),
            }]
        );
        let msg = explain(&r);
        assert!(
            msg.contains("every-push is still running on 1234567"),
            "{msg}"
        );
        assert_eq!(fake.pr(PR).unwrap().state, "open");
    }

    #[test]
    fn failed_check_refuses_with_its_name() {
        let fake = green();
        fake.set_check(
            HEAD,
            "protected-test-check",
            CheckState::Failure("failure".into()),
        );
        let r = refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow));
        let msg = explain(&r);
        assert!(msg.contains("protected-test-check"), "{msg}");
        assert!(msg.contains("failure"), "{msg}");
        assert!(msg.contains("1234567"), "{msg}");
        assert_eq!(fake.pr(PR).unwrap().state, "open");
    }

    /// QA-036 as amended (P4a-R38): every merge asks for your authentication, protected files or
    /// not, after every other condition held; a refusal merges nothing.
    #[test] // spec: QA-036
    fn every_merge_asks_for_authentication_last_and_a_refusal_merges_nothing() {
        for verdict in [&clean as &dyn Fn(&str, &str) -> Result<Verdict>, &protected] {
            // Authenticated: merged, and the gate was asked once, about the checked head.
            let fake = green();
            fake.add_review(PR, BOT, "APPROVED", HEAD);
            let gate = Gate {
                asked: RefCell::default(),
                answer: Ok(()),
            };
            assert!(merge_pr(&cfg(), &fake, PR, verdict, hooks(), &gate).is_ok());
            assert_eq!(gate.asked.borrow().clone(), [(PR, HEAD.to_string())]);
            assert_eq!(fake.pr(PR).unwrap().state, "closed");

            // Not authenticated: nothing merged.
            let fake = green();
            fake.add_review(PR, BOT, "APPROVED", HEAD);
            let gate = Gate {
                asked: RefCell::default(),
                answer: Err("cancelled; nothing was merged".into()),
            };
            let r = refusals(merge_pr(&cfg(), &fake, PR, verdict, hooks(), &gate));
            assert!(matches!(&r[..], [Refusal::NotAuthenticated(_)]), "{r:?}");
            assert!(explain(&r).contains("cancelled"), "{}", explain(&r));
            assert_eq!(fake.pr(PR).unwrap().state, "open");
        }
        // A PR that fails another condition never asks you at all.
        let fake = green();
        fake.set_check(HEAD, "pr-ready", CheckState::Missing);
        let gate = Gate {
            asked: RefCell::default(),
            answer: Ok(()),
        };
        refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &gate));
        assert!(gate.asked.borrow().is_empty());
    }

    #[test]
    fn protected_change_without_app_approval_refuses() {
        let fake = green();
        let r = refusals(merge_pr(&cfg(), &fake, PR, &protected, hooks(), &Allow));
        assert!(
            matches!(&r[..], [Refusal::ProtectedNeedsApproval(lines)]
                if lines.iter().any(|l| l.contains("crates/lil-spec-tests/tests/smoke.rs"))),
            "{r:?}"
        );
        let msg = explain(&r);
        assert!(msg.contains("reviewer app"), "{msg}");
        assert!(
            msg.contains("crates/lil-spec-tests/tests/smoke.rs"),
            "{msg}"
        );
        assert_eq!(fake.pr(PR).unwrap().state, "open");

        // The same change with the app's approval of the head commit goes through.
        fake.add_review(PR, BOT, "APPROVED", HEAD);
        assert_eq!(
            merge_pr(&cfg(), &fake, PR, &protected, hooks(), &Allow),
            Ok(format!("merge-of-{HEAD}"))
        );
    }

    #[test]
    fn approval_on_older_commit_does_not_count() {
        let fake = green();
        fake.add_review(PR, BOT, "APPROVED", OLD);
        let r = refusals(merge_pr(&cfg(), &fake, PR, &protected, hooks(), &Allow));
        assert!(
            matches!(&r[..], [Refusal::ProtectedNeedsApproval(_)]),
            "{r:?}"
        );
    }

    #[test]
    fn approval_by_human_does_not_count() {
        let fake = green();
        fake.add_review(PR, "xenofei", "APPROVED", HEAD);
        // Neither does the app slug without the `[bot]` suffix, or another app.
        fake.add_review(PR, "lilith-reviewer", "APPROVED", HEAD);
        fake.add_review(PR, "lilith-app[bot]", "APPROVED", HEAD);
        let r = refusals(merge_pr(&cfg(), &fake, PR, &protected, hooks(), &Allow));
        assert!(
            matches!(&r[..], [Refusal::ProtectedNeedsApproval(_)]),
            "{r:?}"
        );
    }

    #[test]
    fn hooks_not_installed_refuses_before_anything() {
        let panics = |_: &str, _: &str| -> Result<Verdict> { panic!("verdict computed") };
        for found in [None, Some(String::new()), Some(".git/hooks".to_string())] {
            let r = refusals(merge_pr(
                &cfg(),
                &NoNetwork,
                PR,
                &panics,
                found.clone(),
                &Allow,
            ));
            assert_eq!(
                r,
                vec![Refusal::HooksNotInstalled {
                    expected: "tools/git-hooks".into(),
                    found: found.clone(),
                }]
            );
            let msg = explain(&r);
            assert!(msg.contains("hooks"), "{msg}");
            assert!(msg.contains("tools/git-hooks"), "{msg}");
            let r = refusals(merge_conditions(&cfg(), &NoNetwork, PR, &panics, found));
            assert!(matches!(&r[..], [Refusal::HooksNotInstalled { .. }]));
        }
    }

    #[test]
    fn merge_is_pinned_to_checked_sha() {
        let fake = green();
        let gh = Scripted {
            inner: &fake,
            fail: None,
            move_head_on_merge_to: Some(NEW),
        };
        let r = refusals(merge_pr(&cfg(), &gh, PR, &clean, hooks(), &Allow));
        assert_eq!(r, vec![Refusal::HeadMoved]);
        let msg = explain(&r);
        assert!(msg.contains("moved"), "{msg}");
        assert!(msg.contains("Nothing was merged"), "{msg}");
        assert_eq!(fake.pr(PR).unwrap().state, "open");
        assert_eq!(fake.pr(PR).unwrap().head_sha, NEW);
    }

    // ---- controller rulings: fail closed, drafts, approval state ---------------------------

    #[test]
    fn verdict_is_asked_for_origin_base_and_the_head_sha() {
        let fake = green();
        let seen = RefCell::new(Vec::new());
        let spy = |base: &str, head: &str| -> Result<Verdict> {
            seen.borrow_mut().push((base.to_string(), head.to_string()));
            Ok(Verdict::default())
        };
        assert!(merge_conditions(&cfg(), &fake, PR, &spy, hooks()).is_ok());
        assert_eq!(
            seen.into_inner(),
            vec![("refs/remotes/origin/main".to_string(), HEAD.to_string())]
        );
    }

    #[test]
    fn draft_pr_refuses() {
        let fake = green();
        fake.set_draft(PR, true);
        let r = refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow));
        assert_eq!(r, vec![Refusal::PrIsDraft]);
        assert!(explain(&r).contains("draft"));
        assert_eq!(fake.pr(PR).unwrap().state, "open");
    }

    #[test]
    fn closed_pr_refuses() {
        let fake = green();
        fake.set_state(PR, "closed");
        let r = refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow));
        assert_eq!(
            r,
            vec![Refusal::PrNotOpen {
                state: "closed".into()
            }]
        );
        assert!(explain(&r).contains("closed"));
    }

    #[test]
    fn pr_into_another_branch_refuses() {
        let fake = FakeGitHub::new();
        fake.add_pr(PR, HEAD, "release");
        for name in cfg().required_checks {
            fake.set_check(HEAD, &name, CheckState::Success);
        }
        let r = refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow));
        assert_eq!(
            r,
            vec![Refusal::WrongBase {
                expected: "main".into(),
                found: "release".into()
            }]
        );
    }

    #[test]
    fn github_errors_are_refusals_never_passes() {
        for method in ["pr", "check", "reviews"] {
            let fake = green();
            fake.add_review(PR, BOT, "APPROVED", HEAD);
            let gh = Scripted::failing(&fake, method);
            let r = refusals(merge_pr(&cfg(), &gh, PR, &protected, hooks(), &Allow));
            assert!(
                r.iter()
                    .any(|x| matches!(x, Refusal::CouldNotCheck(m) if m.contains("502"))),
                "{method}: {r:?}"
            );
            assert_eq!(fake.pr(PR).unwrap().state, "open", "{method}");
        }
    }

    #[test]
    fn verdict_error_is_a_refusal() {
        let fake = green();
        let broken = |_: &str, _: &str| -> Result<Verdict> { Err(anyhow!("unknown revision")) };
        let r = refusals(merge_pr(&cfg(), &fake, PR, &broken, hooks(), &Allow));
        assert!(
            matches!(&r[..], [Refusal::CouldNotCheck(m)] if m.contains("unknown revision")),
            "{r:?}"
        );
        assert_eq!(fake.pr(PR).unwrap().state, "open");
    }

    #[test]
    fn failed_merge_call_is_reported_as_uncertain() {
        let fake = green();
        let gh = Scripted::failing(&fake, "merge");
        let r = refusals(merge_pr(&cfg(), &gh, PR, &clean, hooks(), &Allow));
        assert!(matches!(&r[..], [Refusal::MergeCallFailed(_)]), "{r:?}");
        assert!(explain(&r).contains("may or may not"), "{}", explain(&r));
    }

    #[test]
    fn not_mergeable_is_explained() {
        let fake = green();
        fake.set_not_mergeable(PR, "Merge conflict");
        let r = refusals(merge_pr(&cfg(), &fake, PR, &clean, hooks(), &Allow));
        assert_eq!(r, vec![Refusal::NotMergeable("Merge conflict".into())]);
        assert!(explain(&r).contains("Merge conflict"));
    }

    #[test]
    fn every_failing_condition_is_reported() {
        let fake = FakeGitHub::new();
        fake.add_pr(PR, HEAD, "main");
        let r = refusals(merge_pr(&cfg(), &fake, PR, &protected, hooks(), &Allow));
        // Four missing checks and the missing approval.
        assert_eq!(r.len(), 5, "{r:?}");
        assert_eq!(explain(&r).lines().count(), 5);
    }

    #[test]
    fn later_non_approval_by_the_app_cancels_its_approval() {
        for later in ["DISMISSED", "CHANGES_REQUESTED", "COMMENTED"] {
            let reviews = vec![
                Review {
                    login: BOT.into(),
                    state: "APPROVED".into(),
                    commit_id: HEAD.into(),
                },
                Review {
                    login: BOT.into(),
                    state: later.into(),
                    commit_id: HEAD.into(),
                },
            ];
            assert!(!app_approved(&reviews, BOT, HEAD), "{later}");
        }
        // A human's later review does not touch the app's approval.
        let reviews = vec![
            Review {
                login: BOT.into(),
                state: "APPROVED".into(),
                commit_id: HEAD.into(),
            },
            Review {
                login: "xenofei".into(),
                state: "CHANGES_REQUESTED".into(),
                commit_id: HEAD.into(),
            },
        ];
        assert!(app_approved(&reviews, BOT, HEAD));
        // Re-approval after a dismissal counts.
        let reviews = vec![
            Review {
                login: BOT.into(),
                state: "DISMISSED".into(),
                commit_id: HEAD.into(),
            },
            Review {
                login: BOT.into(),
                state: "APPROVED".into(),
                commit_id: HEAD.into(),
            },
        ];
        assert!(app_approved(&reviews, BOT, HEAD));
        assert!(!app_approved(&[], BOT, HEAD));
    }

    #[test]
    fn dismissed_app_approval_refuses_the_merge() {
        let fake = green();
        fake.add_review(PR, BOT, "APPROVED", HEAD);
        fake.add_review(PR, BOT, "DISMISSED", HEAD);
        let r = refusals(merge_pr(&cfg(), &fake, PR, &protected, hooks(), &Allow));
        assert!(
            matches!(&r[..], [Refusal::ProtectedNeedsApproval(_)]),
            "{r:?}"
        );
    }

    #[test]
    fn warnings_alone_do_not_block() {
        let fake = green();
        let warned = |_: &str, _: &str| -> Result<Verdict> {
            Ok(Verdict {
                needs_approval: vec![],
                warnings: vec![Warning::NewSleepOrRetry {
                    path: "crates/lil-core/src/lib.rs".into(),
                    line: "sleep(d);".into(),
                }],
            })
        };
        assert_eq!(
            merge_conditions(&cfg(), &fake, PR, &warned, hooks()),
            Ok(HEAD.to_string())
        );
    }

    #[test]
    fn explain_is_one_line_per_refusal() {
        let all = vec![
            Refusal::HooksNotInstalled {
                expected: "tools/git-hooks".into(),
                found: None,
            },
            Refusal::CheckNotGreen {
                name: "pr-ready".into(),
                state: "missing".into(),
                sha: HEAD.into(),
            },
            Refusal::ProtectedNeedsApproval(vec!["a".into(), "b".into()]),
            Refusal::PrNotOpen {
                state: "closed".into(),
            },
            Refusal::PrIsDraft,
            Refusal::WrongBase {
                expected: "main".into(),
                found: "x".into(),
            },
            Refusal::HeadMoved,
            Refusal::NotMergeable("multi\nline".into()),
            Refusal::CouldNotCheck("first\nsecond".into()),
            Refusal::MergeCallFailed("timeout".into()),
            Refusal::WrongOrigin {
                url: "C:/x/origin.git".into(),
                expected: "github.com/xenofei/LILITH".into(),
            },
        ];
        let text = explain(&all);
        assert_eq!(text.lines().count(), all.len(), "{text}");
        assert!(text.lines().all(|l| !l.trim().is_empty()), "{text}");
    }

    #[test]
    fn config_parses_the_committed_file_and_rejects_empty_lists() {
        let committed = include_str!("../../../../dev/merge-guard.toml");
        assert_eq!(MergeGuardConfig::parse(committed).unwrap(), cfg());
        let no_checks = committed.replace(
            r#"required_checks = ["every-push", "pr-ready", "protected-test-check", "privacy-check"]"#,
            "required_checks = []",
        );
        assert_ne!(no_checks, committed);
        assert!(MergeGuardConfig::parse(&no_checks).is_err());
        assert!(MergeGuardConfig::parse(&format!("{committed}\nextra = 1\n")).is_err());
        let bad_branch = committed.replace(r#"base_branch = "main""#, r#"base_branch = "-x""#);
        assert!(MergeGuardConfig::parse(&bad_branch).is_err());
    }

    fn helper_entries(local: &[&str], effective: &[(&str, &str)]) -> Option<Refusal> {
        let local: Vec<String> = local.iter().map(|s| s.to_string()).collect();
        let effective: Vec<(String, String)> = effective
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        credential_helper_refusal(&local, &effective)
    }

    #[test] // spec: QA-079
    fn credential_helper_must_be_the_reset_then_lil() {
        let h = GIT_CREDENTIAL_HELPER;
        let ch = "credential.helper";
        // Installed: global helpers before the reset don't matter.
        assert_eq!(
            helper_entries(
                &["", h],
                &[
                    (ch, "manager"),
                    (
                        "credential.https://github.com.helper",
                        "!gh auth git-credential"
                    ),
                    (ch, ""),
                    (ch, h)
                ]
            ),
            None
        );
        for (local, effective) in [
            (&[][..], &[(ch, "manager")][..]),
            (&[h], &[(ch, "manager"), (ch, h)]),
            (&[h, ""], &[(ch, h), (ch, "")]),
            (&["", h, "manager"], &[(ch, ""), (ch, h), (ch, "manager")]),
            (&["", "manager"], &[(ch, ""), (ch, "manager")]),
            // A URL-specific helper read after ours (worktree config, -c, ...).
            (
                &["", h],
                &[
                    (ch, ""),
                    (ch, h),
                    ("credential.https://github.com.helper", "x"),
                ],
            ),
            // Local entries right, but something later also resets and replaces them.
            (&["", h], &[(ch, ""), (ch, h), (ch, ""), (ch, "x")]),
        ] {
            let r = helper_entries(local, effective).unwrap();
            assert!(
                matches!(&r, Refusal::CredentialHelperNotInstalled { found, .. } if found.len() == local.len()),
                "{local:?} {effective:?}"
            );
            assert!(explain(&[r]).contains("lil dev install-hooks"));
        }
        // The refusal names the helper keys read after the reset, not their values.
        let r = helper_entries(
            &["", h],
            &[
                (ch, ""),
                (ch, h),
                ("credential.https://github.com.helper", "!secret-ish"),
            ],
        )
        .unwrap();
        let text = explain(&[r]);
        assert!(
            text.contains("credential.https://github.com.helper"),
            "{text}"
        );
        assert!(!text.contains("secret-ish"), "{text}");
    }

    #[test] // spec: QA-079
    fn preflight_requires_the_credential_helper_entries() {
        let f = fixture();
        assert!(preflight(&f.work).is_ok());
        let not_installed =
            |r: Vec<Refusal>| matches!(&r[..], [Refusal::CredentialHelperNotInstalled { .. }]);

        // Removed.
        git_t(
            &f.work,
            &["config", "--local", "--unset-all", "credential.helper"],
        );
        assert!(not_installed(preflight(&f.work).unwrap_err()));

        // The helper without the reset in front.
        git_t(
            &f.work,
            &[
                "config",
                "--local",
                "credential.helper",
                GIT_CREDENTIAL_HELPER,
            ],
        );
        assert!(not_installed(preflight(&f.work).unwrap_err()));

        // Installed, then a URL-specific helper added after it.
        install_helper_t(&f.work);
        assert!(preflight(&f.work).is_ok());
        git_t(
            &f.work,
            &[
                "config",
                "--local",
                "credential.https://github.com.helper",
                "!echo",
            ],
        );
        let r = preflight(&f.work).unwrap_err();
        assert!(explain(&r).contains("credential.https://github.com.helper"));
        assert!(not_installed(r));
    }

    #[test] // spec: QA-079
    fn helper_readers_keep_empty_entries_and_order() {
        let f = fixture();
        assert_eq!(
            local_credential_helpers(&f.work).unwrap(),
            ["", GIT_CREDENTIAL_HELPER]
        );
        let effective = effective_credential_helpers(&f.work).unwrap();
        let tail: Vec<(&str, &str)> = effective[effective.len() - 2..]
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            tail,
            [
                ("credential.helper", ""),
                ("credential.helper", GIT_CREDENTIAL_HELPER)
            ]
        );
    }

    #[test]
    fn hooks_refusal_suggests_install_hooks() {
        for found in [None, Some("/elsewhere".to_string())] {
            let r = hooks_refusal(&cfg(), found.as_deref()).unwrap();
            let text = explain(&[r]);
            assert!(text.contains("lil dev install-hooks"), "{text}");
        }
    }

    // ---- fix round 1: committed settings (R12) and a verified origin (R13) ------------------

    const COMMITTED: &str = include_str!("../../../../dev/merge-guard.toml");

    /// Writes the credential helper entries `lil dev install-hooks` writes.
    fn install_helper_t(dir: &Path) {
        git_t(
            dir,
            &[
                "config",
                "--local",
                "--replace-all",
                "credential.helper",
                "",
            ],
        );
        git_t(
            dir,
            &[
                "config",
                "--local",
                "--add",
                "credential.helper",
                GIT_CREDENTIAL_HELPER,
            ],
        );
    }

    /// Runs git with the user's global and system config switched off.
    fn git_t(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write_t(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn init_t(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git_t(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git_t(dir, &["config", "user.name", "Merge Test"]);
        git_t(dir, &["config", "user.email", "merge@test.invalid"]);
        git_t(dir, &["config", "commit.gpgsign", "false"]);
        git_t(dir, &["config", "core.autocrlf", "false"]);
    }

    fn commit_t(dir: &Path, msg: &str) {
        git_t(dir, &["add", "-A"]);
        git_t(dir, &["commit", "-q", "-m", msg]);
    }

    struct Fixture {
        tmp: tempfile::TempDir,
        work: std::path::PathBuf,
        origin: std::path::PathBuf,
        head: String,
    }

    /// A work repo whose `origin` is a local bare repo.
    ///
    /// - `main` (pushed) commits the real merge-guard settings and protects
    ///   `crates/lil-spec-tests/**`.
    /// - PR 42 (`refs/pull/42/head` on origin) modifies a spec test, so it needs approval.
    /// - `core.hooksPath` is `tools/git-hooks`, and the credential helper entries are written.
    /// - The working tree's `dev/merge-guard.toml` is then edited, and not committed: pr-ready
    ///   dropped, the reviewer slug swapped to `github-actions`, `hooks_path` set to `.git/hooks`.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let work = tmp.path().join("work");
        git_t(
            tmp.path(),
            &[
                "-c",
                "init.defaultBranch=main",
                "init",
                "--bare",
                "-q",
                "origin.git",
            ],
        );
        init_t(&work);
        write_t(&work, CONFIG_PATH, COMMITTED);
        write_t(
            &work,
            "dev/protected-paths.txt",
            "crates/lil-spec-tests/**\ndev/merge-guard.toml\n",
        );
        write_t(
            &work,
            "crates/lil-spec-tests/tests/smoke.rs",
            "#[test]\nfn spec_harness_runs() {}\n",
        );
        commit_t(&work, "base");
        git_t(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_t(&work, &["push", "-q", "origin", "main"]);
        git_t(&work, &["fetch", "-q", "origin"]);

        git_t(&work, &["checkout", "-q", "-b", "feature"]);
        write_t(
            &work,
            "crates/lil-spec-tests/tests/smoke.rs",
            "#[test]\nfn spec_harness_runs() { assert!(true) }\n",
        );
        commit_t(&work, "touch a spec test");
        let head = git_t(&work, &["rev-parse", "HEAD"]);
        git_t(&work, &["push", "-q", "origin", "HEAD:refs/pull/42/head"]);
        git_t(&work, &["config", "core.hooksPath", "tools/git-hooks"]);
        install_helper_t(&work);

        let edited = COMMITTED
            .replace(r#""pr-ready", "#, "")
            .replace("lilith-reviewer", "github-actions")
            .replace(
                r#"hooks_path = "tools/git-hooks""#,
                r#"hooks_path = ".git/hooks""#,
            );
        let parsed = MergeGuardConfig::parse(&edited).unwrap();
        assert!(!parsed.required_checks.contains(&"pr-ready".to_string()));
        assert_eq!(parsed.reviewer_app_slug, "github-actions");
        assert_eq!(parsed.hooks_path, ".git/hooks");
        write_t(&work, CONFIG_PATH, &edited);

        Fixture {
            tmp,
            work,
            origin,
            head,
        }
    }

    /// Pushes a new `main` to origin from a second clone, leaving the work repo's
    /// `origin/main` stale until it fetches.
    fn push_settings_to_origin(f: &Fixture, settings: &str) {
        let other = f.tmp.path().join("other");
        git_t(
            f.tmp.path(),
            &[
                "clone",
                "-q",
                "-b",
                "main",
                f.origin.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        git_t(&other, &["config", "user.name", "Merge Test"]);
        git_t(&other, &["config", "user.email", "merge@test.invalid"]);
        git_t(&other, &["config", "commit.gpgsign", "false"]);
        write_t(&other, CONFIG_PATH, settings);
        commit_t(&other, "change settings");
        git_t(&other, &["push", "-q", "origin", "main"]);
    }

    fn boxed(fake: &FakeGitHub) -> Box<dyn GitHubApi + '_> {
        Box::new(Scripted {
            inner: fake,
            fail: None,
            move_head_on_merge_to: None,
        })
    }

    #[test]
    fn preflight_reads_committed_settings_not_the_working_tree() {
        let f = fixture();
        let (settings, found) = preflight(&f.work).unwrap();
        assert_eq!(settings, cfg(), "the uncommitted edit leaked in");
        assert_eq!(found, hooks());

        // Pointing the hooks at the edited working-tree value does not satisfy QA-034.
        git_t(&f.work, &["config", "core.hooksPath", ".git/hooks"]);
        assert_eq!(
            preflight(&f.work).unwrap_err(),
            vec![Refusal::HooksNotInstalled {
                expected: "tools/git-hooks".into(),
                found: Some(".git/hooks".into()),
            }]
        );
    }

    #[test]
    fn edited_working_tree_settings_do_not_change_the_merge_conditions() {
        let f = fixture();
        let fake = FakeGitHub::new();
        fake.add_pr(42, &f.head, "main");
        fake.set_check(&f.head, "every-push", CheckState::Success);
        fake.set_check(&f.head, "protected-test-check", CheckState::Success);
        fake.set_check(&f.head, "privacy-check", CheckState::Success);
        // The swapped-in slug's approval must not count.
        fake.add_review(42, "github-actions[bot]", "APPROVED", &f.head);
        let seen = RefCell::new(Vec::new());
        let github = |c: &MergeGuardConfig| -> Result<Box<dyn GitHubApi + '_>> {
            seen.borrow_mut().push(c.clone());
            Ok(boxed(&fake))
        };

        let r = refusals(merge_in_repo(&f.work, 42, &cfg(), hooks(), &github, &Allow));
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(
            r[0],
            Refusal::CheckNotGreen {
                name: "pr-ready".into(),
                state: "missing".into(),
                sha: f.head.clone(),
            }
        );
        assert!(
            matches!(&r[1], Refusal::ProtectedNeedsApproval(lines)
                if lines.iter().any(|l| l.contains("crates/lil-spec-tests/tests/smoke.rs"))),
            "{r:?}"
        );
        assert_eq!(seen.borrow().clone(), vec![cfg()]);
        assert_eq!(fake.pr(42).unwrap().state, "open");

        // A hooks path matching the edited working tree is still refused.
        let r = refusals(merge_in_repo(
            &f.work,
            42,
            &cfg(),
            Some(".git/hooks".into()),
            &github,
            &Allow,
        ));
        assert!(
            matches!(&r[..], [Refusal::HooksNotInstalled { .. }]),
            "{r:?}"
        );

        // With the committed conditions met, the real fetch + verdict path merges.
        fake.set_check(&f.head, "pr-ready", CheckState::Success);
        fake.add_review(42, BOT, "APPROVED", &f.head);
        assert_eq!(
            merge_in_repo(&f.work, 42, &cfg(), hooks(), &github, &Allow),
            Ok(format!("merge-of-{}", f.head))
        );
    }

    #[test]
    fn a_local_branch_named_origin_main_does_not_shadow_the_base() {
        let f = fixture();
        // A decoy local branch `origin/main` (refs/heads/origin/main) at the PR head: git's
        // short-name lookup prefers it to refs/remotes/origin/main, and judged against it the
        // PR would change nothing.
        git_t(&f.work, &["branch", "origin/main", &f.head]);
        let fake = FakeGitHub::new();
        fake.add_pr(42, &f.head, "main");
        for name in cfg().required_checks {
            fake.set_check(&f.head, &name, CheckState::Success);
        }
        let github = |_: &MergeGuardConfig| -> Result<Box<dyn GitHubApi + '_>> { Ok(boxed(&fake)) };
        let r = refusals(merge_in_repo(&f.work, 42, &cfg(), hooks(), &github, &Allow));
        assert!(
            matches!(&r[..], [Refusal::ProtectedNeedsApproval(lines)]
                if lines.iter().any(|l| l.contains("crates/lil-spec-tests/tests/smoke.rs"))),
            "{r:?}"
        );
        assert_eq!(fake.pr(42).unwrap().state, "open");
    }

    #[test]
    fn settings_are_re_read_from_the_fetched_origin_main() {
        let f = fixture();
        let (stale, found) = preflight(&f.work).unwrap();
        push_settings_to_origin(
            &f,
            &COMMITTED.replace(r#""privacy-check"]"#, r#""privacy-check", "nightly"]"#),
        );
        let fake = FakeGitHub::new();
        fake.add_pr(42, &f.head, "main");
        for name in cfg().required_checks {
            fake.set_check(&f.head, &name, CheckState::Success);
        }
        fake.add_review(42, BOT, "APPROVED", &f.head);
        let github = |_: &MergeGuardConfig| -> Result<Box<dyn GitHubApi + '_>> { Ok(boxed(&fake)) };
        let r = refusals(merge_in_repo(&f.work, 42, &stale, found, &github, &Allow));
        assert_eq!(
            r,
            vec![Refusal::CheckNotGreen {
                name: "nightly".into(),
                state: "missing".into(),
                sha: f.head.clone(),
            }]
        );
    }

    #[test]
    fn fetched_settings_naming_another_repo_refuse() {
        let f = fixture();
        let (checked, found) = preflight(&f.work).unwrap();
        push_settings_to_origin(
            &f,
            &COMMITTED.replace(r#"repo = "LILITH""#, r#"repo = "Elsewhere""#),
        );
        let github = |_: &MergeGuardConfig| -> Result<Box<dyn GitHubApi + 'static>> {
            panic!("GitHub must not be contacted")
        };
        let r = refusals(merge_in_repo(&f.work, 42, &checked, found, &github, &Allow));
        assert!(
            matches!(&r[..], [Refusal::CouldNotCheck(m)] if m.contains("Elsewhere")),
            "{r:?}"
        );
    }

    #[test]
    fn missing_settings_ref_or_file_refuses() {
        let want = "can't read dev/merge-guard.toml from origin/main; run `git fetch origin`";

        // No origin/main at all.
        let tmp = tempfile::tempdir().unwrap();
        let lone = tmp.path().join("lone");
        init_t(&lone);
        write_t(&lone, CONFIG_PATH, COMMITTED);
        commit_t(&lone, "settings only in a local commit");
        git_t(&lone, &["config", "core.hooksPath", "tools/git-hooks"]);
        let r = preflight(&lone).unwrap_err();
        assert!(
            matches!(&r[..], [Refusal::CouldNotCheck(m)] if m.contains(want)),
            "{r:?}"
        );
        assert!(explain(&r).contains(want));

        // origin/main exists but doesn't have the file.
        let origin = tmp.path().join("origin.git");
        git_t(
            tmp.path(),
            &[
                "-c",
                "init.defaultBranch=main",
                "init",
                "--bare",
                "-q",
                "origin.git",
            ],
        );
        let work = tmp.path().join("work");
        init_t(&work);
        write_t(&work, "README.md", "x\n");
        commit_t(&work, "no settings");
        git_t(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_t(&work, &["push", "-q", "origin", "main"]);
        git_t(&work, &["fetch", "-q", "origin"]);
        write_t(&work, CONFIG_PATH, COMMITTED);
        git_t(&work, &["config", "core.hooksPath", "tools/git-hooks"]);
        let r = preflight(&work).unwrap_err();
        assert!(
            matches!(&r[..], [Refusal::CouldNotCheck(m)] if m.contains(want)),
            "{r:?}"
        );
    }

    #[test]
    fn local_bare_origin_is_not_trusted() {
        let f = fixture();
        let r = check_origin(&f.work, &cfg()).unwrap_err();
        assert!(
            matches!(&r, Refusal::WrongOrigin { expected, .. }
                if expected == "github.com/xenofei/LILITH"),
            "{r:?}"
        );
        let msg = explain(&[r]);
        assert!(msg.contains("expected github.com/xenofei/LILITH"), "{msg}");
        git_t(
            &f.work,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/xenofei/LILITH.git",
            ],
        );
        assert_eq!(check_origin(&f.work, &cfg()), Ok(()));
    }

    #[test]
    fn origin_url_matcher() {
        let ok = [
            "https://github.com/xenofei/LILITH",
            "https://github.com/xenofei/LILITH.git",
            "https://github.com/xenofei/LILITH/",
            "https://github.com/xenofei/LILITH.git/",
            "HTTPS://GitHub.com/XENOFEI/lilith",
            "git@github.com:xenofei/LILITH",
            "git@github.com:xenofei/LILITH.git",
            "git@GITHUB.COM:Xenofei/LILITH.git/",
            "ssh://git@github.com/xenofei/LILITH",
            "ssh://git@github.com/xenofei/LILITH.git",
            "  https://github.com/xenofei/LILITH.git\n",
        ];
        for url in ok {
            assert!(origin_matches(url, "xenofei", "LILITH"), "{url:?}");
        }
        let bad = [
            "",
            "C:/repos/origin.git",
            "/srv/git/LILITH.git",
            "file:///srv/xenofei/LILITH.git",
            "http://github.com/xenofei/LILITH",
            "https://github.com.evil.example/xenofei/LILITH",
            "https://evil.example/github.com/xenofei/LILITH",
            "https://gitlab.com/xenofei/LILITH",
            "https://github.com/other/LILITH",
            "https://github.com/xenofei/LILITH-fork",
            "https://github.com/xenofei/LILITH/extra",
            "https://github.com/xenofei/LILITH.git.git",
            "https://github.com/xenofei//LILITH",
            "https://github.com/xenofei",
            "https://token@github.com/xenofei/LILITH",
            "git@github.com/xenofei/LILITH",
            "evil@github.com:xenofei/LILITH",
            "ssh://git@github.com:22/xenofei/LILITH",
            "ssh://github.com/xenofei/LILITH",
        ];
        for url in bad {
            assert!(!origin_matches(url, "xenofei", "LILITH"), "{url:?}");
        }
    }

    #[test]
    fn wrong_origin_message_hides_credentials() {
        let msg = explain(&[Refusal::WrongOrigin {
            url: "https://user:ghp_SECRET@evil.example/xenofei/LILITH.git".into(),
            expected: "github.com/xenofei/LILITH".into(),
        }]);
        assert!(!msg.contains("ghp_SECRET"), "{msg}");
        assert!(!msg.contains("user:"), "{msg}");
        assert!(msg.contains("evil.example"), "{msg}");
        assert!(msg.contains("expected github.com/xenofei/LILITH"), "{msg}");
    }

    // ---- the reviewer App's slug ------------------------------------------------------------

    #[test] // spec: QA-020
    fn committed_config_names_the_live_slug() {
        let parsed = MergeGuardConfig::parse(COMMITTED).unwrap();
        assert_eq!(parsed, cfg());
        assert_eq!(parsed.reviewer_app_slug, "lilith-reviewer");
        assert_eq!(parsed.reviewer_login(), BOT);
        // The slug is checked like every other name.
        for bad in ["", "-x", "a..b", "lilith reviewer", "lilith-reviewer[bot]"] {
            let text = COMMITTED.replace(r#""lilith-reviewer""#, &format!("{bad:?}"));
            assert_ne!(text, COMMITTED);
            assert!(MergeGuardConfig::parse(&text).is_err(), "{bad:?}");
        }
    }

    /// The second slug of the App's rename is gone: settings that still name it are refused,
    /// like any other unknown key.
    #[test] // spec: QA-020
    fn unknown_config_keys_are_still_refused() {
        for extra in [
            "extra = 1",
            "reviewer_app_slug_next = \"lilith-reviewer\"",
            "reviewer_app_slugs = [\"lilith-reviewer\"]",
            "reviewer_logins = [\"lilith-reviewer[bot]\"]",
        ] {
            let text = format!("{COMMITTED}\n{extra}\n");
            assert!(MergeGuardConfig::parse(&text).is_err(), "{extra}");
        }
        // A second slug is refused, not merged or overridden.
        let twice = format!("{COMMITTED}\nreviewer_app_slug = \"github-actions\"\n");
        assert!(MergeGuardConfig::parse(&twice).is_err());
    }

    #[test] // spec: QA-020
    fn approval_from_another_login_or_a_person_does_not_count() {
        let fake = green();
        // The App's login before its rename no longer counts either.
        let old_bot = format!(
            "{}-reviewer[bot]",
            lil_platform::names::LEGACY_APP_NAME.to_lowercase()
        );
        for login in [
            // People: an account may be named like the slug, but never ends in `[bot]`.
            "octocat",
            "lilith-reviewer",
            // Other Apps, the old slug, lookalikes and other spellings.
            old_bot.as_str(),
            "github-actions[bot]",
            "lilith-reviewer-next[bot]",
            "lilith[bot]",
            "Lilith-Reviewer[bot]",
            "lilith-reviewer[bot] ",
            "lilith-reviewer[bot][bot]",
            "ghost",
            "",
        ] {
            fake.add_review(PR, login, "APPROVED", HEAD);
        }
        let r = refusals(merge_pr(&cfg(), &fake, PR, &protected, hooks(), &Allow));
        assert!(
            matches!(&r[..], [Refusal::ProtectedNeedsApproval(_)]),
            "{r:?}"
        );
        assert!(!app_approved(
            &fake.reviews(PR).unwrap(),
            &cfg().reviewer_login(),
            HEAD
        ));
    }

    /// Review round 1, I-4: the enforcement decision itself. With `--pr`, a change with no
    /// protected files still fails without the App's approval of the head, and passes with it;
    /// without `--pr`, only the verdict counts.
    #[test] // spec: QA-021, QA-036
    fn guard_with_a_pr_needs_the_approval_even_when_nothing_is_protected() {
        let clean = Verdict::default();
        let blocked = protected("", "").unwrap();
        let approved = green();
        approved.add_review(PR, BOT, "APPROVED", HEAD);
        let unapproved = green();
        let code = |fake: &FakeGitHub, v: &Verdict| {
            guard_exit(v, Some(PR), &|pr| pr_approval(fake, &cfg(), pr, HEAD)).0
        };
        assert_eq!(
            code(&unapproved, &clean),
            1,
            "nothing protected, no approval"
        );
        assert_eq!(code(&approved, &clean), 0, "nothing protected, approved");
        assert_eq!(code(&unapproved, &blocked), 1, "protected, no approval");
        assert_eq!(code(&approved, &blocked), 0, "protected, approved");
        let never = |_: u64| -> std::result::Result<String, String> {
            panic!("no PR: the approval isn't read")
        };
        assert_eq!(guard_exit(&clean, None, &never), (0, None));
        assert_eq!(guard_exit(&blocked, None, &never).0, 1);
        // The line says why.
        let (_, line) = guard_exit(&clean, Some(PR), &|pr| {
            pr_approval(&unapproved, &cfg(), pr, HEAD)
        });
        assert!(line.unwrap().contains("Not approved"));
    }

    /// CI's protected-test check on a PR (owner decision 2026-10-26): only the reviewer App's
    /// approval of the PR's current head passes it, protected files or not.
    #[test] // spec: QA-021, QA-036
    fn the_pr_check_passes_only_on_the_apps_approval_of_the_head() {
        let check = |fake: &dyn GitHubApi| pr_approval(fake, &cfg(), PR, HEAD);

        // Approved at the head: passes.
        let fake = green();
        fake.add_review(PR, BOT, "APPROVED", HEAD);
        let line = check(&fake).unwrap();
        assert!(line.contains(BOT) && line.contains(short(HEAD)), "{line}");

        // No review at all: fails, and says how the approval is given.
        let fake = green();
        let line = check(&fake).unwrap_err();
        assert!(line.contains("Not approved"), "{line}");
        assert!(line.contains("lil dev merge"), "{line}");

        // Approved at an older head only: a new push needs a new approval.
        let fake = green();
        fake.add_review(PR, BOT, "APPROVED", OLD);
        assert!(check(&fake).is_err());

        // Approved by anyone but the App: people, other Apps, lookalikes.
        let fake = green();
        for login in [
            "octocat",
            "lilith-reviewer",
            "github-actions[bot]",
            "lilith-reviewer-next[bot]",
            "Lilith-Reviewer[bot]",
        ] {
            fake.add_review(PR, login, "APPROVED", HEAD);
        }
        assert!(check(&fake).is_err());

        // A later non-approval by the App on that head cancels its approval.
        let fake = green();
        fake.add_review(PR, BOT, "APPROVED", HEAD);
        fake.add_review(PR, BOT, "CHANGES_REQUESTED", HEAD);
        assert!(check(&fake).is_err());

        // A dismissed approval doesn't count (GitHub keeps it in the list as DISMISSED).
        let fake = green();
        fake.add_review(PR, BOT, "DISMISSED", HEAD);
        assert!(check(&fake).is_err());

        // Reviews that can't be read never pass.
        let fake = green();
        fake.add_review(PR, BOT, "APPROVED", HEAD);
        let line = check(&Scripted::failing(&fake, "reviews")).unwrap_err();
        assert!(line.contains("doesn't count"), "{line}");
    }
}
