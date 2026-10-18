//! The committed credential helper, `tools/git-hooks/credential-helper` (QA-079, ruling R28).
//!
//! `lil dev install-hooks` writes one `credential.helper` entry that runs this script, and every
//! worktree of a clone shares it. The script runs whichever CLI crate the checkout has
//! (`lil-cli` after the LILITH rename, `ww-cli` before it), so the entry survives the rename.
//!
//! The script is run with `sh`, as the `pre-push` tests in `dev/hooks.rs` run their hook, in a
//! temp repository. A fake `cargo` comes first on `PATH`: it records its arguments, its working
//! directory and its stdin, and answers with `STUB_OUT`. No directory that holds a real cargo
//! stays on `PATH`, and `HOME` is a temp folder, so nothing is built and no token is read.
//! Git's global and system config are switched off.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The entry `lil dev install-hooks` writes. The unit test
/// `helper_entry_runs_the_committed_script` in `dev/agent_access.rs` pins
/// `GIT_CREDENTIAL_HELPER` to this same text.
const HELPER_ENTRY: &str =
    r#"!sh "$(git rev-parse --show-toplevel)/tools/git-hooks/credential-helper""#;
const NO_CLI: &str = "credential-helper: this checkout has no CLI crate";
const LIL_RUN: &str = "cargo run -q --locked -p lil-cli --bin lil -- dev git-credential";
const LEGACY_CLI_RUN: &str = "cargo run -q --locked -p ww-cli --bin ww -- dev git-credential";
const REQUEST: &str = "protocol=https\nhost=github.com\npath=owner/repo.git\n\n";
const ANSWER: &str = "username=x-access-token\npassword=fake-answer-from-the-stub\n";

/// The script under test, in this checkout, with forward slashes for `sh`.
fn script() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/git-hooks/credential-helper")
        .to_string_lossy()
        .replace('\\', "/")
}

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
            eprintln!("SKIPPED: no `sh` on PATH, so the helper script can't be run here");
            return;
        }
    };
}

struct Env {
    tmp: tempfile::TempDir,
    /// A git repository whose top level is named `checkout`.
    checkout: PathBuf,
    home: PathBuf,
    stub_dir: PathBuf,
    log: PathBuf,
    global: PathBuf,
}

#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// What the fake cargo recorded (empty when it wasn't run).
    log: String,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("checkout");
        let home = tmp.path().join("home");
        let stub_dir = tmp.path().join("stub-bin");
        for d in [&checkout, &home, &stub_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        let global = tmp.path().join("global-gitconfig");
        std::fs::write(&global, "").unwrap();
        let log = tmp.path().join("cargo.log");
        let env = Env {
            tmp,
            checkout,
            home,
            stub_dir,
            log,
            global,
        };
        env.git_ok(&env.checkout, &["init", "-q"]);
        env
    }

    /// Creates `crates/<name>/Cargo.toml` in `dir`.
    fn add_crate(&self, dir: &Path, name: &str) {
        let d = dir.join("crates").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .unwrap();
    }

    /// A fake `cargo` in `dir` that records its arguments, working directory and stdin, prints
    /// `STUB_OUT` and exits with `STUB_EXIT`.
    fn write_stub(&self, dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let stub = dir.join("cargo");
        std::fs::write(
            &stub,
            "#!/bin/sh\n{ echo \"cargo $*\"; echo \"pwd $(pwd)\"; cat; } >> \"$STUB_LOG\"\nprintf '%s' \"$STUB_OUT\"\nexit \"${STUB_EXIT:-0}\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// PATH without any directory that holds a real cargo, plus `extra` first.
    fn path(&self, extra: Option<&Path>) -> OsString {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let kept = std::env::split_paths(&current)
            .filter(|d| !d.join("cargo").exists() && !d.join("cargo.exe").exists());
        std::env::join_paths(extra.map(Path::to_path_buf).into_iter().chain(kept)).unwrap()
    }

    /// `program args`, from `cwd`, with git's config isolated and the stub's variables set.
    fn command(&self, program: &str, args: &[&str], cwd: &Path, path: OsString) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("STUB_LOG", &self.log)
            .env("STUB_OUT", ANSWER)
            .env("STUB_EXIT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.global)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GIT_ASKPASS")
            .env_remove("SSH_ASKPASS")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR");
        cmd
    }

    fn output(&self, mut cmd: Command, stdin: &str) -> Ran {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the command must start");
        {
            let mut pipe = child.stdin.take().unwrap();
            let _ = pipe.write_all(stdin.as_bytes());
        }
        let out: Output = child.wait_with_output().unwrap();
        Ran {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            log: std::fs::read_to_string(&self.log).unwrap_or_default(),
        }
    }

    /// Runs the script with `sh` from `cwd`, with the fake cargo first on PATH.
    fn run_script(&self, cwd: &Path, args: &[&str], stdin: &str) -> Ran {
        self.write_stub(&self.stub_dir);
        let script = script();
        let mut all = vec![script.as_str()];
        all.extend_from_slice(args);
        let cmd = self.command("sh", &all, cwd, self.path(Some(&self.stub_dir)));
        self.output(cmd, stdin)
    }

    fn git_ok(&self, cwd: &Path, args: &[&str]) -> String {
        let ran = self.output(self.command("git", args, cwd, self.path(None)), "");
        assert_eq!(ran.code, Some(0), "git {args:?}: {ran:?}");
        ran.stdout
    }
}

