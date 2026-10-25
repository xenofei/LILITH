//! `lil_client::session::Session` against the real `lil-core` (QA-056): the state it reports,
//! reconnecting with its backoff, resubscribing from each topic's last `seq` (or getting a
//! snapshot), in-flight calls across a lost core, switching workspace, app calls, and the
//! GUI-thread guard of the blocking API (APP-024, APP-033–036, ARCH-100, QA-026).
//!
//! Every test runs on its own `TestEnv` channel and drains what it started; `TestEnv`'s drop
//! drains anything left (QA-041). Durations are failure deadlines only, never measurements
//! (QA-031), except in `backoff_doubles_from_250_ms_to_5_s`, which reads tokio's paused clock.
//!
//! Some tests stop the session's runtime on purpose: each runs on a current-thread runtime of
//! its own, so the session's tasks run only inside `block_on`. Between two `block_on` calls the
//! test kills the core and prepares the next one, and the session meets the result only when
//! the test lets it run again.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lil_client::blocking::{self, BlockingClient};
use lil_client::session::{Session, SessionState};
use lil_client::{ClientConfig, ClientError, DrainOutcome, StartPolicy, mark_gui_thread, mint_id};
use lil_platform::process;
use lil_protocol::{
    CallerKind, EventKind, EventParams, HelloResult, Outcome, PROTOCOL, ProtocolVersion, ToolOutput,
};
use lil_testkit::{CoreFixture, RawPipe, TestApp, TestEnv, ensure_built, sha256_file};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// The test build of the core, trusted from the test binaries' folder, on `env`'s channel.
fn config(env: &TestEnv) -> ClientConfig {
    let bins = ensure_built();
    let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).expect("for_env");
    cfg.core_exe = bins.lil_core.clone();
    cfg.server_dirs = vec![bins.dir.clone()];
    cfg.spawn_env = env.child_env();
    cfg
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("arguments must be an object, got {other}"),
    }
}

fn ok(outcome: Outcome<ToolOutput>) -> ToolOutput {
    match outcome {
        Outcome::Ok(output) => output,
        other => panic!("expected an ok outcome, got {other:?}"),
    }
}

/// The first state `pred` accepts, waiting at most the deadline.
async fn wait_state(
    state: &mut watch::Receiver<SessionState>,
    what: &str,
    pred: impl FnMut(&SessionState) -> bool,
) -> SessionState {
    let seen = match tokio::time::timeout(DEADLINE, state.wait_for(pred)).await {
        Ok(seen) => Some(seen.expect("the session ended").clone()),
        Err(_) => None,
    };
    seen.unwrap_or_else(|| panic!("no {what} state within the deadline: {:?}", *state.borrow()))
}

/// The hello of the first `Ready` state whose core instance is not `not`.
async fn wait_ready(state: &mut watch::Receiver<SessionState>, not: Option<&str>) -> HelloResult {
    let ready = wait_state(state, "Ready", |s| match s {
        SessionState::Ready(hello) => not.is_none_or(|old| hello.instance_id != old),
        _ => false,
    })
    .await;
    match ready {
        SessionState::Ready(hello) => *hello,
        other => panic!("expected Ready, got {other:?}"),
    }
}

async fn next_event(events: &mut mpsc::Receiver<EventParams>) -> EventParams {
    tokio::time::timeout(DEADLINE, events.recv())
        .await
        .expect("no event within the deadline")
        .expect("the event stream ended")
}

/// Every state the session reports from now on, in order.
fn record_states(session: &Session) -> Arc<Mutex<Vec<SessionState>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (sink, mut state) = (seen.clone(), session.state());
    tokio::spawn(async move {
        while state.changed().await.is_ok() {
            let now = state.borrow_and_update().clone();
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(now);
        }
    });
    seen
}

/// Kills the core `pid` and waits for it to exit, without running the session's runtime.
fn kill_core(pid: u32) {
    let handle = process::open(pid).expect("open the core");
    handle.kill().expect("kill the core");
    runtime()
        .block_on(async { tokio::time::timeout(DEADLINE, handle.wait()).await })
        .expect("the killed core did not exit")
        .expect("wait for the core");
}

