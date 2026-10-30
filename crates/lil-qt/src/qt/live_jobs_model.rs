//! `LiveJobsModel` (module `Lilith.Core`): the Jobs panel's running and queued jobs as a list
//! model over [`crate::live_jobs::LiveJobs`], which orders them and works out each one's place,
//! ETA and elapsed time, so the GUI thread only applies the changes (APP-114, APP-139).
//!
//! The panel makes the calls and hands over the raw result JSON, never parsed in QML:
//! - `beginRead(fresh)` (`fresh` after a snapshot or a `resync_required`: nothing is carried
//!   over), then `setRunning(resultJson)` for `jobs.list {status: "running"}` and
//!   `addQueuePage(resultJson)` for each `jobs.list {order: "queue"}` page (it returns the next
//!   page's cursor, empty when the read has them all), then `commit(nowMs)`;
//! - `applyJob(jobJson, nowMs)` for each `jobs` event's job: 0 when it is no live row, 1 when it
//!   is one and its status stayed, 2 when it came, went or changed status (the queue order may
//!   have changed);
//! - `tick(nowMs)`: the clock, which updates only the rows whose times read differently.
//!
//! `lanesJson()` is the read's `lanes` (`QueueState`s) as JSON, `overview(app)`
//! [`LiveJobs::overview`] as JSON; `get(row)` a row as a map; `rowOf(jobId)` its row, or -1; `loaded` turns true
//! at the first commit. Roles: the `JobSummary` fields, `waiting_for_earlier_prompt` among them,
//! and `place` (the queue read's place, null without one), `in_read` (whether the read listed it),
//! `eta_text`, `elapsed_text` and `batch_size`.

use core::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QModelIndex, QString, QVariant};
use serde_json::Value;

use super::core_object::qobject::json_variant;
use crate::live_jobs::{LiveJobs, LiveRow, Op, apply};

/// `Qt::UserRole`: the first role.
const USER_ROLE: i32 = 0x0100;

