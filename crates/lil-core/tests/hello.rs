//! `core/hello` over the real pipe of an in-process core (ARCH-024, ARCH-027, ARCH-029, ARCH-030,
//! ARCH-045, QA-010): `core/prove`, the session, the frozen version check, strict params, the
//! connection-state rules and the test app's `ui:test` claim.
//!
//! Wall-clock durations here are failure deadlines only, never measurements (QA-031).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::identity::TrustedDirs;
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_platform::Env;
use lil_platform::pipe::ProofEvent;
use lil_testkit::{FakeClock, RawPipe, SeqIds, TestApp, TestEnv, ensure_built, merge_hello};
use serde_json::{Value, json};

/// Failure deadline for anything the test waits on.
const DEADLINE: Duration = Duration::from_secs(30);

/// An in-process core with the fake clock and ids (QA-031).
fn core(env: &TestEnv) -> CoreThread {
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    })
}

fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn read(pipe: &mut RawPipe) -> Value {
    let line = pipe
        .read_line(DEADLINE)
        .expect("no line from the core within the deadline");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
}

#[test] // spec: ARCH-029
fn hello_returns_session_and_shared_capabilities() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({
        "capabilities": ["tools.v1", "future.v9"],
        "non_interactive": true,
    }));
    let result = &response["result"];
    assert_eq!(result["status"], "ok", "{response}");
    let session = result["session_id"].as_str().expect("session_id");
    let instance = result["instance_id"].as_str().expect("instance_id");
    assert!(session.starts_with("s_"), "{response}");
    assert!(instance.starts_with("c_"), "{response}");
    assert_eq!(result["protocol"], "1.3", "{response}");
    assert_eq!(result["build_id"], lil_protocol::BUILD_ID, "{response}");
    assert_eq!(
        result["schema_hash"],
        lil_protocol::schema_hash(),
        "{response}"
    );
    assert_eq!(result["capabilities"], json!(["tools.v1"]), "{response}");
    assert_eq!(result["caller_kind"], "cli", "{response}");
    assert_eq!(result["workspace"], Value::Null, "{response}");

    // A second connection gets its own session from the same core instance.
    let mut other = RawPipe::connect(&env);
    let second =
        other.hello(json!({"protocol": "1.7", "capabilities": lil_protocol::CAPABILITIES}));
    assert_eq!(second["result"]["status"], "ok", "{second}");
    assert_eq!(second["result"]["instance_id"], instance, "{second}");
    assert_ne!(second["result"]["session_id"], session, "{second}");
    // The session runs at the lower minor, 1.3, and shares the core's list: protocol 1.1's
    // five capabilities, then `queue.v1`, `desktop.v1` and `context.v1` (later capabilities
    // follow them).
    assert_eq!(second["result"]["protocol"], "1.3", "{second}");
    let shared = second["result"]["capabilities"].as_array().unwrap();
    assert_eq!(
        shared[..8],
        [
            "events.v1",
            "tools.v1",
            "jobs.v1",
            "confirmations.v1",
            "caps.v1",
            "queue.v1",
            "desktop.v1",
            "context.v1"
        ],
        "{second}"
    );
    // A 1.1 client keeps working: it shares its five, never `queue.v1`.
    let mut older = RawPipe::connect(&env);
    let fourth = older.hello(json!({"protocol": "1.1", "capabilities": [
        "events.v1", "tools.v1", "jobs.v1", "confirmations.v1", "caps.v1"
    ]}));
    assert_eq!(fourth["result"]["protocol"], "1.1", "{fourth}");
    assert_eq!(
        fourth["result"]["capabilities"],
        json!([
            "events.v1",
            "tools.v1",
            "jobs.v1",
            "confirmations.v1",
            "caps.v1"
        ]),
        "{fourth}"
    );

    // A 1.0 client keeps working: its session runs at 1.0 with what it offers.
    let mut old = RawPipe::connect(&env);
    let third = old.hello(json!({"protocol": "1.0", "capabilities": ["events.v1", "tools.v1"]}));
    assert_eq!(third["result"]["status"], "ok", "{third}");
    assert_eq!(third["result"]["protocol"], "1.0", "{third}");
    assert_eq!(
        third["result"]["capabilities"],
        json!(["events.v1", "tools.v1"]),
        "{third}"
    );
}