/// Drains the channel's core and checks that it went.
fn stop(cfg: &ClientConfig) {
    assert_eq!(
        blocking::drain(cfg, DEADLINE).expect("drain"),
        DrainOutcome::Stopped
    );
}

/// Everything the channel's cores have logged so far.
fn core_log(env: &TestEnv) -> String {
    let logs = env.env.logs_dir();
    std::fs::read_dir(&logs)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", logs.display()))
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect()
}

/// Review Focus 1: five events on each of two topics, the core killed and a new core started.
/// The state goes `Reconnecting`, then `Ready` on the new core; each topic resumes after its
/// last seq in the new core's numbering, so the new core's own earlier events are replayed and
/// later ones follow live: nothing is lost or doubled.
#[test] // spec: APP-033, APP-034, APP-035, ARCH-100, ARCH-032
fn session_reconnects_and_resubscribes_from_the_last_seq() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let rt = runtime();
    let topics = ["test:a", "test:b"];
    let (session, mut state, mut events, states) = rt.block_on(async {
        let session = Session::start(cfg.clone(), topics.iter().map(|t| t.to_string()).collect());
        let (state, events, states) = (session.state(), session.events(), record_states(&session));
        (session, state, events, states)
    });

    let first = rt.block_on(async {
        let first = wait_ready(&mut state, None).await;
        for n in 1..=5_u64 {
            for topic in ["a", "b"] {
                let emit = json!({"topic": topic, "data": {"n": n}});
                ok(session
                    .call_tool("test.emit", args(emit), None, None)
                    .await
                    .expect("test.emit"));
            }
        }
        for n in 1..=5_u64 {
            for topic in topics {
                let event = next_event(&mut events).await;
                assert_eq!((event.topic.as_str(), event.seq), (topic, n), "{event:?}");
                assert_eq!(event.kind, EventKind::Event);
                assert_eq!(event.data, json!({"n": n}));
            }
        }
        first
    });
    for topic in topics {
        assert_eq!(session.last_seq(topic), Some(5), "{topic}");
    }

    // The session can't run now: the core dies, a new one starts and publishes twice on each
    // topic before the session hears of any of it.
    kill_core(first.core.pid);
    let second_core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
    for n in 6..=7_u64 {
        for topic in ["a", "b"] {
            pipe.tool_ok("test.emit", json!({"topic": topic, "data": {"n": n}}), None);
        }
    }

    rt.block_on(async {
        let second = wait_ready(&mut state, Some(&first.instance_id)).await;
        assert_eq!(second.core.pid, second_core.pid());
        let mut got: Vec<(String, u64, u64)> = Vec::new();
        for _ in 0..4 {
            let event = next_event(&mut events).await;
            assert_eq!(event.kind, EventKind::Event, "{event:?}");
            got.push((
                event.topic.clone(),
                event.seq,
                event.data["n"].as_u64().unwrap(),
            ));
        }
        for topic in ["a", "b"] {
            let emit = json!({"topic": topic, "data": {"n": 8}});
            ok(session
                .call_tool("test.emit", args(emit), None, None)
                .await
                .expect("test.emit on the new core"));
        }
        for _ in 0..2 {
            let event = next_event(&mut events).await;
            assert_eq!(event.kind, EventKind::Event, "{event:?}");
            got.push((
                event.topic.clone(),
                event.seq,
                event.data["n"].as_u64().unwrap(),
            ));
        }
        for topic in topics {
            let of_topic: Vec<(u64, u64)> = got
                .iter()
                .filter(|(t, _, _)| t == topic)
                .map(|(_, seq, n)| (*seq, *n))
                .collect();
            assert_eq!(of_topic, [(1, 6), (2, 7), (3, 8)], "{topic}: {got:?}");
        }
        // A late duplicate would come before this one.
        let emit = json!({"topic": "a", "data": {"n": 9}});
        ok(session
            .call_tool("test.emit", args(emit), None, None)
            .await
            .expect("test.emit on the new core"));
        let last = next_event(&mut events).await;
        assert_eq!((last.topic.as_str(), last.seq), ("test:a", 4), "{last:?}");
        assert_eq!(last.data, json!({"n": 9}));
    });
    assert_eq!(session.last_seq("test:a"), Some(4));
    assert_eq!(session.last_seq("test:b"), Some(3));

    let seen = states
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let reconnecting = seen
        .iter()
        .position(|s| matches!(s, SessionState::Reconnecting { .. }))
        .unwrap_or_else(|| panic!("never Reconnecting: {seen:?}"));
    assert!(
        matches!(&seen[reconnecting..].last(), Some(SessionState::Ready(h)) if h.instance_id != first.instance_id),
        "{seen:?}"
    );
    assert!(
        !seen.iter().any(|s| matches!(
            s,
            SessionState::Unavailable { .. } | SessionState::Incompatible { .. }
        )),
        "{seen:?}"
    );

    rt.block_on(session.close());
    drop(pipe);
    stop(&cfg);
}

