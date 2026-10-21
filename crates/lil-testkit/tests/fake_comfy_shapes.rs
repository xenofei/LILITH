//! The fake ComfyUI against recorded ComfyUI shapes (QA-066, QA-067): the drift check of key
//! sets and JSON types, and the behaviours that shapes can't show (Task 6's drift list): a bad
//! `/api/jobs` parameter answers 400, the websocket's `feature_flags` reply has the recorded
//! keys, and both cancels are answered as recorded.
//!
//! The recordings are made by `lil test --real --record crates/lil-testkit/fixtures/comfyui`
//! against the owner's ComfyUI (`real_comfyui_behaviours_match_the_fake`); the newest version
//! recorded is the one compared. Until a version is recorded, the two recording checks say so
//! and pass. The probe itself runs against the fake every time.

use lil_testkit::FakeComfy;
use lil_testkit::comfy_shapes::{
    Capture, FILES, behaviour_drift, capture, capture_drift, fixtures_dir, newest_recorded,
};

/// The newest recording, or `None` (said on stdout) when nothing is recorded yet.
fn recording() -> Option<Capture> {
    let Some((version, folder)) = newest_recorded(&fixtures_dir()) else {
        println!(
            "no ComfyUI shapes are recorded under {} yet: record them with \
             `lil test --real --record crates/lil-testkit/fixtures/comfyui`",
            fixtures_dir().display()
        );
        return None;
    };
    let recorded =
        Capture::load(&folder).unwrap_or_else(|e| panic!("the {version} recording: {e}"));
    // QA-067: a recording holds nothing the scrub would still change.
    assert_eq!(
        recorded,
        recorded.scrubbed(),
        "the {version} recording is not scrubbed"
    );
    Some(recorded)
}

#[test] // spec: QA-067, QA-066
fn fake_comfy_matches_recorded_shapes() {
    let Some(recorded) = recording() else {
        return;
    };
    let fake = FakeComfy::start();
    let probe = capture(&fake.url()).expect("the probe runs against the fake");
    let drift = capture_drift(&recorded, &probe);
    assert!(
        drift.is_empty(),
        "the fake's shapes drifted from ComfyUI {}:\n{}",
        recorded.version,
        drift.join("\n")
    );
}

#[test] // spec: QA-067, QA-066
fn fake_comfy_behaves_like_the_recording() {
    let Some(recorded) = recording() else {
        return;
    };
    let fake = FakeComfy::start();
    let probe = capture(&fake.url()).expect("the probe runs against the fake");
    let drift = behaviour_drift(&recorded, &probe);
    assert!(
        drift.is_empty(),
        "the fake behaves unlike ComfyUI {}:\n{}",
        recorded.version,
        drift.join("\n")
    );
}

/// The probe that records the real ComfyUI, run against the fake: it gets the prompt's
/// websocket messages, the `feature_flags` reply, a 400 for the bad `status`, both cancels of
/// its finished prompt answered `{"cancelled": false}`, and its history; what it records is
/// scrubbed and reads back the same.
#[test] // spec: QA-067, QA-086
fn the_shape_probe_runs_against_the_fake_and_records_scrubbed_files() {
    let fake = FakeComfy::start();
    let probe = capture(&fake.url()).expect("the probe runs against the fake");
    let types: Vec<&str> = probe
        .ws_messages
        .iter()
        .filter_map(|m| m["type"].as_str())
        .collect();
    for kind in [
        "status",
        "feature_flags",
        "execution_start",
        "executed",
        "execution_success",
    ] {
        assert!(types.contains(&kind), "no {kind}: {types:?}");
    }
    assert!(
        probe
            .feature_flags_reply()
            .is_some_and(serde_json::Value::is_object)
    );
    assert_eq!(
        probe.api_jobs["post_prompt"]["status"], 200,
        "{}",
        probe.api_jobs
    );
    assert_eq!(probe.api_jobs["item"]["status"], 200, "{}", probe.api_jobs);
    assert_eq!(
        probe.api_jobs["bad_status"]["status"], 400,
        "{}",
        probe.api_jobs
    );
    for k in ["cancel_one", "cancel_batch"] {
        assert_eq!(probe.api_jobs[k]["status"], 200, "{k}: {}", probe.api_jobs);
        assert_eq!(
            probe.api_jobs[k]["body"]["cancelled"], false,
            "{k}: {}",
            probe.api_jobs
        );
    }
    assert_eq!(
        probe.api_jobs["list"]["jobs"].as_array().map(Vec::len),
        Some(1),
        "only the probe's own row: {}",
        probe.api_jobs
    );
    assert!(
        probe.history.get("<prompt_id>").is_some(),
        "{}",
        probe.history
    );

    let dir = tempfile::tempdir().unwrap();
    let folder = probe.record(dir.path()).unwrap();
    for name in FILES {
        assert!(folder.join(name).is_file(), "{name}");
    }
    let back = Capture::load(&folder).unwrap();
    assert_eq!(back, probe.scrubbed());
    assert!(capture_drift(&back, &probe).is_empty());
    assert!(behaviour_drift(&back, &probe).is_empty());
    let text: String = FILES
        .iter()
        .map(|n| std::fs::read_to_string(folder.join(n)).unwrap())
        .collect();
    assert!(!text.contains("lil_shape_probe"), "file names are scrubbed");
}
