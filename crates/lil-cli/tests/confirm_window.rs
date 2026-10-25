//! `lil confirm-window [<id>]` (APP-049, ARCH-016; Appendix G "The `lil` commands this plan
//! adds", P4a-R4): it starts the `lil-image.exe` beside `lil.exe` with `--confirm [<id>]` and
//! says so, refuses with `--non-interactive`, without the app beside it, or for an id that isn't
//! a confirmation id. Driven through a copy of the real `lil` beside `lil-fake-image` (as
//! `lil-image.exe`), which records how it was started (QA-066).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lil_testkit::{LilOutput, TestEnv, ensure_built};
use serde_json::{Value, json};

const ID: &str = "cf_0123456789abcdef";

/// `lil.exe` copied into `<test root>\bin`, with `lil-fake-image.exe` beside it as
/// `lil-image.exe` when `with_app`; returns the copied `lil.exe`.
fn lil_in_bin(env: &TestEnv, with_app: bool) -> PathBuf {
    let bin = env.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let bins = ensure_built();
    let mut copies = vec![(bins.lil.clone(), "lil.exe")];
    if with_app {
        copies.push((bins.fake_image.clone(), "lil-image.exe"));
    }
    for (from, to) in copies {
        std::fs::copy(&from, bin.join(to))
            .unwrap_or_else(|e| panic!("cannot copy {} as {to}: {e}", from.display()));
    }
    bin.join("lil.exe")
}

/// Runs the `lil` at `exe` with `args` on the test channel, with an empty stdin; waits for it.
fn run(env: &TestEnv, exe: &Path, args: &[&str]) -> LilOutput {
    let start = Instant::now();
    let out = Command::new(exe)
        .args(args)
        .current_dir(env.root())
        .envs(env.child_env())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", exe.display()));
    LilOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        elapsed: start.elapsed(),
    }
}

/// `lil-fake-image`'s record file.
fn recorded(env: &TestEnv) -> PathBuf {
    env.root().join("fake-image-args.jsonl")
}

/// The record lines so far.
fn records(env: &TestEnv) -> Vec<Value> {
    match std::fs::read_to_string(recorded(env)) {
        Ok(text) => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("cannot read {}: {e}", recorded(env).display()),
    }
}

/// The one record `lil-fake-image` writes, once it is there (within 30 s).
fn the_record(env: &TestEnv) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut all = records(env);
        if !all.is_empty() {
            assert_eq!(all.len(), 1, "one start: {all:?}");
            return all.remove(0);
        }
        assert!(Instant::now() < deadline, "lil-fake-image never ran");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The pid of `confirm window started (pid <pid>)`, the whole of stdout.
fn started_pid(out: &LilOutput) -> u32 {
    out.stdout
        .trim()
        .strip_prefix("confirm window started (pid ")
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("{out:?}"))
}

#[test] // spec: APP-049
fn starts_the_app_with_confirm_and_an_id() {
    let env = TestEnv::new();
    let lil = lil_in_bin(&env, true);
    let out = run(&env, &lil, &["confirm-window", ID]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(started_pid(&out) > 0, "{out:?}");
    let line = the_record(&env);
    assert_eq!(line["args"], json!(["--confirm", ID]), "{line}");
    // Started for you at your terminal: shown as Windows starts any program, not without focus.
    assert_eq!(line["use_show_window"], false, "{line}");
}

#[test] // spec: APP-049
fn without_an_id_lists_all() {
    let env = TestEnv::new();
    let lil = lil_in_bin(&env, true);
    let out = run(&env, &lil, &["confirm-window"]);
    assert_eq!(out.code, 0, "{out:?}");
    started_pid(&out);
    assert_eq!(the_record(&env)["args"], json!(["--confirm"]));
}

#[test] // spec: APP-049, ARCH-055
fn non_interactive_exits_2() {
    let env = TestEnv::new();
    let lil = lil_in_bin(&env, true);
    for args in [
        &["--non-interactive", "confirm-window", ID][..],
        &["confirm-window", "--non-interactive"],
    ] {
        let out = run(&env, &lil, args);
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(
            out.stderr
                .contains("the confirm window is for interactive use"),
            "{args:?}: {out:?}"
        );
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
    }
    assert!(records(&env).is_empty(), "nothing was started");
}

#[test] // spec: APP-049
fn no_app_beside_lil_exits_2() {
    let env = TestEnv::new();
    let lil = lil_in_bin(&env, false);
    let out = run(&env, &lil, &["confirm-window", ID]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr.contains("lil-image.exe isn't beside lil.exe"),
        "{out:?}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
}

#[test] // spec: APP-049, APP-020
fn a_malformed_id_starts_nothing() {
    let env = TestEnv::new();
    let lil = lil_in_bin(&env, true);
    for id in [
        "cf_0123",
        "cf_0123456789ABCDEF",
        "j_0123456789abcdef",
        "--no-activate",
        "cf_0123456789abcdef/",
    ] {
        let out = run(&env, &lil, &["confirm-window", "--", id]);
        assert_eq!(out.code, 2, "{id}: {out:?}");
        assert!(
            out.stderr.contains("isn't a confirmation id"),
            "{id}: {out:?}"
        );
    }
    assert!(records(&env).is_empty(), "nothing was started");
}