/// A cursor older than the history the core still holds (1,024 events at most): the topic
/// starts again from the core's snapshot, then its live events follow.
#[test] // spec: APP-035, ARCH-100, ARCH-032
fn session_gets_a_snapshot_when_the_history_is_gone() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let rt = runtime();
    let (session, mut state, mut events) = rt.block_on(async {
        let session = Session::start(cfg.clone(), vec!["telemetry".to_string()]);
        let (state, events) = (session.state(), session.events());
        (session, state, events)
    });

    let first = rt.block_on(async {
        let first = wait_ready(&mut state, None).await;
        ok(session
            .call_tool("test.hw_tick", args(json!({"samples": 2})), None, None)
            .await
            .expect("test.hw_tick"));
        let mut samples = 0;
        let mut seq = 0;
        while samples < 2 {
            let event = next_event(&mut events).await;
            assert_eq!(event.topic, "telemetry");
            assert_eq!(event.seq, seq + 1, "{event:?}");
            seq = event.seq;
            if event.data["kind"] == "sample" {
                samples += 1;
            }
        }
        assert_eq!(session.last_seq("telemetry"), Some(seq));
        first
    });

    // While the session can't run: a new core whose telemetry history no longer holds its
    // first event.
    kill_core(first.core.pid);
    let second_core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
    pipe.tool_ok(
        "test.hw_tick",
        json!({"samples": 1100}),
        Some(&mint_id("ik")),
    );

    rt.block_on(async {
        let second = wait_ready(&mut state, Some(&first.instance_id)).await;
        assert_eq!(second.core.pid, second_core.pid());
        let snapshot = next_event(&mut events).await;
        assert_eq!(snapshot.topic, "telemetry");
        assert_eq!(snapshot.kind, EventKind::Snapshot, "{snapshot:?}");
        assert!(snapshot.seq >= 1100, "{snapshot:?}");
        assert_eq!(session.last_seq("telemetry"), Some(snapshot.seq));

        ok(session
            .call_tool("test.hw_tick", args(json!({"samples": 1})), None, None)
            .await
            .expect("test.hw_tick on the new core"));
        let live = next_event(&mut events).await;
        assert_eq!(
            (live.kind, live.seq),
            (EventKind::Event, snapshot.seq + 1),
            "{live:?}"
        );
    });

    rt.block_on(session.close());
    drop(pipe);
    stop(&cfg);
}

