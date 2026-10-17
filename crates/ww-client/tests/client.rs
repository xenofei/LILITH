//! `ww-client` against the real `ww-core` (QA-056): finding or starting the core, verifying
//! it, the handshake and its version direction, calls and events.
//!
//! Every test runs on its own `TestEnv` channel. Cores are started by the client under test
//! (with `TestEnv::child_env()`) or by a `CoreFixture`, and each test drains what it started;
//! `TestEnv`'s drop drains anything left (QA-041). Durations are failure deadlines only, never
//! measurements (QA-031).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use serde_json::{Map, Value, json};
use ww_client::blocking::{self, BlockingClient};
use ww_client::{
    Client, ClientConfig, ClientError, DrainOutcome, StartFailure, StartPolicy, drain_and_wait,
};
use ww_platform::process::{self, ProcessInspector, SystemInspector};
use ww_protocol::{CallerKind, EventKind, Outcome, ProtocolVersion, ToolOutput};
use ww_testkit::{CoreFixture, RawPipe, TestEnv, ensure_built, sha256_file};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// The test build of the core, trusted from the test binaries' folder, on `env`'s channel.
fn config(env: &TestEnv) -> ClientConfig {
    let bins = ensure_built();
    let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).expect("for_env");
    cfg.core_exe = bins.ww_core.clone();
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

