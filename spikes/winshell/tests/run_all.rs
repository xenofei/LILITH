//! Automation round (owner direction 2026-10-23): `run-all.ps1` runs a session in two blocks,
//! `-Block Attended` and `-Block Unattended`. Its
//! `-DryRun` prints the stages it would run and stops; nothing else runs. Needs `pwsh`.

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
fn the_attended_block_is_part_b_then_the_watched_sandbox_run() {
    let (code, out, err) = dry_run(&[]);
    assert_eq!(code, Some(0), "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["block"], "Attended");
    let s = stages(&v);
    assert_eq!(s.len(), 2, "{out}");
    assert_eq!(s[0].0, "run-spike.ps1");
    assert_eq!(
        s[0].1,
        vec![
            "-Only",
            "S1.2p,S1.3,S1.4,S1.5,S1.7,S1.8",
            "-PowerPart",
            "sleep-wake"
        ]
    );
    assert_eq!(
        (s[1].0.as_str(), s[1].1.as_slice()),
        ("run-sandbox.ps1", &["-Observe".to_string()][..])
    );
}

#[test]
fn the_unattended_block_is_the_stay_awake_wait_first_then_the_sandbox_runs() {
    let (code, out, err) = dry_run(&["-Block", "Unattended"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = stages(&v);
    assert_eq!(s.len(), 2, "{out}");
    assert_eq!(s[0].0, "run-spike.ps1");
    assert_eq!(
        s[0].1,
        vec![
            "-Only",
            "S1.7",
            "-Append",
            "-PowerPart",
            "stay-awake",
            "-SkipBuild"
        ]
    );
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
    let (_, out, _) = dry_run(&["-Only", "s1.4", "-Block", "Unattended"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(stages(&v).len(), 0, "{out}");
}

#[test]
fn the_task0_folder_is_passed_to_both_sandbox_stages() {
    for block in ["Attended", "Unattended"] {
        let (code, out, err) = dry_run(&["-Block", block, "-Only", "S1.9", "-Task0Repo", r"X:\t0"]);
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
}

#[test]
fn an_unknown_name_stops_before_anything_runs() {
    let (code, out, err) = dry_run(&["-Only", "S1.10"]);
    assert_ne!(code, Some(0), "{out}{err}");
    assert!(err.contains("Unknown check name in -Only: S1.10"), "{err}");
    assert!(out.is_empty(), "printed: {out}");
}