/// The state of idempotency key `key` in the suite `state.db`, if it has a row.
fn idempotency_state(env: &TestEnv, key: &str) -> Option<String> {
    use rusqlite::{OpenFlags, OptionalExtension as _};
    let db = rusqlite::Connection::open_with_flags(
        env.env.suite_state_db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    db.query_row(
        "SELECT state FROM idempotency WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Review Focus 1: a call waiting in the core when the core dies ends with `Closed`, and the
/// session never sends it again: the new core has no record of its key, so the same key with
/// other arguments runs there as a first call.
#[test] // spec: APP-034, ARCH-100, ARCH-035
fn in_flight_calls_fail_and_are_never_resent() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let rt = runtime();
    let key = mint_id("ik");
    rt.block_on(async {
        let session = Session::start(cfg.clone(), Vec::new());
        let mut state = session.state();
        let first = wait_ready(&mut state, None).await;

        // `test.block` waits on a gate nobody opens.
        {
            let call = session.call_tool(
                "test.block",
                args(json!({"gate": "never"})),
                Some(key.clone()),
                None,
            );
            tokio::pin!(call);
            let started = tokio::time::Instant::now();
            while idempotency_state(&env, &key).as_deref() != Some("running") {
                tokio::select! {
                    result = &mut call => panic!("test.block returned: {result:?}"),
                    () = tokio::time::sleep(Duration::from_millis(20)) => {}
                }
                assert!(started.elapsed() < DEADLINE, "the core never claimed {key}");
            }

            process::open(first.core.pid)
                .expect("open the core")
                .kill()
                .expect("kill the core");
            let result = tokio::time::timeout(DEADLINE, &mut call)
                .await
                .expect("the in-flight call did not end");
            assert!(matches!(result, Err(ClientError::Closed)), "{result:?}");
        }

        // The session started a new core itself.
        let second = wait_ready(&mut state, Some(&first.instance_id)).await;
        assert_ne!(second.core.pid, first.core.pid);
        // The new core released the dead core's claim on the key at its start. Had the session
        // sent `test.block` again, the new core would hold the key for it, and a call with the
        // same key and other arguments would be refused; it runs, so nothing was resent.
        let probe = session
            .call_tool(
                "test.write",
                args(json!({"label": "probe"})),
                Some(key.clone()),
                None,
            )
            .await
            .expect("test.write with the in-flight call's key");
        assert!(matches!(probe, Outcome::Ok(_)), "{probe:?}");
        session.close().await;
    });
    stop(&cfg);
}

/// APP-036, ARCH-014: a client of an older major reports the core incompatible, naming its
/// protocol, and leaves it running. The test sets `cfg.protocol` to 0.9 itself, which is what
/// `LIL_TEST_CLIENT_PROTOCOL=0.9` makes `ClientConfig::for_env` say on a test channel (the unit
/// test `client_protocol_override_is_test_channels_only`): setting the variable in-process is
/// `unsafe` in edition 2024, and the session reads only `cfg.protocol`.
#[test] // spec: APP-036, ARCH-014, ARCH-100
fn incompatible_core_reports_incompatible_and_is_left_running() {
    let env = TestEnv::new();
    let mut cfg = config(&env);
    let mut core = CoreFixture::start(&env);
    cfg.protocol = ProtocolVersion { major: 0, minor: 9 };
    let rt = runtime();
    rt.block_on(async {
        let session = Session::start(cfg.clone(), vec!["test:a".to_string()]);
        let mut state = session.state();
        let seen = wait_state(&mut state, "Incompatible", |s| {
            matches!(s, SessionState::Incompatible { .. })
        })
        .await;
        assert_eq!(
            seen,
            SessionState::Incompatible {
                core_protocol: PROTOCOL.to_string()
            }
        );
        let refused = session
            .call_tool("test.echo", args(json!({"text": "x"})), None, None)
            .await;
        assert!(matches!(refused, Err(ClientError::Closed)), "{refused:?}");
        session.close().await;
    });

    // The same core still serves, and nobody asked it to drain.
    let mut pipe = RawPipe::connect(&env);
    assert_eq!(pipe.server_pid(), Some(core.pid()));
    assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
    drop(pipe);
    assert!(
        !core_log(&env).contains("core/drain"),
        "the session sent core/drain"
    );
    let mut same = config(&env);
    same.start = StartPolicy::NoStart;
    stop(&same);
    assert_eq!(core.wait_exit(DEADLINE), Some(0));
}

/// No core can start: a damaged suite `state.db` (nothing is changed), or no core executable.
/// The session says why.
#[test] // spec: ARCH-100, CORE-004
fn unavailable_when_no_core_can_start() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    let before = sha256_file(&db);

    let mut missing = cfg.clone();
    missing.core_exe = env.root().join("no such bin").join("lil-core.exe");
    let rt = runtime();
    rt.block_on(async {
        for (cfg, says) in [
            (&cfg, "damaged"),
            (&missing, "Cannot start the LILITH core"),
        ] {
            let session = Session::start(cfg.clone(), Vec::new());
            let mut state = session.state();
            let seen = wait_state(&mut state, "Unavailable", |s| {
                matches!(s, SessionState::Unavailable { .. })
            })
            .await;
            let SessionState::Unavailable { reason } = seen else {
                unreachable!()
            };
            assert!(reason.contains(says), "{reason}");
            session.close().await;
        }
    });
    assert_eq!(sha256_file(&db), before, "{} was changed", db.display());
}

/// How many cores of the channel have exited with a start failure: each logs `core exiting`.
fn failed_starts(env: &TestEnv) -> usize {
    if !env.env.logs_dir().exists() {
        return 0;
    }
    core_log(env).matches("core exiting").count()
}

/// After a failed start, the session only looks for a core someone else started: one failing
/// core, not one every 5 s. `retry_now` starts one again; once the person has set the
/// database aside and a core runs, the session finds it by itself.
#[test] // spec: ARCH-100, CORE-004, ARCH-018
fn a_failed_start_is_tried_once_until_retry_now() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    let rt = runtime();
    let (session, mut state) = rt.block_on(async {
        let session = Session::start(cfg.clone(), Vec::new());
        let mut state = session.state();
        wait_state(&mut state, "Unavailable", |s| {
            matches!(s, SessionState::Unavailable { .. })
        })
        .await;
        // The attempts 250, 750, 1750 and 3750 ms after the failure only look for a core.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(failed_starts(&env), 1);
        assert!(
            matches!(*state.borrow(), SessionState::Unavailable { .. }),
            "{:?}",
            *state.borrow()
        );

        session.retry_now();
        let asked = tokio::time::Instant::now();
        while failed_starts(&env) < 2 {
            assert!(asked.elapsed() < DEADLINE, "retry_now started no core");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        (session, state)
    });

    // What `lil doctor` does: the damaged database goes aside, and a core starts.
    std::fs::rename(&db, db.with_extension("db.aside")).unwrap();
    let core = CoreFixture::start(&env);
    rt.block_on(async {
        let ready = wait_ready(&mut state, None).await;
        assert_eq!(ready.core.pid, core.pid());
        session.close().await;
    });
    assert_eq!(failed_starts(&env), 2, "the session started another core");
    stop(&cfg);
}

/// A `retry_now` that comes while a start is under way is that start's retry, not a second
/// start: two in a row (the second one 20 ms later, with the core still starting) give one
/// core start. Only a later `retry_now` starts another.
#[test] // spec: ARCH-100, CORE-004, ARCH-018
fn two_retries_during_a_start_give_one_start() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    runtime().block_on(async {
        let session = Session::start(cfg.clone(), Vec::new());
        let mut state = session.state();
        wait_state(&mut state, "Unavailable", |s| {
            matches!(s, SessionState::Unavailable { .. })
        })
        .await;
        assert_eq!(failed_starts(&env), 1);

        session.retry_now();
        tokio::time::sleep(Duration::from_millis(20)).await;
        session.retry_now();
        let asked = tokio::time::Instant::now();
        while failed_starts(&env) < 2 {
            assert!(asked.elapsed() < DEADLINE, "retry_now started no core");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // A second start would follow the first one's failure at once.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(failed_starts(&env), 2, "the second retry started a core");
        session.close().await;
    });
}

/// P4a-R33: after each failed attempt the session waits 250 ms, doubling up to 5 s.
#[test] // spec: APP-034, ARCH-100
fn backoff_doubles_from_250_ms_to_5_s() {
    let env = TestEnv::new();
    let mut cfg = config(&env);
    cfg.start = StartPolicy::NoStart;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a paused tokio runtime");
    rt.block_on(async {
        let session = Session::start(cfg, Vec::new());
        let mut state = session.state();
        assert_eq!(*state.borrow(), SessionState::Connecting);
        let mut attempts = Vec::new();
        while attempts.len() < 8 {
            tokio::time::timeout(Duration::from_secs(60), state.changed())
                .await
                .expect("no new state")
                .expect("the session ended");
            match &*state.borrow_and_update() {
                SessionState::Reconnecting { attempt } => {
                    attempts.push((*attempt, tokio::time::Instant::now()));
                }
                other => panic!("expected Reconnecting, got {other:?}"),
            }
        }
        let numbers: Vec<u32> = attempts.iter().map(|(n, _)| *n).collect();
        assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 7, 8]);
        let gaps: Vec<u128> = attempts
            .windows(2)
            .map(|w| (w[1].1 - w[0].1).as_millis())
            .collect();
        assert_eq!(gaps, [250, 500, 1000, 2000, 4000, 5000, 5000]);
        session.close().await;
    });
}

