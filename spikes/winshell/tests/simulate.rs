//! run-all.ps1's default run (S1.9 in the Sandbox, after the 2026-10-23 scope cut) end to end
//! under the scripts' own strict mode, with every Windows call faked (tests/simulate.ps1,
//! tests/sim-fakes.ps1): the Qt step runs, nothing prompts. Nothing on this PC is touched; the
//! only real processes are the fakes' windowless pings. Needs PowerShell 7 (`pwsh`).

use std::path::Path;
use std::process::Command;

#[test]
fn the_default_run_goes_end_to_end_under_strict_mode_with_fakes() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("simulate.ps1");
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
