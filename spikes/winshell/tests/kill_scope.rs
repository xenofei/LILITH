//! Fix round 3, N2-C1: a `Process` from `Get-Process -Id` holds no handle, so a later
//! `HasExited` or `Kill()` on it opens whatever process has that pid by then. The scripts may
//! turn a pid into a Process only through `Get-PinnedProcess` (uia.ps1), right after they start
//! it, and may end processes only through `Stop-PinnedProcess` or a line that says why it is
//! safe (`# kill-scope:`). This test fails when a script looks a pid up later to kill or check
//! it. The behaviour of a pinned process is tested in tests/strict-results.ps1.
//!
//! Fix round 4, m5: the matches ignore case and cover every form (`Get-Process -ID`,
//! `[Diagnostics.Process]::GetProcessById`, `.Kill($true)`, `taskkill`, `TerminateProcess`),
//! in the six scripts and in the tests' own PowerShell (tests/*.ps1).

use std::path::{Path, PathBuf};

const SCRIPTS: &[&str] = &[
    "run-spike.ps1",
    "run-sandbox.ps1",
    "run-all.ps1",
    "uia.ps1",
    "inside.ps1",
    "popup.ps1",
];

fn files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out: Vec<PathBuf> = SCRIPTS.iter().map(|s| root.join(s)).collect();
    let mut tests: Vec<PathBuf> = std::fs::read_dir(root.join("tests"))
        .expect("tests dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ps1")))
        .collect();
    tests.sort();
    out.extend(tests);
    out
}

/// (file name, line number, the line as written, the line lower-cased).
fn lines(path: &Path) -> Vec<(String, usize, String, String)> {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .lines()
        .enumerate()
        .map(|(i, l)| (name.clone(), i + 1, l.to_string(), l.to_ascii_lowercase()))
        .collect()
}

fn pid_lookup(l: &str) -> bool {
    l.contains("get-process -id") || l.contains("getprocessbyid")
}

fn pid_wait_or_kill(l: &str) -> bool {
    l.contains("wait-process") || l.contains("stop-process -id")
}

fn kills(l: &str) -> bool {
    l.contains(".kill(")
        || l.contains("stop-process")
        || l.contains("taskkill")
        || l.contains("terminateprocess")
}

#[test]
fn a_pid_becomes_a_process_only_where_it_is_pinned_or_only_read() {
    let mut bad = Vec::new();
    for path in files() {
        for (name, n, raw, l) in lines(&path) {
            if l.trim_start().starts_with('#') {
                continue;
            }
            if pid_lookup(&l)
                && !l.contains("# pin-lookup")
                && !l.contains("# read-only pid lookup")
            {
                bad.push(format!("{name}:{n}: {}", raw.trim()));
            }
            if pid_wait_or_kill(&l) {
                bad.push(format!("{name}:{n}: {}", raw.trim()));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "pid lookups outside Get-PinnedProcess:\n{}",
        bad.join("\n")
    );
}

#[test]
fn processes_are_ended_only_through_pinned_objects_or_a_stated_reason() {
    let mut bad = Vec::new();
    for path in files() {
        let mut in_stop_pinned = false;
        for (name, n, raw, l) in lines(&path) {
            if l.starts_with("function stop-pinnedprocess") {
                in_stop_pinned = true;
            } else if l.starts_with("function ") {
                in_stop_pinned = false;
            }
            if l.trim_start().starts_with('#') {
                continue;
            }
            if kills(&l) && !in_stop_pinned && !l.contains("# kill-scope:") {
                bad.push(format!("{name}:{n}: {}", raw.trim()));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "kills outside Stop-PinnedProcess without a reason:\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_patterns_catch_every_form() {
    for l in [
        "$p = Get-Process -ID 4",
        "[Diagnostics.Process]::GetProcessById(4)",
        "Wait-Process -Id 4",
        "STOP-PROCESS -Id 4",
    ] {
        let l = l.to_ascii_lowercase();
        assert!(pid_lookup(&l) || pid_wait_or_kill(&l), "{l}");
    }
    for l in [
        "$p.Kill($true)",
        "$p.Kill()",
        "taskkill /PID 4 /F",
        "Stop-Process -Force",
    ] {
        assert!(kills(&l.to_ascii_lowercase()), "{l}");
    }
}
