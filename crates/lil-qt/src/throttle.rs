//! Update throttling (APP-027, APP-139): leading-edge, so the first update of a stream applies at
//! once, then at most 10 Hz for a job's progress and 1 Hz for the telemetry samples; the newest
//! update waiting in a stream is the one applied. Confirmations are never throttled, and neither
//! is anything else.
//!
//! - **Streams** ([`classify`]): `progress` events on `job:<id>` are one stream per job; the
//!   `telemetry` topic's `sample` events (each carries the whole snapshot) are one stream; its
//!   `alert` and `alert_cleared` events, the `confirmations` topic and every other topic and
//!   kind are not throttled.
//! - **Order on a topic.** An unthrottled event never waits, and never pulls a waiting update
//!   forward (that would break the rate). Where it carries the state the waiting update would
//!   show, it replaces it: a `status` event on `job:<id>` carries the whole job, its progress
//!   included, so the job's waiting progress update is dropped, and a job's last progress never
//!   lands after its final status. A job's `retrying` starts a new attempt, so the old attempt's
//!   waiting progress is dropped too. A `snapshot` or `resync_required` drops the waiting update
//!   of its topic. Anything else leaves it waiting, and so overtakes it: a telemetry `alert`
//!   (samples are whole snapshots), a job's `unresponsive` or `watchdog` flag. A model applies
//!   each event as the state it carries, not as a step after the one before.
//! - **Idle streams** are forgotten ([`Coalescer::prune`]), and so is a topic no longer followed
//!   ([`Coalescer::forget`]), its waiting update with it.
//!
//! Qt-free: the bridge's event pump owns a [`Coalescer`] and asks it when to wake
//! ([`Coalescer::next_due`]).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use lil_protocol::{EventKind, EventParams};
use serde_json::Value;

/// At most this often for one job's progress: 10 Hz.
pub const JOB_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// At most this often for the telemetry samples: 1 Hz.
pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(1);

/// A leading-edge limit: an offer applies now when the last application is at least
/// `min_interval` ago (or there was none), else at the time it will be.
#[derive(Debug, Clone)]
pub struct LeadingEdge {
    pub min_interval: Duration,
    last: Option<Instant>,
}

/// When an offered update applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Now; this counts as the application.
    Now,
    /// Not before this time: offer it again then.
    At(Instant),
}

impl LeadingEdge {
    pub fn new(min_interval: Duration) -> LeadingEdge {
        LeadingEdge {
            min_interval,
            last: None,
        }
    }

    /// Whether an update offered at `now` applies now (recorded as the application) or later.
    pub fn offer(&mut self, now: Instant) -> Offer {
        match self.last {
            Some(last) if now < last + self.min_interval => Offer::At(last + self.min_interval),
            _ => {
                self.last = Some(now);
                Offer::Now
            }
        }
    }

    /// Records an application at `now` that didn't go through [`LeadingEdge::offer`].
    fn applied(&mut self, now: Instant) {
        self.last = Some(now);
    }
}

/// How an event is throttled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThrottleClass {
    /// A `progress` event of the job with this id (`job:<id>`): 10 Hz.
    JobProgress(String),
    /// A `telemetry` event of this series: 1 Hz. The topic publishes the whole snapshot in each
    /// `sample`, so its one series is `"sample"`.
    Telemetry(String),
    /// Never throttled: the `confirmations` topic (agent safety and cap prompts, APP-027).
    Never,
    /// Not throttled: everything else.
    Plain,
}

/// The class of an event on `topic` whose data is `data` (APP-027). A snapshot or a
/// `resync_required` (whose data is not an event's) is never throttled: the caller classifies
/// only `EventKind::Event`s.
pub fn classify(topic: &str, data: &Value) -> ThrottleClass {
    let kind = data.get("kind").and_then(Value::as_str);
    if topic == "confirmations" {
        return ThrottleClass::Never;
    }
    if let Some(job) = topic.strip_prefix("job:")
        && kind == Some("progress")
    {
        return ThrottleClass::JobProgress(job.to_string());
    }
    if topic == "telemetry" && kind == Some("sample") {
        return ThrottleClass::Telemetry("sample".to_string());
    }
    ThrottleClass::Plain
}

