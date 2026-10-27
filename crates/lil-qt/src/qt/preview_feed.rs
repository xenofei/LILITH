//! The live-preview feed (APP-031, APP-037): the session's event pump ([`super::runtime`]) hands
//! it every event QML follows before QML sees it, off the GUI thread.
//!
//! - A `preview:<job_id>` frame goes to the image provider ([`crate::preview::take_frame`]):
//!   decoded from base64 here, from its JPEG or PNG by the provider, both on the pump's thread,
//!   and the event goes on as `{format, bytes, url}`, its URL naming the frame by the bridge's
//!   own count (one up per frame, so no two frames share one).
//! - A job's frame is dropped when an event QML follows shows the job finished
//!   ([`crate::preview::ended_job`]), and when the app stops following the job's
//!   `preview:<job_id>` ([`forgotten`]), so the provider holds frames only for jobs on show.
//!
//! The sink is the shim's provider, which `main.cpp` names (`lil_qt::setPreviewSink`,
//! `src/qt/preview_sink.h`). The QML tests' FakeCore runs the same step through
//! `preview_feed_event`.

use std::sync::atomic::{AtomicU64, Ordering};

use lil_protocol::EventParams;
use serde_json::Value;

use crate::preview::{ended_job, frame_url, take_frame};

#[cxx_qt::bridge]
pub mod ffi {
    #[namespace = "lil_qt"]
    unsafe extern "C++" {
        include!("lil_qt/src/qt/preview_sink.h");

        /// Hands a frame to the provider; false when there is none or the frame doesn't decode.
        #[cxx_name = "previewPut"]
        fn preview_put(job: &str, bytes: &[u8]) -> bool;

        #[cxx_name = "previewDrop"]
        fn preview_drop(job: &str);
    }

    #[namespace = "lil_qt"]
    extern "Rust" {
        /// The feed's step for an event's data (JSON) on `topic`, as the pump runs it: the data
        /// QML then gets, as JSON.
        fn preview_feed_event(topic: &str, data_json: &str) -> String;
    }
}

/// The frames handed to the provider so far.
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// The feed's step for one event (module docs).
pub fn on_event(ev: &mut EventParams) {
    apply(&ev.topic, &mut ev.data);
}

/// The app no longer follows `topic`: a job's frame goes with its `preview:<job_id>`.
pub fn forgotten(topic: &str) {
    if let Some(job) = topic.strip_prefix("preview:").filter(|job| !job.is_empty()) {
        ffi::preview_drop(job);
    }
}

fn apply(topic: &str, data: &mut Value) {
    if let Some(frame) = take_frame(topic, data) {
        if ffi::preview_put(&frame.job, &frame.bytes) {
            let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
            data["url"] = Value::from(frame_url(&frame.job, n));
        }
    } else if let Some(job) = ended_job(topic, data) {
        ffi::preview_drop(&job);
    }
}

fn preview_feed_event(topic: &str, data_json: &str) -> String {
    let mut data = serde_json::from_str(data_json).unwrap_or(Value::Null);
    apply(topic, &mut data);
    data.to_string()
}
