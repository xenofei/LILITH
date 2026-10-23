//! `GridModel`: 10,000 rows, fetched 200 at a time.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;

        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, loaded)]
        #[qproperty(i32, fetches)]
        type GridModel = super::GridModelRust;
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginInsertRows"]
        fn begin_insert_rows(
            self: Pin<&mut GridModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );

        #[inherit]
        #[cxx_name = "endInsertRows"]
        fn end_insert_rows(self: Pin<&mut GridModel>);
    }

    unsafe extern "RustQt" {
        #[cxx_override]
        fn data(self: &GridModel, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &GridModel, parent: &QModelIndex) -> i32;

        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &GridModel) -> QHash_i32_QByteArray;

        #[cxx_override]
        #[cxx_name = "canFetchMore"]
        fn can_fetch_more(self: &GridModel, parent: &QModelIndex) -> bool;

        #[cxx_override]
        #[cxx_name = "fetchMore"]
        fn fetch_more(self: Pin<&mut GridModel>, parent: &QModelIndex);
    }

    extern "RustQt" {
        /// The next page, for a view that does not call fetchMore itself.
        #[qinvokable]
        #[cxx_name = "fetchPage"]
        fn fetch_page(self: Pin<&mut GridModel>);
    }
}

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

pub const TOTAL_ROWS: i32 = 10_000;
pub const PAGE_ROWS: i32 = 200;

const ROLE_LABEL: i32 = 0x0100; // Qt::UserRole
const ROLE_VALUE: i32 = 0x0101;
const ROLE_STATUS: i32 = 0x0102;

#[derive(Default)]
pub struct GridModelRust {
    loaded: i32,
    fetches: i32,
}

impl qobject::GridModel {
    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = index.row();
        if !index.is_valid() || row < 0 || row >= *self.loaded() {
            return QVariant::default();
        }
        match role {
            ROLE_LABEL => QVariant::from(&QString::from(&format!("asset {row:05}"))),
            ROLE_VALUE => QVariant::from(&((row * 7919) % 1000)),
            ROLE_STATUS => QVariant::from(&QString::from(
                ["queued", "running", "done", "failed"][(row % 4) as usize],
            )),
            _ => QVariant::default(),
        }
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() { 0 } else { *self.loaded() }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_LABEL, QByteArray::from("label"));
        roles.insert(ROLE_VALUE, QByteArray::from("value"));
        roles.insert(ROLE_STATUS, QByteArray::from("status"));
        roles
    }

    pub fn can_fetch_more(&self, parent: &QModelIndex) -> bool {
        !parent.is_valid() && *self.loaded() < TOTAL_ROWS
    }

    pub fn fetch_page(self: Pin<&mut Self>) {
        self.fetch_more(&QModelIndex::default());
    }

    pub fn fetch_more(mut self: Pin<&mut Self>, parent: &QModelIndex) {
        if parent.is_valid() {
            return;
        }
        let first = *self.loaded();
        let last = (first + PAGE_ROWS).min(TOTAL_ROWS) - 1;
        if last < first {
            return;
        }
        self.as_mut().begin_insert_rows(parent, first, last);
        self.as_mut().rust_mut().loaded = last + 1;
        self.as_mut().end_insert_rows();
        let fetches = *self.fetches() + 1;
        self.as_mut().set_fetches(fetches);
        self.as_mut().loaded_changed();
    }
}
