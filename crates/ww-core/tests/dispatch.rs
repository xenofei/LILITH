//! `tools/call` over the real pipe of an in-process core (ARCH-031, CORE-016, QA-010): the
//! `app/*` gate, concurrent calls with one idempotency key, and a client killed mid-call.
//!
//! Wall-clock durations here are failure deadlines and retry pauses only, never measurements
//! (QA-031).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use ww_core::{CoreConfig, CoreThread, start_in_thread};
use ww_testkit::{FakeClock, RawPipe, SeqIds, TestEnv};

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
fn wait_until_idle(lifecycle: &ww_core::Lifecycle) {
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
    let big = json!({"label": "big", "pad": ww_protocol::MAX_LINE_BYTES});

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
