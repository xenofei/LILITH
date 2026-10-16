//! The event bus (ARCH-032, CORE-012) behind `ww/subscribe`, `ww/unsubscribe` and the
//! `ww/event` notifications. Infrastructure, not a CORE-014 module.
//!
//! - **Sequences.** Each topic's `seq` starts at 1 and goes up by one per [`EventBus::publish`].
//! - **History.** Each topic keeps its events for [`HISTORY_SECS`] of *monotonic* time (a
//!   wall-clock jump changes nothing), pruned on publish and on subscribe (CORE-012).
//! - **Subscribing with a cursor `s`.** `s` equal to the topic's current seq replays nothing;
//!   a history that still holds `s+1..=current` replays those events in order; anything else
//!   (older than the history, or beyond the current seq, such as a cursor from an earlier
//!   core) gets one `snapshot` from the [`SnapshotProvider`] with the longest matching prefix,
//!   or else one `resync_required`. No cursor means live events only.
//! - **Delivery.** Each client has its own queue of at most [`CLIENT_QUEUE`] events, separate
//!   from the connection's outbound channel that responses share. A forwarder task per client
//!   moves events from the queue into that channel in order, as `ww/event` lines; only it
//!   ever waits on a slow client. When [`EventBus::publish`] finds a client's queue full, it
//!   clears the queue and queues one `resync_required` per topic the client subscribes to, at
//!   that topic's current seq. Responses are never dropped: only the bus queue overflows.
//! - **Forwarder lifetime.** A forwarder ends when its client is dropped
//!   ([`EventBus::drop_client`], from `Router::closed`), re-attached, or the bus is dropped,
//!   and when the outbound channel closes.
//!
//! **Locks.** One `std` mutex guards the topics and the clients; each client's queue has its
//! own, taken after the bus lock when both are needed. Nothing is awaited while either is
//! held. A [`SnapshotProvider`] runs under the bus lock, so the snapshot and the sequence it is
//! labelled with agree; a provider must not call back into the bus.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use ww_platform::Clock;
use ww_protocol::{
    CLIENT_QUEUE, EventKind, EventParams, HISTORY_SECS, TopicState, encode_notification, method,
};

/// A client of the bus: the connection's `ConnCtx::conn_id`.
pub type ClientId = u64;

/// Gives the current state of a topic, for a subscriber whose cursor the history no longer
/// covers. It runs under the bus lock and must not call back into the bus.
pub trait SnapshotProvider: Send + Sync {
    fn snapshot(&self, topic: &str) -> Value;
}

/// The core's event bus (module docs).
pub struct EventBus {
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    topics: HashMap<String, Topic>,
    clients: HashMap<ClientId, Client>,
    /// `(topic prefix, provider)`.
    providers: Vec<(String, Arc<dyn SnapshotProvider>)>,
}

/// One topic: its current seq and its retained events, oldest first, with consecutive seqs.
#[derive(Default)]
struct Topic {
    seq: u64,
    history: VecDeque<Retained>,
}

struct Retained {
    /// Monotonic time of the publish.
    at: Duration,
    seq: u64,
    /// The event's `ww/event` line, shared with every queue it is in.
    line: Arc<str>,
}

/// One queued `ww/event`.
#[derive(Clone)]
struct Queued {
    topic: Arc<str>,
    line: Arc<str>,
}

struct Client {
    topics: BTreeSet<String>,
    pending: Arc<Pending>,
    /// Stops the forwarder, once one is attached.
    forwarder: Option<CancellationToken>,
}

/// A client's queue, shared with its forwarder.
#[derive(Default)]
struct Pending {
    queue: Mutex<VecDeque<Queued>>,
    wake: Notify,
}

impl Pending {
    fn lock(&self) -> MutexGuard<'_, VecDeque<Queued>> {
        // Every update is a single push, pop or clear: a panicking holder leaves it usable.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Client {
    fn new() -> Client {
        Client {
            topics: BTreeSet::new(),
            pending: Arc::new(Pending::default()),
            forwarder: None,
        }
    }

