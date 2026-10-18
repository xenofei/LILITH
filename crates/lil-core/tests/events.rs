//! `lil/subscribe`, `lil/unsubscribe` and `lil/event` over the real pipe of an in-process core
//! (ARCH-032), driven with `RawPipe` and the test-channel tool `test.emit`; the bus with
//! snapshots computed outside its lock and its history cap; and the `asset:` and `project:`
//! topics with their snapshots (ARCH-044).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lil_core::events::{EventBus, SnapshotProvider};
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_protocol::{EventKind, EventParams, rpc_code};
use lil_testkit::{FakeClock, RawPipe, SeqIds, TestEnv};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// An in-process core with the fake clock and ids (QA-031), on a test channel.
fn core(env: &TestEnv) -> CoreThread {
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    })
}

/// A connection that has said hello with the test defaults.
fn connect(env: &TestEnv) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let response = pipe.hello(json!({}));
    assert_eq!(response["result"]["status"], "ok", "hello: {response}");
    pipe
}

fn next_event(pipe: &mut RawPipe) -> EventParams {
    let params = pipe
        .next_event(DEADLINE)
        .unwrap_or_else(|| panic!("no lil/event within {DEADLINE:?}"));
    serde_json::from_value(params.clone())
        .unwrap_or_else(|e| panic!("lil/event params are not EventParams ({e}): {params}"))
}

fn emit(pipe: &mut RawPipe, topic: &str, data: Value) -> Value {
    let outcome = pipe.tool("test.emit", json!({"topic": topic, "data": data}), None);
    assert_eq!(outcome["status"], "ok", "test.emit: {outcome}");
    outcome
}

/// The JSON-RPC error code of `response`.
fn rpc_error(response: &Value) -> i64 {
    response["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("expected a JSON-RPC error: {response}"))
}

#[test] // spec: ARCH-032
fn events_arrive_as_lil_event_notifications() {
    let env = TestEnv::with_label("Ünïcödé space");
    let _core = core(&env);
    let mut pipe = connect(&env);

    // The result is an `Outcome<SubscribeResult>`: each topic with its current seq.
    let response = pipe.call("lil/subscribe", json!({"topics": ["test:x", "test:y"]}));
    assert_eq!(
        response["result"],
        json!({"status": "ok", "topics": [
            {"topic": "test:x", "seq": 0},
            {"topic": "test:y", "seq": 0},
        ]}),
        "{response}"
    );

    // `test.emit` is a read tool: no idempotency key. It publishes to `test:<topic>`.
    let outcome = emit(&mut pipe, "x", json!({"n": 1, "label": "Ünïcödé space"}));
    assert_eq!(
        outcome["structured"],
        json!({"topic": "test:x", "seq": 1}),
        "{outcome}"
    );
    assert_eq!(
        next_event(&mut pipe),
        EventParams {
            topic: "test:x".into(),
            seq: 1,
            kind: EventKind::Event,
            data: json!({"n": 1, "label": "Ünïcödé space"}),
        }
    );

    // Another connection sees the same topic's sequence go on.
    let mut other = connect(&env);
    let response = other.call(
        "lil/subscribe",
        json!({"topics": ["test:x"], "since_seq": {"test:x": 0}}),
    );
    assert_eq!(response["result"]["topics"][0]["seq"], 1, "{response}");
    let replayed = next_event(&mut other);
    assert_eq!((replayed.seq, replayed.kind), (1, EventKind::Event));

    // After unsubscribing from `test:x`, only `test:y` is delivered: the `test:y` event
    // emitted after a `test:x` one is the next event on the connection.
    let response = pipe.call("lil/unsubscribe", json!({"topics": ["test:x", "test:z"]}));
    assert_eq!(
        response["result"],
        json!({"status": "ok", "topics": ["test:x"]}),
        "{response}"
    );
    emit(&mut pipe, "x", json!(2));
    emit(&mut pipe, "y", json!(1));
    let next = next_event(&mut pipe);
    assert_eq!((next.topic.as_str(), next.seq), ("test:y", 1));
    // The other connection still gets `test:x`.
    let next = next_event(&mut other);
    assert_eq!((next.topic.as_str(), next.seq), ("test:x", 2));
}

#[test] // spec: ARCH-032, QA-010
fn invalid_topic_is_invalid_params() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = connect(&env);

    for params in [
        json!({"topics": ["Test:a"]}),
        json!({"topics": ["test:a", "a b"]}),
        json!({"topics": [""]}),
        json!({"topics": ["test:"]}),
        // A cursor for a topic that is not subscribed.
        json!({"topics": ["test:a"], "since_seq": {"test:b": 1}}),
        // Strict params.
        json!({"topics": ["test:a"], "extra": true}),
        json!({"topics": "test:a"}),
    ] {
        let response = pipe.call("lil/subscribe", params.clone());
        assert_eq!(
            rpc_error(&response),
            rpc_code::INVALID_PARAMS,
            "{params}: {response}"
        );
    }
    for params in [json!({"topics": ["1a"]}), json!({"topics": ["a"], "x": 1})] {
        let response = pipe.call("lil/unsubscribe", params.clone());
        assert_eq!(
            rpc_error(&response),
            rpc_code::INVALID_PARAMS,
            "{params}: {response}"
        );
    }

    // `test.emit` with a topic that makes no valid name is a `params.invalid` error result.
    let outcome = pipe.tool("test.emit", json!({"topic": "", "data": 1}), None);
    assert_eq!(outcome["status"], "error", "{outcome}");
    assert_eq!(outcome["error"]["code"], "params.invalid", "{outcome}");
    let outcome = pipe.tool("test.emit", json!({"topic": "a", "data": 1, "x": 1}), None);
    assert_eq!(outcome["error"]["code"], "params.invalid", "{outcome}");

    // The connection is still usable, and a valid subscription works.
    let response = pipe.call("lil/subscribe", json!({"topics": ["test:a"]}));
    assert_eq!(response["result"]["status"], "ok", "{response}");

    // Before core/hello the methods need the handshake.
    let mut fresh = RawPipe::connect(&env);
    let response = fresh.call("lil/subscribe", json!({"topics": ["test:a"]}));
    assert_eq!(
        rpc_error(&response),
        rpc_code::HANDSHAKE_REQUIRED,
        "{response}"
    );
}

