//! The real-engine tests (QA-063, QA-067): `#[ignore]`d, run only by `lil test --real`, which
//! sets `LIL_REAL_COMFYUI` to the URL of the ComfyUI already running (and `LIL_REAL_RECORD` to
//! the fixtures folder with `--record`). They attach to that ComfyUI and never start, stop,
//! free, restart or install anything, and never cancel any prompt but their own. They need no
//! model: every graph is `EmptyImage` into `SaveImage` or `PreviewImage`. Each tiny prompt
//! that saves leaves one small PNG named `lil_*` in ComfyUI's `output\` folder (the suite
//! removes ComfyUI-side copies only when `[engines.comfyui] root` is set, which these tests
//! leave unset, so they never read the models folder).
//!
//! Cores here are test-channel cores on the test's own `TestEnv` (no real suite folder, no
//! real workspace); the only real things are the ComfyUI and, for the telemetry test, the
//! GPU's NVML counters. Durations are failure deadlines only (QA-031).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lil_core::engines::comfyui::{ComfyClient, DEFAULT_PREVIEW_METHOD, RemoteState};
use lil_core::events::EventBus;
use lil_core::telemetry::{HwSources, PowerSettings, Telemetry, TickMode};
use lil_platform::Clock;
use lil_platform::clock::SystemClock;
use lil_testkit::comfy_shapes::{
    Capture, behaviour_drift, capture, capture_drift, newest_recorded, uuid_v4,
};
use lil_testkit::{CoreFixture, FakeComfy, RawPipe, TestEnv, sha256_file};
use serde_json::{Value, json};

/// How long a real prompt may take before a test fails.
const DEADLINE: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_millis(250);

/// `LIL_REAL_COMFYUI`, the ComfyUI these tests attach to.
fn real_url() -> String {
    std::env::var("LIL_REAL_COMFYUI").unwrap_or_else(|_| {
        panic!("LIL_REAL_COMFYUI is not set: run the real tests with `lil test --real`")
    })
}

/// The port of [`real_url`].
fn real_port() -> u16 {
    real_url()
        .rsplit_once(':')
        .and_then(|(_, p)| p.trim_end_matches('/').parse().ok())
        .unwrap_or_else(|| panic!("LIL_REAL_COMFYUI = {} has no port", real_url()))
}

/// The real ComfyUI's behaviours that key sets and JSON types can't show (Task 6's drift
/// list) match the fake's: `/api/jobs` with a bad parameter answers 400, the `feature_flags`
/// reply has the same keys, and both cancels are answered alike. With `LIL_REAL_RECORD` the
/// probe is first recorded (scrubbed) as that version's fixtures; a recording already there
/// is compared with it first, and its shape drift is printed (QA-067's capture-to-capture
/// comparison).
#[test] // spec: QA-063, QA-067
#[ignore = "real engine: the ComfyUI on LIL_REAL_COMFYUI (lil test --real)"]
fn real_comfyui_behaviours_match_the_fake() {
    let real =
        capture(&real_url()).unwrap_or_else(|e| panic!("the probe of the real ComfyUI: {e}"));
    if let Ok(dir) = std::env::var("LIL_REAL_RECORD") {
        let dir = PathBuf::from(dir);
        if let Some((version, folder)) = newest_recorded(&dir) {
            let old =
                Capture::load(&folder).unwrap_or_else(|e| panic!("the {version} recording: {e}"));
            let drift = capture_drift(&old, &real);
            println!(
                "ComfyUI {} against the recorded {version}: {}",
                real.version,
                if drift.is_empty() {
                    "no shape drift".to_string()
                } else {
                    drift.join("\n")
                }
            );
        }
        let folder = real
            .record(&dir)
            .unwrap_or_else(|e| panic!("recording into {}: {e}", dir.display()));
        println!(
            "recorded ComfyUI {} into {}",
            real.version,
            folder.display()
        );
    }
    let fake = FakeComfy::start();
    let probe = capture(&fake.url()).expect("the probe runs against the fake");
    let drift = behaviour_drift(&real, &probe);
    assert!(
        drift.is_empty(),
        "the fake behaves unlike ComfyUI {}:\n{}",
        real.version,
        drift.join("\n")
    );
}

