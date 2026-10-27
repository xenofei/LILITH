//! The one-click merge flow and the merge gate (QA-036 as amended 2026-10-23 and 2026-10-26,
//! QA-033 as amended 2026-10-27, P4a-R38; plan 4a Task 28).
//!
//! **`lil dev merge-flow <PR> --summary <file> --audit <file>`.** An agent may prepare and run it
//! for a PR you approved on its card. It reads what lil itself can check about the PR
//! ([`Facts`]: the head commit, the files, the protected changes, CI on that exact head). Then it
//! waits, with nothing unlocked and nothing asked, until every other required check on that head
//! is green ([`Wait::BeforeAuth`], owner decision 2026-10-27: CI takes longer than a wait with the
//! key unlocked should last), and reads the PR again. A failure, a moved head, a PR that is no
//! longer open, Ctrl+C during the wait or the wait running out asks nothing; from the end of the
//! wait until you have authenticated, Ctrl+C ends lil at once, as it did before (review I-1).
//! Only then does it show
//! you what it read the second time with the agent's summary and the independent audit (a review
//! written by an agent that didn't make the change), and ask for your authentication: Windows
//! Hello first, then the passphrase ([`Authenticator`]). Only after it succeeds does it check
//! again that every other required check on that head is green ([`Wait::Others`]), then post the
//! reviewer App's approval of that head, wait for the guard check, and merge through
//! `lil dev merge`'s checks. Without your authentication nothing is approved or merged
//! ([`merge_flow`]).
//!
//! **An approval is a standing grant for its commit** (review rounds 1 and 2): while it stands,
//! and while the guard check on that commit is green, anyone who can call GitHub's merge API can
//! merge it. So the flow asks for nothing while another required check has already failed
//! ([`failed_check`]), posts the approval only once every other required check is green
//! ([`Wait::Others`]), and any exit after posting that doesn't merge (a CI failure, the wait
//! running out, a moved head, an error, Ctrl+C, a panic) withdraws it again
//! ([`MergeFlowSteps::withdraw_approval`]: the dismissal, then a re-run of the guard check until
//! it reports failure) and says exactly what holds. Closing the console window, a logoff, sleep
//! or a kill can't be caught; then the approval stays until you dismiss it.
//! `lil dev merge <PR>` run on its own is the same flow without the summary and the audit
//! ([`merge_alone`]).
//!
//! **Every merge asks for your authentication, and GitHub enforces it** (owner decision
//! 2026-10-26): whoever runs it and whether or not the PR touches protected files, the approval
//! is posted only after you authenticated, and the protected-test check, which the ruleset on
//! `main` requires, fails on a PR whose head the reviewer App hasn't approved (QA-021). The App's
//! key is unlocked only by your authentication, so the agent token alone can't merge. The merge
//! itself is let through only for the PR and the exact head you authenticated for
//! ([`Authenticated`], which only the flow can make).
//!
//! This module needs no GitHub and no git: the steps that do are behind [`MergeFlowSteps`] (the
//! real ones are `reviewer.rs`'s), so `tests/reviewer_hello.rs` compiles it on its own.

use super::reviewer_hello::{Authenticator, quoted};
use anyhow::{Context as _, Result, anyhow, bail};
use std::fmt::Write as _;

/// The check the approval turns green (`ci-guard.yml`'s job name). A failure of any other
/// required check refuses the merge before you are asked anything.
pub const GUARD_CHECK: &str = "protected-test-check";

/// What the audit slot says when `lil dev merge` runs on its own.
pub const NO_AUDIT: &str =
    "(none: `lil dev merge` was run on its own; `lil dev merge-flow` adds an independent audit)";

