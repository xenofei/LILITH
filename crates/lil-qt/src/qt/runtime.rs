//! The bridge's background side (APP-024, APP-026, APP-029): a tokio runtime that owns the
//! [`Session`], runs every call, pumps the events through the throttle and hands what the GUI
//! must apply to the GUI thread. The GUI thread never waits on the core.
//!
//! - **Delivery** ([`Delivery`]). Everything for the GUI goes through one queue, applied in
//!   order on the GUI thread by the `Core` object (`CxxQtThread::queue`); a batch whose object
//!   is gone is dropped (APP-026). One batch is in Qt's queue at a time: what comes meanwhile
//!   waits here, and a frozen GUI makes it grow only up to 1,024 events or 64 MiB of them,
//!   past which the waiting events fold into one `resync_required` per topic (call outcomes are
//!   never dropped).
//! - **Calls.** Each `Core.call` mints its own idempotency key (`ik_…`) and trace id
//!   (`tr_<16 hex>`, ARCH-118) and makes one `tools/call`. The session never resends a call, and
//!   neither does this, with one exception: a call that returned `pending_confirmation` waits on
//!   its confirmation ([`PendingCall`]) and, once it is approved, is sent again **once** with its
//!   key, the retry token (APP-029). That resend is bound to the workspace the call was made in:
//!   after a switch it is not made, and the call ends with `confirm.expired`. A resend that gets
//!   `NotConnected` (nothing sent) or `Closed` (maybe sent) is made again on the next connection
//!   to that workspace, with the same key, at most [`RESEND_ATTEMPTS`] times in all: the core
//!   runs a key once. `Core.appCall` and `Core.setWorkspace` are never sent again.
//! - **Confirmations.** The session is always subscribed to `confirmations`, which the pending
//!   calls watch. A `resync_required` on it (a reconnect to another core, an overflow) makes a
//!   pending call read `app/confirmations.list`, so an answer given while the app was away is
//!   still found (Review Focus 1). That read-only list is the one `app_call` the bridge makes
//!   again: after a failure, 1 s later at first, doubling to 30 s, until it is read.
//! - **Events.** Each topic counts its QML subscribers ([`Runtime::subscribe`], [`Topics`]): the
//!   session subscribes it for the first and unsubscribes it after the last (`confirmations`
//!   never), so a panel that stops following a topic never stops another's events. A subscribe
//!   the session refused or timed out is sent again by the topic's next subscriber or on the
//!   next connection. The session's
//!   subscribes and unsubscribes go out one at a time, in the order QML made them, through one
//!   task, so an unsubscribe and a subscribe of the same topic in a row end subscribed. The
//!   followed topics' events reach QML through the throttle ([`Coalescer`], APP-027); a topic
//!   no longer followed loses its waiting update. `confirmations` reaches QML only when QML
//!   subscribed to it. Before the throttle, each event goes through the live-preview feed
//!   ([`super::preview_feed`]), which takes a preview's frame out of it for the image provider.
//! - **Shutdown.** Dropping the runtime gives its tasks 100 ms to end; the session is dropped
//!   with the last of them, which closes its connection.

// `ClientError::Refused` carries the core's `ErrorBody` by value, as in lil-client itself.
#![allow(clippy::result_large_err)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lil_client::session::{Session, SessionState, UnavailableCause};
use lil_client::{ClientConfig, ClientError};
use lil_protocol::codes::{CONFIRM_EXPIRED, PARAMS_INVALID};
use lil_protocol::{
    CLIENT_QUEUE, ErrorBody, EventKind, EventParams, HelloResult, NO_WORKSPACE, Outcome, ToolOutput,
};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, broadcast, mpsc};
use tokio::task::AbortHandle;

use crate::requests::client_error;
use crate::retry::{CONFIRMATIONS, PendingCall, PendingStep};
use crate::throttle::Coalescer;
use crate::topics::Topics;

/// How many times an approved call is sent with its retry token before it ends
/// `core.unreachable`: each time the connection was lost before an answer.
pub const RESEND_ATTEMPTS: u32 = 3;
/// Events waiting for the GUI before they fold into resyncs, as the session's own backlog.
const GUI_EVENTS_MAX: usize = CLIENT_QUEUE;
const GUI_EVENT_BYTES_MAX: usize = 64 * 1024 * 1024;
/// A pending call whose snapshot of the confirmations failed tries again after this, doubling
/// up to [`SNAPSHOT_RETRY_MAX`].
const SNAPSHOT_RETRY_FIRST: Duration = Duration::from_secs(1);
const SNAPSHOT_RETRY_MAX: Duration = Duration::from_secs(30);
/// How long a dropped runtime waits for its tasks.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(100);
/// How often the pump forgets the throttle's idle streams.
const PRUNE_EVERY: Duration = Duration::from_secs(10);
/// What `confirmations` events a pending call can fall behind by before it reads the list.
const CONFIRMATIONS_BUFFER: usize = 256;

