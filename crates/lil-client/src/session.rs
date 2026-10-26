//! An app's session with the core (APP-024, APP-033–036, ARCH-100): it stays connected across
//! core restarts, resubscribes its topics and merges their events into one stream. It runs on
//! the tokio runtime it was started on, which an app runs off its GUI thread (APP-024).
//!
//! One task owns the connection and the merged event stream:
//! - **Connecting.** [`Client::connect`] finds or starts the core as the configuration says,
//!   with the configuration's caller kind and workspace; the session's topics are then
//!   subscribed, and the state becomes [`SessionState::Ready`] with the core's hello.
//! - **Backoff** (P4a-R33). After a failed attempt, or once the connection is lost, the task
//!   waits 250 ms before the next attempt, doubling up to 5 s, and keeps trying until the
//!   session is closed. Meanwhile the state says why: [`SessionState::Reconnecting`] while no
//!   core answers for now (none runs, the connection closed, no answer in time);
//!   [`SessionState::Incompatible`] for a core of another major protocol, which is left
//!   running (ARCH-014, APP-036); [`SessionState::Unavailable`] when no core can be used (it
//!   can't start, isn't trusted, or refused the hello). A `lil/subscribe` gets at most the
//!   configuration's `start_hard_cap` for its answer; a core that takes longer counts as one
//!   that didn't answer in time, and its connection is dropped.
//! - **After a failed start** (a core that exited with a start failure, or a core executable
//!   that could not be launched), the attempts only look for a core someone else started
//!   ([`StartPolicy::NoStart`]): such a failure needs a person (`lil doctor`, CORE-004), and a
//!   core launched every 5 s would get in the way of the fix. While none is found, the state
//!   stays [`SessionState::Unavailable`] with the start failure. Starting is allowed again
//!   after the next connection, on [`Session::set_workspace`], and on [`Session::retry_now`]
//!   (the banner's "Try again"). A retry asked for during an attempt that then fails to start
//!   a core is that start's retry, not a second start. After any other failure of the attempt
//!   (a core that doesn't answer, or a start that timed out), the retry is kept and the next
//!   attempt follows at once, with no backoff, and may start a core.
//! - **Resubscribing** (APP-035, ARCH-100). Each topic keeps its last seq: the seq of the
//!   newest event the session took from it, else its seq when it was subscribed. Every
//!   connection after the first subscribes the topics with `since_seq` from their last seqs,
//!   so the core replays what was missed, or sends a `snapshot` (or `resync_required`) when
//!   its history no longer holds it. Seqs count per core instance: when the hello names
//!   another `instance_id` than the last connection's (the core restarted), each last seq
//!   starts again from 0 first, so the new core replays everything it still holds instead of
//!   skipping its first events. A topic the new core hasn't published on yet then gets no
//!   snapshot, so the new `instance_id` in [`SessionState::Ready`] is the app's signal to
//!   reload everything it built from the old core (ARCH-100).
//! - **Events** reach [`Session::events`] in the core's order, across reconnects. A topic
//!   subscribed while there is no connection, or while one is being set up, is subscribed on
//!   it: the task makes the connection the current one, then subscribes the topics added
//!   meanwhile and unsubscribes the ones removed meanwhile, until the session's topics and the
//!   connection's agree. [`Session::subscribe`] and [`Session::unsubscribe`] change the
//!   topics and send the change on the current connection one call at a time (the task's
//!   catch-up included), so the core ends with what the last call left; a call that comes in
//!   meanwhile waits its turn, within the same `start_hard_cap` as the call itself.
//! - **Events of an ended subscription** (a topic unsubscribed and then subscribed again) are
//!   dropped where the session can tell them: those the connection had received when the
//!   unsubscribe was answered, those of a subscription the setup ended, and, as every topic's
//!   seqs only rise within a connection, any event at or below the topic's last seq (what the
//!   newest subscribe was answered with, or the newest event taken). The core may send an
//!   event just before it answers the unsubscribe, and that one can still arrive in the short
//!   moment between a subscribe's edit of the topics and its answer. An app that loads a
//!   topic's state itself ignores the events at or below the seq it loaded at (APP-033).
//! - **A switch** ([`Session::set_workspace`]) or a lost connection leaves no current
//!   connection until the next one is established: the old one is no longer read, so calls
//!   fail with [`ClientError::NotConnected`] meanwhile and topics are recorded for the next
//!   one, which learns their cursors from its own answers only.
//! - **Calls** ([`Session::call_tool`], [`Session::app_call`]) go out on the current
//!   connection and the session never sends one again. A call while there is no connection is
//!   [`ClientError::NotConnected`] at once: nothing was sent, so it is safe to make again. A
//!   call in flight when the connection is lost ends with [`ClientError::Closed`]: it may have
//!   run. Retrying after `Closed` is the caller's, with the same idempotency key, so a caller
//!   that may retry passes its own ([`crate::mint_id`]`("ik")`); `app/*` methods have no key,
//!   so they are not retried after `Closed`.
//! - **Switching workspace** (P4a-R34) is a new connection whose hello names the workspace. It
//!   replaces the current one only once the core accepted it, and it never changes
//!   `suite.toml`'s default.
//! - **Topic names** that aren't topics (Appendix D's pattern) never reach the core, which
//!   would refuse the whole subscribe: [`Session::start`] leaves them out, and
//!   [`Session::subscribe`] refuses them.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lil_protocol::codes::PARAMS_INVALID;
use lil_protocol::{
    CLIENT_QUEUE, ErrorBody, EventKind, EventParams, HelloResult, Outcome, ToolOutput, TopicState,
    is_valid_topic,
};
use serde_json::Value;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::{Client, ClientConfig, ClientError, StartPolicy};

/// The first pause before another attempt.
const BACKOFF_FIRST: Duration = Duration::from_millis(250);
/// The longest pause between attempts.
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Events [`Session::events`] holds before the session keeps the rest itself.
const EVENTS_CAPACITY: usize = 1024;
/// Events the session keeps itself before it drops them for one `resync_required` per topic,
/// as the core does with a client's queue (ARCH-032).
const BACKLOG_MAX: usize = CLIENT_QUEUE;

/// Where the session stands (APP-034, APP-036, ARCH-100).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// The first attempt is under way.
    Connecting,
    /// Connected and subscribed; the core's hello. Another `instance_id` than the last
    /// `Ready`'s means the core restarted: reload what was built from the old one.
    Ready(Box<HelloResult>),
    /// No core answers for now; `attempt` counts the attempts since the last connection (or
    /// since the start), the next of which follows the backoff.
    Reconnecting { attempt: u32 },
    /// The core speaks another major protocol and was left running (ARCH-014).
    Incompatible { core_protocol: String },
    /// No core can be used, and why (the client's message).
    Unavailable { reason: String },
}

/// A session with the core (module docs). Dropping it stops its task and closes the
/// connection; [`Session::close`] does so and waits for the task.
pub struct Session {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

/// What the session's task and its methods share.
struct Shared {
    state: watch::Sender<SessionState>,
    /// The configuration of the next attempt; `set_workspace` changes its workspace.
    cfg: Mutex<ClientConfig>,
    /// The connection calls go out on, while there is one.
    client: Mutex<Option<Arc<Client>>>,
    /// The subscribed topics and their last seqs (`None`: not subscribed on a connection yet).
    topics: Mutex<BTreeMap<String, Option<u64>>>,
    /// [`Session::events`]' receiver, until it is taken.
    events: Mutex<Option<mpsc::Receiver<EventParams>>>,
    /// Held for the whole of a `set_workspace`, so the configuration and the adopted
    /// connection always name the same workspace.
    switching: tokio::sync::Mutex<()>,
    /// Held by each topic change from the edit of `topics` to the end of the matching call on
    /// the core, and by the task's catch-up on a new connection: the core sees the changes in
    /// the order they were made.
    ops: tokio::sync::Mutex<()>,
    /// What tells the events of an earlier subscription from the current one's.
    history: Mutex<History>,
}

/// What the session remembers about subscriptions that ended, so that their events are not
/// taken for the events of a later subscription to the same topic.
#[derive(Default)]
struct History {
    /// Topics removed while there was no connection: the one being set up may already have
    /// subscribed them (its first `lil/subscribe` is under way), and then they are subscribed
    /// there although the session removed them.
    removed: BTreeSet<String>,
    /// Per topic: the events of the current connection's stream up to this position (the
    /// stream's `n`th event is position `n`) belong to a subscription that has ended.
    stale: BTreeMap<String, u64>,
}

/// Every update is a single step, so the data stays consistent after a panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Command {
    /// Adopt this connection (its hello named another workspace), then say so.
    Switch(Box<Client>, oneshot::Sender<()>),
    /// Allow starting a core again, and try now.
    Retry,
    Close,
}

