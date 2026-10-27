//! The list models of module `Lilith.Core` (APP-025): Rust `QAbstractListModel`s.
//!
//! **`JsonListModel`** pages a list tool's rows and keeps them live from a topic:
//! - **Properties:** `core` (the `Core` singleton, or any object with its API), `tool`,
//!   `argsJson` (the tool's arguments, a JSON object), `listKey` (where the rows are in the
//!   tool's `structured` output), `idKey` (each row's id), `pageSize` (default 100), `topic`,
//!   `eventKind` and `rowKey` (below), `roleNames` (the row fields a delegate reads, as roles),
//!   and `loaded` (read-only: the first page of the current load is in).
//! - **Paging** ([`crate::paging::PageState`]): a load asks for the first page with `limit` set
//!   to `pageSize`; `canFetchMore`/`fetchMore` ask for the next with the last page's
//!   `next_cursor` as `cursor`, one at a time, until it is `null`. A row a later page holds
//!   already (inserted live meanwhile) is updated in place, not added twice.
//! - **Events.** The model follows `topic` through `core` (once, before its first page is asked
//!   for, so no event is missed), and upserts each `event` of it by `idKey`
//!   ([`crate::paging::Rows`]): a new id goes first (the lists are newest first), a known one is
//!   updated in place (one `dataChanged`) unless the row is older than the one held. With
//!   `eventKind` set, only events whose `kind` is that are rows, the others are skipped; with
//!   `rowKey` set, the row is the event's value under it, else the event's data itself. (The
//!   `jobs` topic: `eventKind: "job"`, `rowKey: "job"`; its `queue` events are skipped.)
//! - **Reloads.** Changing `core`, `tool`, `argsJson`, `listKey`, `idKey` or `topic` reloads it
//!   on the next turn of the event loop (once for several changes), and so do `reload()`, a
//!   `snapshot` and a `resync_required` of its topic (a seq of 0: a core restart or a workspace
//!   switch), never `Ready` (ARCH-100). A reload gives up the page asked for, and `loaded` is
//!   false until its first page is in: the topic's events are dropped meanwhile, and a panel
//!   holds back its writes (calls can reach a new connection before the app sees the mark). The
//!   rows held stay until that page replaces them (one model reset).
//! - `get(row)`: the row as a map. When the model goes, its page asked for is cancelled and its
//!   topic left.
//!
//! **`EventLogModel`** (Diagnostics' event log): the newest [`EVENT_LOG_ROWS`] events of every
//! topic the app follows, oldest first, from when it was made: the shell makes one at start. It
//! reads `core`'s `coreEvent` and subscribes to nothing itself. Roles `topic`, `kind`, `seq`,
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
    rows: Rows,
    page: PageState,
    /// The page asked for: its request id, and whether it is a load's first.
    asked: Option<(QString, bool)>,
    /// The topic followed through `core`.
    followed: Option<QString>,
    reload_queued: bool,
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
            rows: Rows::new(),
            page: PageState::new(),
            asked: None,
            followed: None,
            reload_queued: false,
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

    pub fn reload(mut self: Pin<&mut Self>) {
        if let Some((id, _)) = self.as_mut().rust_mut().asked.take() {
            qobject::core_cancel(&self.link, &id);
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
        self.as_mut().rust_mut().page.reset();
        self.as_mut().set_loaded_to(false);
        self.ask(true);
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
        let id_key = self.id_key.to_string();
        if first {
            // SAFETY: the reset is begun and ended around the change, on the GUI thread.
            unsafe { self.as_mut().begin_reset_model() };
            self.as_mut().rust_mut().rows.replace(page, &id_key);
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
    }

    pub fn core_call_failed(mut self: Pin<&mut Self>, id: &QString, error_json: &QString) {
        if self.as_mut().answered(id).is_some() {
            self.as_mut().rust_mut().page.on_failed();
            tracing::warn!(tool = %self.tool, error = %error_json, "a list page failed");
        }
    }

    /// A list tool never waits on a confirmation: the page is given up as failed.
    pub fn core_call_pending(mut self: Pin<&mut Self>, id: &QString, pending_json: &QString) {
        if self.as_mut().answered(id).is_some() {
            qobject::core_cancel(&self.link, id);
            self.as_mut().rust_mut().page.on_failed();
            tracing::warn!(tool = %self.tool, pending = %pending_json, "a list page asked for a confirmation");
        }
    }

    pub fn core_event_received(
        self: Pin<&mut Self>,
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
            "event" if self.loaded => {
                let Ok(mut data) = serde_json::from_str::<Value>(&data_json.to_string()) else {
                    return;
                };
                let event_kind = self.event_kind.to_string();
                if !event_kind.is_empty()
                    && data.get("kind").and_then(Value::as_str) != Some(event_kind.as_str())
                {
                    return;
                }
                let row_key = self.row_key.to_string();
                let row = if row_key.is_empty() {
                    data
                } else {
                    data[row_key.as_str()].take()
                };
                if row.is_object() {
                    self.upsert(row);
                }
            }
            _ => {}
        }
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

/// An event's data as the log shows it (module docs).
fn log_text(data_json: &str) -> String {
    let mut text = data_json.to_string();
    if text.contains("\"data_b64\"")
        && let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&text)
        && let Some(Value::String(b64)) = data.remove("data_b64")
    {
        let bytes = b64.trim_end_matches('=').len() * 3 / 4;
        data.insert("bytes".to_string(), Value::from(bytes));
        text = Value::Object(data).to_string();
    }
    if text.chars().count() > EVENT_LOG_TEXT_CHARS {
        text = text.chars().take(EVENT_LOG_TEXT_CHARS - 1).collect();
        text.push('…');
    }
    text
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
        let entry = Entry {
            topic: topic.to_string(),
            kind: kind.to_string(),
            seq,
            text: log_text(&data_json.to_string()),
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
