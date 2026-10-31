//! The list models of module `Lilith.Core` (APP-025): Rust `QAbstractListModel`s.
//!
//! **`JsonListModel`** pages a list tool's rows and keeps them live from a topic:
//! - **Properties:** `core` (the `Core` singleton, or any object with its API), `tool`,
//!   `argsJson` (the tool's arguments, a JSON object), `listKey` (where the rows are in the
//!   tool's `structured` output), `idKey` (each row's id), `pageSize` (default 100), `topic`,
//!   `eventKind` and `rowKey` (below), `roleNames` (the row fields a delegate reads, as roles),
//!   and, read-only, `loaded` (the first page of the current load is in) and `error` (the last
//!   failed page's error body as a map; none, which QML reads as `undefined`, once a page
//!   is in).
//! - **`idKey` must be unique within the list**: two rows with one id are one row. For assets
//!   that is `ref`, not `asset_id`, which is unique only within a project.
//! - **Paging** ([`crate::paging::PageState`]): a load asks for the first page with `limit` set
//!   to `pageSize`; `canFetchMore`/`fetchMore` ask for the next with the last page's
//!   `next_cursor` as `cursor`, one at a time, until it is `null`; `pageLoaded` says each of those
//!   pages is in, whether or not it added a row. A row a later page holds
//!   already (inserted live meanwhile) is updated in place, not added twice. `argsJson` filters
//!   the pages only: events are upserted whatever the filter, so a filtered panel filters in QML
//!   or reloads.
//! - **Events.** The model asks `core` to follow `topic` once, before its first page is asked
//!   for. The subscribe and the call still reach the core in no fixed order (Task 16's design),
//!   so an event published just before the subscribe took effect can be missed; the next event
//!   of that row, or a reload, puts it right. Each `event` of the topic is upserted by `idKey`
//!   ([`crate::paging::Rows`]): a new id goes first (the lists are newest first), a known one is
//!   updated in place (one `dataChanged`) unless the row is older than the one held. "Older"
//!   compares the rows' `updated` times: no list tool says the seq its answer stands for, so this
//!   is what "drop an event at or below the seq the model loaded at" means here (equal times
//!   apply: each event is the full state). With `eventKind` set, only events whose `kind` is that
//!   are rows, the others are skipped; with `rowKey` set, the row is the event's value under it,
//!   else the event's data itself. (The `jobs` topic: `eventKind: "job"`, `rowKey: "job"`; its
//!   `queue` events are skipped.)
//! - **Reloads.** Changing `core`, `tool`, `argsJson`, `listKey`, `idKey` or `topic` reloads it
//!   on the next turn of the event loop (once for several changes), and so do `reload()`, a
//!   `snapshot` and a `resync_required` of its topic (a seq of 0: a core restart or a workspace
//!   switch). A reload gives up the page asked for, and `loaded` is false until its first page is
//!   in: a panel holds back its writes meanwhile (calls can reach a new connection before the app
//!   sees the mark). The topic's events are held meanwhile, at most [`EVENT_BUFFER_MAX`], and
//!   applied through the same `updated` guard right after the first page replaces the rows: an
//!   event newer than the page's row wins, an older one is dropped. The rows held stay until that
//!   page replaces them (one model reset). Past the bound (review N-1), the held events and the
//!   rest of the load's are dropped, and the page asked for and any retry waiting go on as they
//!   are: when the page lands it replaces the rows and the model loads once more, keeping the
//!   backoff (that load failing waits as any failure does).
//! - **Failures** (review C-1). A failed page sets `error` and is asked again after 1 s, then
//!   2 s, doubling up to 30 s; a reload, a change that reloads, or a page that comes in starts
//!   that over. While the current load isn't in, `core`'s `connection` becoming `ready` reloads
//!   at once (the session fails calls at once until it is connected, and its first connection
//!   puts no resync mark into the stream). That is not ARCH-100's forbidden reload on `Ready`:
//!   there is no loaded state for it to replace. A loaded model never reloads on `ready`.
//! - `get(row)`: the row as a map. When the model goes, its page asked for is cancelled and its
//!   topic left.
//!
//! **`EventLogModel`** (Diagnostics' event log): the newest [`EVENT_LOG_ROWS`] events of every
//! topic the app follows, oldest first, from when it was made: the shell makes one at start. What
//! repeats state at a high rate is left out ([`is_heartbeat`]), or one running job would push
//! everything else out in about 100 s. It reads `core`'s `coreEvent` and subscribes to nothing
//! itself. Roles `topic`, `kind`, `seq`,
//! `text` (the event's data as JSON, cut to [`EVENT_LOG_TEXT_CHARS`] characters, with no image
//! bytes: a preview shows its format and size, never `data_b64`) and `receivedMs` (Unix ms);
//! `get(row)` as for the list model.
//!
//! Both reach `core` through `src/qt/core_link.h`, which works with Core and FakeCore alike.