/// `Client::connect` on a runtime of the test's own, within the failure deadline.
// `ClientError` carries the core's `ErrorBody` by value (see the crate docs).
#[allow(clippy::result_large_err)]
fn connect_async(cfg: &ClientConfig) -> Result<Client, ClientError> {
    runtime().block_on(async {
        tokio::time::timeout(DEADLINE, Client::connect(cfg))
            .await
            .expect("Client::connect did not finish within the deadline")
    })
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
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

/// Drains the channel's core and checks that it went.
fn stop(cfg: &ClientConfig) {
    assert_eq!(
        blocking::drain(cfg, DEADLINE).expect("drain"),
        DrainOutcome::Stopped
    );
}

/// The exit code of process `pid` once it has exited, within the deadline.
fn wait_exit(handle: &process::ProcessHandle) -> Option<u32> {
    runtime()
        .block_on(async { tokio::time::timeout(DEADLINE, handle.wait()).await })
        .ok()
        .and_then(Result::ok)
}

/// Everything the channel's cores have logged so far.
fn core_log(env: &TestEnv) -> String {
    let logs = env.env.logs_dir();
    std::fs::read_dir(&logs)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", logs.display()))
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect()
}

#[test] // spec: ARCH-004, ARCH-018, ARCH-019
fn starts_core_when_missing_and_connects() {
    let env = TestEnv::new();
    let cfg = config(&env);
    assert!(
        RawPipe::try_connect(&env, Duration::ZERO).is_none(),
        "a core already serves the fresh channel"
    );

    let client = BlockingClient::connect(&cfg).unwrap_or_else(|e| panic!("{e}"));
    let hello = client.hello().clone();
    assert_eq!(hello.core.channel, env.env.channel().to_string());
    assert_eq!(hello.core.pipe, env.env.pipe_name());
    assert_eq!(hello.caller_kind, "cli");
    assert!(hello.workspace.is_none());

    // An on-demand process this client started, from the configured executable: not a
    // service (ARCH-004), spawned detached by this process (ARCH-019).
    let info = SystemInspector
        .inspect(hello.core.pid)
        .expect("inspect the core");
    assert_eq!(canonical(&info.image), canonical(&cfg.core_exe));
    assert_eq!(info.parent_pid, Some(std::process::id()));

    let echoed = ok(client
        .call_tool("test.echo", args(json!({"text": "Ünïcödé space"})), None)
        .expect("test.echo"));
    assert_eq!(echoed.structured, json!({"text": "Ünïcödé space"}));

    let handle = process::open(hello.core.pid).expect("open the core");
    drop(client);
    stop(&cfg);
    assert_eq!(wait_exit(&handle), Some(0));
}

#[test] // spec: ARCH-018
fn no_start_reports_not_running_without_spawning() {
    let env = TestEnv::new();
    let mut cfg = config(&env);
    cfg.start = StartPolicy::NoStart;

    let err = BlockingClient::connect(&cfg).expect_err("connected without a core");
    assert!(matches!(err, ClientError::NotRunning), "{err:?}");
    assert!(err.to_string().contains("not running"), "{err}");

    // Nothing was started: no pipe appears, and no core created its log folder.
    assert!(
        RawPipe::try_connect(&env, Duration::from_secs(2)).is_none(),
        "a core was started"
    );
    assert!(
        !env.env.logs_dir().exists(),
        "{} exists",
        env.env.logs_dir().display()
    );
    assert_eq!(
        blocking::drain(&cfg, DEADLINE).expect("drain"),
        DrainOutcome::NotRunning
    );
}

#[test] // spec: ARCH-004, ARCH-018
fn racing_starts_yield_one_core() {
    const CLIENTS: usize = 8;
    let env = TestEnv::new();
    let cfg = config(&env);
    let barrier = Arc::new(Barrier::new(CLIENTS));

    let pids: Vec<u32> = std::thread::scope(|s| {
        let threads: Vec<_> = (0..CLIENTS)
            .map(|_| {
                let barrier = barrier.clone();
                let cfg = &cfg;
                s.spawn(move || {
                    barrier.wait();
                    let client = BlockingClient::connect(cfg).unwrap_or_else(|e| panic!("{e}"));
                    client.hello().core.pid
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|t| t.join().expect("a client thread panicked"))
            .collect()
    });

    assert_eq!(pids.len(), CLIENTS);
    assert!(pids.iter().all(|&p| p == pids[0]), "{pids:?}");
    // The cores that lost the pipe claim touched nothing, not even a log: one core log.
    let logs = env.env.logs_dir();
    let core_logs: Vec<String> = std::fs::read_dir(&logs)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", logs.display()))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("core.") && name.ends_with(".log"))
        .collect();
    assert_eq!(core_logs.len(), 1, "{core_logs:?}");
    stop(&cfg);
}

#[test] // spec: ARCH-027
fn foreign_core_is_refused_and_named() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let other = env.root().join("other bin");
    std::fs::create_dir_all(&other).unwrap();
    let copy = other.join("ww-core.exe");
    std::fs::copy(&cfg.core_exe, &copy).unwrap();
    let mut fixture = CoreFixture::start_exe(&env, &copy);

    let err = BlockingClient::connect(&cfg).expect_err("an untrusted core was accepted");
    let ClientError::Untrusted { pid, image, .. } = &err else {
        panic!("expected Untrusted, got {err:?}");
    };
    assert_eq!(*pid, fixture.pid());
    assert_eq!(canonical(image), canonical(&copy));
    let text = err.to_string();
    assert!(text.contains(&image.display().to_string()), "{text}");
    assert!(text.contains(&pid.to_string()), "{text}");
    assert!(text.contains("ww core stop"), "{text}");

    // No second core: the fixture still serves the pipe.
    let raw = RawPipe::connect(&env);
    assert_eq!(raw.server_pid(), Some(fixture.pid()));
    drop(raw);

    // `ww core stop` clears it.
    stop(&cfg);
    assert_eq!(fixture.wait_exit(DEADLINE), Some(0));
}

#[test] // spec: ARCH-023
fn incompatible_older_core_is_drained_and_replaced() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let mut first = CoreFixture::start(&env);

    let mut newer = cfg.clone();
    newer.protocol = ProtocolVersion { major: 2, minor: 0 };
    let err = connect_async(&newer).expect_err("a 2.0 client connected to a 1.0 core");
    match &err {
        ClientError::Incompatible { core_protocol } => assert_eq!(core_protocol, "1.0"),
        other => panic!("expected Incompatible, got {other:?}"),
    }

    // The old core drained and exited; the client started a new one, which it left running.
    assert_eq!(first.wait_exit(DEADLINE), Some(0));
    let mut same = cfg.clone();
    same.start = StartPolicy::NoStart;
    let second = BlockingClient::connect(&same).unwrap_or_else(|e| panic!("{e}"));
    let second_pid = second.hello().core.pid;
    assert_ne!(second_pid, first.pid());
    let info = SystemInspector
        .inspect(second_pid)
        .expect("inspect the new core");
    assert_eq!(info.parent_pid, Some(std::process::id()));
    drop(second);
    stop(&cfg);
}

#[test] // spec: ARCH-018, ARCH-023
fn newer_client_with_no_start_never_drains_the_core() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let mut core = CoreFixture::start(&env);

    // A 2.0 client that may not start a core must not stop the one that runs either: it
    // reports itself incompatible and leaves the 1.0 core serving.
    let mut newer = cfg.clone();
    newer.protocol = ProtocolVersion { major: 2, minor: 0 };
    newer.start = StartPolicy::NoStart;
    let err = connect_async(&newer).expect_err("a 2.0 client connected to a 1.0 core");
    match &err {
        ClientError::Incompatible { core_protocol } => assert_eq!(core_protocol, "1.0"),
        other => panic!("expected Incompatible, got {other:?}"),
    }
    assert!(
        !core_log(&env).contains("core/drain"),
        "the --no-start client sent core/drain"
    );
    let mut same = cfg.clone();
    same.start = StartPolicy::NoStart;
    let client = BlockingClient::connect(&same).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(client.hello().core.pid, core.pid());
    drop(client);

    stop(&cfg);
    assert_eq!(core.wait_exit(DEADLINE), Some(0));
}

#[test] // spec: ARCH-014, ARCH-023
fn newer_core_is_never_drained_by_older_client() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let mut core = CoreFixture::start(&env);

    let mut older = cfg.clone();
    older.protocol = ProtocolVersion { major: 0, minor: 9 };
    let err = connect_async(&older).expect_err("a 0.9 client connected to a 1.0 core");
    match &err {
        ClientError::Incompatible { core_protocol } => assert_eq!(core_protocol, "1.0"),
        other => panic!("expected Incompatible, got {other:?}"),
    }
    assert!(err.to_string().contains("1.0"), "{err}");
    // The client never asked: the core logs every drain it refuses, and has logged none.
    assert!(
        !core_log(&env).contains("core/drain refused"),
        "the older client sent core/drain"
    );

    // Same core, still serving.
    let mut same = cfg.clone();
    same.start = StartPolicy::NoStart;
    let client = BlockingClient::connect(&same).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(client.hello().core.pid, core.pid());
    let echoed = ok(client
        .call_tool("test.echo", args(json!({"text": "still here"})), None)
        .expect("test.echo"));
    assert_eq!(echoed.structured["text"], "still here");
    drop(client);

    // An explicit stop from the same major is a normal drain.
    stop(&cfg);
    assert_eq!(core.wait_exit(DEADLINE), Some(0));
}