/// Every update is a single step, so the data stays consistent after a panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the GUI thread applies to the `Core` object.
#[derive(Debug, Clone)]
pub enum Delivery {
    /// The connection changed (APP-028's properties).
    State(StateView),
    /// A request finished ok: its result as JSON.
    Finished { id: String, json: String },
    /// A request failed: its error body as JSON.
    Failed { id: String, json: String },
    /// A request got `pending_confirmation`: the pending confirmation as JSON. `open`: the
    /// request stays open for its automatic retry (a tool call); else it has ended.
    Pending {
        id: String,
        json: String,
        open: bool,
    },
    /// An event of a topic QML follows.
    Event(EventParams),
}

/// The `Core` properties a session state sets; `None` leaves one as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct StateView {
    /// `connecting`, `ready`, `reconnecting`, `incompatible` or `unavailable`.
    pub connection: &'static str,
    pub core_protocol: Option<String>,
    /// The hello's workspace as JSON (`None` inside: no workspace).
    pub workspace: Option<Option<String>>,
    /// While unavailable, why as JSON: `{reason, cause, code?, retry?}`.
    pub unavailable: Option<String>,
}

impl StateView {
    pub fn of(state: &SessionState) -> StateView {
        let only = |connection| StateView {
            connection,
            core_protocol: None,
            workspace: None,
            unavailable: None,
        };
        match state {
            SessionState::Connecting => only("connecting"),
            SessionState::Reconnecting { .. } => only("reconnecting"),
            SessionState::Unavailable { reason, cause } => StateView {
                unavailable: Some(unavailable_json(reason, cause)),
                ..only("unavailable")
            },
            SessionState::Incompatible { core_protocol } => StateView {
                connection: "incompatible",
                core_protocol: Some(core_protocol.clone()),
                workspace: None,
                unavailable: None,
            },
            SessionState::Ready(hello) => StateView {
                connection: "ready",
                core_protocol: Some(hello.protocol.clone()),
                workspace: Some(hello.workspace.as_ref().map(to_json)),
                unavailable: None,
            },
        }
    }
}

/// Hands a batch to the GUI thread; `Err` once its object is gone.
pub type PostFn = dyn Fn(Vec<Delivery>) -> Result<(), ()> + Send + Sync;

/// The GUI queue (module docs, "Delivery").
struct GuiOut {
    post: Box<PostFn>,
    state: Mutex<GuiState>,
    /// Woken when the GUI applied a batch and more waits.
    more: Notify,
}

#[derive(Default)]
struct GuiState {
    waiting: VecDeque<Delivery>,
    events: usize,
    event_bytes: usize,
    in_flight: bool,
    gone: bool,
}

impl GuiOut {
    fn push(&self, delivery: Delivery) {
        let batch = {
            let mut st = lock(&self.state);
            if st.gone {
                return;
            }
            if let Delivery::Event(ev) = &delivery {
                st.events += 1;
                st.event_bytes += event_bytes(ev);
            }
            st.waiting.push_back(delivery);
            if st.events > GUI_EVENTS_MAX || st.event_bytes > GUI_EVENT_BYTES_MAX {
                fold(&mut st);
            }
            if st.in_flight {
                return;
            }
            st.in_flight = true;
            take(&mut st)
        };
        self.send(batch);
    }

    /// The GUI applied a batch (on the GUI thread): the next one goes from the flusher.
    fn drained(&self) {
        let mut st = lock(&self.state);
        if st.waiting.is_empty() {
            st.in_flight = false;
        } else {
            drop(st);
            self.more.notify_one();
        }
    }

    /// Sends what waits (the flusher, after [`GuiOut::drained`]).
    fn flush(&self) {
        let batch = {
            let mut st = lock(&self.state);
            if st.gone {
                return;
            }
            if st.waiting.is_empty() {
                st.in_flight = false;
                return;
            }
            take(&mut st)
        };
        self.send(batch);
    }

    fn send(&self, batch: Vec<Delivery>) {
        if (self.post)(batch).is_err() {
            // The object is gone (APP-026): nothing is delivered again.
            let mut st = lock(&self.state);
            st.gone = true;
            st.waiting.clear();
        }
    }
}

fn take(st: &mut GuiState) -> Vec<Delivery> {
    st.events = 0;
    st.event_bytes = 0;
    st.waiting.drain(..).collect()
}

/// The waiting events give way to one `resync_required` per topic at its newest seq, after
/// every other delivery (which are never dropped).
fn fold(st: &mut GuiState) {
    let mut newest: BTreeMap<String, u64> = BTreeMap::new();
    st.waiting.retain(|d| match d {
        Delivery::Event(ev) => {
            newest.insert(ev.topic.clone(), ev.seq);
            false
        }
        _ => true,
    });
    st.events = newest.len();
    st.event_bytes = 0;
    for (topic, seq) in newest {
        let ev = EventParams {
            topic,
            seq,
            kind: EventKind::ResyncRequired,
            data: Value::Null,
        };
        st.event_bytes += event_bytes(&ev);
        st.waiting.push_back(Delivery::Event(ev));
    }
}

