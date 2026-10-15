//! `ww dev git-credential` through real git, in a temp repository (QA-079, ruling R28).
//!
//! The repository gets the same two local entries `ww dev install-hooks` writes (an empty
//! `credential.helper`, then the helper), except that the helper runs the built `ww` binary
//! directly instead of through `cargo run`. Git's global and system config are switched off,
//! and the global file holds a helper that would answer for every host, to show the empty
//! entry drops it.
//!
//! The token comes from `GITHUB_TOKEN`, which `ww` resolves before Credential Manager, so no
//! test here reads a real credential. `ww dev agent-token set` is run only without a terminal,
//! where it refuses before touching Credential Manager.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const FAKE_TOKEN: &str = "github_pat_FAKE_for_tests_only";
const GLOBAL_PASSWORD: &str = "from-the-global-helper";

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    global: PathBuf,
}

/// Runs `program` with git's config isolated, prompts off, and `GITHUB_TOKEN` set to the fake.
fn run(program: &str, args: &[&str], cwd: &Path, global: &Path, stdin: &str) -> Output {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GITHUB_TOKEN", FAKE_TOKEN)
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{program} must start: {e}"));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn git(repo: &Repo, args: &[&str], stdin: &str) -> Output {
    run("git", args, &repo.dir, &repo.global, stdin)
}

fn git_ok(repo: &Repo, args: &[&str]) -> String {
    let out = git(repo, args, "");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The built `ww`, quoted for the `sh` git runs `!` helpers with.
fn ww_helper() -> String {
    let exe = env!("CARGO_BIN_EXE_ww").replace('\\', "/");
    format!("!'{exe}' dev git-credential")
}

fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let global = tmp.path().join("global-gitconfig");
    std::fs::write(
        &global,
        format!(
            "[credential]\n\thelper = \"!f() {{ echo username=global; echo password={GLOBAL_PASSWORD}; }}; f\"\n"
        ),
    )
    .unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let r = Repo {
        _tmp: tmp,
        dir,
        global,
    };
    git_ok(&r, &["init", "-q"]);
    git_ok(
        &r,
        &[
            "config",
            "--local",
            "--replace-all",
            "credential.helper",
            "",
        ],
    );
    git_ok(
        &r,
        &[
            "config",
            "--local",
            "--add",
            "credential.helper",
            &ww_helper(),
        ],
    );
    r
}

#[test] // spec: QA-079
fn credential_fill_for_github_comes_from_ww_and_not_the_inherited_helper() {
    let r = repo();
    let local = git_ok(
        &r,
        &[
            "config",
            "--local",
            "--null",
            "--get-all",
            "credential.helper",
        ],
    );
    assert_eq!(
        local.split_terminator('\0').collect::<Vec<_>>(),
        ["", ww_helper().as_str()]
    );
    let out = git(
        &r,
        &["credential", "fill"],
        "protocol=https\nhost=github.com\npath=owner/repo.git\n\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("username=x-access-token\n"), "{stdout}");
    assert!(
        stdout.contains(&format!("password={FAKE_TOKEN}\n")),
        "{stdout}"
    );
    assert!(!stdout.contains(GLOBAL_PASSWORD), "{stdout}");
}

#[test] // spec: QA-079
fn credential_fill_for_another_host_gets_nothing() {
    let r = repo();
    let out = git(
        &r,
        &["credential", "fill"],
        "protocol=https\nhost=example.com\n\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // No helper answers and prompting is off, so git gives up.
    assert!(!out.status.success(), "{stdout}");
    assert!(!stdout.contains("password="), "{stdout}");
    assert!(!stdout.contains(FAKE_TOKEN), "{stdout}");
}

#[test] // spec: QA-079
fn credential_approve_and_reject_are_harmless() {
    let r = repo();
    for action in ["approve", "reject"] {
        let out = git(
            &r,
            &["credential", action],
            "protocol=https\nhost=github.com\nusername=x\npassword=y\n\n",
        );
        assert!(
            out.status.success(),
            "{action}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test] // spec: QA-079
fn the_helper_speaks_gits_protocol_directly() {
    let r = repo();
    let ww = env!("CARGO_BIN_EXE_ww");
    let get = run(
        ww,
        &["dev", "git-credential", "get"],
        &r.dir,
        &r.global,
        "protocol=https\nhost=github.com\n\n",
    );
    assert!(get.status.success());
    assert_eq!(
        String::from_utf8_lossy(&get.stdout),
        format!("username=x-access-token\npassword={FAKE_TOKEN}\n")
    );
    for (action, input) in [
        ("get", "protocol=https\nhost=example.com\n\n"),
        ("store", "protocol=https\nhost=github.com\npassword=x\n\n"),
        ("erase", "protocol=https\nhost=github.com\n\n"),
    ] {
        let out = run(
            ww,
            &["dev", "git-credential", action],
            &r.dir,
            &r.global,
            input,
        );
        assert!(out.status.success(), "{action}");
        assert!(out.stdout.is_empty(), "{action}");
    }
}

#[test] // spec: QA-079
fn agent_token_set_refuses_without_a_terminal() {
    let r = repo();
    let out = run(
        env!("CARGO_BIN_EXE_ww"),
        &["dev", "agent-token", "set"],
        &r.dir,
        &r.global,
        "github_pat_typed_into_a_pipe\n2027-01-31\n",
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("real terminal"), "{stderr}");
    assert!(stderr.contains("nothing was stored"), "{stderr}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("github_pat_"));
}
