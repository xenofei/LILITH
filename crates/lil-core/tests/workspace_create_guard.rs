//! QA-040: `workspace.create` passes its path through the harness guard before it creates
//! anything, so a core on a test channel never creates a folder inside a workspace registered
//! in a real `suite.toml`.
//!
//! As plan 2a's `workspace_guard.rs`, this test arms the process-global harness guard with a
//! fake "real" workspace, so it is its own test binary, and the arming happens before
//! `TestEnv::new` (whose own arming is then a no-op). The tool is run directly, outside the
//! dispatcher, so the guard's panic reaches the test instead of becoming `core.bug`.

use std::io::Write as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lil_core::events::EventBus;
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::tools::{CallCtx, Registry, Settings, register_workspace_tools};
use lil_core::workspace::WorkspaceHub;
use lil_platform::fs::{Probe, SystemProbe};
use lil_platform::{Clock, RealLocations, arm_test_guard};
use lil_protocol::CallerKind;
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::json;

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
    let mut file = std::fs::File::create(ws.join("worldwright.yaml")).expect("create");
    file.write_all(b"name: guard\n").expect("write");
}

/// The text of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_default()
}

#[test] // spec: QA-040
fn create_on_a_real_registered_workspace_trips_the_guard() {
    // 1. A workspace folder, as if the owner had registered it for real.
    let dir = tempfile::Builder::new()
        .prefix("lil-guard-create-")
        .tempdir()
        .expect("tempdir");
    let real = canonical(dir.path());
    write_manifest(&real);
    // 2. Arm the guard with it among the real workspaces, before any `TestEnv`.
    arm_test_guard(RealLocations {
        workspaces: vec![real.clone()],
        ..RealLocations::from_system().expect("the real known folders")
    });
    // 3. A test channel whose core has the workspace tools.
    let env = TestEnv::new();
    let states = Arc::new(
        StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}")))
            .expect("the suite state.db"),
    );
    let clock: Arc<dyn Clock> = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        events.clone(),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        probe.clone(),
        lil_core::watch::WatchConfig::default(),
    );
    let mut reg = Registry::new();
    register_workspace_tools(&mut reg, hub, env.env.clone(), probe);
    let tool = reg.get("workspace.create").expect("workspace.create");
    let ctx = CallCtx {
        session: Arc::new(Session {
            id: "s_1".into(),
            caller: CallerKind::Cli,
            non_interactive: false,
            workspace: None,
            capabilities: Vec::new(),
            client_pid: 1,
            profile_id: None,
        }),
        trace_id: "t_1".into(),
        settings: Settings::default(),
        events,
        workspace: None,
        project: None,
        record: Arc::new(lil_core::pipeline::RecordSlot::new()),
    };

    // 4. Creating a new subfolder of that workspace trips the guard before the folder exists.
    let sub = real.join("Ünïcödé new");
    let args = json!({"path": sub.to_str().expect("a UTF-8 path")});
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    // The outcome is kept as its code only: the guard's panic is what this test expects.
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        rt.block_on(tool.run(&ctx, args))
            .map(|out| out.structured)
            .map_err(|e| e.code)
    }));
    let payload = match outcome {
        Err(payload) => payload,
        Ok(result) => panic!("the guard did not trip; workspace.create returned {result:?}"),
    };
    let text = panic_text(payload.as_ref());
    assert!(text.contains("real LILITH location"), "{text}");
    assert!(
        !sub.exists(),
        "the subfolder was created before the guard tripped"
    );
    states.close();
}
