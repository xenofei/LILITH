//! `lil dev ...` subcommands. Add a variant to `DevCommand` and a match arm in `run`.

pub mod agent_access;
pub mod desktop_tests;
pub mod github;
#[cfg(test)]
pub mod github_fake;
pub mod guard;
pub mod hooks;
pub mod merge;
pub mod merge_flow;
pub mod post_merge;
pub mod privacy;
pub mod reviewer;
pub mod reviewer_hello;
pub mod reviewer_key;
pub mod root;
pub mod spec_report;
pub mod spec_tests;
#[cfg(test)]
mod workflows;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum DevCommand {
    /// Run the spec tests and apply the expected-fail list.
    SpecTests,

    /// Run the desktop-group tests in Windows Sandbox, or the toast and foreground tests here.
    ///
    /// The desktop group is the `desktop_*` tests marked #[ignore = "desktop"] (QA-042).
    /// By default it builds a bundle in build\sandbox\in (a nextest archive of the desktop
    /// packages, the tracked source files, the test kit's binaries, cargo-nextest and the VC++
    /// runtime), writes build\sandbox\lilith-desktop.wsb, starts Windows Sandbox with the bundle
    /// and the deploy folder read-only and build\sandbox\out writable, waits for the results,
    /// copies them to build\sandbox\results and closes the Sandbox and its window. About 8
    /// minutes (plus about 10 for the first bundle); keep the PC unlocked meanwhile. The toast and
    /// foreground tests never run in the Sandbox (its tokens are elevated): use --host for them.
    ///
    ///   --suite qml|grabs  the CTest preset's commands in the Sandbox instead (P4a-R35)
    ///   --host             the toast and foreground tests on this PC, no Sandbox (P4a-R39):
    ///                      refused when elevated; a hands-off pop-up warns at least 10 s ahead
    ///                      and counts down; afterwards every test shortcut, registry key, CLSID
    ///                      and toast is checked gone (and removed if not)
    ///
    /// Exits 0 when every test passed, 1 when a test failed or something was left behind, and
    /// 2 when the run couldn't happen (no Windows Sandbox, another one running, a timeout).
    #[command(verbatim_doc_comment)]
    DesktopTests {
        /// Which tests run in the Sandbox: desktop (the Rust desktop group), qml or grabs.
        #[arg(long, default_value = "desktop", value_parser = ["desktop", "qml", "grabs"])]
        suite: String,
        /// Run the toast and foreground tests on this PC instead, without the Sandbox.
        #[arg(long, conflicts_with_all = ["suite", "keep_open"])]
        host: bool,
        /// How long the Sandbox run may take in all, in minutes.
        #[arg(long, value_name = "N", default_value_t = desktop_tests::DEFAULT_TIMEOUT_MINS)]
        timeout_mins: u64,
        /// The same in seconds (for tests).
        #[arg(long, hide = true, conflicts_with = "timeout_mins")]
        timeout_secs: Option<u64>,
        /// Leave the Sandbox open afterwards.
        #[arg(long)]
        keep_open: bool,
    },

    /// Report which spec rules the tests cite (QA-028).
    ///
    /// Counts the spec's rules per classification (dev/rule-classification.toml) and lists the
    /// rules classified `test` that no test cites. Tests cite a rule with a `spec: ID, ID`
    /// line comment. Exits 1 when a citation names an ID the spec does not define, and 2
    /// when the report couldn't be made.
    #[command(verbatim_doc_comment)]
    SpecReport,

