//! Scope cut (coordinator's ruling, 2026-10-23; review C1): nothing the default `run-all.ps1`
//! reaches may wait for a person. tests/no-prompt.ps1 takes the default stages from `-DryRun`,
//! adds the scripts they load, and fails on any prompting call in their syntax trees. Needs
//! PowerShell 7 (`pwsh`). Touches nothing.

use std::path::Path;
use std::process::Command;

#[test]
fn no_prompt_is_reachable_by_the_default_run() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("no-prompt.ps1");
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
    assert!(text.contains("no prompt is reachable"), "{text}");
}