/// What an event weighs here: its topic and its data as JSON, counted without a copy.
fn event_bytes(ev: &EventParams) -> usize {
    /// Counts what is written, keeping nothing.
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Writing a `Value` to a writer that never fails can't fail.
    let _ = serde_json::to_writer(&mut count, &ev.data);
    ev.topic.len() + count.0
}

/// A change to the session's topics, in QML's order.
enum TopicEdit {
    /// With the generation `Topics::subscribe` gave it.
    Subscribe(String, u64),
    Unsubscribe(String),
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("protocol values serialise")
}

/// What the runtime's tasks share.
struct Shared {
    session: Option<Session>,
    gui: GuiOut,
    /// The topics QML follows, with how many subscribers each has.
    qml_topics: Mutex<Topics>,
    /// The session's topic changes, to the one task that makes them in order.
    edits: mpsc::UnboundedSender<TopicEdit>,
    /// Topics no longer followed, for the pump's throttle to forget.
    forgotten: mpsc::UnboundedSender<String>,
    /// The `confirmations` events, for the pending calls.
    confirmations: broadcast::Sender<EventParams>,
}

impl Shared {
    fn session(&self) -> Result<&Session, ClientError> {
        self.session.as_ref().ok_or(ClientError::NotConnected)
    }

    fn follows(&self, topic: &str) -> bool {
        lock(&self.qml_topics).follows(topic)
    }
}

/// The background runtime (module docs). Dropping it ends the session and every call.
pub struct Runtime {
    rt: Option<tokio::runtime::Runtime>,
    shared: Option<Arc<Shared>>,
}