    fn stop_forwarder(&mut self) {
        if let Some(stop) = self.forwarder.take() {
            stop.cancel();
        }
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock`: formatting must not deadlock when the bus lock is already held.
        let mut out = f.debug_struct("EventBus");
        if let Ok(inner) = self.inner.try_lock() {
            out.field("topics", &inner.topics.len())
                .field("clients", &inner.clients.len());
        }
        out.finish_non_exhaustive()
    }
}

impl Drop for EventBus {
    fn drop(&mut self) {
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        for client in inner.clients.values_mut() {
            client.stop_forwarder();
        }
    }
}

impl EventBus {
    /// A bus whose history is timed by `clock`'s monotonic time.
    pub fn new(clock: Arc<dyn Clock>) -> EventBus {
        EventBus {
            clock,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Each operation leaves the maps consistent before it can panic (a provider is called
        // before anything is changed for its topic), so a poisoned lock is still usable.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publishes `data` on `topic` (a valid topic name) and returns its new seq. Never waits:
    /// a client whose queue is full gets `resync_required` instead (module docs).
    pub fn publish(&self, topic: &str, data: Value) -> u64 {
        debug_assert!(ww_protocol::is_valid_topic(topic), "{topic:?}");
        let now = self.clock.monotonic();
        let mut guard = self.lock();
        let inner = &mut *guard;
        let log = inner.topics.entry(topic.to_string()).or_default();
        log.seq += 1;
        let seq = log.seq;
        let line = event_line(topic, seq, EventKind::Event, data);
        log.history.push_back(Retained {
            at: now,
            seq,
            line: line.clone(),
        });
        prune(&mut log.history, now);

        let item = Queued {
            topic: Arc::from(topic),
            line,
        };
        for (id, client) in &inner.clients {
            if client.topics.contains(topic) {
                let overflowed = {
                    let mut queue = client.pending.lock();
                    enqueue(&mut queue, item.clone(), &client.topics, &inner.topics)
                };
                if overflowed {
                    tracing::info!(
                        client = id,
                        topic,
                        "a client's event queue overflowed; it gets resync_required"
                    );
                }
                client.pending.wake.notify_one();
            }
        }
        seq
    }

    /// Starts `client`'s forwarder, which sends its queued events into `outbound` as
    /// `ww/event` lines. A forwarder attached earlier for the client is stopped. Must be
    /// called inside a tokio runtime.
    pub fn attach(&self, client: ClientId, outbound: mpsc::Sender<String>) {
        let mut inner = self.lock();
        let entry = inner.clients.entry(client).or_insert_with(Client::new);
        entry.stop_forwarder();
        let stop = CancellationToken::new();
        entry.forwarder = Some(stop.clone());
        tokio::spawn(forward(entry.pending.clone(), stop, outbound));
    }

    /// Subscribes `client` to `topics` (valid names), replaying from the cursors in `since`
    /// (module docs). Returns each topic, once, in the order given, with its current seq.
    pub fn subscribe(
        &self,
        client: ClientId,
        topics: &[String],
        since: &BTreeMap<String, u64>,
    ) -> Vec<TopicState> {
        let now = self.clock.monotonic();
        let mut guard = self.lock();
        let inner = &mut *guard;
        let entry = inner.clients.entry(client).or_insert_with(Client::new);
        let mut states: Vec<TopicState> = Vec::with_capacity(topics.len());
        for topic in topics {
            if states.iter().any(|s| &s.topic == topic) {
                continue;
            }
            if let Some(log) = inner.topics.get_mut(topic) {
                prune(&mut log.history, now);
            }
            entry.topics.insert(topic.clone());
            let log = inner.topics.get(topic);
            let current = log.map_or(0, |l| l.seq);
            states.push(TopicState {
                topic: topic.clone(),
                seq: current,
            });
            let Some(&cursor) = since.get(topic) else {
                continue; // live events only
            };
            if cursor == current {
                continue;
            }
            let covered = cursor < current
                && log
                    .and_then(|l| l.history.front())
                    .is_some_and(|first| first.seq <= cursor + 1);
            let shared_topic: Arc<str> = Arc::from(topic.as_str());
            let mut items = Vec::new();
            if covered {
                let history = log.map(|l| &l.history).into_iter().flatten();
                for retained in history.filter(|r| r.seq > cursor) {
                    items.push(Queued {
                        topic: shared_topic.clone(),
                        line: retained.line.clone(),
                    });
                }
            } else {
                let provider = inner
                    .providers
                    .iter()
                    .filter(|(prefix, _)| topic.starts_with(prefix.as_str()))
                    .max_by_key(|(prefix, _)| prefix.len());
                let (kind, data) = match provider {
                    Some((_, p)) => (EventKind::Snapshot, p.snapshot(topic)),
                    None => (EventKind::ResyncRequired, Value::Null),
                };
                items.push(Queued {
                    topic: shared_topic,
                    line: event_line(topic, current, kind, data),
                });
            }
            let mut queue = entry.pending.lock();
            for item in items {
                enqueue(&mut queue, item, &entry.topics, &inner.topics);
            }
        }
        entry.pending.wake.notify_one();
        states
    }

    /// Unsubscribes `client` from `topics` and drops its queued events of those topics.
    /// Returns the topics it had, once each, in the order given.
    pub fn unsubscribe(&self, client: ClientId, topics: &[String]) -> Vec<String> {
        let mut inner = self.lock();
        let Some(entry) = inner.clients.get_mut(&client) else {
            return Vec::new();
        };
        let removed: Vec<String> = topics
            .iter()
            .filter(|t| entry.topics.remove(t.as_str()))
            .cloned()
            .collect();
        if !removed.is_empty() {
            entry
                .pending
                .lock()
                .retain(|q| !removed.iter().any(|t| t.as_str() == &*q.topic));
        }
        removed
    }

    /// Forgets `client`: its subscriptions, its queue and its forwarder.
    pub fn drop_client(&self, client: ClientId) {
        let gone = self.lock().clients.remove(&client);
        if let Some(mut gone) = gone {
            gone.stop_forwarder();
        }
    }

    /// Topics starting with `topic_prefix` get their snapshots from `p` (the longest
    /// matching prefix wins).
    pub fn register_snapshot(&self, topic_prefix: &str, p: Arc<dyn SnapshotProvider>) {
        self.lock().providers.push((topic_prefix.to_string(), p));
    }

    /// How many events wait in `client`'s queue (not yet taken by its forwarder).
    pub fn queued(&self, client: ClientId) -> usize {
        let inner = self.lock();
        inner
            .clients
            .get(&client)
            .map_or(0, |c| c.pending.lock().len())
    }

    /// The events waiting in `client`'s queue.
    #[cfg(test)]
    fn queued_events(&self, client: ClientId) -> Vec<EventParams> {
        let inner = self.lock();
        let Some(entry) = inner.clients.get(&client) else {
            return Vec::new();
        };
        let queue = entry.pending.lock();
        queue
            .iter()
            .map(|q| {
                let message: Value = serde_json::from_str(&q.line).expect("a queued line is JSON");
                let event: EventParams =
                    serde_json::from_value(message["params"].clone()).expect("EventParams");
                assert_eq!(event.topic, &*q.topic);
                event
            })
            .collect()
    }
}

/// Queues `item` unless the queue is full; a full queue is cleared and gets one
/// `resync_required` per topic in `subscribed`, at its current seq in `logs`. Returns whether
/// it overflowed.
fn enqueue(
    queue: &mut VecDeque<Queued>,
    item: Queued,
    subscribed: &BTreeSet<String>,
    logs: &HashMap<String, Topic>,
) -> bool {
    if queue.len() < CLIENT_QUEUE {
        queue.push_back(item);
        return false;
    }
    queue.clear();
    for topic in subscribed {
        let seq = logs.get(topic).map_or(0, |l| l.seq);
        queue.push_back(Queued {
            topic: Arc::from(topic.as_str()),
            line: event_line(topic, seq, EventKind::ResyncRequired, Value::Null),
        });
    }
    true
}

/// Drops the events older than [`HISTORY_SECS`] of monotonic time at `now`.
fn prune(history: &mut VecDeque<Retained>, now: Duration) {
    let keep = Duration::from_secs(HISTORY_SECS);
    while history
        .front()
        .is_some_and(|r| now.saturating_sub(r.at) > keep)
    {
        history.pop_front();
    }
}

/// The `ww/event` notification line of one event.
fn event_line(topic: &str, seq: u64, kind: EventKind, data: Value) -> Arc<str> {
    let params = EventParams {
        topic: topic.to_string(),
        seq,
        kind,
        data,
    };
    let params = serde_json::to_value(&params).expect("EventParams serialise");
    Arc::from(encode_notification(method::EVENT, &params))
}

/// A client's forwarder: moves its queued events into `outbound`, in order, until `stop` is
/// cancelled or `outbound` closes. No lock is held across an `.await`.
async fn forward(pending: Arc<Pending>, stop: CancellationToken, outbound: mpsc::Sender<String>) {
    loop {
        let next = pending.lock().pop_front();
        match next {
            Some(item) => {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => return,
                    sent = outbound.send(item.line.to_string()) => {
                        if sent.is_err() {
                            return; // the connection's writer is gone
                        }
                    }
                }
            }
            None => {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => return,
                    () = outbound.closed() => return,
                    () = pending.wake.notified() => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::mpsc;
    use ww_protocol::{CLIENT_QUEUE, EventKind, EventParams, method};
    use ww_testkit::FakeClock;

    use super::*;

    /// A failure deadline only, never a measurement (QA-031).
    const DEADLINE: Duration = Duration::from_secs(30);

    fn bus() -> (Arc<FakeClock>, EventBus) {
        let clock = FakeClock::new();
        let bus = EventBus::new(clock.clone());
        (clock, bus)
    }

    fn names(topics: &[&str]) -> Vec<String> {
        topics.iter().map(|t| (*t).to_string()).collect()
    }

    fn cursors(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(t, s)| ((*t).to_string(), *s)).collect()
    }

    fn state(topic: &str, seq: u64) -> TopicState {
        TopicState {
            topic: topic.into(),
            seq,
        }
    }

    fn event(topic: &str, seq: u64, kind: EventKind, data: Value) -> EventParams {
        EventParams {
            topic: topic.into(),
            seq,
            kind,
            data,
        }
    }

    /// The `ww/event` params of an outbound line.
    fn parse_line(line: &str) -> EventParams {
        let message: Value = serde_json::from_str(line).expect("a JSON line");
        assert_eq!(message["method"], method::EVENT, "{line}");
        serde_json::from_value(message["params"].clone()).expect("EventParams")
    }

    /// The next line on `rx`; the timeout is a hang guard only.
    async fn next_line(rx: &mut mpsc::Receiver<String>) -> Option<String> {
        tokio::time::timeout(DEADLINE, rx.recv())
            .await
            .expect("no outbound line within the deadline")
    }

    /// Waits (yielding, no sleeps) until `client`'s bus queue is empty: its forwarder has
    /// taken everything queued.
    async fn drained(bus: &EventBus, client: ClientId) {
        tokio::time::timeout(DEADLINE, async {
            while bus.queued(client) > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the forwarder did not take the queued events");
    }

    struct Fixed(&'static str);

    impl SnapshotProvider for Fixed {
        fn snapshot(&self, topic: &str) -> Value {
            json!({"provider": self.0, "topic": topic})
        }
    }

    #[test] // spec: ARCH-032
    fn seq_is_per_topic_and_monotonic() {
        let (_clock, bus) = bus();
        assert_eq!(bus.publish("test:a", json!(1)), 1);
        assert_eq!(bus.publish("test:a", json!(2)), 2);
        assert_eq!(bus.publish("test:b", json!(1)), 1);
        assert_eq!(bus.publish("test:a", json!(3)), 3);
        assert_eq!(bus.publish("test:b", json!(2)), 2);
        // `ww/subscribe` reports each topic's current seq; a topic never published is at 0.
        let states = bus.subscribe(1, &names(&["test:a", "test:b", "jobs"]), &cursors(&[]));
        assert_eq!(
            states,
            [state("test:a", 3), state("test:b", 2), state("jobs", 0)]
        );
        // Live events only: nothing was queued, and the next one continues the sequence.
        assert_eq!(bus.queued(1), 0);
        assert_eq!(bus.publish("jobs", json!("x")), 1);
        assert_eq!(bus.publish("test:a", json!(4)), 4);
        assert_eq!(
            bus.queued_events(1),
            [
                event("jobs", 1, EventKind::Event, json!("x")),
                event("test:a", 4, EventKind::Event, json!(4)),
            ]
        );
    }

    #[test] // spec: ARCH-032, CORE-012
    fn replay_from_cursor() {
        let (clock, bus) = bus();
        for n in 1..=3 {
            bus.publish("test:a", json!({"n": n}));
            clock.advance(Duration::from_secs(10));
        }
        // A cursor inside the history replays what came after it, in order.
        let states = bus.subscribe(1, &names(&["test:a"]), &cursors(&[("test:a", 1)]));
        assert_eq!(states, [state("test:a", 3)]);
        assert_eq!(
            bus.queued_events(1),
            [
                event("test:a", 2, EventKind::Event, json!({"n": 2})),
                event("test:a", 3, EventKind::Event, json!({"n": 3})),
            ]
        );
        // A cursor of 0 replays everything retained.
        bus.subscribe(2, &names(&["test:a"]), &cursors(&[("test:a", 0)]));
        let seqs: Vec<u64> = bus.queued_events(2).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2, 3]);
        // A cursor at the current seq replays nothing; neither does no cursor.
        bus.subscribe(3, &names(&["test:a"]), &cursors(&[("test:a", 3)]));
        bus.subscribe(4, &names(&["test:a"]), &cursors(&[]));
        assert_eq!(bus.queued(3), 0);
        assert_eq!(bus.queued(4), 0);
        // Live events follow the replay.
        bus.publish("test:a", json!({"n": 4}));
        let seqs: Vec<u64> = bus.queued_events(1).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [2, 3, 4]);
        assert_eq!(bus.queued(3), 1);
        assert_eq!(bus.queued(4), 1);
    }

    #[test] // spec: ARCH-032, CORE-012
    fn cursor_older_than_ten_minutes_gets_snapshot() {
        let (clock, bus) = bus();
        bus.register_snapshot("test:", Arc::new(Fixed("test")));
        bus.register_snapshot("test:a", Arc::new(Fixed("test:a")));
        bus.publish("test:a", json!(1));
        bus.publish("test:a", json!(2));
        bus.publish("test:b", json!(1));
        clock.advance(Duration::from_secs(601));
        // The history no longer covers seq 2: one snapshot, at the current seq, from the
        // provider with the longest matching prefix.
        let states = bus.subscribe(
            1,
            &names(&["test:a", "test:b"]),
            &cursors(&[("test:a", 1), ("test:b", 0)]),
        );
        assert_eq!(states, [state("test:a", 2), state("test:b", 1)]);
        assert_eq!(
            bus.queued_events(1),
            [
                event(
                    "test:a",
                    2,
                    EventKind::Snapshot,
                    json!({"provider": "test:a", "topic": "test:a"})
                ),
                event(
                    "test:b",
                    1,
                    EventKind::Snapshot,
                    json!({"provider": "test", "topic": "test:b"})
                ),
            ]
        );
        // The sequence goes on after the history expired.
        assert_eq!(bus.publish("test:a", json!(3)), 3);
    }

    #[test] // spec: ARCH-032, CORE-012
    fn cursor_older_without_provider_gets_resync_required() {
        let (clock, bus) = bus();
        bus.register_snapshot("other:", Arc::new(Fixed("other")));
        bus.publish("jobs", json!(1));
        bus.publish("jobs", json!(2));
        clock.advance(Duration::from_secs(601));
        bus.publish("jobs", json!(3));
        // Seq 3 is retained, seq 2 is not: the cursor 1 is not covered.
        bus.subscribe(1, &names(&["jobs"]), &cursors(&[("jobs", 1)]));
        assert_eq!(
            bus.queued_events(1),
            [event("jobs", 3, EventKind::ResyncRequired, Value::Null)]
        );
        // The cursor 2 is: seq 3 is replayed.
        bus.subscribe(2, &names(&["jobs"]), &cursors(&[("jobs", 2)]));
        assert_eq!(
            bus.queued_events(2),
            [event("jobs", 3, EventKind::Event, json!(3))]
        );
    }

    #[test] // spec: ARCH-032
    fn cursor_from_the_future_gets_resync_required() {
        let (_clock, bus) = bus();
        bus.publish("test:a", json!(1));
        // A cursor from an earlier core instance: beyond the current seq.
        let states = bus.subscribe(
            1,
            &names(&["test:a", "test:new"]),
            &cursors(&[("test:a", 5), ("test:new", 2)]),
        );
        assert_eq!(states, [state("test:a", 1), state("test:new", 0)]);
        assert_eq!(
            bus.queued_events(1),
            [
                event("test:a", 1, EventKind::ResyncRequired, Value::Null),
                event("test:new", 0, EventKind::ResyncRequired, Value::Null),
            ]
        );
    }

    #[tokio::test] // spec: ARCH-032
    async fn overflow_clears_queue_and_sends_resync_required_once_per_topic() {
        let (_clock, bus) = bus();
        // An outbound channel of capacity 1 whose receiver is never read.
        let (tx, _rx) = mpsc::channel(1);
        bus.attach(7, tx);
        bus.subscribe(7, &names(&["test:a", "test:b"]), &cursors(&[]));
        // Two events leave the bus queue: one fills the channel, the forwarder holds the other
        // in a `send` that never completes.
        bus.publish("test:a", json!(1));
        bus.publish("test:b", json!(1));
        drained(&bus, 7).await;
        // The bus queue fills up...
        for n in 0..CLIENT_QUEUE {
            let topic = if n % 2 == 0 { "test:a" } else { "test:b" };
            bus.publish(topic, json!(n));
        }
        assert_eq!(bus.queued(7), CLIENT_QUEUE);
        // ...and one more event overflows it (CLIENT_QUEUE + 1 + 2 published in all).
        bus.publish("test:a", json!("overflow"));
        // Only `resync_required`, exactly one per subscribed topic, at its current seq (P9).
        assert_eq!(bus.queued(7), 2);
        assert_eq!(
            bus.queued_events(7),
            [
                event("test:a", 514, EventKind::ResyncRequired, Value::Null),
                event("test:b", 513, EventKind::ResyncRequired, Value::Null),
            ]
        );
        // Later events queue normally behind them.
        bus.publish("test:b", json!("after"));
        assert_eq!(bus.queued(7), 3);
    }

    #[tokio::test] // spec: ARCH-032
    async fn responses_are_not_dropped_by_event_overflow() {
        let (_clock, bus) = bus();
        let (tx, mut rx) = mpsc::channel(1);
        // The connection's own sender, which responses use.
        let responses = tx.clone();
        bus.attach(3, tx);
        bus.subscribe(3, &names(&["test:a"]), &cursors(&[]));
        bus.publish("test:a", json!(0));
        drained(&bus, 3).await;
        // Seq 1 fills the channel. Overflow the client's bus queue with seqs 2..=1026;
        // `publish` never waits on the slow client.
        for n in 1..=CLIENT_QUEUE + 1 {
            bus.publish("test:a", json!(n));
        }
        assert_eq!(
            bus.queued_events(3),
            [event(
                "test:a",
                1026,
                EventKind::ResyncRequired,
                Value::Null
            )]
        );
        // A response queued meanwhile reaches the writer.
        let response = tokio::spawn(async move {
            responses
                .send(r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok"}}"#.to_string())
                .await
        });
        let mut events = Vec::new();
        let mut saw_response = false;
        while !(saw_response && events.len() == 2) {
            let line = next_line(&mut rx).await.expect("the channel stays open");
            if line.contains(r#""id":1"#) {
                assert!(!saw_response, "the response arrived twice");
                saw_response = true;
            } else {
                events.push(parse_line(&line));
            }
        }
        response.await.unwrap().expect("the response was queued");
        // The events the client got: the one already out of the bus, then the resync.
        let kinds: Vec<(u64, EventKind)> = events.iter().map(|e| (e.seq, e.kind)).collect();
        assert_eq!(
            kinds,
            [(1, EventKind::Event), (1026, EventKind::ResyncRequired)]
        );
    }

    #[tokio::test] // spec: ARCH-032
    async fn unsubscribe_stops_delivery_and_returns_removed_topics() {
        let (_clock, bus) = bus();
        let (tx, mut rx) = mpsc::channel(16);
        bus.attach(5, tx);
        bus.subscribe(5, &names(&["test:a", "test:b"]), &cursors(&[]));
        bus.publish("test:a", json!("a1"));
        assert_eq!(parse_line(&next_line(&mut rx).await.unwrap()).seq, 1);

        // Only topics the client had are reported, in the order asked.
        let removed = bus.unsubscribe(5, &names(&["test:c", "test:a", "test:a"]));
        assert_eq!(removed, ["test:a"]);
        assert!(bus.unsubscribe(5, &names(&["test:a"])).is_empty());

        // `test:a` is no longer delivered; `test:b`, published after it, still is, so the next
        // line on the connection shows that `test:a` was skipped.
        bus.publish("test:a", json!("a2"));
        bus.publish("test:b", json!("b1"));
        let next = parse_line(&next_line(&mut rx).await.unwrap());
        assert_eq!(next, event("test:b", 1, EventKind::Event, json!("b1")));
        assert_eq!(bus.queued(5), 0);
    }

    #[test] // spec: ARCH-032
    fn unsubscribe_drops_queued_events_of_removed_topics() {
        let (_clock, bus) = bus();
        bus.subscribe(1, &names(&["test:a", "test:b"]), &cursors(&[]));
        bus.publish("test:a", json!(1));
        bus.publish("test:b", json!(1));
        bus.unsubscribe(1, &names(&["test:a"]));
        assert_eq!(
            bus.queued_events(1),
            [event("test:b", 1, EventKind::Event, json!(1))]
        );
    }

    #[tokio::test] // spec: ARCH-032
    async fn dropped_client_is_forgotten() {
        let (_clock, bus) = bus();
        let (tx, mut rx) = mpsc::channel(16);
        bus.attach(9, tx);
        bus.subscribe(9, &names(&["test:a"]), &cursors(&[("test:a", 0)]));
        bus.drop_client(9);
        // Its forwarder has stopped: it dropped the only sender, so the channel ends.
        assert_eq!(next_line(&mut rx).await, None);
        // Nothing is queued for it any more, and publishing goes on.
        assert_eq!(bus.publish("test:a", json!(1)), 1);
        assert_eq!(bus.queued(9), 0);
        assert!(bus.unsubscribe(9, &names(&["test:a"])).is_empty());
        // Dropping an unknown client is harmless.
        bus.drop_client(9);
        bus.drop_client(10);
    }

    #[tokio::test] // spec: ARCH-032
    async fn forwarder_ends_when_the_outbound_channel_closes() {
        let (_clock, bus) = bus();
        let (tx, rx) = mpsc::channel(16);
        let watch = tx.clone();
        bus.attach(2, tx);
        drop(rx);
        // The forwarder notices the closed channel while idle and drops its sender.
        tokio::time::timeout(DEADLINE, async {
            while watch.strong_count() > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the forwarder did not end");
    }

    #[test] // spec: CORE-012, QA-031
    fn utc_jump_does_not_expire_history() {
        let (clock, bus) = bus();
        bus.publish("test:a", json!(1));
        clock.advance(Duration::from_secs(599));
        // The wall clock jumps back, then far ahead; only monotonic time counts.
        clock.shift_utc(-3600);
        bus.subscribe(1, &names(&["test:a"]), &cursors(&[("test:a", 0)]));
        assert_eq!(
            bus.queued_events(1),
            [event("test:a", 1, EventKind::Event, json!(1))]
        );
        clock.shift_utc(7200);
        bus.publish("test:b", json!(1));
        bus.subscribe(2, &names(&["test:a"]), &cursors(&[("test:a", 0)]));
        assert_eq!(
            bus.queued_events(2),
            [event("test:a", 1, EventKind::Event, json!(1))]
        );
        // Past 600 s of monotonic time it is gone.
        clock.advance(Duration::from_secs(2));
        bus.subscribe(3, &names(&["test:a"]), &cursors(&[("test:a", 0)]));
        assert_eq!(
            bus.queued_events(3),
            [event("test:a", 1, EventKind::ResyncRequired, Value::Null)]
        );
    }
}