#[test] // spec: ARCH-030, ARCH-090
fn hello_with_the_default_workspace_reports_it() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé space");
    let other = env.make_workspace("other");
    env.register_workspace(&ws, true);
    env.register_workspace(&other, false);
    let id = |p: &std::path::Path| Env::workspace_hash(&std::fs::canonicalize(p).unwrap());
    let _core = core(&env);

    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({"workspace": id(&ws)}));
    let result = &response["result"];
    assert_eq!(result["status"], "ok", "{response}");
    assert_eq!(result["workspace"]["id"], id(&ws), "{response}");
    let path = result["workspace"]["path"].as_str().expect("a path");
    assert_eq!(
        std::fs::canonicalize(path).unwrap(),
        std::fs::canonicalize(&ws).unwrap()
    );

    // Another registered workspace is refused; the connection may say hello again.
    let mut pipe = RawPipe::connect(&env);
    let refused = pipe.hello(json!({"workspace": id(&other)}));
    assert_eq!(refused["result"]["status"], "error", "{refused}");
    assert_eq!(
        refused["result"]["error"]["code"], "workspace.unsupported",
        "{refused}"
    );
    let again = pipe.hello(json!({}));
    assert_eq!(again["result"]["status"], "ok", "{again}");
}

#[test] // spec: ARCH-029, QA-005
fn major_mismatch_with_unknown_params_is_incompatible_not_invalid_params() {
    let env = TestEnv::new();
    let _core = core(&env);
    for protocol in ["2.0", "0.9"] {
        let mut pipe = RawPipe::connect(&env);
        // Only `protocol` and a field this core does not know: the refusal comes first.
        let response = pipe.call("core/hello", json!({"protocol": protocol, "new_field": 1}));
        assert!(response.get("error").is_none(), "{response}");
        let result = &response["result"];
        assert_eq!(result["status"], "error", "{response}");
        assert_eq!(
            result["error"]["code"], "protocol.incompatible",
            "{response}"
        );
        assert_eq!(
            result["error"]["data"]["core_protocol"], "1.3",
            "{response}"
        );
        assert!(
            result["error"]["trace_id"]
                .as_str()
                .is_some_and(|t| !t.is_empty()),
            "{response}"
        );
    }
}

#[test] // spec: ARCH-029, ARCH-031
fn refused_hello_allows_nothing_else() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"protocol": "2.0"}));
    assert_eq!(hello["result"]["error"]["code"], "protocol.incompatible");
    for method in ["tools/list", "tools/call", "lil/subscribe", "app/anything"] {
        let response = pipe.call(method, json!({}));
        assert_eq!(response["error"]["code"], -32001, "{method}: {response}");
    }
    // A hello after the refusal is a second hello.
    let again = pipe.hello(json!({}));
    assert_eq!(again["error"]["code"], -32600, "{again}");
}

#[test] // spec: ARCH-029, QA-010
fn unknown_hello_param_is_invalid_params() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    for params in [
        merge_hello(json!({"color": "red"})),
        merge_hello(json!({"caller_kind": "robot"})),
        merge_hello(json!({"workspace": null})),
        json!({"protocol": "one"}),
        json!({}),
        json!([]),
    ] {
        let response = pipe.call("core/hello", params.clone());
        assert_eq!(response["error"]["code"], -32602, "{params}: {response}");
    }
    // None of those settled the handshake.
    let list = pipe.call("tools/list", json!({}));
    assert_eq!(list["error"]["code"], -32001, "{list}");
    let ok = pipe.hello(json!({}));
    assert_eq!(ok["result"]["status"], "ok", "{ok}");
}

#[test] // spec: ARCH-029, QA-010
fn second_hello_is_invalid_request() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let first = pipe.hello(json!({}));
    assert_eq!(first["result"]["status"], "ok", "{first}");
    let second = pipe.hello(json!({}));
    assert_eq!(second["error"]["code"], -32600, "{second}");
}

