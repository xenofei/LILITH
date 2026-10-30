//! `JobText`, a QML singleton of module `Lilith.Core`: the words of [`crate::format`] for the
//! Jobs panel and the cost and time chip (APP-114, APP-140). QML passes estimates and caps as
//! JSON (an `Estimate`, `caps.get`'s `CapsView`); anything that doesn't parse is no estimate.
//! A count or a time that is "none" is passed as a negative number.
//!
//! - `chipText(estimateJson, capSharePermille)`; `capSharePermille(estimateJson, capsJson)`
//!   (-1: none);
//! - `etaText(estimateJson, elapsedS)`; `runningLeft(estimateJson, elapsedS)` (-1: none);
//! - `queuedEtaText(runningLeft, aheadJson, ownJson)`: `aheadJson` is a JSON array of the
//!   estimates (or nulls) of the jobs ahead in lane order, the running job without a `wall_s`
//!   and each job the panel can't see included as null;
//! - `elapsedText(s)`.

use cxx_qt_lib::QString;
use lil_protocol::app::CapsView;
use lil_protocol::estimate::{Estimate, Spread};

use crate::format;

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
        type JobText = super::JobTextRust;

        #[qinvokable]
        fn chip_text(self: &JobText, estimate_json: &QString, cap_share_permille: i32) -> QString;

        #[qinvokable]
        fn cap_share_permille(self: &JobText, estimate_json: &QString, caps_json: &QString) -> i32;

        #[qinvokable]
        fn eta_text(self: &JobText, estimate_json: &QString, elapsed_s: f64) -> QString;

        #[qinvokable]
        fn running_left(self: &JobText, estimate_json: &QString, elapsed_s: f64) -> f64;

        #[qinvokable]
        fn queued_eta_text(
            self: &JobText,
            running_left: f64,
            ahead_json: &QString,
            own_json: &QString,
        ) -> QString;

        #[qinvokable]
        fn elapsed_text(self: &JobText, s: f64) -> QString;
    }
}

#[derive(Default)]
pub struct JobTextRust;

fn estimate(json: &QString) -> Estimate {
    serde_json::from_str::<Option<Estimate>>(&json.to_string())
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn wall(e: &Estimate) -> Option<Spread> {
    e.wall_s.as_ref()?.spread()
}

impl qobject::JobText {
    fn chip_text(&self, estimate_json: &QString, cap_share_permille: i32) -> QString {
        let share = u32::try_from(cap_share_permille).ok();
        QString::from(&format::chip_text(&estimate(estimate_json), share))
    }

    fn cap_share_permille(&self, estimate_json: &QString, caps_json: &QString) -> i32 {
        let Ok(caps) = serde_json::from_str::<CapsView>(&caps_json.to_string()) else {
            return -1;
        };
        format::cap_share_permille(&estimate(estimate_json), &caps)
            .and_then(|p| i32::try_from(p).ok())
            .unwrap_or(-1)
    }

    fn eta_text(&self, estimate_json: &QString, elapsed_s: f64) -> QString {
        QString::from(&format::eta_text(&estimate(estimate_json), elapsed_s))
    }

    fn running_left(&self, estimate_json: &QString, elapsed_s: f64) -> f64 {
        format::running_left(&estimate(estimate_json), elapsed_s).unwrap_or(-1.0)
    }

    fn queued_eta_text(
        &self,
        running_left: f64,
        ahead_json: &QString,
        own_json: &QString,
    ) -> QString {
        let ahead: Vec<Option<Estimate>> =
            serde_json::from_str(&ahead_json.to_string()).unwrap_or_default();
        let ahead: Vec<Option<Spread>> = ahead.iter().map(|e| e.as_ref().and_then(wall)).collect();
        let refs: Vec<Option<&Spread>> = ahead.iter().map(Option::as_ref).collect();
        let own = wall(&estimate(own_json));
        let running = (running_left >= 0.0).then_some(running_left);
        QString::from(&format::queued_eta_text(running, &refs, own.as_ref()))
    }

    fn elapsed_text(&self, s: f64) -> QString {
        QString::from(&format::elapsed_text(s))
    }
}
