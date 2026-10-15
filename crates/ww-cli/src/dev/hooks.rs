//! Git hooks that keep direct changes out of `main` (QA-034, rulings R15-R17) and personal data
//! out of every pushed ref (QA-086, ruling R32).
//!
//! `tools/git-hooks/pre-push` and `tools/git-hooks/pre-merge-commit` call `ww dev hook ...`.
//! Agents get changes into `main` only through `ww dev merge <PR>`, so a push to `main` or a
//! local merge commit on `main` is refused.
//!
//! - **Privacy** ([`privacy_scans`], [`privacy_pre_push`]): every pushed ref, on any branch, gets
//!   the privacy scan ([`privacy::scan_push`]): the pushed ref's name, the annotated tag it
//!   points at (if any), and its commits: `remote..local` for an existing ref, and for a new
//!   ref the commits that nothing the remote currently has reaches (`git ls-remote <push URL>`,
//!   never the possibly stale remote-tracking refs). Each such commit's added lines, paths,
//!   message and identities are scanned. Deletions publish nothing and aren't scanned. A
//!   finding, a missing denylist, a failed `ls-remote` or a scan that can't run refuses the
//!   push. Until the denylist exists (`%USERPROFILE%\.worldwright\privacy-denylist.txt`),
//!   every push is refused.
//! - **Pushes** ([`pre_push`]): a push to `main` is refused unless it only moves `main` forward
//!   (`remote_sha` is an ancestor of `local_sha`; a rollback, a force-push or rewritten history
//!   is refused), the guard verdict for the range has no protected-file blockers (no approval is
//!   possible on a direct push), and CI's `every-push` check passed on the pushed commit. A
//!   push that creates or deletes `main` is refused. Pushes to `main` on *any* remote are
//!   refused, not just `origin`; the check keys on the remote ref name.
//! - **Merges** ([`pre_merge_commit`]): on `main`, every merge commit is refused without being
//!   judged (R16). Git runs this hook before it records `MERGE_HEAD`, so the merged commit
//!   can't be seen, and git never runs the hook once `MERGE_HEAD` exists.
//!
//! **What the merge hook can't see.** Git does not run `pre-merge-commit` for a fast-forward
//! merge, for the `git commit` that concludes `git merge --no-commit`, or for a commit that
//! resolves a conflicted merge. `pre-push` is the backstop for all of those: `main` can't
//! reach a remote except through it.
//!
//! **Settings** are read from the committed `origin/main` ([`merge::committed_config`]), never
//! from the working tree (ruling R12). The pure functions take the settings, the guard verdict,
//! the ancestry test and the GitHub client as parameters and are tested with closures and the
//! fake; the CLI glue ([`run_pre_push`], [`run_pre_merge_commit`]) loads the real ones.
//!
//! **Fail closed (R15).** If the settings, the ancestry, the verdict or the GitHub check cannot
//! be read, a push to `main` is refused. The `main` rules need no network call or settings read
//! for a push that doesn't touch `main`, so feature-branch pushes work offline and before the
//! settings exist on `origin/main`.
//!
//! **Scripts (R17, updated by R32).** The `pre-push` script hands every push that names at least
//! one ref to `cargo run` (the privacy scan applies to every branch); only an empty push list
//! passes without a build. The `pre-merge-commit` script still filters cheaply and hands off
//! only on `main`. Because the hand-off is `cargo run`, a cargo lock held by another build can
//! make a push wait until that build finishes.

use super::agent_access::GIT_CREDENTIAL_HELPER;
use super::github::{BASE_BRANCH, CheckState, GitHubApi, RestClient, github_token};
use super::guard::{self, Verdict};
use super::merge::{self, MergeGuardConfig};
use super::privacy::{self, PushScan};
use super::root;
#[cfg(test)]
use super::spec_tests::workspace_root;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// What `core.hooksPath` is set to: the folder holding the hook scripts.
pub const HOOKS_PATH: &str = "tools/git-hooks";

/// The CI check that must have passed on a commit before it may reach `main` directly.
const EVERY_PUSH: &str = "every-push";

const REFUSAL_HEAD: &str = "Pushes to main go through `ww dev merge <PR>`.";

/// The first words of a refusal by the privacy scan.
const PRIVACY_HEAD: &str = "Privacy scan (QA-086): this push is refused.";

/// One `<local ref> <local sha> <remote ref> <remote sha>` line from git's pre-push stdin.
struct PushLine<'a> {
    remote_ref: &'a str,
    local_sha: &'a str,
    remote_sha: &'a str,
}

/// `None` when the line is not exactly four fields.
fn parse_line(line: &str) -> Option<PushLine<'_>> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    match fields.as_slice() {
        [_local_ref, local_sha, remote_ref, remote_sha] => Some(PushLine {
            remote_ref,
            local_sha,
            remote_sha,
        }),
        _ => None,
    }
}

fn is_all_zero(sha: &str) -> bool {
    !sha.is_empty() && sha.chars().all(|c| c == '0')
}

/// `true` when any push line targets `refs/heads/<branch>`, or is malformed (a line that can't
/// be read can't be shown to leave `branch` alone). When `false`, the push needs no settings
/// and no network.
pub fn touches_branch(stdin_lines: &str, branch: &str) -> bool {
    let target = format!("refs/heads/{branch}");
    stdin_lines
        .lines()
        .filter(|l| !l.trim().is_empty())
        .any(|l| parse_line(l).is_none_or(|p| p.remote_ref == target))
}

/// Judges the range `base..head` that is about to land on `main`. Returns the plain reasons it
/// must be refused (empty when it may go ahead).
fn range_reasons(
    base: &str,
    head: &str,
    verdict_for: &dyn Fn(&str, &str) -> Result<Verdict>,
    is_ancestor: &dyn Fn(&str, &str) -> Result<bool>,
    gh: &dyn GitHubApi,
) -> Vec<String> {
    // Main may only move forward. The guard diffs from the merge-base, so a rollback or a
    // rewrite would show no blocker; this check comes first and stops the others.
    match is_ancestor(base, head) {
        Ok(true) => {}
        Ok(false) => {
            return vec![format!(
                "main can only move forward: {} isn't an ancestor of {}, so this would roll back or rewrite main.",
                short(base),
                short(head)
            )];
        }
        Err(e) => {
            return vec![format!(
                "Couldn't tell whether this only moves main forward, so it is refused: {}",
                one_line(&format!("{e:#}"))
            )];
        }
    }
    let mut reasons = Vec::new();
    match verdict_for(base, head) {
        Err(e) => reasons.push(format!(
            "The test-diff guard couldn't judge the change, so it is refused: {}",
            one_line(&format!("{e:#}"))
        )),
        Ok(v) if !v.needs_approval.is_empty() => reasons.push(format!(
            "The change touches protected files, and approval isn't possible on a direct push: {}",
            merge::blocker_lines(&v).join(" ")
        )),
        Ok(_) => {}
    }
    match gh.check(head, EVERY_PUSH) {
        Ok(CheckState::Success) => {}
        Ok(CheckState::Pending) => reasons.push(format!(
            "CI check {EVERY_PUSH} is still running on {}.",
            short(head)
        )),
        Ok(CheckState::Missing) => reasons.push(format!(
            "CI check {EVERY_PUSH} hasn't reported for {}.",
            short(head)
        )),
        Ok(CheckState::Failure(conclusion)) => reasons.push(format!(
            "CI check {EVERY_PUSH} finished as {conclusion}, not success, on {}.",
            short(head)
        )),
        Err(e) => reasons.push(format!(
            "Couldn't read CI check {EVERY_PUSH} on {} from GitHub, so the change is refused: {}",
            short(head),
            one_line(&format!("{e:#}"))
        )),
    }
    reasons
}