#[test] // spec: ARCH-014, ARCH-023
fn older_client_drain_and_wait_is_incompatible() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let mut core = CoreFixture::start(&env);

    // `ww core stop` from an older major: the core refuses the drain, the client says why,
    // and the core keeps serving.
    let mut older = cfg.clone();
    older.protocol = ProtocolVersion { major: 0, minor: 9 };
    let err = runtime()
        .block_on(drain_and_wait(&older, DEADLINE))
        .expect_err("an older client drained a newer core");
    match &err {
        ClientError::Incompatible { core_protocol } => assert_eq!(core_protocol, "1.0"),
        other => panic!("expected Incompatible, got {other:?}"),
    }
    let mut same = cfg.clone();
    same.start = StartPolicy::NoStart;
    let client = BlockingClient::connect(&same).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(client.hello().core.pid, core.pid());
    drop(client);

    stop(&cfg);
    assert_eq!(core.wait_exit(DEADLINE), Some(0));
}

#[test] // spec: ARCH-004, ARCH-019
fn non_ascii_and_spaced_roots_work() {
    let env = TestEnv::with_label("Ünïcödé space");
    assert!(env.root().to_string_lossy().contains("Ünïcödé space"));
    let cfg = config(&env);

    let client = BlockingClient::connect(&cfg).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(client.hello().core.pipe, env.env.pipe_name());
    let echoed = ok(client
        .call_tool("test.echo", args(json!({"text": "Ünïcödé space"})), None)
        .expect("test.echo"));
    assert_eq!(echoed.structured["text"], "Ünïcödé space");
    assert!(
        env.env.suite_state_db().exists(),
        "{} is missing",
        env.env.suite_state_db().display()
    );
    drop(client);
    stop(&cfg);
}

#[test] // spec: CORE-004
fn damaged_state_db_is_a_plain_start_failure() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    let before = sha256_file(&db);
    let expected = format!(
        "The core can't start: its state database at {} is damaged. Nothing was changed. `ww doctor` can set it aside.",
        db.display()
    );

    // Repeated, so a connection made in the moment between the core claiming the pipe and
    // exiting is met too: it must still be a start failure, never Closed or Io.
    for round in 0..20 {
        let err = BlockingClient::connect(&cfg)
            .expect_err(&format!("round {round}: connected to a damaged core"));
        match &err {
            ClientError::StartFailed {
                exit_code: Some(5),
                kind: StartFailure::StateDamaged,
                state_db,
                logs_dir,
            } => {
                assert_eq!(state_db, &db, "round {round}");
                assert_eq!(logs_dir, &env.env.logs_dir(), "round {round}");
            }
            other => panic!("round {round}: expected StartFailed(5), got {other:?}"),
        }
        let text = err.to_string();
        assert_eq!(text, expected, "round {round}");
        assert!(text.contains("Nothing was changed"));
    }
    assert_eq!(sha256_file(&db), before, "{} was changed", db.display());
}

