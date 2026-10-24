//! `tools/call` over the real pipe of an in-process core (ARCH-031, CORE-016, QA-010): the
//! `app/*` gate, concurrent calls with one idempotency key, and a client killed mid-call.
//!
//! Wall-clock durations here are failure deadlines and retry pauses only, never measurements
//! (QA-031).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_testkit::{FakeClock, RawPipe, SeqIds, TestEnv};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// Failure deadline for anything the test waits on.
const DEADLINE: Duration = Duration::from_secs(30);
/// How long a client retries while its call is `idempotency.in_progress` (ruling I16).
const RETRY_FOR: Duration = Duration::from_secs(10);
/// The pause between those retries.
const RETRY_EVERY: Duration = Duration::from_millis(50);

/// An in-process core with the fake clock and ids (QA-031), on a test channel.
fn core(env: &TestEnv) -> CoreThread {
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    })
}

/// A connection that has said hello with the test defaults merged with `hello`.
fn connect(env: &TestEnv, hello: Value) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let response = pipe.hello(hello);
    assert_eq!(response["result"]["status"], "ok", "hello: {response}");
    pipe
}

/// Sends a `tools/call` without waiting for its response.
fn send_call(pipe: &mut RawPipe, id: u64, name: &str, args: Value, key: &str) {
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": args, "idempotency_key": key},
    });
    pipe.send_line(&request.to_string());
}

/// The `result` of the response with `id`, reading past anything else.
fn response_for(pipe: &mut RawPipe, id: u64) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = pipe
            .read_line(left)
            .unwrap_or_else(|| panic!("no response to request {id} within {DEADLINE:?}"));
        let message: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
        if message["id"] == json!(id) {
            return message["result"].clone();
        }
    }
}