/// The last directory the fake cargo ran in (`pwd` as `sh` prints it).
fn cargo_dir(log: &str) -> &str {
    log.lines()
        .rev()
        .find_map(|l| l.strip_prefix("pwd "))
        .unwrap_or_default()
}

#[test] // spec: QA-079
fn helper_runs_lil_cli_when_the_checkout_has_it() {
    require_sh!();
    let e = Env::new();
    e.add_crate(&e.checkout, "lil-cli");
    e.add_crate(&e.checkout, "ww-cli");
    // Run from a subdirectory: the script works from the top level of the checkout.
    let sub = e.checkout.join("crates").join("lil-cli");
    let ran = e.run_script(&sub, &["get"], REQUEST);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert!(ran.log.contains(&format!("{LIL_RUN} get\n")), "{ran:?}");
    assert!(!ran.log.contains("ww-cli"), "{ran:?}");
    assert!(cargo_dir(&ran.log).ends_with("/checkout"), "{ran:?}");
}

#[test] // spec: QA-079
fn helper_runs_the_legacy_cli_crate_when_only_it_exists() {
    require_sh!();
    let e = Env::new();
    e.add_crate(&e.checkout, "ww-cli");
    let ran = e.run_script(&e.checkout, &["get"], REQUEST);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert!(
        ran.log.contains(&format!("{LEGACY_CLI_RUN} get\n")),
        "{ran:?}"
    );
    assert!(!ran.log.contains("lil-cli"), "{ran:?}");
    assert!(cargo_dir(&ran.log).ends_with("/checkout"), "{ran:?}");
}

#[test] // spec: QA-079
fn helper_fails_plainly_without_a_cli_crate() {
    require_sh!();
    let e = Env::new();
    // A folder named like a crate, but without a Cargo.toml, isn't one.
    std::fs::create_dir_all(e.checkout.join("crates").join("lil-cli")).unwrap();
    let ran = e.run_script(&e.checkout, &["get"], REQUEST);
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(ran.stderr.contains(NO_CLI), "{ran:?}");
    assert_eq!(ran.stdout, "", "nothing may reach git's stdout: {ran:?}");
    assert_eq!(ran.log, "", "cargo must not run: {ran:?}");
}

#[test] // spec: QA-079
fn helper_passes_the_action_and_stdin_through() {
    require_sh!();
    let e = Env::new();
    e.add_crate(&e.checkout, "ww-cli");
    for action in ["get", "store", "erase"] {
        let _ = std::fs::remove_file(&e.log);
        let ran = e.run_script(&e.checkout, &[action], REQUEST);
        assert_eq!(ran.code, Some(0), "{action}: {ran:?}");
        assert!(
            ran.log.contains(&format!("{LEGACY_CLI_RUN} {action}\n")),
            "{action}: {ran:?}"
        );
        assert!(
            ran.log.contains(REQUEST),
            "{action}: stdin must pass: {ran:?}"
        );
        // The CLI's answer is the only thing on stdout, unchanged.
        assert_eq!(ran.stdout, ANSWER, "{action}: {ran:?}");
        assert_eq!(ran.stderr, "", "{action}: {ran:?}");
    }
    // The CLI's exit status is the helper's.
    e.write_stub(&e.stub_dir);
    let mut cmd = e.command(
        "sh",
        &[script().as_str(), "get"],
        &e.checkout,
        e.path(Some(&e.stub_dir)),
    );
    cmd.env("STUB_EXIT", "7");
    assert_eq!(e.output(cmd, REQUEST).code, Some(7));
}

