//! `CsvExport`, a QML element of module `Lilith.Core`: the Usage & spend panel's save-file
//! bridge (APP-117). `save(file, rowsJson, columnsJson)` writes the rows the panel shows to the
//! file you picked ([`crate::csv::export`], through `replace_file`) on a thread of its own, never
//! the GUI thread (QA-026), and then emits `saved(path)` or `failed(path, why)` on the GUI
//! thread. `why` says why the write failed and what became of the file that was there.
//!
//! - `file` is a `file:` URL (the file dialog's); `path` in the signals is its local path.
//! - When the object goes (the app is closing), it waits for a write under way, so no temporary
//!   file stays behind.
//! - `busy` is true from a `save` until its signal. A `save` while busy, or one whose arguments
//!   don't read (not a local file, `rowsJson` not a JSON array, `columnsJson` not an array of
//!   strings), returns false and writes nothing; else it returns true and its signal follows.

use core::pin::Pin;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QUrl};
use serde_json::Value;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(bool, busy, READ, NOTIFY)]
        type CsvExport = super::CsvExportRust;

        /// Writes the rows to `file` off the GUI thread (module docs); false when it can't start.
        #[qinvokable]
        fn save(
            self: Pin<&mut CsvExport>,
            file: &QUrl,
            rows_json: &QString,
            columns_json: &QString,
        ) -> bool;

        #[qsignal]
        fn saved(self: Pin<&mut CsvExport>, path: QString);

        #[qsignal]
        fn failed(self: Pin<&mut CsvExport>, path: QString, why: QString);
    }

    impl cxx_qt::Threading for CsvExport {}
}

#[derive(Default)]
pub struct CsvExportRust {
    busy: bool,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for CsvExportRust {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn set_busy(mut obj: Pin<&mut qobject::CsvExport>, busy: bool) {
    if obj.busy != busy {
        obj.as_mut().rust_mut().busy = busy;
        obj.as_mut().busy_changed();
    }
}

impl qobject::CsvExport {
    fn save(
        mut self: Pin<&mut Self>,
        file: &QUrl,
        rows_json: &QString,
        columns_json: &QString,
    ) -> bool {
        if self.busy {
            return false;
        }
        let Some(path) = file.to_local_file().map(|p| p.to_string()) else {
            return false;
        };
        if path.is_empty() {
            return false;
        }
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&rows_json.to_string()) else {
            return false;
        };
        let Ok(columns) = serde_json::from_str::<Vec<String>>(&columns_json.to_string()) else {
            return false;
        };
        let thread = self.qt_thread();
        set_busy(self.as_mut(), true);
        let shown = path.clone();
        let spawned = std::thread::Builder::new()
            .name("lil-csv-export".to_string())
            .spawn(move || {
                let columns: Vec<&str> = columns.iter().map(String::as_str).collect();
                let outcome = crate::csv::export(std::path::Path::new(&path), &rows, &columns)
                    .map_err(|e| e.to_string());
                if let Err(why) = &outcome {
                    tracing::warn!(error = %why, "the usage CSV wasn't written");
                }
                // An export whose object is gone has nobody to tell.
                let _ = thread.queue(move |obj| finish(obj, &path, outcome));
            });
        match spawned {
            Ok(worker) => self.as_mut().rust_mut().worker = Some(worker),
            Err(e) => {
                let why = e.to_string();
                tracing::warn!(error = %why, "the usage CSV export didn't start");
                let _ = self
                    .qt_thread()
                    .queue(move |obj| finish(obj, &shown, Err(why)));
            }
        }
        true
    }
}

fn finish(mut obj: Pin<&mut qobject::CsvExport>, path: &str, outcome: Result<(), String>) {
    set_busy(obj.as_mut(), false);
    match outcome {
        Ok(()) => obj.saved(QString::from(path)),
        Err(why) => obj.failed(QString::from(path), QString::from(&why)),
    }
}