/// What lil itself checked about the PR, shown above the agent's summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub pr: u64,
    /// The PR's title, its author's login, its base branch and its head branch, as GitHub
    /// reports them (shown only).
    pub title: String,
    pub author: String,
    pub base_ref: String,
    pub head_ref: String,
    /// The PR's head commit everything was checked on.
    pub head: String,
    /// The files the PR changes against `origin/main`.
    pub files: Vec<String>,
    /// One plain line per protected change; empty when none.
    pub protected: Vec<String>,
    /// Each required CI check on `head`, with its state.
    pub checks: Vec<(String, String)>,
    /// Whether the reviewer App already approved `head` (shown only: every merge posts the
    /// approval again).
    pub already_approved: bool,
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// The facts as you read them.
pub fn render_facts(facts: &Facts) -> String {
    let given = |s: &str| {
        if s.is_empty() {
            "(not given)".to_string()
        } else {
            s.to_string()
        }
    };
    let mut out = format!(
        "PR #{}: {}\nAuthor: {}\nInto {} from {}, at {} (head commit {})\n",
        facts.pr,
        given(&facts.title),
        given(&facts.author),
        given(&facts.base_ref),
        given(&facts.head_ref),
        short(&facts.head),
        facts.head
    );
    let _ = writeln!(out, "Files changed ({}):", facts.files.len());
    for f in &facts.files {
        let _ = writeln!(out, "  {f}");
    }
    if facts.protected.is_empty() {
        out.push_str("Protected files: none changed.\n");
    } else {
        out.push_str("Protected changes:\n");
        for p in &facts.protected {
            let _ = writeln!(out, "  {p}");
        }
    }
    let _ = writeln!(
        out,
        "Merging first posts the reviewer App's approval of {}: protected-test-check needs it on every PR.",
        short(&facts.head)
    );
    if facts.already_approved {
        out.push_str("  (the reviewer App already approved this commit)\n");
    }
    let _ = writeln!(out, "CI on {}:", short(&facts.head));
    for (name, state) in &facts.checks {
        let _ = writeln!(out, "  {name}: {state}");
    }
    out
}

/// The merge a [`MergeGate`] is asked about: the PR, the head commit every condition was
/// checked on, and what is shown of it (the title and author as GitHub reports them, escaped by
/// the caller; the configured base branch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeSubject<'a> {
    pub pr: u64,
    pub head: &'a str,
    pub title: &'a str,
    pub author: &'a str,
    pub base_branch: &'a str,
}

/// A merge's authentication: [`MergeGate::authorize`] runs after every other merge condition
/// held and just before GitHub is asked to merge. An error refuses the merge.
pub trait MergeGate {
    fn authorize(&self, subject: &MergeSubject) -> std::result::Result<(), String>;
}

/// The flow's authentication, already given for exactly one PR and head. Only the flow
/// ([`merge_flow`], [`merge_alone`]) makes one, after you authenticated.
pub struct Authenticated {
    pr: u64,
    head: String,
}

impl MergeGate for Authenticated {
    fn authorize(&self, s: &MergeSubject) -> std::result::Result<(), String> {
        let (pr, head) = (s.pr, s.head);
        if pr == self.pr && head == self.head {
            Ok(())
        } else {
            Err(format!(
                "you authenticated PR #{} at {}, not PR #{pr} at {}; nothing was merged",
                self.pr,
                short(&self.head),
                short(head)
            ))
        }
    }
}

/// The flow's steps that touch git and GitHub. The real ones are in `reviewer.rs`.
pub trait MergeFlowSteps {
    /// Reads the PR (read-only; nothing is posted or changed).
    fn facts(&self) -> Result<Facts>;
    /// Posts the reviewer App's approval of `facts.head`, signed with the unlocked key, on
    /// every merge. `true` when GitHub accepted a re-run of the guard check, which then turns
    /// green.
    fn post_approval(&self, pem: &[u8], facts: &Facts) -> Result<bool>;
    /// Waits for required CI checks on `facts.head` to pass ([`Wait`]). Any failure ends the
    /// wait at once, except a failed guard check while its re-run was asked for.
    fn wait_for_checks(&self, facts: &Facts, wait: Wait) -> Result<()>;
    /// Merges through `lil dev merge`'s checks, which ask `gate` last.
    fn merge(&self, gate: &dyn MergeGate) -> Result<String>;
    /// Withdraws the reviewer App's approval of `facts.head`, signed with the unlocked key: the
    /// flow stopped without merging after it may have posted it. `Ok` is lil's line saying
    /// exactly what holds now (withdrawn and the guard check red, withdrawn but the check not
    /// red yet, or nothing to withdraw); `Err` when the approval may still stand.
    fn withdraw_approval(&self, pem: &[u8], facts: &Facts) -> Result<String>;
    /// Whether Ctrl+C came while lil was catching it. The flow looks right before it asks for
    /// your authentication.
    fn interrupted(&self) -> bool;
}

/// Which checks [`MergeFlowSteps::wait_for_checks`] waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Every required check but [`GUARD_CHECK`], before you are asked for your authentication
    /// (owner decision 2026-10-27). Nothing is unlocked or asked yet, so this wait may last
    /// longer than the ones after it.
    BeforeAuth,
    /// Every required check but [`GUARD_CHECK`], after your authentication and before the
    /// approval is posted.
    Others,
    /// [`GUARD_CHECK`] after the approval; `rerun` when its re-run was asked for, so a failed
    /// run is waited on.
    Guard { rerun: bool },
}

