//! Owner rerun of 2026-10-23: `run-sandbox.ps1` stopped at once with "Task 0's deploy folder is
//! missing", because Task 0's deploy lives in its own worktree. `-Preflight` starts nothing and
//! prints where the deploy folder and the Qt exe were found. These cases use temp folders only.
//! Needs `pwsh`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn preflight(args: &[&str], env_repo: Option<&Path>) -> serde_json::Value {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("run-sandbox.ps1");
    let mut cmd = Command::new("pwsh");
    cmd.args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(&script)
        .arg("-Preflight")
        .args(args)
        .env_remove("LILITH_TASK0_REPO");
    if let Some(r) = env_repo {
        cmd.env("LILITH_TASK0_REPO", r);
    }
    let out = cmd.output().expect("run pwsh");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// A fake Task 0 worktree whose deploy folder holds the three .exe files the real one has.
fn fake_task0() -> PathBuf {
    let root = std::env::temp_dir().join(format!("winshell-task0-test-{}", std::process::id()));
    let deploy = root.join(r"spikes\app-shell\build\deploy");
    std::fs::create_dir_all(&deploy).expect("mkdir");
    for exe in [
        "app_shell_spike.exe",
        "QtWebEngineProcess.exe",
        "vc_redist.x64.exe",
    ] {
        std::fs::write(deploy.join(exe), b"").expect("write");
    }
    root
}

#[test]
fn task0_repo_env_and_deploy_dir_find_the_deploy_and_the_app_exe() {
    let root = fake_task0();
    let deploy = root.join(r"spikes\app-shell\build\deploy");
    let r = root.to_str().unwrap();

    let v = preflight(&["-Task0Repo", r], None);
    assert_eq!(v["deploy"]["from"], "-Task0Repo");
    assert_eq!(v["deploy"]["path"], deploy.to_str().unwrap());
    assert_eq!(v["deploy"]["exists"], true);
    assert_eq!(v["qt_exe"], "app_shell_spike.exe");
    assert_eq!(
        v["qt_grab_args"],
        "--screenshot {png} --quit-after 5000 --no-activate"
    );

    let v = preflight(&[], Some(&root));
    assert_eq!(v["deploy"]["from"], "LILITH_TASK0_REPO");
    assert_eq!(v["qt_exe"], "app_shell_spike.exe");

    let v = preflight(
        &[
            "-DeployDir",
            deploy.to_str().unwrap(),
            "-Task0Repo",
            r"X:\nowhere",
        ],
        None,
    );
    assert_eq!(v["deploy"]["from"], "-DeployDir");
    assert_eq!(v["deploy"]["exists"], true);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_deploy_is_reported_not_guessed() {
    let v = preflight(&["-Task0Repo", r"X:\no-such-task0-worktree"], None);
    assert_eq!(v["ready"], false);
    assert_eq!(v["deploy"]["exists"], false);
    assert!(v["qt_exe"].is_null());
    let why = v["why"].to_string();
    assert!(why.contains("Task 0's deploy folder is missing"), "{why}");
}
