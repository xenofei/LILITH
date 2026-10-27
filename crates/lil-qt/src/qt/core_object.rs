//! `Core`, the QML singleton of module `Lilith.Core` (APP-028): the app's one connection to the
//! core.
//!
//! - **Properties:** `connection` (`connecting`, `ready`, `reconnecting`, `incompatible` or
//!   `unavailable`), `coreProtocol`, `workspace` (`{id, path}`, or null), `project` (empty:
//!   nothing sets the app's project yet; the project switcher's `app/set_context` will),
//!   `channelScheme`, `startTimes` (`{firstFrameMs, interactiveMs}` of this start, APP-136, as
//!   they become known) and `buildId`.
//! - **Requests:** `call(tool, argsJson)`, `appCall(method, paramsJson)` and
//!   `setWorkspace(id)` return a request id (a string, never an object) at once; the outcome
//!   comes later as `callFinished(id, resultJson)`, `callFailed(id, errorJson)` or
//!   `callPendingConfirmation(id, pendingJson)`, never before the method has returned. A
//!   tool call's pending confirmation keeps the request open: once the confirmation is
//!   answered, the call's `callFinished` or `callFailed` follows for the same id (APP-029,
//!   `runtime`). `cancel(id)` drops the request and its callback (`false` for an id that isn't
//!   open); the core isn't told, since a call has no cancel of its own.
//! - **Events:** `subscribe(topic)` and `unsubscribe(topic)`; each event of a followed topic
//!   comes as `event(topic, kind, seq, dataJson)`, throttled (APP-027). `kind` is `event`,
//!   `snapshot` or `resync_required`; reload a topic's model on every `resync_required`.
//!   `lastSeq(topic)` is the session's last seq of a topic (-1 for none).
//! - **Others:** `retryNow()` (the Unavailable banner's "Try again"), `shownText(text,
//!   maxChars)` (`lil_parse::shown_text`, P4a-R8).
//!
//! The session starts only in a process that asked for one ([`super::serve`]): lil-image does,
//! the QML tests don't, and there every request fails `core.unreachable` and the connection is
//! `unavailable`. Everything that reaches the object comes through `CxxQtThread::queue`, on the
//! GUI thread, which this object marks for `lil_client::blocking`'s check (QA-026).

use core::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QVariant};
use lil_protocol::EventKind;
use tokio::task::AbortHandle;

use super::runtime::{Delivery, Runtime};
use crate::requests::Requests;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, build_id, READ, CONSTANT)]
        #[qproperty(QString, connection, READ, NOTIFY)]
        #[qproperty(QString, core_protocol, READ, NOTIFY)]
        #[qproperty(QVariant, workspace, READ, NOTIFY)]
        #[qproperty(QString, project, READ, NOTIFY)]
        #[qproperty(QString, channel_scheme, READ, NOTIFY)]
        #[qproperty(QVariant, start_times, READ, NOTIFY)]
        type Core = super::CoreRust;

        /// `tools/call` of `tool` with `args_json` (a JSON object; empty for none).
        #[qinvokable]
        fn call(self: Pin<&mut Core>, tool: &QString, args_json: &QString) -> QString;

        /// The `app/*` method `method` with `params_json`.
        #[qinvokable]
        fn app_call(self: Pin<&mut Core>, method: &QString, params_json: &QString) -> QString;

        /// Drops request `request_id` and its outcome; `false` when it isn't open.
        #[qinvokable]
        fn cancel(self: Pin<&mut Core>, request_id: &QString) -> bool;

        #[qinvokable]
        fn subscribe(self: Pin<&mut Core>, topic: &QString);

        #[qinvokable]
        fn unsubscribe(self: Pin<&mut Core>, topic: &QString);

        /// `text` as shown: controls removed, bidi and zero-width marks made visible, at most
        /// `max_chars` characters (P4a-R8).
        #[qinvokable]
        fn shown_text(self: &Core, text: &QString, max_chars: i32) -> QString;

        /// Switches to workspace `id` with a new hello (P4a-R34).
        #[qinvokable]
        fn set_workspace(self: Pin<&mut Core>, id: &QString) -> QString;

        /// Tries to reach the core now (the Unavailable banner's "Try again").
        #[qinvokable]
        fn retry_now(self: &Core);

        /// The session's last seq of `topic`; -1 when it has none.
        #[qinvokable]
        fn last_seq(self: &Core, topic: &QString) -> f64;

        #[qsignal]
        fn call_finished(self: Pin<&mut Core>, id: QString, result_json: QString);

        #[qsignal]
        fn call_failed(self: Pin<&mut Core>, id: QString, error_json: QString);

        #[qsignal]
        fn call_pending_confirmation(self: Pin<&mut Core>, id: QString, pending_json: QString);

        #[qsignal]
        fn event(self: Pin<&mut Core>, topic: QString, kind: QString, seq: f64, data_json: QString);
    }

    impl cxx_qt::Threading for Core {}
    impl cxx_qt::Initialize for Core {}

    unsafe extern "C++" {
        include!("lil_qt/src/qt/core_helpers.h");

        /// JSON text as a QVariant: an object is a map, `null` (or text that isn't JSON) an
        /// invalid variant, which QML reads as null.
        #[namespace = "lil_qt"]
        #[cxx_name = "jsonVariant"]
        fn json_variant(json: &str) -> QVariant;

        /// A new `Core` with no parent (the e2e tests', outside a QML engine).
        #[namespace = "lil_qt"]
        #[cxx_name = "newCore"]
        fn new_core() -> UniquePtr<Core>;
    }
}