/// How a failed check reads while a newer run of its workflow is in progress on the same commit
/// (ci-time review I-1): pending, never failed or passed.
pub const PENDING_NEWER_RUN: &str = "pending (a newer run is in progress)";

/// The first required check other than [`GUARD_CHECK`] that already finished as anything but
/// success, with its state. Pending and not yet reported checks may still pass.
pub fn failed_check(facts: &Facts) -> Option<(&str, &str)> {
    facts
        .checks
        .iter()
        .find(|(name, state)| {
            name != GUARD_CHECK
                && !matches!(
                    state.as_str(),
                    "success" | "pending" | "missing" | PENDING_NEWER_RUN
                )
        })
        .map(|(n, s)| (n.as_str(), s.as_str()))
}

/// Stops the flow, asking nothing, when another required check already failed on the head
/// (review round 1, I-3).
fn refuse_failed(facts: &Facts) -> Result<()> {
    if let Some((name, state)) = failed_check(facts) {
        bail!(
            "CI check {name} finished as {state} on {}; nothing was asked, approved or merged. Fix it, then run the merge again",
            short(&facts.head)
        );
    }
    Ok(())
}

/// Whether every required check other than [`GUARD_CHECK`] is green: only then are you asked.
fn others_green(facts: &Facts) -> bool {
    facts
        .checks
        .iter()
        .all(|(name, state)| name == GUARD_CHECK || state == "success")
}

/// What the flow says when the PR's head moved while it waited for CI, before you were asked.
/// `with_summary` for `lil dev merge-flow`, whose summary and audit were for the old head.
pub fn moved_while_waiting(pr: u64, from: &str, to: &str, with_summary: bool) -> String {
    let read = if with_summary {
        format!("; the summary and audit were for {}", short(from))
    } else {
        String::new()
    };
    format!(
        "PR #{pr} moved from {} to {} while waiting for CI{read}; nothing was asked, approved or merged",
        short(from),
        short(to)
    )
}

/// How many lines of the agent's summary are shown (re-review m3): a long one mustn't bury the
/// independent audit below it.
pub const SUMMARY_LINES: usize = 60;

/// How many characters of the agent's summary are shown (re-review 2 n1): the window wraps long
/// lines, so one long line could bury the audit too.
pub const SUMMARY_CHARS: usize = 4_000;

/// The part of `text` that is shown: at most `max_lines` lines and `max_chars` characters (the
/// newlines between the lines don't count). When anything was cut, also lil's own line saying
/// so, to be shown after the quoted text, never inside it.
pub fn capped(text: &str, max_lines: usize, max_chars: usize) -> (String, Option<String>) {
    let text = text.trim_end();
    let mut kept = String::new();
    let mut chars = 0;
    let mut end = 0;
    'lines: for (i, line) in text.lines().enumerate() {
        if i == max_lines {
            break;
        }
        if i > 0 {
            kept.push('\n');
        }
        for c in line.chars() {
            if chars == max_chars {
                break 'lines;
            }
            kept.push(c);
            chars += 1;
        }
        end = i + 1;
    }
    let all: usize = text.lines().map(|l| l.chars().count()).sum();
    if chars == all && end == text.lines().count() {
        return (kept, None);
    }
    let rest = all - chars;
    let s = if rest == 1 { "" } else { "s" };
    let cut = format!(
        "(lil cut the agent's summary here: {rest} more character{s} not shown. The whole text is in the --summary file.)"
    );
    (kept, Some(cut))
}

/// The flow: the facts, the wait for CI with nothing unlocked, the facts again, then your
/// authentication with those facts, the summary and the audit shown first, then (only after it
/// succeeded) the reviewer App's approval of the shown head, CI, and the merge of that head.
/// Returns the merge commit.
pub fn merge_flow(
    summary: &str,
    audit: &str,
    steps: &dyn MergeFlowSteps,
    auth: &dyn Authenticator,
) -> Result<String> {
    flow(Some(summary), audit, steps, auth)
}

/// `lil dev merge <PR>` run on its own: the flow with no agent summary and no audit
/// ([`NO_AUDIT`]). It asks for your authentication on every merge, protected files or not.
pub fn merge_alone(steps: &dyn MergeFlowSteps, auth: &dyn Authenticator) -> Result<String> {
    flow(None, NO_AUDIT, steps, auth)
}