/// A provider that waits for its gate before it answers, counting the calls that started.
struct Gated {
    gate: tokio::sync::watch::Receiver<bool>,
    started: Arc<AtomicUsize>,
}

impl SnapshotProvider for Gated {
    fn snapshot<'a>(
        &'a self,
        topic: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            let mut gate = self.gate.clone();
            let _ = gate.wait_for(|open| *open).await;
            json!({"snapshot_of": topic})
        })
    }
}

/// A bus with a gated provider for `prefix`, the gate's sender and the provider's start count.
fn gated(bus: &EventBus, prefix: &str) -> (tokio::sync::watch::Sender<bool>, Arc<AtomicUsize>) {
    let (open, gate) = tokio::sync::watch::channel(false);
    let started = Arc::new(AtomicUsize::new(0));
    bus.register_snapshot(
        prefix,
        Arc::new(Gated {
            gate,
            started: started.clone(),
        }),
    );
    (open, started)
}

fn new_bus() -> Arc<EventBus> {
    Arc::new(EventBus::new(FakeClock::new()))
}

/// Yields until the gated provider has started `n` snapshots (a failure deadline only).
async fn started(count: &AtomicUsize, n: usize) {
    tokio::time::timeout(DEADLINE, async {
        while count.load(Ordering::SeqCst) < n {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the provider did not start");
}

/// The event lines waiting in `rx`, parsed.
fn lines(rx: &mut tokio::sync::mpsc::Receiver<String>) -> Vec<EventParams> {
    let mut out = Vec::new();
    while let Ok(line) = rx.try_recv() {
        let message: Value = serde_json::from_str(&line).unwrap();
        out.push(serde_json::from_value(message["params"].clone()).unwrap());
    }
    out
}

/// Waits until `bus` has handed every queued event of `client` to its forwarder, then gives
/// the forwarder a chance to put them in the channel.
async fn settle(bus: &EventBus, client: u64) {
    tokio::time::timeout(DEADLINE, async {
        while bus.queued(client) > 0 {
            tokio::task::yield_now().await;
        }
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the forwarder did not take the events");
}

fn topics(names: &[&str]) -> Vec<String> {
    names.iter().map(|t| (*t).to_string()).collect()
}

fn since(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(t, s)| ((*t).to_string(), *s)).collect()
}

/// `bus.subscribe` in a task of its own.
fn subscribe_later(
    bus: &Arc<EventBus>,
    client: u64,
    topic: &'static str,
    cursor: u64,
) -> tokio::task::JoinHandle<Vec<lil_protocol::TopicState>> {
    let bus = bus.clone();
    tokio::spawn(async move {
        bus.subscribe(client, &topics(&[topic]), &since(&[(topic, cursor)]))
            .await
    })
}

#[tokio::test] // spec: ARCH-032, ARCH-044
async fn snapshot_is_computed_outside_the_bus_lock() {
    let bus = new_bus();
    let (open, count) = gated(&bus, "test:");
    bus.publish("test:a", json!(1));
    let (tx2, mut rx2) = tokio::sync::mpsc::channel(16);
    bus.attach(2, tx2);
    bus.subscribe(2, &topics(&["test:b"]), &since(&[])).await;
    // Client 1 needs a snapshot of test:a, and its provider waits on the gate.
    let waiting = subscribe_later(&bus, 1, "test:a", 99);
    started(&count, 1).await;
    // A publish to another client goes on meanwhile: no lock is held across the provider.
    assert_eq!(bus.publish("test:b", json!("live")), 1);
    settle(&bus, 2).await;
    let got = lines(&mut rx2);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!((got[0].topic.as_str(), got[0].seq), ("test:b", 1));
    assert!(!waiting.is_finished());
    open.send(true).unwrap();
    let states = tokio::time::timeout(DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(states[0].seq, 1);
}

#[tokio::test] // spec: ARCH-032
async fn events_published_during_a_snapshot_follow_it_in_order() {
    let bus = new_bus();
    let (open, count) = gated(&bus, "test:");
    for n in 1..=3 {
        bus.publish("test:a", json!(n));
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    bus.attach(1, tx);
    let waiting = subscribe_later(&bus, 1, "test:a", 99);
    started(&count, 1).await;
    bus.publish("test:a", json!(4));
    bus.publish("test:a", json!(5));
    // Nothing reaches the client while its snapshot is pending.
    settle(&bus, 1).await;
    assert!(lines(&mut rx).is_empty());
    open.send(true).unwrap();
    tokio::time::timeout(DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap();
    settle(&bus, 1).await;
    let got: Vec<(u64, EventKind)> = lines(&mut rx).iter().map(|e| (e.seq, e.kind)).collect();
    // The snapshot is labelled with the seq the topic had when the call began, then what was
    // published while it was computed, in order; later events follow live.
    assert_eq!(
        got,
        [
            (3, EventKind::Snapshot),
            (4, EventKind::Event),
            (5, EventKind::Event)
        ]
    );
    bus.publish("test:a", json!(6));
    settle(&bus, 1).await;
    let got: Vec<u64> = lines(&mut rx).iter().map(|e| e.seq).collect();
    assert_eq!(got, [6]);
}

#[tokio::test] // spec: ARCH-032
async fn client_unsubscribed_during_a_snapshot_gets_nothing() {
    let bus = new_bus();
    let (open, count) = gated(&bus, "test:");
    bus.publish("test:a", json!(1));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    bus.attach(1, tx);
    let waiting = subscribe_later(&bus, 1, "test:a", 99);
    started(&count, 1).await;
    assert_eq!(bus.unsubscribe(1, &topics(&["test:a"])), ["test:a"]);
    bus.publish("test:a", json!(2));
    open.send(true).unwrap();
    tokio::time::timeout(DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap();
    settle(&bus, 1).await;
    assert!(lines(&mut rx).is_empty());
    assert_eq!(bus.queued(1), 0);

    // A client dropped meanwhile isn't brought back by its snapshot either.
    let other = new_bus();
    let (open, count) = gated(&other, "test:");
    other.publish("test:b", json!(1));
    let waiting = subscribe_later(&other, 5, "test:b", 99);
    started(&count, 1).await;
    assert_eq!(other.clients(), 1);
    other.drop_client(5);
    open.send(true).unwrap();
    tokio::time::timeout(DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.clients(), 0);
}

#[tokio::test] // spec: ARCH-032
async fn history_is_capped_at_1024_events_per_topic() {
    assert_eq!(lil_protocol::HISTORY_MAX_EVENTS, 1024);
    let bus = new_bus();
    for n in 1..=1030_u64 {
        bus.publish("test:a", json!(n));
    }
    // Seqs 7..=1030 are retained: a cursor of 6 replays them all, in order.
    let (tx, mut rx) = tokio::sync::mpsc::channel(2048);
    bus.attach(1, tx);
    bus.subscribe(1, &topics(&["test:a"]), &since(&[("test:a", 6)]))
        .await;
    settle(&bus, 1).await;
    let got = lines(&mut rx);
    assert_eq!(got.len(), 1024);
    assert_eq!(got[0].seq, 7);
    assert_eq!(got[1023].seq, 1030);
    assert!(got.iter().all(|e| e.kind == EventKind::Event));
    // A cursor of 5 is older than the first retained event: no provider, so resync_required.
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    bus.attach(2, tx);
    bus.subscribe(2, &topics(&["test:a"]), &since(&[("test:a", 5)]))
        .await;
    settle(&bus, 2).await;
    let got = lines(&mut rx);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].seq, got[0].kind), (1030, EventKind::ResyncRequired));
}

#[test] // spec: ARCH-044, ARCH-032
fn asset_and_project_topics_carry_kinds_and_snapshots() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("topics");
    env.register_workspace(&ws, true);
    let _core = core(&env);
    let ws_id = env.workspace_id(&ws);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": ws_id}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let asset = format!("asset:lil://{ws_id}/sketchbook/prop_kite");
    let response = pipe.call(
        "lil/subscribe",
        json!({"topics": [asset, "project:sketchbook"]}),
    );
    assert_eq!(response["result"]["status"], "ok", "{response}");

    let source = env.root().join("import-src").join("kite.png");
    lil_testkit::files::write_file(&source, b"kite");
    let imported = pipe.tool_ok(
        "asset.import",
        json!({"project": "sketchbook", "asset_id": "prop_kite", "type": "image",
               "files": [source.to_string_lossy()], "mode": "copy"}),
        Some("topics-1"),
    );
    let run = imported["run_id"].as_str().unwrap().to_string();
    pipe.tool_ok(
        "asset.signoff",
        json!({"project": "sketchbook", "asset_id": "prop_kite", "run_id": run,
               "files": ["kite.png"]}),
        Some("topics-2"),
    );
    pipe.tool_ok(
        "asset.pin_input",
        json!({"project": "sketchbook", "asset_id": "prop_kite",
               "ref": "proj:harbor/env_wall@1.0.0", "reason": "kept"}),
        Some("topics-3"),
    );
    let mut kinds = Vec::new();
    while kinds.len() < 6 {
        let e = next_event(&mut pipe);
        kinds.push((
            e.topic.clone(),
            e.data["kind"].as_str().unwrap().to_string(),
        ));
    }
    let on = |topic: &str| -> Vec<String> {
        kinds
            .iter()
            .filter(|(t, _)| t == topic)
            .map(|(_, k)| k.clone())
            .collect()
    };
    assert_eq!(
        on(&asset),
        ["candidate_run", "version_sealed", "manifest"],
        "{kinds:?}"
    );
    assert_eq!(
        on("project:sketchbook"),
        ["progress", "progress", "status"],
        "{kinds:?}"
    );

    // A cursor the history doesn't cover gets the current state: `asset.get` and
    // `project.status`.
    let mut other = RawPipe::connect(&env);
    let hello = other.hello(json!({"workspace": ws_id}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let mut cursors = serde_json::Map::new();
    cursors.insert(asset.clone(), json!(999));
    cursors.insert("project:sketchbook".to_string(), json!(999));
    let response = other.call(
        "lil/subscribe",
        json!({"topics": [asset, "project:sketchbook"], "since_seq": cursors}),
    );
    assert_eq!(response["result"]["status"], "ok", "{response}");
    let mut snapshots = BTreeMap::new();
    while snapshots.len() < 2 {
        let e = next_event(&mut other);
        assert_eq!(e.kind, EventKind::Snapshot, "{e:?}");
        snapshots.insert(e.topic.clone(), e.data);
    }
    let detail = &snapshots[&asset];
    assert_eq!(detail["asset_id"], "prop_kite", "{detail}");
    assert_eq!(detail["approved_version"], "1.0.0", "{detail}");
    assert_eq!(detail["versions"][0]["version"], "1.0.0", "{detail}");
    let status = &snapshots["project:sketchbook"];
    assert_eq!(status["project"], "sketchbook", "{status}");
    assert_eq!(status["assets_by_status"]["approved"], 1, "{status}");
}
