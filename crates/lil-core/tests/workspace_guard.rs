//! QA-040: a core on a test channel refuses a workspace registered in a real `suite.toml`.
//!
//! This test arms the process-global harness guard with a fake "real" workspace, so it is its
//! own test binary: the arming cannot leak into other suites under plain `cargo test`. Only the
//! first arming counts, so it happens before `TestEnv::new` (whose own arming is then a no-op).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use lil_core::workspace::resolve_workspace;
use lil_platform::{Env, RealLocations, arm_test_guard};
use lil_testkit::TestEnv;

/// The canonical spelling of `path` without a `\\?\` prefix, as a real `suite.toml` would
/// register it. (The guard compares paths as text.)
fn canonical(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Writes the workspace manifest. A test fixture: `lil-core`'s own code writes only through its
/// store (ARCH-112).
#[allow(clippy::disallowed_methods)]
fn write_manifest(ws: &Path) {
    let mut file = std::fs::File::create(ws.join("lilith.yaml")).expect("create");
    file.write_all(b"name: guard\n").expect("write");
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn test_core_refuses_real_registered_workspace() {
    // 1. A workspace folder, as if the owner had registered it for real.
    let dir = tempfile::Builder::new()
        .prefix("lil-guard-ws-")
        .tempdir()
        .expect("tempdir");
    let ws = canonical(dir.path());
    write_manifest(&ws);
    // 2. Arm the guard with it among the real workspaces.
    arm_test_guard(RealLocations {
        workspaces: vec![ws.clone()],
        ..RealLocations::from_system().expect("the real known folders")
    });
    // 3. A test channel that registers the same folder as its default workspace.
    let env = TestEnv::new();
    env.register_workspace(&ws, true);
    let id = Env::workspace_hash(&std::fs::canonicalize(&ws).expect("canonicalize"));
    // 4. Resolving it trips the guard.
    let resolved = resolve_workspace(&env.env, &id, "t");
    panic!("the guard did not trip; resolve_workspace returned {resolved:?}");
}
