//! Owner run 3 (owner q1/q2): `run-all.ps1` is one unattended command — Part B on the host, then
//! Part C in Windows Sandbox. Its `-DryRun` prints the stages it would run and stops; nothing else
//! runs. Needs `pwsh`.

use std::path::Path;
use std::process::Command;

fn dry_run(extra: &[&str]) -> (Option<i32>, String, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("run-all.ps1");
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(&script)
        .args(extra)
        .arg("-DryRun")
        .output()
        .expect("run pwsh");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn stages(v: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    v["stages"]
        .as_array()
        .expect("stages")
        .iter()
        .map(|s| {
            let args = s["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| x.as_str().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            (s["script"].as_str().unwrap_or("").to_string(), args)
        })
        .collect()
}

#[test]
fn the_one_command_is_part_b_then_the_sandbox_runs() {
    let (code, out, err) = dry_run(&[]);
    assert_eq!(code, Some(0), "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = stages(&v);
    assert_eq!(s.len(), 2, "{out}");
    assert_eq!(s[0].0, "run-spike.ps1");
    // Part B: the host checks, one run, no -PowerPart and no -Append (S1.7 is self-contained now).
    assert_eq!(s[0].1, vec!["-Only", "S1.2p,S1.3,S1.4,S1.5,S1.7,S1.8"]);
    // Part C: 3 Sandbox runs, kept awake, no -Observe (no watched run, owner run 3).
    assert_eq!(s[1].0, "run-sandbox.ps1");
    assert_eq!(s[1].1, vec!["-Runs", "3", "-KeepAwake"]);
}

#[test]
fn a_rerun_of_one_check_runs_only_that_part() {
    let (code, out, err) = dry_run(&["-Only", "s1.4"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = stages(&v);
    assert_eq!(s.len(), 1, "{out}");
    assert_eq!(s[0].1, vec!["-Only", "S1.4"]);
    let (_, out, _) = dry_run(&["-Only", "s1.9"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = stages(&v);
    assert_eq!(s.len(), 1, "{out}");
    assert_eq!(s[0].0, "run-sandbox.ps1");
}

#[test]
fn the_task0_folder_is_passed_to_the_sandbox_stage() {
    let (code, out, err) = dry_run(&["-Only", "S1.9", "-Task0Repo", r"X:\t0"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = stages(&v);
    assert_eq!(s.len(), 1, "{out}");
    assert_eq!(s[0].0, "run-sandbox.ps1");
    assert!(
        s[0].1
            .ends_with(&["-Task0Repo".to_string(), r"X:\t0".to_string()]),
        "{:?}",
        s[0].1
    );
}

#[test]
fn an_unknown_name_stops_before_anything_runs() {
    let (code, out, err) = dry_run(&["-Only", "S1.10"]);
    assert_ne!(code, Some(0), "{out}{err}");
    assert!(err.contains("Unknown check name in -Only: S1.10"), "{err}");
    assert!(out.is_empty(), "printed: {out}");
}