/// A throttled stream: its limit, and the newest update waiting with the time it is due.
#[derive(Debug)]
struct Stream {
    topic: String,
    edge: LeadingEdge,
    waiting: Option<(EventParams, Instant)>,
}

/// The streams' throttle (module docs): what to apply now, and what later.
#[derive(Debug, Default)]
pub struct Coalescer {
    /// Keyed by the stream (`job:<id>`, `telemetry:sample`).
    streams: BTreeMap<String, Stream>,
}

impl Coalescer {
    pub fn new() -> Coalescer {
        Coalescer::default()
    }

    /// Takes `ev`, which arrived at `now`, and returns what to apply now, in order.
    pub fn push(&mut self, ev: EventParams, now: Instant) -> Vec<EventParams> {
        let class = match ev.kind {
            EventKind::Event => classify(&ev.topic, &ev.data),
            EventKind::Snapshot | EventKind::ResyncRequired => ThrottleClass::Plain,
        };
        let (key, interval) = match class {
            ThrottleClass::JobProgress(job) => (format!("job:{job}"), JOB_PROGRESS_INTERVAL),
            ThrottleClass::Telemetry(series) => (format!("telemetry:{series}"), TELEMETRY_INTERVAL),
            ThrottleClass::Never | ThrottleClass::Plain => {
                // Module docs, "Order on a topic".
                let replaces = ev.kind != EventKind::Event
                    || (ev.topic.starts_with("job:")
                        && matches!(
                            ev.data.get("kind").and_then(Value::as_str),
                            Some("status" | "retrying")
                        ));
                if replaces {
                    for stream in self.streams.values_mut().filter(|s| s.topic == ev.topic) {
                        stream.waiting = None;
                    }
                }
                return vec![ev];
            }
        };
        let stream = self.streams.entry(key).or_insert_with(|| Stream {
            topic: ev.topic.clone(),
            edge: LeadingEdge::new(interval),
            waiting: None,
        });
        match stream.edge.offer(now) {
            Offer::Now => {
                stream.waiting = None;
                vec![ev]
            }
            Offer::At(due) => {
                // The newest update replaces the one waiting.
                stream.waiting = Some((ev, due));
                Vec::new()
            }
        }
    }

    /// The updates due at `now`, oldest due first.
    pub fn due(&mut self, now: Instant) -> Vec<EventParams> {
        let mut out: Vec<(Instant, EventParams)> = Vec::new();
        for stream in self.streams.values_mut() {
            if stream.waiting.as_ref().is_some_and(|(_, due)| *due <= now)
                && let Some((ev, due)) = stream.waiting.take()
            {
                stream.edge.applied(now);
                out.push((due, ev));
            }
        }
        out.sort_by_key(|(due, _)| *due);
        out.into_iter().map(|(_, ev)| ev).collect()
    }

    /// When the next waiting update is due, if one waits.
    pub fn next_due(&self) -> Option<Instant> {
        self.streams
            .values()
            .filter_map(|s| s.waiting.as_ref().map(|(_, due)| *due))
            .min()
    }

    /// Forgets every stream of `topic` (an unsubscribed topic): its waiting update is dropped.
    pub fn forget(&mut self, topic: &str) {
        self.streams.retain(|_, s| s.topic != topic);
    }

    /// Forgets the streams with nothing waiting whose interval has passed at `now`: their next
    /// update applies at once either way, so only the finished jobs' streams go.
    pub fn prune(&mut self, now: Instant) {
        self.streams.retain(|_, s| {
            s.waiting.is_some()
                || s.edge
                    .last
                    .is_some_and(|last| now < last + s.edge.min_interval)
        });
    }

