//! What the bridge does with live previews (APP-031, APP-037), Qt-free: the `preview:<job_id>`
//! events carry a frame as `{format, data_b64}` (`format` `jpeg` or `png`), and the bridge
//! takes it out before QML sees the event.
//!
//! - [`take_frame`] decodes the frame and leaves the event's data as `{format, bytes, url}`:
//!   `bytes` is the frame's size and `url` its `image://lilpreview/<job_id>/<frame>`, set once the
//!   provider holds the frame (`null` until then, and for a frame that doesn't decode). No image
//!   byte reaches QML: not in an event, a property or a model role.
//! - `<frame>` is the bridge's own count, one up for every frame of the process, so each frame
//!   has its own URL, a core restart included (the core's seqs start again).
//! - [`ended_job`] names the job a `status` event on `job:<id>`, a `job` event on `jobs`, or a
//!   `snapshot` of `job:<id>` (the job itself, after a reconnect) shows finished (`succeeded`,
//!   `failed` or `cancelled`): its frame is then dropped.

use base64::Engine as _;
use lil_protocol::EventKind;
use serde_json::{Value, json};

/// The image provider's scheme and host, as QML's `Image.source` names it.
pub const PREVIEW_URL_PREFIX: &str = "image://lilpreview/";

/// A decoded frame, for the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub job: String,
    /// `jpeg` or `png`, as the event says (the provider also reads the bytes' own header).
    pub format: String,
    pub bytes: Vec<u8>,
}

/// The URL of frame `frame` of job `job`.
pub fn frame_url(job: &str, frame: u64) -> String {
    format!("{PREVIEW_URL_PREFIX}{job}/{frame}")
}

/// Takes the frame out of a `preview:<job_id>` event's `data` (module docs): `None` for another
/// topic, or for a frame that doesn't decode, whose data is cut down all the same.
pub fn take_frame(topic: &str, data: &mut Value) -> Option<Frame> {
    let job = topic
        .strip_prefix("preview:")
        .filter(|job| !job.is_empty())?;
    let b64 = data.get("data_b64")?.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok();
    let format = data
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let size = bytes.as_ref().map_or(0, Vec::len);
    *data = json!({"format": format, "bytes": size, "url": null});
    Some(Frame {
        job: job.to_string(),
        format,
        bytes: bytes?,
    })
}

/// The job whose end `data`, an event of `kind` on `topic`, shows (module docs).
pub fn ended_job(topic: &str, kind: EventKind, data: &Value) -> Option<String> {
    let finished = |job: &Value| {
        matches!(
            job.get("status").and_then(Value::as_str),
            Some("succeeded" | "failed" | "cancelled")
        )
    };
    let id_of = |job: &Value| job.get("job_id")?.as_str().map(str::to_string);
    match (topic.strip_prefix("job:"), kind) {
        // A snapshot of job:<id> is the JobSummary itself (review M-1).
        (Some(of), EventKind::Snapshot) => {
            (finished(data) && id_of(data).as_deref() == Some(of)).then(|| of.to_string())
        }
        (Some(of), EventKind::Event) => {
            let job = data.get("job")?;
            let status = data.get("kind").and_then(Value::as_str) == Some("status");
            (status && finished(job) && id_of(job).as_deref() == Some(of)).then(|| of.to_string())
        }
        (None, EventKind::Event) if topic == "jobs" => {
            let job = data.get("job")?;
            let shown = data.get("kind").and_then(Value::as_str) == Some("job");
            if shown && finished(job) {
                id_of(job)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1: &[u8] = &[0x89, b'P', b'N', b'G', 1, 2, 3];

    #[test] // spec: APP-037, APP-031
    fn preview_frames_leave_no_image_bytes_in_the_event() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(PNG_1X1);
        let mut data = json!({"format": "png", "data_b64": b64});
        let frame = take_frame("preview:j_1", &mut data).expect("a frame");
        assert_eq!(
            frame,
            Frame {
                job: "j_1".to_string(),
                format: "png".to_string(),
                bytes: PNG_1X1.to_vec()
            }
        );
        assert_eq!(data, json!({"format": "png", "bytes": 7, "url": null}));
        assert_eq!(frame_url("j_1", 12), "image://lilpreview/j_1/12");

        // Bytes that aren't base64: no frame, and no payload left either.
        let mut bad = json!({"format": "jpeg", "data_b64": "%%%"});
        assert_eq!(take_frame("preview:j_1", &mut bad), None);
        assert_eq!(bad, json!({"format": "jpeg", "bytes": 0, "url": null}));

        // Another topic, or a preview topic without a job, is left alone.
        let mut other = json!({"format": "png", "data_b64": b64});
        assert_eq!(take_frame("job:j_1", &mut other), None);
        assert!(other.get("data_b64").is_some());
        assert_eq!(take_frame("preview:", &mut data.clone()), None);
    }

    #[test] // spec: APP-031
    fn preview_a_finished_job_is_named() {
        let status = |s: &str| json!({"kind": "status", "job": {"job_id": "j_1", "status": s}});
        for s in ["succeeded", "failed", "cancelled"] {
            assert_eq!(
                ended_job("job:j_1", EventKind::Event, &status(s)).as_deref(),
                Some("j_1"),
                "{s}"
            );
            let on_jobs = json!({"kind": "job", "job": {"job_id": "j_2", "status": s}});
            assert_eq!(
                ended_job("jobs", EventKind::Event, &on_jobs).as_deref(),
                Some("j_2"),
                "{s}"
            );
        }
        for s in ["queued", "running"] {
            assert_eq!(
                ended_job("job:j_1", EventKind::Event, &status(s)),
                None,
                "{s}"
            );
        }
        let progress = json!({"kind": "progress", "progress": {"value": 1, "max": 2}});
        assert_eq!(ended_job("job:j_1", EventKind::Event, &progress), None);
        assert_eq!(
            ended_job(
                "jobs",
                EventKind::Event,
                &json!({"kind": "queue", "state": {}})
            ),
            None
        );
        assert_eq!(
            ended_job("assets", EventKind::Event, &status("succeeded")),
            None
        );

        // Review M-1: after a reconnect, a job:<id> whose history is gone gets a snapshot of
        // the JobSummary itself, its status at the top (its `kind` is the job's kind).
        let summary = |s: &str| json!({"job_id": "j_1", "kind": "comfyui.workflow", "status": s});
        assert_eq!(
            ended_job("job:j_1", EventKind::Snapshot, &summary("succeeded")).as_deref(),
            Some("j_1")
        );
        assert_eq!(
            ended_job("job:j_1", EventKind::Snapshot, &summary("running")),
            None
        );
        assert_eq!(
            ended_job("job:j_1", EventKind::Event, &summary("failed")),
            None
        );
        assert_eq!(
            ended_job("job:j_2", EventKind::Snapshot, &summary("failed")),
            None
        );
        assert_eq!(
            ended_job("jobs", EventKind::Snapshot, &summary("failed")),
            None
        );
        assert_eq!(
            ended_job("job:j_1", EventKind::ResyncRequired, &Value::Null),
            None
        );
    }
}
