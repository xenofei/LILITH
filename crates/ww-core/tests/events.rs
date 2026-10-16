//! `ww/subscribe`, `ww/unsubscribe` and `ww/event` over the real pipe of an in-process core
//! (ARCH-032), driven with `RawPipe` and the test-channel tool `test.emit`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use ww_core::{CoreConfig, CoreThread, start_in_thread};
use ww_protocol::{EventKind, EventParams, rpc_code};
use ww_testkit::{FakeClock, RawPipe, SeqIds, TestEnv};

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
        .unwrap_or_else(|| panic!("no ww/event within {DEADLINE:?}"));
    serde_json::from_value(params.clone())
        .unwrap_or_else(|e| panic!("ww/event params are not EventParams ({e}): {params}"))
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
fn events_arrive_as_ww_event_notifications() {
    let env = TestEnv::with_label("Ünïcödé space");
    let _core = core(&env);
    let mut pipe = connect(&env);

    // The result is an `Outcome<SubscribeResult>`: each topic with its current seq.
    let response = pipe.call("ww/subscribe", json!({"topics": ["test:x", "test:y"]}));
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
        "ww/subscribe",
        json!({"topics": ["test:x"], "since_seq": {"test:x": 0}}),
    );
    assert_eq!(response["result"]["topics"][0]["seq"], 1, "{response}");
    let replayed = next_event(&mut other);
    assert_eq!((replayed.seq, replayed.kind), (1, EventKind::Event));

    // After unsubscribing from `test:x`, only `test:y` is delivered: the `test:y` event
    // emitted after a `test:x` one is the next event on the connection.
    let response = pipe.call("ww/unsubscribe", json!({"topics": ["test:x", "test:z"]}));
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
        let response = pipe.call("ww/subscribe", params.clone());
        assert_eq!(
            rpc_error(&response),
            rpc_code::INVALID_PARAMS,
            "{params}: {response}"
        );
    }
    for params in [json!({"topics": ["1a"]}), json!({"topics": ["a"], "x": 1})] {
        let response = pipe.call("ww/unsubscribe", params.clone());
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
    let response = pipe.call("ww/subscribe", json!({"topics": ["test:a"]}));
    assert_eq!(response["result"]["status"], "ok", "{response}");

    // Before core/hello the methods need the handshake.
    let mut fresh = RawPipe::connect(&env);
    let response = fresh.call("ww/subscribe", json!({"topics": ["test:a"]}));
    assert_eq!(
        rpc_error(&response),
        rpc_code::HANDSHAKE_REQUIRED,
        "{response}"
    );
}