#[test] // spec: ARCH-027, ARCH-029
fn prove_sets_only_a_proofs_event_and_leaves_hello_working() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let proof = ProofEvent::new().unwrap();
    let hex = proof.name().rsplit('-').next().unwrap().to_string();
    // Not a proof's name, or not only a name: refused, with nothing set.
    for params in [
        json!({}),
        json!({"name": 1}),
        json!({"name": r"Local\other"}),
        json!({"name": format!(r"Global\lil-launch-proof-{hex}")}),
        json!({"name": proof.name().to_uppercase()}),
        json!({"name": proof.name(), "workspace": "none"}),
    ] {
        let refused = pipe.call("core/prove", params.clone());
        assert_eq!(refused["error"]["code"], -32602, "{params}: {refused}");
        assert!(!proof.is_set(), "{params}");
    }
    // A proof's shape that names no event.
    let none = format!(r"Local\lil-launch-proof-{}", "0".repeat(32));
    let refused = pipe.call("core/prove", json!({ "name": none }));
    assert_eq!(refused["error"]["code"], -32602, "{refused}");

    let proved = pipe.call("core/prove", json!({"name": proof.name()}));
    assert_eq!(
        proved["result"],
        json!({"status": "ok", "proved": true}),
        "{proved}"
    );
    assert!(proof.is_set());
    // It started nothing: the connection still has to say hello, and can.
    let waiting = pipe.call("tools/list", json!({}));
    assert_eq!(waiting["error"]["code"], -32001, "{waiting}");
    let hello = pipe.hello(json!({}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");

    // After a hello it is refused like a second hello, and sets nothing.
    let late = ProofEvent::new().unwrap();
    let refused = pipe.call("core/prove", json!({"name": late.name()}));
    assert_eq!(refused["error"]["code"], -32600, "{refused}");
    assert!(!late.is_set());
}

#[test] // spec: ARCH-027, ARCH-023
fn another_majors_client_is_answered_prove_and_then_refused_as_ever() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let proof = ProofEvent::new().unwrap();
    let proved = pipe.call("core/prove", json!({"name": proof.name()}));
    assert_eq!(proved["result"]["status"], "ok", "{proved}");
    assert!(proof.is_set());
    let hello = pipe.hello(json!({"protocol": "9.0"}));
    assert_eq!(
        hello["result"]["error"]["code"], "protocol.incompatible",
        "{hello}"
    );
    let late = ProofEvent::new().unwrap();
    let refused = pipe.call("core/prove", json!({"name": late.name()}));
    assert_eq!(refused["error"]["code"], -32600, "{refused}");
    assert!(!late.is_set());
}

#[test] // spec: ARCH-027, ARCH-029
fn prove_is_answered_before_the_state_db_opens_and_a_hello_waits_for_the_core_to_serve() {
    /// What a client saw while the core stood right before opening its `state.db`.
    struct Seen {
        pipe: RawPipe,
        proved: Value,
        event_set: bool,
        state_db_there: bool,
        hello_answered: Option<String>,
    }
    let env = Arc::new(TestEnv::new());
    let seen: Arc<Mutex<Option<Seen>>> = Arc::default();
    let before_state_open = {
        let (env, seen) = (env.clone(), seen.clone());
        Arc::new(move || {
            let env = env.clone();
            // RawPipe has a runtime of its own, and this runs inside the core's.
            let client = std::thread::spawn(move || {
                let mut pipe = RawPipe::connect(&env);
                let proof = ProofEvent::new().unwrap();
                let proved = pipe.call("core/prove", json!({"name": proof.name()}));
                pipe.send_line(&request(7, "core/hello", merge_hello(json!({}))));
                Seen {
                    proved,
                    event_set: proof.is_set(),
                    state_db_there: env.env.suite_state_db().exists(),
                    hello_answered: pipe.read_line(Duration::from_millis(300)),
                    pipe,
                }
            });
            *seen.lock().unwrap() = Some(client.join().expect("the client's checks"));
        })
    };
    let _core = start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        before_state_open: Some(before_state_open),
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    });
    let mut seen = seen.lock().unwrap().take().expect("the hook ran");
    assert_eq!(seen.proved["result"]["proved"], true, "{}", seen.proved);
    assert!(seen.event_set);
    assert!(!seen.state_db_there, "the state.db was open already");
    assert_eq!(seen.hello_answered, None, "a hello waits for the router");
    // Now the core serves, and the hello that waited is answered.
    let hello = read(&mut seen.pipe);
    assert_eq!(hello["id"], 7, "{hello}");
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
}

#[test] // spec: ARCH-029
fn requests_before_hello_need_the_handshake() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    for method in ["tools/list", "core/drain", "nope"] {
        let response = pipe.call(method, json!({}));
        assert_eq!(response["error"]["code"], -32001, "{method}: {response}");
    }
}

