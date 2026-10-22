//! I3 (fix round 1): `inside.ps1` refuses to run outside Windows Sandbox. Run on the host with
//! `-GuardOnly`, so even a broken guard can't reach anything past it (`-GuardOnly` exits 0 right
//! after the guard). Uses Windows PowerShell 5.1, the Sandbox's shell.

use std::path::Path;
use std::process::Command;

fn run_guard(username: Option<&str>) -> (Option<i32>, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("inside.ps1");
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ])
    .arg(&script)
    .arg("-GuardOnly");
    if let Some(name) = username {
        cmd.env("USERNAME", name);
    }
    let out = cmd.output().expect("run powershell.exe");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn inside_ps1_refuses_to_run_on_the_host() {
    let (code, stderr) = run_guard(None);
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("runs only inside Windows Sandbox"),
        "stderr: {stderr}"
    );
}

#[test]
fn the_sandbox_user_name_alone_is_not_enough() {
    // Without the marker file the bundle maps in, the Sandbox's user name doesn't pass.
    assert!(!Path::new(r"C:\lil\in\in-sandbox.marker").exists());
    let (code, stderr) = run_guard(Some("WDAGUtilityAccount"));
    assert_eq!(code, Some(2), "stderr: {stderr}");
}