pub use qobject::{Core, new_core};

/// `Core`'s Rust side.
pub struct CoreRust {
    build_id: QString,
    connection: QString,
    core_protocol: QString,
    workspace: QVariant,
    project: QString,
    channel_scheme: QString,
    start_times: QVariant,
    /// The open requests, with the task to abort on a cancel.
    requests: Requests<Option<AbortHandle>>,
    runtime: Option<Arc<Runtime>>,
}

impl Default for CoreRust {
    fn default() -> Self {
        Self {
            build_id: QString::from(lil_protocol::session::BUILD_ID),
            connection: QString::from("connecting"),
            core_protocol: QString::default(),
            workspace: QVariant::default(),
            project: QString::default(),
            channel_scheme: QString::default(),
            start_times: qobject::json_variant("{}"),
            requests: Requests::new(),
            runtime: None,
        }
    }
}

/// The newest `Core`, told of the start times as `main.cpp` notes them.
static NEWEST: Mutex<Option<cxx_qt::CxxQtThread<Core>>> = Mutex::new(None);

impl cxx_qt::Initialize for Core {
    fn initialize(mut self: Pin<&mut Self>) {
        lil_client::mark_gui_thread();
        let thread = self.qt_thread();
        *NEWEST.lock().unwrap_or_else(PoisonError::into_inner) = Some(thread.clone());
        let post = Box::new(move |batch: Vec<Delivery>| {
            thread.queue(move |core| apply(core, batch)).map_err(|_| ())
        });
        let setup = super::setup();
        if let Some(scheme) = setup.as_ref().map(|cfg| cfg.env.activation_scheme()) {
            self.as_mut().rust_mut().channel_scheme = QString::from(scheme.as_str());
            self.as_mut().channel_scheme_changed();
        }
        match Runtime::start(setup, post) {
            Ok(runtime) => self.as_mut().rust_mut().runtime = Some(Arc::new(runtime)),
            Err(e) => {
                tracing::error!(error = %e, "the core session's runtime didn't start");
                self.as_mut().rust_mut().connection = QString::from("unavailable");
                self.as_mut().connection_changed();
            }
        }
        refresh_start_times(self);
    }
}

impl Drop for CoreRust {
    fn drop(&mut self) {
        let open = self.requests.drain().len();
        if open > 0 {
            // APP-029: a call still waiting on its confirmation is cancelled with the app.
            tracing::info!(
                open,
                "the app closed with requests open; they were cancelled"
            );
        }
    }
}