#[test] // spec: ARCH-029, ARCH-025
fn pipelined_request_after_hello_sees_the_session() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let hello = request(1, "core/hello", merge_hello(json!({})));
    let list = request(2, "tools/list", json!({}));
    pipe.send_line(&format!("{hello}\n{list}"));
    let first = read(&mut pipe);
    assert_eq!(first["id"], 1, "{first}");
    assert_eq!(first["result"]["status"], "ok", "{first}");
    let second = read(&mut pipe);
    assert_eq!(second["id"], 2, "{second}");
    assert_ne!(second["error"]["code"], -32001, "{second}");
}

#[test] // spec: QA-010
fn tool_call_without_arguments_is_invalid_params() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
    let response = pipe.call("tools/call", json!({"name": "test.echo"}));
    assert_eq!(response["error"]["code"], -32602, "{response}");
}

#[test] // spec: ARCH-024, ARCH-029
fn hello_result_reports_pid_channel_and_pipe() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({}));
    let core = &response["result"]["core"];
    assert_eq!(core["pid"], std::process::id(), "{response}");
    assert_eq!(core["channel"], env.env.channel().to_string(), "{response}");
    assert_eq!(core["pipe"], env.env.pipe_name(), "{response}");
}

#[test] // spec: ARCH-045, ARCH-048, ARCH-050
fn unverified_claims_resolve_to_cli_and_tokens_are_refused() {
    let env = TestEnv::new();
    let _core = core(&env);
    // This test binary is neither an app nor the MCP server.
    for declared in ["ui:image", "mcp", "assistant", "cli"] {
        let mut pipe = RawPipe::connect(&env);
        let response = pipe.hello(json!({"caller_kind": declared}));
        assert_eq!(response["result"]["caller_kind"], "cli", "{response}");
    }
    let mut pipe = RawPipe::connect(&env);
    let forged = pipe.hello(json!({"caller_kind": "assistant", "engine_token": "forged"}));
    assert_eq!(
        forged["result"]["error"]["code"], "caller.engine_token_invalid",
        "{forged}"
    );
    assert!(!forged.to_string().contains("forged\""), "{forged}");
    // That connection is closed (ARCH-050); a token with another kind is a params error.
    let mut pipe = RawPipe::connect(&env);
    let wrong_kind = pipe.hello(json!({"caller_kind": "cli", "engine_token": "x"}));
    assert_eq!(
        wrong_kind["result"]["error"]["code"], "params.invalid",
        "{wrong_kind}"
    );
}

#[test] // spec: ARCH-048, ARCH-050
fn engine_token_invalid_is_terminal_for_the_connection() {
    let env = TestEnv::new();
    let core = core(&env);
    let lifecycle = core.core().lifecycle().clone();
    let mut pipe = RawPipe::connect(&env);
    // A request pipelined right behind the hello is never served.
    pipe.send_line(&format!(
        "{}
{}",
        request(
            1,
            "core/hello",
            merge_hello(json!({"caller_kind": "assistant", "engine_token": "forged"}))
        ),
        request(2, "core/hello", merge_hello(json!({})))
    ));
    let refusal = read(&mut pipe);
    assert_eq!(refusal["id"], 1, "{refusal}");
    assert_eq!(
        refusal["result"]["error"]["code"], "caller.engine_token_invalid",
        "{refusal}"
    );
    // The refusal is written, then the connection is closed; nothing else is answered.
    let next = pipe.read_line(DEADLINE);
    assert_eq!(next, None, "a request after the refusal was answered");
    assert!(
        pipe.wait_closed(DEADLINE),
        "the refused connection stays open"
    );
    let deadline = std::time::Instant::now() + DEADLINE;
    while lifecycle.connections() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the refused connection still counts"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The core still serves.
    let mut fresh = RawPipe::connect(&env);
    let ok = fresh.hello(json!({}));
    assert_eq!(ok["result"]["status"], "ok", "{ok}");
}

/// An in-process core that trusts the test binaries' folder for apps (`lil-test-app.exe` lives
/// there), accepting the test app or not.
fn core_trusting_testbins(env: &TestEnv, test_app: bool) -> CoreThread {
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        trusted: TrustedDirs {
            app_dirs: vec![ensure_built().dir.clone()],
            test_app,
            ..base.trusted.clone()
        },
        ..base
    })
}

