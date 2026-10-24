//! The real `lil`, `lil-core` and `lil-mcp` binaries and the test kit's own (`lil-test-app`,
//! `lil-fake-comfyui`, `lil-fake-comfy-cli` and the fake `claude`) for E2E tests (QA-056,
//! QA-066).
//!
//! They are built into their own target directory, `<workspace>\target\testbins`, so a test
//! core that is still running never locks a binary the main build wants to relink, and the
//! main build's feature unification never rebuilds one under a running test.
//!
//! Under `cargo nextest` (profiles `default` and `spec`) the setup script `build-testbins` in
//! `.config/nextest.toml` has built them once before any test starts. Anywhere else (plain
//! `cargo test`, another profile), or when any binary is missing although the script ran,
//! [`ensure_built`] runs the same `cargo build` itself, once per process.
//!
//! Inside Windows Sandbox there is no cargo (QA-042, P4a-R31): `lil dev desktop-tests` bundles
//! `target\testbins\debug` and sets [`ENV_TESTBINS_DIR`] to it. When that variable is set, the
//! binaries are taken from that folder and nothing is ever built; when it is set and a binary is
//! missing, or when `LIL_SANDBOX=1` and it isn't set, the test fails ([`locate`]).

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
    /// `<dir>\lil-mcp.exe`
    pub lil_mcp: PathBuf,
    /// `<dir>\lil-test-app.exe`: the `ui:test` app ([`crate::TestApp`]).
    pub test_app: PathBuf,
    /// `<dir>\lil-fake-comfyui.exe`
    pub fake_comfyui: PathBuf,
    /// `<dir>\lil-fake-comfy-cli.exe`
    pub fake_comfy_cli: PathBuf,
    /// `<dir>\claude.exe`: the fake Claude Code. Never on the real `PATH`; tests put the
    /// folder on a child's `PATH` with [`crate::TestEnv::path_with_testbins`].
    pub claude: PathBuf,
}

impl Bins {
    /// The test binaries in `dir`.
    pub fn in_dir(dir: &Path) -> Bins {
        Bins {
            lil: dir.join("lil.exe"),
            lil_core: dir.join("lil-core.exe"),
            lil_mcp: dir.join("lil-mcp.exe"),
            test_app: dir.join("lil-test-app.exe"),
            fake_comfyui: dir.join("lil-fake-comfyui.exe"),
            fake_comfy_cli: dir.join("lil-fake-comfy-cli.exe"),
            claude: dir.join("claude.exe"),
            dir: dir.to_path_buf(),
        }
    }

    fn all(&self) -> [&PathBuf; 7] {
        [
            &self.lil,
            &self.lil_core,
            &self.lil_mcp,
            &self.test_app,
            &self.fake_comfyui,
            &self.fake_comfy_cli,
            &self.claude,
        ]
    }
}

/// The folder the test binaries are taken from instead of `target\testbins\debug`, without
/// building: the Sandbox runner sets it to its bundled copy (P4a-R31).
pub const ENV_TESTBINS_DIR: &str = "LIL_TESTBINS_DIR";

/// Set to `1` inside Windows Sandbox by the runner (`lil_platform::guard::ENV_SANDBOX`'s name).
const ENV_SANDBOX: &str = "LIL_SANDBOX";

/// Where [`ensure_built`] takes the binaries from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    /// [`ENV_TESTBINS_DIR`]'s folder, which holds every binary: used as it is, never built.
    Use(Bins),
    /// The compile-time `target\testbins\debug`, built when this run's setup script didn't.
    BuildIfNeeded(Bins),
}

/// Where the test binaries come from: `testbins_dir` ([`ENV_TESTBINS_DIR`]) when it is given,
/// else `compiled` (the compile-time `target\testbins\debug`), which may be built, except inside
/// the Sandbox (`sandbox`, `LIL_SANDBOX=1`), where nothing can be built. `Err` holds the test's
/// failure message: "test binaries not found in <dir>; the Sandbox runner bundles
/// target\testbins\debug".
pub fn locate(
    testbins_dir: Option<&Path>,
    sandbox: bool,
    compiled: &Path,
) -> Result<Located, String> {
    let not_found = |dir: &Path| {
        format!(
            r"test binaries not found in {}; the Sandbox runner bundles target\testbins\debug",
            dir.display()
        )
    };
    match testbins_dir {
        Some(dir) => {
            let bins = Bins::in_dir(dir);
            if bins.all().iter().all(|b| b.is_file()) {
                Ok(Located::Use(bins))
            } else {
                Err(not_found(dir))
            }
        }
        None if sandbox => Err(not_found(compiled)),
        None => Ok(Located::BuildIfNeeded(Bins::in_dir(compiled))),
    }
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

/// The test binaries, built if this run's setup script did not build them, or taken from
/// [`ENV_TESTBINS_DIR`] without building ([`locate`]).
///
/// # Panics
/// If the build fails (the message carries cargo's output), or when [`locate`] refuses.
pub fn ensure_built() -> &'static Bins {
    static BINS: OnceLock<Bins> = OnceLock::new();
    BINS.get_or_init(|| {
        let root = workspace_root();
        let target = root.join("target").join("testbins");
        let override_dir = std::env::var_os(ENV_TESTBINS_DIR)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let sandbox = std::env::var(ENV_SANDBOX).is_ok_and(|v| v == "1");
        match locate(override_dir.as_deref(), sandbox, &target.join("debug")) {
            Err(message) => panic!("{message}"),
            Ok(Located::Use(bins)) => bins,
            Ok(Located::BuildIfNeeded(bins)) => {
                if needs_build(
                    built_by_setup_script(),
                    bins.all().iter().all(|b| b.is_file()),
                ) {
                    build(&root, &target);
                }
                bins
            }
        }
    })
}

/// Whether [`ensure_built`] must build: unless the setup script says it built the binaries
/// **and** all exist. A flag alone is not trusted: a script that was skipped or filtered, or
/// a binary deleted since, would otherwise surface as a confusing "cannot start" later.
pub(crate) fn needs_build(built_by_script: bool, all_exist: bool) -> bool {
    !(built_by_script && all_exist)
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
/// -p lil-mcp -p lil-testkit --target-dir <target>`, for this workspace's manifest.
/// `CARGO_TARGET_DIR` is removed so it cannot redirect the build.
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
            "-p",
            "lil-mcp",
            "-p",
            "lil-testkit",
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
        // The setup script's build covers lil-mcp and the test kit's binaries too.
        for bin in bins.all() {
            assert!(bin.is_file(), "{} is missing", bin.display());
            assert_eq!(bin.parent(), Some(expected.as_path()));
        }
    }
}