    /// Run the test-diff guard on a change and say what needs approval.
    ///
    /// Prints one plain line per protected change that needs approval and per warning
    /// (removed or switched-off tests, new sleeps or retries, new lint suppressions).
    /// Exits 0 when nothing needs approval, 1 when something does, and 2 when the guard
    /// couldn't run. With --pr (CI's protected-test-check) it exits 0 only when the
    /// reviewer app approved the head commit on that PR, protected files or not.
    ///
    /// With --post-merge --before <sha> --after <sha> (CI, after a push to main), it judges
    /// every step main took from before to after instead: each must come from a PR merged
    /// into main, and protected changes need the reviewer app's approval of that PR's final
    /// head commit. Exits 0 when nothing is flagged, 1 when something is, and 2 when it
    /// couldn't run. Any GitHub or git error is a flag, never a pass.
    #[command(verbatim_doc_comment)]
    Guard {
        /// The commit or ref to compare against (for example refs/remotes/origin/main).
        #[arg(long, required_unless_present = "post_merge")]
        base: Option<String>,
        /// The commit or ref being judged.
        #[arg(long, required_unless_present = "post_merge")]
        head: Option<String>,
        /// Require the reviewer app's approval from this PR, whatever changed. Only an APPROVED
        /// review by the app on the exact head commit counts.
        #[arg(long)]
        pr: Option<u64>,
        /// Judge a push to main (before..after) instead of a PR.
        #[arg(long, requires_all = ["before", "after"], conflicts_with_all = ["base", "head", "pr"])]
        post_merge: bool,
        /// main before the push (all zeros when the push created main).
        #[arg(long, requires = "post_merge")]
        before: Option<String>,
        /// main after the push.
        #[arg(long, requires = "post_merge")]
        after: Option<String>,
    },

    /// Open a revert PR and an issue for a push to main that failed the post-merge check (CI).
    ///
    /// Creates the branch revert/<first 7 of after>, reverts every step main took from before
    /// to after, pushes the branch, opens a PR into main and an issue assigned to the owner
    /// with the plain-language verdict. Uses the gh CLI with GH_TOKEN. If the revert can't be
    /// made (a conflict), it opens only the issue. When before is all zeros (the push created
    /// main) it does nothing; when dev/merge-guard.toml isn't committed at before it refuses.
    #[command(verbatim_doc_comment)]
    RevertPr {
        /// main before the push.
        #[arg(long)]
        before: String,
        /// main after the push.
        #[arg(long)]
        after: String,
        /// The post-merge test job's result (success, failure, cancelled, ...), for the issue.
        #[arg(long)]
        tests_result: Option<String>,
    },

    /// Merge a PR into main, but only when it is safe and the owner authenticated it.
    ///
    /// First, `lil dev install-hooks` must have run: core.hooksPath is tools/git-hooks, and
    /// the repository's local credential.helper entries are an empty one and then the
    /// `lil dev git-credential` helper, with no other helper after them. Otherwise it
    /// refuses before contacting GitHub. Then it shows the PR's head commit, files,
    /// protected changes and CI, and asks for the owner's authentication: Windows Hello
    /// when enrolled, else the reviewer key's passphrase at a terminal. Without it nothing
    /// is approved or merged. After it, on every merge, protected files or not, it posts
    /// the reviewer app's approval of that head commit (GitHub's protected-test-check
    /// needs it), waits for CI on that commit, and merges only when all three of these
    /// hold for it:
    ///
    ///   1. CI is green: every-push, pr-ready, protected-test-check and privacy-check
    ///      have all passed on that exact commit. A check that is missing, still
    ///      running or failed means no merge.
    ///   2. The reviewer app (named in dev/merge-guard.toml) has approved that exact
    ///      commit: protected-test-check needs it on every PR, and lil checks it again
    ///      itself for protected changes. A human approval, or an approval of an older
    ///      commit, doesn't count.
    ///   3. The merge is pinned: GitHub is asked to merge only the commit you
    ///      authenticated, so a push after it makes the merge fail instead of slipping in.
    ///
    /// It asks nothing when a required check other than protected-test-check has
    /// already failed. When it stops without merging after posting the approval, it
    /// dismisses the approval again and says so. Draft PRs, closed PRs and PRs into
    /// another branch are refused. If anything can't be checked (a network or git
    /// error), it refuses. Every refusal says, in plain words, which condition failed.
    #[command(verbatim_doc_comment)]
    Merge {
        /// The pull request number.
        pr: u64,
        /// Skip Windows Hello: show the same texts, then ask for the passphrase at a terminal.
        #[arg(long)]
        passphrase: bool,
    },