/// P4a-R34: `set_workspace` says a new hello naming the workspace and never touches
/// `suite.toml`; a workspace other than the one the core serves is `workspace.unsupported`, and
/// the session stays where it was.
#[test] // spec: ARCH-030, ARCH-090, APP-033
fn set_workspace_rehellos_and_never_changes_the_default() {
    let env = TestEnv::new();
    let served = env.make_workspace("served");
    let other = env.make_workspace("other");
    env.register_workspace(&served, true);
    env.register_workspace(&other, false);
    let suite_toml = env.env.suite_toml();
    let before = std::fs::read(&suite_toml).unwrap();
    let cfg = config(&env);

    let rt = runtime();
    rt.block_on(async {
        let session = Session::start(cfg.clone(), Vec::new());
        let mut state = session.state();
        let first = wait_ready(&mut state, None).await;
        assert!(first.workspace.is_none(), "{first:?}");

        let id = env.workspace_id(&served);
        let hello = session.set_workspace(&id).await.expect("set_workspace");
        assert_eq!(
            hello.workspace.as_ref().map(|w| w.id.as_str()),
            Some(id.as_str())
        );
        assert_eq!(hello.instance_id, first.instance_id);
        assert_eq!(
            *state.borrow(),
            SessionState::Ready(Box::new(hello.clone()))
        );
        // Calls now work in the served workspace.
        let write = json!({"path": "notes/switched.txt", "text": "one", "expect": "new"});
        ok(session
            .call_tool("test.store_write", args(write), None, None)
            .await
            .expect("test.store_write in the served workspace"));
        assert!(served.join(r"notes\switched.txt").is_file());

        let refused = session
            .set_workspace(&env.workspace_id(&other))
            .await
            .expect_err("a workspace the core doesn't serve");
        match &refused {
            ClientError::Refused(error) => assert_eq!(error.code, "workspace.unsupported"),
            other => panic!("expected workspace.unsupported, got {other:?}"),
        }
        assert_eq!(*state.borrow(), SessionState::Ready(Box::new(hello)));
        ok(session
            .call_tool("test.echo", args(json!({"text": "still here"})), None, None)
            .await
            .expect("test.echo after the refusal"));
        session.close().await;
    });
    assert_eq!(
        std::fs::read(&suite_toml).unwrap(),
        before,
        "suite.toml changed"
    );
    stop(&cfg);
}

