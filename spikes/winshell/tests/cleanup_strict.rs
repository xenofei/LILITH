//! Part B run 1 analysis: run-spike.ps1's cleanup under its own strict mode, with fakes for
//! everything that would touch this PC (tests/cleanup-strict.ps1). Needs PowerShell 7 (`pwsh`).

use std::path::Path;
use std::process::Command;

#[test]
fn cleanup_passes_under_strict_mode_when_everything_is_gone() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cleanup-strict.ps1");
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
