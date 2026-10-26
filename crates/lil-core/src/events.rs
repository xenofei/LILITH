//! The event bus (ARCH-032, CORE-012) behind `lil/subscribe`, `lil/unsubscribe` and the
//! `lil/event` notifications. Infrastructure, not a CORE-014 module.
//!
//! - **Sequences.** Each topic's `seq` goes up by one per [`EventBus::publish`], from 1 (or,
//!   for a topic re-created after it was forgotten, from its tombstone or the base below).
//! - **History.** Each topic keeps its events for [`HISTORY_SECS`] of *monotonic* time (a
//!   wall-clock jump changes nothing) and at most its [`HISTORY_MAX_EVENTS`] newest events
//!   (ARCH-032 clarification), pruned on publish and on subscribe (CORE-012). A `preview:`
//!   topic keeps only its newest event ([`PREVIEW_HISTORY`]): a late subscriber wants only the
//!   newest frame. A publish also prunes every topic once per [`SWEEP_EVERY`], so a topic
//!   nothing publishes to any more (a finished job's) holds nothing once its events are
//!   [`HISTORY_SECS`] old; a topic's seq is kept, so cursors stay valid. A per-job topic
//!   (`job:`, `preview:`) no client follows is forgotten once empty, its seq kept as a
//!   tombstone ([`TOMBSTONES`] at most): published or subscribed again, it goes on from there.
//!   A topic with no entry and no tombstone starts above every seq no longer kept, so an old
//!   cursor of it gets a snapshot or `resync_required`, never a replay (Appendix G "Events").
//! - **Subscribing with a cursor `s`.** `s` equal to the topic's current seq replays nothing;
//!   a history that still holds `s+1..=current` replays those events in order; anything else
//!   (older than the history, or beyond the current seq, such as a cursor from an earlier
//!   core) gets one `snapshot` from the [`SnapshotProvider`] with the longest matching prefix,
//!   or else one `resync_required`. No cursor means live events only. A topic the client
//!   already subscribes to ignores its cursor, and a replay that overflows the queue stops
//!   (the resyncs cover it).
//! - **Snapshots** are computed outside every lock ([`EventBus::subscribe`]):
//!   1. under the bus lock, the topics are recorded, the replays the history covers are
//!      queued, and each topic that needs a snapshot is marked *pending* for the client with
//!      `s0`, its current seq. A pending topic's live events stay in the history and are not
//!      queued;
//!   2. each provider is awaited with no lock held;
//!   3. under the bus lock again, if the client still subscribes to the topic (and it is still
//!      pending from this call), the snapshot is queued at seq `s0`, then the history's events
//!      after `s0` (or `resync_required` when the history no longer holds them all), and the
//!      topic stops being pending.
//! - **Delivery.** Each client has its own queue of at most [`CLIENT_QUEUE`] events, separate
//!   from the connection's outbound channel that responses share. A forwarder task per client
//!   moves events from the queue into that channel in order, as `lil/event` lines; only it
//!   ever waits on a slow client. When [`EventBus::publish`] finds a client's queue full, it
//!   clears the queue and queues one `resync_required` per topic the client subscribes to, at
//!   that topic's current seq. Responses are never dropped: only the bus queue overflows.
//! - **Line cap** (ARCH-025). Each event's line is encoded once, when it is published (or
//!   when a snapshot is taken), and shared by every queue and the history. A line that would
//!   pass 4 MiB is replaced there by `resync_required` for its topic and seq, and logged.
//! - **Forwarder lifetime.** A forwarder ends when its client is dropped
//!   ([`EventBus::drop_client`], from `Router::closed`), re-attached, or the bus is dropped,
//!   and when the outbound channel closes.
//!
//! **Locks.** One `std` mutex guards the topics and the clients; each client's queue has its
//! own, taken after the bus lock when both are needed. Nothing is awaited while either is
//! held: a [`SnapshotProvider`] runs with no lock held (it may read the workspace, and a
//! publish to any client goes on meanwhile), and the events published while it ran follow
//! its snapshot in order.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lil_platform::Clock;
use lil_protocol::{
    CLIENT_QUEUE, EventKind, EventParams, HISTORY_MAX_EVENTS, HISTORY_SECS, TopicState,
    encode_notification, fits_line, method,
};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

/// The history a `preview:` topic keeps (Appendix G "Events").
pub const PREVIEW_HISTORY: usize = 1;
/// How often a publish prunes every topic's history (Appendix G "Events").
pub const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// How many forgotten per-job topics keep their seq as a tombstone (Appendix G "Events").
pub const TOMBSTONES: usize = 4096;

/// A client of the bus: the connection's `ConnCtx::conn_id`.
pub type ClientId = u64;