fn refusal(reasons: Vec<String>) -> String {
    reasons
        .into_iter()
        .map(|r| format!("{REFUSAL_HEAD} {r}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Decides a push. `stdin_lines` is git's pre-push stdin. Every line whose remote ref is
/// `refs/heads/<base_branch>` is refused when:
///
/// - the remote sha is all zeros (the push would create `main`) or the local sha is all zeros
///   (it would delete `main`);
/// - `remote_sha` is not an ancestor of `local_sha` (or that can't be told): main only moves
///   forward;
/// - the guard verdict for `remote_sha..local_sha` can't be read or has blockers;
/// - `every-push` isn't `Success` on `local_sha` (or can't be read);
/// - `every-push` isn't one of the settings' `required_checks` (the names disagree).
///
/// Blank lines are ignored; a malformed line is refused. Lines for other branches pass.
pub fn pre_push(
    stdin_lines: &str,
    cfg: &MergeGuardConfig,
    verdict_for: &dyn Fn(&str, &str) -> Result<Verdict>,
    is_ancestor: &dyn Fn(&str, &str) -> Result<bool>,
    gh: &dyn GitHubApi,
) -> Result<(), String> {
    let target = format!("refs/heads/{}", cfg.base_branch);
    let mut reasons = Vec::new();
    for line in stdin_lines.lines().filter(|l| !l.trim().is_empty()) {
        let Some(p) = parse_line(line) else {
            reasons.push(format!(
                "Git sent a push line this hook can't read ({:?}), so the push is refused.",
                one_line(line)
            ));
            continue;
        };
        if p.remote_ref != target {
            continue;
        }
        if !cfg.required_checks.iter().any(|c| c == EVERY_PUSH) {
            reasons.push(format!(
                "The merge-guard settings don't list the CI check {EVERY_PUSH}, so this hook can't tell whether CI passed."
            ));
        } else if is_all_zero(p.remote_sha) {
            reasons.push(format!("This push would create {}.", cfg.base_branch));
        } else if is_all_zero(p.local_sha) {
            reasons.push(format!("This push would delete {}.", cfg.base_branch));
        } else {
            reasons.extend(range_reasons(
                p.remote_sha,
                p.local_sha,
                verdict_for,
                is_ancestor,
                gh,
            ));
        }
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(refusal(reasons))
    }
}

/// Which pushed refs need a privacy scan (QA-086, rulings R32 and R33), in stdin order:
///
/// - a deletion (local sha all zeros) publishes nothing, so it needs none;
/// - a new remote ref (remote sha all zeros) is [`PushScan::NewBranch`]: the commits reachable
///   from the local sha that nothing the remote has right now reaches. `remote_tips` lists what
///   the pushed remote has (`git ls-remote`); it is asked at most once, and only ids this
///   repository has (`have_commit`) are kept, so an unknown id means scanning more, never less.
///   Remote-tracking refs are never trusted: a deleted remote branch leaves a stale one;
/// - an existing remote ref is [`PushScan::Range`] `remote..local`, unless `have_commit` says
///   the remote's old commit isn't in this repository (someone else pushed it, or this is a
///   force-push over history this clone never had); then it is scanned as a new branch.
///
/// If `remote_tips` fails, the push is refused. Blank and malformed lines are skipped here:
/// [`pre_push`] refuses a malformed line.
pub fn privacy_scans(
    stdin_lines: &str,
    have_commit: &dyn Fn(&str) -> bool,
    remote_tips: &dyn Fn() -> Result<Vec<String>>,
) -> Result<Vec<PushScan>, String> {
    let mut tips: Option<Vec<String>> = None;
    let mut out = Vec::new();
    for p in stdin_lines.lines().filter_map(parse_line) {
        if is_all_zero(p.local_sha) {
            continue;
        }
        if is_all_zero(p.remote_sha) || !have_commit(p.remote_sha) {
            if tips.is_none() {
                let listed = remote_tips().map_err(|e| {
                    format!(
                        "{PRIVACY_HEAD} Couldn't ask the remote what it already has (git ls-remote), so the push is refused: {}",
                        one_line(&format!("{e:#}"))
                    )
                })?;
                tips = Some(listed.into_iter().filter(|t| have_commit(t)).collect());
            }
            out.push(PushScan::NewBranch {
                remote_ref: p.remote_ref.to_string(),
                head: p.local_sha.to_string(),
                exclude: tips.clone().unwrap_or_default(),
            });
        } else {
            out.push(PushScan::Range {
                remote_ref: p.remote_ref.to_string(),
                base: p.remote_sha.to_string(),
                head: p.local_sha.to_string(),
            });
        }
    }
    Ok(out)
}

/// Runs `scan` (which returns printable finding lines) on every pushed ref. Any finding, or any
/// scan that can't run, refuses the push. A finding reported for two refs is shown once; two
/// findings of one ref that print alike (two different addresses on one line both mask to
/// `j***@***`) both stay (ruling R37, M3): each printed line is kept as many times as the ref
/// that has it most often.
pub fn privacy_pre_push(
    scans: &[PushScan],
    scan: &dyn Fn(&PushScan) -> Result<Vec<String>>,
) -> Result<(), String> {
    let mut order: Vec<String> = Vec::new();
    let mut most: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in scans {
        match scan(s) {
            Ok(found) => {
                let mut here: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for f in found {
                    *here.entry(f.clone()).or_default() += 1;
                    if !most.contains_key(&f) {
                        most.insert(f.clone(), 0);
                        order.push(f);
                    }
                }
                for (f, n) in here {
                    let m = most.entry(f).or_default();
                    *m = (*m).max(n);
                }
            }
            Err(e) => {
                return Err(format!(
                    "{PRIVACY_HEAD} The privacy scan couldn't run, so the push is refused: {}",
                    one_line(&format!("{e:#}"))
                ));
            }
        }
    }
    let lines: Vec<&str> = order
        .iter()
        .flat_map(|f| std::iter::repeat_n(f.as_str(), most[f]))
        .collect();
    if lines.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{PRIVACY_HEAD} Nothing personal may enter the public repository; remove or replace each of these, rewriting the commits that hold them:\n{}",
        lines.join("\n")
    ))
}

/// Decides a merge commit (R16). `head_ref` is the full ref `HEAD` points at (for example
/// `refs/heads/main`). On `refs/heads/<base_branch>` every merge commit is refused without
/// being judged; other branches pass. A detached `HEAD` never reaches this function.
///
/// Fast-forward merges, `git merge --no-commit` + `git commit`, and conflict-resolution commits
/// don't run this hook; [`pre_push`] is the backstop.
pub fn pre_merge_commit(head_ref: &str, base_branch: &str) -> Result<(), String> {
    if head_ref == format!("refs/heads/{base_branch}") {
        Err(format!(
            "Merge commits on {base_branch} aren't allowed locally; run `git merge --abort`. Changes reach {base_branch} only through `ww dev merge <PR>`."
        ))
    } else {
        Ok(())
    }
}

/// Points `core.hooksPath` at [`HOOKS_PATH`] in the repo's local config, and writes the
/// repository-local credential helper entries, after removing any local URL-specific
/// `credential.<url>.helper` keys: an empty `credential.helper` (which resets
/// inherited helpers such as Git Credential Manager), then [`GIT_CREDENTIAL_HELPER`]. Running
/// it again leaves exactly those two entries.
pub fn install_hooks(repo: &Path) -> Result<()> {
    install_hooks_with(repo, &|_| {})
}

/// [`install_hooks`], with `adjust` applied to every git command (tests isolate git's global
/// and system config with it).
pub(crate) fn install_hooks_with(repo: &Path, adjust: &dyn Fn(&mut Command)) -> Result<()> {
    let git = |args: &[&str]| -> Result<std::process::Output> {
        let mut cmd = root::git_command();
        cmd.arg("-C").arg(repo).args(args);
        adjust(&mut cmd);
        cmd.output().context("could not run git")
    };
    // Local URL-specific helpers (`credential.<url>.helper`) would be read after ours, so
    // they go. Other `credential.<url>.*` settings stay.
    let listed = git(&[
        "config",
        "--local",
        "--null",
        "--name-only",
        "--get-regexp",
        r"^credential\..+\.helper$",
    ])?;
    match listed.status.code() {
        Some(0) => {
            let names = String::from_utf8_lossy(&listed.stdout).into_owned();
            let mut keys: Vec<&str> = names.split_terminator('\0').collect();
            keys.dedup();
            for key in keys {
                let out = git(&["config", "--local", "--unset-all", key])?;
                if !out.status.success() {
                    bail!(
                        "git config --unset-all {key} failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
            }
        }
        Some(1) => {}
        _ => bail!(
            "git config --get-regexp credential helpers failed: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        ),
    }
    // The empty entry drops every helper git inherited (Git Credential Manager, gh's helper)
    // and any earlier local one; the second entry is then the only helper git uses here.
    let steps: [&[&str]; 3] = [
        &["config", "core.hooksPath", HOOKS_PATH],
        &[
            "config",
            "--local",
            "--replace-all",
            "credential.helper",
            "",
        ],
        &[
            "config",
            "--local",
            "--add",
            "credential.helper",
            GIT_CREDENTIAL_HELPER,
        ],
    ];
    for args in steps {
        let out = git(args)?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args[..args.len() - 1].join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    Ok(())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

// ---------------------------------------------------------------------------------------------
// CLI glue.
// ---------------------------------------------------------------------------------------------

fn git_out(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    root::git_command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("could not run git")
}

/// The full ref `HEAD` points at (`refs/heads/main`), or `Ok(None)` when `HEAD` is detached.
/// The full ref is used because `--short` prints `heads/main` when a tag named `main` exists.
fn current_head_ref(repo: &Path) -> Result<Option<String>> {
    let out = git_out(repo, &["symbolic-ref", "-q", "HEAD"])?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => bail!(
            "git symbolic-ref HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// `git merge-base --is-ancestor <ancestor> <descendant>`: exit 0 is `true`, exit 1 is `false`,
/// anything else (a missing object, a bad revision) is an error.
fn git_is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let out = git_out(repo, &["merge-base", "--is-ancestor", ancestor, descendant])?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git merge-base --is-ancestor failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// The committed settings and a GitHub client, or the plain reason they aren't available.
fn load_real(repo: &Path) -> Result<(MergeGuardConfig, RestClient), String> {
    let cfg = merge::committed_config(repo).map_err(|r| merge::explain(&[r]))?;
    let no_access = |e: anyhow::Error| {
        format!(
            "No GitHub access, so the change is refused: {}",
            one_line(&format!("{e:#}"))
        )
    };
    let token = github_token().map_err(no_access)?;
    let client = RestClient::new(&cfg.owner, &cfg.repo, &token).map_err(no_access)?;
    Ok((cfg, client))
}

fn report(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

/// What the privacy scan asks `git ls-remote` (ruling R34): the URL git passed to the hook,
/// which is the push destination after `remote.<name>.pushurl` and `pushInsteadOf`, or the
/// remote's name when git passed no URL. Asking the name would list the fetch URL's refs, which
/// can hold commits the push destination lacks.
pub fn ls_remote_target<'a>(remote: Option<&'a str>, url: Option<&'a str>) -> Option<&'a str> {
    url.filter(|u| !u.is_empty())
        .or(remote.filter(|r| !r.is_empty()))
}

/// The privacy scan for every pushed ref, with the real denylist. The only network call is
/// `git ls-remote <push URL>`, made only when a ref is new to the remote.
fn privacy_real(repo: &Path, remote: Option<&str>, stdin: &str) -> Result<(), String> {
    let tips = || -> Result<Vec<String>> {
        let remote = remote.ok_or_else(|| anyhow::anyhow!("git passed no remote or URL"))?;
        privacy::remote_tips(repo, remote)
    };
    let scans = privacy_scans(stdin, &|sha| privacy::has_commit(repo, sha), &tips)?;
    if scans.is_empty() {
        return Ok(());
    }
    let deny = privacy::real_denylist().map_err(|m| format!("{PRIVACY_HEAD} {m}"))?;
    for w in deny.warnings() {
        eprintln!("Privacy scan (QA-086): warning: {w}");
    }
    let scan = |s: &PushScan| -> Result<Vec<String>> {
        Ok(privacy::scan_push(repo, s, &deny)?
            .iter()
            .map(ToString::to_string)
            .collect())
    };
    privacy_pre_push(&scans, &scan)
}

/// `ww dev hook pre-push`: reads git's stdin; exit 0 to allow, 1 to refuse.
///
/// Every pushed ref gets the privacy scan (R32). Only a push that touches main also needs the
/// committed settings and GitHub.
/// `remote` is what `git ls-remote` is asked ([`ls_remote_target`]).
pub fn run_pre_push(remote: Option<&str>) -> i32 {
    let stdin = match std::io::read_to_string(std::io::stdin()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{PRIVACY_HEAD} Couldn't read git's push list, so the push is refused: {e}");
            return 1;
        }
    };
    let repo = match root::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{PRIVACY_HEAD} {e}");
            return 1;
        }
    };
    let mut refusals = Vec::new();
    if let Err(m) = privacy_real(&repo, remote, &stdin) {
        refusals.push(m);
    }
    // Pushes that leave main alone need no settings and no network.
    if touches_branch(&stdin, BASE_BRANCH) {
        let main = load_real(&repo)
            .map_err(|e| format!("{REFUSAL_HEAD} {e}"))
            .and_then(|(cfg, client)| {
                let verdict_for = |b: &str, h: &str| guard::verdict(&repo, b, h);
                let is_ancestor = |a: &str, d: &str| git_is_ancestor(&repo, a, d);
                pre_push(&stdin, &cfg, &verdict_for, &is_ancestor, &client)
            });
        if let Err(m) = main {
            refusals.push(m);
        }
    }
    report(if refusals.is_empty() {
        Ok(())
    } else {
        Err(refusals.join("\n"))
    })
}

/// `ww dev hook pre-merge-commit`: git passes no arguments; exit 0 to allow, 1 to refuse.
pub fn run_pre_merge_commit() -> i32 {
    let repo = match root::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{REFUSAL_HEAD} This merge is refused: {e}");
            return 1;
        }
    };
    match current_head_ref(&repo) {
        Ok(None) => 0, // detached HEAD: not main
        Ok(Some(head_ref)) => report(pre_merge_commit(&head_ref, BASE_BRANCH)),
        Err(e) => {
            eprintln!(
                "{REFUSAL_HEAD} Couldn't tell which branch this merge is on, so it is refused: {}",
                one_line(&format!("{e:#}"))
            );
            1
        }
    }
}

/// `ww dev install-hooks`.
pub fn run_install_hooks() -> i32 {
    let repo = match root::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ww dev install-hooks: nothing was changed: {e}");
            return 1;
        }
    };
    match install_hooks(&repo) {
        Ok(()) => {
            println!(
                "Git hooks installed: core.hooksPath is {HOOKS_PATH}. Git credentials for this repository now come only from `ww dev git-credential` (the agent token)."
            );
            0
        }
        Err(e) => {
            eprintln!("ww dev install-hooks: {}", one_line(&format!("{e:#}")));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::github::{CheckState, MergeOutcome, PrInfo, Review};
    use crate::dev::github_fake::FakeGitHub;
    use crate::dev::guard::Blocker;
    use anyhow::anyhow;
    use std::path::PathBuf;
    use std::process::Stdio;

    const LOCAL: &str = "1234567890abcdef1234567890abcdef12345678";
    const REMOTE: &str = "fedcba0987654321fedcba0987654321fedcba09";
    const ZERO: &str = "0000000000000000000000000000000000000000";
    const MAIN_REFUSAL: &str = "Pushes to main go through `ww dev merge <PR>`";

    fn cfg() -> MergeGuardConfig {
        MergeGuardConfig::parse(include_str!("../../../../dev/merge-guard.toml")).unwrap()
    }

    fn clean(_: &str, _: &str) -> Result<Verdict> {
        Ok(Verdict::default())
    }

    fn protected(_: &str, _: &str) -> Result<Verdict> {
        Ok(Verdict {
            needs_approval: vec![Blocker::ProtectedChanged {
                path: "test-params.toml".into(),
                kind: "modified".into(),
            }],
            warnings: vec![],
        })
    }

    fn unreadable(_: &str, _: &str) -> Result<Verdict> {
        Err(anyhow!("bad object"))
    }

    fn never_judged(_: &str, _: &str) -> Result<Verdict> {
        panic!("this change must not be judged");
    }

    fn forward(_: &str, _: &str) -> Result<bool> {
        Ok(true)
    }

    fn not_forward(_: &str, _: &str) -> Result<bool> {
        Ok(false)
    }

    fn ancestry_unknown(_: &str, _: &str) -> Result<bool> {
        Err(anyhow!("fatal: Not a valid commit name"))
    }

    fn never_asked(_: &str, _: &str) -> Result<bool> {
        panic!("ancestry must not be asked");
    }

    fn green(sha: &str) -> FakeGitHub {
        let fake = FakeGitHub::new();
        fake.set_check(sha, "every-push", CheckState::Success);
        fake
    }

    /// Panics on any call: proves no network is used.
    struct NoNetwork;

    impl GitHubApi for NoNetwork {
        fn pr(&self, _: u64) -> Result<PrInfo> {
            panic!("no network call expected")
        }
        fn check(&self, _: &str, _: &str) -> Result<CheckState> {
            panic!("no network call expected")
        }
        fn reviews(&self, _: u64) -> Result<Vec<Review>> {
            panic!("no network call expected")
        }
        fn merge(&self, _: u64, _: &str) -> Result<MergeOutcome> {
            panic!("no network call expected")
        }
        fn post_review_as_app(&self, _: u64, _: &str, _: &str, _: &str) -> Result<()> {
            panic!("no network call expected")
        }
        fn commit_pulls(&self, _: &str) -> Result<Vec<crate::dev::github::CommitPull>> {
            panic!("no network call expected")
        }
    }

    /// Every GitHub call fails.
    struct Broken;

    impl GitHubApi for Broken {
        fn pr(&self, _: u64) -> Result<PrInfo> {
            Err(anyhow!("HTTP 502"))
        }
        fn check(&self, _: &str, _: &str) -> Result<CheckState> {
            Err(anyhow!("HTTP 502"))
        }
        fn reviews(&self, _: u64) -> Result<Vec<Review>> {
            Err(anyhow!("HTTP 502"))
        }
        fn merge(&self, _: u64, _: &str) -> Result<MergeOutcome> {
            Err(anyhow!("HTTP 502"))
        }
        fn post_review_as_app(&self, _: u64, _: &str, _: &str, _: &str) -> Result<()> {
            Err(anyhow!("HTTP 502"))
        }
        fn commit_pulls(&self, _: &str) -> Result<Vec<crate::dev::github::CommitPull>> {
            Err(anyhow!("HTTP 502"))
        }
    }

    fn push_line(remote_ref: &str, local_sha: &str, remote_sha: &str) -> String {
        format!("refs/heads/work {local_sha} {remote_ref} {remote_sha}\n")
    }

    // ---- the brief's six tests ------------------------------------------------------------

    #[test]
    fn push_to_feature_branch_passes() {
        let line = push_line("refs/heads/feature/x", LOCAL, REMOTE);
        assert_eq!(
            pre_push(&line, &cfg(), &never_judged, &never_asked, &NoNetwork),
            Ok(())
        );
    }

    #[test]
    fn push_to_main_with_protected_change_refused() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &protected, &forward, &green(LOCAL)).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("test-params.toml"), "{err}");
    }

    #[test]
    fn push_to_main_without_ci_refused() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &clean, &forward, &FakeGitHub::new()).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("every-push"), "{err}");
    }

    #[test]
    fn merge_into_main_locally_refused() {
        let err = pre_merge_commit("refs/heads/main", "main").unwrap_err();
        assert_eq!(
            err,
            "Merge commits on main aren't allowed locally; run `git merge --abort`. Changes reach main only through `ww dev merge <PR>`."
        );
    }

    #[test]
    fn merge_on_feature_branch_passes() {
        assert_eq!(pre_merge_commit("refs/heads/feature/x", "main"), Ok(()));
        assert_eq!(pre_merge_commit("refs/heads/mainline", "main"), Ok(()));
        assert_eq!(pre_merge_commit("refs/heads/feature/main", "main"), Ok(()));
    }

    // ---- the privacy scan on every pushed ref (Task 17, R32) -------------------------------

    fn have_all(_: &str) -> bool {
        true
    }

    fn no_tips() -> Result<Vec<String>> {
        Ok(vec![])
    }

    fn tips_never_asked() -> Result<Vec<String>> {
        panic!("ls-remote must not be asked")
    }

    #[test] // spec: QA-086
    fn privacy_scans_cover_every_pushed_ref() {
        let text = format!(
            "{}{}{}{}\n  \ngarbage\n",
            push_line("refs/heads/feature/x", LOCAL, REMOTE),
            push_line("refs/heads/main", LOCAL, REMOTE),
            push_line("refs/tags/v1", LOCAL, ZERO),
            push_line("refs/heads/gone", ZERO, REMOTE),
        );
        let scans = privacy_scans(&text, &have_all, &no_tips).unwrap();
        let range = |r: &str| PushScan::Range {
            remote_ref: r.into(),
            base: REMOTE.into(),
            head: LOCAL.into(),
        };
        assert_eq!(
            scans,
            vec![
                range("refs/heads/feature/x"),
                range("refs/heads/main"),
                PushScan::NewBranch {
                    remote_ref: "refs/tags/v1".into(),
                    head: LOCAL.into(),
                    exclude: vec![],
                }
            ],
            "deletions and malformed lines need no scan here"
        );
        // Existing refs whose old commit is here never ask the remote.
        let one = push_line("refs/heads/feature/x", LOCAL, REMOTE);
        assert_eq!(
            privacy_scans(&one, &have_all, &tips_never_asked).unwrap(),
            vec![range("refs/heads/feature/x")]
        );
    }

    #[test] // spec: QA-086
    fn privacy_scan_of_a_new_branch_or_unknown_remote_commit() {
        // A new branch: the remote sha is all zeros.
        // What the remote has excludes commits, but only ids this repository has.
        let known = "a".repeat(40);
        let unknown = "b".repeat(40);
        let tips = || -> Result<Vec<String>> { Ok(vec![known.clone(), unknown.clone()]) };
        let have = |sha: &str| sha != unknown;
        let new = push_line("refs/heads/feature/x", LOCAL, ZERO);
        let expect = PushScan::NewBranch {
            remote_ref: "refs/heads/feature/x".into(),
            head: LOCAL.into(),
            exclude: vec![known.clone()],
        };
        assert_eq!(
            privacy_scans(&new, &have, &tips).unwrap(),
            vec![expect.clone()]
        );
        // The remote's old commit isn't here (someone else pushed, or a force-push over
        // history this clone never had): scan it as a new branch.
        let line = push_line("refs/heads/feature/x", LOCAL, REMOTE);
        let missing = |sha: &str| sha != REMOTE && sha != unknown;
        assert_eq!(privacy_scans(&line, &missing, &tips).unwrap(), vec![expect]);
        // ls-remote failing refuses the push (R33: fail closed).
        let broken = || -> Result<Vec<String>> { Err(anyhow!("could not read from remote")) };
        let err = privacy_scans(&new, &have, &broken).unwrap_err();
        assert!(err.contains("Privacy scan (QA-086)"), "{err}");
        assert!(err.contains("could not read from remote"), "{err}");
    }

    #[test] // spec: QA-086
    fn ls_remote_asks_the_url_git_passed() {
        // git passes the push destination (after pushurl / pushInsteadOf) as the URL.
        assert_eq!(
            ls_remote_target(Some("origin"), Some("https://push.example/repo.git")),
            Some("https://push.example/repo.git")
        );
        assert_eq!(ls_remote_target(Some("origin"), None), Some("origin"));
        assert_eq!(ls_remote_target(Some("origin"), Some("")), Some("origin"));
        assert_eq!(ls_remote_target(None, None), None);
    }

    #[test] // spec: QA-086
    fn privacy_pre_push_refuses_findings_and_errors_and_passes_clean() {
        let scans = vec![
            PushScan::Range {
                remote_ref: "refs/heads/a".into(),
                base: REMOTE.into(),
                head: LOCAL.into(),
            },
            PushScan::NewBranch {
                remote_ref: "refs/heads/b".into(),
                head: LOCAL.into(),
                exclude: vec![],
            },
        ];
        let clean = |_: &PushScan| -> Result<Vec<String>> { Ok(vec![]) };
        assert_eq!(privacy_pre_push(&scans, &clean), Ok(()));
        assert_eq!(privacy_pre_push(&[], &clean), Ok(()));

        // The same finding from two refs is shown once.
        let found = |_: &PushScan| -> Result<Vec<String>> {
            Ok(vec!["a.txt:3: matches private denylist entry 1".into()])
        };
        let err = privacy_pre_push(&scans, &found).unwrap_err();
        assert!(err.contains("Privacy scan (QA-086)"), "{err}");
        assert_eq!(err.matches("a.txt:3").count(), 1, "{err}");

        // Two findings of one ref that print alike (two addresses that mask alike) both stay,
        // and are still not doubled by a second ref (ruling R37, M3).
        let twice = |_: &PushScan| -> Result<Vec<String>> {
            Ok(vec![
                "a.txt:3: an e-mail address (j***@***)".into(),
                "a.txt:3: an e-mail address (j***@***)".into(),
            ])
        };
        let err = privacy_pre_push(&scans, &twice).unwrap_err();
        assert_eq!(err.matches("a.txt:3").count(), 2, "{err}");

        let broken = |_: &PushScan| -> Result<Vec<String>> { Err(anyhow!("bad object")) };
        let err = privacy_pre_push(&scans, &broken).unwrap_err();
        assert!(err.contains("bad object"), "{err}");
        assert!(err.contains("refused"), "{err}");
    }

    // ---- git helpers ----------------------------------------------------------------------

    fn git_cmd(dir: &Path, args: &[&str], empty_global: &Path) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", empty_global)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE");
        cmd
    }

    fn git_t(dir: &Path, args: &[&str], empty_global: &Path) -> String {
        let out = git_cmd(dir, args, empty_global)
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A temp repo (on `main`, no commits yet) with git's global and system config switched off.
    fn temp_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty-gitconfig");
        std::fs::write(&empty, "").unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_t(
            &repo,
            &["-c", "init.defaultBranch=main", "init", "-q"],
            &empty,
        );
        git_t(&repo, &["config", "user.name", "Hook Test"], &empty);
        git_t(
            &repo,
            &["config", "user.email", "hook@test.invalid"],
            &empty,
        );
        git_t(&repo, &["config", "commit.gpgsign", "false"], &empty);
        (tmp, repo, empty)
    }

    /// Commits a file named `name` and returns the new commit's sha.
    fn commit_file(repo: &Path, empty: &Path, name: &str) -> String {
        std::fs::write(repo.join(name), name).unwrap();
        git_t(repo, &["add", "-A"], empty);
        git_t(repo, &["commit", "-q", "-m", name], empty);
        git_t(repo, &["rev-parse", "HEAD"], empty)
    }

    /// Switches git's global and system config off for `install_hooks_with`.
    fn isolated(empty_global: &Path) -> impl Fn(&mut Command) + '_ {
        move |cmd: &mut Command| {
            cmd.env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", empty_global);
        }
    }

    /// The local `credential.helper` entries, in order (NUL-separated, so empty ones show).
    fn local_helpers(repo: &Path, empty: &Path) -> Vec<String> {
        let out = git_cmd(
            repo,
            &[
                "config",
                "--local",
                "--null",
                "--get-all",
                "credential.helper",
            ],
            empty,
        )
        .output()
        .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_string)
            .collect()
    }

    #[test] // spec: QA-079
    fn install_hooks_writes_the_helper_reset_and_the_helper() {
        let (tmp, repo, empty) = temp_repo();
        // A global helper, as Git Credential Manager installs one, and a stale local one.
        let global = tmp.path().join("global-gitconfig");
        std::fs::write(&global, GLOBAL_HELPER).unwrap();
        git_t(
            &repo,
            &["config", "--local", "credential.helper", "stale"],
            &empty,
        );
        git_t(
            &repo,
            &[
                "config",
                "--local",
                "--add",
                "credential.helper",
                "stale-too",
            ],
            &empty,
        );
        // URL-specific helpers would be read after ours; other URL settings stay.
        for (key, value) in [
            (
                "credential.https://github.com.helper",
                "!gh auth git-credential",
            ),
            ("credential.https://example.com.helper", "store"),
            ("credential.https://github.com.username", "someone"),
        ] {
            git_t(&repo, &["config", "--local", "--add", key, value], &empty);
        }
        // Twice: the second run leaves exactly the same two entries.
        install_hooks_with(&repo, &isolated(&global)).unwrap();
        install_hooks_with(&repo, &isolated(&global)).unwrap();
        assert_eq!(
            local_helpers(&repo, &global),
            ["", crate::dev::agent_access::GIT_CREDENTIAL_HELPER]
        );
        let url_helpers = git_cmd(
            &repo,
            &[
                "config",
                "--local",
                "--get-regexp",
                r"^credential\..+\.helper$",
            ],
            &global,
        )
        .output()
        .unwrap();
        assert_eq!(
            url_helpers.status.code(),
            Some(1),
            "URL-specific helpers remain"
        );
        assert_eq!(
            git_t(
                &repo,
                &[
                    "config",
                    "--local",
                    "--get",
                    "credential.https://github.com.username"
                ],
                &global
            ),
            "someone"
        );
        assert_eq!(
            git_t(
                &repo,
                &["config", "--local", "--get", "core.hooksPath"],
                &global
            ),
            "tools/git-hooks"
        );
        // The global file was not touched.
        assert_eq!(std::fs::read_to_string(&global).unwrap(), GLOBAL_HELPER);
    }

    const GLOBAL_HELPER: &str = "[credential]\n\thelper = manager\n";

    #[test]
    fn install_hooks_sets_config() {
        let (_tmp, repo, empty) = temp_repo();
        install_hooks_with(&repo, &isolated(&empty)).unwrap();
        assert_eq!(
            git_t(
                &repo,
                &["config", "--local", "--get", "core.hooksPath"],
                &empty
            ),
            "tools/git-hooks"
        );
    }

    #[test]
    fn install_hooks_errors_when_git_fails() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(install_hooks(&tmp.path().join("does-not-exist")).is_err());
    }

    // ---- pre_push: R15 cases --------------------------------------------------------------

    #[test]
    fn push_to_main_clean_and_green_passes() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        assert_eq!(
            pre_push(&line, &cfg(), &clean, &forward, &green(LOCAL)),
            Ok(())
        );
    }

    #[test]
    fn push_to_main_checks_ci_on_the_pushed_sha_not_the_old_one() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &clean, &forward, &green(REMOTE)).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
    }

    #[test]
    fn push_to_main_with_pending_or_failed_ci_refused() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        for state in [CheckState::Pending, CheckState::Failure("failure".into())] {
            let fake = FakeGitHub::new();
            fake.set_check(LOCAL, "every-push", state);
            let err = pre_push(&line, &cfg(), &clean, &forward, &fake).unwrap_err();
            assert!(err.contains(MAIN_REFUSAL), "{err}");
        }
    }

    #[test]
    fn creating_main_is_refused() {
        let line = push_line("refs/heads/main", LOCAL, ZERO);
        let err = pre_push(&line, &cfg(), &never_judged, &never_asked, &NoNetwork).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("create"), "{err}");
    }

    #[test]
    fn deleting_main_is_refused() {
        let line = push_line("refs/heads/main", ZERO, REMOTE);
        let err = pre_push(&line, &cfg(), &never_judged, &never_asked, &NoNetwork).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("delete"), "{err}");
    }

    #[test]
    fn malformed_line_is_refused() {
        for bad in ["refs/heads/work abc", "a b c d e", "garbage"] {
            let err = pre_push(bad, &cfg(), &clean, &forward, &green(LOCAL)).unwrap_err();
            assert!(err.contains(MAIN_REFUSAL), "{err}");
        }
    }

    #[test]
    fn blank_lines_are_ignored() {
        let text = format!(
            "\n  \n{}\n",
            push_line("refs/heads/feature/x", LOCAL, REMOTE)
        );
        assert_eq!(
            pre_push(&text, &cfg(), &never_judged, &never_asked, &NoNetwork),
            Ok(())
        );
        assert_eq!(
            pre_push("", &cfg(), &never_judged, &never_asked, &NoNetwork),
            Ok(())
        );
    }

    #[test]
    fn github_error_refuses_the_push_to_main() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &clean, &forward, &Broken).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("502"), "{err}");
    }

    #[test]
    fn unreadable_verdict_refuses_the_push_to_main() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &unreadable, &forward, &green(LOCAL)).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("bad object"), "{err}");
    }

    #[test]
    fn one_main_line_among_feature_lines_refuses() {
        let text = format!(
            "{}{}",
            push_line("refs/heads/feature/x", LOCAL, REMOTE),
            push_line("refs/heads/main", LOCAL, REMOTE)
        );
        assert!(pre_push(&text, &cfg(), &clean, &forward, &FakeGitHub::new()).is_err());
    }

    #[test]
    fn tags_and_lookalike_branches_are_not_main() {
        for r in [
            "refs/tags/main",
            "refs/heads/main2",
            "refs/heads/feature/main",
        ] {
            let line = push_line(r, LOCAL, REMOTE);
            assert_eq!(
                pre_push(&line, &cfg(), &never_judged, &never_asked, &NoNetwork),
                Ok(()),
                "{r}"
            );
        }
    }

    #[test]
    fn touches_branch_only_for_main_or_unreadable_lines() {
        let feature = push_line("refs/heads/feature/x", LOCAL, REMOTE);
        let main = push_line("refs/heads/main", LOCAL, REMOTE);
        assert!(!touches_branch(&feature, "main"));
        assert!(!touches_branch("", "main"));
        assert!(!touches_branch("\n\n", "main"));
        assert!(touches_branch(&main, "main"));
        assert!(touches_branch(&format!("{feature}{main}"), "main"));
        assert!(touches_branch("junk", "main"));
    }

    // ---- fix round 1: C1 (main only moves forward) and minor 7 -----------------------------

    #[test]
    fn rollback_of_main_is_refused() {
        // The pushed commit is an ancestor of main: a rollback to an older green commit.
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &never_judged, &not_forward, &green(LOCAL)).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("main can only move forward"), "{err}");
    }

    #[test]
    fn divergent_history_on_main_is_refused() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &clean, &not_forward, &green(LOCAL)).unwrap_err();
        assert!(err.contains("main can only move forward"), "{err}");
    }

    #[test]
    fn unknown_ancestry_refuses_the_push_to_main() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg(), &clean, &ancestry_unknown, &green(LOCAL)).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("Not a valid commit name"), "{err}");
    }

    #[test]
    fn ancestry_is_asked_remote_then_local() {
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let asked = std::cell::RefCell::new(Vec::new());
        let record = |a: &str, d: &str| {
            asked.borrow_mut().push((a.to_string(), d.to_string()));
            Ok(true)
        };
        pre_push(&line, &cfg(), &clean, &record, &green(LOCAL)).unwrap();
        assert_eq!(
            *asked.borrow(),
            vec![(REMOTE.to_string(), LOCAL.to_string())]
        );
    }

    #[test]
    fn git_is_ancestor_tells_forward_rollback_divergent_and_unknown() {
        let (_tmp, repo, empty) = temp_repo();
        let first = commit_file(&repo, &empty, "a.txt");
        let second = commit_file(&repo, &empty, "b.txt");
        git_t(&repo, &["checkout", "-q", "-b", "other", &first], &empty);
        let other = commit_file(&repo, &empty, "c.txt");
        // main moved forward: first -> second
        assert!(git_is_ancestor(&repo, &first, &second).unwrap());
        // rollback: pushing `first` over a main that is at `second`
        assert!(!git_is_ancestor(&repo, &second, &first).unwrap());
        // divergent histories
        assert!(!git_is_ancestor(&repo, &second, &other).unwrap());
        // a missing object is an error, not "false"
        let missing = "1".repeat(40);
        assert!(git_is_ancestor(&repo, &missing, &second).is_err());
    }

    #[test]
    fn every_push_missing_from_required_checks_refuses() {
        let mut cfg = cfg();
        cfg.required_checks = vec!["pr-ready".into()];
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let err = pre_push(&line, &cfg, &clean, &forward, &green(LOCAL)).unwrap_err();
        assert!(err.contains(MAIN_REFUSAL), "{err}");
        assert!(err.contains("every-push"), "{err}");
    }

    // ---- fix round 1: C2 (branch detection) and I3 -----------------------------------------

    #[test]
    fn head_ref_is_the_full_ref_even_with_a_tag_named_main() {
        let (_tmp, repo, empty) = temp_repo();
        commit_file(&repo, &empty, "a.txt");
        git_t(&repo, &["tag", "main"], &empty);
        // `git symbolic-ref --short HEAD` would print `heads/main` here.
        assert_eq!(
            current_head_ref(&repo).unwrap().as_deref(),
            Some("refs/heads/main")
        );
        assert!(pre_merge_commit("refs/heads/main", "main").is_err());
    }

    #[test]
    fn detached_head_has_no_head_ref() {
        let (_tmp, repo, empty) = temp_repo();
        commit_file(&repo, &empty, "a.txt");
        git_t(&repo, &["checkout", "-q", "--detach"], &empty);
        assert_eq!(current_head_ref(&repo).unwrap(), None);
    }

    #[test]
    fn feature_branch_head_ref_passes_the_merge_hook() {
        let (_tmp, repo, empty) = temp_repo();
        commit_file(&repo, &empty, "a.txt");
        git_t(&repo, &["checkout", "-q", "-b", "feature/x"], &empty);
        let head_ref = current_head_ref(&repo).unwrap().unwrap();
        assert_eq!(head_ref, "refs/heads/feature/x");
        assert_eq!(pre_merge_commit(&head_ref, "main"), Ok(()));
    }

    // ---- fix round 1: I4 (the sh scripts) --------------------------------------------------

    fn sh_available() -> bool {
        Command::new("sh")
            .args(["-c", "exit 0"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Skips (returning early from the test) when there is no `sh` on PATH.
    macro_rules! require_sh {
        () => {
            if !sh_available() {
                eprintln!("SKIPPED: no `sh` on PATH, so the hook scripts can't be run here");
                return;
            }
        };
    }

    struct Script {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        empty: PathBuf,
        home: PathBuf,
        stub_dir: PathBuf,
        log: PathBuf,
    }

    #[derive(Debug)]
    struct Ran {
        code: Option<i32>,
        stderr: String,
        /// What the fake cargo recorded (empty when it wasn't called).
        log: String,
    }

    impl Script {
        fn new() -> Script {
            let (tmp, repo, empty) = temp_repo();
            let home = tmp.path().join("home");
            let stub_dir = tmp.path().join("stub-bin");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&stub_dir).unwrap();
            let log = tmp.path().join("cargo.log");
            Script {
                _tmp: tmp,
                repo,
                empty,
                home,
                stub_dir,
                log,
            }
        }

        /// A fake `cargo` in `dir` that records its arguments and stdin.
        fn write_stub(&self, dir: &Path) {
            std::fs::create_dir_all(dir).unwrap();
            let stub = dir.join("cargo");
            std::fs::write(
                &stub,
                "#!/bin/sh\n{ echo \"cargo $*\"; cat; } >> \"$STUB_LOG\"\nexit \"${STUB_EXIT:-0}\"\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        /// PATH without any directory that holds a real cargo, plus `extra` first.
        fn path(&self, extra: Option<&Path>) -> std::ffi::OsString {
            let current = std::env::var_os("PATH").unwrap_or_default();
            let kept = std::env::split_paths(&current)
                .filter(|d| !d.join("cargo").exists() && !d.join("cargo.exe").exists());
            std::env::join_paths(extra.map(Path::to_path_buf).into_iter().chain(kept)).unwrap()
        }

        fn run(&self, script: &str, stdin: &str, path: std::ffi::OsString) -> Ran {
            let script_path = workspace_root()
                .join("tools")
                .join("git-hooks")
                .join(script)
                .to_string_lossy()
                .replace('\\', "/");
            let mut child = Command::new("sh")
                .arg(&script_path)
                .current_dir(&self.repo)
                .env("PATH", path)
                .env("HOME", &self.home)
                .env("USERPROFILE", &self.home)
                .env("STUB_LOG", &self.log)
                .env("STUB_EXIT", "0")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", &self.empty)
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("sh must start");
            {
                use std::io::Write;
                let mut pipe = child.stdin.take().unwrap();
                let _ = pipe.write_all(stdin.as_bytes());
            }
            let out = child.wait_with_output().unwrap();
            Ran {
                code: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                log: std::fs::read_to_string(&self.log).unwrap_or_default(),
            }
        }

        fn run_with_stub(&self, script: &str, stdin: &str) -> Ran {
            self.write_stub(&self.stub_dir);
            self.run(script, stdin, self.path(Some(&self.stub_dir)))
        }
    }

    const HANDOFF: &str = "cargo run --quiet --locked -p ww-cli --bin ww -- dev hook";

    #[test] // spec: QA-086
    fn pre_push_script_hands_a_feature_branch_to_ww() {
        require_sh!();
        let s = Script::new();
        // Every pushed ref gets the privacy scan (R32), so a feature branch is handed off too.
        let line = push_line("refs/heads/feature/x", LOCAL, REMOTE);
        let ran = s.run_with_stub("pre-push", &format!("{line}\n"));
        assert_eq!(ran.code, Some(0), "{ran:?}");
        assert!(ran.log.contains(&format!("{HANDOFF} pre-push")), "{ran:?}");
        assert!(ran.log.contains(line.trim()), "stdin must pass: {ran:?}");
    }

    #[test] // spec: QA-086
    fn pre_push_script_hands_tags_and_deletions_to_ww() {
        require_sh!();
        let s = Script::new();
        for line in [
            push_line("refs/tags/v1", LOCAL, ZERO),
            push_line("refs/heads/feature/x", ZERO, REMOTE),
        ] {
            let ran = s.run_with_stub("pre-push", &line);
            assert!(ran.log.contains(HANDOFF), "{line}: {ran:?}");
        }
    }

    #[test] // spec: QA-086
    fn pre_push_script_without_cargo_refuses_a_feature_branch() {
        require_sh!();
        let s = Script::new();
        // No stub, and PATH has no cargo: the privacy scan can't run, so the push is refused.
        let line = push_line("refs/heads/feature/x", LOCAL, REMOTE);
        let ran = s.run("pre-push", &line, s.path(None));
        assert_eq!(ran.code, Some(1), "{ran:?}");
        assert!(ran.stderr.contains("cargo"), "{ran:?}");
        assert!(ran.stderr.contains("privacy"), "{ran:?}");
    }

    #[test] // spec: QA-086
    fn pre_push_script_does_not_build_for_an_empty_push() {
        require_sh!();
        let s = Script::new();
        for stdin in ["", "\n", "\n  \n"] {
            let ran = s.run_with_stub("pre-push", stdin);
            assert_eq!(ran.code, Some(0), "{stdin:?}: {ran:?}");
            assert_eq!(ran.log, "", "cargo must not run: {ran:?}");
        }
    }

    #[test]
    fn pre_push_script_hands_a_malformed_line_to_ww() {
        require_sh!();
        let s = Script::new();
        let ran = s.run_with_stub("pre-push", "refs/heads/work abc\n");
        assert!(ran.log.contains(&format!("{HANDOFF} pre-push")), "{ran:?}");
        assert!(ran.log.contains("refs/heads/work abc"), "{ran:?}");
    }

    #[test]
    fn pre_push_script_hands_main_to_ww_with_stdin_and_args() {
        require_sh!();
        let s = Script::new();
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let ran = s.run_with_stub("pre-push", &line);
        assert_eq!(ran.code, Some(0), "{ran:?}");
        assert!(ran.log.contains(&format!("{HANDOFF} pre-push")), "{ran:?}");
        assert!(ran.log.contains(line.trim()), "stdin must pass: {ran:?}");
    }

    #[test]
    fn pre_push_script_hands_over_when_any_line_is_main() {
        require_sh!();
        let s = Script::new();
        let text = format!(
            "{}{}",
            push_line("refs/heads/feature/x", LOCAL, REMOTE),
            push_line("refs/heads/main", LOCAL, REMOTE)
        );
        let ran = s.run_with_stub("pre-push", &text);
        assert!(ran.log.contains(HANDOFF), "{ran:?}");
    }

    #[test]
    fn pre_push_script_returns_the_exit_status_of_ww() {
        require_sh!();
        let s = Script::new();
        s.write_stub(&s.stub_dir);
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        // Same as run_with_stub, but the fake cargo exits 7.
        let script_path = workspace_root()
            .join("tools/git-hooks/pre-push")
            .to_string_lossy()
            .replace('\\', "/");
        let mut child = Command::new("sh")
            .arg(script_path)
            .current_dir(&s.repo)
            .env("PATH", s.path(Some(&s.stub_dir)))
            .env("HOME", &s.home)
            .env("STUB_LOG", &s.log)
            .env("STUB_EXIT", "7")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let _ = child.stdin.take().unwrap().write_all(line.as_bytes());
        }
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }

    #[test]
    fn pre_push_script_finds_cargo_in_home_cargo_bin() {
        require_sh!();
        let s = Script::new();
        s.write_stub(&s.home.join(".cargo").join("bin"));
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let ran = s.run("pre-push", &line, s.path(None));
        assert_eq!(ran.code, Some(0), "{ran:?}");
        assert!(ran.log.contains(HANDOFF), "{ran:?}");
    }

    #[test]
    fn pre_push_script_without_cargo_refuses_main_plainly() {
        require_sh!();
        let s = Script::new();
        let line = push_line("refs/heads/main", LOCAL, REMOTE);
        let ran = s.run("pre-push", &line, s.path(None));
        assert_eq!(ran.code, Some(1), "{ran:?}");
        assert!(ran.stderr.contains("cargo"), "{ran:?}");
        assert!(ran.log.is_empty(), "{ran:?}");
    }

    #[test]
    fn pre_merge_commit_script_passes_a_feature_branch_without_building() {
        require_sh!();
        let s = Script::new();
        git_t(&s.repo, &["checkout", "-q", "-b", "feature/x"], &s.empty);
        let ran = s.run_with_stub("pre-merge-commit", "");
        assert_eq!(ran.code, Some(0), "{ran:?}");
        assert_eq!(ran.log, "", "{ran:?}");
    }

    #[test]
    fn pre_merge_commit_script_hands_main_to_ww() {
        require_sh!();
        let s = Script::new();
        let ran = s.run_with_stub("pre-merge-commit", "");
        assert_eq!(ran.code, Some(0), "{ran:?}");
        assert!(
            ran.log.contains(&format!("{HANDOFF} pre-merge-commit")),
            "{ran:?}"
        );
    }

    #[test]
    fn pre_merge_commit_script_hands_main_to_ww_even_with_a_main_tag() {
        require_sh!();
        let s = Script::new();
        commit_file(&s.repo, &s.empty, "a.txt");
        git_t(&s.repo, &["tag", "main"], &s.empty);
        let ran = s.run_with_stub("pre-merge-commit", "");
        assert!(ran.log.contains(HANDOFF), "{ran:?}");
    }

    #[test]
    fn pre_merge_commit_script_passes_a_detached_head_without_building() {
        require_sh!();
        let s = Script::new();
        commit_file(&s.repo, &s.empty, "a.txt");
        git_t(&s.repo, &["checkout", "-q", "--detach"], &s.empty);
        let ran = s.run_with_stub("pre-merge-commit", "");
        assert_eq!(ran.code, Some(0), "{ran:?}");
        assert_eq!(ran.log, "", "{ran:?}");
    }

    #[test]
    fn pre_merge_commit_script_without_cargo_refuses_main_plainly() {
        require_sh!();
        let s = Script::new();
        let ran = s.run("pre-merge-commit", "", s.path(None));
        assert_eq!(ran.code, Some(1), "{ran:?}");
        assert!(ran.stderr.contains("cargo"), "{ran:?}");
    }
}