#[test] // spec: ARCH-110, ARCH-035
fn call_mints_an_idempotency_key() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let client = BlockingClient::connect(&cfg).unwrap_or_else(|e| panic!("{e}"));

    // A write tool needs a key; two calls without one get two fresh keys and both run.
    let first = ok(client
        .call_tool("test.write", args(json!({"label": "minted"})), None)
        .expect("first test.write"));
    let second = ok(client
        .call_tool("test.write", args(json!({"label": "minted"})), None)
        .expect("second test.write"));
    assert_eq!(first.structured["executions"], 1);
    assert_eq!(second.structured["executions"], 2);

    // The same explicit key twice is one execution, replayed.
    let key = Some("ik_00000000000000aa".to_string());
    let a = ok(client
        .call_tool("test.write", args(json!({"label": "keyed"})), key.clone())
        .expect("keyed test.write"));
    let b = ok(client
        .call_tool("test.write", args(json!({"label": "keyed"})), key)
        .expect("keyed test.write again"));
    assert_eq!(a.structured["executions"], 1);
    assert_eq!(b.structured["executions"], 1);
    drop(client);
    stop(&cfg);
}

#[test] // spec: ARCH-031, ARCH-032
fn blocking_client_round_trip() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let client = BlockingClient::connect(&cfg).unwrap_or_else(|e| panic!("{e}"));

    let tools = client.list_tools().expect("tools/list");
    assert!(tools.iter().any(|t| t.name == "test.echo"), "{tools:?}");

    let mut events = client
        .subscribe(vec!["test:a".to_string()], BTreeMap::new())
        .expect("ww/subscribe");
    assert_eq!(events.topics().len(), 1);
    assert_eq!(events.topics()[0].topic, "test:a");

    ok(client
        .call_tool(
            "test.emit",
            args(json!({"topic": "a", "data": {"n": 1}})),
            None,
        )
        .expect("test.emit"));
    let event = events
        .next_within(DEADLINE)
        .expect("an event within the deadline")
        .expect("the core went away");
    assert_eq!(event.topic, "test:a");
    assert_eq!(event.seq, 1);
    assert_eq!(event.kind, EventKind::Event);
    assert_eq!(event.data, json!({"n": 1}));
    drop(events);
    drop(client);
    stop(&cfg);
}

#[test] // spec: ARCH-032
fn subscribe_keeps_replayed_events_in_order() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let client = BlockingClient::connect(&cfg).unwrap_or_else(|e| panic!("{e}"));
    for n in 1..=5 {
        ok(client
            .call_tool(
                "test.emit",
                args(json!({"topic": "r", "data": {"n": n}})),
                None,
            )
            .expect("test.emit"));
    }

    // Replays may arrive before the subscribe reply; none is lost or reordered. A cursor for
    // a topic not subscribed is dropped by the client (ruling P21).
    let since = BTreeMap::from([("test:r".to_string(), 0), ("test:stray".to_string(), 3)]);
    let mut events = client
        .subscribe(vec!["test:r".to_string()], since)
        .expect("ww/subscribe");
    assert_eq!(events.topics()[0].seq, 5);
    ok(client
        .call_tool(
            "test.emit",
            args(json!({"topic": "r", "data": {"n": 6}})),
            None,
        )
        .expect("test.emit"));
    for n in 1..=6_u64 {
        let event = events
            .next_within(DEADLINE)
            .expect("an event within the deadline")
            .expect("the core went away");
        assert_eq!((event.topic.as_str(), event.seq), ("test:r", n));
        assert_eq!(event.data, json!({"n": n}));
    }
    drop(events);
    drop(client);
    stop(&cfg);
}

#[test] // spec: ARCH-023
fn drain_and_wait_stops_a_running_core() {
    let env = TestEnv::new();
    let cfg = config(&env);
    let mut core = CoreFixture::start(&env);
    let outcome = runtime()
        .block_on(drain_and_wait(&cfg, DEADLINE))
        .expect("drain_and_wait");
    assert_eq!(outcome, DrainOutcome::Stopped);
    assert_eq!(core.wait_exit(DEADLINE), Some(0));
    assert!(RawPipe::try_connect(&env, Duration::ZERO).is_none());
}