impl Session {
    /// Starts the session on the current tokio runtime, subscribed to `topics` (live events
    /// only: the app loads their current state itself, APP-033); a name that isn't a topic is
    /// left out. The state starts as [`SessionState::Connecting`].
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn start(cfg: ClientConfig, topics: Vec<String>) -> Session {
        let (events_tx, events_rx) = mpsc::channel(EVENTS_CAPACITY);
        let shared = Arc::new(Shared {
            state: watch::Sender::new(SessionState::Connecting),
            cfg: Mutex::new(cfg),
            client: Mutex::new(None),
            topics: Mutex::new(
                topics
                    .into_iter()
                    .filter(|t| is_valid_topic(t))
                    .map(|t| (t, None))
                    .collect(),
            ),
            events: Mutex::new(Some(events_rx)),
            switching: tokio::sync::Mutex::new(()),
            ops: tokio::sync::Mutex::new(()),
            history: Mutex::new(History::default()),
        });
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(
            shared.clone(),
            commands_rx,
            Outbox {
                tx: events_tx,
                backlog: VecDeque::new(),
            },
        ));
        Session {
            shared,
            commands,
            task,
        }
    }

    /// The state, and every change to it.
    pub fn state(&self) -> watch::Receiver<SessionState> {
        self.shared.state.subscribe()
    }

    /// `tools/call` on the current connection ([`Client::call_tool`]: without `key`, a fresh
    /// idempotency key is minted), carrying `trace_id` when given (ARCH-118). Never sent again
    /// by the session (module docs).
    ///
    /// [`ClientError::NotConnected`]: no connection, nothing was sent. [`ClientError::Closed`]:
    /// the connection was lost while the call was in flight, so it may have run and its
    /// outcome is unknown. A caller that may retry after `Closed` passes its own key
    /// ([`crate::mint_id`]`("ik")`) and retries with that key: a key minted here is never seen
    /// by the caller.
    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
        key: Option<String>,
        trace_id: Option<String>,
    ) -> Result<Outcome<ToolOutput>, ClientError> {
        self.client()?
            .call_tool_traced(name, args, key, trace_id)
            .await
    }

    /// An `app/*` method on the current connection ([`Client::app_call`]). These have no
    /// idempotency key: after [`ClientError::Closed`] the method may have run, and sending it
    /// again may run it twice. [`ClientError::NotConnected`] means nothing was sent.
    pub async fn app_call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Outcome<Value>, ClientError> {
        self.client()?.app_call(method, params).await
    }

    /// Adds `topics` (live events only). Without a connection, or while the first
    /// `lil/subscribe` of one being set up is under way, they are subscribed on it, and this
    /// returns once it has its turn. A name that isn't a topic is
    /// `params.invalid`, and so is a topic the core refuses: the error, and none of this call's
    /// topics stay; so it is when the core doesn't answer within `start_hard_cap`
    /// ([`ClientError::Timeout`]; the call may still have reached the core, which the session
    /// then has no events from).
    ///
    /// A topic the session already has is left as it is: no restart, and its cursor and last
    /// seq are kept.
    ///
    /// Calls on topics (this and [`Session::unsubscribe`]) take turns, in the order they get
    /// to the front, and wait for the one before: a call that finds the session's topic list
    /// first also gets to the core first, so the last call wins on both. The `start_hard_cap`
    /// counts from the call, waiting included.
    pub async fn subscribe(&self, topics: Vec<String>) -> Result<(), ClientError> {
        if let Some(bad) = topics.iter().find(|t| !is_valid_topic(t)) {
            return Err(ClientError::Refused(ErrorBody::new(
                &PARAMS_INVALID,
                format!("\"{bad}\" is not a topic name"),
                crate::mint_id("t"),
            )));
        }
        let deadline = Instant::now() + lock(&self.shared.cfg).start_hard_cap;
        let _turn = self.turn(deadline).await?;
        let added: Vec<String> = {
            let mut known = lock(&self.shared.topics);
            topics
                .into_iter()
                .filter(|topic| match known.entry(topic.clone()) {
                    // A topic the session has keeps its last seq.
                    Entry::Vacant(slot) => {
                        slot.insert(None);
                        true
                    }
                    Entry::Occupied(_) => false,
                })
                .collect()
        };
        if added.is_empty() {
            return Ok(());
        }
        let Some(client) = lock(&self.shared.client).clone() else {
            return Ok(());
        };
        // Topics the setup's first subscribe may have sent although the app removed them:
        // their earlier subscription ends here, as it would in the setup's catch-up, which
        // must not end the subscription made now.
        let ended: Vec<String> = {
            let mut history = lock(&self.shared.history);
            added
                .iter()
                .filter(|topic| history.removed.remove(*topic))
                .cloned()
                .collect()
        };
        let wire = async {
            if !ended.is_empty() {
                client.unsubscribe_only(ended.clone()).await?;
                mark_stale(&self.shared, &client, &ended);
            }
            client.subscribe_only(added.clone(), BTreeMap::new()).await
        };
        match by(deadline, wire).await {
            Ok(subscribed) => {
                note_if_current(&self.shared, &client, &subscribed.topics);
                Ok(())
            }
            // The connection went or was replaced: the next one subscribes them.
            Err(ClientError::Closed) => Ok(()),
            Err(_) if !is_current(&self.shared, &client) => Ok(()),
            Err(e) => {
                let mut known = lock(&self.shared.topics);
                for topic in &added {
                    known.remove(topic);
                }
                Err(e)
            }
        }
    }

    /// Removes `topics`, and their last seqs; their events stop reaching [`Session::events`].
    /// The core gets `start_hard_cap` to answer, as for [`Session::subscribe`] (whose turn
    /// rule applies): the topics are out of the session either way, and a core that doesn't
    /// answer is [`ClientError::Timeout`].
    pub async fn unsubscribe(&self, topics: Vec<String>) -> Result<(), ClientError> {
        let deadline = Instant::now() + lock(&self.shared.cfg).start_hard_cap;
        let _turn = self.turn(deadline).await?;
        // The connection is read while the topics are held (in the order of
        // `note_if_current`), so a setup that starts meanwhile reads the topics after this
        // edit, or finds the connection here: either way the earlier subscription is ended.
        let (client, removed) = {
            let current = lock(&self.shared.client);
            let mut known = lock(&self.shared.topics);
            let removed: Vec<String> = topics
                .into_iter()
                .filter(|topic| known.remove(topic).is_some())
                .collect();
            if current.is_none() {
                lock(&self.shared.history)
                    .removed
                    .extend(removed.iter().cloned());
            }
            (current.clone(), removed)
        };
        if removed.is_empty() {
            return Ok(());
        }
        let Some(client) = client else {
            return Ok(());
        };
        match by(deadline, client.unsubscribe_only(removed.clone())).await {
            Ok(()) => {
                // Its events still on their way are not a later subscription's.
                mark_stale(&self.shared, &client, &removed);
                Ok(())
            }
            Err(ClientError::Closed) => Ok(()),
            // Replaced meanwhile: the next connection never subscribes them.
            Err(_) if !is_current(&self.shared, &client) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The turn to change topics (`Shared::ops`), or [`ClientError::Timeout`] at `deadline`.
    async fn turn(
        &self,
        deadline: Instant,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>, ClientError> {
        by(deadline, async { Ok(self.shared.ops.lock().await) }).await
    }

    /// The events of every subscribed topic, across reconnects, in the core's order per topic.
    /// It can be taken once: a later call gets a receiver that is already closed. Until it is
    /// taken and read, the session keeps the events itself.
    pub fn events(&self) -> mpsc::Receiver<EventParams> {
        lock(&self.shared.events).take().unwrap_or_else(|| {
            let (_closed, rx) = mpsc::channel(1);
            rx
        })
    }

    /// `topic`'s last seq (module docs), if it is subscribed and has one.
    pub fn last_seq(&self, topic: &str) -> Option<u64> {
        lock(&self.shared.topics).get(topic).copied().flatten()
    }

    /// Switches to the workspace `id` with a new `core/hello` that names it (P4a-R34), and
    /// returns that hello; later reconnects name it too. A core is started for it as the
    /// configuration says, even after a failed start. `suite.toml`'s default is never changed.
    /// A refusal (`workspace.unsupported` for a workspace the core doesn't serve, ARCH-090) is
    /// the error, and the session stays as it was.
    ///
    /// `Ok` means the core accepted the workspace. If the new connection is lost while its
    /// topics are subscribed, the session reconnects in that workspace, and [`Session::state`]
    /// says so. Calls to it run one at a time.
    pub async fn set_workspace(&self, id: &str) -> Result<HelloResult, ClientError> {
        let _switching = self.shared.switching.lock().await;
        let mut cfg = lock(&self.shared.cfg).clone();
        cfg.workspace = id.to_string();
        cfg.workspace_origin = None;
        let client = Client::connect(&cfg).await?;
        let hello = client.hello().clone();
        *lock(&self.shared.cfg) = cfg;
        let (done, switched) = oneshot::channel();
        self.commands
            .send(Command::Switch(Box::new(client), done))
            .map_err(|_| ClientError::Closed)?;
        switched.await.map_err(|_| ClientError::Closed)?;
        Ok(hello)
    }

    /// Allows starting a core again after a failed start (module docs), and makes the next
    /// attempt now instead of after the backoff. Does nothing while connected.
    pub fn retry_now(&self) {
        let _ = self.commands.send(Command::Retry);
    }

    /// Stops the session: its connection closes, calls in flight end with
    /// [`ClientError::Closed`] and [`Session::events`] ends.
    pub async fn close(mut self) {
        let _ = self.commands.send(Command::Close);
        let _ = (&mut self.task).await;
    }

    fn client(&self) -> Result<Arc<Client>, ClientError> {
        lock(&self.shared.client)
            .clone()
            .ok_or(ClientError::NotConnected)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("state", &*self.shared.state.borrow())
            .finish_non_exhaustive()
    }
}

/// The pause before the attempt that follows `failed` failed attempts: 250 ms, doubling up to
/// 5 s (P4a-R33).
fn backoff(failed: u32) -> Duration {
    let doublings = failed.saturating_sub(1).min(16);
    BACKOFF_FIRST
        .saturating_mul(1 << doublings)
        .min(BACKOFF_MAX)
}

/// The session's task (module docs).
async fn run(shared: Arc<Shared>, commands: mpsc::UnboundedReceiver<Command>, out: Outbox) {
    let mut ctl = Ctl {
        commands,
        out,
        retried: false,
    };
    // The core instance of the last connection.
    let mut instance: Option<String> = None;
    // Failed attempts since the last connection, a lost connection counting as one.
    let mut failed: u32 = 0;
    // After a failed start: attempts only look for a core (module docs), and the state they
    // keep showing while none is found.
    let mut attach_only: Option<SessionState> = None;
    // A connection `set_workspace` made, to adopt instead of the next attempt.
    let mut offered: Option<(Box<Client>, oneshot::Sender<()>)> = None;
    loop {
        if offered.is_none() && failed > 0 && !ctl.retried {
            let wait = tokio::time::sleep(backoff(failed));
            match until(wait, &mut ctl, true).await {
                Step::Done(()) | Step::Retried => {}
                Step::Switch(client, done) => offered = Some((client, done)),
                Step::Close => break,
            }
        }
        if std::mem::take(&mut ctl.retried) {
            attach_only = None;
        }
        let (attempt, done) = match offered.take() {
            Some((client, done)) => (Ok(*client), Some(done)),
            None => {
                let mut cfg = lock(&shared.cfg).clone();
                if attach_only.is_some() {
                    cfg.start = StartPolicy::NoStart;
                }
                match until(Client::connect(&cfg), &mut ctl, false).await {
                    Step::Done(attempt) => (attempt, None),
                    Step::Switch(client, done) => (Ok(*client), Some(done)),
                    Step::Close => break,
                    Step::Retried => unreachable!("only the backoff stops for a retry"),
                }
            }
        };
        let live = match attempt {
            Ok(client) => {
                match until(establish(&shared, client, &mut instance), &mut ctl, false).await {
                    Step::Done(live) => live,
                    // The offered connection replaces this one. No `set_workspace` waits for
                    // this one: they run one at a time. This one is no longer read, so it
                    // is no longer current either.
                    Step::Switch(client, next) => {
                        *lock(&shared.client) = None;
                        offered = Some((client, next));
                        continue;
                    }
                    Step::Close => break,
                    Step::Retried => unreachable!("only the backoff stops for a retry"),
                }
            }
            Err(e) => Err(e),
        };
        let live = match live {
            Ok(live) => live,
            Err(e) => {
                *lock(&shared.client) = None;
                failed += 1;
                let state = state_after(&e, failed);
                let start_failed = matches!(
                    e,
                    ClientError::StartFailed { .. } | ClientError::CannotStart { .. }
                );
                if start_failed {
                    // A retry asked for during the start was this start's. After any other
                    // failure it stays: the next attempt follows at once.
                    ctl.retried = false;
                    attach_only = Some(state.clone());
                    set_state(&shared, state);
                } else if let (Some(waiting), ClientError::NotRunning) = (&attach_only, &e) {
                    // No core yet: the start failure is still why (and a state an earlier
                    // attach-only attempt met, a foreign core's, clears).
                    set_state(&shared, waiting.clone());
                } else {
                    set_state(&shared, state);
                }
                if let Some(done) = done {
                    let _ = done.send(());
                }
                continue;
            }
        };
        failed = 0;
        attach_only = None;
        shared
            .state
            .send_replace(SessionState::Ready(Box::new(live.client.hello().clone())));
        if let Some(done) = done {
            let _ = done.send(());
        }
        let interrupt = forward(&shared, live.events, &mut ctl).await;
        // A retry asked for while connected has nothing to do.
        ctl.retried = false;
        match interrupt {
            Interrupt::Lost => {
                *lock(&shared.client) = None;
                failed = 1;
                set_state(&shared, SessionState::Reconnecting { attempt: failed });
            }
            // This connection is no longer read: none is current until the offered one is
            // established.
            Interrupt::Switch(client, done) => {
                *lock(&shared.client) = None;
                offered = Some((client, done));
            }
            Interrupt::Close => break,
        }
    }
    *lock(&shared.client) = None;
}

/// A connection the session uses: calls go out on `client`, every event it receives comes in
/// on `events`.
struct Live {
    client: Arc<Client>,
    events: mpsc::UnboundedReceiver<EventParams>,
}

/// Subscribes the session's topics on `client`, from their last seqs (module docs), and makes
/// it the connection calls go out on. `instance` is the last connection's core instance.
async fn establish(
    shared: &Shared,
    client: Client,
    instance: &mut Option<String>,
) -> Result<Live, ClientError> {
    let client = Arc::new(client);
    let cap = lock(&shared.cfg).start_hard_cap;
    // Registered before the subscribe is written: replays may beat its reply.
    let events = client.conn().add_sink_all().ok_or(ClientError::Closed)?;
    let now = &client.hello().instance_id;
    let restarted = instance.as_ref().is_some_and(|old| old != now);
    let (topics, since) = {
        let mut known = lock(&shared.topics);
        // A new stream: what was noted about the last one is of no use now.
        *lock(&shared.history) = History::default();
        if restarted {
            for last in known.values_mut().filter(|last| last.is_some()) {
                *last = Some(0);
            }
        }
        let since: BTreeMap<String, u64> = known
            .iter()
            .filter_map(|(topic, last)| last.map(|seq| (topic.clone(), seq)))
            .collect();
        (known.keys().cloned().collect::<Vec<_>>(), since)
    };
    let mut sent: BTreeSet<String> = topics.iter().cloned().collect();
    if !topics.is_empty() {
        let subscribed = bounded(cap, client.subscribe_only(topics, since)).await?;
        note_subscribed(shared, &subscribed.topics);
    }
    *instance = Some(now.clone());
    *lock(&shared.client) = Some(client.clone());
    // From now on `Session::subscribe` and `unsubscribe` send on this connection themselves.
    // What they changed before, and while this runs, is put right here, one turn at a time.
    let _turn = by(Instant::now() + cap, async { Ok(shared.ops.lock().await) }).await?;
    // Topics removed while the first subscribe was under way, and possibly added again since:
    // the core has them from the earlier subscription, and its events are on the stream.
    let mut removed = std::mem::take(&mut lock(&shared.history).removed);
    loop {
        let (added, stale): (Vec<String>, Vec<String>) = {
            let mut known = lock(&shared.topics);
            let stale: Vec<String> = sent
                .iter()
                .filter(|topic| !known.contains_key(*topic) || removed.contains(*topic))
                .cloned()
                .collect();
            // Added again: a new subscription, with no cursor of the earlier one.
            for topic in &stale {
                if let Some(last) = known.get_mut(topic) {
                    *last = None;
                }
            }
            let added = known
                .keys()
                .filter(|topic| !sent.contains(*topic) || stale.contains(*topic))
                .cloned()
                .collect();
            (added, stale)
        };
        removed.clear();
        if added.is_empty() && stale.is_empty() {
            break;
        }
        if !stale.is_empty() {
            for topic in &stale {
                sent.remove(topic);
            }
            bounded(cap, client.unsubscribe_only(stale.clone())).await?;
            mark_stale(shared, &client, &stale);
        }
        if !added.is_empty() {
            sent.extend(added.iter().cloned());
            let subscribed = bounded(cap, client.subscribe_only(added, BTreeMap::new())).await?;
            note_live(shared, &subscribed.topics);
        }
    }
    Ok(Live { client, events })
}

/// `call`, or [`ClientError::Timeout`] when it takes longer than `cap`.
async fn bounded<T>(
    cap: Duration,
    call: impl Future<Output = Result<T, ClientError>>,
) -> Result<T, ClientError> {
    by(Instant::now() + cap, call).await
}

/// `call`, or [`ClientError::Timeout`] when it isn't done at `deadline`.
async fn by<T>(
    deadline: Instant,
    call: impl Future<Output = Result<T, ClientError>>,
) -> Result<T, ClientError> {
    tokio::time::timeout_at(deadline, call)
        .await
        .unwrap_or(Err(ClientError::Timeout))
}

/// Whether `client` is the connection calls go out on now.
fn is_current(shared: &Shared, client: &Arc<Client>) -> bool {
    lock(&shared.client)
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, client))
}