/// `app_call` reaches the core's `app/*` methods: with the `ui:test` app connected, a `cli`
/// session's destructive call waits for a confirmation, and `app/confirmations.list` (which
/// `cli` may call, APP-040) lists it; a method only a verified app may call is refused as an
/// outcome.
#[test] // spec: APP-033, APP-040, APP-043
fn app_call_reaches_app_methods() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let _core = CoreFixture::start(&env);
    let _app = TestApp::start(&env, None, &[]);
    let rt = runtime();
    rt.block_on(async {
        let session = Session::start(cfg.clone(), Vec::new());
        let mut state = session.state();
        wait_ready(&mut state, None).await;

        let pending = session
            .call_tool("test.destructive", Map::new(), None, None)
            .await
            .expect("test.destructive");
        let Outcome::PendingConfirmation(pending) = pending else {
            panic!("expected a pending confirmation, got {pending:?}");
        };

        let listed = session
            .app_call("app/confirmations.list", json!({"state": "pending"}))
            .await
            .expect("app/confirmations.list");
        let Outcome::Ok(listed) = listed else {
            panic!("app/confirmations.list was refused: {listed:?}");
        };
        let entries = listed["confirmations"].as_array().expect("confirmations");
        let entry = entries
            .iter()
            .find(|c| c["id"] == pending.confirmation_id.as_str())
            .unwrap_or_else(|| panic!("{} is not listed: {listed}", pending.confirmation_id));
        assert_eq!(entry["tool"], "test.destructive", "{entry}");
        assert_eq!(entry["caller_kind"], "cli", "{entry}");

        let refused = session
            .app_call("app/caps.set", json!({}))
            .await
            .expect("app/caps.set");
        match refused {
            Outcome::Error { error } => assert_eq!(error.code, "caller.not_allowed"),
            other => panic!("a cli session set the caps: {other:?}"),
        }
        session.close().await;
    });
}

