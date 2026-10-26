//! The one-click merge flow and the merge gate (QA-036 as amended 2026-10-23 and 2026-10-26,
//! P4a-R38; plan 4a Task 28).
//!
//! **`lil dev merge-flow <PR> --summary <file> --audit <file>`.** An agent may prepare and run it
//! for a PR you approved on its card. It reads what lil itself can check about the PR
//! ([`Facts`]: the head commit, the files, the protected changes, CI on that exact head), shows
//! you that with the agent's summary and the independent audit (a review written by an agent
//! that didn't make the change), and asks for your authentication: Windows Hello first, then the
//! passphrase ([`Authenticator`]). Only after it succeeds does it post the reviewer App's
//! approval of that head, wait for CI on that head, and merge through `lil dev merge`'s checks.
//! Without your authentication nothing is approved or merged ([`merge_flow`]).
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
use anyhow::{Result, anyhow};
use std::fmt::Write as _;

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
    /// Waits for every required CI check on `facts.head` to pass. A failed guard check is
    /// waited on only when `guard_rerun` (the approval just asked for its re-run); any other
    /// failure ends the wait at once.
    fn wait_for_checks(&self, facts: &Facts, guard_rerun: bool) -> Result<()>;
    /// Merges through `lil dev merge`'s checks, which ask `gate` last.
    fn merge(&self, gate: &dyn MergeGate) -> Result<String>;
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

/// The flow: the facts, then your authentication with the summary and the audit shown first,
/// then (only after it succeeded) the reviewer App's approval of the shown head, CI, and the
/// merge of that head. Returns the merge commit.
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
    let facts = steps.facts()?;
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
    let pem = auth
        .authenticate(&shown, audit)
        .map_err(|e| anyhow!("{e}; nothing was approved or merged"))?;
    // Every merge, protected files or not (owner decision 2026-10-26): the protected-test check
    // passes only once the App approved this exact head.
    let guard_rerun = steps.post_approval(&pem, &facts)?;
    drop(pem);
    steps.wait_for_checks(&facts, guard_rerun)?;
    let gate = Authenticated {
        pr: facts.pr,
        head: facts.head.clone(),
    };
    steps.merge(&gate)
}
