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
//!   can't start, isn't trusted, or refused the hello).
//! - **Resubscribing** (APP-035, ARCH-100). Each topic keeps its last seq: the seq of the
//!   newest event the session took from it, else its seq when it was subscribed. Every
//!   connection after the first subscribes the topics with `since_seq` from their last seqs,
//!   so the core replays what was missed, or sends a `snapshot` (or `resync_required`) when
//!   its history no longer holds it. Seqs count per core instance: when the hello names
//!   another `instance_id` than the last connection's (the core restarted), each last seq
//!   starts again from 0 first, so the new core replays everything it still holds instead of
//!   skipping its first events. Whatever the app built from the old core it reloads then.
//! - **Events** reach [`Session::events`] in the core's order, across reconnects. A topic
//!   subscribed while there is no connection is subscribed on the next one.
//! - **Calls** ([`Session::call_tool`], [`Session::app_call`]) go out on the current
//!   connection and the session never sends one again: a call in flight when the connection
//!   is lost ends with [`ClientError::Closed`], and a call while there is no connection is
//!   [`ClientError::Closed`] at once. Retrying is the caller's, with the same idempotency key.
//! - **Switching workspace** (P4a-R34) is a new connection whose hello names the workspace. It
//!   replaces the current one only once the core accepted it, and it never changes
//!   `suite.toml`'s default.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lil_protocol::{EventParams, HelloResult, Outcome, ToolOutput, TopicState};
use serde_json::Value;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::{Client, ClientConfig, ClientError};

/// The first pause before another attempt.
const BACKOFF_FIRST: Duration = Duration::from_millis(250);
/// The longest pause between attempts.
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Events [`Session::events`] holds before the session keeps the rest itself.
const EVENTS_CAPACITY: usize = 1024;

/// Where the session stands (APP-034, APP-036, ARCH-100).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// The first attempt is under way.
    Connecting,
    /// Connected and subscribed; the core's hello.
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
    /// Held while topics are subscribed on a connection, so a topic added meanwhile is either
    /// in the task's subscribe or sent on the connection after it.
    subscribing: tokio::sync::Mutex<()>,
    /// [`Session::events`]' receiver, until it is taken.
    events: Mutex<Option<mpsc::Receiver<EventParams>>>,
}

/// Every update is a single step, so the data stays consistent after a panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Command {
    /// Adopt this connection (its hello named another workspace), then say so.
    Switch(Box<Client>, oneshot::Sender<()>),
    Close,
}

