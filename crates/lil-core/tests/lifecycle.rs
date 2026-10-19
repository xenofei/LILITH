//! The core's lifecycle over its real pipe (ARCH-014, ARCH-020, ARCH-021, ARCH-023, QA-012):
//! draining, `core/drain` and the version direction, the session-end window, the fatal exit,
//! and the order of the last shutdown steps.
//!
//! Wall-clock durations here are failure deadlines and poll pauses only, never measurements
//! (QA-031).

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lil_core::config::{ParentWatchFault, ParentWatchStep};
use lil_core::lifecycle::{DrainReason, ShutdownPhase, ShutdownStep};
use lil_core::state::StateError;
use lil_core::{CoreConfig, CoreThread, start, start_in_thread};
use lil_platform::session_end::SessionEndSim;
use lil_testkit::{CoreFixture, FakeClock, RawPipe, SeqIds, TestEnv};
use serde_json::{Value, json};

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

/// Every line of every log file of `env`'s cores, as JSON; panics on a line that is not.
fn log_lines(env: &TestEnv) -> Vec<Value> {
    let logs = env.env.logs_dir();
    let mut lines = Vec::new();
    for entry in
        std::fs::read_dir(&logs).unwrap_or_else(|e| panic!("cannot list {}: {e}", logs.display()))
    {
        let path = entry.expect("a log folder entry").path();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for line in text.lines() {
            lines.push(
                serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("a log line is not JSON ({e}): {line:?}")),
            );
        }
    }
    lines
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
    let subscribed = other.call("lil/subscribe", json!({"topics": ["test:a"]}));
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

/// An in-process core whose test parent is this test process, watched through `fault`.
fn core_watching_parent(env: &TestEnv, fault: ParentWatchFault) -> CoreThread {
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: Some(std::process::id()),
        parent_watch_fault: Some(fault),
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    })
}

#[test] // spec: QA-012, QA-056
fn failed_parent_watch_registration_exits_4() {
    // The parent runs (it is this process), but opening it fails other than with `NotFound`
    // (access denied, error 5), or the wait for it cannot be registered: the core cannot watch
    // it, so it stops as after a fatal error.
    for failing in [ParentWatchStep::Open, ParentWatchStep::Wait] {
        let env = TestEnv::new();
        let core = core_watching_parent(
            &env,
            Arc::new(move |step| {
                if step == failing {
                    Err(io::Error::from_raw_os_error(5))
                } else {
                    Ok(())
                }
            }),
        );
        let report = core.wait_stopped(DEADLINE).expect("the core stops");
        match &report.reason {
            DrainReason::Fatal(why) => assert!(why.contains("test parent"), "{failing:?}: {why}"),
            other => panic!("{failing:?}: not a fatal stop: {other:?}"),
        }
        assert_eq!(report.exit_code(), 4, "{failing:?}");
    }
}

#[test] // spec: QA-056
fn test_parent_not_found_at_open_is_gone_and_exits_0() {
    let env = TestEnv::new();
    let core = core_watching_parent(
        &env,
        Arc::new(|step| match step {
            ParentWatchStep::Open => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "process is not running",
            )),
            ParentWatchStep::Wait => panic!("a parent that is not running is never waited for"),
        }),
    );
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(report.reason, DrainReason::TestParentGone);
    assert_eq!(report.exit_code(), 0);
}

#[test] // spec: QA-012, ARCH-024
fn pipe_instance_creation_failure_is_fatal_exit_4() {
    let env = TestEnv::new();
    // The first instance after the claimed one is created; the next one fails.
    let created = Arc::new(AtomicUsize::new(0));
    let counted = created.clone();
    let core = start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        pipe_instance_fault: Some(Arc::new(move || {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                // ERROR_PIPE_BUSY
                Err(io::Error::from_raw_os_error(231))
            }
        })),
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    });

    // The first connection is served.
    let mut first = connect(&env, json!({}));
    let echo = first.tool("test.echo", json!({"text": "Ünïcödé space"}), None);
    assert_eq!(echo["status"], "ok", "{echo}");
    // The second connection needs the instance that fails: the core stops, as after a fatal
    // error.
    let _second = RawPipe::try_connect(&env, DEADLINE);
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(created.load(Ordering::SeqCst), 2);
    match &report.reason {
        DrainReason::Fatal(why) => assert!(why.contains("pipe"), "{why}"),
        other => panic!("not a fatal stop: {other:?}"),
    }
    assert_eq!(report.exit_code(), 4);
    assert_eq!(report.steps.last().copied(), Some("pipe.close"));
}

