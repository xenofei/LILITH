//! `LIL_TESTBINS_DIR` and `LIL_SANDBOX` in `lil_testkit::bins` (QA-042, QA-056; P4a-R31): inside
//! Windows Sandbox there is no cargo, so the test binaries come from the folder the runner
//! bundles, and nothing is ever built.

use std::path::{Path, PathBuf};

use lil_testkit::bins::{Located, locate, locate_from};

/// The file names every test binary folder holds.
const NAMES: [&str; 8] = [
    "lil.exe",
    "lil-core.exe",
    "lil-mcp.exe",
    "lil-test-app.exe",
    "lil-fake-comfyui.exe",
    "lil-fake-comfy-cli.exe",
    "lil-fake-image.exe",
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

#[test] // spec: QA-042
fn the_variables_are_read_as_the_runner_sets_them() {
    let dir = tempfile::tempdir().unwrap();
    fill(dir.path(), &NAMES);
    let compiled = PathBuf::from(r"C:\nowhere\target\testbins\debug");
    let dir_s = dir.path().as_os_str().to_owned();
    let with = |vars: &[(&str, &std::ffi::OsStr)]| {
        let vars: Vec<(String, std::ffi::OsString)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_owned()))
            .collect();
        locate_from(
            &move |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            &compiled,
        )
    };
    // Nothing set: the compile-time folder, which may be built.
    assert!(matches!(with(&[]), Ok(Located::BuildIfNeeded(b)) if b.dir == compiled));
    // An empty override counts as unset.
    let empty = std::ffi::OsStr::new("");
    assert!(matches!(
        with(&[("LIL_TESTBINS_DIR", empty)]),
        Ok(Located::BuildIfNeeded(_))
    ));
    // In the Sandbox: an empty override is no override, so it fails without building.
    let one = std::ffi::OsStr::new("1");
    assert!(with(&[("LIL_SANDBOX", one)]).is_err());
    assert!(with(&[("LIL_SANDBOX", one), ("LIL_TESTBINS_DIR", empty)]).is_err());
    // Only `1` is the Sandbox.
    assert!(matches!(
        with(&[("LIL_SANDBOX", std::ffi::OsStr::new("true"))]),
        Ok(Located::BuildIfNeeded(_))
    ));
    // The override is used, in the Sandbox or not.
    assert!(matches!(
        with(&[("LIL_SANDBOX", one), ("LIL_TESTBINS_DIR", dir_s.as_os_str())]),
        Ok(Located::Use(b)) if b.dir == dir.path()
    ));
}