/// Waits until the suite `state.db` holds a `running` row for `key` (read-only, ruling P8).
fn wait_for_running_row(db: &Path, key: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let running = db.exists()
            && Connection::open_with_flags(
                db,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .and_then(|c| {
                c.query_row(
                    "SELECT count(*) FROM idempotency WHERE key = ?1 AND state = 'running'",
                    [key],
                    |r| r.get::<_, i64>(0),
                )
            })
            .is_ok_and(|n| n == 1);
        if running {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no running row for {key:?} within {DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test] // spec: ARCH-031, APP-040
fn app_namespace_refuses_non_ui() {
    let env = TestEnv::new();
    let _core = core(&env);
    for caller in ["cli", "mcp"] {
        let mut pipe = connect(&env, json!({"caller_kind": caller}));
        let response = pipe.call("app/set_context", json!({}));
        let result = &response["result"];
        assert_eq!(result["status"], "error", "{response}");
        assert_eq!(result["error"]["code"], "caller.not_allowed", "{response}");
        assert_eq!(result["error"]["category"], "declined", "{response}");
        assert_eq!(result["error"]["retry"], "never", "{response}");
        // Not a JSON-RPC error (QA-010).
        assert!(response.get("error").is_none(), "{response}");
        // Other unknown methods are still -32601.
        let response = pipe.call("nope/nothing", json!({}));
        assert_eq!(response["error"]["code"], -32601, "{response}");
    }
}

/// An in-process core that trusts the test binaries' folder for apps (`lil-test-app.exe`).
fn app_core(env: &TestEnv) -> CoreThread {
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        trusted: lil_core::identity::TrustedDirs {
            app_dirs: vec![lil_testkit::ensure_built().dir.clone()],
            test_app: true,
            ..base.trusted.clone()
        },
        ..base
    })
}

/// Protocol 1.2's capabilities (Appendix D), as a plan-4a app offers them.
const CAPS_1_2: &[&str] = &[
    "events.v1",
    "tools.v1",
    "jobs.v1",
    "confirmations.v1",
    "caps.v1",
    "queue.v1",
];

/// `app/queue.pause` and `app/queue.resume` (CORE-160, APP-040; Appendix D): a verified app
/// only, and only with `queue.v1` in the shared capabilities; then a `QueueState`.
#[test] // spec: APP-040, APP-039, CORE-160
fn queue_methods_need_a_verified_app_and_the_capability() {
    let env = TestEnv::new();
    let _core = app_core(&env);
    let mut cli = connect(&env, json!({}));
    for m in ["app/queue.pause", "app/queue.resume"] {
        let refused = cli.call(m, json!({}));
        assert_eq!(
            refused["result"]["error"]["code"], "caller.not_allowed",
            "{refused}"
        );
    }
    let mut old = lil_testkit::TestApp::start(&env, None, &[]);
    assert_eq!(old.hello()["caller_kind"], "ui:test");
    let refused = old.call("app/queue.pause", json!({}));
    assert_eq!(
        refused["result"]["error"]["code"], "caller.not_allowed",
        "an app without queue.v1: {refused}"
    );
    let mut app = lil_testkit::TestApp::start_with_capabilities(&env, None, &[], CAPS_1_2);
    let paused = app.call("app/queue.pause", json!({}));
    assert_eq!(paused["result"]["status"], "ok", "{paused}");
    assert_eq!(
        paused["result"]["lane"], "gpu-heavy",
        "the default lane: {paused}"
    );
    assert_eq!(paused["result"]["paused"], true, "{paused}");
    assert_eq!(paused["result"]["reasons"], json!(["manual"]), "{paused}");
    for key in ["held", "restart_held", "withdrawn"] {
        assert_eq!(paused["result"][key], 0, "{paused}");
    }
    // `held_only` is for a resume; the params are strict.
    for (m, params) in [
        ("app/queue.pause", json!({"held_only": true})),
        ("app/queue.pause", json!({"lane": "gpu-light"})),
        ("app/queue.resume", json!({"reason": "fullscreen"})),
    ] {
        let bad = app.call(m, params.clone());
        assert_eq!(bad["error"]["code"], -32602, "{m} {params}: {bad}");
    }
    let held_only = app.call("app/queue.resume", json!({"held_only": true}));
    assert_eq!(
        held_only["result"]["reasons"],
        json!(["manual"]),
        "{held_only}"
    );
    let resumed = app.call("app/queue.resume", json!({}));
    assert_eq!(resumed["result"]["paused"], false, "{resumed}");
    assert_eq!(resumed["result"]["reasons"], json!([]), "{resumed}");
    let cpu = app.call("app/queue.pause", json!({"lane": "cpu"}));
    assert_eq!(cpu["result"]["lane"], "cpu", "{cpu}");
    // The pause shows in `jobs.list`'s lanes.
    let listed = cli.tool("jobs.list", json!({}), None);
    let lanes = listed["structured"]["lanes"].as_array().unwrap();
    assert_eq!(lanes[1]["lane"], "cpu", "{listed}");
    assert_eq!(lanes[1]["paused"], true, "{listed}");
    assert_eq!(lanes[0]["paused"], false, "{listed}");
}

#[test] // spec: CORE-016
fn concurrent_same_key_gets_in_progress() {
    let env = TestEnv::new();
    let core = core(&env);
    let mut a = connect(&env, json!({}));
    let mut b = connect(&env, json!({}));
    let args = json!({"gate": "concurrent"});
    send_call(&mut a, 100, "test.block", args.clone(), "k");
    wait_for_running_row(&env.env.suite_state_db(), "k");

    let busy = b.tool("test.block", args.clone(), Some("k"));
    assert_eq!(busy["status"], "error", "{busy}");
    assert_eq!(busy["error"]["code"], "idempotency.in_progress", "{busy}");
    assert_eq!(busy["error"]["category"], "transient", "{busy}");
    assert_eq!(busy["error"]["retry"], "same_idempotency_key", "{busy}");

    core.core().test_gates().open("concurrent");
    let first = response_for(&mut a, 100);
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(
        first["structured"],
        json!({"gate": "concurrent", "executions": 1})
    );
    // Now the same key replays the stored result.
    let replay = b.tool("test.block", args, Some("k"));
    assert_eq!(replay, first);
}

#[test] // spec: CORE-016, QA-012, ARCH-033
fn client_killed_mid_call_leaves_core_healthy() {
    let env = TestEnv::new();
    let core = core(&env);
    let args = json!({"gate": "g"});

    // 1. Connection A starts a blocked write, then goes away.
    let mut a = connect(&env, json!({}));
    send_call(&mut a, 100, "test.block", args.clone(), "k");
    wait_for_running_row(&env.env.suite_state_db(), "k");
    drop(a);

    // 2. The core still serves others.
    let mut b = connect(&env, json!({}));
    let echo = b.tool("test.echo", json!({"text": "x"}), None);
    assert_eq!(echo["status"], "ok", "{echo}");

    // 3. A's call is still running, and still in flight although its caller has gone.
    let mut c = connect(&env, json!({}));
    let busy = c.tool("test.block", args.clone(), Some("k"));
    assert_eq!(busy["error"]["code"], "idempotency.in_progress", "{busy}");
    let lifecycle = core.core().lifecycle();
    assert_eq!(lifecycle.in_flight(), 1);

    // 4. Open the gate; 5. C's retry gets the stored result of A's call.
    core.core().test_gates().open("g");
    let deadline = Instant::now() + RETRY_FOR;
    let result = loop {
        let outcome = c.tool("test.block", args.clone(), Some("k"));
        if outcome["status"] == "error" && outcome["error"]["code"] == "idempotency.in_progress" {
            assert!(
                Instant::now() < deadline,
                "still in progress after {RETRY_FOR:?}: {outcome}"
            );
            std::thread::sleep(RETRY_EVERY);
            continue;
        }
        break outcome;
    };
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["structured"], json!({"gate": "g", "executions": 1}));

    // 6. Nothing is left behind: with B and C gone, no connection is counted and no call is
    // in flight, so the core can go idle (ARCH-020).
    drop((b, c));
    wait_until_idle(lifecycle);
}

