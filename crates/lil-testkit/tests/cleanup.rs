//! The in-process fixtures of `lil-core`'s tests shut down before their `TestEnv` drops, so no
//! test folder is left in `%TEMP%` (the rename's carry; plan-2b final review).
//!
//! Each fixture's `shutdown()` closes the hub and joins the state actors, then fails its test if
//! `TestEnv::close` leaves anything behind, so every run of those tests checks it. This test
//! runs five of them in one `cargo test` process, the way the leaks showed up (many tests, one
//! process), and checks that [`lil_testkit::cleanup::leftovers_log`] gained no entry for them.
//! It runs cargo, so it is ignored by default:
//! `cargo nextest run -p lil-testkit --run-ignored only -E 'test(leave_no_test_root)'`.

use std::path::Path;
use std::process::Command;

/// The five tests, as `(test binary, test name)`.
const TESTS: [(&str, &str); 5] = [
    (
        "assets",
        "import_copy_creates_asset_with_uuid_and_candidate_run",
    ),
    (
        "record",
        "record_runs_after_meter_and_applies_the_plan_in_write_order",
    ),
    (
        "projects",
        "resolve_explicit_then_session_then_hint_then_sketchbook",
    ),
    ("settings", "cascade_is_a_plain_override_chain"),
    (
        "versions",
        "seal_moves_files_and_writes_a_read_only_snapshot",
    ),
];

/// The leftovers log's text from byte `from` on (empty when there is no log).
fn log_since(from: u64) -> String {
    let bytes = std::fs::read(lil_testkit::cleanup::leftovers_log()).unwrap_or_default();
    let from = usize::try_from(from).unwrap_or(usize::MAX).min(bytes.len());
    String::from_utf8_lossy(&bytes[from..]).into_owned()
}

#[test] // spec: QA-040
#[ignore = "runs cargo test for five lil-core test binaries"]
fn in_process_fixtures_leave_no_test_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("lil-testkit lives at <workspace>/crates/lil-testkit");
    let before = std::fs::metadata(lil_testkit::cleanup::leftovers_log()).map_or(0, |m| m.len());

    let mut cargo = Command::new(env!("CARGO"));
    cargo.args(["test", "--locked", "--offline", "-p", "lil-core"]);
    cargo.arg("--manifest-path").arg(root.join("Cargo.toml"));
    for (binary, _) in TESTS {
        cargo.args(["--test", binary]);
    }
    cargo.args(["--", "--exact"]);
    cargo.args(TESTS.iter().map(|(_, name)| *name));
    let out = cargo.current_dir(root).output().expect("cargo runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the five tests failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (_, name) in TESTS {
        assert!(
            stdout.contains(&format!("test {name} ... ok")),
            "{name} didn't run:\n{stdout}"
        );
    }

    let added = log_since(before);
    let leaked: Vec<&str> = TESTS
        .iter()
        .map(|(_, name)| *name)
        .filter(|name| added.contains(&format!("test `{name}`")))
        .collect();
    assert!(
        leaked.is_empty(),
        "these tests left their folder behind: {leaked:?}\n{added}"
    );
}