/// The roles, in order.
pub const ROLES: [&str; 33] = [
    "job_id",
    "kind",
    "lane",
    "priority",
    "status",
    "substatus",
    "created",
    "updated",
    "started",
    "finished",
    "attempt",
    "progress",
    "error",
    "retry_of",
    "batch_id",
    "index",
    "status_seq",
    "origin",
    "phase",
    "subtasks",
    "indeterminate",
    "unresponsive",
    "pause_reasons",
    "estimate",
    "queue_position",
    "watchdog",
    "all_cached",
    "waiting_for_earlier_prompt",
    "place",
    "in_read",
    "eta_text",
    "elapsed_text",
    "batch_size",
];

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
        #[qproperty(bool, loaded, READ, NOTIFY)]
        #[qproperty(i32, count, READ, NOTIFY)]
        type LiveJobsModel = super::LiveJobsModelRust;

        #[qinvokable]
        fn begin_read(self: Pin<&mut LiveJobsModel>, fresh: bool);
        #[qinvokable]
        fn add_queue_page(self: Pin<&mut LiveJobsModel>, result_json: &QString) -> QString;
        #[qinvokable]
        fn set_running(self: Pin<&mut LiveJobsModel>, result_json: &QString);
        #[qinvokable]
        fn commit(self: Pin<&mut LiveJobsModel>, now_ms: f64);
        #[qinvokable]
        fn apply_job(self: Pin<&mut LiveJobsModel>, job_json: &QString, now_ms: f64) -> i32;
        #[qinvokable]
        fn tick(self: Pin<&mut LiveJobsModel>, now_ms: f64);
        #[qinvokable]
        fn lanes_json(self: &LiveJobsModel) -> QString;
        #[qinvokable]
        fn overview(self: &LiveJobsModel, app: &QString) -> QString;
        #[qinvokable]
        fn get(self: &LiveJobsModel, row: i32) -> QVariant;
        #[qinvokable]
        fn row_of(self: &LiveJobsModel, job_id: &QString) -> i32;

        #[cxx_override]
        fn data(self: &LiveJobsModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        fn row_count(self: &LiveJobsModel, parent: &QModelIndex) -> i32;
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_hash(self: &LiveJobsModel) -> QHash_i32_QByteArray;

        #[inherit]
        unsafe fn begin_insert_rows(
            self: Pin<&mut LiveJobsModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        unsafe fn end_insert_rows(self: Pin<&mut LiveJobsModel>);
        #[inherit]
        unsafe fn begin_remove_rows(
            self: Pin<&mut LiveJobsModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        unsafe fn end_remove_rows(self: Pin<&mut LiveJobsModel>);
        #[inherit]
        unsafe fn begin_move_rows(
            self: Pin<&mut LiveJobsModel>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            destination_parent: &QModelIndex,
            destination_child: i32,
        ) -> bool;
        #[inherit]
        unsafe fn end_move_rows(self: Pin<&mut LiveJobsModel>);
        #[inherit]
        fn index(self: &LiveJobsModel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
        #[inherit]
        #[qsignal]
        fn data_changed(
            self: Pin<&mut LiveJobsModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );
    }
}

pub use qobject::LiveJobsModel;

/// `LiveJobsModel`'s Rust side (module docs): the rows as the views know them, and the live
/// jobs that say what they should be.
#[derive(Default)]
pub struct LiveJobsModelRust {
    loaded: bool,
    count: i32,
    live: LiveJobs,
    shown: Vec<LiveRow>,
}

fn to_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

fn ms(now_ms: f64) -> i64 {
    if now_ms.is_finite() { now_ms as i64 } else { 0 }
}

fn parse(json: &QString) -> Value {
    serde_json::from_str(&json.to_string()).unwrap_or(Value::Null)
}

fn to_variant(value: &Value) -> QVariant {
    match value {
        Value::Null => QVariant::default(),
        Value::Bool(b) => QVariant::from(b),
        Value::Number(n) => QVariant::from(&n.as_f64().unwrap_or_default()),
        Value::String(s) => QVariant::from(&QString::from(s.as_str())),
        Value::Array(_) | Value::Object(_) => json_variant(&value.to_string()),
    }
}

fn field(row: &LiveRow, name: &str) -> QVariant {
    match name {
        "place" => match row.place {
            Some(Some(p)) => QVariant::from(&f64::from(p)),
            _ => QVariant::default(),
        },
        "in_read" => QVariant::from(&row.place.is_some()),
        "eta_text" => QVariant::from(&QString::from(row.eta_text.as_str())),
        "elapsed_text" => QVariant::from(&QString::from(row.elapsed_text.as_str())),
        "batch_size" => QVariant::from(&f64::from(row.batch_size)),
        _ => row.job.get(name).map_or_else(QVariant::default, to_variant),
    }
}

impl qobject::LiveJobsModel {
    fn begin_read(mut self: Pin<&mut Self>, fresh: bool) {
        let live = &mut self.as_mut().rust_mut().live;
        if fresh {
            live.begin_fresh_read();
        } else {
            live.begin_read();
        }
    }

    fn add_queue_page(mut self: Pin<&mut Self>, result_json: &QString) -> QString {
        let page = parse(result_json);
        let next = self.as_mut().rust_mut().live.add_queue_page(&page);
        QString::from(next.unwrap_or_default().as_str())
    }

    fn set_running(mut self: Pin<&mut Self>, result_json: &QString) {
        let result = parse(result_json);
        self.as_mut().rust_mut().live.set_running(&result);
    }

    fn commit(mut self: Pin<&mut Self>, now_ms: f64) {
        let ops = self.as_mut().rust_mut().live.commit(ms(now_ms));
        self.as_mut().apply_ops(ops);
        if let Some(listed) = self.live.capped() {
            tracing::warn!(
                listed,
                "the queue read stopped at its page cap: the queued jobs after these aren't listed"
            );
        }
        if !self.loaded {
            self.as_mut().rust_mut().loaded = true;
            self.as_mut().loaded_changed();
        }
    }

    fn apply_job(mut self: Pin<&mut Self>, job_json: &QString, now_ms: f64) -> i32 {
        let job = parse(job_json);
        let id = job
            .get("job_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let before = self.live.status_of(&id);
        let ops = self.as_mut().rust_mut().live.apply_job(job, ms(now_ms));
        self.as_mut().apply_ops(ops);
        let after = self.live.status_of(&id);
        match (before, after) {
            (None, None) => 0,
            (b, a) if b == a => 1,
            _ => 2,
        }
    }

    fn tick(mut self: Pin<&mut Self>, now_ms: f64) {
        let ops = self.as_mut().rust_mut().live.tick(ms(now_ms));
        self.apply_ops(ops);
    }

    fn lanes_json(&self) -> QString {
        QString::from(&Value::Array(self.live.lanes().to_vec()).to_string())
    }

    fn overview(&self, app: &QString) -> QString {
        QString::from(&self.live.overview(&app.to_string()).to_string())
    }

    fn get(&self, row: i32) -> QVariant {
        let Some(r) = usize::try_from(row).ok().and_then(|r| self.shown.get(r)) else {
            return QVariant::default();
        };
        let mut map = (*r.job).clone();
        if let Value::Object(m) = &mut map {
            m.insert(
                "place".into(),
                r.place.flatten().map_or(Value::Null, Value::from),
            );
            m.insert("in_read".into(), Value::from(r.place.is_some()));
            m.insert("eta_text".into(), Value::from(r.eta_text.as_str()));
            m.insert("elapsed_text".into(), Value::from(r.elapsed_text.as_str()));
            m.insert("batch_size".into(), Value::from(r.batch_size));
        }
        to_variant(&map)
    }

    fn row_of(&self, job_id: &QString) -> i32 {
        let id = job_id.to_string();
        self.shown
            .iter()
            .position(|r| r.id == id)
            .map_or(-1, to_i32)
    }

    /// Applies the changes in order, each between its begin and end call, on the GUI thread.
    fn apply_ops(mut self: Pin<&mut Self>, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        let root = QModelIndex::default();
        // Updates of rows next to each other are one `dataChanged` over their range.
        let mut changed: Option<(usize, usize)> = None;
        for op in ops {
            if let Op::Update { at, .. } = &op {
                apply(&mut self.as_mut().rust_mut().shown, &op);
                changed = match changed {
                    Some((first, last)) if *at == last + 1 => Some((first, *at)),
                    other => {
                        if let Some((first, last)) = other {
                            self.as_mut().changed(first, last);
                        }
                        Some((*at, *at))
                    }
                };
                continue;
            }
            if let Some((first, last)) = changed.take() {
                self.as_mut().changed(first, last);
            }
            match &op {
                Op::Remove { first, last } => {
                    // SAFETY: begun and ended around the change, on the GUI thread.
                    unsafe {
                        self.as_mut()
                            .begin_remove_rows(&root, to_i32(*first), to_i32(*last));
                    }
                    apply(&mut self.as_mut().rust_mut().shown, &op);
                    unsafe { self.as_mut().end_remove_rows() };
                }
                Op::Insert { at, rows } => {
                    let last = at + rows.len() - 1;
                    // SAFETY: as above.
                    unsafe {
                        self.as_mut()
                            .begin_insert_rows(&root, to_i32(*at), to_i32(last));
                    }
                    apply(&mut self.as_mut().rust_mut().shown, &op);
                    unsafe { self.as_mut().end_insert_rows() };
                }
                Op::Move { from, to } => {
                    // SAFETY: as above; `to` is never `from` or `from + 1`.
                    let moved = unsafe {
                        self.as_mut().begin_move_rows(
                            &root,
                            to_i32(*from),
                            to_i32(*from),
                            &root,
                            to_i32(*to),
                        )
                    };
                    apply(&mut self.as_mut().rust_mut().shown, &op);
                    if moved {
                        unsafe { self.as_mut().end_move_rows() };
                    }
                }
                Op::Update { .. } => {}
            }
        }
        if let Some((first, last)) = changed {
            self.as_mut().changed(first, last);
        }
        let count = to_i32(self.shown.len());
        if count != self.count {
            self.as_mut().rust_mut().count = count;
            self.count_changed();
        }
    }

    fn changed(self: Pin<&mut Self>, first: usize, last: usize) {
        let root = QModelIndex::default();
        let top = self.index(to_i32(first), 0, &root);
        let bottom = self.index(to_i32(last), 0, &root);
        self.data_changed(&top, &bottom, &cxx_qt_lib::QList::default());
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = (!index.parent().is_valid())
            .then(|| usize::try_from(index.row()).ok())
            .flatten()
            .and_then(|r| self.shown.get(r));
        let name = usize::try_from(role - USER_ROLE)
            .ok()
            .and_then(|r| ROLES.get(r));
        match (row, name) {
            (Some(row), Some(name)) => field(row, name),
            _ => QVariant::default(),
        }
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0
        } else {
            to_i32(self.shown.len())
        }
    }

    pub fn role_hash(&self) -> cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray> {
        let mut hash = cxx_qt_lib::QHash::default();
        for (i, name) in ROLES.iter().enumerate() {
            hash.insert(USER_ROLE + to_i32(i), QByteArray::from(*name));
        }
        hash
    }
}
