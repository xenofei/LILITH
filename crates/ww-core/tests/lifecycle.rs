//! The core's lifecycle over its real pipe (ARCH-014, ARCH-020, ARCH-021, ARCH-023, QA-012):
//! draining, `core/drain` and the version direction, the session-end window, the fatal exit,
//! and the order of the last shutdown steps.
//!
//! Wall-clock durations here are failure deadlines and poll pauses only, never measurements
//! (QA-031).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ww_core::lifecycle::{DrainReason, ShutdownPhase, ShutdownStep};
use ww_core::state::StateError;
use ww_core::{CoreConfig, CoreThread, start_in_thread};
use ww_platform::session_end::SessionEnd;
use ww_testkit::{CoreFixture, FakeClock, RawPipe, SeqIds, TestEnv};

/// Failure deadline for anything the test waits on.
const DEADLINE: Duration = Duration::from_secs(30);
/// Pause between polls.
const POLL: Duration = Duration::from_millis(10);

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

/// The `result` of the response with `id`, reading past anything else.
fn response_for(pipe: &mut RawPipe, id: u64) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = pipe
            .read_line(left)
            .unwrap_or_else(|| panic!("no response to request {id} within {DEADLINE:?}"));
        let message: Value = serde_json::from_str(&line).expect("a JSON line");
        if message["id"] == json!(id) {
            return message["result"].clone();
        }
    }
}

/// Waits until `cond` holds; panics after the deadline.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !cond() {
        assert!(Instant::now() < deadline, "{what}: not within {DEADLINE:?}");
        std::thread::sleep(POLL);
    }
}

#[test] // spec: ARCH-021, ARCH-023
fn draining_core_refuses_new_calls_with_core_draining() {
    let env = TestEnv::new();
    let core = core(&env);

    // A call in flight holds the requested drain.
    let mut busy = connect(&env, json!({}));
    busy.send_line(
        &json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "test.block", "arguments": {"gate": "g"}, "idempotency_key": "k1"},
        })
        .to_string(),
    );
    let lifecycle = core.core().lifecycle().clone();
    wait_until("the blocked call to be in flight", || {
        lifecycle.in_flight() == 1
    });

    let mut other = connect(&env, json!({}));
    let drain = other.call("core/drain", json!({}));
    assert_eq!(
        drain["result"],
        json!({"status": "ok", "draining": true}),
        "{drain}"
    );
    assert!(lifecycle.is_draining());

    // New calls are refused; the rest of the protocol still works.
    let refused = other.tool("test.echo", json!({"text": "x"}), None);
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(refused["error"]["code"], "core.draining", "{refused}");
    assert_eq!(refused["error"]["category"], "transient", "{refused}");
    assert_eq!(
        refused["error"]["retry"], "same_idempotency_key",
        "{refused}"
    );
    let write = other.tool("test.write", json!({"label": "Ünïcödé space"}), Some("k2"));
    assert_eq!(write["error"]["code"], "core.draining", "{write}");
    let subscribed = other.call("ww/subscribe", json!({"topics": ["test:a"]}));
    assert_eq!(subscribed["result"]["status"], "ok", "{subscribed}");
    let late = connect(&env, json!({}));
    drop(late);

    // The call in flight finishes, then the core stops.
    core.core().test_gates().open("g");
    let blocked = response_for(&mut busy, 7);
    assert_eq!(blocked["status"], "ok", "{blocked}");
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(report.reason, DrainReason::Requested);
    assert_eq!(report.abandoned_calls, 0);
    assert_eq!(report.exit_code(), 0);
}

#[test] // spec: ARCH-023, ARCH-031
fn newer_client_drains_after_incompatible_hello() {
    let env = TestEnv::new();
    let core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"protocol": "2.0"}));
    assert_eq!(
        hello["result"]["error"]["code"], "protocol.incompatible",
        "{hello}"
    );

    let drain = pipe.call("core/drain", json!({}));
    assert_eq!(
        drain["result"],
        json!({"status": "ok", "draining": true}),
        "{drain}"
    );
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(report.reason, DrainReason::Requested);
}

#[test] // spec: ARCH-014, ARCH-023
fn older_client_cannot_drain_after_incompatible_hello() {
    let env = TestEnv::new();
    let core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"protocol": "0.9"}));
    assert_eq!(
        hello["result"]["error"]["code"], "protocol.incompatible",
        "{hello}"
    );

    let drain = pipe.call("core/drain", json!({}));
    let result = &drain["result"];
    assert_eq!(result["status"], "error", "{drain}");
    assert_eq!(result["error"]["code"], "protocol.incompatible", "{drain}");
    assert_eq!(result["error"]["data"]["core_protocol"], "1.0", "{drain}");
    assert!(!core.core().lifecycle().is_draining());

    // The core still serves.
    let mut fresh = connect(&env, json!({}));
    let echo = fresh.tool("test.echo", json!({"text": "still here"}), None);
    assert_eq!(echo["status"], "ok", "{echo}");
}