    /// How many streams it keeps.
    pub fn streams(&self) -> usize {
        self.streams.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(topic: &str, seq: u64, data: Value) -> EventParams {
        EventParams {
            topic: topic.to_string(),
            seq,
            kind: EventKind::Event,
            data,
        }
    }

    fn progress(job: &str, seq: u64, value: u64) -> EventParams {
        ev(
            &format!("job:{job}"),
            seq,
            json!({"kind": "progress", "value": value, "max": 100}),
        )
    }

    fn values(evs: &[EventParams]) -> Vec<u64> {
        evs.iter()
            .map(|e| e.data["value"].as_u64().unwrap())
            .collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test] // spec: APP-027, APP-139
    fn first_progress_applies_now_then_at_most_10_hz() {
        let t0 = Instant::now();
        let mut c = Coalescer::new();
        assert_eq!(
            values(&c.push(progress("j1", 1, 1), t0)),
            [1],
            "leading edge"
        );
        // 50 updates in the next 99 ms: none applies yet, and one waits for t0 + 100 ms.
        for n in 2..=51 {
            assert!(
                c.push(progress("j1", n, n), t0 + ms(n * 99 / 51))
                    .is_empty()
            );
        }
        assert_eq!(c.next_due(), Some(t0 + ms(100)));
        assert!(c.due(t0 + ms(99)).is_empty());
        assert_eq!(values(&c.due(t0 + ms(100))), [51], "the newest one");
        assert_eq!(c.next_due(), None);
        // The next one, 30 ms after that application, waits 70 ms.
        assert!(c.push(progress("j1", 52, 52), t0 + ms(130)).is_empty());
        assert_eq!(c.next_due(), Some(t0 + ms(200)));
        // Another job is its own stream.
        assert_eq!(values(&c.push(progress("j2", 1, 7), t0 + ms(131))), [7]);
        assert_eq!(values(&c.due(t0 + ms(200))), [52]);
        // After a quiet second the next one applies at once again.
        assert_eq!(values(&c.push(progress("j1", 53, 53), t0 + ms(1300))), [53]);
    }

    #[test] // spec: APP-027
    fn telemetry_samples_are_1_hz_and_alerts_are_not_throttled() {
        let t0 = Instant::now();
        let sample = |seq, gpu| {
            ev(
                "telemetry",
                seq,
                json!({"kind": "sample", "snapshot": {"gpu": gpu}}),
            )
        };
        assert_eq!(
            classify("telemetry", &json!({"kind": "sample"})),
            ThrottleClass::Telemetry("sample".into())
        );
        let mut c = Coalescer::new();
        assert_eq!(c.push(sample(1, 10), t0).len(), 1);
        assert!(c.push(sample(2, 20), t0 + ms(400)).is_empty());
        assert!(c.push(sample(3, 30), t0 + ms(800)).is_empty());
        assert_eq!(c.next_due(), Some(t0 + ms(1000)));
        // An alert goes at once; the waiting sample keeps its time.
        let alert = ev("telemetry", 4, json!({"kind": "alert", "alert": "vram"}));
        let out = c.push(alert, t0 + ms(900));
        let seqs: Vec<u64> = out.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [4], "the alert alone");
        assert_eq!(c.next_due(), Some(t0 + ms(1000)));
        let due: Vec<u64> = c.due(t0 + ms(1000)).iter().map(|e| e.seq).collect();
        assert_eq!(due, [3], "the newest sample, on time");
        assert!(c.push(sample(5, 50), t0 + ms(1500)).is_empty());
        assert_eq!(c.next_due(), Some(t0 + ms(2000)));
        assert_eq!(
            classify("telemetry", &json!({"kind": "alert_cleared"})),
            ThrottleClass::Plain
        );
    }

    #[test] // spec: APP-027, APP-139
    fn confirmations_are_never_throttled() {
        let t0 = Instant::now();
        let mut c = Coalescer::new();
        for n in 1..=20 {
            let pending = ev(
                "confirmations",
                n,
                json!({"kind": "pending", "confirmation": {"id": format!("cf_{n}")}}),
            );
            assert_eq!(
                classify("confirmations", &pending.data),
                ThrottleClass::Never
            );
            assert_eq!(
                c.push(pending, t0 + ms(n)).len(),
                1,
                "update {n} applies at once"
            );
        }
        assert_eq!(c.next_due(), None);
        // Other kinds on a job topic are not throttled either.
        let status = ev("job:j1", 1, json!({"kind": "status", "job": {}}));
        assert_eq!(classify("job:j1", &status.data), ThrottleClass::Plain);
        assert_eq!(
            classify("jobs", &json!({"kind": "progress"})),
            ThrottleClass::Plain
        );
    }