/// The tiny workflow through a test core attached to the real ComfyUI: the job succeeds, its
/// `JobSummary` has the Appendix F shape, its one output is staged in the workspace under the
/// job's prefix with the hash the summary names, and the engine is `attached`.
#[test] // spec: QA-063, CORE-117, CORE-130
#[ignore = "real engine: the ComfyUI on LIL_REAL_COMFYUI (lil test --real)"]
fn real_tiny_workflow_runs_and_matches_the_contract() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("real");
    env.register_workspace(&ws, true);
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\nautostart = false\n",
        real_port()
    ));
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");

    let key = uuid_v4();
    let submitted = pipe.tool_ok("test.comfy_submit", json!({"graph": "tiny"}), Some(&key));
    let id = submitted["job"]["job_id"].as_str().unwrap().to_string();
    let job = wait_done(&mut pipe, &id);
    assert_eq!(job["status"], "succeeded", "{job}");

    // The JobSummary contract (Appendix F "Jobs").
    assert!(
        id.len() == 18 && id.starts_with("j_") && id[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "{id}"
    );
    assert_eq!(job["kind"], "comfy.prompt", "{job}");
    assert_eq!(job["lane"], "gpu-heavy", "{job}");
    assert_eq!(job["priority"], "agent", "a cli caller's job: {job}");
    assert_eq!(job["attempt"], 1, "{job}");
    for t in ["created", "updated", "started", "finished"] {
        let s = job[t].as_str().unwrap_or_else(|| panic!("no {t}: {job}"));
        assert!(s.ends_with('Z') && s.len() >= 20, "{t} = {s}");
    }
    assert!(
        job["trace_id"].as_str().is_some_and(|t| !t.is_empty()),
        "{job}"
    );
    let artifacts = job["result"]["artifacts"]
        .as_array()
        .unwrap_or_else(|| panic!("{job}"));
    assert_eq!(artifacts.len(), 1, "{job}");
    let path = artifacts[0]["path"].as_str().unwrap();
    let name = path.rsplit('/').next().unwrap();
    let prefix = format!("lil_{}_", &id[2..]);
    assert!(
        path.starts_with(".lilith/staging/") && !path.contains('\\') && name.starts_with(&prefix),
        "{path:?} is not .lilith/staging/…/{prefix}…"
    );
    let staged = ws.join(path);
    let bytes = std::fs::read(&staged).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "{path} is not a PNG"
    );
    assert_eq!(
        artifacts[0]["sha256"].as_str(),
        Some(sha256_file(&staged).as_str()),
        "{job}"
    );

    let engines = pipe.tool_ok("engines.status", json!({}), None);
    let engine = &engines["engines"][0];
    assert_eq!(engine["class"], "attached", "{engines}");
    assert_eq!(engine["state"], "ready", "{engines}");
}

