//! The real `lil` and `lil-core` binaries for E2E tests (QA-056).
//!
//! They are built into their own target directory, `<workspace>\target\testbins`, so a test
//! core that is still running never locks a binary the main build wants to relink, and the
//! main build's feature unification never rebuilds one under a running test.
//!
//! Under `cargo nextest` (profiles `default` and `spec`) the setup script `build-testbins` in
//! `.config/nextest.toml` has built them once before any test starts. Anywhere else (plain
//! `cargo test`, another profile), or when either binary is missing although the script ran,
//! [`ensure_built`] runs the same `cargo build` itself, once per process.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Where the test binaries are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bins {
    /// `<workspace>\target\testbins\debug`
    pub dir: PathBuf,
    /// `<dir>\lil.exe`
    pub lil: PathBuf,
    /// `<dir>\lil-core.exe`. It may be missing (before the core binary exists);
    /// `CoreFixture::start` reports that.
    pub lil_core: PathBuf,
}

/// The nextest profiles whose setup script builds the binaries.
const PROFILES_WITH_SETUP_SCRIPT: &[&str] = &["default", "spec"];

/// The workspace root: two levels above this crate's manifest folder.
pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("lil-testkit lives at <workspace>/crates/lil-testkit")
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
        let dir = target.join("debug");
        let bins = Bins {
            lil: dir.join("lil.exe"),
            lil_core: dir.join("lil-core.exe"),
            dir,
        };
        if needs_build(
            built_by_setup_script(),
            bins.lil.is_file() && bins.lil_core.is_file(),
        ) {
            build(&root, &target);
        }
        bins
    })
}

/// Whether [`ensure_built`] must build: unless the setup script says it built the binaries
/// **and** both exist. A flag alone is not trusted: a script that was skipped or filtered, or
/// a binary deleted since, would otherwise surface as a confusing "cannot start" later.
pub(crate) fn needs_build(built_by_script: bool, both_exist: bool) -> bool {
    !(built_by_script && both_exist)
}

/// True inside a nextest run whose profile attaches `build-testbins`. Every package that can
/// call [`ensure_built`] depends on `lil-testkit`, so the script's filter
/// (`rdeps(lil-testkit)` and the E2E packages) always covers the calling test.
fn built_by_setup_script() -> bool {
    std::env::var_os("NEXTEST_RUN_ID").is_some()
        && std::env::var("NEXTEST_PROFILE")
            .is_ok_and(|p| PROFILES_WITH_SETUP_SCRIPT.contains(&p.as_str()))
}

/// The setup script's command: `cargo build --locked --offline --bins -p lil-cli -p lil-core
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
            "lil-cli",
            "-p",
            "lil-core",
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
    fn ensure_built_rebuilds_when_flagged_but_missing() {
        assert!(!needs_build(true, true));
        assert!(needs_build(true, false), "flagged but missing");
        assert!(needs_build(false, true));
        assert!(needs_build(false, false));
    }

    #[test] // spec: QA-056
    fn ensure_built_puts_lil_in_testbins() {
        let bins = ensure_built();
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap();
        let expected = workspace.join("target").join("testbins").join("debug");
        assert_eq!(bins.dir, expected);
        assert!(bins.lil.exists(), "{} is missing", bins.lil.display());
        assert!(bins.lil.starts_with(&expected), "{}", bins.lil.display());
        assert_eq!(bins.lil, expected.join("lil.exe"));
        assert_eq!(bins.lil_core, expected.join("lil-core.exe"));
    }
}