impl Runtime {
    /// Starts the runtime, and the session when `cfg` is given (without one, every call fails
    /// `core.unreachable` and the connection stays `unavailable`). `post` hands batches to the
    /// GUI thread.
    ///
    /// # Errors
    /// When the runtime's threads can't be started.
    pub fn start(cfg: Option<ClientConfig>, post: Box<PostFn>) -> std::io::Result<Runtime> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("lil-core-session")
            .enable_all()
            .build()?;
        let session = cfg.map(|cfg| {
            let _entered = rt.enter();
            Session::start(cfg, vec![CONFIRMATIONS.to_string()])
        });
        let (confirmations, _) = broadcast::channel(CONFIRMATIONS_BUFFER);
        let (edits, edits_rx) = mpsc::unbounded_channel();
        let (forgotten, forgotten_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            session,
            gui: GuiOut {
                post,
                state: Mutex::new(GuiState::default()),
                more: Notify::new(),
            },
            qml_topics: Mutex::new(Topics::new()),
            edits,
            forgotten,
            confirmations,
        });
        match &shared.session {
            Some(session) => {
                rt.spawn(watch_state(shared.clone(), session.state()));
                rt.spawn(pump(shared.clone(), session.events(), forgotten_rx));
                rt.spawn(edit_topics(shared.clone(), edits_rx));
            }
            None => shared.gui.push(Delivery::State(StateView {
                connection: "unavailable",
                core_protocol: None,
                workspace: Some(None),
                unavailable: Some(
                    json!({"reason": "This process has no core session.", "cause": "other"})
                        .to_string(),
                ),
            })),
        }
        rt.spawn(flusher(shared.clone()));
        Ok(Runtime {
            rt: Some(rt),
            shared: Some(shared),
        })
    }

    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().expect("the runtime lives until drop")
    }

    fn shared(&self) -> &Arc<Shared> {
        self.shared.as_ref().expect("the runtime lives until drop")
    }

    /// `Core.call`: `tools/call` of `tool` with `args_json` (a JSON object, or empty for
    /// none), its outcome delivered for `id`.
    pub fn call(&self, id: String, tool: String, args_json: &str) -> Option<AbortHandle> {
        let args = match parse_object(args_json) {
            Ok(args) => args,
            Err(why) => {
                self.fail(id, &why);
                return None;
            }
        };
        let shared = self.shared().clone();
        Some(
            self.rt()
                .spawn(tool_call(shared, id, tool, args))
                .abort_handle(),
        )
    }

    /// `Core.appCall`: the `app/*` method `method` with `params_json`, never sent again.
    pub fn app_call(&self, id: String, method: String, params_json: &str) -> Option<AbortHandle> {
        let params = match parse_object(params_json) {
            Ok(params) => Value::Object(params),
            Err(why) => {
                self.fail(id, &why);
                return None;
            }
        };
        let shared = self.shared().clone();
        let task = async move {
            let trace = lil_client::mint_id("t");
            let outcome = match shared.session() {
                Ok(session) => session.app_call(&method, params).await,
                Err(e) => Err(e),
            };
            let delivery = match outcome {
                Ok(Outcome::Ok(v)) => Delivery::Finished {
                    id,
                    json: to_json(&v),
                },
                Ok(Outcome::Error { error }) => Delivery::Failed {
                    id,
                    json: to_json(&error),
                },
                Ok(Outcome::PendingConfirmation(p)) => Delivery::Pending {
                    id,
                    json: to_json(&p),
                    open: false,
                },
                Err(e) => Delivery::Failed {
                    id,
                    json: to_json(&client_error(&e, &trace)),
                },
            };
            shared.gui.push(delivery);
        };
        Some(self.rt().spawn(task).abort_handle())
    }

    /// `Core.setWorkspace`: a new hello naming the workspace `ws` (P4a-R34); the hello, or
    /// the refusal (`workspace.unsupported`), is delivered for `id`.
    pub fn set_workspace(&self, id: String, ws: String) -> Option<AbortHandle> {
        let shared = self.shared().clone();
        let task = async move {
            let outcome = match shared.session() {
                Ok(session) => session.set_workspace(&ws).await,
                Err(e) => Err(e),
            };
            let delivery = match outcome {
                Ok(hello) => Delivery::Finished {
                    id,
                    json: to_json(&hello),
                },
                Err(e) => Delivery::Failed {
                    id,
                    json: to_json(&client_error(&e, &lil_client::mint_id("t"))),
                },
            };
            shared.gui.push(delivery);
        };
        Some(self.rt().spawn(task).abort_handle())
    }

    /// `Core.useDefaultWorkspace`: a new hello on what `suite.toml` names as the default now, or
    /// on none when there is no usable default. Its outcome is delivered for `id`.
    pub fn use_default_workspace(&self, id: String) -> Option<AbortHandle> {
        let shared = self.shared().clone();
        let task = async move {
            let outcome = match shared.session() {
                Ok(session) => {
                    let env = session.env();
                    let ws = tokio::task::spawn_blocking(move || {
                        lil_client::resolve_workspace(&env, None, &env.local_dir())
                            .map_or_else(|_| NO_WORKSPACE.to_string(), |ws| ws.id)
                    })
                    .await
                    .unwrap_or_else(|_| NO_WORKSPACE.to_string());
                    session.set_workspace(&ws).await
                }
                Err(e) => Err(e),
            };
            let delivery = match outcome {
                Ok(hello) => Delivery::Finished {
                    id,
                    json: to_json(&hello),
                },
                Err(e) => Delivery::Failed {
                    id,
                    json: to_json(&client_error(&e, &lil_client::mint_id("t"))),
                },
            };
            shared.gui.push(delivery);
        };
        Some(self.rt().spawn(task).abort_handle())
    }

    /// Delivers `params.invalid` for `id`: its arguments weren't a JSON object.
    fn fail(&self, id: String, why: &str) {
        let error = ErrorBody::new(&PARAMS_INVALID, why, lil_client::mint_id("t"));
        self.shared().gui.push(Delivery::Failed {
            id,
            json: to_json(&error),
        });
    }

    /// One more QML subscriber of `topic`; the first makes the session subscribe it (live
    /// events only; module docs, "Events").
    pub fn subscribe(&self, topic: String) {
        let send = lock(&self.shared().qml_topics).subscribe(&topic);
        if let Some(generation) = send
            && topic != CONFIRMATIONS
        {
            // The editor task ends only with the runtime.
            let _ = self
                .shared()
                .edits
                .send(TopicEdit::Subscribe(topic, generation));
        }
    }

    /// One QML subscriber of `topic` fewer; after the last the session unsubscribes it, except
    /// `confirmations`, which the pending calls watch. An unsubscribe of a topic QML doesn't
    /// follow does nothing.
    pub fn unsubscribe(&self, topic: String) {
        let last = lock(&self.shared().qml_topics).unsubscribe(&topic);
        if last {
            let _ = self.shared().forgotten.send(topic.clone());
            if topic != CONFIRMATIONS {
                let _ = self.shared().edits.send(TopicEdit::Unsubscribe(topic));
            }
        }
    }

    /// `Session::retry_now` (the Unavailable banner's "Try again").
    pub fn retry_now(&self) {
        if let Some(session) = &self.shared().session {
            session.retry_now();
        }
    }

    /// The session's last seq of `topic`, if it has one.
    pub fn last_seq(&self, topic: &str) -> Option<u64> {
        self.shared().session.as_ref()?.last_seq(topic)
    }

    /// The GUI thread applied a batch.
    pub fn drained(&self) {
        self.shared().gui.drained();
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let Some(rt) = self.rt.take() else { return };
        {
            // Dropped inside the runtime's context: the tasks hold the rest of `Shared`.
            let _entered = rt.enter();
            if let Some(shared) = self.shared.take() {
                lock(&shared.gui.state).gone = true;
                drop(shared);
            }
        }
        // The tasks end, and the last one drops the session, closing its connection, within a
        // short bound: the GUI thread never waits on the core itself (APP-024).
        rt.shutdown_timeout(SHUTDOWN_WAIT);
    }
}