/// Waits until `lifecycle` counts no connection and no call in flight. The deadline is a hang
/// guard only, never a measurement (QA-031).
fn wait_until_idle(lifecycle: &lil_core::Lifecycle) {
    let deadline = Instant::now() + DEADLINE;
    while lifecycle.connections() != 0 || lifecycle.in_flight() != 0 {
        assert!(
            Instant::now() < deadline,
            "still {} connection(s) and {} call(s) in flight after {DEADLINE:?}",
            lifecycle.connections(),
            lifecycle.in_flight()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test] // spec: ARCH-025, CORE-016
fn oversized_result_is_an_error_that_replays_and_connection_stays() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = connect(&env, json!({}));
    let big = json!({"label": "big", "pad": lil_protocol::MAX_LINE_BYTES});

    // The result would not fit in a line: the caller gets core.bug, plainly worded.
    let first = pipe.tool("test.write", big.clone(), Some("k-big"));
    // (Only the error is printed: a failing result is 4 MiB.)
    assert_eq!(first["status"], "error", "error: {}", first["error"]);
    assert_eq!(first["error"]["code"], "core.bug", "{}", first["error"]);
    assert_eq!(first["error"]["category"], "bug", "{}", first["error"]);
    let message = first["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("too large to send"), "{message}");

    // The stored outcome is that error: a retry with the same key replays it, byte for byte,
    // and does not run the tool again.
    let replay = pipe.tool("test.write", big, Some("k-big"));
    assert_eq!(replay, first);
    let after = pipe.tool("test.write", json!({"label": "big"}), Some("k-after"));
    assert_eq!(after["status"], "ok", "{after}");
    assert_eq!(
        after["structured"],
        json!({"label": "big", "executions": 2})
    );

    // The connection stayed open throughout.
    let echo = pipe.tool("test.echo", json!({"text": "still open"}), None);
    assert_eq!(echo["status"], "ok", "{echo}");
}

// ---------------------------------------------------------------------------------------------
// The dispatcher built directly, with tools of the test's own (plan 2b hardening).
// ---------------------------------------------------------------------------------------------

/// A session of `caller` in `workspace` (or none).
fn direct_session(
    workspace: Option<lil_core::workspace::WorkspaceRef>,
) -> Arc<lil_core::session::Session> {
    Arc::new(lil_core::session::Session {
        id: "s_1".into(),
        caller: lil_protocol::CallerKind::Cli,
        non_interactive: false,
        workspace,
        capabilities: Vec::new(),
        client_pid: 1,
        profile_id: None,
    })
}

fn call_params(name: &str, args: Value, key: Option<&str>) -> lil_protocol::ToolCallParams {
    let Value::Object(arguments) = args else {
        panic!("arguments must be an object");
    };
    lil_protocol::ToolCallParams {
        name: name.into(),
        arguments,
        idempotency_key: key.map(str::to_string),
        trace_id: None,
    }
}

/// The error code of an outcome, after checking it is an error.
fn code_of(outcome: &lil_protocol::Outcome<lil_protocol::ToolOutput>) -> String {
    match outcome {
        lil_protocol::Outcome::Error { error } => error.code.clone(),
        other => panic!("expected an error, got {other:?}"),
    }
}

/// The idempotency rows of `path` as `(scope, key, state)`, read-only.
fn idem_rows(path: &Path) -> Vec<(String, String, String)> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    let mut stmt = db
        .prepare("SELECT scope, key, state FROM idempotency ORDER BY scope, key")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A dispatcher over `env` with the test tools and `extra`, its state registry and its hub.
fn direct(
    env: &TestEnv,
    extra: Vec<Arc<dyn lil_core::tools::Tool>>,
) -> (
    lil_core::tools::Dispatcher,
    Arc<lil_core::state::StateRegistry>,
    Arc<lil_core::workspace::WorkspaceHub>,
) {
    use lil_core::tools::{PipelineHooks, TestGates, core_registry};
    let states = Arc::new(
        lil_core::state::StateRegistry::open(
            &env.env,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("the suite state.db"),
    );
    let gates = Arc::new(TestGates::new());
    let clock: Arc<dyn lil_platform::Clock> = FakeClock::new();
    let events = Arc::new(lil_core::events::EventBus::new(clock.clone()));
    let ids: Arc<dyn lil_platform::IdSource> = Arc::new(SeqIds::new());
    let probe: Arc<dyn lil_platform::fs::Probe> = Arc::new(lil_platform::fs::SystemProbe);
    let hub = lil_core::workspace::WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        events.clone(),
        clock.clone(),
        ids.clone(),
        lil_core::store::FailPoints::none(),
        probe.clone(),
        lil_core::watch::WatchConfig::default(),
    );
    let mut reg = core_registry(
        true,
        &gates,
        &states,
        &hub,
        &env.env,
        &probe,
        Arc::default(),
    );
    for tool in extra {
        reg.register(tool);
    }
    let lifecycle = lil_core::Lifecycle::new(clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
    let dispatcher = lil_core::tools::Dispatcher::new(
        reg,
        PipelineHooks::plan2(),
        states.clone(),
        events,
        hub.clone(),
        ids,
        clock,
        lifecycle,
    );
    (dispatcher, states, hub)
}

/// `ws` as a resolved workspace: its canonical path without `\\?\`, and its id.
fn ws_ref(ws: &Path) -> lil_core::workspace::WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    lil_core::workspace::WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: std::path::PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

/// A write tool that panics on its first run and succeeds after.
fn panics_once() -> Arc<dyn lil_core::tools::Tool> {
    use std::sync::atomic::{AtomicU64, Ordering};
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct NoArgs {}
    #[derive(serde::Serialize, schemars::JsonSchema)]
    struct Runs {
        executions: u64,
    }
    let runs = Arc::new(AtomicU64::new(0));
    lil_core::tools::typed_tool(
        "rec.panic_write",
        "A write that panics on its first run.",
        lil_protocol::EffectClass::Write,
        "test",
        move |_ctx, _args: NoArgs| {
            let executions = runs.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                assert!(executions > 1, "rec.panic_write panics on its first run");
                Ok((Runs { executions }, format!("run {executions}")))
            })
        },
    )
}

#[tokio::test] // spec: QA-012, CORE-016, ARCH-060
async fn write_tool_panic_is_core_bug_and_releases_the_key() {
    let env = TestEnv::new();
    let (dispatcher, _states, _hub) = direct(&env, vec![panics_once()]);
    let first = dispatcher
        .call(
            direct_session(None),
            call_params("rec.panic_write", json!({}), Some("k")),
        )
        .await;
    assert_eq!(code_of(&first), "core.bug");
    let lil_protocol::Outcome::Error { error } = &first else {
        unreachable!()
    };
    let detail = error.detail.as_ref().expect("the panic text as detail");
    assert!(detail.untrusted, "{detail:?}");
    assert!(
        detail.text.contains("panics on its first run"),
        "{detail:?}"
    );
    assert!(
        idem_rows(&env.env.suite_state_db()).is_empty(),
        "the key was kept after a panic"
    );
    // The key was released: the retry runs the tool again.
    let retry = dispatcher
        .call(
            direct_session(None),
            call_params("rec.panic_write", json!({}), Some("k")),
        )
        .await;
    let lil_protocol::Outcome::Ok(output) = &retry else {
        panic!("the retry runs: {retry:?}");
    };
    assert_eq!(output.structured, json!({"executions": 2}));
}

#[tokio::test] // spec: CORE-016, QA-012
async fn state_errors_map_to_state_unavailable() {
    // A workspace whose state.db is from a newer version can't be opened: its calls get
    // `state.unavailable`, and the file is untouched.
    let env = TestEnv::new();
    let ws_path = env.make_workspace("newer");
    env.register_workspace(&ws_path, true);
    let ws = ws_ref(&ws_path);
    let db = env
        .env
        .workspace_local_dir(&ws.path)
        .unwrap()
        .join("state.db");
    env.write_state_db_version(&db, 99, true);
    let before = lil_testkit::sha256_file(&db);
    let (dispatcher, states, _hub) = direct(&env, Vec::new());
    for (tool, args, key) in [
        ("test.echo", json!({"text": "x"}), None),
        ("test.write", json!({"label": "w"}), Some("k-ws")),
    ] {
        let out = dispatcher
            .call(
                direct_session(Some(ws.clone())),
                call_params(tool, args, key),
            )
            .await;
        assert_eq!(code_of(&out), "state.unavailable", "{tool}");
    }
    assert_eq!(
        lil_testkit::sha256_file(&db),
        before,
        "the newer state.db changed"
    );

    // A closed registry: every database call fails (ActorGone).
    let states_closing = states.clone();
    tokio::task::spawn_blocking(move || states_closing.close())
        .await
        .unwrap();
    let out = dispatcher
        .call(
            direct_session(None),
            call_params("test.write", json!({"label": "s"}), Some("k")),
        )
        .await;
    assert_eq!(code_of(&out), "state.unavailable");
    let lil_protocol::Outcome::Error { error } = &out else {
        unreachable!()
    };
    assert_eq!(error.category, lil_protocol::ErrorCategory::Transient);
    assert_eq!(error.retry, lil_protocol::RetryPolicy::SameIdempotencyKey);
}

/// A read tool that needs the session's workspace and returns its folder.
fn needs_workspace() -> Arc<dyn lil_core::tools::Tool> {
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct NoArgs {}
    #[derive(serde::Serialize, schemars::JsonSchema)]
    struct Folder {
        path: String,
        read_only: bool,
    }
    lil_core::tools::typed_tool(
        "rec.needs_workspace",
        "A read that needs the session's workspace.",
        lil_protocol::EffectClass::Read,
        "test",
        |ctx, _args: NoArgs| {
            Box::pin(async move {
                let open = ctx.workspace()?;
                let folder = Folder {
                    path: open.ws.path.display().to_string(),
                    read_only: open.access == lil_core::workspace::Access::ReadOnly,
                };
                Ok((folder, "found the workspace".to_string()))
            })
        },
    )
}

#[tokio::test] // spec: ARCH-028, CORE-016
async fn read_only_workspace_refuses_writes_before_idempotency() {
    // Two cores whose local folder for the workspace is one folder (two channels share
    // `<volume>\.lilith-local\<hash>\` for a workspace on another volume, ARCH-097; the
    // harness guard keeps tests out of those folders, so both dispatchers here are on one
    // env). The second core's `state.db` connection is read-only, so an `idem_begin` there
    // would fail with `state.unavailable`: the refusal must come first.
    let a = TestEnv::new();
    let ws_path = a.make_workspace("shared");
    a.register_workspace(&ws_path, true);
    let ws = ws_ref(&ws_path);
    let db = a
        .env
        .workspace_local_dir(&ws.path)
        .unwrap()
        .join("state.db");

    let (first, _states_a, hub_a) = direct(&a, Vec::new());
    let open = hub_a.open(&ws).await.unwrap();
    assert_eq!(open.access, lil_core::workspace::Access::ReadWrite);
    let ok = first
        .call(
            direct_session(Some(ws.clone())),
            call_params("test.write", json!({"label": "a"}), Some("k-a")),
        )
        .await;
    assert!(matches!(ok, lil_protocol::Outcome::Ok(_)), "{ok:?}");
    let rows_before = idem_rows(&db);

    let (second, _states_b, _hub_b) = direct(&a, vec![needs_workspace()]);
    for (tool, args) in [
        ("test.write", json!({"label": "b"})),
        (
            "test.store_write",
            json!({"path": "notes/b.txt", "text": "b", "expect": "new"}),
        ),
        ("test.destructive", json!({})),
    ] {
        let out = second
            .call(
                direct_session(Some(ws.clone())),
                call_params(tool, args, Some("k-b")),
            )
            .await;
        assert_eq!(code_of(&out), "workspace.read_only", "{tool}");
        let lil_protocol::Outcome::Error { error } = &out else {
            unreachable!()
        };
        assert_eq!(error.category, lil_protocol::ErrorCategory::UserFixable);
    }
    assert_eq!(
        idem_rows(&db),
        rows_before,
        "no idempotency row was touched"
    );
    assert!(!ws.path.join("notes").exists());
    // Reads still work there.
    let read = second
        .call(
            direct_session(Some(ws.clone())),
            call_params("rec.needs_workspace", json!({}), None),
        )
        .await;
    let lil_protocol::Outcome::Ok(output) = &read else {
        panic!("a read works on a read-only workspace: {read:?}");
    };
    assert_eq!(output.structured["read_only"], true);
}

#[tokio::test] // spec: ARCH-090, QA-003
async fn default_switch_makes_old_sessions_unsupported() {
    let env = TestEnv::new();
    let (old_path, new_path) = (env.make_workspace("old"), env.make_workspace("new"));
    env.register_workspace(&old_path, true);
    env.register_workspace(&new_path, false);
    let (old, new) = (ws_ref(&old_path), ws_ref(&new_path));
    let (dispatcher, _states, hub) = direct(&env, vec![needs_workspace()]);
    assert_eq!(hub.default_id().as_deref(), Some(old.id.as_str()));
    let before = dispatcher
        .call(
            direct_session(Some(old.clone())),
            call_params("rec.needs_workspace", json!({}), None),
        )
        .await;
    assert!(matches!(before, lil_protocol::Outcome::Ok(_)), "{before:?}");

    // The default switches (as `workspace.set_default` does): the old session is refused,
    // with the way back.
    env.register_workspace(&new_path, true);
    hub.refresh_default();
    assert_eq!(hub.default_id().as_deref(), Some(new.id.as_str()));
    for (tool, args, key) in [
        ("rec.needs_workspace", json!({}), None),
        ("test.write", json!({"label": "x"}), Some("k-old")),
    ] {
        let out = dispatcher
            .call(
                direct_session(Some(old.clone())),
                call_params(tool, args, key),
            )
            .await;
        assert_eq!(code_of(&out), "workspace.unsupported", "{tool}");
        let lil_protocol::Outcome::Error { error } = &out else {
            unreachable!()
        };
        assert!(
            error.message.contains("lil workspace use"),
            "{}",
            error.message
        );
        let fix = error.fix.as_ref().expect("a fix");
        assert_eq!(fix.ui_action.id, "workspace.set_default");
    }
    // A session in the new default works.
    let after = dispatcher
        .call(
            direct_session(Some(new.clone())),
            call_params("rec.needs_workspace", json!({}), None),
        )
        .await;
    assert!(matches!(after, lil_protocol::Outcome::Ok(_)), "{after:?}");
}

#[tokio::test] // spec: QA-005, CORE-016
async fn no_workspace_tool_gets_workspace_required() {
    let env = TestEnv::new();
    let ws_path = env.make_workspace("alpha");
    env.register_workspace(&ws_path, true);
    let (dispatcher, _states, _hub) = direct(&env, vec![needs_workspace()]);
    let out = dispatcher
        .call(
            direct_session(None),
            call_params("rec.needs_workspace", json!({}), None),
        )
        .await;
    assert_eq!(code_of(&out), "workspace.required");
    let lil_protocol::Outcome::Error { error } = &out else {
        unreachable!()
    };
    assert_eq!(error.category, lil_protocol::ErrorCategory::UserFixable);
    assert_eq!(error.retry, lil_protocol::RetryPolicy::AfterResolve);
    // The test-channel store tools need one too.
    let out = dispatcher
        .call(
            direct_session(None),
            call_params(
                "test.store_write",
                json!({"path": "a.txt", "text": "a", "expect": "new"}),
                Some("k"),
            ),
        )
        .await;
    assert_eq!(code_of(&out), "workspace.required");

    let ws = ws_ref(&ws_path);
    let found = dispatcher
        .call(
            direct_session(Some(ws.clone())),
            call_params("rec.needs_workspace", json!({}), None),
        )
        .await;
    let lil_protocol::Outcome::Ok(output) = &found else {
        panic!("{found:?}");
    };
    assert_eq!(output.structured["path"], ws.path.display().to_string());
    assert_eq!(output.structured["read_only"], false);
}

// ---------------------------------------------------------------------------------------------
// `job.retry` and confirmations (CORE-146, ARCH-052, ruling P3-R21).
// ---------------------------------------------------------------------------------------------

/// A `destructive` tool that submits a `test.job` job with its arguments
/// (`{lane: "cpu", fail?}`).
fn destructive_job_tool() -> Arc<dyn lil_core::tools::Tool> {
    #[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct JobArgs {
        lane: String,
        #[serde(default)]
        fail: Option<String>,
    }
    #[derive(serde::Serialize, schemars::JsonSchema)]
    struct Submitted {
        job: lil_core::jobs::JobSummary,
    }
    lil_core::tools::typed_tool(
        "x.destructive_job",
        "A destructive tool that submits a test.job job.",
        lil_protocol::EffectClass::Destructive,
        "test",
        |ctx, args: JobArgs| {
            Box::pin(async move {
                let spec = lil_core::jobs::JobSpec {
                    kind: lil_core::tools::TEST_JOB_KIND,
                    lane: lil_core::jobs::Lane::Cpu,
                    priority: lil_core::jobs::Priority::for_caller(&ctx.session.caller, false),
                    module: "test".into(),
                    args: serde_json::to_value(&args).unwrap(),
                    project: None,
                    asset_id: None,
                    doc_id: None,
                    expected_secs: 10,
                    expected_output_bytes: 0,
                    vram_need_mib: None,
                    engine: None,
                    group: None,
                    external_side_effects: false,
                };
                let job = ctx.jobs.submit(&ctx.job_caller(), spec).await?;
                Ok((Submitted { job }, "submitted".to_string()))
            })
        },
    )
}