/// [`note_subscribed`] for an answer of `client`, unless it is no longer the current
/// connection: its seqs would be another connection's cursors, and the current one's setup
/// subscribes the topic itself.
fn note_if_current(shared: &Shared, client: &Arc<Client>, states: &[TopicState]) {
    let current = lock(&shared.client);
    if current
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, client))
    {
        note_live(shared, states);
    }
}

/// A live-only answer: each topic's last seq is at least the seq it was answered with, as all
/// events of that subscription are above it (an event of an earlier one that got in first
/// does not keep the floor lower).
fn note_live(shared: &Shared, states: &[TopicState]) {
    let mut known = lock(&shared.topics);
    for state in states {
        if let Some(last) = known.get_mut(&state.topic) {
            *last = Some(last.map_or(state.seq, |seq| seq.max(state.seq)));
        }
    }
}

/// A topic subscribed without a cursor starts at its seq when it was subscribed.
fn note_subscribed(shared: &Shared, states: &[TopicState]) {
    let mut known = lock(&shared.topics);
    for state in states {
        if let Some(last @ None) = known.get_mut(&state.topic) {
            *last = Some(state.seq);
        }
    }
}

/// The state after a failed attempt (module docs, "Backoff").
fn state_after(error: &ClientError, failed: u32) -> SessionState {
    match error {
        ClientError::Incompatible { core_protocol } => SessionState::Incompatible {
            core_protocol: core_protocol.clone(),
        },
        ClientError::NotRunning
        | ClientError::Timeout
        | ClientError::Closed
        | ClientError::NotConnected => SessionState::Reconnecting { attempt: failed },
        other => SessionState::Unavailable {
            reason: other.to_string(),
        },
    }
}