/// An object of JSON text; empty text is an empty object.
fn parse_object(text: &str) -> Result<Map<String, Value>, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("The arguments must be a JSON object.".to_string()),
        Err(e) => Err(format!("The arguments aren't valid JSON: {e}")),
    }
}

/// [`StateView::unavailable`].
fn unavailable_json(reason: &str, cause: &UnavailableCause) -> String {
    let mut out = json!({"reason": reason});
    out["cause"] = json!(match cause {
        UnavailableCause::StartFailed(_) => "start_failed",
        UnavailableCause::CannotStart => "cannot_start",
        UnavailableCause::Untrusted => "untrusted",
        UnavailableCause::Refused(_) => "refused",
        UnavailableCause::Other => "other",
    });
    if let UnavailableCause::Refused(error) = cause {
        // The core's own words name the fix.
        out["reason"] = json!(error.message);
        out["code"] = json!(error.code);
        out["retry"] = serde_json::to_value(error.retry).unwrap_or(Value::Null);
    }
    out.to_string()
}

/// The workspace id a hello names (`none` for none).
fn workspace_of(hello: &HelloResult) -> String {
    hello
        .workspace
        .as_ref()
        .map_or_else(|| NO_WORKSPACE.to_string(), |w| w.id.clone())
}

/// The workspace of the current connection, if the session is ready.
fn ready_workspace(state: &SessionState) -> Option<String> {
    match state {
        SessionState::Ready(hello) => Some(workspace_of(hello)),
        _ => None,
    }
}

/// Makes QML's topic changes on the session, one at a time, in order (module docs, "Events").
async fn edit_topics(shared: Arc<Shared>, mut edits: mpsc::UnboundedReceiver<TopicEdit>) {
    let Some(session) = shared.session.as_ref() else {
        return;
    };
    while let Some(edit) = edits.recv().await {
        match edit {
            TopicEdit::Subscribe(topic, generation) => {
                if let Err(e) = session.subscribe(vec![topic.clone()]).await {
                    // The session dropped it; QML still follows it, so its next subscriber, or
                    // the next connection, sends it again (re-review R-M1).
                    tracing::warn!(topic, error = %e, "the subscribe failed");
                    lock(&shared.qml_topics).failed(&topic, generation);
                }
            }
            TopicEdit::Unsubscribe(topic) => {
                if let Err(e) = session.unsubscribe(vec![topic.clone()]).await {
                    tracing::warn!(topic, error = %e, "the unsubscribe failed");
                }
            }
        }
    }
}

async fn flusher(shared: Arc<Shared>) {
    loop {
        shared.gui.more.notified().await;
        shared.gui.flush();
    }
}

async fn watch_state(shared: Arc<Shared>, mut state: tokio::sync::watch::Receiver<SessionState>) {
    loop {
        let (view, ready) = {
            let st = state.borrow_and_update();
            (StateView::of(&st), matches!(*st, SessionState::Ready(_)))
        };
        shared.gui.push(Delivery::State(view));
        if ready {
            // A new connection: the subscribes that failed are sent again (re-review R-M1).
            let unheld = lock(&shared.qml_topics).unheld();
            for (topic, generation) in unheld {
                if topic != CONFIRMATIONS {
                    let _ = shared.edits.send(TopicEdit::Subscribe(topic, generation));
                }
            }
        }
        if state.changed().await.is_err() {
            return;
        }
    }
}

/// The event pump: every session event to the pending calls (`confirmations`) and, through
/// the throttle, to QML (module docs).
async fn pump(
    shared: Arc<Shared>,
    mut events: mpsc::Receiver<EventParams>,
    mut forgotten: mpsc::UnboundedReceiver<String>,
) {
    let mut throttle = Coalescer::new();
    let mut next_prune = std::time::Instant::now() + PRUNE_EVERY;
    loop {
        let now = std::time::Instant::now();
        if now >= next_prune {
            throttle.prune(now);
            next_prune = now + PRUNE_EVERY;
        }
        let due = throttle.next_due();
        let wake = async {
            match due {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { return };
                if ev.topic == CONFIRMATIONS {
                    // No pending call listens: nothing to tell.
                    let _ = shared.confirmations.send(ev.clone());
                }
                if !shared.follows(&ev.topic) {
                    continue;
                }
                let mut ev = ev;
                super::preview_feed::on_event(&mut ev);
                for ev in throttle.push(ev, std::time::Instant::now()) {
                    shared.gui.push(Delivery::Event(ev));
                }
            }
            () = wake => {
                for ev in throttle.due(std::time::Instant::now()) {
                    if shared.follows(&ev.topic) {
                        shared.gui.push(Delivery::Event(ev));
                    }
                }
            }
            Some(topic) = forgotten.recv() => {
                throttle.forget(&topic);
                super::preview_feed::forgotten(&topic);
            }
        }
    }
}