    /// The one-click merge: show a PR's summary and audit, authenticate, approve, merge.
    ///
    /// Reads what lil checks itself (the head commit, the files, the protected changes,
    /// CI on that commit) and shows it with the summary and the independent audit from
    /// the two files. Then it asks for the owner's authentication: Windows Hello first,
    /// then the reviewer key's passphrase. Only after that succeeds does it post the
    /// reviewer approval of that commit (on every merge), wait for CI on that commit and
    /// merge with `lil dev merge`'s checks. Without it nothing is approved or merged.
    #[command(verbatim_doc_comment)]
    MergeFlow {
        /// The pull request number.
        pr: u64,
        /// A file with the change's summary.
        #[arg(long)]
        summary: std::path::PathBuf,
        /// A file with the independent audit (a review by an agent that didn't make it).
        #[arg(long)]
        audit: std::path::PathBuf,
        /// Skip Windows Hello: show the same texts, then ask for the passphrase at a terminal.
        #[arg(long)]
        passphrase: bool,
    },

    /// Set up, show or remove Windows Hello for the reviewer key (the passphrase stays).
    #[command(subcommand)]
    ReviewerHello(ReviewerHelloCommand),

    /// Set up the reviewer GitHub App's key on this machine (run once, by the owner, at a terminal).
    ///
    /// Asks for the App id, the installation id and the path to the App's downloaded .pem, then
    /// for a passphrase (at least 12 characters, typed twice). Writes the key, encrypted with the
    /// passphrase, to %USERPROFILE%\.lilith\reviewer\app-key.age. The .pem is never deleted
    /// or changed: delete it yourself afterwards.
    #[command(verbatim_doc_comment)]
    ReviewerSetup,

    /// Approve a PR's protected changes as the reviewer app (owner only, at a real terminal).
    ///
    /// Shows what the test-diff guard says needs approval on the PR's current head commit (judged
    /// against origin/main), asks "(y/N)", then asks for the reviewer key's passphrase. The key
    /// is read from %USERPROFILE%\.lilith\reviewer\app-key.age, and the path used is printed.
    /// With Windows Hello enrolled (`lil dev reviewer-hello`), Hello is asked first, and the
    /// passphrase only when Hello is cancelled or fails.
    /// Only then does it contact GitHub as the app and post one APPROVE review pinned to that exact commit.
    /// It refuses when stdin or stderr isn't a terminal, when nothing protected changed, when the
    /// PR is closed, a draft or not into main, and when the head moved after the summary.
    ///
    /// After posting, it re-runs this PR's failed protected-test-check runs for that commit, so
    /// the check sees the approval and turns green. If a run is still going, it waits for it for
    /// up to 60 s first. If the re-run can't be requested, the approval still stands: it prints
    /// a warning with the address to re-run the check from in the browser.
    ///
    /// Run it from a lil built from a clean origin/main worktree that agents can't write to: a
    /// binary built from a tree an agent can change could capture the passphrase.
    #[command(verbatim_doc_comment)]
    Approve {
        /// The pull request number.
        pr: u64,
        /// Skip Windows Hello: show the same summary, then ask for the passphrase.
        #[arg(long)]
        passphrase: bool,
    },

    /// Point git at the repo's hooks and at the agent credential helper.
    ///
    /// Sets core.hooksPath to tools/git-hooks. The hooks refuse direct pushes to main and
    /// local merges into main, so changes reach main only through `lil dev merge <PR>`.
    ///
    /// Also removes local URL-specific credential.<url>.helper entries and writes two
    /// repository-local credential.helper entries, in this order: an empty
    /// one, which drops inherited helpers such as Git Credential Manager, and then
    /// `!sh "$(git rev-parse --show-toplevel)/tools/git-hooks/credential-helper"`,
    /// which runs `dev git-credential` from the checkout's CLI crate. So git in this
    /// repository authenticates to github.com only with the agent token.
    #[command(verbatim_doc_comment)]
    InstallHooks,

