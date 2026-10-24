//! `LIL_TESTBINS_DIR` and `LIL_SANDBOX` in `lil_testkit::bins` (QA-042, QA-056; P4a-R31): inside
//! Windows Sandbox there is no cargo, so the test binaries come from the folder the runner
//! bundles, and nothing is ever built.

use std::path::{Path, PathBuf};

use lil_testkit::bins::{Located, locate};

/// The file names every test binary folder holds.
const NAMES: [&str; 7] = [
    "lil.exe",
    "lil-core.exe",
    "lil-mcp.exe",
    "lil-test-app.exe",
    "lil-fake-comfyui.exe",
    "lil-fake-comfy-cli.exe",
    "claude.exe",
];

fn fill(dir: &Path, names: &[&str]) {
    for name in names {
        std::fs::write(dir.join(name), b"").unwrap();
    }
}

#[test] // spec: QA-042, QA-056
fn testbins_dir_override_is_used_before_the_compile_time_path() {
    let dir = tempfile::tempdir().unwrap();
    fill(dir.path(), &NAMES);
    let compiled = PathBuf::from(r"C:\nowhere\target\testbins\debug");
    for sandbox in [false, true] {
        let Located::Use(bins) = locate(Some(dir.path()), sandbox, &compiled).unwrap() else {
            panic!("an override that holds every binary is used as it is, sandbox={sandbox}");
        };
        assert_eq!(bins.dir, dir.path());
        assert_eq!(bins.lil, dir.path().join("lil.exe"));
        assert_eq!(bins.lil_core, dir.path().join("lil-core.exe"));
        assert_eq!(bins.claude, dir.path().join("claude.exe"));
    }
    // Without the override, outside the Sandbox: the compile-time folder, built when needed.
    let Located::BuildIfNeeded(bins) = locate(None, false, &compiled).unwrap() else {
        panic!("without the override the compile-time folder is used");
    };
    assert_eq!(bins.dir, compiled);
}

#[test] // spec: QA-042
fn a_missing_binary_under_the_override_fails_without_building() {
    let dir = tempfile::tempdir().unwrap();
    fill(dir.path(), &NAMES[..6]);
    let compiled = PathBuf::from(r"C:\nowhere\target\testbins\debug");
    let err = match locate(Some(dir.path()), false, &compiled) {
        Err(e) => e,
        Ok(other) => panic!("a missing binary must fail, not {other:?}"),
    };
    assert_eq!(
        err,
        format!(
            r"test binaries not found in {}; the Sandbox runner bundles target\testbins\debug",
            dir.path().display()
        )
    );
}

#[test] // spec: QA-042
fn in_the_sandbox_without_the_override_it_fails_with_the_message() {
    let compiled = PathBuf::from(r"C:\nowhere\target\testbins\debug");
    let err = match locate(None, true, &compiled) {
        Err(e) => e,
        Ok(other) => panic!("the Sandbox has no cargo: nothing may be built, not {other:?}"),
    };
    assert_eq!(
        err,
        r"test binaries not found in C:\nowhere\target\testbins\debug; the Sandbox runner bundles target\testbins\debug"
    );
}