/// One `Core.call`, its automatic retry included (module docs, "Calls").
async fn tool_call(shared: Arc<Shared>, id: String, tool: String, args: Map<String, Value>) {
    let key = lil_client::mint_id("ik");
    let trace = lil_client::mint_id("tr");
    tracing::debug!(request = %id, tool, trace_id = %trace, "tools/call");
    let Ok(session) = shared.session() else {
        let error = client_error(&ClientError::NotConnected, &trace);
        shared.gui.push(Delivery::Failed {
            id,
            json: to_json(&error),
        });
        return;
    };
    // Subscribed before the call, so no answer to its confirmation can be missed.
    let mut confirmations = shared.confirmations.subscribe();
    let workspace = ready_workspace(&session.state().borrow());
    let mut outcome = session
        .call_tool(&tool, args.clone(), Some(key), Some(trace.clone()))
        .await;
    let mut resent: BTreeSet<String> = BTreeSet::new();
    loop {
        let pending = match outcome {
            Ok(Outcome::PendingConfirmation(pending)) => pending,
            done => {
                shared.gui.push(finished(id, done, &trace));
                return;
            }
        };
        if resent.contains(&pending.confirmation_id) {
            // Approved and sent once already: never a second time.
            let error = ErrorBody::new(
                &CONFIRM_EXPIRED,
                "The core asked for the same confirmation again after it was approved; the \
                 call wasn't sent again.",
                trace.clone(),
            );
            shared.gui.push(Delivery::Failed {
                id,
                json: to_json(&error),
            });
            return;
        }
        shared.gui.push(Delivery::Pending {
            id: id.clone(),
            json: to_json(&pending),
            open: true,
        });
        // The workspace the call was made in: the connection's when it was sent.
        let Some(workspace) = workspace
            .clone()
            .or_else(|| ready_workspace(&session.state().borrow()))
        else {
            shared.gui.push(settled(id, left_workspace(&trace), &trace));
            return;
        };
        let call = PendingCall::new(pending.confirmation_id.clone(), pending.retry_token)
            .traced(trace.clone());
        match wait_for_answer(session, &mut confirmations, call, &workspace, &trace).await {
            PendingStep::Resend { retry_token } => {
                resent.insert(pending.confirmation_id);
                tracing::info!(request = %id, tool, trace_id = %trace, "the confirmation was approved: the call is sent again with its retry token");
                outcome = resend(session, &tool, &args, &retry_token, &trace, &workspace).await;
            }
            step @ (PendingStep::Settle(_) | PendingStep::Wait) => {
                shared.gui.push(settled(id, step, &trace));
                return;
            }
        }
    }
}

/// The delivery for a call's final outcome.
fn finished(
    id: String,
    outcome: Result<Outcome<ToolOutput>, ClientError>,
    trace: &str,
) -> Delivery {
    match outcome {
        Ok(Outcome::Ok(out)) => Delivery::Finished {
            id,
            json: to_json(&out),
        },
        Ok(Outcome::Error { error }) => Delivery::Failed {
            id,
            json: to_json(&error),
        },
        Ok(Outcome::PendingConfirmation(p)) => Delivery::Pending {
            id,
            json: to_json(&p),
            open: false,
        },
        Err(e) => Delivery::Failed {
            id,
            json: to_json(&client_error(&e, trace)),
        },
    }
}

/// The delivery for a pending call that ended without a resend.
fn settled(id: String, step: PendingStep, trace: &str) -> Delivery {
    let error = match step {
        PendingStep::Settle(v) => v["error"].clone(),
        // Never returned by `wait_for_answer`; ended all the same, never left open.
        PendingStep::Resend { .. } | PendingStep::Wait => {
            let error = ErrorBody::new(
                &CONFIRM_EXPIRED,
                "The call stopped waiting for its confirmation.",
                trace,
            );
            serde_json::to_value(error).expect("an error body serialises")
        }
    };
    Delivery::Failed {
        id,
        json: error.to_string(),
    }
}

/// The error a pending call ends with when the session left its workspace.
fn left_workspace(trace: &str) -> PendingStep {
    let error = ErrorBody::new(
        &CONFIRM_EXPIRED,
        "The workspace changed while the call waited for its confirmation, so it wasn't sent again.",
        trace,
    );
    let outcome: Outcome<Value> = Outcome::Error { error };
    PendingStep::Settle(serde_json::to_value(outcome).expect("an outcome serialises"))
}