    /// Store, show or remove the GitHub token agents use (in Windows Credential Manager).
    ///
    /// The agent token is a fine-grained personal access token limited to this repository,
    /// without administration or workflow permissions. `set` stores it at the Credential
    /// Manager target LILITH-dev/github/agent-token. `clear` removes it. Every lil dev GitHub
    /// call uses GITHUB_TOKEN when it is set (CI), and this token otherwise.
    #[command(subcommand, verbatim_doc_comment)]
    AgentToken(AgentTokenCommand),

    /// Git's credential helper for this repository. Git runs it; you don't.
    ///
    /// For `get` with protocol=https and host=github.com it answers with the user name
    /// x-access-token and the agent token. Every other host gets an empty answer, and
    /// `store` and `erase` do nothing.
    #[command(verbatim_doc_comment)]
    GitCredential {
        /// get, store or erase (passed by git).
        action: String,
    },

    /// Run the gh CLI with the agent token: lil dev gh -- <gh arguments>.
    ///
    /// GH_TOKEN is set for gh's process only. Only these run: pr (not merge), issue, search,
    /// status, repo view|clone, workflow list|view, release list|view, run list|view|watch,
    /// and read-only `api` calls to one REST path (no graphql; flags limited to --jq/-q,
    /// --template/-t, --paginate, --slurp, --include/-i, --silent, --verbose, --cache,
    /// --hostname github.com and Accept:/X-GitHub-Api-Version: headers). Agents merge only
    /// through `lil dev merge <PR>`.
    #[command(verbatim_doc_comment)]
    Gh {
        /// The arguments for gh, after `--`.
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Look for anything personal before it becomes public (QA-086).
    ///
    /// Finds user-profile paths (a drive letter or /x/ form, then Users, then a name that
    /// isn't a placeholder; write %USERPROFILE% instead), e-mail addresses other than
    /// GitHub no-reply, noreply@anthropic.com, git@github.com and reserved test domains,
    /// commit identities that aren't GitHub no-reply addresses, and the entries of the
    /// private denylist. The denylist comes from the first of these that is there:
    /// LIL_PRIVACY_DENYLIST (CI) and %USERPROFILE%\.lilith\privacy-denylist.txt. A blank
    /// variable is skipped. Without a denylist, or when the first one found has no entries, it
    /// refuses. A denylist finding
    /// names only the entry's number and the place.
    ///
    ///   --range <base>..<head>  every commit's added lines, paths, message and identities
    ///   --tree <rev>            every tracked file's content and path at <rev>
    ///   --commits <range>       identities only
    ///   --ref-name <name>       also scan a pushed ref name (with any mode)
    ///
    /// A single revision instead of <base>..<head> means all of its history. Exits 0 when
    /// nothing is found, 1 with findings, and 2 when it couldn't scan.
    #[command(
        verbatim_doc_comment,
        group(clap::ArgGroup::new("mode").required(true).args(["range", "tree", "commits"]))
    )]
    PrivacyScan {
        /// The commits to scan: <base>..<head>, or one revision for all of its history.
        #[arg(long, value_name = "BASE..HEAD")]
        range: Option<String>,
        /// The commit whose whole tree is scanned.
        #[arg(long, value_name = "REV")]
        tree: Option<String>,
        /// The commits whose identities are checked: <base>..<head>, or one revision.
        #[arg(long, value_name = "RANGE")]
        commits: Option<String>,
        /// A pushed ref name to scan too (CI passes the branch it checks).
        #[arg(long, value_name = "NAME")]
        ref_name: Option<String>,
    },

    /// Run a git hook. Git calls this from the scripts in tools/git-hooks; you don't.
    Hook {
        #[command(subcommand)]
        hook: HookCommand,
    },
}

