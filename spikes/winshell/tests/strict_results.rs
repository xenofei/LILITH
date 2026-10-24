//! Rerun review I1 and M1: run-spike.ps1's S1.3, S1.4 and S1.7 verdict logic under its own
//! strict mode (tests/strict-results.ps1). Needs PowerShell 7 (`pwsh`). Touches nothing.

use std::path::Path;
use std::process::Command;

#[test]
fn verdicts_hold_under_strict_mode() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("strict-results.ps1");
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(&script)
        .output()
        .expect("run pwsh");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("{text}");
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("all cases passed"), "{text}");
}
