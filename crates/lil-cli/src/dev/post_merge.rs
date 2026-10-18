//! `lil dev guard --post-merge` and `lil dev revert-pr` (QA-035, ruling R24).
//!
//! CI runs these after every push to `main`, with a `lil` built from `before` (the trusted
//! pre-push main), so the pushed code never judges itself.
//!
//! **What is judged.** Every step main took from `before` to `after`: the commits on the
//! first-parent chain (`git rev-list --first-parent before..after`). A merge commit made by
//! `lil dev merge` stands for its whole PR; the PR's own commits are covered by it and are not
//! judged one by one (a PR that adds and then removes a protected change nets to nothing). For
//! each step:
//!
//! - GitHub must associate it with a PR merged into the base branch
//!   ([`GitHubApi::commit_pulls`]); otherwise it is a direct push and is flagged, whatever it
//!   changed.
//! - If the step changed protected files (the guard verdict of the step against its first
//!   parent), the reviewer App must have approved that PR's final head commit
//!   ([`app_approved`], under either of its logins: [`MergeGuardConfig::reviewer_logins`]).
//!   A merge commit must then also be a plain merge of that head: exactly
//!   two parents, the second being the PR's head, and a tree equal to
//!   `git merge-tree --write-tree <first parent> <head>`. A conflict or any difference is
//!   flagged ([`Flag::MergeNotClean`]), because a merge commit can carry changes of its own
//!   that the PR's approval never covered.
//!
//! `before` not being an ancestor of `after` (a force push or a rewind) is flagged too.
//!
//! **Repository creation (ruling R27).** When `before` is all zeros the push created main:
//! there is nothing to compare against, so nothing is judged and nothing is reverted.
//!
//! **Fail closed.** Any git, GitHub or settings error is a [`Flag::CouldNotCheck`], never a pass
//! and never "no PR".
//!
//! **Settings** (the reviewer slug, owner and repo) are read from `dev/merge-guard.toml` as
//! committed at `before`, so a direct push cannot swap the reviewer it is judged by. When
//! `before` has no settings, the check refuses (exit 2, and `revert-pr` fails, so CI opens its
//! plain issue); the settings are never read from `after` (R38). A push that created main
//! (`before` all zeros) is the one exception, and it is not judged at all (R27).
//!
//! **`revert-pr`** creates `revert/<after7>-<run attempt>` from `after`, reverts the same first-parent steps
//! newest first (`git revert --no-edit -m 1 ...`: a plain `git revert before..after` stops at the
//! first merge commit, and would revert the PR's inner commits twice), checks that the result
//! has exactly `before`'s tree, pushes the branch and opens a PR and an issue assigned to the
//! owner with the plain-language verdict, through the `gh` CLI (`GH_TOKEN`). The push uses
//! `gh auth git-credential` as git's credential helper, so the token is never written to
//! `.git/config` or put on a command line. When the revert can't be made, pushed or opened as a
//! PR, the issue is still opened, with the verdict and what went wrong.
//!
//! Text that a pushed commit can influence (paths, error messages) goes into the PR and issue
//! bodies only inside a fenced code block whose fence is longer than any run of backticks in
//! it, so it can't mention people, link, or close issues.

use super::github::{CommitPull, GitHubApi, RestClient, github_token};
use super::guard;
use super::merge::{
    CONFIG_PATH, MergeGuardConfig, app_approved, blocker_lines, is_commit_sha, one_line, short,
};
use super::root;
use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// One reason a pushed range on main is not acceptable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flag {
    /// `before` is not an ancestor of `after`: main was force-pushed or rewound.
    HistoryRewritten { before: String, after: String },
    /// No PR merged into main is associated with this commit. `lines` are the protected
    /// changes it made, if any.
    DirectPush { sha: String, lines: Vec<String> },
    /// Protected changes without the reviewer App's approval of the PR's final head.
    UnapprovedProtected {
        sha: String,
        prs: Vec<u64>,
        lines: Vec<String>,
    },
    /// A merge step with protected changes that is not a plain merge of the PR's head.
    MergeNotClean {
        sha: String,
        prs: Vec<u64>,
        reason: String,
    },
    /// Something could not be checked. Fails closed: this is a flag, never a pass.
    CouldNotCheck {
        sha: Option<String>,
        message: String,
    },
}

/// All zeros: what GitHub sends as `before` when a push creates the branch.
pub fn is_zero_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b == b'0')
}

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    root::git_command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("could not run git")
}