/// Gives the current state of a topic, for a subscriber whose cursor the history no longer
/// covers. It runs with no bus lock held (module docs).
pub trait SnapshotProvider: Send + Sync {
    fn snapshot<'a>(&'a self, topic: &'a str) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>>;
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
    /// The last token handed to a pending snapshot.
    tokens: u64,
    /// When a publish last pruned every topic (monotonic).
    swept: Duration,
    /// The seqs of the per-job topics the sweep forgot.
    forgotten: Forgotten,
}

/// The seqs of forgotten per-job topics, as tombstones (re-review fix I-1): a topic published
/// or subscribed again continues its numbering from its tombstone, so an old cursor of it is
/// judged against its real seq. The newest [`TOMBSTONES`] are kept; `base` is the highest seq
/// of those no longer kept, and a topic with neither a live entry nor a tombstone starts its
/// numbering there, so a cursor of a topic whose tombstone is gone is always older than its
/// history (a snapshot or `resync_required`), never a replay that skips events.
#[derive(Default)]
struct Forgotten {
    /// Topic → (its seq, the generation of its entry in `order`).
    seqs: HashMap<String, (u64, u64)>,
    /// Forgotten topics, oldest first, with their generation; an entry whose generation
    /// `seqs` no longer holds is stale and skipped.
    order: VecDeque<(String, u64)>,
    generation: u64,
    base: u64,
    /// How many are kept; 0 means [`TOMBSTONES`] (a test sets fewer).
    cap: usize,
}

impl Forgotten {
    fn cap(&self) -> usize {
        if self.cap == 0 { TOMBSTONES } else { self.cap }
    }

    /// Keeps `topic`'s `seq`; the oldest tombstone beyond the cap goes, raising `base`.
    fn keep(&mut self, topic: String, seq: u64) {
        self.generation += 1;
        self.seqs.insert(topic.clone(), (seq, self.generation));
        self.order.push_back((topic, self.generation));
        while self.seqs.len() > self.cap() {
            let Some((oldest, generation)) = self.order.pop_front() else {
                break;
            };
            if self
                .seqs
                .get(&oldest)
                .is_some_and(|(_, g)| *g == generation)
                && let Some((seq, _)) = self.seqs.remove(&oldest)
            {
                self.base = self.base.max(seq);
            }
        }
        // Stale entries of revived topics don't pile up.
        if self.order.len() > 2 * self.cap() {
            let seqs = &self.seqs;
            self.order
                .retain(|(name, g)| seqs.get(name).is_some_and(|(_, kept)| kept == g));
        }
    }

    /// The seq a topic without a live entry starts from: its tombstone (taken), else `base`.
    fn take(&mut self, topic: &str) -> u64 {
        self.seqs.remove(topic).map_or(self.base, |(seq, _)| seq)
    }
}

/// `topic`'s live entry, made from its tombstone or the base when it has none.
fn live<'t>(
    topics: &'t mut HashMap<String, Topic>,
    forgotten: &mut Forgotten,
    topic: &str,
) -> &'t mut Topic {
    topics.entry(topic.to_string()).or_insert_with(|| Topic {
        seq: forgotten.take(topic),
        history: VecDeque::new(),
    })
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
    /// The event's `lil/event` line, shared with every queue it is in.
    line: Arc<str>,
}

/// A snapshot [`EventBus::subscribe`] computes outside the lock.
struct Wanted {
    topic: String,
    /// The topic's seq when the subscription began: the snapshot's label.
    s0: u64,
    /// Matches the client's pending mark only if nothing unsubscribed it meanwhile.
    token: u64,
    provider: Arc<dyn SnapshotProvider>,
}

/// One queued `lil/event`.
#[derive(Clone)]
struct Queued {
    topic: Arc<str>,
    line: Arc<str>,
}

struct Client {
    topics: BTreeSet<String>,
    /// Topics whose snapshot a `subscribe` is computing, with that call's token: their live
    /// events wait in the history until the snapshot is queued.
    snapshots: HashMap<String, u64>,
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
            snapshots: HashMap::new(),
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
        debug_assert!(lil_protocol::is_valid_topic(topic), "{topic:?}");
        let now = self.clock.monotonic();
        let mut guard = self.lock();
        let inner = &mut *guard;
        let log = live(&mut inner.topics, &mut inner.forgotten, topic);
        log.seq += 1;
        let seq = log.seq;
        let line = event_line(topic, seq, EventKind::Event, data);
        log.history.push_back(Retained {
            at: now,
            seq,
            line: line.clone(),
        });
        prune(&mut log.history, now, history_max(topic));
        if now.saturating_sub(inner.swept) >= SWEEP_EVERY {
            inner.swept = now;
            for (name, other) in &mut inner.topics {
                prune(&mut other.history, now, history_max(name));
            }
            // A per-job topic no client follows is forgotten once it is empty: a finished
            // job's topics would otherwise stay for the core's lifetime (review fix M-4). Its
            // seq stays as a tombstone (re-review fix I-1).
            let (clients, forgotten) = (&inner.clients, &mut inner.forgotten);
            inner.topics.retain(|name, t| {
                let forget = t.history.is_empty()
                    && (name.starts_with("job:") || name.starts_with("preview:"))
                    && !clients.values().any(|c| c.topics.contains(name));
                if forget {
                    forgotten.keep(name.clone(), t.seq);
                }
                !forget
            });
        }

