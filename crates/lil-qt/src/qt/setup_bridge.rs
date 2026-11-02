//! `SetupBridge`, a QML singleton of module `Lilith.Core`: the notification setup for the first-start
//! card and the Settings panel, which share it, so two presses, or a press on each, run it once
//! (APP-022; [`crate::setup`]).
//!
//! - `run()` runs [`crate::setup::set_up`] on a thread of its own, never the GUI thread (QA-026),
//!   and then emits `finished(reportJson)` on the GUI thread, with [`crate::setup::SetupReport`] as
//!   JSON. It returns false, and writes nothing, while one is running, in a process with no core
//!   session ([`super::serve`]: the QML tests have none, so nothing of theirs can reach a real
//!   registration), or when its thread can't start.
//! - `busy` is true from a `run` until its signal.
//! - `appMissing` is true, with `appPath` the path, while the channel's `lil-image.exe` isn't where
//!   the core checks for it: then there is no Set up to offer. Read once off the GUI thread as the
//!   object is made.
//! - An answer whose object is gone (the app closing) has nobody to tell. The thread goes on
//!   [`crate::csv::WRITES`], which `main` waits for when the event loop ends, so a write under way
//!   isn't cut off.

use core::pin::Pin;
use std::panic::{AssertUnwindSafe, catch_unwind};

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;

use crate::setup::{SetupReport, app_exe, app_missing, set_up};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(bool, busy, READ, NOTIFY)]
        #[qproperty(bool, app_missing, READ, NOTIFY)]
        #[qproperty(QString, app_path, READ, NOTIFY)]
        type SetupBridge = super::SetupBridgeRust;

        /// Runs the setup off the GUI thread (module docs); false when it didn't start.
        #[qinvokable]
        fn run(self: Pin<&mut SetupBridge>) -> bool;

        #[qsignal]
        fn finished(self: Pin<&mut SetupBridge>, report_json: QString);
    }

    impl cxx_qt::Threading for SetupBridge {}
    impl cxx_qt::Initialize for SetupBridge {}
}

#[derive(Default)]
pub struct SetupBridgeRust {
    busy: bool,
    app_missing: bool,
    app_path: QString,
}

impl cxx_qt::Initialize for qobject::SetupBridge {
    fn initialize(self: Pin<&mut Self>) {
        let Some(cfg) = super::setup() else {
            return;
        };
        let thread = self.qt_thread();
        let looked = std::thread::Builder::new()
            .name("lil-setup-check".to_string())
            .spawn(move || {
                let Some(exe) = app_exe(&cfg) else {
                    return;
                };
                if app_missing(&cfg.env, &exe) {
                    let path = exe.display().to_string();
                    let _ = thread.queue(move |mut obj| {
                        obj.as_mut().rust_mut().app_path = QString::from(&path);
                        obj.as_mut().app_path_changed();
                        obj.as_mut().rust_mut().app_missing = true;
                        obj.as_mut().app_missing_changed();
                    });
                }
            });
        if let Err(e) = looked {
            tracing::warn!(kind = ?e.kind(), "the setup's check didn't start");
        }
    }
}

impl qobject::SetupBridge {
    fn run(mut self: Pin<&mut Self>) -> bool {
        if self.busy {
            return false;
        }
        let Some(cfg) = super::setup() else {
            return false;
        };
        let thread = self.qt_thread();
        self.as_mut().rust_mut().busy = true;
        self.as_mut().busy_changed();
        let spawned = std::thread::Builder::new()
            .name("lil-setup".to_string())
            .spawn(move || {
                let report = catch_unwind(AssertUnwindSafe(|| set_up(&cfg)))
                    .unwrap_or_else(|_| SetupReport::stopped());
                let json = serde_json::to_string(&report).expect("a report serialises");
                // A setup whose object is gone has nobody to tell.
                let _ = thread.queue(move |mut obj| {
                    obj.as_mut().rust_mut().busy = false;
                    obj.as_mut().busy_changed();
                    obj.finished(QString::from(&json));
                });
            });
        match spawned {
            Ok(worker) => {
                crate::csv::WRITES.add(worker);
                true
            }
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "the setup didn't start");
                self.as_mut().rust_mut().busy = false;
                self.as_mut().busy_changed();
                false
            }
        }
    }
}
