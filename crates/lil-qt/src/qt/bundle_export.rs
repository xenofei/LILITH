//! `BundleExport`, a QML element of module `Lilith.Core`: Diagnostics' save-file bridge
//! (ARCH-121, ARCH-122). `diagnostics.bundle` writes its zip in the core's diagnostics folder;
//! `save(file, source)` copies that zip to the file you picked ([`crate::csv::write_kept`]) on a
//! thread of its own, never the GUI thread (QA-026), and then emits `saved(path)` or
//! `failed(path, why)` on the GUI thread. `why` says why and what became of the file that was
//! there.
//!
//! - `file` is a `file:` URL (the file dialog's); `path` in the signals is its local path.
//! - `busy` is true from a `save` until its signal. A `save` while busy, or one whose `file` isn't
//!   a local file, returns false and writes nothing; else it returns true and its signal follows.
//! - The log names no path: a bundle's folder can name the owner's files.

use core::pin::Pin;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QUrl};

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
        type BundleExport = super::BundleExportRust;

        /// Copies the zip at `source` to `file` off the GUI thread (module docs); false when it
        /// can't start.
        #[qinvokable]
        fn save(self: Pin<&mut BundleExport>, file: &QUrl, source: &QString) -> bool;

        #[qsignal]
        fn saved(self: Pin<&mut BundleExport>, path: QString);

        #[qsignal]
        fn failed(self: Pin<&mut BundleExport>, path: QString, why: QString);
    }

    impl cxx_qt::Threading for BundleExport {}
}

#[derive(Default)]
pub struct BundleExportRust {
    busy: bool,
}

fn set_busy(mut obj: Pin<&mut qobject::BundleExport>, busy: bool) {
    if obj.busy != busy {
        obj.as_mut().rust_mut().busy = busy;
        obj.as_mut().busy_changed();
    }
}

/// The copy: `source` read whole (a bundle holds at most tens of MiB), then written as a CSV
/// export is.
fn copy(source: &str, path: &str) -> std::io::Result<()> {
    let bytes = std::fs::read(source).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("the bundle couldn't be read ({e}). Nothing was written."),
        )
    })?;
    crate::csv::write_kept(std::path::Path::new(path), &bytes)
}

impl qobject::BundleExport {
    fn save(mut self: Pin<&mut Self>, file: &QUrl, source: &QString) -> bool {
        if self.busy {
            return false;
        }
        let Some(path) = file.to_local_file().map(|p| p.to_string()) else {
            return false;
        };
        if path.is_empty() {
            return false;
        }
        let source = source.to_string();
        let thread = self.qt_thread();
        set_busy(self.as_mut(), true);
        let shown = path.clone();
        let spawned = std::thread::Builder::new()
            .name("lil-bundle-export".to_string())
            .spawn(move || {
                let outcome = copy(&source, &path);
                if let Err(e) = &outcome {
                    tracing::warn!(kind = ?e.kind(), "the diagnostics bundle wasn't saved");
                }
                let outcome = outcome.map_err(|e| e.to_string());
                // An export whose object is gone has nobody to tell.
                let _ = thread.queue(move |obj| finish(obj, &path, outcome));
            });
        if let Err(e) = spawned {
            let why = e.to_string();
            tracing::warn!(kind = ?e.kind(), "the diagnostics bundle's copy didn't start");
            let _ = self
                .qt_thread()
                .queue(move |obj| finish(obj, &shown, Err(why)));
        }
        true
    }
}

fn finish(mut obj: Pin<&mut qobject::BundleExport>, path: &str, outcome: Result<(), String>) {
    set_busy(obj.as_mut(), false);
    match outcome {
        Ok(()) => obj.saved(QString::from(path)),
        Err(why) => obj.failed(QString::from(path), QString::from(&why)),
    }
}