/// Sets the state; receivers hear of it only when it changed.
fn set_state(shared: &Shared, state: SessionState) {
    shared.state.send_if_modified(|now| {
        if *now == state {
            return false;
        }
        *now = state;
        true
    });
}

/// Why [`forward`] stopped.
enum Interrupt {
    Lost,
    Switch(Box<Client>, oneshot::Sender<()>),
    Close,
}

/// Takes `events` from the connection until it closes or a command comes.
async fn forward(
    shared: &Shared,
    mut events: mpsc::UnboundedReceiver<EventParams>,
    ctl: &mut Ctl,
) -> Interrupt {
    // Events taken from this stream so far: the position of the one in hand.
    let mut position: u64 = 0;
    loop {
        match until(events.recv(), ctl, false).await {
            Step::Done(Some(event)) => {
                position += 1;
                if took(shared, &event, position) {
                    ctl.out.push(event);
                }
            }
            Step::Done(None) => return Interrupt::Lost,
            Step::Switch(client, done) => return Interrupt::Switch(client, done),
            Step::Close => return Interrupt::Close,
            Step::Retried => unreachable!("only the backoff stops for a retry"),
        }
    }
}

/// Notes that the events of `topics` that the stream of `client` has received so far belong to
/// subscriptions that have ended, if `client` is still the current connection (another one has
/// a stream of its own, and a fresh [`History`]).
fn mark_stale(shared: &Shared, client: &Arc<Client>, topics: &[String]) {
    let current = lock(&shared.client);
    if current
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, client))
    {
        let upto = client.conn().routed_all();
        let mut history = lock(&shared.history);
        for topic in topics {
            history.stale.insert(topic.clone(), upto);
        }
    }
}

/// Records `event`, the `position`th of its connection's stream, as its topic's last seq;
/// `false` for a topic no longer subscribed (one still on its way after an unsubscribe), for
/// an event of a subscription that has ended while the topic is subscribed again, and for an
/// `event` at or below the topic's last seq: seqs only rise within a connection, so it is from
/// an earlier subscription or a duplicate (a snapshot or resync request is kept).
fn took(shared: &Shared, event: &EventParams, position: u64) -> bool {
    let ended = lock(&shared.history)
        .stale
        .get(&event.topic)
        .is_some_and(|upto| position <= *upto);
    if ended {
        return false;
    }
    match lock(&shared.topics).get_mut(&event.topic) {
        Some(last) => {
            if event.kind == EventKind::Event && last.is_some_and(|seq| event.seq <= seq) {
                return false;
            }
            *last = Some(event.seq);
            true
        }
        None => false,
    }
}

/// The task's commands, its outbox, and whether a [`Command::Retry`] came.
struct Ctl {
    commands: mpsc::UnboundedReceiver<Command>,
    out: Outbox,
    retried: bool,
}

enum Step<T> {
    Done(T),
    Switch(Box<Client>, oneshot::Sender<()>),
    /// A [`Command::Close`], or every [`Session`] handle is gone.
    Close,
    /// A [`Command::Retry`] came, and the step stops for it.
    Retried,
}

/// Runs `fut` until it finishes or a command comes, handing kept events to the app meanwhile.
/// A [`Command::Retry`] sets `ctl.retried`, and stops `fut` only when `stop_on_retry`.
async fn until<F: Future>(fut: F, ctl: &mut Ctl, stop_on_retry: bool) -> Step<F::Output> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            biased;
            command = ctl.commands.recv() => match command {
                Some(Command::Retry) => {
                    ctl.retried = true;
                    if stop_on_retry {
                        return Step::Retried;
                    }
                }
                Some(Command::Switch(client, done)) => return Step::Switch(client, done),
                Some(Command::Close) | None => return Step::Close,
            },
            () = ctl.out.flush_one(), if ctl.out.has_backlog() => {}
            done = &mut fut => return Step::Done(done),
        }
    }
}

/// [`Session::events`]' sender, and the events it had no room for yet, in order.
struct Outbox {
    tx: mpsc::Sender<EventParams>,
    backlog: VecDeque<EventParams>,
}

impl Outbox {
    fn push(&mut self, event: EventParams) {
        if self.backlog.len() >= BACKLOG_MAX {
            self.overflow(event);
            return;
        }
        if !self.backlog.is_empty() {
            self.backlog.push_back(event);
            return;
        }
        match self.tx.try_send(event) {
            // Closed: the app dropped its receiver, so nobody wants the event.
            Ok(()) | Err(TrySendError::Closed(_)) => {}
            Err(TrySendError::Full(event)) => self.backlog.push_back(event),
        }
    }

    /// The backlog is full: its events and `event` give way to one `resync_required` per
    /// topic among them, at that topic's newest seq, as the core does (ARCH-032).
    fn overflow(&mut self, event: EventParams) {
        let mut newest: BTreeMap<String, u64> = BTreeMap::new();
        for dropped in self.backlog.drain(..).chain(std::iter::once(event)) {
            newest.insert(dropped.topic, dropped.seq);
        }
        self.backlog
            .extend(newest.into_iter().map(|(topic, seq)| EventParams {
                topic,
                seq,
                kind: EventKind::ResyncRequired,
                data: Value::Null,
            }));
    }

    fn has_backlog(&self) -> bool {
        !self.backlog.is_empty()
    }

    /// Hands the oldest kept event over once there is room.
    async fn flush_one(&mut self) {
        match self.tx.reserve().await {
            Ok(permit) => {
                if let Some(event) = self.backlog.pop_front() {
                    permit.send(event);
                }
            }
            Err(_) => self.backlog.clear(),
        }
    }
}

#[cfg(test)]
mod tests {
    use lil_platform::process::current_exe_dir;
    use lil_protocol::{
        CallerKind, CoreInfo, ProtocolVersion, SubscribeResult, UnsubscribeResult,
        incompatible_refusal, schema_hash,
    };
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::windows::named_pipe::NamedPipeServer;

    use super::*;
    use crate::StartPolicy;

    fn hello_result(instance: &str) -> HelloResult {
        HelloResult {
            session_id: "s_0000000000000001".into(),
            instance_id: instance.into(),
            protocol: lil_protocol::PROTOCOL.to_string(),
            build_id: "fake".into(),
            schema_hash: schema_hash(),
            capabilities: vec!["events.v1".into()],
            caller_kind: "cli".into(),
            workspace: None,
            core: CoreInfo {
                pid: std::process::id(),
                channel: "test-fake".into(),
                pipe: "fake".into(),
            },
        }
    }

