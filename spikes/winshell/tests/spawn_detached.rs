//! C1 (fix round 1): `winshell-tool spawn` must return at once even when its own stdout is a
//! pipe its caller reads to EOF, as PowerShell's `$out = & winshell-tool ...` does. A child that
//! inherited that pipe would hold it open until it exits. Touches no desktop state: the child
//! is `ping -n 31 127.0.0.1` (about 30 s of loopback pings), killed at the end.
//!
//! Two checks:
//! - decisive and independent of machine load: the child is still running when the spawn's
//!   output reaches EOF (with the pipe inherited, EOF comes only when the child exits);
//! - the 2 s bar: the spawn takes less than 2 s longer than a plain `winshell-tool` run timed
//!   just before it (an absolute 2 s fails on a loaded machine, where starting any exe can
//!   take seconds).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run_tool(args: &[&str]) -> (Duration, String) {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_winshell-tool"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run winshell-tool");
    (
        started.elapsed(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn spawn_returns_before_a_30_s_child_exits_with_stdout_piped() {
    let (plain, _) = run_tool(&["b64url", "x"]);
    let (spawn, text) = run_tool(&["spawn", "--", "ping.exe", "-n", "31", "127.0.0.1"]);
    let reply: serde_json::Value = serde_json::from_str(&text).expect("one JSON line");
    let pid = reply["pid"].as_u64().map(|p| p as u32);
    let created = reply["created"].as_u64().unwrap_or(0);
    let image = pid.and_then(winshell::win::process_image);
    let alive = image.is_some();
    // Ended only while it still runs PING.EXE, the child this test started (fix round 4, m5).
    let is_ping = image
        .as_ref()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n.eq_ignore_ascii_case("PING.EXE"));
    if let (Some(pid), true) = (pid, alive && is_ping) {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    eprintln!("plain run {plain:?}, spawn {spawn:?}, child alive at return: {alive}");
    assert!(pid.is_some(), "spawn reported no pid: {text}");
    assert!(created > 0, "spawn reported no creation time: {text}");
    assert!(
        alive,
        "the child had exited when the spawn returned after {spawn:?}: it held the caller's stdout pipe"
    );
    assert!(
        spawn < plain + Duration::from_secs(2),
        "spawn took {spawn:?}, a plain run {plain:?}"
    );
}