impl Session {
    /// Starts the session on the current tokio runtime, subscribed to `topics` (live events
    /// only: the app loads their current state itself, APP-033). The state starts as
    /// [`SessionState::Connecting`].
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn start(cfg: ClientConfig, topics: Vec<String>) -> Session {
        let (events_tx, events_rx) = mpsc::channel(EVENTS_CAPACITY);
        let shared = Arc::new(Shared {
            state: watch::Sender::new(SessionState::Connecting),
            cfg: Mutex::new(cfg),
            client: Mutex::new(None),
            topics: Mutex::new(topics.into_iter().map(|t| (t, None)).collect()),
            subscribing: tokio::sync::Mutex::new(()),
            events: Mutex::new(Some(events_rx)),
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

    /// An `app/*` method on the current connection ([`Client::app_call`]).
    pub async fn app_call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Outcome<Value>, ClientError> {
        self.client()?.app_call(method, params).await
    }

    /// Adds `topics` (live events only). Without a connection they are subscribed on the next
    /// one. A topic the core refuses is not added: the error, and none of this call's topics
    /// stay.
    pub async fn subscribe(&self, topics: Vec<String>) -> Result<(), ClientError> {
        let _subscribing = self.shared.subscribing.lock().await;
        let added: Vec<String> = {
            let mut known = lock(&self.shared.topics);
            topics
                .into_iter()
                .filter(|topic| known.insert(topic.clone(), None).is_none())
                .collect()
        };
        if added.is_empty() {
            return Ok(());
        }
        let Some(client) = lock(&self.shared.client).clone() else {
            return Ok(());
        };
        match client.subscribe_only(added.clone(), BTreeMap::new()).await {
            Ok(subscribed) => {
                note_subscribed(&self.shared, &subscribed.topics);
                Ok(())
            }
            // The connection went: the next one subscribes them.
            Err(ClientError::Closed) => Ok(()),
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
    pub async fn unsubscribe(&self, topics: Vec<String>) -> Result<(), ClientError> {
        let _subscribing = self.shared.subscribing.lock().await;
        let removed: Vec<String> = {
            let mut known = lock(&self.shared.topics);
            topics
                .into_iter()
                .filter(|topic| known.remove(topic).is_some())
                .collect()
        };
        if removed.is_empty() {
            return Ok(());
        }
        let Some(client) = lock(&self.shared.client).clone() else {
            return Ok(());
        };
        match client.unsubscribe_only(removed).await {
            Ok(()) | Err(ClientError::Closed) => Ok(()),
            Err(e) => Err(e),
        }
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
    /// returns that hello; later reconnects name it too. `suite.toml`'s default is never
    /// changed. A refusal (`workspace.unsupported` for a workspace the core doesn't serve,
    /// ARCH-090) is the error, and the session stays as it was.
    pub async fn set_workspace(&self, id: &str) -> Result<HelloResult, ClientError> {
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

    /// Stops the session: its connection closes, calls in flight end with
    /// [`ClientError::Closed`] and [`Session::events`] ends.
    pub async fn close(mut self) {
        let _ = self.commands.send(Command::Close);
        let _ = (&mut self.task).await;
    }

    fn client(&self) -> Result<Arc<Client>, ClientError> {
        lock(&self.shared.client).clone().ok_or(ClientError::Closed)
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
async fn run(shared: Arc<Shared>, mut commands: mpsc::UnboundedReceiver<Command>, mut out: Outbox) {
    // The core instance of the last connection.
    let mut instance: Option<String> = None;
    // Failed attempts since the last connection, a lost connection counting as one.
    let mut failed: u32 = 0;
    // A connection `set_workspace` made, to adopt instead of the next attempt.
    let mut offered: Option<(Box<Client>, oneshot::Sender<()>)> = None;
    loop {
        if offered.is_none() && failed > 0 {
            match until(tokio::time::sleep(backoff(failed)), &mut commands, &mut out).await {
                Step::Done(()) => {}
                Step::Command(Command::Switch(client, done)) => offered = Some((client, done)),
                Step::Command(Command::Close) | Step::Ended => break,
            }
        }
        let (attempt, done) = match offered.take() {
            Some((client, done)) => (Ok(*client), Some(done)),
            None => {
                let cfg = lock(&shared.cfg).clone();
                match until(Client::connect(&cfg), &mut commands, &mut out).await {
                    Step::Done(attempt) => (attempt, None),
                    Step::Command(Command::Switch(client, done)) => (Ok(*client), Some(done)),
                    Step::Command(Command::Close) | Step::Ended => break,
                }
            }
        };
        let live = match attempt {
            Ok(client) => establish(&shared, client, &mut instance).await,
            Err(e) => Err(e),
        };
        let live = match live {
            Ok(live) => live,
            Err(e) => {
                *lock(&shared.client) = None;
                failed += 1;
                report(&shared, &e, failed);
                if let Some(done) = done {
                    let _ = done.send(());
                }
                continue;
            }
        };
        failed = 0;
        shared
            .state
            .send_replace(SessionState::Ready(Box::new(live.client.hello().clone())));
        if let Some(done) = done {
            let _ = done.send(());
        }
        match forward(&shared, live.events, &mut commands, &mut out).await {
            Interrupt::Lost => {
                *lock(&shared.client) = None;
                failed = 1;
                set_state(&shared, SessionState::Reconnecting { attempt: failed });
            }
            // The current connection stays in use until the offered one is established.
            Interrupt::Switch(client, done) => offered = Some((client, done)),
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
    // Registered before the subscribe is written: replays may beat its reply.
    let events = client.conn().add_sink_all().ok_or(ClientError::Closed)?;
    let _subscribing = shared.subscribing.lock().await;
    let now = &client.hello().instance_id;
    let restarted = instance.as_ref().is_some_and(|old| old != now);
    let (topics, since) = {
        let mut known = lock(&shared.topics);
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
    if !topics.is_empty() {
        let subscribed = client.subscribe_only(topics, since).await?;
        note_subscribed(shared, &subscribed.topics);
    }
    *lock(&shared.client) = Some(client.clone());
    *instance = Some(now.clone());
    Ok(Live { client, events })
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
fn report(shared: &Shared, error: &ClientError, failed: u32) {
    let state = match error {
        ClientError::Incompatible { core_protocol } => SessionState::Incompatible {
            core_protocol: core_protocol.clone(),
        },
        ClientError::NotRunning | ClientError::Timeout | ClientError::Closed => {
            SessionState::Reconnecting { attempt: failed }
        }
        other => SessionState::Unavailable {
            reason: other.to_string(),
        },
    };
    set_state(shared, state);
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
    commands: &mut mpsc::UnboundedReceiver<Command>,
    out: &mut Outbox,
) -> Interrupt {
    loop {
        match until(events.recv(), commands, out).await {
            Step::Done(Some(event)) => {
                if took(shared, &event) {
                    out.push(event);
                }
            }
            Step::Done(None) => return Interrupt::Lost,
            Step::Command(Command::Switch(client, done)) => return Interrupt::Switch(client, done),
            Step::Command(Command::Close) | Step::Ended => return Interrupt::Close,
        }
    }
}

/// Records `event` as its topic's last seq; `false` for a topic no longer subscribed (one
/// still on its way after an unsubscribe).
fn took(shared: &Shared, event: &EventParams) -> bool {
    match lock(&shared.topics).get_mut(&event.topic) {
        Some(last) => {
            *last = Some(event.seq);
            true
        }
        None => false,
    }
}

enum Step<T> {
    Done(T),
    Command(Command),
    /// Every [`Session`] handle is gone.
    Ended,
}

/// Runs `fut` until it finishes or a command comes, handing kept events to the app meanwhile.
async fn until<F: Future>(
    fut: F,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    out: &mut Outbox,
) -> Step<F::Output> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => return command.map_or(Step::Ended, Step::Command),
            () = out.flush_one(), if out.has_backlog() => {}
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
    use lil_protocol::{CallerKind, CoreInfo, SubscribeResult, schema_hash};
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::windows::named_pipe::NamedPipeServer;

    use super::*;
    use crate::StartPolicy;

    /// A fake core on the pipe: answers one hello as core instance `instance`, then one
    /// `lil/subscribe`, whose params it returns, with the topic at `seq`, then sends `events`.
    struct FakeCore {
        lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<NamedPipeServer>>>,
        write: tokio::io::WriteHalf<NamedPipeServer>,
    }

    impl FakeCore {
        async fn accept(server: NamedPipeServer, instance: &str) -> FakeCore {
            server.connect().await.expect("accept");
            let (read, write) = tokio::io::split(server);
            let mut fake = FakeCore {
                lines: BufReader::new(read).lines(),
                write,
            };
            let hello = fake.request("core/hello").await;
            let result = HelloResult {
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
            };
            fake.reply(&hello, &Outcome::Ok(result)).await;
            fake
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
            let result = SubscribeResult {
                topics: vec![TopicState {
                    topic: topic.into(),
                    seq,
                }],
            };
            self.reply(&request, &Outcome::Ok(result)).await;
            request["params"].clone()
        }

        async fn event(&mut self, topic: &str, seq: u64) {
            let params = json!({"topic": topic, "seq": seq, "kind": "event", "data": {"n": seq}});
            let line = lil_protocol::encode_notification(lil_protocol::method::EVENT, &params);
            self.send(&line).await;
        }
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
}