/// Waits until `call`'s confirmation is answered (or the session leaves `workspace`).
async fn wait_for_answer(
    session: &Session,
    confirmations: &mut broadcast::Receiver<EventParams>,
    mut call: PendingCall,
    workspace: &str,
    trace: &str,
) -> PendingStep {
    let mut state = session.state();
    let mut need_snapshot = false;
    let mut backoff = SNAPSHOT_RETRY_FIRST;
    loop {
        let ready = ready_workspace(&state.borrow_and_update());
        let mut failed = false;
        match ready.as_deref() {
            Some(ws) if ws != workspace => return left_workspace(trace),
            Some(_) if need_snapshot => match snapshot(session).await {
                Ok((pending, answered)) => {
                    need_snapshot = false;
                    backoff = SNAPSHOT_RETRY_FIRST;
                    let step = call.on_snapshot(&pending, &answered);
                    if step != PendingStep::Wait {
                        return step;
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, confirmation = call.confirmation_id(), "the confirmations snapshot failed; it is read again");
                    failed = true;
                }
            },
            _ => {}
        }
        let wait = backoff;
        if failed {
            backoff = (backoff * 2).min(SNAPSHOT_RETRY_MAX);
        }
        let retry = async {
            if need_snapshot {
                tokio::time::sleep(wait).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            ev = confirmations.recv() => match ev {
                Ok(ev) if PendingCall::wants_snapshot(&ev) => need_snapshot = true,
                Ok(ev) => {
                    let step = call.on_event(&ev);
                    if step != PendingStep::Wait {
                        return step;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => need_snapshot = true,
                Err(broadcast::error::RecvError::Closed) => return call.on_close(),
            },
            changed = state.changed() => {
                if changed.is_err() {
                    return call.on_close();
                }
            }
            () = retry => {}
        }
    }
}

/// The ids of the pending confirmations, and the answered ones with their state.
async fn snapshot(session: &Session) -> Result<(Vec<String>, Vec<(String, String)>), ClientError> {
    let list = async |which: &str| -> Result<Vec<Value>, ClientError> {
        let params = json!({"state": which, "limit": 500});
        match session.app_call("app/confirmations.list", params).await? {
            Outcome::Ok(v) => Ok(v["confirmations"].as_array().cloned().unwrap_or_default()),
            Outcome::Error { error } => Err(ClientError::Refused(error)),
            Outcome::PendingConfirmation(_) => Ok(Vec::new()),
        }
    };
    let field = |c: &Value, name: &str| c[name].as_str().unwrap_or_default().to_string();
    let pending = list("pending")
        .await?
        .iter()
        .map(|c| field(c, "id"))
        .collect();
    let answered = list("history")
        .await?
        .iter()
        .map(|c| (field(c, "id"), field(c, "state")))
        .collect();
    Ok((pending, answered))
}

/// Sends the approved call again with its retry token, in `workspace` only (module docs).
async fn resend(
    session: &Session,
    tool: &str,
    args: &Map<String, Value>,
    retry_token: &str,
    trace: &str,
    workspace: &str,
) -> Result<Outcome<ToolOutput>, ClientError> {
    let mut state = session.state();
    let mut last = ClientError::NotConnected;
    for _ in 0..RESEND_ATTEMPTS {
        // The next connection to the call's workspace.
        loop {
            let ready = ready_workspace(&state.borrow_and_update());
            match ready.as_deref() {
                Some(ws) if ws == workspace => break,
                Some(_) => {
                    let PendingStep::Settle(v) = left_workspace(trace) else {
                        unreachable!()
                    };
                    let error: ErrorBody = serde_json::from_value(v["error"].clone())
                        .expect("an error body reads back");
                    return Ok(Outcome::Error { error });
                }
                None => {
                    if state.changed().await.is_err() {
                        return Err(ClientError::Closed);
                    }
                }
            }
        }
        match session
            .call_tool(
                tool,
                args.clone(),
                Some(retry_token.to_string()),
                Some(trace.to_string()),
            )
            .await
        {
            // Nothing sent, or maybe sent: the key makes another send safe.
            Err(e @ (ClientError::NotConnected | ClientError::Closed)) => {
                last = e;
                // Wait for the state to move on before trying again.
                if matches!(*state.borrow(), SessionState::Ready(_))
                    && state.changed().await.is_err()
                {
                    return Err(ClientError::Closed);
                }
            }
            other => return other,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(topic: &str, seq: u64, bytes: usize) -> Delivery {
        Delivery::Event(EventParams {
            topic: topic.into(),
            seq,
            kind: EventKind::Event,
            data: json!({"pad": "p".repeat(bytes)}),
        })
    }

    /// A refused hello reaches the banner in the core's words, with what it takes to get out;
    /// any other cause keeps the client's reason.
    #[test] // spec: ARCH-100
    fn an_unavailable_state_carries_its_cause() {
        let refused = ErrorBody::new(
            &lil_protocol::codes::WORKSPACE_UNSUPPORTED,
            "This core serves only the default workspace, \"B\".",
            "t_1",
        );
        let view = StateView::of(&SessionState::Unavailable {
            reason: "The core refused the hello.".into(),
            cause: UnavailableCause::Refused(Box::new(refused)),
        });
        assert_eq!(view.connection, "unavailable");
        let shown: Value = serde_json::from_str(view.unavailable.as_deref().unwrap()).unwrap();
        assert_eq!(shown["cause"], "refused");
        assert_eq!(shown["code"], "workspace.unsupported");
        assert_eq!(shown["retry"], "after_resolve");
        assert_eq!(
            shown["reason"],
            "This core serves only the default workspace, \"B\"."
        );

        let view = StateView::of(&SessionState::Unavailable {
            reason: "Cannot start the core.".into(),
            cause: UnavailableCause::CannotStart,
        });
        let shown: Value = serde_json::from_str(view.unavailable.as_deref().unwrap()).unwrap();
        assert_eq!(shown["cause"], "cannot_start");
        assert_eq!(shown["reason"], "Cannot start the core.");
        assert_eq!(shown.get("code"), None);

        assert_eq!(StateView::of(&SessionState::Connecting).unavailable, None);
    }

    #[test] // spec: APP-026
    fn a_frozen_gui_folds_its_waiting_events_but_keeps_every_outcome() {
        let posted: Arc<Mutex<Vec<Vec<Delivery>>>> = Arc::default();
        let sink = posted.clone();
        let gui = GuiOut {
            post: Box::new(move |batch| {
                lock(&sink).push(batch);
                Ok(())
            }),
            state: Mutex::new(GuiState::default()),
            more: Notify::new(),
        };
        // The first delivery goes at once; the GUI doesn't apply it (frozen).
        gui.push(event("test:a", 1, 10));
        assert_eq!(lock(&posted).len(), 1);
        gui.push(Delivery::Finished {
            id: "rq_1".into(),
            json: "{}".into(),
        });
        for n in 2..=(GUI_EVENTS_MAX as u64 + 5) {
            gui.push(event(if n % 2 == 0 { "test:a" } else { "test:b" }, n, 10));
        }
        assert_eq!(lock(&posted).len(), 1, "one batch in Qt's queue at a time");
        {
            let st = lock(&gui.state);
            assert!(st.events <= GUI_EVENTS_MAX, "{}", st.events);
            let finished = st
                .waiting
                .iter()
                .filter(|d| matches!(d, Delivery::Finished { .. }))
                .count();
            assert_eq!(finished, 1, "an outcome is never dropped");
            assert!(st.waiting.iter().any(|d| matches!(d,
                Delivery::Event(ev) if ev.kind == EventKind::ResyncRequired && ev.topic == "test:b")));
        }
        // The GUI applies its batch: the flusher sends the rest.
        gui.drained();
        gui.flush();
        assert_eq!(lock(&posted).len(), 2);
        gui.drained();
        assert!(!lock(&gui.state).in_flight, "nothing waits");
    }

    /// Few but large events (preview frames) fold at the byte bound, well before the count's.
    #[test] // spec: APP-026
    fn a_frozen_gui_folds_its_waiting_events_past_the_byte_bound() {
        let gui = GuiOut {
            post: Box::new(|_| Ok(())),
            state: Mutex::new(GuiState::default()),
            more: Notify::new(),
        };
        gui.push(event("preview:j1", 1, 10));
        let frame = 1024 * 1024;
        let fits = GUI_EVENT_BYTES_MAX / frame - 1;
        for n in 0..fits as u64 {
            gui.push(event("preview:j1", 2 + n, frame));
        }
        assert_eq!(
            lock(&gui.state).waiting.len(),
            fits,
            "under the bound all wait"
        );
        gui.push(event("preview:j1", 9000, frame));
        gui.push(event("preview:j1", 9001, frame));
        let st = lock(&gui.state);
        let waiting: Vec<(u64, EventKind)> = st
            .waiting
            .iter()
            .map(|d| match d {
                Delivery::Event(ev) => (ev.seq, ev.kind),
                other => panic!("only events wait: {other:?}"),
            })
            .collect();
        assert!(
            waiting.len() <= 3 && waiting.contains(&(9000, EventKind::ResyncRequired)),
            "folded into a resync at the newest seq: {waiting:?}"
        );
        assert!(st.event_bytes <= GUI_EVENT_BYTES_MAX);
    }

    #[test] // spec: APP-026
    fn a_gone_object_gets_nothing_more() {
        let gui = GuiOut {
            post: Box::new(|_| Err(())),
            state: Mutex::new(GuiState::default()),
            more: Notify::new(),
        };
        gui.push(event("test:a", 1, 1));
        gui.push(event("test:a", 2, 1));
        let st = lock(&gui.state);
        assert!(st.gone && st.waiting.is_empty());
    }

    #[test] // spec: APP-028
    fn arguments_are_a_json_object() {
        assert!(parse_object("").unwrap().is_empty());
        assert_eq!(parse_object(r#"{"a": 1}"#).unwrap()["a"], 1);
        assert!(parse_object("[1]").is_err());
        assert!(parse_object("{").is_err());
    }
}
