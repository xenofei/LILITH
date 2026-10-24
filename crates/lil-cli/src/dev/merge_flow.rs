//! The one-click merge flow and the merge gate (QA-036 as amended 2026-10-23, P4a-R38; plan 4a
//! Task 28).
//!
//! **`lil dev merge-flow <PR> --summary <file> --audit <file>`.** An agent may prepare and run it
//! for a PR you approved on its card. It reads what lil itself can check about the PR
//! ([`Facts`]: the head commit, the files, the protected changes, CI on that exact head), shows
//! you that with the agent's summary and the independent audit (a review written by an agent
//! that didn't make the change), and asks for your authentication: Windows Hello first, then the
//! passphrase ([`Authenticator`]). Only after it succeeds does it post the reviewer approval
//! (when protected files changed), wait for CI on that head, and merge through `lil dev merge`'s
//! checks. Without your authentication nothing is approved or merged ([`merge_flow`]).
//!
//! **Every merge asks for your authentication** ([`MergeGate`]), whoever runs it and whether or
//! not the PR touches protected files: `lil dev merge` run on its own asks on every merge
//! ([`AskOnEveryMerge`]); the flow's merge is let through only for the PR and the exact head you
//! authenticated for ([`Authenticated`], which only [`merge_flow`] can make).
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
    /// One plain line per protected change that needs the reviewer approval; empty when none.
    pub protected: Vec<String>,
    /// Each required CI check on `head`, with its state.
    pub checks: Vec<(String, String)>,
    /// Whether the reviewer App already approved `head`.
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
        out.push_str("Protected changes (merging them posts the reviewer approval):\n");
        for p in &facts.protected {
            let _ = writeln!(out, "  {p}");
        }
        if facts.already_approved {
            out.push_str("  (the reviewer App already approved this commit)\n");
        }
    }
    let _ = writeln!(out, "CI on {}:", short(&facts.head));
    for (name, state) in &facts.checks {
        let _ = writeln!(out, "  {name}: {state}");
    }
    out
}

/// A merge's authentication: [`MergeGate::authorize`] runs after every other merge condition
/// held and just before GitHub is asked to merge. An error refuses the merge.
pub trait MergeGate {
    fn authorize(&self, pr: u64, head: &str) -> std::result::Result<(), String>;
}

/// `lil dev merge` on its own: asks you to authenticate on every merge, protected files or not.
pub struct AskOnEveryMerge<'a> {
    auth: &'a dyn Authenticator,
}

impl<'a> AskOnEveryMerge<'a> {
    pub fn new(auth: &'a dyn Authenticator) -> Self {
        Self { auth }
    }
}

impl MergeGate for AskOnEveryMerge<'_> {
    fn authorize(&self, pr: u64, head: &str) -> std::result::Result<(), String> {
        let summary = format!(
            "Merge PR #{pr} at {} (head commit {head}) into main.\nCI is green on that commit, and protected changes, if any, carry the reviewer approval.",
            short(head)
        );
        self.auth
            .authenticate(&summary, NO_AUDIT)
            .map(drop)
            .map_err(|e| format!("every merge needs your authentication, and it didn't succeed ({e}); nothing was merged"))
    }
}

/// The flow's authentication, already given for exactly one PR and head. Only [`merge_flow`]
/// makes one, after you authenticated.
pub struct Authenticated {
    pr: u64,
    head: String,
}

impl MergeGate for Authenticated {
    fn authorize(&self, pr: u64, head: &str) -> std::result::Result<(), String> {
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
    /// Posts the reviewer App's approval of `facts.head`, signed with the unlocked key. `true`
    /// when GitHub accepted a re-run of the guard check, which then turns green.
    fn post_approval(&self, pem: &[u8], facts: &Facts) -> Result<bool>;
    /// Waits for every required CI check on `facts.head` to pass. A failed guard check is
    /// waited on only when `guard_rerun` (the approval just asked for its re-run); any other
    /// failure ends the wait at once.
    fn wait_for_checks(&self, facts: &Facts, guard_rerun: bool) -> Result<()>;
    /// Merges through `lil dev merge`'s checks, which ask `gate` last.
    fn merge(&self, gate: &dyn MergeGate) -> Result<String>;
}

/// The flow: the facts, then your authentication with the summary and the audit shown first,
/// then (only after it succeeded) the approval when protected files changed, CI, and the merge.
/// Returns the merge commit.
pub fn merge_flow(
    summary: &str,
    audit: &str,
    steps: &dyn MergeFlowSteps,
    auth: &dyn Authenticator,
) -> Result<String> {
    let facts = steps.facts()?;
    let shown = format!(
        "{}\nSummary of the change (written by the agent that ran the flow):\n{}",
        render_facts(&facts),
        quoted(summary)
    );
    let pem = auth
        .authenticate(&shown, audit)
        .map_err(|e| anyhow!("{e}; nothing was approved or merged"))?;
    let guard_rerun = if !facts.protected.is_empty() && !facts.already_approved {
        steps.post_approval(&pem, &facts)?
    } else {
        false
    };
    drop(pem);
    steps.wait_for_checks(&facts, guard_rerun)?;
    let gate = Authenticated {
        pr: facts.pr,
        head: facts.head.clone(),
    };
    steps.merge(&gate)
}
