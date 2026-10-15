//! The real `ww` and `ww-core` binaries for E2E tests (QA-056).
//!
//! They are built into their own target directory, `<workspace>\target\testbins`, so a test
//! core that is still running never locks a binary the main build wants to relink, and the
//! main build's feature unification never rebuilds one under a running test.
//!
//! Under `cargo nextest` (profiles `default` and `spec`) the setup script `build-testbins` in
//! `.config/nextest.toml` has built them once before any test starts. Anywhere else (plain
//! `cargo test`, another profile) [`ensure_built`] runs the same `cargo build` itself, once per
//! process.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Where the test binaries are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bins {
    /// `<workspace>\target\testbins\debug`
    pub dir: PathBuf,
    /// `<dir>\ww.exe`
    pub ww: PathBuf,
    /// `<dir>\ww-core.exe`. It may be missing (before the core binary exists);
    /// `CoreFixture::start` reports that.
    pub ww_core: PathBuf,
}

/// The nextest profiles whose setup script builds the binaries.
const PROFILES_WITH_SETUP_SCRIPT: &[&str] = &["default", "spec"];

/// The workspace root: two levels above this crate's manifest folder.
pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ww-testkit lives at <workspace>/crates/ww-testkit")
        .to_path_buf()
}

/// The test binaries, built if this run's setup script did not build them.
///
/// # Panics
/// If the build fails; the message carries cargo's output.
pub fn ensure_built() -> &'static Bins {
    static BINS: OnceLock<Bins> = OnceLock::new();
    BINS.get_or_init(|| {
        let root = workspace_root();
        let target = root.join("target").join("testbins");
        if !built_by_setup_script() {
            build(&root, &target);
        }
        let dir = target.join("debug");
        Bins {
            ww: dir.join("ww.exe"),
            ww_core: dir.join("ww-core.exe"),
            dir,
        }
    })
}

/// True inside a nextest run whose profile attaches `build-testbins`. Every package that can
/// call [`ensure_built`] depends on `ww-testkit`, so the script's filter
/// (`rdeps(ww-testkit)` and the E2E packages) always covers the calling test.
fn built_by_setup_script() -> bool {
    std::env::var_os("NEXTEST_RUN_ID").is_some()
        && std::env::var("NEXTEST_PROFILE")
            .is_ok_and(|p| PROFILES_WITH_SETUP_SCRIPT.contains(&p.as_str()))
}

/// The setup script's command: `cargo build --locked --offline --bins -p ww-cli -p ww-core
/// --target-dir <target>`, for this workspace's manifest. `CARGO_TARGET_DIR` is removed so it
/// cannot redirect the build.
fn build(root: &Path, target: &Path) {
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--offline",
            "--bins",
            "-p",
            "ww-cli",
            "-p",
            "ww-core",
        ])
        .arg("--target-dir")
        .arg(target)
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| panic!("cannot run cargo to build the test binaries: {e}"));
    assert!(
        output.status.success(),
        "building the test binaries into {} failed ({}):\n{}",
        target.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-056
    fn ensure_built_puts_ww_in_testbins() {
        let bins = ensure_built();
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap();
        let expected = workspace.join("target").join("testbins").join("debug");
        assert_eq!(bins.dir, expected);
        assert!(bins.ww.exists(), "{} is missing", bins.ww.display());
        assert!(bins.ww.starts_with(&expected), "{}", bins.ww.display());
        assert_eq!(bins.ww, expected.join("ww.exe"));
        assert_eq!(bins.ww_core, expected.join("ww-core.exe"));
    }
}
