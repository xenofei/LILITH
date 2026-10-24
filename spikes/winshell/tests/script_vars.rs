//! Fix round 4 (review N3-C1): a rename in fix round 3 left `HelloWatched` reading
//! `$script:PopupPids`, which nothing assigned any more; under `Set-StrictMode -Version Latest`
//! the read throws, and S1.8 stopped with ERROR on every run. This test fails when a script reads
//! a `$script:Name` that no script assigns, either as `$script:Name = ...` (or `+=`) or as a plain
//! `$Name = ...` (a top-level script variable). The scripts dot-source each other, so a name
//! assigned in any of them counts. tests/simulate.ps1 runs the same paths for real.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out: Vec<PathBuf> = [
        "run-spike.ps1",
        "run-sandbox.ps1",
        "run-all.ps1",
        "uia.ps1",
        "inside.ps1",
        "popup.ps1",
    ]
    .iter()
    .map(|s| root.join(s))
    .collect();
    for e in std::fs::read_dir(root.join("tests"))
        .expect("tests dir")
        .flatten()
    {
        let p = e.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ps1")) {
            out.push(p);
        }
    }
    out
}

fn ident(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

/// Whether what follows a variable name is an assignment (`=`, `+=`, `-=`), not `==` or `-eq`.
fn assigns(rest: &str) -> bool {
    let r = rest.trim_start();
    (r.starts_with('=') && !r.starts_with("==")) || r.starts_with("+=") || r.starts_with("-=")
}

/// (reads, assignments) of `$script:` names in one script's text, case-insensitive.
fn scan(text: &str) -> (Vec<(String, usize)>, BTreeSet<String>) {
    let mut reads = Vec::new();
    let mut assigned = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with('#') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        let mut at = 0;
        while let Some(i) = lower[at..].find("$script:") {
            let start = at + i + "$script:".len();
            let name = ident(&lower[start..]);
            if !name.is_empty() {
                if assigns(&lower[start + name.len()..]) {
                    assigned.insert(name);
                } else {
                    reads.push((name, n + 1));
                }
            }
            at = start;
        }
        // A plain `$Name = ...` at the start of a statement assigns a script variable when it
        // runs at the top level; counted wherever it is, which can only hide a finding, never
        // make one.
        if let Some(rest) = code.strip_prefix('$') {
            let lower_rest = rest.to_ascii_lowercase();
            let name = ident(&lower_rest);
            if !name.is_empty()
                && !lower_rest.starts_with("script:")
                && assigns(&lower_rest[name.len()..])
            {
                assigned.insert(name);
            }
        }
    }
    (reads, assigned)
}

#[test]
fn every_script_variable_read_is_assigned_somewhere() {
    let mut assigned = BTreeSet::new();
    let mut reads = Vec::new();
    for path in files() {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let (r, a) = scan(&text);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        reads.extend(r.into_iter().map(|(v, n)| (name.clone(), n, v)));
        assigned.extend(a);
    }
    let missing: Vec<String> = reads
        .iter()
        .filter(|(_, _, v)| !assigned.contains(v))
        .map(|(f, n, v)| format!("{f}:{n}: $script:{v}"))
        .collect();
    assert!(
        missing.is_empty(),
        "$script: variables read but never assigned:\n{}",
        missing.join("\n")
    );
}

#[test]
fn the_scan_finds_a_read_of_a_removed_variable() {
    let (reads, assigned) = scan(
        "$script:Popups = @()\nif ($f.pid -notin $script:PopupPids) { }\n$Other = 1\n$script:Popups += 1\n",
    );
    let unassigned: Vec<_> = reads
        .iter()
        .filter(|(v, _)| !assigned.contains(v))
        .collect();
    assert_eq!(unassigned, vec![&("popuppids".to_string(), 2)]);
    assert!(assigned.contains("other") && assigned.contains("popups"));
}