/// A dispatcher over `env` with the test tools, `x.destructive_job`, the `test.job` handler
/// and the confirm stage of a [`lil_core::confirm::Confirm`] (no app connected yet).
fn confirming(env: &TestEnv) -> (lil_core::tools::Dispatcher, Arc<lil_core::confirm::Confirm>) {
    use lil_core::confirm::{
        AgentConfirm, Confirm, McpConfirmation, NoNotificationPath, UiPresence,
    };
    use lil_core::tools::{PipelineHooks, TestGates, TestJobHandler, core_registry};
    let states = Arc::new(
        lil_core::state::StateRegistry::open(
            &env.env,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("the suite state.db"),
    );
    let gates = Arc::new(TestGates::new());
    let clock: Arc<dyn lil_platform::Clock> = FakeClock::new();
    let events = Arc::new(lil_core::events::EventBus::new(clock.clone()));
    let ids: Arc<dyn lil_platform::IdSource> = Arc::new(SeqIds::new());
    let probe: Arc<dyn lil_platform::fs::Probe> = Arc::new(lil_platform::fs::SystemProbe);
    let hub = lil_core::workspace::WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        events.clone(),
        clock.clone(),
        ids.clone(),
        lil_core::store::FailPoints::none(),
        probe.clone(),
        lil_core::watch::WatchConfig::default(),
    );
    let mut reg = core_registry(
        true,
        &gates,
        &states,
        &hub,
        &env.env,
        &probe,
        Arc::default(),
    );
    reg.register(destructive_job_tool());
    let confirm = Confirm::new(
        states.clone(),
        events.clone(),
        clock.clone(),
        ids.clone(),
        UiPresence::new(),
        Arc::new(NoNotificationPath),
        Duration::from_secs(30 * 60),
        McpConfirmation::Core,
        lil_core::store::FailPoints::none(),
    );
    let hooks = PipelineHooks {
        confirm: AgentConfirm::new(confirm.clone()),
        ..PipelineHooks::plan2()
    };
    let lifecycle = lil_core::Lifecycle::new(clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
    let dispatcher =
        lil_core::tools::Dispatcher::new(reg, hooks, states, events, hub, ids, clock, lifecycle);
    dispatcher
        .jobs()
        .register(Arc::new(TestJobHandler::new(gates)));
    (dispatcher, confirm)
}

fn ui_session() -> Arc<lil_core::session::Session> {
    Arc::new(lil_core::session::Session {
        id: "s_app".into(),
        caller: lil_protocol::CallerKind::Ui("test".into()),
        non_interactive: false,
        workspace: None,
        capabilities: lil_protocol::CAPABILITIES
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        client_pid: 1,
        profile_id: None,
    })
}

/// The structured result of an `ok` outcome.
fn ok_of(outcome: &lil_protocol::Outcome<lil_protocol::ToolOutput>) -> Value {
    match outcome {
        lil_protocol::Outcome::Ok(out) => out.structured.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

/// The pending confirmation of an outcome.
fn pending_of(
    outcome: &lil_protocol::Outcome<lil_protocol::ToolOutput>,
) -> lil_protocol::PendingConfirmation {
    match outcome {
        lil_protocol::Outcome::PendingConfirmation(p) => p.clone(),
        other => panic!("expected pending_confirmation, got {other:?}"),
    }
}

async fn approve(confirm: &lil_core::confirm::Confirm, id: &str) {
    confirm
        .answer(
            &ui_session(),
            lil_protocol::app::ConfirmationAnswerParams {
                confirmation_id: id.into(),
                answer: lil_protocol::app::Answer::Approve,
                cap: None,
            },
        )
        .await
        .expect("the app approves");
}

/// Waits (failure deadline 30 s) until job `id` ends: its summary.
async fn job_done(d: &lil_core::tools::Dispatcher, id: &str) -> Value {
    let out = d
        .call(
            direct_session(None),
            call_params("job.wait", json!({"job_id": id, "timeout_s": 30}), None),
        )
        .await;
    let waited = ok_of(&out);
    assert_eq!(waited["done"], true, "{waited}");
    waited["job"].clone()
}

async fn job_count(d: &lil_core::tools::Dispatcher) -> usize {
    let out = d
        .call(
            direct_session(None),
            call_params("jobs.list", json!({}), None),
        )
        .await;
    ok_of(&out)["jobs"].as_array().unwrap().len()
}

#[tokio::test] // spec: CORE-146, ARCH-052, ARCH-055, QA-016
async fn job_retry_of_a_confirm_tool_is_pending_and_runs_once() {
    let env = TestEnv::new();
    let (d, confirm) = confirming(&env);
    let made = d
        .call(
            ui_session(),
            call_params(
                "x.destructive_job",
                json!({"lane": "cpu", "fail": "final"}),
                Some("make"),
            ),
        )
        .await;
    let id = ok_of(&made)["job"]["job_id"].as_str().unwrap().to_string();
    assert_eq!(job_done(&d, &id).await["status"], "failed");
    let jobs = job_count(&d).await;
    let retry = |key: &str| call_params("job.retry", json!({"job_id": id}), Some(key));

    // No app: the agent's retry can't be confirmed.
    let out = d.call(direct_session(None), retry("no-app")).await;
    assert_eq!(code_of(&out), "confirm.unavailable");

    // With an app, the retry call itself waits; again with its token, the same.
    confirm.presence().opened();
    let p = pending_of(&d.call(direct_session(None), retry("r")).await);
    assert_eq!(p.retry_token, "r");
    assert_eq!(
        pending_of(&d.call(direct_session(None), retry("r")).await),
        p
    );
    let shown = confirm
        .list(lil_core::confirm::ListState::Pending, 10)
        .await
        .unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].tool, "x.destructive_job", "you see the job's tool");
    assert_eq!(shown[0].arguments, json!({"lane": "cpu", "fail": "final"}));
    assert_eq!(job_count(&d).await, jobs, "nothing ran yet");

    // Approved: the same call submits exactly one new job, with `retry_of`.
    approve(&confirm, &p.confirmation_id).await;
    let out = d.call(direct_session(None), retry("r")).await;
    let retried = ok_of(&out);
    assert_eq!(retried["retry_of"], id.as_str(), "{retried}");
    assert_eq!(retried["job"]["retry_of"], id.as_str(), "{retried}");
    let replay = d.call(direct_session(None), retry("r")).await;
    assert_eq!(replay, out, "the stored outcome");
    assert_eq!(job_count(&d).await, jobs + 1, "exactly one new job");
    let again = retried["job"]["job_id"].as_str().unwrap();
    assert_eq!(job_done(&d, again).await["status"], "failed");
}

#[tokio::test] // spec: CORE-128, CORE-146, ARCH-052
async fn job_retry_of_an_interrupted_job_with_side_effects_needs_a_confirmation() {
    let env = TestEnv::new();
    let (d, confirm) = confirming(&env);
    let mut ids = Vec::new();
    // A job is retried once (Appendix G): the third is yours to retry.
    for (n, external) in [false, true, true].into_iter().enumerate() {
        let made = d
            .call(
                ui_session(),
                call_params(
                    "test.job",
                    json!({"lane": "cpu", "fail": "final", "external_side_effects": external}),
                    Some(&format!("make-{n}")),
                ),
            )
            .await;
        let id = ok_of(&made)["job"]["job_id"].as_str().unwrap().to_string();
        assert_eq!(job_done(&d, &id).await["status"], "failed");
        ids.push(id);
    }
    // Both as an interrupted job is left (CORE-128): failed, substatus `interrupted`.
    let db = Connection::open(env.env.suite_state_db()).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    for id in &ids {
        db.execute(
            "UPDATE jobs SET substatus = 'interrupted' WHERE job_id = ?1",
            [id],
        )
        .unwrap();
    }
    drop(db);
    let retry = |id: &str, key: &str| call_params("job.retry", json!({"job_id": id}), Some(key));
    // A local job's retry is an agent's write: it runs.
    let plain = d.call(direct_session(None), retry(&ids[0], "plain")).await;
    assert_eq!(ok_of(&plain)["retry_of"], ids[0].as_str());
    // One that may have run elsewhere (P3-R21) waits for you, or can't be confirmed.
    let out = d.call(direct_session(None), retry(&ids[1], "no-app")).await;
    assert_eq!(code_of(&out), "confirm.unavailable");
    confirm.presence().opened();
    let p = pending_of(&d.call(direct_session(None), retry(&ids[1], "r")).await);
    approve(&confirm, &p.confirmation_id).await;
    let out = d.call(direct_session(None), retry(&ids[1], "r")).await;
    assert_eq!(ok_of(&out)["retry_of"], ids[1].as_str());
    // Yours never asks.
    let yours = d.call(ui_session(), retry(&ids[2], "yours")).await;
    assert_eq!(ok_of(&yours)["retry_of"], ids[2].as_str());
    // A job retried already is never retried again, by anyone.
    let again = d.call(ui_session(), retry(&ids[1], "again")).await;
    assert_eq!(code_of(&again), "job.not_retryable");
}