/// The suite's cancel, `POST /api/jobs/{id}/cancel` (never `/interrupt`, ARCH-021), on the
/// real ComfyUI, of a prompt of its own that waits behind another of its own: the cancel is
/// answered, and the prompt it named leaves the queue without running (it never reaches
/// `/history`). If ComfyUI ran it before the cancel arrived, the cancel is still answered and
/// the prompt ended on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: QA-063, ARCH-021, CORE-130
#[ignore = "real engine: the ComfyUI on LIL_REAL_COMFYUI (lil test --real)"]
async fn real_cancel_uses_the_per_job_endpoint() {
    let client = ComfyClient::new(&real_url())
        .unwrap()
        .with_client_id(uuid_v4().replace('-', ""));
    let preview = |width: u32, batch: u32| {
        json!({
            "1": {"class_type": "EmptyImage",
                  "inputs": {"width": width, "height": width, "batch_size": batch, "color": 0}},
            "2": {"class_type": "PreviewImage", "inputs": {"images": ["1", 0]}},
        })
    };
    // The first keeps ComfyUI busy a moment (a PreviewImage goes to ComfyUI's temp folder).
    let (ahead, target) = (uuid_v4(), uuid_v4());
    client
        .submit(
            &ahead,
            &preview(1024, 8),
            DEFAULT_PREVIEW_METHOD,
            "t_real_cancel",
        )
        .await
        .unwrap();
    client
        .submit(
            &target,
            &preview(8, 1),
            DEFAULT_PREVIEW_METHOD,
            "t_real_cancel",
        )
        .await
        .unwrap();
    let before = client.state(&target).await.unwrap();
    client
        .cancel(&target)
        .await
        .expect("the per-job cancel is answered");
    let after = until_settled(&client, &target).await;
    if matches!(before, RemoteState::Queued) {
        assert!(
            matches!(after, RemoteState::Gone | RemoteState::Cancelled),
            "a queued prompt cancelled by id left the queue without running: {after:?}"
        );
    } else {
        println!("the prompt was {before:?} when the cancel arrived; it ended {after:?}");
    }
    // The prompt ahead ends on its own.
    let first = until_settled(&client, &ahead).await;
    assert!(matches!(first, RemoteState::Done(_)), "{first:?}");
}

/// The real telemetry sampler on this machine's GPU: snapshots are `measured`, carry the
/// NVML energy counter, and the counter moves between samples (CORE-059, CORE-063).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: QA-063, CORE-059
#[ignore = "real hardware: an NVIDIA GPU and its driver (lil test --real)"]
async fn real_telemetry_snapshot_has_measured_gpu_energy() {
    let env = TestEnv::new();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
    let telemetry = Telemetry::start(
        HwSources::real(),
        env.env.local_dir().join("telemetry.db"),
        clock.clone(),
        PowerSettings::default(),
        Arc::new(EventBus::new(clock)),
        TickMode::Real,
    );
    assert!(telemetry.has_gpu(), "NVML found no GPU");
    let gpu = |n: u64| {
        let telemetry = telemetry.clone();
        async move {
            let snap = tokio::time::timeout(Duration::from_secs(30), telemetry.wait_for_samples(n))
                .await
                .unwrap_or_else(|_| panic!("no {n} samples within 30 s"));
            serde_json::to_value(&snap).unwrap()["gpu"].clone()
        }
    };
    let first = gpu(2).await;
    assert_eq!(first["measured"], true, "{first}");
    let e0 = first["energy_mj"]
        .as_u64()
        .unwrap_or_else(|| panic!("no energy counter: {first}"));
    let later = gpu(telemetry.sample_count() + 3).await;
    let e1 = later["energy_mj"]
        .as_u64()
        .unwrap_or_else(|| panic!("{later}"));
    assert!(e1 > e0, "the energy counter moves: {e0} then {e1}");
    telemetry.shutdown().await;
}

/// `job.wait` (90 s at a time) until the job is done, at most [`DEADLINE`].
fn wait_done(pipe: &mut RawPipe, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let waited = pipe.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 90}), None);
        if waited["done"] == true {
            return waited["job"].clone();
        }
        assert!(
            started.elapsed() < DEADLINE,
            "the job did not end: {waited}"
        );
    }
}

/// Polls `id` until it is neither queued nor running, at most [`DEADLINE`].
async fn until_settled(client: &ComfyClient, id: &str) -> RemoteState {
    let started = Instant::now();
    loop {
        match client.state(id).await.unwrap() {
            RemoteState::Queued | RemoteState::Running => {}
            other => return other,
        }
        assert!(started.elapsed() < DEADLINE, "{id} did not end");
        tokio::time::sleep(POLL).await;
    }
}