#[derive(Subcommand)]
pub enum AgentTokenCommand {
    /// Store the agent token (at a real terminal; the token is read without echo).
    Set,
    /// Say whether a token is stored, with its first four characters and the expiry entered.
    Status,
    /// Remove the stored agent token (at a real terminal).
    Clear,
}

#[derive(Subcommand)]
pub enum ReviewerHelloCommand {
    /// Enroll Windows Hello (at a real terminal): your passphrase first, then Windows Hello.
    Enroll,
    /// Say whether Windows Hello is enrolled, its route, whether the key is hardware-held and
    /// whether face sign-in is on; with face sign-in on, it removes the enrolment first.
    Status,
    /// Remove the Windows Hello files and key credential; the passphrase key stays.
    Remove,
}

#[derive(Subcommand)]
pub enum HookCommand {
    /// pre-push: reads git's list of refs on stdin, runs the privacy scan on every pushed ref,
    /// and refuses anything that changes main.
    PrePush {
        /// The remote's name, or its URL when there is no name (passed by git). Used only when
        /// git passes no URL.
        remote: Option<String>,
        /// The URL being pushed to (passed by git; after pushurl and pushInsteadOf). The privacy
        /// scan asks it what it already has (`git ls-remote`).
        url: Option<String>,
    },
    /// pre-merge-commit: refuses a local merge into main.
    PreMergeCommit,
}

