//! Events: per-topic sequence numbers and replay from a cursor (spec Appendix D, "Events").
//! Driven over a real `lil-core`'s pipe with `RawPipe` and the `test.emit` tool (QA-018).

use std::time::Duration;

use lil_protocol::{EventKind, EventParams};
use lil_testkit::{CoreFixture, RawPipe, TestEnv};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

fn hello(pipe: &mut RawPipe) {
    let response = pipe.hello(json!({}));
    assert_eq!(response["result"]["status"], "ok", "hello: {response}");
}

fn subscribe(pipe: &mut RawPipe, since_seq: Value) {
    let response = pipe.call(
        "ww/subscribe",
        json!({"topics": ["test:a"], "since_seq": since_seq}),
    );
    assert_eq!(response["result"]["status"], "ok", "subscribe: {response}");
}

fn emit(pipe: &mut RawPipe, n: u64) {
    let outcome = pipe.tool("test.emit", json!({"topic": "a", "data": {"n": n}}), None);
    assert_eq!(outcome["status"], "ok", "test.emit: {outcome}");
}

/// The next `ww/event` on `pipe`, parsed as the protocol's `EventParams`.
fn next_event(pipe: &mut RawPipe) -> EventParams {
    let params = pipe
        .next_event(DEADLINE)
        .unwrap_or_else(|| panic!("no ww/event within {DEADLINE:?}"));
    serde_json::from_value(params.clone())
        .unwrap_or_else(|e| panic!("ww/event params are not EventParams ({e}): {params}"))
}

fn assert_event(event: &EventParams, seq: u64, n: u64) {
    assert_eq!(event.topic, "test:a", "{event:?}");
    assert_eq!(event.seq, seq, "{event:?}");
    assert_eq!(event.kind, EventKind::Event, "{event:?}");
    assert_eq!(event.data, json!({"n": n}), "{event:?}");
}

#[test] // spec: ARCH-032
fn events_have_per_topic_sequence_and_replay_from_cursor() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);

    // Live subscriber: an empty cursor map means live events only.
    let mut live = RawPipe::connect(&env);
    hello(&mut live);
    subscribe(&mut live, json!({}));
    emit(&mut live, 1);
    emit(&mut live, 2);
    assert_event(&next_event(&mut live), 1, 1);
    assert_event(&next_event(&mut live), 2, 2);

    // Replaying subscriber: a cursor of 1 on `test:a` replays what came after it.
    let mut replay = RawPipe::connect(&env);
    hello(&mut replay);
    subscribe(&mut replay, json!({"test:a": 1}));
    // A third, live event marks the end of the replay without waiting on the clock: the
    // replaying subscriber sees exactly seq 2 and then seq 3, never seq 1.
    emit(&mut live, 3);
    assert_event(&next_event(&mut replay), 2, 2);
    assert_event(&next_event(&mut replay), 3, 3);
}