fn git_ok(repo: &Path, args: &[&str]) -> Result<String> {
    let out = git(repo, args)?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_ancestor(repo: &Path, older: &str, newer: &str) -> Result<bool> {
    let out = git(repo, &["merge-base", "--is-ancestor", older, newer])?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git merge-base --is-ancestor failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// The first-parent steps main took, oldest first: `before..after`, or all of `after`'s
/// first-parent history when there is no `before`.
fn main_steps(repo: &Path, before: Option<&str>, after: &str) -> Result<Vec<String>> {
    let range = match before {
        Some(b) => format!("{b}..{after}"),
        None => after.to_string(),
    };
    let out = git_ok(
        repo,
        &["rev-list", "--first-parent", "--reverse", &range, "--"],
    )?;
    Ok(out.lines().map(str::to_string).collect())
}

fn parents(repo: &Path, sha: &str) -> Result<Vec<String>> {
    let out = git_ok(repo, &["rev-list", "--parents", "-n", "1", sha, "--"])?;
    Ok(out.split_whitespace().skip(1).map(str::to_string).collect())
}

fn could_not(sha: Option<&str>, what: &str, e: &anyhow::Error) -> Flag {
    Flag::CouldNotCheck {
        sha: sha.map(str::to_string),
        message: one_line(&format!("{what}: {e:#}")),
    }
}

/// Judges the push `before..after` on main. An empty list means nothing is flagged.
pub fn post_merge_flags(
    repo: &Path,
    before: &str,
    after: &str,
    gh: &dyn GitHubApi,
    cfg: &MergeGuardConfig,
) -> Vec<Flag> {
    let after_sha = match guard::resolve_commit(repo, after) {
        Ok(s) => s,
        Err(e) => return vec![could_not(None, "can't find the pushed commit (after)", &e)],
    };
    if is_zero_sha(before) {
        // R27: the push created main; there is nothing to compare against.
        return Vec::new();
    }
    let before_sha = match guard::resolve_commit(repo, before) {
        Ok(s) => Some(s),
        Err(e) => {
            return vec![could_not(
                None,
                "can't find main as it was before the push (before)",
                &e,
            )];
        }
    };

    let mut flags = Vec::new();
    if let Some(b) = &before_sha {
        match is_ancestor(repo, b, &after_sha) {
            Ok(true) => {}
            Ok(false) => flags.push(Flag::HistoryRewritten {
                before: b.clone(),
                after: after_sha.clone(),
            }),
            Err(e) => {
                flags.push(could_not(None, "can't compare before and after", &e));
                return flags;
            }
        }
    }
    let steps = match main_steps(repo, before_sha.as_deref(), &after_sha) {
        Ok(s) => s,
        Err(e) => {
            flags.push(could_not(None, "can't list the pushed commits", &e));
            return flags;
        }
    };
    let logins = cfg.reviewer_logins();
    flags.extend(
        steps
            .iter()
            .filter_map(|sha| judge_step(repo, sha, gh, cfg, &logins)),
    );
    flags
}

/// Judges one first-parent step of main; `None` when it is acceptable.
fn judge_step(
    repo: &Path,
    sha: &str,
    gh: &dyn GitHubApi,
    cfg: &MergeGuardConfig,
    logins: &[String],
) -> Option<Flag> {
    let parents = match parents(repo, sha) {
        Ok(p) => p,
        Err(e) => return Some(could_not(Some(sha), "can't read its parents", &e)),
    };
    let Some(first) = parents.first() else {
        return Some(Flag::CouldNotCheck {
            sha: Some(sha.to_string()),
            message: "it is a root commit, with nothing to compare it against".to_string(),
        });
    };
    let verdict = match guard::verdict(repo, first, sha) {
        Ok(v) => v,
        Err(e) => {
            return Some(could_not(
                Some(sha),
                "the test-diff guard couldn't judge it",
                &e,
            ));
        }
    };
    // `blocker_lines` of a clean verdict is a "nothing needs approval" sentence, not a blocker.
    let lines = if verdict.needs_approval.is_empty() {
        Vec::new()
    } else {
        blocker_lines(&verdict)
    };
    let pulls = match gh.commit_pulls(sha) {
        Ok(p) => p,
        Err(e) => {
            return Some(could_not(
                Some(sha),
                "looking up the PR that brought it in failed",
                &e,
            ));
        }
    };
    let merged: Vec<&CommitPull> = pulls
        .iter()
        .filter(|p| p.merged && p.base_ref == cfg.base_branch && is_commit_sha(&p.head_sha))
        .collect();
    if merged.is_empty() {
        return Some(Flag::DirectPush {
            sha: sha.to_string(),
            lines,
        });
    }
    if verdict.needs_approval.is_empty() {
        return None;
    }
    let numbers = |ps: &[&CommitPull]| ps.iter().map(|p| p.number).collect::<Vec<_>>();
    let candidates: Vec<&CommitPull> = if parents.len() > 1 {
        // A merge step must be a plain merge of the approved head, or the approval doesn't
        // cover what it brings in.
        let not_clean = |reason: String| {
            Some(Flag::MergeNotClean {
                sha: sha.to_string(),
                prs: numbers(&merged),
                reason,
            })
        };
        if parents.len() != 2 {
            return not_clean(format!(
                "it has {} parents; a PR merge has exactly 2",
                parents.len()
            ));
        }
        let matching: Vec<&CommitPull> = merged
            .iter()
            .copied()
            .filter(|p| p.head_sha == parents[1])
            .collect();
        if matching.is_empty() {
            return not_clean(format!(
                "its second parent {} is not the PR's head",
                short(&parents[1])
            ));
        }
        match merge_differs(repo, &parents[0], &parents[1], sha) {
            Ok(None) => {}
            Ok(Some(reason)) => return not_clean(reason),
            Err(e) => {
                return Some(could_not(Some(sha), "recomputing the merge failed", &e));
            }
        }
        matching
    } else {
        merged.clone()
    };
    let mut errors = Vec::new();
    for p in &candidates {
        match gh.reviews(p.number) {
            Ok(reviews) if app_approved(&reviews, logins, &p.head_sha) => return None,
            Ok(_) => {}
            Err(e) => errors.push(format!(
                "reading the reviews of PR #{} failed: {e:#}",
                p.number
            )),
        }
    }
    if !errors.is_empty() {
        return Some(Flag::CouldNotCheck {
            sha: Some(sha.to_string()),
            message: one_line(&errors.join("; ")),
        });
    }
    Some(Flag::UnapprovedProtected {
        sha: sha.to_string(),
        prs: numbers(&candidates),
        lines,
    })
}

/// `None` when `merge`'s tree is exactly what `git merge-tree --write-tree first second` gives;
/// otherwise why not (a conflict, or a different tree).
fn merge_differs(repo: &Path, first: &str, second: &str, merge: &str) -> Result<Option<String>> {
    let out = git(
        repo,
        &["merge-tree", "--write-tree", "--no-messages", first, second],
    )?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match out.status.code() {
        Some(0) => {}
        Some(1) => {
            return Ok(Some(format!(
                "merging {} into {} has conflicts, so its content was written by hand",
                short(second),
                short(first)
            )));
        }
        _ => bail!(
            "git merge-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
    let clean = stdout.lines().next().unwrap_or("").trim().to_string();
    let actual = git_ok(repo, &["rev-parse", &format!("{merge}^{{tree}}")])?;
    if clean.is_empty() || clean != actual {
        return Ok(Some(format!(
            "its content differs from a clean merge of {} into {} (it carries changes of its own)",
            short(second),
            short(first)
        )));
    }
    Ok(None)
}

fn explain_one(f: &Flag) -> String {
    match f {
        Flag::HistoryRewritten { before, after } => format!(
            "main was rewritten (force-pushed or rewound): {} is not an ancestor of {}, so commits that were on main may be gone.",
            short(before),
            short(after)
        ),
        Flag::DirectPush { sha, lines } if lines.is_empty() => format!(
            "Commit {} was a direct push to main: no merged PR into main brought it in.",
            short(sha)
        ),
        Flag::DirectPush { sha, lines } => format!(
            "Commit {} was a direct push to main: no merged PR into main brought it in. It also changed protected files: {}",
            short(sha),
            lines.join(" ")
        ),
        Flag::UnapprovedProtected { sha, prs, lines } => format!(
            "Commit {} (from PR {}) changed protected files, and the reviewer app hadn't approved the PR's final head commit: {}",
            short(sha),
            prs.iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", "),
            lines.join(" ")
        ),
        Flag::MergeNotClean { sha, prs, reason } => format!(
            "Merge commit {} (PR {}) changed protected files but isn't a plain merge of the PR's head: {reason}. The PR's approval doesn't cover it.",
            short(sha),
            prs.iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Flag::CouldNotCheck {
            sha: Some(sha),
            message,
        } => format!(
            "The post-merge check couldn't check commit {}, so it is flagged: {message}",
            short(sha)
        ),
        Flag::CouldNotCheck { sha: None, message } => {
            format!("The post-merge check couldn't check the push, so it is flagged: {message}")
        }
    }
}

/// Plain language, one line per flag, with control and invisible characters escaped.
pub fn explain_flags(flags: &[Flag]) -> String {
    if flags.is_empty() {
        return "Nothing to flag.".to_string();
    }
    flags
        .iter()
        .map(|f| guard::escape_controls(&one_line(&explain_one(f))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The settings to judge by: committed at `before`, never at `after` (R38). When `before` has
/// none, the check is refused rather than trusting the pushed commit's settings.
fn trusted_config(repo: &Path, before: &str, after: &str) -> Result<MergeGuardConfig> {
    if is_zero_sha(before) {
        bail!("the push created main (before is all zeros), so there are no earlier settings");
    }
    let cfg = match guard::file_at(repo, before, CONFIG_PATH)? {
        Some(text) => MergeGuardConfig::parse(&text)
            .with_context(|| format!("{CONFIG_PATH} at {} is unusable", short(before)))?,
        None => bail!(
            "{CONFIG_PATH} isn't committed at before ({}), so the push to {} is refused: the settings are never read from the pushed commit",
            short(before),
            short(after)
        ),
    };
    if cfg.base_branch != super::github::BASE_BRANCH {
        bail!(
            "{CONFIG_PATH} names the base branch {:?}; the post-merge check only judges {}",
            cfg.base_branch,
            super::github::BASE_BRANCH
        );
    }
    Ok(cfg)
}

/// `lil dev guard --post-merge --before <sha> --after <sha>`: 0 when nothing is flagged, 1 when
/// something is, 2 when the check couldn't run.
pub fn run_post_merge(before: &str, after: &str) -> i32 {
    if is_zero_sha(before) {
        println!("repository creation push; nothing to compare (R27).");
        return 0;
    }
    let repo = match root::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lil dev guard --post-merge: the check couldn't run: {e}");
            return 2;
        }
    };
    let cfg = match trusted_config(&repo, before, after) {
        Ok(x) => x,
        Err(e) => {
            eprintln!(
                "lil dev guard --post-merge: couldn't read the settings, so the check couldn't run: {}",
                guard::escape_controls(&one_line(&format!("{e:#}")))
            );
            return 2;
        }
    };
    let gh = match github_token().and_then(|t| RestClient::new(&cfg.owner, &cfg.repo, &t)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "lil dev guard --post-merge: no GitHub access, so the check couldn't run: {}",
                guard::escape_controls(&one_line(&format!("{e:#}")))
            );
            return 2;
        }
    };
    let flags = post_merge_flags(&repo, before, after, &gh, &cfg);
    println!(
        "Post-merge check of main {}..{}:",
        guard::escape_controls(short(before)),
        guard::escape_controls(short(after))
    );
    for line in explain_flags(&flags).lines() {
        println!("  - {line}");
    }
    i32::from(!flags.is_empty())
}

/// Creates `revert/<after7>-<attempt>` from `after` and reverts main's first-parent steps
/// since `before`, newest first. `attempt` is the workflow run attempt, so a re-run doesn't
/// collide with an earlier run's branch. The branch must end with exactly `before`'s tree.
/// Leaves the working tree on the new branch. Returns the branch name.
pub fn make_revert_branch(repo: &Path, before: &str, after: &str, attempt: u32) -> Result<String> {
    if is_zero_sha(before) {
        bail!(
            "the push created main (before is all zeros), so there is no earlier main to go back to"
        );
    }
    let before_sha = guard::resolve_commit(repo, before)?;
    let after_sha = guard::resolve_commit(repo, after)?;
    let mut steps = main_steps(repo, Some(&before_sha), &after_sha)?;
    if steps.is_empty() {
        bail!("there are no commits between before and after to revert");
    }
    steps.reverse();
    let branch = format!("revert/{}-{attempt}", short(&after_sha));
    git_ok(
        repo,
        &["switch", "--quiet", "--create", &branch, &after_sha],
    )?;
    // `-m 1` reverts a merge commit against main's side; git accepts it for ordinary commits too.
    let mut args = vec!["revert", "--no-edit", "-m", "1"];
    args.extend(steps.iter().map(String::as_str));
    if let Err(e) = git_ok(repo, &args) {
        let _ = git(repo, &["revert", "--abort"]);
        return Err(e.context("git revert stopped (most likely a conflict)"));
    }
    let tree = |rev: &str| git_ok(repo, &["rev-parse", &format!("{rev}^{{tree}}")]);
    if tree("HEAD")? != tree(&before_sha)? {
        bail!(
            "reverting {}..{} didn't restore main as it was at {} (was main rewritten?)",
            short(&before_sha),
            short(&after_sha),
            short(&before_sha)
        );
    }
    Ok(branch)
}

/// Everything the revert PR and the issue say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertReport {
    pub owner: String,
    pub repo: String,
    pub base_branch: String,
    pub before: String,
    pub after: String,
    /// The plain-language post-merge verdict.
    pub verdict: String,
    /// The post-merge test job's result (`success`, `failure`, ...), when known.
    pub tests_result: Option<String>,
    /// A link to the workflow run, when known.
    pub run_url: Option<String>,
}

/// Runs a program (`git` or `gh`) with arguments and optional stdin; returns its stdout.
pub type Runner<'a> = &'a dyn Fn(&str, &[String], Option<&str>) -> Result<String>;

/// `text` (control and invisible characters escaped) in a fenced code block whose backtick fence is longer
/// than any run of backticks inside it, so nothing in it can close the block early. Inside a
/// code block GitHub renders no mentions, links or closing keywords.
pub fn fenced(text: &str) -> String {
    let body = text
        .lines()
        .map(guard::escape_controls)
        .collect::<Vec<_>>()
        .join("\n");
    let mut longest = 0;
    let mut run = 0;
    for c in body.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}text\n{body}\n{fence}\n")
}

fn findings(r: &RevertReport) -> String {
    let mut text = r.verdict.clone();
    if let Some(t) = &r.tests_result {
        text.push_str(&format!(
            "\nThe tests on {} finished as: {}",
            short(&r.after),
            one_line(t)
        ));
    }
    let mut out = format!("What the post-merge check found:\n\n{}", fenced(&text));
    if let Some(url) = &r.run_url {
        out.push_str(&format!("\nThe workflow run: {url}\n"));
    }
    out
}

/// Pushes the revert branch (when there is one), opens the PR into the base branch and opens
/// the issue assigned to the owner. `branch` is the revert branch, or why it couldn't be made.
/// When the push or the PR fails, the issue is still opened, with the verdict and the reason;
/// only a failure to open the issue is an error.
pub fn publish_revert(
    repo: &Path,
    report: &RevertReport,
    branch: std::result::Result<String, String>,
    run: Runner<'_>,
) -> Result<()> {
    let slug = format!("{}/{}", report.owner, report.repo);
    let base = &report.base_branch;
    let range = format!("{}..{}", short(&report.before), short(&report.after));
    let strings = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    let issue_title = format!(
        "Post-merge check failed on {base} at {}",
        short(&report.after)
    );
    let intro = format!(
        "The post-merge check failed for {range} on {base}.\n\n{}\n",
        findings(report)
    );
    let by_hand = |what: &str, reason: &str| {
        format!(
            "{intro}{what}\n\n{}\nRevert the range by hand if the change shouldn't stay on {base}.\n",
            fenced(&one_line(reason))
        )
    };

    let issue_body = match branch {
        Err(reason) => by_hand("The revert couldn't be made automatically:", &reason),
        Ok(b) => {
            let refspec = format!("refs/heads/{b}:refs/heads/{b}");
            let push = strings(&[
                "-C",
                &repo.to_string_lossy(),
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!gh auth git-credential",
                "push",
                "origin",
                &refspec,
            ]);
            match run("git", &push, None) {
                Err(e) => by_hand(
                    &format!("The revert branch `{b}` couldn't be pushed:"),
                    &format!("{e:#}"),
                ),
                Ok(_) => {
                    let pr_body = format!(
                        "This reverts {range} on {base}, because the post-merge check failed.\n\n{}\n\
                         This PR was opened with the workflow's GITHUB_TOKEN, so GitHub doesn't run \
                         any workflows on it. To get the checks `lil dev merge` needs, close and \
                         reopen the PR, or push an empty commit to `{b}` as yourself \
                         (`git commit --allow-empty -m \"run checks\"`).\n",
                        findings(report),
                    );
                    let pr_args = strings(&[
                        "pr",
                        "create",
                        "--repo",
                        &slug,
                        "--base",
                        base,
                        "--head",
                        &b,
                        "--title",
                        &format!("Revert {range}: the post-merge check failed"),
                        "--body-file",
                        "-",
                    ]);
                    match run("gh", &pr_args, Some(&pr_body)) {
                        Err(e) => by_hand(
                            &format!(
                                "The revert branch `{b}` was pushed, but opening its PR failed. Open a PR from `{b}` into {base} yourself:"
                            ),
                            &format!("{e:#}"),
                        ),
                        Ok(pr_url) => format!(
                            "{intro}A revert PR is open: {}\n\n\
                             Look at the findings. If the change was fine, close the revert PR and \
                             this issue; otherwise merge the revert with `lil dev merge`.\n",
                            pr_url.trim(),
                        ),
                    }
                }
            }
        }
    };
    let issue_args = strings(&[
        "issue",
        "create",
        "--repo",
        &slug,
        "--title",
        &issue_title,
        "--assignee",
        &report.owner,
        "--body-file",
        "-",
    ]);
    let issue_url =
        run("gh", &issue_args, Some(&issue_body)).context("opening the issue failed")?;
    println!("Opened {}", issue_url.trim());
    Ok(())
}

/// Runs `program` in `repo` with `args`, feeding `stdin`; fails with its stderr on a nonzero
/// exit.
fn run_program(repo: &Path, program: &str, args: &[String], stdin: Option<&str>) -> Result<String> {
    let mut child = root::scrub_git_env(&mut Command::new(program))
        .args(args)
        .current_dir(repo)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {program}"))?;
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin for {program}"))?
            .write_all(text.as_bytes())
            .with_context(|| format!("writing to {program}"))?;
    }
    let out = child
        .wait_with_output()
        .with_context(|| format!("waiting for {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.first().map(String::as_str).unwrap_or(""),
            one_line(&String::from_utf8_lossy(&out.stderr))
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `GITHUB_RUN_ATTEMPT` (1 for a first run); 1 when unset or not a number.
fn run_attempt_from_env() -> u32 {
    std::env::var("GITHUB_RUN_ATTEMPT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(1)
}

fn run_url_from_env() -> Option<String> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    Some(format!(
        "{}/{}/actions/runs/{}",
        var("GITHUB_SERVER_URL")?,
        var("GITHUB_REPOSITORY")?,
        var("GITHUB_RUN_ID")?
    ))
}

/// `lil dev revert-pr --before <sha> --after <sha> [--tests-result <r>]`: 0 when the PR (or,
/// failing the revert, the issue alone) was opened; 1 otherwise.
pub fn run_revert_pr(before: &str, after: &str, tests_result: Option<&str>) -> i32 {
    if is_zero_sha(before) {
        println!("repository creation push; nothing to compare, so nothing to revert (R27).");
        return 0;
    }
    let repo = match root::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lil dev revert-pr: nothing was done: {e}");
            return 1;
        }
    };
    let fail = |what: &str, e: &anyhow::Error| {
        eprintln!(
            "lil dev revert-pr: {what}: {}",
            guard::escape_controls(&one_line(&format!("{e:#}")))
        );
        1
    };
    let cfg = match trusted_config(&repo, before, after) {
        Ok(x) => x,
        Err(e) => return fail("couldn't read the settings", &e),
    };
    let verdict = match github_token().and_then(|t| RestClient::new(&cfg.owner, &cfg.repo, &t)) {
        Ok(gh) => explain_flags(&post_merge_flags(&repo, before, after, &gh, &cfg)),
        Err(e) => format!(
            "The post-merge judgement couldn't be repeated: {}",
            guard::escape_controls(&one_line(&format!("{e:#}")))
        ),
    };
    let after_sha = guard::resolve_commit(&repo, after).unwrap_or_else(|_| after.to_string());
    let before_sha = if is_zero_sha(before) {
        before.to_string()
    } else {
        guard::resolve_commit(&repo, before).unwrap_or_else(|_| before.to_string())
    };
    let report = RevertReport {
        owner: cfg.owner.clone(),
        repo: cfg.repo.clone(),
        base_branch: cfg.base_branch.clone(),
        before: before_sha.clone(),
        after: after_sha.clone(),
        verdict,
        tests_result: tests_result.map(str::to_string),
        run_url: run_url_from_env(),
    };
    let branch = make_revert_branch(&repo, &before_sha, &after_sha, run_attempt_from_env())
        .map_err(|e| one_line(&format!("{e:#}")));
    if let Err(reason) = &branch {
        eprintln!(
            "lil dev revert-pr: no revert branch, opening only the issue: {}",
            guard::escape_controls(reason)
        );
    }
    let runner = |program: &str, args: &[String], stdin: Option<&str>| {
        run_program(&repo, program, args, stdin)
    };
    match publish_revert(&repo, &report, branch, &runner) {
        Ok(()) => 0,
        Err(e) => fail("couldn't open the revert PR and the issue", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::github::{CheckState, CommitPull, MergeOutcome, PrInfo, Review};
    use crate::dev::github_fake::FakeGitHub;
    use anyhow::anyhow;
    use std::path::PathBuf;
    use std::process::Command;

    const BOT: &str = "worldwright-reviewer[bot]";
    const SPEC: &str = "crates/lil-spec-tests/tests/smoke.rs";

    fn cfg() -> MergeGuardConfig {
        MergeGuardConfig::parse(include_str!("../../../../dev/merge-guard.toml")).unwrap()
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

    fn commit_t(dir: &Path, msg: &str) -> String {
        git_t(dir, &["add", "-A"]);
        git_t(dir, &["commit", "-q", "-m", msg]);
        git_t(dir, &["rev-parse", "HEAD"])
    }

    /// R38: the settings are read only from `before`. When `before` has none, the check refuses
    /// (and CI opens the plain issue) instead of trusting the pushed commit's settings.
    #[test]
    fn settings_missing_at_before_are_a_refusal_not_a_fallback_to_after() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        git_t(&dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git_t(&dir, &["config", "user.name", "Post Merge Test"]);
        git_t(&dir, &["config", "user.email", "post-merge@test.invalid"]);
        git_t(&dir, &["config", "commit.gpgsign", "false"]);
        write_t(
            &dir,
            "README.md",
            "x
",
        );
        let before = commit_t(&dir, "no settings yet");
        write_t(
            &dir,
            CONFIG_PATH,
            include_str!("../../../../dev/merge-guard.toml"),
        );
        let after = commit_t(&dir, "settings arrive with the push");

        let err = trusted_config(&dir, &before, &after).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("isn't committed at before"), "{text}");
        assert!(text.contains("refused"), "{text}");

        // With settings at before, they are the ones used.
        write_t(
            &dir,
            "README.md",
            "y
",
        );
        let later = commit_t(&dir, "later push");
        let cfg = trusted_config(&dir, &after, &later).unwrap();
        assert_eq!(cfg.repo, self::cfg().repo);
    }

    /// A repo whose `main` protects `crates/lil-spec-tests/**`. Returns the dir and `before`.
    fn repo() -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        git_t(&dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git_t(&dir, &["config", "user.name", "Post Merge Test"]);
        git_t(&dir, &["config", "user.email", "post-merge@test.invalid"]);
        git_t(&dir, &["config", "commit.gpgsign", "false"]);
        git_t(&dir, &["config", "core.autocrlf", "false"]);
        write_t(
            &dir,
            "dev/protected-paths.txt",
            "crates/lil-spec-tests/**\n",
        );
        write_t(&dir, SPEC, "#[test]\nfn spec_harness_runs() {}\n");
        write_t(&dir, "README.md", "hello\n");
        let before = commit_t(&dir, "base");
        (tmp, dir, before)
    }

    /// A two-commit PR branch off main, merged into main with a merge commit (as
    /// `lil dev merge` does). Returns (the PR's final head, the merge commit).
    fn merge_pr(dir: &Path, branch: &str, path: &str, text: &str) -> (String, String) {
        git_t(dir, &["checkout", "-q", "-b", branch, "main"]);
        write_t(dir, "notes.txt", branch);
        commit_t(dir, "first PR commit");
        write_t(dir, path, text);
        let head = commit_t(dir, "second PR commit");
        git_t(dir, &["checkout", "-q", "main"]);
        git_t(
            dir,
            &["merge", "-q", "--no-ff", "-m", "Merge pull request", branch],
        );
        (head, git_t(dir, &["rev-parse", "HEAD"]))
    }

    fn direct_commit(dir: &Path, path: &str, text: &str) -> String {
        write_t(dir, path, text);
        commit_t(dir, "pushed straight to main")
    }

    /// Wraps the fake and fails `reviews` with an error.
    struct ReviewsFail<'a>(&'a FakeGitHub);

    impl GitHubApi for ReviewsFail<'_> {
        fn pr(&self, n: u64) -> Result<PrInfo> {
            self.0.pr(n)
        }
        fn check(&self, sha: &str, name: &str) -> Result<CheckState> {
            self.0.check(sha, name)
        }
        fn reviews(&self, _: u64) -> Result<Vec<Review>> {
            Err(anyhow!("HTTP 502 from GitHub (reviews)"))
        }
        fn merge(&self, n: u64, sha: &str) -> Result<MergeOutcome> {
            self.0.merge(n, sha)
        }
        fn post_review_as_app(&self, _: u64, _: &str, _: &str, _: &str) -> Result<()> {
            panic!("post-merge must never post reviews")
        }
        fn commit_pulls(&self, sha: &str) -> Result<Vec<CommitPull>> {
            self.0.commit_pulls(sha)
        }
    }

    // ---- the brief's tests ----------------------------------------------------------------

    #[test]
    fn post_merge_flags_direct_push() {
        let (_tmp, dir, before) = repo();
        let after = direct_commit(&dir, "README.md", "hello again\n");
        let fake = FakeGitHub::new();
        let flags = post_merge_flags(&dir, &before, &after, &fake, &cfg());
        assert_eq!(
            flags,
            vec![Flag::DirectPush {
                sha: after.clone(),
                lines: vec![]
            }]
        );
        let text = explain_flags(&flags);
        assert!(text.contains("direct push to main"), "{text}");
        assert!(text.contains(&after[..7]), "{text}");
    }

    #[test]
    fn post_merge_flags_unapproved_protected_change() {
        let (_tmp, dir, before) = repo();
        let (head, merge) = merge_pr(
            &dir,
            "spec",
            SPEC,
            "#[test]\nfn spec_harness_runs() { assert!(true) }\n",
        );
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        // Approvals that do not count: a human's, the App's on another commit.
        fake.add_review(7, "xenofei", "APPROVED", &head);
        fake.add_review(7, BOT, "APPROVED", &before);
        let flags = post_merge_flags(&dir, &before, &merge, &fake, &cfg());
        assert!(
            matches!(&flags[..], [Flag::UnapprovedProtected { sha, prs, lines }]
                if *sha == merge && *prs == vec![7] && lines.iter().any(|l| l.contains(SPEC))),
            "{flags:?}"
        );
        let text = explain_flags(&flags);
        assert!(text.contains("#7"), "{text}");
        assert!(text.contains(SPEC), "{text}");
        assert!(text.contains("reviewer app"), "{text}");
    }

    #[test]
    fn post_merge_passes_approved_pr() {
        let (_tmp, dir, before) = repo();
        let (head, merge) = merge_pr(
            &dir,
            "spec",
            SPEC,
            "#[test]\nfn spec_harness_runs() { assert!(true) }\n",
        );
        let (docs_head, docs_merge) = merge_pr(&dir, "docs", "README.md", "hello again\n");
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        fake.add_review(7, BOT, "APPROVED", &head);
        // A PR without protected changes needs no approval.
        fake.add_commit_pull(&docs_merge, 8, &docs_head, "main", true);
        assert_eq!(
            post_merge_flags(&dir, &before, &docs_merge, &fake, &cfg()),
            vec![]
        );
        assert_eq!(explain_flags(&[]), "Nothing to flag.");
    }

    /// Plan 2026-10-17, D6: the App's approval counts under its current slug or the next one
    /// in the settings at `before`, and under no other login.
    #[test] // spec: QA-035
    fn post_merge_counts_either_login() {
        let (_tmp, dir, before) = repo();
        let (head, merge) = merge_pr(
            &dir,
            "spec",
            SPEC,
            "#[test]\nfn spec_harness_runs() { assert!(true) }\n",
        );
        for login in [BOT, "lilith-reviewer[bot]"] {
            let fake = FakeGitHub::new();
            fake.add_commit_pull(&merge, 7, &head, "main", true);
            fake.add_review(7, login, "APPROVED", &head);
            assert_eq!(
                post_merge_flags(&dir, &before, &merge, &fake, &cfg()),
                vec![],
                "{login}"
            );
        }

        // People named like either slug, other Apps, and the next slug once the settings at
        // `before` don't name it: all flagged.
        let only_current = MergeGuardConfig {
            reviewer_app_slug_next: None,
            ..cfg()
        };
        for (login, settings) in [
            ("lilith-reviewer", cfg()),
            ("worldwright-reviewer", cfg()),
            ("github-actions[bot]", cfg()),
            ("lilith-reviewer-next[bot]", cfg()),
            ("lilith-reviewer[bot]", only_current),
        ] {
            let fake = FakeGitHub::new();
            fake.add_commit_pull(&merge, 7, &head, "main", true);
            fake.add_review(7, login, "APPROVED", &head);
            let flags = post_merge_flags(&dir, &before, &merge, &fake, &settings);
            assert!(
                matches!(&flags[..], [Flag::UnapprovedProtected { sha, .. }] if *sha == merge),
                "{login}: {flags:?}"
            );
        }

        // The latest review across both logins decides.
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        fake.add_review(7, "lilith-reviewer[bot]", "APPROVED", &head);
        fake.add_review(7, BOT, "DISMISSED", &head);
        let flags = post_merge_flags(&dir, &before, &merge, &fake, &cfg());
        assert!(
            matches!(&flags[..], [Flag::UnapprovedProtected { .. }]),
            "{flags:?}"
        );
    }

    // ---- fail closed and the edges ---------------------------------------------------------

    #[test]
    fn post_merge_github_errors_are_flags() {
        let (_tmp, dir, before) = repo();
        let (head, merge) = merge_pr(&dir, "spec", SPEC, "#[test]\nfn x() {}\n");
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        fake.add_review(7, BOT, "APPROVED", &head);

        // The PR lookup fails: a flag, never "no PR" (which would also flag) or a pass.
        fake.fail_commit_pulls(&merge, "HTTP 502");
        let flags = post_merge_flags(&dir, &before, &merge, &fake, &cfg());
        assert!(
            matches!(&flags[..], [Flag::CouldNotCheck { sha: Some(s), message }]
                if *s == merge && message.contains("502")),
            "{flags:?}"
        );

        // The reviews lookup fails, even though the App approved: a flag.
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        fake.add_review(7, BOT, "APPROVED", &head);
        let flags = post_merge_flags(&dir, &before, &merge, &ReviewsFail(&fake), &cfg());
        assert!(
            matches!(&flags[..], [Flag::CouldNotCheck { sha: Some(s), message }]
                if *s == merge && message.contains("502")),
            "{flags:?}"
        );
        assert!(explain_flags(&flags).contains("couldn't"));

        // A revision git doesn't know: a flag.
        let flags = post_merge_flags(&dir, &before, "no-such-rev", &fake, &cfg());
        assert!(
            matches!(&flags[..], [Flag::CouldNotCheck { .. }]),
            "{flags:?}"
        );
    }

    #[test]
    fn post_merge_needs_a_pr_merged_into_main() {
        let (_tmp, dir, before) = repo();
        let after = direct_commit(&dir, "README.md", "x\n");
        for (base, merged) in [("dev", true), ("main", false)] {
            let fake = FakeGitHub::new();
            fake.add_commit_pull(&after, 9, &after, base, merged);
            assert_eq!(
                post_merge_flags(&dir, &before, &after, &fake, &cfg()),
                vec![Flag::DirectPush {
                    sha: after.clone(),
                    lines: vec![]
                }],
                "{base} merged={merged}"
            );
        }
    }

    #[test]
    fn post_merge_direct_push_lists_its_protected_changes() {
        let (_tmp, dir, before) = repo();
        let after = direct_commit(&dir, SPEC, "// gone\n");
        let flags = post_merge_flags(&dir, &before, &after, &FakeGitHub::new(), &cfg());
        assert!(
            matches!(&flags[..], [Flag::DirectPush { lines, .. }] if lines.iter().any(|l| l.contains(SPEC))),
            "{flags:?}"
        );
    }

    #[test]
    fn post_merge_merge_commit_must_merge_the_approved_head() {
        let (_tmp, dir, before) = repo();
        let (head, merge) = merge_pr(&dir, "spec", SPEC, "#[test]\nfn x() {}\n");
        let first_pr_commit = git_t(&dir, &["rev-parse", &format!("{head}^")]);
        // GitHub says the PR's head was an earlier commit, and the App approved that one: the
        // merge commit merged something else, so the approval does not cover it.
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &first_pr_commit, "main", true);
        fake.add_review(7, BOT, "APPROVED", &first_pr_commit);
        let flags = post_merge_flags(&dir, &before, &merge, &fake, &cfg());
        assert!(
            matches!(&flags[..], [Flag::MergeNotClean { reason, prs, .. }]
                if reason.contains("second parent") && *prs == vec![7]),
            "{flags:?}"
        );
    }

    /// I2: an "evil merge" whose merge commit carries a protected change of its own is flagged
    /// even though the PR's head is approved.
    #[test]
    fn post_merge_flags_a_merge_that_carries_extra_protected_changes() {
        let (_tmp, dir, before) = repo();
        git_t(&dir, &["checkout", "-q", "-b", "docs", "main"]);
        write_t(&dir, "README.md", "harmless\n");
        let head = commit_t(&dir, "docs only");
        git_t(&dir, &["checkout", "-q", "main"]);
        git_t(&dir, &["merge", "-q", "--no-ff", "--no-commit", "docs"]);
        // The extra, unreviewed change rides in the merge commit itself.
        write_t(&dir, SPEC, "// the spec test is gone\n");
        git_t(&dir, &["add", "-A"]);
        git_t(&dir, &["commit", "-q", "-m", "Merge pull request"]);
        let merge = git_t(&dir, &["rev-parse", "HEAD"]);
        assert_eq!(
            git_t(&dir, &["rev-parse", &format!("{merge}^2")]),
            head,
            "the second parent is the approved head"
        );
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        fake.add_review(7, BOT, "APPROVED", &head);
        let flags = post_merge_flags(&dir, &before, &merge, &fake, &cfg());
        assert!(
            matches!(&flags[..], [Flag::MergeNotClean { reason, .. }]
                if reason.contains("differs from a clean merge")),
            "{flags:?}"
        );
        let text = explain_flags(&flags);
        assert!(text.contains("isn't a plain merge"), "{text}");
    }

    /// I2: a merge commit with three parents is never a plain PR merge.
    #[test]
    fn post_merge_flags_an_octopus_merge() {
        let (_tmp, dir, before) = repo();
        git_t(&dir, &["checkout", "-q", "-b", "a", "main"]);
        write_t(&dir, SPEC, "// changed by a\n");
        let head = commit_t(&dir, "a");
        git_t(&dir, &["checkout", "-q", "-b", "b", "main"]);
        write_t(&dir, "b.txt", "b\n");
        commit_t(&dir, "b");
        git_t(&dir, &["checkout", "-q", "main"]);
        git_t(&dir, &["merge", "-q", "--no-ff", "-m", "octopus", "a", "b"]);
        let merge = git_t(&dir, &["rev-parse", "HEAD"]);
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        fake.add_review(7, BOT, "APPROVED", &head);
        let flags = post_merge_flags(&dir, &before, &merge, &fake, &cfg());
        assert!(
            matches!(&flags[..], [Flag::MergeNotClean { reason, .. }] if reason.contains("3 parents")),
            "{flags:?}"
        );
    }

    /// R27: a push that created main is not judged and never reaches GitHub.
    #[test]
    fn post_merge_skips_repository_creation() {
        struct Panics;
        impl GitHubApi for Panics {
            fn pr(&self, _: u64) -> Result<PrInfo> {
                panic!("network")
            }
            fn check(&self, _: &str, _: &str) -> Result<CheckState> {
                panic!("network")
            }
            fn reviews(&self, _: u64) -> Result<Vec<Review>> {
                panic!("network")
            }
            fn merge(&self, _: u64, _: &str) -> Result<MergeOutcome> {
                panic!("network")
            }
            fn post_review_as_app(&self, _: u64, _: &str, _: &str, _: &str) -> Result<()> {
                panic!("network")
            }
            fn commit_pulls(&self, _: &str) -> Result<Vec<CommitPull>> {
                panic!("network")
            }
        }
        let (_tmp, dir, _) = repo();
        let after = direct_commit(&dir, "README.md", "x\n");
        let zero = "0".repeat(40);
        assert_eq!(
            post_merge_flags(&dir, &zero, &after, &Panics, &cfg()),
            vec![]
        );
        assert!(make_revert_branch(&dir, &zero, &after, 1).is_err());
        assert_eq!(run_post_merge(&zero, &after), 0);
        assert_eq!(run_revert_pr(&zero, &after, Some("failure")), 0);
    }

    #[test]
    fn post_merge_judges_only_main_steps_and_every_one_of_them() {
        let (_tmp, dir, before) = repo();
        let (head, merge) = merge_pr(&dir, "spec", SPEC, "#[test]\nfn x() {}\n");
        let after = direct_commit(&dir, "README.md", "late\n");
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        fake.add_review(7, BOT, "APPROVED", &head);
        // The PR's own commits are covered by its merge commit; only the direct one is flagged.
        assert_eq!(
            post_merge_flags(&dir, &before, &after, &fake, &cfg()),
            vec![Flag::DirectPush {
                sha: after.clone(),
                lines: vec![]
            }]
        );
    }

    #[test]
    fn post_merge_flags_rewritten_history() {
        let (_tmp, dir, before) = repo();
        let old_tip = direct_commit(&dir, "README.md", "one\n");
        git_t(&dir, &["reset", "-q", "--hard", &before]);
        let (head, merge) = merge_pr(&dir, "docs", "README.md", "two\n");
        let fake = FakeGitHub::new();
        fake.add_commit_pull(&merge, 7, &head, "main", true);
        let flags = post_merge_flags(&dir, &old_tip, &merge, &fake, &cfg());
        assert_eq!(
            flags,
            vec![Flag::HistoryRewritten {
                before: old_tip.clone(),
                after: merge.clone()
            }]
        );
        assert!(explain_flags(&flags).contains("rewritten"));
    }

    #[test]
    fn explain_escapes_control_characters() {
        let flags = vec![Flag::CouldNotCheck {
            sha: None,
            message: "bad\u{1b}[2Kpath\nnext".into(),
        }];
        let text = explain_flags(&flags);
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert_eq!(text.lines().count(), 1, "{text:?}");
    }

    // ---- revert-pr -------------------------------------------------------------------------

    #[test]
    fn revert_branch_undoes_the_range() {
        let (_tmp, dir, before) = repo();
        let (_, _merge) = merge_pr(&dir, "spec", SPEC, "#[test]\nfn x() {}\n");
        let after = direct_commit(&dir, "README.md", "late\n");
        let branch = make_revert_branch(&dir, &before, &after, 2).unwrap();
        assert_eq!(branch, format!("revert/{}-2", &after[..7]));
        assert_eq!(
            git_t(&dir, &["rev-parse", &format!("{branch}^{{tree}}")]),
            git_t(&dir, &["rev-parse", &format!("{before}^{{tree}}")]),
            "the revert branch must restore before's tree"
        );
        // The branch builds on after; main itself is untouched.
        git_t(&dir, &["merge-base", "--is-ancestor", &after, &branch]);
        assert_eq!(git_t(&dir, &["rev-parse", "main"]), after);
    }

    type Calls = std::cell::RefCell<Vec<(String, Vec<String>, Option<String>)>>;

    fn report() -> RevertReport {
        RevertReport {
            owner: "xenofei".into(),
            repo: "LILITH".into(),
            base_branch: "main".into(),
            before: "1111111111111111111111111111111111111111".into(),
            after: "abcdef0123456789abcdef0123456789abcdef01".into(),
            verdict: "Commit abcdef0 was a direct push to main: no merged PR brought it in.".into(),
            tests_result: Some("failure".into()),
            run_url: Some("https://github.com/xenofei/LILITH/actions/runs/5".into()),
        }
    }

    fn recorder(calls: &Calls) -> impl Fn(&str, &[String], Option<&str>) -> Result<String> + '_ {
        |program: &str, args: &[String], stdin: Option<&str>| {
            calls.borrow_mut().push((
                program.to_string(),
                args.to_vec(),
                stdin.map(str::to_string),
            ));
            Ok(if args.iter().any(|a| a == "pr") {
                "https://github.com/xenofei/LILITH/pull/9\n".to_string()
            } else {
                "https://github.com/xenofei/LILITH/issues/10\n".to_string()
            })
        }
    }

    fn has(args: &[String], pair: [&str; 2]) -> bool {
        args.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1])
    }

    #[test]
    fn revert_publish_pushes_and_opens_a_pr_and_an_issue() {
        let calls = Calls::default();
        let run = recorder(&calls);
        let dir = Path::new("C:/repo");
        publish_revert(dir, &report(), Ok("revert/abcdef0".into()), &run).unwrap();
        let calls = calls.take();
        assert_eq!(calls.len(), 3, "{calls:?}");

        // 1. The push uses gh as the credential helper (the token stays in GH_TOKEN, never in
        //    .git/config or on the command line) and pushes only the revert branch.
        let (prog, args, _) = &calls[0];
        assert_eq!(prog, "git");
        assert!(has(args, ["-c", "credential.helper="]), "{args:?}");
        assert!(
            has(args, ["-c", "credential.helper=!gh auth git-credential"]),
            "{args:?}"
        );
        assert!(
            args.ends_with(&[
                "push".into(),
                "origin".into(),
                "refs/heads/revert/abcdef0:refs/heads/revert/abcdef0".into()
            ]),
            "{args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|a| a.contains("no-verify") || a.contains("--force")),
            "{args:?}"
        );

        // 2. The PR: from the revert branch into main, body on stdin.
        let (prog, args, body) = &calls[1];
        assert_eq!(prog, "gh");
        assert!(
            args.starts_with(&["pr".into(), "create".into()]),
            "{args:?}"
        );
        assert!(has(args, ["--repo", "xenofei/LILITH"]), "{args:?}");
        assert!(has(args, ["--base", "main"]), "{args:?}");
        assert!(has(args, ["--head", "revert/abcdef0"]), "{args:?}");
        assert!(has(args, ["--body-file", "-"]), "{args:?}");
        let body = body.as_deref().unwrap();
        assert!(body.contains("direct push to main"), "{body}");
        assert!(body.contains("actions/runs/5"), "{body}");
        assert!(body.contains("close and reopen"), "{body}");

        // 3. The issue: assigned to the owner, links the PR, carries the verdict.
        let (prog, args, body) = &calls[2];
        assert_eq!(prog, "gh");
        assert!(
            args.starts_with(&["issue".into(), "create".into()]),
            "{args:?}"
        );
        assert!(has(args, ["--assignee", "xenofei"]), "{args:?}");
        assert!(has(args, ["--repo", "xenofei/LILITH"]), "{args:?}");
        let body = body.as_deref().unwrap();
        assert!(body.contains("/pull/9"), "{body}");
        assert!(body.contains("direct push to main"), "{body}");
        assert!(body.contains("failure"), "{body}");
    }

    #[test]
    fn revert_without_a_branch_opens_only_the_issue() {
        let calls = Calls::default();
        let run = recorder(&calls);
        publish_revert(
            Path::new("C:/repo"),
            &report(),
            Err("git revert hit a conflict".into()),
            &run,
        )
        .unwrap();
        let calls = calls.take();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let (prog, args, body) = &calls[0];
        assert_eq!(prog, "gh");
        assert!(
            args.starts_with(&["issue".into(), "create".into()]),
            "{args:?}"
        );
        let body = body.as_deref().unwrap();
        assert!(body.contains("conflict"), "{body}");
        assert!(body.contains("by hand"), "{body}");
    }

    #[test]
    fn revert_publish_failure_is_an_error() {
        let failing = |_: &str, _: &[String], _: Option<&str>| -> Result<String> {
            Err(anyhow!("gh: HTTP 403"))
        };
        assert!(
            publish_revert(
                Path::new("C:/repo"),
                &report(),
                Ok("revert/abcdef0".into()),
                &failing
            )
            .is_err()
        );
    }

    #[test]
    fn revert_pr_failure_after_push_still_opens_an_issue_with_the_verdict() {
        let calls = Calls::default();
        let run = |program: &str, args: &[String], stdin: Option<&str>| -> Result<String> {
            calls
                .borrow_mut()
                .push((program.into(), args.to_vec(), stdin.map(str::to_string)));
            if args.iter().any(|a| a == "pr") {
                Err(anyhow!(
                    "gh: GitHub Actions is not permitted to create pull requests"
                ))
            } else {
                Ok("https://github.com/xenofei/LILITH/issues/10\n".into())
            }
        };
        publish_revert(
            Path::new("C:/repo"),
            &report(),
            Ok("revert/abcdef0-1".into()),
            &run,
        )
        .unwrap();
        let calls = calls.take();
        assert_eq!(calls.len(), 3, "{calls:?}");
        let (prog, args, body) = &calls[2];
        assert_eq!(prog, "gh");
        assert!(
            args.starts_with(&["issue".into(), "create".into()]),
            "{args:?}"
        );
        let body = body.as_deref().unwrap();
        assert!(body.contains("direct push to main"), "{body}");
        assert!(body.contains("revert/abcdef0-1"), "{body}");
        assert!(body.contains("not permitted"), "{body}");
    }

    #[test]
    fn revert_push_failure_still_opens_an_issue() {
        let calls = Calls::default();
        let run = |program: &str, args: &[String], stdin: Option<&str>| -> Result<String> {
            calls
                .borrow_mut()
                .push((program.into(), args.to_vec(), stdin.map(str::to_string)));
            if program == "git" {
                Err(anyhow!("git push failed: rejected"))
            } else {
                Ok("https://github.com/xenofei/LILITH/issues/10\n".into())
            }
        };
        publish_revert(
            Path::new("C:/repo"),
            &report(),
            Ok("revert/abcdef0-1".into()),
            &run,
        )
        .unwrap();
        let calls = calls.take();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[1].1.starts_with(&["issue".into(), "create".into()]));
        assert!(calls[1].2.as_deref().unwrap().contains("rejected"));
    }

    /// Lines of `body` outside fenced code blocks.
    fn outside_fences(body: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut open: Option<String> = None;
        for line in body.lines() {
            let t = line.trim();
            let ticks: String = t.chars().take_while(|c| *c == '`').collect();
            match &open {
                None if ticks.len() >= 3 => open = Some(ticks),
                Some(f) if t == f.as_str() => open = None,
                None => out.push(line.to_string()),
                Some(_) => {}
            }
        }
        assert!(open.is_none(), "unclosed fence in {body}");
        out
    }

    #[test]
    fn bodies_fence_attacker_influenced_text() {
        let hostile = "Commit abcdef0: the protected file `x` was modified.\n\
                       @xenofei Fixes #1 [click](https://evil.example) ````\n```\nstill inside \u{1b}[2K";
        let mut r = report();
        r.verdict = hostile.into();
        r.tests_result = Some("failure @someone".into());
        let calls = Calls::default();
        let run = recorder(&calls);
        publish_revert(
            Path::new("C:/repo"),
            &r,
            Ok("revert/abcdef0-1".into()),
            &run,
        )
        .unwrap();
        publish_revert(
            Path::new("C:/repo"),
            &r,
            Err("conflict in `@xenofei` Fixes #2".into()),
            &run,
        )
        .unwrap();
        let calls = calls.take();
        let bodies: Vec<&str> = calls.iter().filter_map(|c| c.2.as_deref()).collect();
        assert_eq!(bodies.len(), 3, "PR, issue, issue");
        for body in bodies {
            assert!(body.contains("@xenofei"), "{body}");
            assert!(!body.contains('\u{1b}'), "{body:?}");
            for line in outside_fences(body) {
                for bad in ["@", "Fixes #", "](", "evil.example"] {
                    assert!(
                        !line.contains(bad),
                        "`{bad}` outside a fence: {line}\n{body}"
                    );
                }
            }
        }
        // The fence is longer than the longest backtick run inside.
        assert!(fenced("a ```` b").starts_with("`````text\n"));
        assert!(fenced("plain").starts_with("```text\n"));
        // R38: bidi overrides and invisible characters are escaped too.
        let body = fenced("a\u{202E}b \u{200B}c\u{FEFF}");
        assert!(
            !body
                .chars()
                .any(|c| c != '\n' && guard::is_dangerous_char(c)),
            "{body:?}"
        );
        assert!(body.contains(r"a\u{202e}b \u{200b}c\u{feff}"), "{body}");
    }
}
