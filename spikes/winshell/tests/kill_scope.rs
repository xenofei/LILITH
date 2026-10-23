//! Fix round 3, N2-C1: a `Process` from `Get-Process -Id` holds no handle, so a later
//! `HasExited` or `Kill()` on it opens whatever process has that pid by then. The scripts may
//! turn a pid into a Process only through `Get-PinnedProcess` (uia.ps1), right after they start
//! it, and may end processes only through `Stop-PinnedProcess` or a line that says why it is
//! safe (`# kill-scope:`). This test fails when a script looks a pid up later to kill or check
//! it. The behaviour of a pinned process is tested in tests/strict-results.ps1.

use std::path::Path;

const SCRIPTS: &[&str] = &[
    "run-spike.ps1",
    "run-sandbox.ps1",
    "run-all.ps1",
    "uia.ps1",
    "inside.ps1",
    "popup.ps1",
];

fn lines(name: &str) -> Vec<(usize, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.to_string()))
        .collect()
}

#[test]
fn a_pid_becomes_a_process_only_where_it_is_pinned_or_only_read() {
    let mut bad = Vec::new();
    for name in SCRIPTS {
        for (n, l) in lines(name) {
            if l.trim_start().starts_with('#') {
                continue;
            }
            if l.contains("Get-Process -Id")
                && !l.contains("# pin-lookup")
                && !l.contains("# read-only pid lookup")
            {
                bad.push(format!("{name}:{n}: {}", l.trim()));
            }
            if l.contains("Wait-Process -Id") || l.contains("Stop-Process -Id") {
                bad.push(format!("{name}:{n}: {}", l.trim()));
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
    for name in SCRIPTS {
        let mut in_stop_pinned = false;
        for (n, l) in lines(name) {
            if l.starts_with("function Stop-PinnedProcess") {
                in_stop_pinned = true;
            } else if l.starts_with("function ") {
                in_stop_pinned = false;
            }
            if l.trim_start().starts_with('#') {
                continue;
            }
            let kills = l.contains(".Kill()") || l.contains("Stop-Process");
            if kills && !in_stop_pinned && !l.contains("# kill-scope:") {
                bad.push(format!("{name}:{n}: {}", l.trim()));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "kills outside Stop-PinnedProcess without a reason:\n{}",
        bad.join("\n")
    );
}
