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
use std::sync::atomic::{AtomicUsize, Ordering};

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
    let mut file = std::fs::File::create(ws.join("lilith.yaml")).expect("create");
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

/// A test channel's `workspace.create` tool with its call context, run directly (outside the
/// dispatcher), over a hub that owns `failpoints`.
struct Rig {
    states: Arc<StateRegistry>,
    reg: Registry,
    ctx: CallCtx,
}

fn rig(env: &TestEnv, failpoints: Arc<FailPoints>) -> Rig {
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
        failpoints,
        probe.clone(),
        lil_core::watch::WatchConfig::default(),
    );
    let jobs = lil_core::jobs::Jobs::new(
        states.clone(),
        hub.clone(),
        events.clone(),
        FakeClock::new(),
        Arc::new(SeqIds::new()),
        FailPoints::none(),
    );
    let mut reg = Registry::new();
    register_workspace_tools(&mut reg, hub, env.env.clone(), probe);
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
        jobs,
        submitted: Arc::new(lil_core::jobs::SubmittedSlot::new()),
        reservation: None,
        confirmed_via: None,
        idem_key: None,
        call_id: String::new(),
        tool: String::new(),
        arguments: serde_json::Value::Null,
        retry_of: None,
        redispatch: None,
    };
    Rig { states, reg, ctx }
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
    let Rig { states, reg, ctx } = rig(&env, FailPoints::none());
    let tool = reg.get("workspace.create").expect("workspace.create");

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

/// The check after the folder is made (ARCH-094, CORE-110): a workspace registered around the
/// new folder between the two checks (the race the second check is for) makes `workspace.create`
/// refuse, remove the empty folder it made, and register and write nothing.
///
/// The race is injected at the fail point `workspace.after_create`: its hook registers the
/// folder's parent in `suite.toml`, as another core or `lil workspace init` could. No ACL, link
/// or timing is involved. (Under nextest each test is its own process, so the guard's arming in
/// the other test of this file doesn't reach this one.)
#[test] // spec: ARCH-094, QA-040
fn workspace_create_post_create_race_is_refused_cleanly() {
    let env = TestEnv::new();
    let outer = env.make_workspace("outer");
    let inner = outer.join("Ünïcödé inner");
    let suite = env.env.suite_toml();
    let registered_outer = outer.to_str().expect("a UTF-8 path").to_string();

    let failpoints = FailPoints::none();
    let hits = Arc::new(AtomicUsize::new(0));
    {
        let (hits, suite, inner) = (hits.clone(), suite.clone(), inner.clone());
        failpoints.on(
            "workspace.after_create",
            Box::new(move || {
                // The folder exists now; the registration comes after the first check.
                assert!(inner.is_dir(), "the hook runs after the folder was made");
                hits.fetch_add(1, Ordering::SeqCst);
                write_suite(&suite, &registered_outer);
                Ok(())
            }),
        );
    }
    let Rig { states, reg, ctx } = rig(&env, failpoints);
    let tool = reg.get("workspace.create").expect("workspace.create");
    assert!(
        !suite.exists(),
        "nothing is registered before the call: the first check passes"
    );

    let args = json!({"path": inner.to_str().expect("a UTF-8 path")});
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let err = rt
        .block_on(tool.run(&ctx, args))
        .map(|out| out.structured)
        .expect_err("the second check refuses");

    assert_eq!(hits.load(Ordering::SeqCst), 1, "the race was injected once");
    assert_eq!(err.code, "workspace.path_invalid", "{err:?}");
    assert!(
        err.message.contains("inside the registered workspace"),
        "{err:?}"
    );
    assert!(
        !inner.exists(),
        "the refused call left the folder it made behind"
    );
    assert!(outer.is_dir(), "the registered workspace is untouched");
    assert_eq!(
        std::fs::read_to_string(&suite)
            .expect("suite.toml")
            .matches("[[workspaces]]")
            .count(),
        1,
        "only the racing registration is in suite.toml"
    );
    assert!(
        !suite_text(&suite).contains("inner"),
        "the refused folder was not registered"
    );
    states.close();
}

/// Registers `path` in `suite`: what another core's `workspace.create` would have written.
#[allow(clippy::disallowed_methods)]
fn write_suite(suite: &Path, path: &str) {
    if let Some(dir) = suite.parent() {
        std::fs::create_dir_all(dir).expect("the data folder");
    }
    std::fs::write(suite, format!("[[workspaces]]\npath = '{path}'\n")).expect("suite.toml");
}

fn suite_text(suite: &Path) -> String {
    std::fs::read_to_string(suite).expect("suite.toml")
}