/// Applies a batch from the runtime, in order, on the GUI thread.
fn apply(mut core: Pin<&mut Core>, batch: Vec<Delivery>) {
    for delivery in batch {
        match delivery {
            Delivery::State(view) => {
                let connection = QString::from(view.connection);
                if core.connection != connection {
                    core.as_mut().rust_mut().connection = connection;
                    core.as_mut().connection_changed();
                }
                if let Some(protocol) = view.core_protocol {
                    let protocol = QString::from(protocol.as_str());
                    if core.core_protocol != protocol {
                        core.as_mut().rust_mut().core_protocol = protocol;
                        core.as_mut().core_protocol_changed();
                    }
                }
                if let Some(workspace) = view.workspace {
                    let json = workspace.unwrap_or_else(|| "null".to_string());
                    core.as_mut().rust_mut().workspace = qobject::json_variant(&json);
                    core.as_mut().workspace_changed();
                }
            }
            Delivery::Finished { id, json } => {
                if core.as_mut().rust_mut().requests.finish(&id).is_some() {
                    core.as_mut()
                        .call_finished(QString::from(id.as_str()), QString::from(json.as_str()));
                }
            }
            Delivery::Failed { id, json } => {
                if core.as_mut().rust_mut().requests.finish(&id).is_some() {
                    core.as_mut()
                        .call_failed(QString::from(id.as_str()), QString::from(json.as_str()));
                }
            }
            Delivery::Pending { id, json, open } => {
                let still = if open {
                    core.requests.is_open(&id)
                } else {
                    core.as_mut().rust_mut().requests.finish(&id).is_some()
                };
                if still {
                    core.as_mut().call_pending_confirmation(
                        QString::from(id.as_str()),
                        QString::from(json.as_str()),
                    );
                }
            }
            Delivery::Event(ev) => {
                let kind = match ev.kind {
                    EventKind::Event => "event",
                    EventKind::Snapshot => "snapshot",
                    EventKind::ResyncRequired => "resync_required",
                };
                #[allow(clippy::cast_precision_loss, reason = "seqs stay far below 2^53")]
                let seq = ev.seq as f64;
                core.as_mut().event(
                    QString::from(ev.topic.as_str()),
                    QString::from(kind),
                    seq,
                    QString::from(ev.data.to_string().as_str()),
                );
            }
        }
    }
    if let Some(runtime) = core.runtime.clone() {
        runtime.drained();
    }
}

/// `startTimes` from what `main.cpp` noted ([`super::note_start_time`]).
fn refresh_start_times(mut core: Pin<&mut Core>) {
    let times = super::start_times_json();
    core.as_mut().rust_mut().start_times = qobject::json_variant(&times);
    core.as_mut().start_times_changed();
}

/// Tells the newest `Core` that a start time was noted.
pub(super) fn start_time_noted() {
    let newest = NEWEST
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(thread) = newest {
        // A Core that is gone has nothing to show.
        let _ = thread.queue(refresh_start_times);
    }
}

impl qobject::Core {
    pub fn call(mut self: Pin<&mut Self>, tool: &QString, args_json: &QString) -> QString {
        let id = self.as_mut().rust_mut().requests.open(None);
        if let Some(runtime) = self.runtime.clone()
            && let Some(task) = runtime.call(id.clone(), tool.to_string(), &args_json.to_string())
        {
            self.as_mut().rust_mut().requests.set(&id, Some(task));
        }
        QString::from(id.as_str())
    }

    pub fn app_call(mut self: Pin<&mut Self>, method: &QString, params_json: &QString) -> QString {
        let id = self.as_mut().rust_mut().requests.open(None);
        if let Some(runtime) = self.runtime.clone()
            && let Some(task) =
                runtime.app_call(id.clone(), method.to_string(), &params_json.to_string())
        {
            self.as_mut().rust_mut().requests.set(&id, Some(task));
        }
        QString::from(id.as_str())
    }

    pub fn set_workspace(mut self: Pin<&mut Self>, ws: &QString) -> QString {
        let id = self.as_mut().rust_mut().requests.open(None);
        if let Some(runtime) = self.runtime.clone()
            && let Some(task) = runtime.set_workspace(id.clone(), ws.to_string())
        {
            self.as_mut().rust_mut().requests.set(&id, Some(task));
        }
        QString::from(id.as_str())
    }

    pub fn cancel(mut self: Pin<&mut Self>, request_id: &QString) -> bool {
        match self
            .as_mut()
            .rust_mut()
            .requests
            .cancel(&request_id.to_string())
        {
            Some(task) => {
                if let Some(task) = task {
                    task.abort();
                }
                true
            }
            None => false,
        }
    }

    pub fn subscribe(self: Pin<&mut Self>, topic: &QString) {
        if let Some(runtime) = &self.runtime {
            runtime.subscribe(topic.to_string());
        }
    }

    pub fn unsubscribe(self: Pin<&mut Self>, topic: &QString) {
        if let Some(runtime) = &self.runtime {
            runtime.unsubscribe(topic.to_string());
        }
    }

    pub fn shown_text(&self, text: &QString, max_chars: i32) -> QString {
        let max = usize::try_from(max_chars).unwrap_or(0);
        QString::from(lil_parse::shown_text(&text.to_string(), max).as_str())
    }

    pub fn retry_now(&self) {
        if let Some(runtime) = &self.runtime {
            runtime.retry_now();
        }
    }

    pub fn last_seq(&self, topic: &QString) -> f64 {
        #[allow(clippy::cast_precision_loss, reason = "seqs stay far below 2^53")]
        self.runtime
            .as_ref()
            .and_then(|r| r.last_seq(&topic.to_string()))
            .map_or(-1.0, |seq| seq as f64)
    }
}