use core::pin::Pin;
use std::collections::{HashSet, VecDeque};

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QByteArray, QModelIndex, QString, QVariant};
use serde_json::Value;

use super::core_object::qobject::json_variant;
use crate::paging::{Change, PageState, Rows, row_id};

/// The newest events the event log keeps.
pub const EVENT_LOG_ROWS: usize = 1000;
/// The longest `text` an event log row keeps.
pub const EVENT_LOG_TEXT_CHARS: usize = 2000;
/// `pageSize`'s default.
pub const PAGE_SIZE: i32 = 100;
/// The topic's events a model holds while its first page is asked for.
pub const EVENT_BUFFER_MAX: usize = 1024;
/// The first wait before a failed page is asked again, and the longest.
pub const RETRY_FIRST_MS: u32 = 1000;
pub const RETRY_MAX_MS: u32 = 30_000;
/// `Qt::UserRole`: the first role.
const USER_ROLE: i32 = 0x0100;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;

        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qproperty(QVariant, core)]
        #[qproperty(QString, tool)]
        #[qproperty(QString, args_json)]
        #[qproperty(QString, list_key)]
        #[qproperty(QString, id_key)]
        #[qproperty(i32, page_size)]
        #[qproperty(QString, topic)]
        #[qproperty(QString, event_kind)]
        #[qproperty(QString, row_key)]
        #[qproperty(QStringList, role_names)]
        #[qproperty(bool, loaded, READ, NOTIFY)]
        #[qproperty(QVariant, error, READ, NOTIFY)]
        type JsonListModel = super::JsonListModelRust;

        /// Loads again from the first page (module docs).
        #[qinvokable]
        fn reload(self: Pin<&mut JsonListModel>);

        /// Row `row` as a map; null past the end.
        #[qinvokable]
        fn get(self: &JsonListModel, row: i32) -> QVariant;

        /// `core`'s signals (`src/qt/core_link.h`), not for QML.
        #[qinvokable]
        fn core_call_finished(self: Pin<&mut JsonListModel>, id: &QString, result_json: &QString);
        #[qinvokable]
        fn core_call_failed(self: Pin<&mut JsonListModel>, id: &QString, error_json: &QString);
        #[qinvokable]
        fn core_call_pending(self: Pin<&mut JsonListModel>, id: &QString, pending_json: &QString);
        #[qinvokable]
        fn core_connection_changed(self: Pin<&mut JsonListModel>);
        /// The retry timer (`src/qt/core_link.h`), not for QML.
        #[qinvokable]
        fn core_retry_due(self: Pin<&mut JsonListModel>, generation: u64);
        #[qinvokable]
        fn core_event_received(
            self: Pin<&mut JsonListModel>,
            topic: &QString,
            kind: &QString,
            seq: f64,
            data_json: &QString,
        );

        #[cxx_override]
        fn data(self: &JsonListModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        fn row_count(self: &JsonListModel, parent: &QModelIndex) -> i32;
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_hash(self: &JsonListModel) -> QHash_i32_QByteArray;
        #[cxx_override]
        fn can_fetch_more(self: &JsonListModel, parent: &QModelIndex) -> bool;
        #[cxx_override]
        fn fetch_more(self: Pin<&mut JsonListModel>, parent: &QModelIndex);

        /// A page after the first is in, its rows applied, whether or not it added any.
        #[qsignal]
        fn page_loaded(self: Pin<&mut JsonListModel>);

        #[inherit]
        unsafe fn begin_reset_model(self: Pin<&mut JsonListModel>);
        #[inherit]
        unsafe fn end_reset_model(self: Pin<&mut JsonListModel>);
        #[inherit]
        unsafe fn begin_insert_rows(
            self: Pin<&mut JsonListModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        unsafe fn end_insert_rows(self: Pin<&mut JsonListModel>);
        #[inherit]
        fn index(self: &JsonListModel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
        #[inherit]
        #[qsignal]
        fn data_changed(
            self: Pin<&mut JsonListModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qproperty(QVariant, core)]
        type EventLogModel = super::EventLogModelRust;

        /// Row `row` as a map; null past the end.
        #[qinvokable]
        fn get(self: &EventLogModel, row: i32) -> QVariant;

        /// `core`'s `coreEvent` (`src/qt/core_link.h`), not for QML.
        #[qinvokable]
        fn core_event_received(
            self: Pin<&mut EventLogModel>,
            topic: &QString,
            kind: &QString,
            seq: f64,
            data_json: &QString,
        );

        #[cxx_override]
        fn data(self: &EventLogModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        fn row_count(self: &EventLogModel, parent: &QModelIndex) -> i32;
        #[cxx_override]
        fn role_names(self: &EventLogModel) -> QHash_i32_QByteArray;

        #[inherit]
        unsafe fn begin_insert_rows(
            self: Pin<&mut EventLogModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        unsafe fn end_insert_rows(self: Pin<&mut EventLogModel>);
        #[inherit]
        unsafe fn begin_remove_rows(
            self: Pin<&mut EventLogModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        unsafe fn end_remove_rows(self: Pin<&mut EventLogModel>);
    }

    impl cxx_qt::Threading for JsonListModel {}
    impl cxx_qt::Initialize for JsonListModel {}
    impl cxx_qt::Initialize for EventLogModel {}

    #[namespace = "lil_qt"]
    unsafe extern "C++" {
        include!("lil_qt/src/qt/core_link.h");
        type CoreLink;

        #[cxx_name = "newCoreLink"]
        fn new_core_link() -> UniquePtr<CoreLink>;
        #[cxx_name = "bindListModel"]
        fn bind_list_model(link: &CoreLink, model: &JsonListModel, core: &QVariant) -> bool;
        #[cxx_name = "bindLogModel"]
        fn bind_log_model(link: &CoreLink, model: &EventLogModel, core: &QVariant) -> bool;
        #[cxx_name = "coreBound"]
        fn core_bound(link: &CoreLink) -> bool;
        #[cxx_name = "coreCall"]
        fn core_call(link: &CoreLink, tool: &QString, args_json: &QString) -> QString;
        #[cxx_name = "coreCancel"]
        fn core_cancel(link: &CoreLink, id: &QString);
        #[cxx_name = "coreSubscribe"]
        fn core_subscribe(link: &CoreLink, topic: &QString);
        #[cxx_name = "coreUnsubscribe"]
        fn core_unsubscribe(link: &CoreLink, topic: &QString);
        /// `core`'s `connection` (empty without one).
        #[cxx_name = "coreConnection"]
        fn core_connection(link: &CoreLink) -> QString;
        /// Calls `model.coreRetryDue(generation)` in `ms` ms, unless the model is gone by then.
        #[cxx_name = "retryLater"]
        fn retry_later(model: &JsonListModel, ms: u32, generation: u64);
    }
}

pub use qobject::{EventLogModel, JsonListModel};

/// A JSON value as a delegate reads it: text, a number, a bool, a map, a list or null.
fn to_variant(value: &Value) -> QVariant {
    match value {
        Value::Null => QVariant::default(),
        Value::Bool(b) => QVariant::from(b),
        Value::Number(n) => QVariant::from(&n.as_f64().unwrap_or_default()),
        Value::String(s) => QVariant::from(&QString::from(s.as_str())),
        Value::Array(_) | Value::Object(_) => json_variant(&value.to_string()),
    }
}

fn row_index(index: &QModelIndex) -> Option<usize> {
    (!index.parent().is_valid())
        .then(|| usize::try_from(index.row()).ok())
        .flatten()
}

fn to_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// `JsonListModel`'s Rust side (module docs).
pub struct JsonListModelRust {
    core: QVariant,
    tool: QString,
    args_json: QString,
    list_key: QString,
    id_key: QString,
    page_size: i32,
    topic: QString,
    event_kind: QString,
    row_key: QString,
    role_names: cxx_qt_lib::QStringList,
    loaded: bool,
    error: QVariant,
    rows: Rows,
    page: PageState,
    /// The page asked for: its request id, and whether it is a load's first.
    asked: Option<(QString, bool)>,
    /// The topic followed through `core`.
    followed: Option<QString>,
    reload_queued: bool,
    /// The topic's rows held while the first page is asked for (module docs), and whether more
    /// came than it holds.
    held: Vec<Value>,
    overflowed: bool,
    /// The wait before the next retry (0: none failed since the last start); the retry timer's
    /// generation (a timer of another is stale); whether the retry is of a first page.
    retry_ms: u32,
    retry_generation: u64,
    retry_first: Option<bool>,
    link: cxx::UniquePtr<qobject::CoreLink>,
}

impl Default for JsonListModelRust {
    fn default() -> Self {
        Self {
            core: QVariant::default(),
            tool: QString::default(),
            args_json: QString::default(),
            list_key: QString::default(),
            id_key: QString::default(),
            page_size: PAGE_SIZE,
            topic: QString::default(),
            event_kind: QString::default(),
            row_key: QString::default(),
            role_names: cxx_qt_lib::QStringList::default(),
            loaded: false,
            error: QVariant::default(),
            rows: Rows::new(),
            page: PageState::new(),
            asked: None,
            followed: None,
            reload_queued: false,
            held: Vec::new(),
            overflowed: false,
            retry_ms: 0,
            retry_generation: 0,
            retry_first: None,
            link: qobject::new_core_link(),
        }
    }
}

impl Drop for JsonListModelRust {
    fn drop(&mut self) {
        if let Some((id, _)) = self.asked.take() {
            qobject::core_cancel(&self.link, &id);
        }
        if let Some(topic) = self.followed.take() {
            qobject::core_unsubscribe(&self.link, &topic);
        }
    }
}

impl cxx_qt::Initialize for JsonListModel {
    fn initialize(mut self: Pin<&mut Self>) {
        self.as_mut()
            .on_core_changed(JsonListModel::rebind)
            .release();
        self.as_mut()
            .on_tool_changed(JsonListModel::queue_reload)
            .release();
        self.as_mut()
            .on_args_json_changed(JsonListModel::queue_reload)
            .release();
        self.as_mut()
            .on_list_key_changed(JsonListModel::queue_reload)
            .release();
        self.as_mut()
            .on_id_key_changed(JsonListModel::queue_reload)
            .release();
        self.as_mut()
            .on_topic_changed(JsonListModel::queue_reload)
            .release();
    }
}

impl qobject::JsonListModel {
    /// A new `core`: the old one's page and topic are given up, and the model loads from it.
    fn rebind(mut self: Pin<&mut Self>) {
        self.as_mut().leave();
        qobject::bind_list_model(&self.link, &self, &self.core);
        self.queue_reload();
    }

    /// Gives up the page asked for and the topic followed.
    fn leave(mut self: Pin<&mut Self>) {
        if let Some((id, _)) = self.as_mut().rust_mut().asked.take() {
            qobject::core_cancel(&self.link, &id);
        }
        if let Some(topic) = self.as_mut().rust_mut().followed.take() {
            qobject::core_unsubscribe(&self.link, &topic);
        }
    }

    fn queue_reload(mut self: Pin<&mut Self>) {
        if self.reload_queued {
            return;
        }
        self.as_mut().rust_mut().reload_queued = true;
        // A model that is gone loads nothing.
        let _ = self.qt_thread().queue(|mut model| {
            model.as_mut().rust_mut().reload_queued = false;
            model.reload();
        });
    }

    fn set_loaded_to(mut self: Pin<&mut Self>, loaded: bool) {
        if self.loaded != loaded {
            self.as_mut().rust_mut().loaded = loaded;
            self.loaded_changed();
        }
    }

    pub fn reload(self: Pin<&mut Self>) {
        self.start_load(true);
    }

    /// Loads from the first page: a reload (`reset_backoff`), or a first page's retry.
    fn start_load(mut self: Pin<&mut Self>, reset_backoff: bool) {
        if let Some((id, _)) = self.as_mut().rust_mut().asked.take() {
            qobject::core_cancel(&self.link, &id);
        }
        if reset_backoff {
            self.as_mut().reset_backoff();
        }
        let topic = self.topic.clone();
        if self.followed.as_ref() != Some(&topic) {
            if let Some(old) = self.as_mut().rust_mut().followed.take() {
                qobject::core_unsubscribe(&self.link, &old);
            }
            if !topic.is_empty() && qobject::core_bound(&self.link) {
                qobject::core_subscribe(&self.link, &topic);
                self.as_mut().rust_mut().followed = Some(topic);
            }
        }
        self.as_mut().rust_mut().held.clear();
        self.as_mut().rust_mut().overflowed = false;
        self.as_mut().rust_mut().page.reset();
        self.as_mut().set_loaded_to(false);
        self.ask(true);
    }

    /// No retry waits, and the next failure waits [`RETRY_FIRST_MS`].
    fn reset_backoff(mut self: Pin<&mut Self>) {
        let mut rust = self.as_mut().rust_mut();
        rust.retry_ms = 0;
        rust.retry_generation += 1;
        rust.retry_first = None;
    }

    /// The page asked for failed with `error_json`: `error` is set, and it is asked again later.
    fn page_failed(mut self: Pin<&mut Self>, first: bool, error_json: &QString) {
        self.as_mut().rust_mut().page.on_failed();
        self.as_mut().rust_mut().error = json_variant(&error_json.to_string());
        self.as_mut().error_changed();
        let ms = match self.retry_ms {
            0 => RETRY_FIRST_MS,
            ms => ms.saturating_mul(2).min(RETRY_MAX_MS),
        };
        let generation = {
            let mut rust = self.as_mut().rust_mut();
            rust.retry_ms = ms;
            rust.retry_generation += 1;
            rust.retry_first = Some(first);
            rust.retry_generation
        };
        qobject::retry_later(&self, ms, generation);
    }

    pub fn core_retry_due(mut self: Pin<&mut Self>, generation: u64) {
        if generation != self.retry_generation {
            return;
        }
        match self.as_mut().rust_mut().retry_first.take() {
            Some(true) => self.start_load(false),
            Some(false) => self.ask(false),
            None => {}
        }
    }

    pub fn core_connection_changed(self: Pin<&mut Self>) {
        if !self.loaded && qobject::core_connection(&self.link) == QString::from("ready") {
            self.reload();
        }
    }

    /// Asks for the next page (the first of a load when `first`).
    fn ask(mut self: Pin<&mut Self>, first: bool) {
        if self.tool.is_empty() || !qobject::core_bound(&self.link) {
            return;
        }
        let base: Value = serde_json::from_str(&self.args_json.to_string()).unwrap_or_default();
        let page = u32::try_from(self.page_size)
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(PAGE_SIZE.unsigned_abs());
        let Some(args) = self.as_mut().rust_mut().page.next_args(&base, page) else {
            return;
        };
        let id = qobject::core_call(&self.link, &self.tool, &QString::from(args.to_string()));
        if id.is_empty() {
            self.as_mut().rust_mut().page.on_failed();
            return;
        }
        self.as_mut().rust_mut().asked = Some((id, first));
    }

    /// Whether `id` is the page asked for: it is then no longer asked, and whether it was a
    /// first page is returned.
    fn answered(mut self: Pin<&mut Self>, id: &QString) -> Option<bool> {
        match &self.asked {
            Some((asked, first)) if asked == id => {
                let first = *first;
                self.as_mut().rust_mut().asked = None;
                Some(first)
            }
            _ => None,
        }
    }

    pub fn core_call_finished(mut self: Pin<&mut Self>, id: &QString, result_json: &QString) {
        let Some(first) = self.as_mut().answered(id) else {
            return;
        };
        let mut result: Value = serde_json::from_str(&result_json.to_string()).unwrap_or_default();
        let structured = result["structured"].take();
        let next = structured
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        let page = match structured.get(self.list_key.to_string().as_str()) {
            Some(Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        };
        self.as_mut().rust_mut().page.on_page(next);
        if !self.error.is_null() {
            self.as_mut().rust_mut().error = QVariant::default();
            self.as_mut().error_changed();
        }
        let id_key = self.id_key.to_string();
        if first && self.overflowed {
            // More events came during the load than it holds: the page stands for now, and the
            // model loads once more, with the backoff kept (module docs, review N-1).
            // SAFETY: the reset is begun and ended around the change, on the GUI thread.
            unsafe { self.as_mut().begin_reset_model() };
            self.as_mut().rust_mut().rows.replace(page, &id_key);
            unsafe { self.as_mut().end_reset_model() };
            self.start_load(false);
            return;
        }
        self.as_mut().reset_backoff();
        if first {
            // SAFETY: the reset is begun and ended around the change, on the GUI thread.
            unsafe { self.as_mut().begin_reset_model() };
            let mut rust = self.as_mut().rust_mut();
            let held = std::mem::take(&mut rust.held);
            rust.rows.replace(page, &id_key);
            // The events held during the load, through the `updated` guard (module docs).
            for row in held {
                let _ = rust.rows.upsert(row, &id_key);
            }
            unsafe { self.as_mut().end_reset_model() };
            self.set_loaded_to(true);
            return;
        }
        // A row held already (inserted live meanwhile) is updated in place; the rest are new.
        let mut fresh = Vec::new();
        let mut held = Vec::new();
        let mut seen = HashSet::new();
        for row in page {
            match row_id(&row, &id_key) {
                Some(rid) if self.rows.contains(&rid) => held.push(row),
                Some(rid) if !seen.insert(rid.clone()) => {}
                _ => fresh.push(row),
            }
        }
        for row in held {
            self.as_mut().upsert(row);
        }
        if !fresh.is_empty() {
            let start = to_i32(self.rows.len());
            let last = start + to_i32(fresh.len()) - 1;
            // SAFETY: as above.
            unsafe {
                self.as_mut()
                    .begin_insert_rows(&QModelIndex::default(), start, last);
            }
            self.as_mut().rust_mut().rows.append_page(fresh, &id_key);
            unsafe { self.as_mut().end_insert_rows() };
        }
        self.page_loaded();
    }

    pub fn core_call_failed(mut self: Pin<&mut Self>, id: &QString, error_json: &QString) {
        if let Some(first) = self.as_mut().answered(id) {
            tracing::warn!(tool = %self.tool, error = %error_json, "a list page failed");
            self.page_failed(first, error_json);
        }
    }

    /// A list tool never waits on a confirmation: the page is given up as failed.
    pub fn core_call_pending(mut self: Pin<&mut Self>, id: &QString, pending_json: &QString) {
        if let Some(first) = self.as_mut().answered(id) {
            qobject::core_cancel(&self.link, id);
            tracing::warn!(tool = %self.tool, pending = %pending_json, "a list page asked for a confirmation");
            self.page_failed(first, pending_json);
        }
    }

    pub fn core_event_received(
        mut self: Pin<&mut Self>,
        topic: &QString,
        kind: &QString,
        _seq: f64,
        data_json: &QString,
    ) {
        if self.topic.is_empty() || *topic != self.topic {
            return;
        }
        match kind.to_string().as_str() {
            "snapshot" | "resync_required" => self.reload(),
            "event" => {
                let Some(row) = self.event_row(data_json) else {
                    return;
                };
                if self.loaded {
                    self.upsert(row);
                } else if self.overflowed {
                } else if self.held.len() < EVENT_BUFFER_MAX {
                    self.as_mut().rust_mut().held.push(row);
                } else {
                    tracing::warn!(topic = %self.topic, "too many events during a load: it loads once more when it lands");
                    let mut rust = self.as_mut().rust_mut();
                    rust.held.clear();
                    rust.overflowed = true;
                }
            }
            _ => {}
        }
    }

    /// The row an event's data carries (module docs, "Events"), if it carries one.
    fn event_row(&self, data_json: &QString) -> Option<Value> {
        let mut data = serde_json::from_str::<Value>(&data_json.to_string()).ok()?;
        let event_kind = self.event_kind.to_string();
        if !event_kind.is_empty()
            && data.get("kind").and_then(Value::as_str) != Some(event_kind.as_str())
        {
            return None;
        }
        let row_key = self.row_key.to_string();
        let row = if row_key.is_empty() {
            data
        } else {
            data[row_key.as_str()].take()
        };
        row.is_object().then_some(row)
    }

    /// Upserts one row with the model's signals.
    fn upsert(mut self: Pin<&mut Self>, row: Value) {
        let id_key = self.id_key.to_string();
        let Some(id) = row_id(&row, &id_key) else {
            return;
        };
        if self.rows.contains(&id) {
            if let Some(Change::Updated(at)) = self.as_mut().rust_mut().rows.upsert(row, &id_key) {
                let index = self.index(to_i32(at), 0, &QModelIndex::default());
                self.as_mut()
                    .data_changed(&index, &index, &cxx_qt_lib::QList::default());
            }
            return;
        }
        // SAFETY: the insert is begun and ended around the change, on the GUI thread.
        unsafe {
            self.as_mut()
                .begin_insert_rows(&QModelIndex::default(), 0, 0)
        };
        let _ = self.as_mut().rust_mut().rows.upsert(row, &id_key);
        unsafe { self.as_mut().end_insert_rows() };
    }

    pub fn get(&self, row: i32) -> QVariant {
        usize::try_from(row)
            .ok()
            .and_then(|r| self.rows.get(r))
            .map_or_else(QVariant::default, to_variant)
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let (Some(row), Some(role)) = (
            row_index(index).and_then(|r| self.rows.get(r)),
            isize::try_from(role - USER_ROLE)
                .ok()
                .and_then(|r| self.role_names.get(r)),
        ) else {
            return QVariant::default();
        };
        row.get(role.to_string().as_str())
            .map_or_else(QVariant::default, to_variant)
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0
        } else {
            to_i32(self.rows.len())
        }
    }

    pub fn role_hash(&self) -> cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray> {
        let mut hash = cxx_qt_lib::QHash::default();
        for (i, name) in self.role_names.iter().enumerate() {
            hash.insert(
                USER_ROLE + to_i32(i),
                QByteArray::from(name.to_string().as_str()),
            );
        }
        hash
    }

    pub fn can_fetch_more(&self, parent: &QModelIndex) -> bool {
        !parent.is_valid() && self.loaded && self.page.can_fetch_more()
    }

    pub fn fetch_more(self: Pin<&mut Self>, parent: &QModelIndex) {
        if self.can_fetch_more(parent) {
            self.ask(false);
        }
    }
}

/// One event of the log.
struct Entry {
    topic: String,
    kind: String,
    seq: f64,
    text: String,
    received_ms: f64,
}

impl Entry {
    fn field(&self, role: i32) -> QVariant {
        match role - USER_ROLE {
            0 => QVariant::from(&QString::from(self.topic.as_str())),
            1 => QVariant::from(&QString::from(self.kind.as_str())),
            2 => QVariant::from(&self.seq),
            3 => QVariant::from(&QString::from(self.text.as_str())),
            4 => QVariant::from(&self.received_ms),
            _ => QVariant::default(),
        }
    }
}

/// The log's roles, in [`Entry::field`]'s order.
const EVENT_LOG_ROLES: [&str; 5] = ["topic", "kind", "seq", "text", "receivedMs"];

/// Whether the log leaves an event out: a `telemetry` `sample`, a `job:<id>` `progress` or a frame
/// of `preview:<job_id>` (APP-134). Each repeats state it already carries, and the Telemetry card
/// and the Jobs panel show them. A session mark (`snapshot`, `resync_required`) stays.
fn is_heartbeat(topic: &str, kind: &str, data_json: &str) -> bool {
    if kind != "event" {
        return false;
    }
    if topic.starts_with("preview:") {
        return true;
    }
    let beat = if topic == "telemetry" {
        "sample"
    } else if topic.starts_with("job:") {
        "progress"
    } else {
        return false;
    };
    serde_json::from_str::<Value>(data_json)
        .is_ok_and(|data| data.get("kind").and_then(Value::as_str) == Some(beat))
}

/// An event's data as the log shows it (module docs).
fn log_text(data_json: &str) -> String {
    let mut text = data_json.to_string();
    if text.contains("\"data_b64\"")
        && let Ok(mut data) = serde_json::from_str::<Value>(&text)
    {
        strip_image_bytes(&mut data);
        text = data.to_string();
    }
    if text.chars().count() > EVENT_LOG_TEXT_CHARS {
        text = text.chars().take(EVENT_LOG_TEXT_CHARS - 1).collect();
        text.push('…');
    }
    text
}

/// Replaces every `data_b64` text, at any depth, with `bytes`: its size (review M-8).
fn strip_image_bytes(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(b64)) = map.remove("data_b64") {
                let bytes = b64.trim_end_matches('=').len() * 3 / 4;
                map.insert("bytes".to_string(), Value::from(bytes));
            }
            map.values_mut().for_each(strip_image_bytes);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_image_bytes),
        _ => {}
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// `EventLogModel`'s Rust side (module docs).
pub struct EventLogModelRust {
    core: QVariant,
    entries: VecDeque<Entry>,
    link: cxx::UniquePtr<qobject::CoreLink>,
}

impl Default for EventLogModelRust {
    fn default() -> Self {
        Self {
            core: QVariant::default(),
            entries: VecDeque::new(),
            link: qobject::new_core_link(),
        }
    }
}

impl cxx_qt::Initialize for EventLogModel {
    fn initialize(mut self: Pin<&mut Self>) {
        self.as_mut()
            .on_core_changed(|model| {
                qobject::bind_log_model(&model.link, &model, &model.core);
            })
            .release();
    }
}

impl qobject::EventLogModel {
    pub fn core_event_received(
        mut self: Pin<&mut Self>,
        topic: &QString,
        kind: &QString,
        seq: f64,
        data_json: &QString,
    ) {
        let (topic, kind, data_json) =
            (topic.to_string(), kind.to_string(), data_json.to_string());
        if is_heartbeat(&topic, &kind, &data_json) {
            return;
        }
        let entry = Entry {
            topic,
            kind,
            seq,
            text: log_text(&data_json),
            received_ms: now_ms(),
        };
        if self.entries.len() >= EVENT_LOG_ROWS {
            // SAFETY: each change is begun and ended around it, on the GUI thread.
            unsafe {
                self.as_mut()
                    .begin_remove_rows(&QModelIndex::default(), 0, 0)
            };
            self.as_mut().rust_mut().entries.pop_front();
            unsafe { self.as_mut().end_remove_rows() };
        }
        let at = to_i32(self.entries.len());
        unsafe {
            self.as_mut()
                .begin_insert_rows(&QModelIndex::default(), at, at)
        };
        self.as_mut().rust_mut().entries.push_back(entry);
        unsafe { self.as_mut().end_insert_rows() };
    }

    pub fn get(&self, row: i32) -> QVariant {
        let Some(e) = usize::try_from(row).ok().and_then(|r| self.entries.get(r)) else {
            return QVariant::default();
        };
        let row = serde_json::json!({
            "topic": e.topic,
            "kind": e.kind,
            "seq": e.seq,
            "text": e.text,
            "receivedMs": e.received_ms,
        });
        json_variant(&row.to_string())
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        row_index(index)
            .and_then(|r| self.entries.get(r))
            .map_or_else(QVariant::default, |e| e.field(role))
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0
        } else {
            to_i32(self.entries.len())
        }
    }

    pub fn role_names(&self) -> cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray> {
        let mut hash = cxx_qt_lib::QHash::default();
        for (i, name) in EVENT_LOG_ROLES.iter().enumerate() {
            hash.insert(USER_ROLE + to_i32(i), QByteArray::from(*name));
        }
        hash
    }
}