    #[test] // spec: APP-027
    fn coalescing_keeps_the_newest_value_per_key() {
        let t0 = Instant::now();
        let mut c = Coalescer::new();
        c.push(progress("a", 1, 1), t0);
        c.push(progress("b", 1, 1), t0);
        for n in 2..=9 {
            c.push(progress("a", n, n * 10), t0 + ms(n));
            c.push(progress("b", n, n * 100), t0 + ms(n));
        }
        let due = c.due(t0 + ms(100));
        let got: Vec<(&str, u64)> = due
            .iter()
            .map(|e| (e.topic.as_str(), e.data["value"].as_u64().unwrap()))
            .collect();
        assert_eq!(got, [("job:a", 90), ("job:b", 900)]);

        // A job's status carries its progress: it replaces the waiting progress, which never
        // lands after it.
        assert!(c.push(progress("a", 10, 95), t0 + ms(150)).is_empty());
        let status = ev(
            "job:a",
            11,
            json!({"kind": "status", "job": {"status": "succeeded"}}),
        );
        let out = c.push(status, t0 + ms(160));
        let seqs: Vec<u64> = out.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [11]);
        assert!(c.due(t0 + ms(300)).iter().all(|e| e.topic != "job:a"));
        // A flag on the job topic leaves the waiting progress as it is.
        assert_eq!(values(&c.push(progress("a", 12, 96), t0 + ms(310))), [96]);
        assert!(c.push(progress("a", 13, 97), t0 + ms(320)).is_empty());
        let flag = ev("job:a", 14, json!({"kind": "unresponsive", "on": false}));
        assert_eq!(c.push(flag, t0 + ms(330)).len(), 1);
        assert!(c.due(t0 + ms(409)).is_empty(), "the rate holds");
        assert_eq!(values(&c.due(t0 + ms(410))), [97]);
        // A new attempt drops the old attempt's waiting progress.
        assert!(c.push(progress("a", 15, 98), t0 + ms(420)).is_empty());
        let retrying = ev(
            "job:a",
            16,
            json!({"kind": "retrying", "attempt": 2, "of": 3}),
        );
        assert_eq!(c.push(retrying, t0 + ms(430)).len(), 1);
        assert!(
            c.due(t0 + ms(600)).is_empty(),
            "attempt 1's progress is gone"
        );

        // A resync drops the waiting update: the topic is read again.
        c.push(progress("b", 10, 1000), t0 + ms(150));
        let resync = EventParams {
            topic: "job:b".into(),
            seq: 10,
            kind: EventKind::ResyncRequired,
            data: Value::Null,
        };
        let out = c.push(resync, t0 + ms(170));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, EventKind::ResyncRequired);
        assert_eq!(c.next_due(), None);
    }

    /// Finished jobs' streams don't pile up, and a forgotten topic's waiting update never
    /// comes.
    #[test] // spec: APP-027
    fn idle_and_forgotten_streams_are_dropped() {
        let t0 = Instant::now();
        let mut c = Coalescer::new();
        for job in 0..50 {
            c.push(progress(&format!("j{job}"), 1, 1), t0);
        }
        c.push(progress("j0", 2, 2), t0 + ms(10));
        assert_eq!(c.streams(), 50);
        c.prune(t0 + ms(50));
        assert_eq!(c.streams(), 50, "none is idle yet");
        c.prune(t0 + ms(100));
        assert_eq!(c.streams(), 1, "only the one with an update waiting");
        c.forget("job:j0");
        assert_eq!(c.streams(), 0);
        assert!(c.due(t0 + ms(200)).is_empty(), "its waiting update is gone");
    }

    #[test] // spec: APP-027
    fn leading_edge_offers_now_then_at_the_interval() {
        let t0 = Instant::now();
        let mut edge = LeadingEdge::new(ms(100));
        assert_eq!(edge.offer(t0), Offer::Now);
        assert_eq!(edge.offer(t0 + ms(40)), Offer::At(t0 + ms(100)));
        assert_eq!(edge.offer(t0 + ms(100)), Offer::Now);
        assert_eq!(edge.offer(t0 + ms(150)), Offer::At(t0 + ms(200)));
    }
}