/// QA-026: once a thread is marked as the GUI thread, every blocking call made on it panics
/// in a debug build before it waits on anything; other threads are unaffected.
#[cfg(debug_assertions)]
#[test] // spec: QA-026, APP-024
fn blocking_calls_on_the_gui_thread_panic_in_debug() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let env = TestEnv::new();
    let mut cfg = config(&env);
    cfg.start = StartPolicy::NoStart;
    let _core = CoreFixture::start(&env);

    let refused: Vec<(&str, bool)> = {
        let (cfg, env) = (cfg.clone(), env.env.clone());
        std::thread::spawn(move || {
            // Made before the mark, to reach the calls that need them.
            let client = BlockingClient::connect(&cfg).expect("connect before the mark");
            let mut events = client
                .subscribe(vec!["test:a".to_string()], Default::default())
                .expect("subscribe before the mark");
            mark_gui_thread();
            let panics =
                |call: &mut dyn FnMut() -> bool| catch_unwind(AssertUnwindSafe(call)).is_err();
            let a = args(json!({"text": "x"}));
            vec![
                (
                    "connect",
                    panics(&mut || BlockingClient::connect(&cfg).is_ok()),
                ),
                (
                    "drain",
                    panics(&mut || blocking::drain(&cfg, DEADLINE).is_ok()),
                ),
                (
                    "call_tool",
                    panics(&mut || client.call_tool("test.echo", a.clone(), None).is_ok()),
                ),
                ("list_tools", panics(&mut || client.list_tools().is_ok())),
                (
                    "confirmations",
                    panics(&mut || {
                        client
                            .confirmations(lil_protocol::app::ListWhich::Pending)
                            .is_ok()
                    }),
                ),
                (
                    "subscribe",
                    panics(&mut || client.subscribe(Vec::new(), Default::default()).is_ok()),
                ),
                (
                    "next_within",
                    panics(&mut || events.next_within(Duration::ZERO).is_ok()),
                ),
                ("next", panics(&mut || events.next().is_some())),
                (
                    "doctor::core_running",
                    panics(&mut || lil_client::doctor::core_running(&env)),
                ),
            ]
        })
        .join()
        .expect("the GUI thread")
    };
    let ran: Vec<&str> = refused
        .iter()
        .filter(|(_, panicked)| !panicked)
        .map(|(call, _)| *call)
        .collect();
    assert!(ran.is_empty(), "ran on the GUI thread: {ran:?}");

    let elsewhere = std::thread::spawn(move || {
        let client = BlockingClient::connect(&cfg).expect("connect off the GUI thread");
        client.list_tools().is_ok()
    })
    .join()
    .expect("another thread panicked");
    assert!(elsewhere, "a thread that is not the GUI thread was refused");
}