    /// A fake core on one pipe connection: it answers the hello as core instance `instance`,
    /// and then only what a test tells it to.
    struct FakeCore {
        lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<NamedPipeServer>>>,
        write: tokio::io::WriteHalf<NamedPipeServer>,
    }

    impl FakeCore {
        async fn accept(server: NamedPipeServer, instance: &str) -> FakeCore {
            let (mut fake, hello) = FakeCore::accept_hello(server).await;
            fake.reply(&hello, &Outcome::Ok(hello_result(instance)))
                .await;
            fake
        }

        /// Accepts the connection and reads its `core/hello`, which it does not answer.
        async fn accept_hello(server: NamedPipeServer) -> (FakeCore, Value) {
            server.connect().await.expect("accept");
            let (read, write) = tokio::io::split(server);
            let mut fake = FakeCore {
                lines: BufReader::new(read).lines(),
                write,
            };
            let hello = fake.request("core/hello").await;
            (fake, hello)
        }

        async fn request(&mut self, method: &str) -> Value {
            let line = self.lines.next_line().await.unwrap().expect("a request");
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], method, "{request}");
            request
        }

        async fn reply<T: serde::Serialize>(&mut self, request: &Value, outcome: &Outcome<T>) {
            let line = json!({"jsonrpc": "2.0", "id": request["id"], "result": outcome});
            self.send(&line.to_string()).await;
        }

        async fn send(&mut self, line: &str) {
            self.write.write_all(line.as_bytes()).await.unwrap();
            self.write.write_all(b"\n").await.unwrap();
            self.write.flush().await.unwrap();
        }

        /// The `lil/subscribe` params, after replying with `topic` at `seq`.
        async fn subscribed(&mut self, topic: &str, seq: u64) -> Value {
            let request = self.request("lil/subscribe").await;
            self.reply_subscribed(&request, &[(topic, seq)]).await;
            request["params"].clone()
        }

        /// Answers a `lil/subscribe` with these topics at these seqs.
        async fn reply_subscribed(&mut self, request: &Value, topics: &[(&str, u64)]) {
            let result = SubscribeResult {
                topics: topics
                    .iter()
                    .map(|(topic, seq)| TopicState {
                        topic: (*topic).into(),
                        seq: *seq,
                    })
                    .collect(),
            };
            self.reply(request, &Outcome::Ok(result)).await;
        }

        /// Answers a `lil/unsubscribe` with the topics it named.
        async fn reply_unsubscribed(&mut self, request: &Value) {
            let topics = request["params"]["topics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .collect();
            self.reply(request, &Outcome::Ok(UnsubscribeResult { topics }))
                .await;
        }

        /// Whether the session sends nothing for `within` (a failure only shows as a request).
        async fn quiet_for(&mut self, within: Duration) -> bool {
            tokio::time::timeout(within, self.lines.next_line())
                .await
                .is_err()
        }

        async fn event(&mut self, topic: &str, seq: u64) {
            let params = json!({"topic": topic, "seq": seq, "kind": "event", "data": {"n": seq}});
            let line = lil_protocol::encode_notification(lil_protocol::method::EVENT, &params);
            self.send(&line).await;
        }
    }

    /// Waits for a state `pred` accepts.
    async fn wait_until(
        state: &mut watch::Receiver<SessionState>,
        pred: impl FnMut(&SessionState) -> bool,
    ) {
        tokio::time::timeout(Duration::from_secs(30), state.wait_for(pred))
            .await
            .expect("no such state in time")
            .expect("the session ended");
    }

    async fn next(events: &mut mpsc::Receiver<EventParams>) -> (String, u64) {
        let event = tokio::time::timeout(Duration::from_secs(30), events.recv())
            .await
            .expect("no event in time")
            .expect("the stream ended");
        (event.topic, event.seq)
    }

    /// The cursor rule of the module docs, against a fake core that closes the connection
    /// twice: the same core instance again gets `since_seq` from the last seq; another
    /// instance gets 0, so its own first events are not skipped.
    #[test] // spec: APP-035, ARCH-100
    fn resubscribes_from_the_last_seq_and_from_0_on_a_new_core() {
        let env = lil_testkit::TestEnv::new();
        let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        cfg.start = StartPolicy::NoStart;
        // The fake core is this process.
        cfg.server_dirs = vec![current_exe_dir().unwrap()];
        let pipe = env.env.pipe_name();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let mut events = session.events();

            // First connection: no cursor; the topic stands at 2, then 3 and 4 come.
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            assert_eq!(
                fake.subscribed("test:r", 2).await,
                json!({"topics": ["test:r"], "since_seq": {}})
            );
            fake.event("test:r", 3).await;
            fake.event("test:r", 4).await;
            assert_eq!(next(&mut events).await, ("test:r".to_string(), 3));
            assert_eq!(next(&mut events).await, ("test:r".to_string(), 4));
            assert_eq!(session.last_seq("test:r"), Some(4));

            // The same core again: from 4.
            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            drop(fake);
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            assert_eq!(
                fake.subscribed("test:r", 5).await,
                json!({"topics": ["test:r"], "since_seq": {"test:r": 4}})
            );
            fake.event("test:r", 5).await;
            assert_eq!(next(&mut events).await, ("test:r".to_string(), 5));

            // Another core instance: from 0 in its numbering.
            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            drop(fake);
            let mut fake = FakeCore::accept(server, "c_0000000000000002").await;
            assert_eq!(
                fake.subscribed("test:r", 1).await,
                json!({"topics": ["test:r"], "since_seq": {"test:r": 0}})
            );
            fake.event("test:r", 1).await;
            assert_eq!(next(&mut events).await, ("test:r".to_string(), 1));
            assert_eq!(session.last_seq("test:r"), Some(1));
            session.close().await;
            drop(fake);
        });
    }

    /// A core that answers the hello but never the `lil/subscribe`: `close` still returns at
    /// once, and with a short `start_hard_cap` the subscribe gives up, the state says
    /// `Reconnecting`, and the next connection makes the session ready.
    #[test] // spec: APP-034, ARCH-100
    fn a_stalled_subscribe_neither_blocks_close_nor_the_reconnect() {
        let env = lil_testkit::TestEnv::new();
        let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        cfg.start = StartPolicy::NoStart;
        cfg.server_dirs = vec![current_exe_dir().unwrap()];
        let pipe = env.env.pipe_name();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // The default bound (60 s): `close` doesn't wait for it.
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg.clone(), vec!["test:r".to_string()]);
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            fake.request("lil/subscribe").await;
            // Nor does a subscribe: the topic waits for the connection being set up.
            tokio::time::timeout(
                Duration::from_secs(5),
                session.subscribe(vec!["test:later".to_string()]),
            )
            .await
            .expect("subscribe waited for the stalled subscribe")
            .expect("subscribe");
            tokio::time::timeout(Duration::from_secs(5), session.close())
                .await
                .expect("close waited for the stalled subscribe");
            drop(fake);

            // A 1 s bound: the subscribe gives up and the session reconnects.
            let mut short = cfg.clone();
            short.start_hard_cap = Duration::from_secs(1);
            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            let session = Session::start(short, vec!["test:r".to_string()]);
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            fake.request("lil/subscribe").await;
            let next = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            wait_until(&mut state, |s| {
                matches!(s, SessionState::Reconnecting { .. })
            })
            .await;
            drop(fake);
            let mut fake = FakeCore::accept(next, "c_0000000000000001").await;
            fake.subscribed("test:r", 0).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;
            session.close().await;
            drop(fake);
        });
    }

    fn event(topic: &str, seq: u64) -> EventParams {
        EventParams {
            topic: topic.to_string(),
            seq,
            kind: EventKind::Event,
            data: json!({"n": seq}),
        }
    }

    /// A full backlog gives way to one `resync_required` per topic at its newest seq, the
    /// event that overflowed it included, and later events queue after them in order.
    #[test] // spec: ARCH-032
    fn a_full_backlog_becomes_one_resync_per_topic() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut out = Outbox {
            tx,
            backlog: VecDeque::new(),
        };
        out.push(event("test:a", 1));
        for n in 0..BACKLOG_MAX as u64 {
            let (topic, seq) = if n % 2 == 0 {
                ("test:a", 2 + n / 2)
            } else {
                ("test:b", 1 + n / 2)
            };
            out.push(event(topic, seq));
        }
        assert_eq!(out.backlog.len(), BACKLOG_MAX);
        let newest_b = out.backlog.back().unwrap().seq;
        out.push(event("test:a", 9000));
        out.push(event("test:b", newest_b + 1));

        let kept: Vec<(&str, u64, EventKind)> = out
            .backlog
            .iter()
            .map(|e| (e.topic.as_str(), e.seq, e.kind))
            .collect();
        assert_eq!(
            kept,
            [
                ("test:a", 9000, EventKind::ResyncRequired),
                ("test:b", newest_b, EventKind::ResyncRequired),
                ("test:b", newest_b + 1, EventKind::Event),
            ]
        );
        assert!(out.backlog.iter().take(2).all(|e| e.data.is_null()));
        assert_eq!(
            rx.try_recv().unwrap().seq,
            1,
            "the event sent before the backlog"
        );
    }

    /// Names that aren't topics never reach the core: left out at the start, refused by
    /// `subscribe` (which adds none of the call's topics then).
    #[test] // spec: ARCH-032
    fn invalid_topic_names_are_left_out_or_refused() {
        let env = lil_testkit::TestEnv::new();
        let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        cfg.start = StartPolicy::NoStart;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let topics = ["Bad Topic", "test:ok", ""].map(str::to_string).to_vec();
            let session = Session::start(cfg, topics);
            let known =
                |s: &Session| -> Vec<String> { lock(&s.shared.topics).keys().cloned().collect() };
            assert_eq!(known(&session), ["test:ok"]);
            let refused = session
                .subscribe(vec!["test:new".to_string(), "no spaces".to_string()])
                .await;
            match refused {
                Err(ClientError::Refused(error)) => assert_eq!(error.code, "params.invalid"),
                other => panic!("expected params.invalid, got {other:?}"),
            }
            assert_eq!(known(&session), ["test:ok"]);
            session.close().await;
        });
    }

    /// The unit tests' configuration: the fake core is this process, and none is started.
    fn fake_config(env: &lil_testkit::TestEnv) -> ClientConfig {
        let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
        cfg.start = StartPolicy::NoStart;
        cfg.server_dirs = vec![current_exe_dir().unwrap()];
        cfg
    }

    fn current_thread() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// A failure deadline for a step that must not wait for anything.
    const PROMPT: Duration = Duration::from_secs(5);

    /// While `set_workspace`'s connection is being made current, a subscribe goes to that
    /// connection, never to the one being replaced: that one is no longer read, and a seq from
    /// it would be another connection's cursor.
    #[test] // spec: APP-034, ARCH-100
    fn a_subscribe_during_a_switch_goes_to_the_new_connection() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let mut state = session.state();
            let mut old = FakeCore::accept(server, "c_0000000000000001").await;
            old.subscribed("test:r", 0).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            // The new connection is made; its `lil/subscribe` is not answered yet.
            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            let switch = session.set_workspace("0123456789abcdef");
            let during = async {
                let mut new = FakeCore::accept(server, "c_0000000000000001").await;
                let first = new.request("lil/subscribe").await;
                tokio::time::timeout(PROMPT, session.subscribe(vec!["test:w".to_string()]))
                    .await
                    .expect("subscribe waited on the connection being replaced")
                    .expect("subscribe");
                new.reply_subscribed(&first, &[("test:r", 0)]).await;
                // The topic added meanwhile reaches the new connection, live only.
                let late = new.request("lil/subscribe").await;
                new.reply_subscribed(&late, &[("test:w", 7)]).await;
                (new, late["params"].clone())
            };
            let (switched, (new, late)) = tokio::join!(switch, during);
            switched.expect("set_workspace");
            assert_eq!(late, json!({"topics": ["test:w"], "since_seq": {}}));
            assert_eq!(session.last_seq("test:w"), Some(7));
            // The old connection was closed without another request.
            let end = tokio::time::timeout(PROMPT, old.lines.next_line()).await;
            assert!(matches!(end, Ok(Ok(None))), "{end:?}");
            session.close().await;
            drop(new);
        });
    }

    /// A switch that cancels a connection's setup leaves no connection current until the new
    /// one is: the cancelled one is no longer read, and calls meanwhile are `NotConnected`.
    #[test] // spec: APP-034, ARCH-100
    fn a_switch_during_the_setup_leaves_no_connection_current() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let mut old = FakeCore::accept(server, "c_0000000000000001").await;
            let first = old.request("lil/subscribe").await;
            // No connection yet: the topic is recorded.
            session
                .subscribe(vec!["test:x".to_string()])
                .await
                .expect("subscribe");
            old.reply_subscribed(&first, &[("test:r", 0)]).await;
            // The connection is current, and its setup still waits for this answer.
            let second = old.request("lil/subscribe").await;
            assert_eq!(second["params"]["topics"], json!(["test:x"]));
            assert!(lock(&session.shared.client).is_some());

            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            let switch = session.set_workspace("0123456789abcdef");
            let during = async {
                let mut new = FakeCore::accept(server, "c_0000000000000001").await;
                let first = new.request("lil/subscribe").await;
                assert!(
                    lock(&session.shared.client).is_none(),
                    "a connection nobody reads is still current"
                );
                // Calls are refused before anything is sent, and say so.
                let called = session
                    .call_tool("test.echo", serde_json::Map::new(), None, None)
                    .await;
                assert!(
                    matches!(called, Err(ClientError::NotConnected)),
                    "{called:?}"
                );
                let app = session.app_call("app/confirmations.list", json!({})).await;
                assert!(matches!(app, Err(ClientError::NotConnected)), "{app:?}");
                new.reply_subscribed(&first, &[("test:r", 0), ("test:x", 3)])
                    .await;
                new
            };
            let (switched, new) = tokio::join!(switch, during);
            switched.expect("set_workspace");
            assert!(lock(&session.shared.client).is_some());
            session.close().await;
            drop((old, new));
        });
    }

    /// A topic unsubscribed while a connection is being set up is still subscribed in the
    /// core on it, once the setup has sent it: the setup takes it off again.
    #[test] // spec: APP-035, ARCH-032
    fn an_unsubscribe_during_the_setup_reaches_the_core() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let topics = ["test:r", "test:s"].map(str::to_string).to_vec();
            let session = Session::start(cfg, topics);
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            let first = fake.request("lil/subscribe").await;
            session
                .unsubscribe(vec!["test:s".to_string()])
                .await
                .expect("unsubscribe");
            fake.reply_subscribed(&first, &[("test:r", 0), ("test:s", 0)])
                .await;
            let off = tokio::time::timeout(PROMPT, fake.request("lil/unsubscribe"))
                .await
                .expect("test:s stayed subscribed in the core");
            assert_eq!(off["params"], json!({"topics": ["test:s"]}));
            fake.reply_unsubscribed(&off).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;
            session.close().await;
            drop(fake);
        });
    }

    /// Calls on one topic reach the core in the order they were made, whichever way they
    /// overlap: the later one is not sent before the earlier one is answered, so the last call
    /// wins in the core as it does in the session.
    #[test] // spec: APP-035, ARCH-032
    fn calls_on_one_topic_reach_the_core_in_the_order_they_were_made() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        let known =
            |s: &Session| -> Vec<String> { lock(&s.shared.topics).keys().cloned().collect() };
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, Vec::new());
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;
            let topic = || vec!["test:x".to_string()];

            // A subscribe, then an unsubscribe: the topic ends unsubscribed.
            let calls =
                async { tokio::join!(session.subscribe(topic()), session.unsubscribe(topic())) };
            let core = async {
                let on = fake.request("lil/subscribe").await;
                assert!(
                    fake.quiet_for(Duration::from_millis(300)).await,
                    "the unsubscribe was sent before the subscribe was answered"
                );
                fake.reply_subscribed(&on, &[("test:x", 4)]).await;
                let off = fake.request("lil/unsubscribe").await;
                fake.reply_unsubscribed(&off).await;
            };
            let ((on, off), ()) = tokio::join!(calls, core);
            on.expect("subscribe");
            off.expect("unsubscribe");
            assert!(known(&session).is_empty());

            // An unsubscribe, then a subscribe: the topic ends subscribed.
            let first = session.subscribe(topic());
            let core = async {
                let on = fake.request("lil/subscribe").await;
                fake.reply_subscribed(&on, &[("test:x", 5)]).await;
            };
            let (first, ()) = tokio::join!(first, core);
            first.expect("subscribe");
            let calls =
                async { tokio::join!(session.unsubscribe(topic()), session.subscribe(topic())) };
            let core = async {
                let off = fake.request("lil/unsubscribe").await;
                assert!(
                    fake.quiet_for(Duration::from_millis(300)).await,
                    "the subscribe was sent before the unsubscribe was answered"
                );
                fake.reply_unsubscribed(&off).await;
                let on = fake.request("lil/subscribe").await;
                fake.reply_subscribed(&on, &[("test:x", 6)]).await;
            };
            let ((off, on), ()) = tokio::join!(calls, core);
            off.expect("unsubscribe");
            on.expect("subscribe");
            assert_eq!(known(&session), ["test:x"]);
            assert_eq!(session.last_seq("test:x"), Some(6));
            session.close().await;
            drop(fake);
        });
    }

    /// A core that never answers a `lil/unsubscribe` gets `start_hard_cap`, as for the other
    /// calls: then the call is [`ClientError::Timeout`], and the topic is out of the session.
    #[test] // spec: APP-034, ARCH-100
    fn an_unsubscribe_the_core_never_answers_times_out() {
        let env = lil_testkit::TestEnv::new();
        let mut cfg = fake_config(&env);
        cfg.start_hard_cap = Duration::from_secs(1);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            fake.subscribed("test:r", 0).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            let call = tokio::time::timeout(PROMPT, session.unsubscribe(vec!["test:r".into()]));
            let core = fake.request("lil/unsubscribe");
            let (call, _request) = tokio::join!(call, core);
            match call.expect("unsubscribe waited for the core") {
                Err(ClientError::Timeout) => {}
                other => panic!("expected a timeout, got {other:?}"),
            }
            assert_eq!(session.last_seq("test:r"), None);
            assert!(lock(&session.shared.topics).is_empty());
            session.close().await;
            drop(fake);
        });
    }

    /// A `retry_now` made while an attempt is stalled (a core that never answers its hello) is
    /// applied as soon as the attempt ends: no backoff comes between that attempt and the next.
    /// Measured on tokio's paused clock, which moves only when the session waits.
    #[test] // spec: ARCH-100, APP-034
    fn a_retry_during_a_stalled_attach_skips_the_backoff() {
        let env = lil_testkit::TestEnv::new();
        let mut cfg = fake_config(&env);
        cfg.start_timeout = Duration::from_secs(5);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, Vec::new());
            let (_stalled, _hello) = FakeCore::accept_hello(server).await;
            let next = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            let mut state = session.state();
            let failed_at = tokio::spawn(async move {
                state
                    .wait_for(|s| matches!(s, SessionState::Reconnecting { .. }))
                    .await
                    .expect("the session ended");
                tokio::time::Instant::now()
            });

            session.retry_now();
            tokio::time::pause();
            next.connect().await.expect("the next attempt");
            let next_at = tokio::time::Instant::now();
            tokio::time::resume();
            let failed_at = failed_at.await.unwrap();
            let gap = next_at - failed_at;
            assert!(gap < BACKOFF_FIRST, "the next attempt waited {gap:?}");
            session.close().await;
            drop(next);
        });
    }

    /// After a failed start, a core someone else started is found by the attach-only attempts;
    /// when it goes, the state is the start failure's again, not the foreign core's.
    #[test] // spec: ARCH-100, ARCH-014
    fn a_foreign_cores_state_clears_when_it_exits() {
        let env = lil_testkit::TestEnv::new();
        let mut cfg = fake_config(&env);
        cfg.start = StartPolicy::StartIfNeeded;
        cfg.core_exe = env.root().join("no such bin").join("lil-core.exe");
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let session = Session::start(cfg, Vec::new());
            let mut state = session.state();
            wait_until(&mut state, |s| {
                matches!(s, SessionState::Unavailable { reason } if reason.contains("Cannot start"))
            })
            .await;
            let failure = state.borrow().clone();

            // A core of a newer protocol: refused, and left running.
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let (mut core, hello) = FakeCore::accept_hello(server).await;
            let newer = ProtocolVersion { major: 9, minor: 0 };
            let refusal = incompatible_refusal(newer, "t_0000000000000001");
            core.reply(&hello, &Outcome::<HelloResult>::Error { error: refusal })
                .await;
            wait_until(&mut state, |s| {
                matches!(s, SessionState::Incompatible { core_protocol } if core_protocol == "9.0")
            })
            .await;

            // It exits: the start failure is why again.
            drop(core);
            wait_until(&mut state, |s| *s == failure).await;
            session.close().await;
        });
    }

    /// A topic unsubscribed and subscribed again while a connection's first `lil/subscribe` is
    /// under way (the session has no connection to tell yet) is a new subscription: the
    /// replay of the earlier one, already on the stream, never reaches the app.
    #[test] // spec: APP-035, APP-033, ARCH-032
    fn a_resubscribed_topic_gets_no_events_of_the_earlier_subscription() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let mut events = session.events();
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            fake.subscribed("test:r", 5).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;
            assert_eq!(session.last_seq("test:r"), Some(5));

            // The core restarts as the same instance (a lost connection); the second
            // connection's first subscribe asks for what follows seq 5.
            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            drop(fake);
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            let first = fake.request("lil/subscribe").await;
            assert_eq!(first["params"]["since_seq"], json!({"test:r": 5}));
            let topic = || vec!["test:r".to_string()];
            session.unsubscribe(topic()).await.expect("unsubscribe");
            session.subscribe(topic()).await.expect("subscribe");

            // The earlier subscription's replay, then its answer.
            for seq in 6..=9 {
                fake.event("test:r", seq).await;
            }
            fake.reply_subscribed(&first, &[("test:r", 9)]).await;
            // The setup ends that subscription and starts a new one, live only.
            let off = tokio::time::timeout(PROMPT, fake.request("lil/unsubscribe"))
                .await
                .expect("the earlier subscription was not ended");
            fake.reply_unsubscribed(&off).await;
            let on = fake.request("lil/subscribe").await;
            assert_eq!(on["params"], json!({"topics": ["test:r"], "since_seq": {}}));
            fake.reply_subscribed(&on, &[("test:r", 9)]).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;
            fake.event("test:r", 10).await;
            assert_eq!(next(&mut events).await, ("test:r".to_string(), 10));
            assert_eq!(session.last_seq("test:r"), Some(10));
            session.close().await;
            drop(fake);
        });
    }

    /// Connected: after an unsubscribe is answered, the topic's events that the stream had
    /// already received are marked as the ended subscription's, and `took` drops exactly
    /// those, whichever topic they follow.
    #[test] // spec: APP-035, ARCH-032
    fn an_unsubscribe_marks_the_events_already_received_as_ended() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let topics = ["test:r", "test:o"].map(str::to_string).to_vec();
            let session = Session::start(cfg, topics);
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            let first = fake.request("lil/subscribe").await;
            fake.reply_subscribed(&first, &[("test:r", 0), ("test:o", 0)])
                .await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            let call = session.unsubscribe(vec!["test:r".to_string()]);
            let core = async {
                let off = fake.request("lil/unsubscribe").await;
                // Two events of the topic, and one of another, come before the answer.
                fake.event("test:r", 1).await;
                fake.event("test:o", 1).await;
                fake.event("test:r", 2).await;
                fake.reply_unsubscribed(&off).await;
            };
            let (called, ()) = tokio::join!(call, core);
            called.expect("unsubscribe");
            assert_eq!(lock(&session.shared.history).stale.get("test:r"), Some(&3));

            // Subscribed again: the three events of the stream so far are the earlier one's.
            let history_says = |position: u64, topic: &str| {
                let event = EventParams {
                    topic: topic.to_string(),
                    seq: position,
                    kind: EventKind::Event,
                    data: Value::Null,
                };
                took(&session.shared, &event, position)
            };
            lock(&session.shared.topics).insert("test:r".to_string(), None);
            assert!(!history_says(3, "test:r"));
            assert!(history_says(4, "test:r"));
            assert!(history_says(3, "test:o"));
            session.close().await;
            drop(fake);
        });
    }

    /// The setup's wait for its turn has the same bound as the topic calls: a topic call that
    /// holds the turn and is never polled again ends the setup with `Timeout` (and a new
    /// attempt), not a session that never becomes ready.
    #[test] // spec: APP-034, ARCH-100
    fn a_setup_does_not_wait_for_a_turn_for_ever() {
        let env = lil_testkit::TestEnv::new();
        let mut cfg = fake_config(&env);
        cfg.start_hard_cap = Duration::from_secs(1);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, Vec::new());
            let mut state = session.state();
            let mut old = FakeCore::accept(server, "c_0000000000000001").await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            // A subscribe that takes the turn and sends, and is then left alone.
            let mut parked = Box::pin(session.subscribe(vec!["test:x".to_string()]));
            std::future::poll_fn(|cx| {
                assert!(parked.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            old.request("lil/subscribe").await;

            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            let switch = session.set_workspace("0123456789abcdef");
            let during = async {
                let mut new = FakeCore::accept(server, "c_0000000000000001").await;
                let first = new.request("lil/subscribe").await;
                new.reply_subscribed(&first, &[("test:x", 0)]).await;
                // The setup now waits for the turn; its bound ends the attempt.
                let reconnecting = SessionState::Reconnecting { attempt: 1 };
                let mut watch = session.state();
                wait_until(&mut watch, |s| *s == reconnecting).await;
                new
            };
            let (switched, new) =
                tokio::time::timeout(PROMPT, async { tokio::join!(switch, during) })
                    .await
                    .expect("the setup waited for the turn for ever");
            switched.expect("set_workspace");
            drop((parked, new));
            session.close().await;
            drop(old);
        });
    }

    /// An answer of a replaced connection to a subscribe that was in flight when the switch
    /// came sets no cursor: the topic's cursor is the new connection's.
    #[test] // spec: APP-035, ARCH-100
    fn a_late_answer_of_a_replaced_connection_sets_no_cursor() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, Vec::new());
            let mut state = session.state();
            let mut old = FakeCore::accept(server, "c_0000000000000001").await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            let (answered, answered_rx) = oneshot::channel();
            let sub = async {
                let subscribed = session.subscribe(vec!["test:x".to_string()]).await;
                let _ = answered.send(());
                subscribed
            };
            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            let switch = session.set_workspace("0123456789abcdef");
            let during = async {
                let on_old = old.request("lil/subscribe").await;
                let mut new = FakeCore::accept(server, "c_0000000000000001").await;
                let first = new.request("lil/subscribe").await;
                // The replaced connection answers first, with its own seq, and the call has
                // taken it before the new connection answers.
                old.reply_subscribed(&on_old, &[("test:x", 99)]).await;
                answered_rx.await.expect("the call ended");
                new.reply_subscribed(&first, &[("test:x", 7)]).await;
                new
            };
            let (subscribed, switched, new) = tokio::join!(sub, switch, during);
            subscribed.expect("subscribe");
            switched.expect("set_workspace");
            assert_eq!(session.last_seq("test:x"), Some(7));
            session.close().await;
            drop((old, new));
        });
    }

    /// An error from a connection that has been replaced is not the call's error: the topic
    /// stays, and the new connection subscribes it.
    #[test] // spec: APP-035, ARCH-100
    fn an_error_of_a_replaced_connection_leaves_the_topic_alone() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, Vec::new());
            let mut state = session.state();
            let mut old = FakeCore::accept(server, "c_0000000000000001").await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            let (answered, answered_rx) = oneshot::channel();
            let sub = async {
                let subscribed = session.subscribe(vec!["test:x".to_string()]).await;
                let _ = answered.send(());
                subscribed
            };
            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            let switch = session.set_workspace("0123456789abcdef");
            let during = async {
                let on_old = old.request("lil/subscribe").await;
                let mut new = FakeCore::accept(server, "c_0000000000000001").await;
                let first = new.request("lil/subscribe").await;
                let fault = json!({
                    "jsonrpc": "2.0",
                    "id": on_old["id"],
                    "error": {"code": -32603, "message": "internal"},
                });
                old.send(&fault.to_string()).await;
                answered_rx.await.expect("the call ended");
                new.reply_subscribed(&first, &[("test:x", 7)]).await;
                new
            };
            let (subscribed, switched, new) = tokio::join!(sub, switch, during);
            subscribed.expect("an error of the replaced connection reached the caller");
            switched.expect("set_workspace");
            assert_eq!(session.last_seq("test:x"), Some(7));
            session.close().await;
            drop((old, new));
        });
    }

    /// A subscribe that gets its turn between the new connection becoming current and the
    /// setup's catch-up sends the topic on that connection itself, and has to end the earlier
    /// subscription (removed while the first subscribe was under way) on the way: the catch-up
    /// then finds nothing to do, and does not end the app's new subscription, whose events
    /// would be lost without a trace.
    #[test] // spec: APP-035, APP-033, ARCH-032
    fn a_subscribe_between_the_connection_and_the_catch_up_keeps_its_events() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let mut events = session.events();
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            fake.subscribed("test:r", 5).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            drop(fake);
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            let first = fake.request("lil/subscribe").await;
            let topic = || vec!["test:r".to_string()];
            session.unsubscribe(topic()).await.expect("unsubscribe");
            // A topic call in flight, whose turn the subscribe and then the catch-up wait for.
            let holder = session.shared.ops.lock().await;
            let sub = session.subscribe(topic());
            let driver = async {
                fake.reply_subscribed(&first, &[("test:r", 5)]).await;
                // Current now, with the catch-up queued behind the subscribe.
                while lock(&session.shared.client).is_none() {
                    tokio::task::yield_now().await;
                }
                drop(holder);
                let off = tokio::time::timeout(PROMPT, fake.request("lil/unsubscribe"))
                    .await
                    .expect("the earlier subscription was not ended first");
                fake.reply_unsubscribed(&off).await;
                let on = fake.request("lil/subscribe").await;
                assert_eq!(on["params"], json!({"topics": ["test:r"], "since_seq": {}}));
                fake.reply_subscribed(&on, &[("test:r", 5)]).await;
                fake.event("test:r", 6).await;
                fake
            };
            let (subscribed, mut fake) = tokio::join!(sub, driver);
            subscribed.expect("subscribe");
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;
            assert_eq!(next(&mut events).await, ("test:r".to_string(), 6));
            assert!(
                fake.quiet_for(Duration::from_millis(300)).await,
                "the catch-up ended the new subscription"
            );
            session.close().await;
            drop(fake);
        });
    }

    /// Within one connection a topic's event seqs only rise, so once the topic has a last seq
    /// (the seq a subscribe was answered with, or the newest event taken), an event at or
    /// below it is from an earlier subscription or a duplicate, and is dropped. A snapshot or
    /// a resync request is not an event of that kind and is kept.
    #[test] // spec: APP-035, ARCH-032
    fn an_event_at_or_below_the_last_seq_is_dropped() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        current_thread().block_on(async {
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let took_kind = |kind: EventKind, seq: u64, position: u64| {
                let event = EventParams {
                    topic: "test:r".to_string(),
                    seq,
                    kind,
                    data: Value::Null,
                };
                took(&session.shared, &event, position)
            };
            // No last seq yet: nothing to compare with.
            assert!(took_kind(EventKind::Event, 3, 1));
            assert_eq!(session.last_seq("test:r"), Some(3));
            assert!(!took_kind(EventKind::Event, 3, 2), "a duplicate");
            assert!(!took_kind(EventKind::Event, 2, 3), "an older event");
            assert_eq!(session.last_seq("test:r"), Some(3));
            assert!(took_kind(EventKind::Snapshot, 3, 4));
            assert!(took_kind(EventKind::ResyncRequired, 3, 5));
            assert!(took_kind(EventKind::Event, 4, 6));
            assert_eq!(session.last_seq("test:r"), Some(4));
            session.close().await;
        });
    }

    /// Subscribing a topic the session already has changes nothing about it: its last seq stays,
    /// so a connection lost right after still resumes the topic from that seq and the events
    /// published meanwhile are replayed, not lost.
    #[test] // spec: APP-035, ARCH-100
    fn subscribing_a_known_topic_keeps_its_cursor() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        let pipe = env.env.pipe_name();
        current_thread().block_on(async {
            let server = lil_platform::pipe::create_instance(&pipe, true).expect("the pipe");
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let mut state = session.state();
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            fake.subscribed("test:r", 5).await;
            wait_until(&mut state, |s| matches!(s, SessionState::Ready(_))).await;

            session
                .subscribe(vec!["test:r".to_string(), "test:r".to_string()])
                .await
                .expect("subscribe");
            assert_eq!(session.last_seq("test:r"), Some(5));

            let server = lil_platform::pipe::create_instance(&pipe, false).expect("the pipe");
            drop(fake);
            let mut fake = FakeCore::accept(server, "c_0000000000000001").await;
            let first = fake.request("lil/subscribe").await;
            assert_eq!(first["params"]["since_seq"], json!({"test:r": 5}));
            fake.reply_subscribed(&first, &[("test:r", 9)]).await;
            session.close().await;
            drop(fake);
        });
    }

    /// A live-only answer raises the floor to its seq when an event of the topic got in before
    /// it, and never lowers it.
    #[test] // spec: APP-035, ARCH-032
    fn a_live_only_answer_raises_the_last_seq() {
        let env = lil_testkit::TestEnv::new();
        let cfg = fake_config(&env);
        current_thread().block_on(async {
            let session = Session::start(cfg, vec!["test:r".to_string()]);
            let answer = |seq: u64| {
                [TopicState {
                    topic: "test:r".to_string(),
                    seq,
                }]
            };
            // An event that slipped in before the answer.
            lock(&session.shared.topics).insert("test:r".to_string(), Some(3));
            note_live(&session.shared, &answer(8));
            assert_eq!(session.last_seq("test:r"), Some(8));
            note_live(&session.shared, &answer(6));
            assert_eq!(session.last_seq("test:r"), Some(8));
            // No last seq yet: the answer's seq.
            lock(&session.shared.topics).insert("test:r".to_string(), None);
            note_live(&session.shared, &answer(2));
            assert_eq!(session.last_seq("test:r"), Some(2));
            session.close().await;
        });
    }
}