#[test] // spec: QA-079
fn helper_finds_cargo_where_the_pre_push_hook_does() {
    require_sh!();
    let e = Env::new();
    e.add_crate(&e.checkout, "ww-cli");
    // No cargo on PATH or in $HOME/.cargo/bin: a plain refusal, and nothing on stdout.
    let cmd = e.command("sh", &[script().as_str(), "get"], &e.checkout, e.path(None));
    let ran = e.output(cmd, REQUEST);
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(ran.stderr.contains("cargo"), "{ran:?}");
    assert_eq!(ran.stdout, "", "{ran:?}");
    // cargo only in $HOME/.cargo/bin: found there.
    e.write_stub(&e.home.join(".cargo").join("bin"));
    let cmd = e.command("sh", &[script().as_str(), "get"], &e.checkout, e.path(None));
    let ran = e.output(cmd, REQUEST);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert!(
        ran.log.contains(&format!("{LEGACY_CLI_RUN} get\n")),
        "{ran:?}"
    );
    assert_eq!(ran.stdout, ANSWER, "{ran:?}");
}

#[test] // spec: QA-079
fn git_runs_the_helper_entry_from_every_worktree() {
    require_sh!();
    let e = Env::new();
    let repo = &e.checkout;
    // A commit from before the script existed, then one with it (as PR-A adds it).
    std::fs::write(repo.join("README"), "x\n").unwrap();
    e.git_ok(repo, &["add", "-A"]);
    e.git_ok(
        repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "-m",
            "before",
        ],
    );
    let hooks = repo.join("tools").join("git-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::copy(script(), hooks.join("credential-helper")).unwrap();
    e.add_crate(repo, "ww-cli");
    e.git_ok(repo, &["add", "-A"]);
    e.git_ok(
        repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "-m",
            "with the helper",
        ],
    );
    // The two entries install-hooks writes.
    e.git_ok(
        repo,
        &[
            "config",
            "--local",
            "--replace-all",
            "credential.helper",
            "",
        ],
    );
    e.git_ok(
        repo,
        &[
            "config",
            "--local",
            "--add",
            "credential.helper",
            HELPER_ENTRY,
        ],
    );
    let wt = e.tmp.path().join("wt");
    let old = e.tmp.path().join("old");
    let wt_s = wt.to_string_lossy().into_owned();
    let old_s = old.to_string_lossy().into_owned();
    e.git_ok(repo, &["worktree", "add", "-q", "--detach", &wt_s, "HEAD"]);
    e.git_ok(
        repo,
        &["worktree", "add", "-q", "--detach", &old_s, "HEAD~1"],
    );

    e.write_stub(&e.stub_dir);
    let fill = |cwd: &Path| {
        let _ = std::fs::remove_file(&e.log);
        let cmd = e.command(
            "git",
            &["credential", "fill"],
            cwd,
            e.path(Some(&e.stub_dir)),
        );
        e.output(cmd, REQUEST)
    };
    // The main checkout, a subdirectory of it, and a linked worktree each run their own copy.
    for (cwd, top) in [
        (repo.clone(), "/checkout"),
        (repo.join("tools"), "/checkout"),
        (wt.clone(), "/wt"),
    ] {
        let ran = fill(&cwd);
        assert_eq!(ran.code, Some(0), "{cwd:?}: {ran:?}");
        assert!(ran.stdout.contains(ANSWER), "{cwd:?}: {ran:?}");
        assert!(
            ran.log.contains(&format!("{LEGACY_CLI_RUN} get\n")),
            "{cwd:?}: {ran:?}"
        );
        assert!(ran.log.contains("host=github.com\n"), "{cwd:?}: {ran:?}");
        assert!(cargo_dir(&ran.log).ends_with(top), "{cwd:?}: {ran:?}");
    }
    // A worktree on a commit from before the script has no helper (the plan's owner checklist,
    // step 3: rebase it first). Git gets no credential, and prompting is off.
    let ran = fill(&old);
    assert_ne!(ran.code, Some(0), "{ran:?}");
    assert!(!ran.stdout.contains("password="), "{ran:?}");
    assert_eq!(ran.log, "", "{ran:?}");
}
