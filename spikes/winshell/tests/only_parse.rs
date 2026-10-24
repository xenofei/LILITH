//! Rerun review C1: `-Only` given to `pwsh -File` arrives as one comma-joined string. The script
//! splits it, trims the names and refuses unknown ones before anything runs. `-DryRun` prints the
//! parsed list as JSON and stops right after the parse (nothing else in the script runs).

use std::path::Path;
use std::process::Command;

fn dry_run(only: Option<&str>) -> (Option<i32>, String, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("run-spike.ps1");
    let mut cmd = Command::new("pwsh");
    cmd.args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(&script);
    if let Some(o) = only {
        cmd.args(["-Only", o]);
    }
    cmd.arg("-DryRun");
    let out = cmd.output().expect("run pwsh");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn parsed(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"))
}

#[test]
fn a_comma_list_through_pwsh_file_becomes_the_named_checks() {
    let (code, out, err) = dry_run(Some("S1.5,S1.6"));
    assert_eq!(code, Some(0), "{out}{err}");
    let v = parsed(&out);
    assert_eq!(v["only"], serde_json::json!(["S1.5", "S1.6"]));
    assert_eq!(v["runs"], serde_json::json!(["S1.5", "S1.6"]));
}

#[test]
fn the_rerun_command_names_seven_checks_and_spaces_are_trimmed() {
    let (code, out, err) = dry_run(Some("S1.2p, S1.3 ,S1.4,S1.5,S1.6,S1.7,s1.8"));
    assert_eq!(code, Some(0), "{out}{err}");
    let v = parsed(&out);
    assert_eq!(
        v["runs"],
        serde_json::json!(["S1.2p", "S1.3", "S1.4", "S1.5", "S1.6", "S1.7", "S1.8"])
    );
}

#[test]
fn no_only_runs_every_check() {
    let (code, out, err) = dry_run(None);
    assert_eq!(code, Some(0), "{out}{err}");
    let v = parsed(&out);
    assert_eq!(v["only"], serde_json::json!([]));
    assert_eq!(v["runs"].as_array().map(Vec::len), Some(9));
}

#[test]
fn an_unknown_name_stops_before_anything_runs() {
    let (code, out, err) = dry_run(Some("S1.3,S1.9"));
    assert_ne!(code, Some(0), "{out}{err}");
    assert!(err.contains("Unknown check name in -Only: S1.9"), "{err}");
    assert!(out.is_empty(), "printed: {out}");
}