        let item = Queued {
            topic: Arc::from(topic),
            line,
        };
        for (id, client) in &inner.clients {
            if client.topics.contains(topic) && !client.snapshots.contains_key(topic) {
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
    /// `lil/event` lines. A forwarder attached earlier for the client is stopped. Must be
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
    ///
    /// A topic the client already subscribes to keeps its subscription: its cursor is ignored
    /// (no replay, snapshot or resync), so nothing it has queued or received is repeated. All
    /// new topics join the client's set before any replay is queued, so if a replay overflows
    /// the queue, the `resync_required` events cover every one of them; nothing more of the
    /// replay is queued after that. Snapshots are awaited with no lock held, then queued at the
    /// seq the topic had when this call began, followed by what was published meanwhile.
    pub async fn subscribe(
        &self,
        client: ClientId,
        topics: &[String],
        since: &BTreeMap<String, u64>,
    ) -> Vec<TopicState> {
        let (states, wanted) = self.subscribe_locked(client, topics, since);
        if wanted.is_empty() {
            return states;
        }
        // 2. The providers, with no lock held.
        let mut taken = Vec::with_capacity(wanted.len());
        for want in wanted {
            let data = want.provider.snapshot(&want.topic).await;
            taken.push((want, data));
        }
        // 3. Queue each snapshot the client still waits for, then what came after it.
        let mut guard = self.lock();
        let inner = &mut *guard;
        let Some(entry) = inner.clients.get_mut(&client) else {
            return states; // dropped meanwhile
        };
        let mut queue = entry.pending.lock();
        let mut overflowed = false;
        for (want, data) in taken {
            if entry.snapshots.get(&want.topic) != Some(&want.token) {
                continue; // unsubscribed (and maybe subscribed again) meanwhile
            }
            entry.snapshots.remove(&want.topic);
            if overflowed || !entry.topics.contains(&want.topic) {
                continue;
            }
            let shared: Arc<str> = Arc::from(want.topic.as_str());
            let mut items = vec![Queued {
                topic: shared.clone(),
                line: event_line(&want.topic, want.s0, EventKind::Snapshot, data),
            }];
            let log = inner.topics.get(&want.topic);
            let current = log.map_or(0, |l| l.seq);
            if current > want.s0 {
                let covered = log
                    .and_then(|l| l.history.front())
                    .is_some_and(|first| first.seq <= want.s0 + 1);
                if covered {
                    let history = log.map(|l| &l.history).into_iter().flatten();
                    items.extend(history.filter(|r| r.seq > want.s0).map(|r| Queued {
                        topic: shared.clone(),
                        line: r.line.clone(),
                    }));
                } else {
                    items.push(Queued {
                        topic: shared.clone(),
                        line: event_line(
                            &want.topic,
                            current,
                            EventKind::ResyncRequired,
                            Value::Null,
                        ),
                    });
                }
            }
            for item in items {
                if enqueue(&mut queue, item, &entry.topics, &inner.topics) {
                    tracing::info!(
                        client,
                        topic = want.topic.as_str(),
                        "a client's event queue overflowed after a snapshot; it gets resync_required"
                    );
                    overflowed = true;
                    break;
                }
            }
        }
        drop(queue);
        entry.pending.wake.notify_one();
        states
    }

    /// [`EventBus::subscribe`]'s first step, under the bus lock: the states, the replays the
    /// history covers (queued), and the snapshots to compute (their topics marked pending).
    fn subscribe_locked(
        &self,
        client: ClientId,
        topics: &[String],
        since: &BTreeMap<String, u64>,
    ) -> (Vec<TopicState>, Vec<Wanted>) {
        let now = self.clock.monotonic();
        let mut guard = self.lock();
        let inner = &mut *guard;
        let entry = inner.clients.entry(client).or_insert_with(Client::new);
        // 1. Prune, report and subscribe; collect the topics that are new to the client.
        let mut states: Vec<TopicState> = Vec::with_capacity(topics.len());
        let mut fresh: Vec<&String> = Vec::new();
        for topic in topics {
            if states.iter().any(|s| &s.topic == topic) {
                continue;
            }
            // A topic without a live entry gets one, from its tombstone or the base: its
            // cursor is then judged against its real seq (re-review fix I-1).
            let log = live(&mut inner.topics, &mut inner.forgotten, topic);
            prune(&mut log.history, now, history_max(topic));
            let current = log.seq;
            states.push(TopicState {
                topic: topic.clone(),
                seq: current,
            });
            if entry.topics.insert(topic.clone()) {
                fresh.push(topic);
            }
        }
        // 2. Replay from the cursors of the new topics, until the queue overflows; mark the
        // topics that need a snapshot as pending.
        let mut wanted = Vec::new();
        let mut queue = entry.pending.lock();
        'topics: for topic in fresh {
            let Some(&cursor) = since.get(topic) else {
                continue; // live events only
            };
            let log = inner.topics.get(topic);
            let current = log.map_or(0, |l| l.seq);
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
                match provider {
                    Some((_, p)) => {
                        inner.tokens += 1;
                        entry.snapshots.insert(topic.clone(), inner.tokens);
                        wanted.push(Wanted {
                            topic: topic.clone(),
                            s0: current,
                            token: inner.tokens,
                            provider: p.clone(),
                        });
                    }
                    None => items.push(Queued {
                        topic: shared_topic,
                        line: event_line(topic, current, EventKind::ResyncRequired, Value::Null),
                    }),
                }
            }
            for item in items {
                if enqueue(&mut queue, item, &entry.topics, &inner.topics) {
                    tracing::info!(
                        client,
                        topic = topic.as_str(),
                        "a client's event queue overflowed during a replay; it gets resync_required"
                    );
                    // The resyncs cover every subscribed topic, this call's included.
                    break 'topics;
                }
            }
        }
        drop(queue);
        entry.pending.wake.notify_one();
        (states, wanted)
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
        for topic in &removed {
            entry.snapshots.remove(topic);
        }
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

    /// How many clients the bus knows (attached or subscribed, not yet dropped).
    pub fn clients(&self) -> usize {
        self.lock().clients.len()
    }

    /// How many events wait in `client`'s queue (not yet taken by its forwarder).
    pub fn queued(&self, client: ClientId) -> usize {
        let inner = self.lock();
        inner
            .clients
            .get(&client)
            .map_or(0, |c| c.pending.lock().len())
    }

    /// Keeps at most `n` tombstones (a test of their eviction).
    #[cfg(test)]
    fn keep_tombstones(&self, n: usize) {
        self.lock().forgotten.cap = n;
    }

    /// Whether the bus still keeps `topic` (its seq and history).
    #[cfg(test)]
    fn knows(&self, topic: &str) -> bool {
        self.lock().topics.contains_key(topic)
    }

    /// How many events `topic`'s history holds now.
    #[cfg(test)]
    fn retained(&self, topic: &str) -> usize {
        self.lock().topics.get(topic).map_or(0, |t| t.history.len())
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

/// The most events `topic`'s history keeps: [`PREVIEW_HISTORY`] for a `preview:` topic, else
/// [`HISTORY_MAX_EVENTS`].
fn history_max(topic: &str) -> usize {
    if topic.starts_with("preview:") {
        PREVIEW_HISTORY
    } else {
        HISTORY_MAX_EVENTS
    }
}

/// Drops the events older than [`HISTORY_SECS`] of monotonic time at `now`, then the oldest
/// beyond `max`.
fn prune(history: &mut VecDeque<Retained>, now: Duration, max: usize) {
    let keep = Duration::from_secs(HISTORY_SECS);
    while history
        .front()
        .is_some_and(|r| now.saturating_sub(r.at) > keep)
    {
        history.pop_front();
    }
    while history.len() > max {
        history.pop_front();
    }
}

/// The `lil/event` notification line of one event. A line that would pass the 4 MiB cap
/// (ARCH-025) is never sent: it is logged and replaced by `resync_required` for the same topic
/// and seq, so the subscriber knows it missed something.
fn event_line(topic: &str, seq: u64, kind: EventKind, data: Value) -> Arc<str> {
    let encode = |kind: EventKind, data: Value| {
        let params = EventParams {
            topic: topic.to_string(),
            seq,
            kind,
            data,
        };
        let params = serde_json::to_value(&params).expect("EventParams serialise");
        encode_notification(method::EVENT, &params)
    };
    let line = encode(kind, data);
    if fits_line(&line) {
        return Arc::from(line);
    }
    tracing::warn!(
        topic,
        seq,
        bytes = line.len(),
        "an event was too large to send; subscribers get resync_required instead"
    );
    Arc::from(encode(EventKind::ResyncRequired, Value::Null))
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

    use lil_protocol::{CLIENT_QUEUE, EventKind, EventParams, method};
    use lil_testkit::FakeClock;
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

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

    /// The `lil/event` params of an outbound line.
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
        fn snapshot<'a>(
            &'a self,
            topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
            Box::pin(async move { json!({"provider": self.0, "topic": topic}) })
        }
    }

    #[tokio::test] // spec: ARCH-032
    async fn seq_is_per_topic_and_monotonic() {
        let (_clock, bus) = bus();
        assert_eq!(bus.publish("test:a", json!(1)), 1);
        assert_eq!(bus.publish("test:a", json!(2)), 2);
        assert_eq!(bus.publish("test:b", json!(1)), 1);
        assert_eq!(bus.publish("test:a", json!(3)), 3);
        assert_eq!(bus.publish("test:b", json!(2)), 2);
        // `lil/subscribe` reports each topic's current seq; a topic never published is at 0.
        let states = bus
            .subscribe(1, &names(&["test:a", "test:b", "jobs"]), &cursors(&[]))
            .await;
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

    #[tokio::test] // spec: ARCH-032, CORE-012
    async fn replay_from_cursor() {
        let (clock, bus) = bus();
        for n in 1..=3 {
            bus.publish("test:a", json!({"n": n}));
            clock.advance(Duration::from_secs(10));
        }
        // A cursor inside the history replays what came after it, in order.
        let states = bus
            .subscribe(1, &names(&["test:a"]), &cursors(&[("test:a", 1)]))
            .await;
        assert_eq!(states, [state("test:a", 3)]);
        assert_eq!(
            bus.queued_events(1),
            [
                event("test:a", 2, EventKind::Event, json!({"n": 2})),
                event("test:a", 3, EventKind::Event, json!({"n": 3})),
            ]
        );
        // A cursor of 0 replays everything retained.
        bus.subscribe(2, &names(&["test:a"]), &cursors(&[("test:a", 0)]))
            .await;
        let seqs: Vec<u64> = bus.queued_events(2).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2, 3]);
        // A cursor at the current seq replays nothing; neither does no cursor.
        bus.subscribe(3, &names(&["test:a"]), &cursors(&[("test:a", 3)]))
            .await;
        bus.subscribe(4, &names(&["test:a"]), &cursors(&[])).await;
        assert_eq!(bus.queued(3), 0);
        assert_eq!(bus.queued(4), 0);
        // Live events follow the replay.
        bus.publish("test:a", json!({"n": 4}));
        let seqs: Vec<u64> = bus.queued_events(1).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [2, 3, 4]);
        assert_eq!(bus.queued(3), 1);
        assert_eq!(bus.queued(4), 1);
    }

    #[tokio::test] // spec: ARCH-032, CORE-012
    async fn cursor_older_than_ten_minutes_gets_snapshot() {
        let (clock, bus) = bus();
        bus.register_snapshot("test:", Arc::new(Fixed("test")));
        bus.register_snapshot("test:a", Arc::new(Fixed("test:a")));
        bus.publish("test:a", json!(1));
        bus.publish("test:a", json!(2));
        bus.publish("test:b", json!(1));
        clock.advance(Duration::from_secs(601));
        // The history no longer covers seq 2: one snapshot, at the current seq, from the
        // provider with the longest matching prefix.
        let states = bus
            .subscribe(
                1,
                &names(&["test:a", "test:b"]),
                &cursors(&[("test:a", 1), ("test:b", 0)]),
            )
            .await;
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

    #[tokio::test] // spec: ARCH-032, CORE-012
    async fn cursor_older_without_provider_gets_resync_required() {
        let (clock, bus) = bus();
        bus.register_snapshot("other:", Arc::new(Fixed("other")));
        bus.publish("jobs", json!(1));
        bus.publish("jobs", json!(2));
        clock.advance(Duration::from_secs(601));
        bus.publish("jobs", json!(3));
        // Seq 3 is retained, seq 2 is not: the cursor 1 is not covered.
        bus.subscribe(1, &names(&["jobs"]), &cursors(&[("jobs", 1)]))
            .await;
        assert_eq!(
            bus.queued_events(1),
            [event("jobs", 3, EventKind::ResyncRequired, Value::Null)]
        );
        // The cursor 2 is: seq 3 is replayed.
        bus.subscribe(2, &names(&["jobs"]), &cursors(&[("jobs", 2)]))
            .await;
        assert_eq!(
            bus.queued_events(2),
            [event("jobs", 3, EventKind::Event, json!(3))]
        );
    }

    #[tokio::test] // spec: ARCH-032
    async fn cursor_from_the_future_gets_resync_required() {
        let (_clock, bus) = bus();
        bus.publish("test:a", json!(1));
        // A cursor from an earlier core instance: beyond the current seq.
        let states = bus
            .subscribe(
                1,
                &names(&["test:a", "test:new"]),
                &cursors(&[("test:a", 5), ("test:new", 2)]),
            )
            .await;
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
        bus.subscribe(7, &names(&["test:a", "test:b"]), &cursors(&[]))
            .await;
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
        bus.subscribe(3, &names(&["test:a"]), &cursors(&[])).await;
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
        bus.subscribe(5, &names(&["test:a", "test:b"]), &cursors(&[]))
            .await;
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

    #[tokio::test] // spec: ARCH-025, ARCH-032
    async fn oversized_event_becomes_resync_required() {
        let (_clock, bus) = bus();
        let (tx, mut rx) = mpsc::channel(16);
        bus.attach(6, tx);
        bus.subscribe(6, &names(&["test:a"]), &cursors(&[])).await;

        // An event whose line would pass the 4 MiB cap is never sent: the subscriber gets
        // resync_required for its topic and seq instead, and later events flow as usual.
        let seq = bus.publish("test:a", json!("x".repeat(lil_protocol::MAX_LINE_BYTES)));
        let line = next_line(&mut rx).await.unwrap();
        assert!(lil_protocol::fits_line(&line), "{} bytes", line.len());
        assert_eq!(
            parse_line(&line),
            event("test:a", seq, EventKind::ResyncRequired, Value::Null)
        );
        bus.publish("test:a", json!("small"));
        assert_eq!(
            parse_line(&next_line(&mut rx).await.unwrap()),
            event("test:a", seq + 1, EventKind::Event, json!("small"))
        );

        // A replay from the history gets the same replacement.
        bus.subscribe(7, &names(&["test:a"]), &cursors(&[("test:a", 0)]))
            .await;
        assert_eq!(
            bus.queued_events(7),
            [
                event("test:a", seq, EventKind::ResyncRequired, Value::Null),
                event("test:a", seq + 1, EventKind::Event, json!("small")),
            ]
        );
    }

    #[tokio::test] // spec: ARCH-032
    async fn unsubscribe_drops_queued_events_of_removed_topics() {
        let (_clock, bus) = bus();
        bus.subscribe(1, &names(&["test:a", "test:b"]), &cursors(&[]))
            .await;
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
        bus.subscribe(9, &names(&["test:a"]), &cursors(&[("test:a", 0)]))
            .await;
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

    #[tokio::test] // spec: ARCH-032
    async fn replay_overflow_leaves_only_resync_required_once_per_topic() {
        let (_clock, bus) = bus();
        // The queue is 3 short of full with live `test:b` events.
        bus.subscribe(1, &names(&["test:b"]), &cursors(&[])).await;
        for n in 0..CLIENT_QUEUE - 3 {
            bus.publish("test:b", json!(n));
        }
        for n in 1..=10 {
            bus.publish("test:a", json!(n));
        }
        bus.publish("test:c", json!(1));
        bus.publish("test:c", json!(2));
        assert_eq!(bus.queued(1), CLIENT_QUEUE - 3);
        // Replaying ten `test:a` events overflows it on the fourth; the replay stops there,
        // and `test:c`, later in the same call, replays nothing either (P9).
        let states = bus
            .subscribe(
                1,
                &names(&["test:a", "test:c"]),
                &cursors(&[("test:a", 0), ("test:c", 0)]),
            )
            .await;
        assert_eq!(states, [state("test:a", 10), state("test:c", 2)]);
        assert_eq!(
            bus.queued_events(1),
            [
                event("test:a", 10, EventKind::ResyncRequired, Value::Null),
                event("test:b", 1021, EventKind::ResyncRequired, Value::Null),
                event("test:c", 2, EventKind::ResyncRequired, Value::Null),
            ]
        );
        // Live events queue normally behind the resyncs.
        bus.publish("test:a", json!(11));
        assert_eq!(bus.queued(1), 4);
    }

    #[tokio::test] // spec: ARCH-032
    async fn resubscribing_does_not_replay_a_topic_already_subscribed() {
        let (_clock, bus) = bus();
        bus.subscribe(1, &names(&["test:a"]), &cursors(&[])).await;
        for n in 1..=3 {
            bus.publish("test:a", json!(n));
        }
        bus.publish("test:b", json!(1));
        assert_eq!(bus.queued(1), 3);
        // `test:a` is subscribed already: its cursor (and a future one) replays nothing, but its
        // state is reported. `test:b` is new and replays from its cursor.
        let states = bus
            .subscribe(
                1,
                &names(&["test:a", "test:b"]),
                &cursors(&[("test:a", 0), ("test:b", 0)]),
            )
            .await;
        assert_eq!(states, [state("test:a", 3), state("test:b", 1)]);
        let seen: Vec<(String, u64, EventKind)> = bus
            .queued_events(1)
            .into_iter()
            .map(|e| (e.topic, e.seq, e.kind))
            .collect();
        assert_eq!(
            seen,
            [
                ("test:a".to_string(), 1, EventKind::Event),
                ("test:a".to_string(), 2, EventKind::Event),
                ("test:a".to_string(), 3, EventKind::Event),
                ("test:b".to_string(), 1, EventKind::Event),
            ]
        );
        let states = bus
            .subscribe(1, &names(&["test:a"]), &cursors(&[("test:a", 99)]))
            .await;
        assert_eq!(states, [state("test:a", 3)]);
        assert_eq!(bus.queued(1), 4);
    }

    /// Checkpoint A I1: a topic nothing publishes to any more is pruned by the sweep a publish
    /// on another topic runs; its seq stays.
    #[tokio::test] // spec: ARCH-032, CORE-012
    async fn a_quiet_topics_history_is_swept_by_a_publish_elsewhere() {
        let (clock, bus) = bus();
        for n in 0..5 {
            bus.publish("test:quiet", json!(n));
        }
        assert_eq!(bus.retained("test:quiet"), 5);
        clock.advance(Duration::from_secs(HISTORY_SECS + 1));
        bus.publish("jobs", json!("elsewhere"));
        assert_eq!(
            bus.retained("test:quiet"),
            0,
            "swept without a publish of its own"
        );
        assert_eq!(bus.retained("jobs"), 1);
        let states = bus
            .subscribe(1, &names(&["test:quiet"]), &cursors(&[]))
            .await;
        assert_eq!(states, [state("test:quiet", 5)], "the seq is kept");
        assert_eq!(bus.publish("test:quiet", json!(5)), 6);
    }

    /// Review M-4: a per-job topic (`job:`, `preview:`) that no client follows is forgotten
    /// once the sweep empties it; one a client follows, and any other topic, keeps its seq.
    #[tokio::test] // spec: ARCH-032, CORE-012
    async fn an_emptied_per_job_topic_nobody_follows_is_forgotten() {
        let (clock, bus) = bus();
        bus.subscribe(1, &names(&["job:j_2"]), &cursors(&[])).await;
        for topic in ["job:j_1", "preview:j_1", "job:j_2", "test:other"] {
            bus.publish(topic, json!(1));
        }
        clock.advance(Duration::from_secs(HISTORY_SECS + 1));
        bus.publish("jobs", json!("elsewhere"));
        assert!(!bus.knows("job:j_1"));
        assert!(!bus.knows("preview:j_1"));
        assert!(bus.knows("job:j_2"), "a client follows it");
        assert!(bus.knows("test:other"), "not a per-job topic");
        // Its seq stays as a tombstone: a cursor at it has missed nothing, one ahead of it is
        // resynced (re-review fix I-1).
        let states = bus
            .subscribe(2, &names(&["job:j_1"]), &cursors(&[("job:j_1", 1)]))
            .await;
        assert_eq!(states, [state("job:j_1", 1)]);
        assert!(bus.queued_events(2).is_empty());
        bus.subscribe(3, &names(&["preview:j_1"]), &cursors(&[("preview:j_1", 7)]))
            .await;
        assert_eq!(
            bus.queued_events(3),
            [event(
                "preview:j_1",
                1,
                EventKind::ResyncRequired,
                Value::Null
            )]
        );
    }

    /// Re-review I-1: once a forgotten topic's tombstone is evicted, an old cursor of it gets
    /// `resync_required`, never a replay: while the topic isn't live, after it is published
    /// again (its numbering starts above every evicted seq), and for a topic never seen.
    #[tokio::test] // spec: ARCH-032, CORE-012
    async fn an_evicted_tombstone_falls_back_to_resync() {
        let (clock, bus) = bus();
        bus.keep_tombstones(1);
        let sweep = || {
            clock.advance(Duration::from_secs(HISTORY_SECS + 1));
            bus.publish("jobs", json!("elsewhere"));
        };
        for n in 1..=3 {
            bus.publish("job:j_1", json!(n));
        }
        sweep();
        bus.publish("job:j_2", json!(1));
        sweep(); // j_2's tombstone evicts j_1's
        let resynced = |client: ClientId, topic: &str, seq: u64| {
            assert_eq!(
                bus.queued_events(client),
                [event(topic, seq, EventKind::ResyncRequired, Value::Null)],
                "client {client}"
            );
        };
        // Not live, no tombstone.
        bus.subscribe(1, &names(&["job:j_1"]), &cursors(&[("job:j_1", 2)]))
            .await;
        resynced(1, "job:j_1", 3);
        bus.drop_client(1);
        // Published again: on from the evicted seqs, so the old cursor is older than it.
        for n in 1..=2 {
            bus.publish("job:j_1", json!({"new": n}));
        }
        bus.subscribe(2, &names(&["job:j_1"]), &cursors(&[("job:j_1", 2)]))
            .await;
        resynced(2, "job:j_1", 5);
        // A cursor at the old end has missed nothing: it gets the new events.
        bus.subscribe(3, &names(&["job:j_1"]), &cursors(&[("job:j_1", 3)]))
            .await;
        let seqs: Vec<u64> = bus.queued_events(3).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [4, 5]);
        // A topic never seen starts at the base too.
        bus.subscribe(4, &names(&["job:j_9"]), &cursors(&[("job:j_9", 1)]))
            .await;
        resynced(4, "job:j_9", 3);
    }

    /// Re-review I-1 (the reviewer's probe): a client follows `job:j_1` to seq 3 and leaves;
    /// the topic is forgotten; the job then publishes five events. A client coming back with
    /// cursor 3 gets all five, numbered on from 3, never just the tail of a new numbering.
    #[tokio::test] // spec: ARCH-032, CORE-012
    async fn a_returning_cursor_of_a_forgotten_topic_misses_nothing() {
        let (clock, bus) = bus();
        bus.subscribe(1, &names(&["job:j_1"]), &cursors(&[])).await;
        for n in 1..=3 {
            bus.publish("job:j_1", json!({"old": n}));
        }
        bus.drop_client(1);
        clock.advance(Duration::from_secs(HISTORY_SECS + 1));
        bus.publish("jobs", json!("elsewhere"));
        assert!(!bus.knows("job:j_1"), "forgotten");
        for n in 1..=5 {
            bus.publish("job:j_1", json!({"new": n}));
        }
        let states = bus
            .subscribe(2, &names(&["job:j_1"]), &cursors(&[("job:j_1", 3)]))
            .await;
        assert_eq!(states, [state("job:j_1", 8)]);
        let got: Vec<(u64, Value)> = bus
            .queued_events(2)
            .into_iter()
            .map(|e| (e.seq, e.data))
            .collect();
        let want: Vec<(u64, Value)> = (1..=5).map(|n| (3 + n, json!({"new": n}))).collect();
        assert_eq!(got, want);
    }

    /// Checkpoint A I1: a `preview:<job_id>` topic keeps only its newest frame.
    #[tokio::test] // spec: ARCH-032
    async fn a_preview_topic_keeps_only_its_newest_frame() {
        let (_clock, bus) = bus();
        for n in 1..=3 {
            bus.publish("preview:j_1", json!({"n": n}));
        }
        assert_eq!(bus.retained("preview:j_1"), 1);
        bus.subscribe(1, &names(&["preview:j_1"]), &cursors(&[("preview:j_1", 2)]))
            .await;
        assert_eq!(
            bus.queued_events(1),
            [event("preview:j_1", 3, EventKind::Event, json!({"n": 3}))]
        );
        // Other topics keep their whole history.
        for n in 1..=3 {
            bus.publish("job:j_1", json!(n));
        }
        assert_eq!(bus.retained("job:j_1"), 3);
    }

    #[tokio::test] // spec: CORE-012, QA-031
    async fn utc_jump_does_not_expire_history() {
        let (clock, bus) = bus();
        bus.publish("test:a", json!(1));
        clock.advance(Duration::from_secs(599));
        // The wall clock jumps back, then far ahead; only monotonic time counts.
        clock.shift_utc(-3600);
        bus.subscribe(1, &names(&["test:a"]), &cursors(&[("test:a", 0)]))
            .await;
        assert_eq!(
            bus.queued_events(1),
            [event("test:a", 1, EventKind::Event, json!(1))]
        );
        clock.shift_utc(7200);
        bus.publish("test:b", json!(1));
        bus.subscribe(2, &names(&["test:a"]), &cursors(&[("test:a", 0)]))
            .await;
        assert_eq!(
            bus.queued_events(2),
            [event("test:a", 1, EventKind::Event, json!(1))]
        );
        // Past 600 s of monotonic time it is gone.
        clock.advance(Duration::from_secs(2));
        bus.subscribe(3, &names(&["test:a"]), &cursors(&[("test:a", 0)]))
            .await;
        assert_eq!(
            bus.queued_events(3),
            [event("test:a", 1, EventKind::ResyncRequired, Value::Null)]
        );
    }
}