#[test] // spec: ARCH-045, ARCH-046, QA-066
fn test_app_resolves_to_ui_test_on_test_channels_only() {
    // A test channel's core trusts the test app (`TrustedDirs::for_env`).
    let env = TestEnv::new();
    assert!(
        CoreConfig::for_env(env.env.clone())
            .unwrap()
            .trusted
            .test_app
    );
    let core = core_trusting_testbins(&env, true);
    let app = TestApp::start(&env, None, &[]);
    assert_eq!(app.hello()["caller_kind"], "ui:test", "{}", app.hello());
    drop(app);
    drop(core);

    // A core that doesn't (a dev or stable channel's) resolves the same claim to `cli`.
    let env = TestEnv::new();
    let _core = core_trusting_testbins(&env, false);
    let app = TestApp::start(&env, None, &[]);
    assert_eq!(app.hello()["caller_kind"], "cli", "{}", app.hello());
}

/// A monthly cap of 1 USD: a `test.destructive` call costing 5 asks about the cap.
const MONTHLY_1: &str = "[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n";

/// The state of confirmation `id` in `app`'s `app/confirmations.list` history, if it is there.
fn history_state(app: &mut TestApp, id: &str) -> Option<Value> {
    let history = app.call("app/confirmations.list", json!({"state": "history"}));
    history["result"]["confirmations"]
        .as_array()
        .and_then(|list| list.iter().find(|c| c["id"] == id))
        .map(|c| c["state"].clone())
}

/// Waits until `app` lists confirmation `id` as cancelled.
fn wait_cancelled(app: &mut TestApp, id: &str, why: &str) {
    let deadline = std::time::Instant::now() + DEADLINE;
    while history_state(app, id) != Some(json!("cancelled")) {
        assert!(std::time::Instant::now() < deadline, "{why}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An app's own cap prompt outlives a reconnect of the app (APP-029; Appendix G
/// "Connections"): the retry its new connection makes with the call's key finds the same
/// confirmation still pending, and the prompt is cancelled only once the app's process exits.
#[test] // spec: APP-029, APP-054, CORE-091
fn an_apps_cap_prompt_outlives_its_reconnect_and_ends_with_its_process() {
    let env = TestEnv::new();
    env.suite_toml_set(MONTHLY_1);
    let _core = core_trusting_testbins(&env, true);
    let mut app = TestApp::start(&env, None, &[]);
    let (key, args) = ("ik_00000000000000a1", json!({"cost": "5"}));
    let asked = app.tool("test.destructive", args.clone(), Some(key));
    assert_eq!(asked["status"], "pending_confirmation", "{asked}");
    assert_eq!(asked["covers"], json!(["cap"]), "{asked}");
    let id = asked["confirmation_id"].as_str().unwrap().to_string();

    let pid = app.pid();
    let hello = app.reconnect("none");
    assert_eq!(hello["caller_kind"], "ui:test", "{hello}");
    assert_eq!(app.pid(), pid);
    let again = app.tool("test.destructive", args, Some(key));
    assert_eq!(
        again["status"], "pending_confirmation",
        "the reconnect ended the prompt: {again}"
    );
    assert_eq!(again["confirmation_id"], id.as_str(), "{again}");

    drop(app);
    let mut other = TestApp::start(&env, None, &[]);
    wait_cancelled(&mut other, &id, "the prompt outlived its app");
}

/// A switch ends the app's cap prompts of the workspace it left at once, while the app runs:
/// its next connection names another workspace.
#[test] // spec: APP-029, ARCH-030
fn a_switch_cancels_the_cap_prompts_of_the_workspace_left() {
    let env = TestEnv::new();
    env.suite_toml_set(MONTHLY_1);
    let served = env.make_workspace("served");
    env.register_workspace(&served, true);
    let _core = core_trusting_testbins(&env, true);
    let mut app = TestApp::start(&env, None, &[]);
    let asked = app.tool(
        "test.destructive",
        json!({"cost": "5"}),
        Some("ik_00000000000000a2"),
    );
    assert_eq!(asked["status"], "pending_confirmation", "{asked}");
    let id = asked["confirmation_id"].as_str().unwrap().to_string();

    let id_served = env.workspace_id(&served);
    let hello = app.reconnect(&id_served);
    assert_eq!(hello["workspace"]["id"], id_served.as_str(), "{hello}");
    wait_cancelled(&mut app, &id, "the switch left the prompt pending");
}