/// Runs a dev subcommand and returns the process exit code.
pub fn run(cmd: DevCommand) -> i32 {
    match cmd {
        DevCommand::SpecTests => spec_tests::run(),
        DevCommand::SpecReport => spec_report::run(),
        DevCommand::DesktopTests {
            suite,
            host,
            timeout_mins,
            timeout_secs,
            keep_open,
        } => {
            let suite = match (host, suite.as_str()) {
                (true, _) => desktop_tests::Suite::Host,
                (false, "qml") => desktop_tests::Suite::Qml,
                (false, "grabs") => desktop_tests::Suite::Grabs,
                _ => desktop_tests::Suite::Desktop,
            };
            let timeout = std::time::Duration::from_secs(
                timeout_secs.unwrap_or(timeout_mins.saturating_mul(60)),
            );
            desktop_tests::cli(suite, timeout, keep_open)
        }
        DevCommand::Guard {
            post_merge: true,
            before: Some(before),
            after: Some(after),
            ..
        } => post_merge::run_post_merge(&before, &after),
        DevCommand::Guard {
            base: Some(base),
            head: Some(head),
            pr,
            post_merge: false,
            ..
        } => merge::run_guard(&base, &head, pr),
        DevCommand::Guard { .. } => {
            // clap's `required_unless_present` / `requires_all` make this unreachable.
            eprintln!("lil dev guard: give --base and --head, or --post-merge --before --after");
            2
        }
        DevCommand::RevertPr {
            before,
            after,
            tests_result,
        } => post_merge::run_revert_pr(&before, &after, tests_result.as_deref()),
        DevCommand::Merge { pr, passphrase } => merge::run_merge(pr, passphrase),
        DevCommand::MergeFlow {
            pr,
            summary,
            audit,
            passphrase,
        } => reviewer::run_merge_flow(pr, &summary, &audit, passphrase),
        DevCommand::ReviewerHello(cmd) => match cmd {
            ReviewerHelloCommand::Enroll => reviewer_hello::run_enroll(),
            ReviewerHelloCommand::Status => reviewer_hello::run_status(),
            ReviewerHelloCommand::Remove => reviewer_hello::run_remove(),
        },
        DevCommand::ReviewerSetup => reviewer::run_reviewer_setup(),
        DevCommand::Approve { pr, passphrase } => reviewer::run_approve(pr, passphrase),
        DevCommand::InstallHooks => hooks::run_install_hooks(),
        DevCommand::AgentToken(cmd) => match cmd {
            AgentTokenCommand::Set => agent_access::run_set(),
            AgentTokenCommand::Status => agent_access::run_status(),
            AgentTokenCommand::Clear => agent_access::run_clear(),
        },
        DevCommand::GitCredential { action } => agent_access::run_git_credential(&action),
        DevCommand::Gh { args } => agent_access::run_gh(&args),
        DevCommand::PrivacyScan {
            range,
            tree,
            commits,
            ref_name,
        } => match (range, tree, commits) {
            (Some(r), None, None) => privacy::run(privacy::Mode::Range(r), ref_name.as_deref()),
            (None, Some(t), None) => privacy::run(privacy::Mode::Tree(t), ref_name.as_deref()),
            (None, None, Some(c)) => privacy::run(privacy::Mode::Commits(c), ref_name.as_deref()),
            _ => {
                // clap's required, exclusive `mode` group makes this unreachable.
                eprintln!("lil dev privacy-scan: give exactly one of --range, --tree or --commits");
                2
            }
        },
        DevCommand::Hook { hook } => match hook {
            HookCommand::PrePush { remote, url } => {
                hooks::run_pre_push(hooks::ls_remote_target(remote.as_deref(), url.as_deref()))
            }
            HookCommand::PreMergeCommit => hooks::run_pre_merge_commit(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(subcommand)]
        cmd: DevCommand,
    }

    fn parse(args: &[&str]) -> Result<DevCommand, clap::Error> {
        T::try_parse_from(std::iter::once("lil-dev").chain(args.iter().copied())).map(|t| t.cmd)
    }

    #[test]
    fn guard_post_merge_cli() {
        let c = parse(&["guard", "--post-merge", "--before", "b", "--after", "a"]).unwrap();
        assert!(matches!(
            c,
            DevCommand::Guard { post_merge: true, before: Some(b), after: Some(a), base: None, head: None, pr: None }
                if b == "b" && a == "a"
        ));
        // --post-merge needs both ends and excludes the PR-mode flags.
        for bad in [
            &["guard", "--post-merge", "--before", "b"][..],
            &["guard", "--post-merge", "--after", "a"],
            &[
                "guard",
                "--post-merge",
                "--before",
                "b",
                "--after",
                "a",
                "--base",
                "x",
            ],
            &[
                "guard",
                "--post-merge",
                "--before",
                "b",
                "--after",
                "a",
                "--pr",
                "3",
            ],
            &["guard", "--before", "b", "--after", "a"],
            &["guard", "--base", "x"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let c = parse(&[
            "guard",
            "--base",
            "refs/remotes/origin/main",
            "--head",
            "h",
            "--pr",
            "4",
        ])
        .unwrap();
        assert!(matches!(
            c,
            DevCommand::Guard {
                post_merge: false,
                base: Some(_),
                head: Some(_),
                pr: Some(4),
                ..
            }
        ));
    }

    /// `install-hooks --help` quotes the helper entry the command actually writes.
    #[test]
    fn install_hooks_help_quotes_the_written_helper() {
        use clap::CommandFactory;
        let mut cmd = T::command();
        let help = cmd
            .find_subcommand_mut("install-hooks")
            .expect("install-hooks subcommand")
            .render_long_help()
            .to_string();
        assert!(
            help.contains(agent_access::GIT_CREDENTIAL_HELPER),
            "install-hooks help doesn't quote the helper entry:\n{help}"
        );
        assert!(
            !help.contains("cargo run"),
            "install-hooks help still quotes the old entry:\n{help}"
        );
    }

    #[test]
    fn agent_access_cli() {
        assert!(matches!(
            parse(&["agent-token", "set"]).unwrap(),
            DevCommand::AgentToken(AgentTokenCommand::Set)
        ));
        assert!(matches!(
            parse(&["agent-token", "status"]).unwrap(),
            DevCommand::AgentToken(AgentTokenCommand::Status)
        ));
        assert!(matches!(
            parse(&["agent-token", "clear"]).unwrap(),
            DevCommand::AgentToken(AgentTokenCommand::Clear)
        ));
        assert!(parse(&["agent-token"]).is_err());
        assert!(matches!(
            parse(&["git-credential", "get"]).unwrap(),
            DevCommand::GitCredential { action } if action == "get"
        ));
        let c = parse(&["gh", "--", "pr", "view", "3", "--json", "state"]).unwrap();
        assert!(
            matches!(c, DevCommand::Gh { args } if args == ["pr", "view", "3", "--json", "state"])
        );
        // gh's arguments only after `--`.
        assert!(parse(&["gh", "pr", "view"]).is_err());
    }

    #[test] // spec: QA-086
    fn privacy_scan_cli() {
        assert!(matches!(
            parse(&["privacy-scan", "--range", "a..b"]).unwrap(),
            DevCommand::PrivacyScan { range: Some(r), tree: None, commits: None, ref_name: None } if r == "a..b"
        ));
        assert!(matches!(
            parse(&["privacy-scan", "--ref-name", "refs/heads/x", "--range", "a..b"]).unwrap(),
            DevCommand::PrivacyScan { ref_name: Some(n), range: Some(_), .. } if n == "refs/heads/x"
        ));
        // A ref name alone is not a mode.
        assert!(parse(&["privacy-scan", "--ref-name", "refs/heads/x"]).is_err());
        assert!(matches!(
            parse(&["privacy-scan", "--tree", "HEAD"]).unwrap(),
            DevCommand::PrivacyScan { tree: Some(t), .. } if t == "HEAD"
        ));
        assert!(matches!(
            parse(&["privacy-scan", "--commits", "HEAD"]).unwrap(),
            DevCommand::PrivacyScan { commits: Some(c), .. } if c == "HEAD"
        ));
        // Exactly one mode.
        assert!(parse(&["privacy-scan"]).is_err());
        assert!(parse(&["privacy-scan", "--tree", "HEAD", "--range", "a..b"]).is_err());
        assert!(parse(&["privacy-scan", "--tree", "HEAD", "--commits", "HEAD"]).is_err());
    }

    #[test] // spec: QA-042
    fn desktop_tests_cli() {
        let c = parse(&["desktop-tests"]).unwrap();
        assert!(matches!(
            c,
            DevCommand::DesktopTests { suite, host: false, timeout_mins: 20, timeout_secs: None, keep_open: false }
                if suite == "desktop"
        ));
        let c = parse(&[
            "desktop-tests",
            "--suite",
            "qml",
            "--keep-open",
            "--timeout-mins",
            "5",
        ])
        .unwrap();
        assert!(matches!(
            c,
            DevCommand::DesktopTests { suite, keep_open: true, timeout_mins: 5, .. } if suite == "qml"
        ));
        assert!(matches!(
            parse(&["desktop-tests", "--host"]).unwrap(),
            DevCommand::DesktopTests { host: true, .. }
        ));
        assert!(matches!(
            parse(&["desktop-tests", "--timeout-secs", "5"]).unwrap(),
            DevCommand::DesktopTests {
                timeout_secs: Some(5),
                ..
            }
        ));
        for bad in [
            &["desktop-tests", "--suite", "host"][..],
            &["desktop-tests", "--host", "--suite", "qml"],
            &["desktop-tests", "--host", "--keep-open"],
            &[
                "desktop-tests",
                "--timeout-secs",
                "5",
                "--timeout-mins",
                "1",
            ],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn revert_pr_cli() {
        let c = parse(&["revert-pr", "--before", "b", "--after", "a"]).unwrap();
        assert!(matches!(
            c,
            DevCommand::RevertPr { before, after, tests_result: None } if before == "b" && after == "a"
        ));
        let c = parse(&[
            "revert-pr",
            "--before",
            "b",
            "--after",
            "a",
            "--tests-result",
            "failure",
        ])
        .unwrap();
        assert!(matches!(c, DevCommand::RevertPr { tests_result: Some(t), .. } if t == "failure"));
        assert!(parse(&["revert-pr", "--before", "b"]).is_err());
    }
}
