//! Event subscriptions and notifications (ARCH-032).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// History kept per topic, by monotonic time.
pub const HISTORY_SECS: u64 = 600;
/// Events queued per client before it gets `resync_required`.
pub const CLIENT_QUEUE: usize = 1024;
/// History kept per topic, by count (ARCH-032 clarification): the newest events, at most.
pub const HISTORY_MAX_EVENTS: usize = 1024;

/// `lil/subscribe` params. A topic missing from `since_seq` means live events only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeParams {
    pub topics: Vec<String>,
    #[serde(default)]
    pub since_seq: BTreeMap<String, u64>,
}

/// `lil/subscribe` result: each topic with its current sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeResult {
    pub topics: Vec<TopicState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TopicState {
    pub topic: String,
    pub seq: u64,
}

/// `lil/unsubscribe` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeParams {
    pub topics: Vec<String>,
}

/// `lil/unsubscribe` result: the topics removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnsubscribeResult {
    pub topics: Vec<String>,
}

/// `lil/event` notification params (core → client).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventParams {
    pub topic: String,
    pub seq: u64,
    pub kind: EventKind,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Event,
    Snapshot,
    ResyncRequired,
}

/// `^[a-z][a-z0-9_]*(:.+)?$`, where `.` is any character but a newline.
pub fn is_valid_topic(t: &str) -> bool {
    let (name, suffix) = match t.split_once(':') {
        Some((name, suffix)) => (name, Some(suffix)),
        None => (t, None),
    };
    let mut bytes = name.bytes();
    let head_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase());
    let name_ok =
        head_ok && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    let suffix_ok = suffix.is_none_or(|s| !s.is_empty() && !s.contains('\n'));
    name_ok && suffix_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test] // spec: ARCH-032
    fn topic_syntax() {
        for good in [
            "a",
            "jobs",
            "test:a",
            "asset:img/1.png",
            "a_1:x:y",
            "z9:Ünïcödé space",
        ] {
            assert!(is_valid_topic(good), "{good}");
        }
        for bad in [
            "", "A", "1a", "_a", "a-b", "a:", ":a", "a b", "Jobs:x", "a:\n", "é",
        ] {
            assert!(!is_valid_topic(bad), "{bad:?}");
        }
        assert_eq!(HISTORY_SECS, 600);
        assert_eq!(CLIENT_QUEUE, 1024);
        assert_eq!(HISTORY_MAX_EVENTS, 1024);

        // Wire shapes of the event messages.
        let ev = EventParams {
            topic: "test:a".into(),
            seq: 3,
            kind: EventKind::ResyncRequired,
            data: json!(null),
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"topic": "test:a", "seq": 3, "kind": "resync_required", "data": null})
        );
        for (k, w) in [
            (EventKind::Event, "event"),
            (EventKind::Snapshot, "snapshot"),
            (EventKind::ResyncRequired, "resync_required"),
        ] {
            assert_eq!(serde_json::to_value(k).unwrap(), json!(w));
        }
        let sub: SubscribeParams = serde_json::from_value(json!({"topics": ["jobs"]})).unwrap();
        assert!(sub.since_seq.is_empty());
        let sub: SubscribeParams =
            serde_json::from_value(json!({"topics": ["test:a"], "since_seq": {"test:a": 0}}))
                .unwrap();
        assert_eq!(sub.since_seq["test:a"], 0);
        assert!(
            serde_json::from_value::<SubscribeParams>(json!({"topics": [], "extra": 1})).is_err()
        );
        assert!(
            serde_json::from_value::<UnsubscribeParams>(json!({"topics": [], "extra": 1})).is_err()
        );
    }
}