fn flow(
    summary: Option<&str>,
    audit: &str,
    steps: &dyn MergeFlowSteps,
    auth: &dyn Authenticator,
) -> Result<String> {
    let first = steps.facts()?;
    // An approval is a standing grant for that commit (review round 1, I-3): it is posted only
    // when the merge can follow, so a failed check stops the flow before you are asked.
    refuse_failed(&first)?;
    // Owner decision 2026-10-27: CI takes longer than the wait with the key unlocked should
    // last, so the flow waits for it first, with nothing unlocked and nothing asked.
    // The read the wait goes on from: its checks are the ones still not green.
    let mut seen = first.clone();
    let facts = loop {
        steps.wait_for_checks(&seen, Wait::BeforeAuth)?;
        // What you are shown and authenticate is read again now, CI green.
        let again = steps.facts().with_context(|| {
            format!(
                "PR #{} was read again after the wait for CI, and nothing was asked",
                first.pr
            )
        })?;
        if again.head != first.head {
            bail!(
                "{}",
                moved_while_waiting(first.pr, &first.head, &again.head, summary.is_some())
            );
        }
        refuse_failed(&again)?;
        if others_green(&again) {
            break again;
        }
        // A check went back to pending or unreported since the wait (a green run re-run, say):
        // wait again, within what is left of the same budget (review M-5).
        seen = again;
    };
    let mut shown = render_facts(&facts);
    match summary {
        Some(summary) => {
            let (kept, cut) = capped(summary, SUMMARY_LINES, SUMMARY_CHARS);
            let _ = write!(
                shown,
                "\nSummary of the change (written by the agent that ran the flow):\n{}",
                quoted(&kept)
            );
            if let Some(cut) = cut {
                shown.push_str(&cut);
                shown.push('\n');
            }
        }
        None => shown.push_str(
            "\nSummary of the change: (none: `lil dev merge` was run on its own; `lil dev merge-flow` adds the agent's summary)\n",
        ),
    }
    // Review I-1: a Ctrl+C that came after the wait's last look still stops the flow before you
    // are asked (from the end of that wait, Ctrl+C ends lil at once).
    if steps.interrupted() {
        bail!(
            "stopped by Ctrl+C before your authentication; nothing was asked, approved or merged"
        );
    }
    let pem = auth
        .authenticate(&shown, audit)
        .map_err(|e| anyhow!("{e}; nothing was approved or merged"))?;
    // The approval comes at the last moment (review round 2, I-2): every other required check
    // must be green first, so a failure or the wait running out here posts nothing.
    steps.wait_for_checks(&facts, Wait::Others)?;
    let gate = Authenticated {
        pr: facts.pr,
        head: facts.head.clone(),
    };
    // Every merge, protected files or not (owner decision 2026-10-26): the protected-test check
    // passes only once the App approved this exact head. From here on, any exit that doesn't
    // merge withdraws the approval again, a panic included (review round 2, M-1); otherwise that
    // commit could be merged later, by anyone who can call the merge API, without you. The key
    // stays unlocked until then, for that one call.
    let posted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let guard_rerun = steps.post_approval(&pem, &facts)?;
        steps.wait_for_checks(&facts, Wait::Guard { rerun: guard_rerun })?;
        steps.merge(&gate)
    }));
    let outcome = match posted {
        Ok(Ok(merge_sha)) => Ok(merge_sha),
        Ok(Err(e)) => Err(anyhow!(
            "{e:#}; {}",
            withdrawn(steps.withdraw_approval(&pem, &facts), &facts)
        )),
        Err(panic) => {
            eprintln!(
                "lil stopped on an internal error after the approval; {}",
                withdrawn(steps.withdraw_approval(&pem, &facts), &facts)
            );
            drop(pem);
            std::panic::resume_unwind(panic);
        }
    };
    drop(pem);
    outcome
}

/// What the flow says after a withdrawal: the step's own line, or that the approval may stand.
fn withdrawn(result: Result<String>, facts: &Facts) -> String {
    match result {
        Ok(line) => line,
        Err(w) => format!(
            "the reviewer App's approval of {} could NOT be withdrawn ({w:#}), so it may still stand: anyone who can call GitHub's merge API can merge that commit while protected-test-check is green, and if the PR is closed, reopening it turns the check green again. Don't reopen or merge it without a new authentication. Dismiss the approval on the PR's page, then re-run protected-test-check (the run's page, Re-run all jobs)",
            short(&facts.head)
        ),
    }
}