#[test] // spec: ARCH-117
fn file_log_has_no_ansi_escapes() {
    let env = TestEnv::new();
    {
        let _core = CoreFixture::start(&env);
        let mut pipe = connect(&env, json!({}));
        // Lines with fields, spans and errors: an ok call, a refused one and a failed one.
        let ok = pipe.tool("test.echo", json!({"text": "Ünïcödé space"}), None);
        assert_eq!(ok["status"], "ok", "{ok}");
        let unknown = pipe.tool("test.nonsense", json!({}), None);
        assert_eq!(unknown["status"], "error", "{unknown}");
        let failed = pipe.tool(
            "test.write",
            json!({"label": "x", "fail": true}),
            Some("k1"),
        );
        assert_eq!(failed["status"], "error", "{failed}");
    }
    let logs = env.env.logs_dir();
    let mut text = String::new();
    for entry in std::fs::read_dir(&logs).expect("the log folder") {
        text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    assert!(text.lines().count() > 3, "too little was logged:\n{text}");
    assert!(
        !text.contains("\x1b["),
        "the file log has ANSI escapes:\n{text}"
    );
    // Every line is JSON.
    let _ = log_lines(&env);
}

#[test] // spec: ARCH-119, QA-012
fn panic_log_line_carries_the_trace_id() {
    let env = TestEnv::new();
    {
        let _core = CoreFixture::start(&env);
        let mut pipe = connect(&env, json!({}));
        let response = pipe.call(
            "tools/call",
            json!({"name": "test.panic", "arguments": {}, "trace_id": "t_panic-log_1"}),
        );
        assert_eq!(response["result"]["status"], "error", "{response}");
        assert_eq!(
            response["result"]["error"]["code"], "core.bug",
            "{response}"
        );
        assert_eq!(
            response["result"]["error"]["trace_id"], "t_panic-log_1",
            "{response}"
        );
    }
    let lines = log_lines(&env);
    let panic_line = lines
        .iter()
        .find(|l| l["fields"]["message"] == "panic" && l["fields"].get("location").is_some())
        .unwrap_or_else(|| panic!("no panic line in the log: {lines:?}"));
    assert_eq!(
        panic_line["fields"]["panic"], "test.panic was called",
        "{panic_line}"
    );
    assert_eq!(
        panic_line["span"]["trace_id"], "t_panic-log_1",
        "{panic_line}"
    );
}

#[test] // spec: ARCH-021
fn end_session_runs_shutdown() {
    let env = TestEnv::new();
    let core = core(&env);

    // A query alone changes nothing: another application may still cancel the logoff.
    assert!(core.core().simulate_session_end(SessionEndSim::QueryEnd));
    let mut pipe = connect(&env, json!({}));
    let echo = pipe.tool("test.echo", json!({"text": "after the query"}), None);
    assert_eq!(echo["status"], "ok", "{echo}");
    assert!(!core.core().lifecycle().is_draining());

    // The end of the session runs the shutdown; the message returns once it is complete.
    assert!(core.core().simulate_session_end(SessionEndSim::End));
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(report.reason, DrainReason::SessionEnd);
    assert_eq!(report.exit_code(), 0);
}

/// A step of the last phase: records whether the suite state.db still answered.
struct ProbeState {
    core_state: Arc<lil_core::state::StateRegistry>,
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
    // The steps ran with the database open; then it was closed; then the pipe. The jobs'
    // `PersistInterrupted` step (plan 3) runs in its phase, then the engines' `StopEngines` step
    // (registered first in that phase), then the probe's.
    assert_eq!(*answered.lock().unwrap(), Some(true));
    assert_eq!(
        report.steps,
        [
            "jobs.persist_interrupted",
            "engines.stop",
            "probe state",
            "state.close",
            "pipe.close"
        ]
    );
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

/// One older client: connects, says hello with protocol 0.9, reads the refusal, hangs up.
async fn refused_cycle(pipe: &str) {
    use lil_platform::pipe::{ConnectError, open_client};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let deadline = Instant::now() + DEADLINE;
    let client = loop {
        match open_client(pipe) {
            Ok(client) => break client,
            Err(ConnectError::Busy) if Instant::now() < deadline => {
                tokio::time::sleep(POLL).await;
            }
            Err(e) => panic!("cannot connect to {pipe}: {e}"),
        }
    };
    let (read, mut write) = tokio::io::split(client);
    let hello = json!({
        "jsonrpc": "2.0", "id": 1, "method": "core/hello",
        "params": lil_testkit::merge_hello(json!({"protocol": "0.9"})),
    });
    write
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .expect("write the hello");
    let mut line = String::new();
    BufReader::new(read)
        .read_line(&mut line)
        .await
        .expect("read the refusal");
    assert!(line.contains("protocol.incompatible"), "{line}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-014, ARCH-020
async fn refused_connections_leave_no_task_behind() {
    let env = TestEnv::new();
    let core = start(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    })
    .await
    .expect("the core starts");
    let pipe = env.env.pipe_name();
    let metrics = tokio::runtime::Handle::current().metrics();
    let baseline = metrics.num_alive_tasks();

    // An older client polling the core (ARCH-014): every connection ends, and so must every
    // task it started.
    for _ in 0..20 {
        refused_cycle(&pipe).await;
    }
    let deadline = Instant::now() + DEADLINE;
    while metrics.num_alive_tasks() > baseline {
        assert!(
            Instant::now() < deadline,
            "{} tasks are alive after 20 refused connections closed; {baseline} before",
            metrics.num_alive_tasks()
        );
        tokio::time::sleep(POLL).await;
    }
    assert!(!core.lifecycle().is_draining());
    core.request_drain();
    assert_eq!(core.wait().await.reason, DrainReason::Requested);
}

/// One `test.block` call on `gate` with its own idempotency key, as a request line.
fn block_line(id: u64, gate: &str) -> String {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": "test.block", "arguments": {"gate": gate}, "idempotency_key": format!("k{id}")},
    })
    .to_string()
}

#[test] // spec: ARCH-025, ARCH-021
fn in_flight_is_capped_per_connection() {
    let env = TestEnv::new();
    let core = core(&env);
    let lifecycle = core.core().lifecycle().clone();
    let cap = lil_core::MAX_IN_FLIGHT_PER_CONN as u64;
    core.core().test_gates().open("free");

    // One write: `cap` calls that wait on their own closed gates, then one on an open gate.
    let mut busy = connect(&env, json!({}));
    let mut lines: Vec<String> = (1..=cap)
        .map(|id| block_line(id, &format!("held-{id}")))
        .collect();
    lines.push(block_line(cap + 1, "free"));
    busy.send_line(&lines.join("\n"));
    wait_until("the capped calls to be in flight", || {
        lifecycle.in_flight() == lil_core::MAX_IN_FLIGHT_PER_CONN
    });

    // The call past the cap has not run: the first run on the open gate is this one, on
    // another connection, which the cap does not touch.
    let mut other = connect(&env, json!({}));
    let first = other.tool("test.block", json!({"gate": "free"}), Some("other"));
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(first["structured"]["executions"], 1, "{first}");

    // One gate opens: that call answers, its permit goes to the waiting call, which runs then.
    core.core().test_gates().open("held-1");
    let answered: Vec<Value> = (0..2)
        .map(|_| {
            let line = busy
                .read_line(DEADLINE)
                .expect("a response within the deadline");
            serde_json::from_str(&line).expect("a JSON line")
        })
        .collect();
    assert_eq!(answered[0]["id"], 1, "{answered:?}");
    assert_eq!(answered[1]["id"], cap + 1, "{answered:?}");
    assert_eq!(
        answered[1]["result"]["structured"]["executions"], 2,
        "{answered:?}"
    );

    // A drain on the other connection is not held up by the full connection; it waits for
    // the calls in flight, which end once their gates open, and the core stops.
    let drain = other.call("core/drain", json!({}));
    assert_eq!(
        drain["result"],
        json!({"status": "ok", "draining": true}),
        "{drain}"
    );
    for id in 2..=cap {
        core.core().test_gates().open(&format!("held-{id}"));
    }
    let report = core.wait_stopped(DEADLINE).expect("the core stops");
    assert_eq!(report.reason, DrainReason::Requested);
    assert_eq!(report.abandoned_calls, 0);
}

#[test] // spec: ARCH-020
fn idle_core_exits_0_end_to_end() {
    let env = TestEnv::new();
    let mut core = CoreFixture::start_with_env(&env, &[(lil_platform::ENV_TEST_IDLE_SECS, "1")]);
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(0),
        "an idle test core with LIL_TEST_IDLE_SECS=1 must exit 0"
    );
}