#[test] // spec: ARCH-029, ARCH-023
fn drain_before_hello_is_handshake_required() {
    let env = TestEnv::new();
    let core = core(&env);
    let mut pipe = RawPipe::connect(&env);
    let drain = pipe.call("core/drain", json!({}));
    assert_eq!(drain["error"]["code"], -32001, "{drain}");
    assert!(!core.core().lifecycle().is_draining());
    let hello = pipe.hello(json!({}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
}

#[test] // spec: QA-012
fn state_actor_death_stops_core_with_exit_4() {
    let env = TestEnv::new();
    let mut core = CoreFixture::start(&env);
    let mut pipe = connect(&env, json!({}));
    pipe.send_line(
        &json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "test.state_panic", "arguments": {}},
        })
        .to_string(),
    );
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(4),
        "a core whose state actor died must exit with code 4"
    );
}

#[test] // spec: ARCH-021
fn end_session_runs_shutdown() {
    let env = TestEnv::new();
    let core = core(&env);

    // A query alone changes nothing: another application may still cancel the logoff.
    assert!(core.core().simulate_session_end(SessionEnd::QueryEnd));
    let mut pipe = connect(&env, json!({}));
    let echo = pipe.tool("test.echo", json!({"text": "after the query"}), None);
    assert_eq!(echo["status"], "ok", "{echo}");
    assert!(!core.core().lifecycle().is_draining());

    // The end of the session runs the shutdown; the message returns once it is complete.
    assert!(core.core().simulate_session_end(SessionEnd::End));
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(report.reason, DrainReason::SessionEnd);
    assert_eq!(report.exit_code(), 0);
}

/// A step of the last phase: records whether the suite state.db still answered.
struct ProbeState {
    core_state: Arc<ww_core::state::StateRegistry>,
    answered: Arc<std::sync::Mutex<Option<bool>>>,
}

impl ShutdownStep for ProbeState {
    fn phase(&self) -> ShutdownPhase {
        ShutdownPhase::StopEngines
    }
    fn name(&self) -> &'static str {
        "probe state"
    }
    fn run(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let ok = self.core_state.suite().diagnostics().await.is_ok();
            *self.answered.lock().unwrap() = Some(ok);
        })
    }
}

#[test] // spec: ARCH-021, CORE-016
fn drain_closes_state_before_releasing_the_pipe() {
    let env = TestEnv::new();
    let core = core(&env);
    let state = core.core().state().clone();
    let answered = Arc::new(std::sync::Mutex::new(None));
    core.core()
        .lifecycle()
        .add_shutdown_step(Arc::new(ProbeState {
            core_state: state.clone(),
            answered: answered.clone(),
        }));
    let mut pipe = connect(&env, json!({}));
    let drain = pipe.call("core/drain", json!({}));
    assert_eq!(drain["result"]["status"], "ok", "{drain}");

    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    // The steps ran with the database open; then it was closed; then the pipe.
    assert_eq!(*answered.lock().unwrap(), Some(true));
    assert_eq!(report.steps, ["probe state", "state.close", "pipe.close"]);
    assert!(state.suite().is_closed());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        rt.block_on(state.suite().diagnostics()),
        Err(StateError::ActorGone)
    );
    assert!(
        RawPipe::try_connect(&env, Duration::from_millis(200)).is_none(),
        "the pipe is still served after the shutdown"
    );
}

#[test] // spec: ARCH-020, ARCH-014
fn refused_connection_does_not_hold_idle_and_closes_on_drain() {
    let env = TestEnv::new();
    let core = core(&env);
    let lifecycle = core.core().lifecycle().clone();

    // A connection that has not said hello counts.
    let mut waiting = RawPipe::connect(&env);
    wait_until("the connection to count", || lifecycle.connections() == 1);
    // One that said hello counts.
    let mut ready = connect(&env, json!({}));
    wait_until("both connections to count", || lifecycle.connections() == 2);
    // A refused one stops counting as soon as it is refused.
    let mut refused = RawPipe::connect(&env);
    wait_until("three connections to count", || {
        lifecycle.connections() == 3
    });
    let hello = refused.hello(json!({"protocol": "0.9"}));
    assert_eq!(
        hello["result"]["error"]["code"], "protocol.incompatible",
        "{hello}"
    );
    assert_eq!(lifecycle.connections(), 2);

    // A drain (held open by a call in flight) closes the refused connection at once; the
    // others stay until the pipe closes.
    ready.send_line(
        &json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "test.block", "arguments": {"gate": "h"}, "idempotency_key": "k"},
        })
        .to_string(),
    );
    wait_until("the call to be in flight", || lifecycle.in_flight() == 1);
    let drain = waiting.call("core/drain", json!({}));
    assert_eq!(drain["error"]["code"], -32001, "{drain}");
    lifecycle.begin_drain(DrainReason::Requested);
    assert!(
        refused.wait_closed(DEADLINE),
        "the refused connection stays open"
    );
    let still = waiting.hello(json!({}));
    assert_eq!(still["result"]["status"], "ok", "{still}");
    assert_eq!(lifecycle.connections(), 2);

    core.core().test_gates().open("h");
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(report.reason, DrainReason::Requested);
}
