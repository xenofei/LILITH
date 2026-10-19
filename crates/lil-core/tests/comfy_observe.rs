//! ComfyUI observation (ARCH-020, ARCH-057, ARCH-060, ARCH-069, CORE-018, CORE-093, CORE-123,
//! CORE-130): outside jobs that keep the core up, are waited for and never cancelled, polled
//! every 2 s; unattributed usage for paid outside prompts; the environment capture; the model
//! inventory by hash; and the graph checks before the POST (partner nodes, model files).
//!
//! Each test drives the jobs and the engines in process against the in-process `FakeComfy`
//! (attached), with the GPU scheduler as the jobs' admission; nothing real is started
//! (QA-038). The fake answers over loopback sockets, so most tests run in real time; wall-clock
//! durations are failure deadlines and poll pauses only (QA-031). The polling test runs on a
//! paused clock.
#![allow(
    clippy::result_large_err,
    reason = "JobFailure holds the wire ErrorBody, as in lil-core itself"
)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lil_core::engines::comfyui::inventory::{self, ModelFile};
use lil_core::engines::comfyui::{INVENTORY_KIND, environment, prompt_spec};
use lil_core::engines::{EngineState, EngineStatus, Engines};
use lil_core::events::EventBus;
use lil_core::jobs::scheduler::GpuScheduler;
use lil_core::jobs::{
    JobCaller, JobFilter, JobId, JobStatus, JobSummary, Jobs, Priority, SubmittedSlot,
};
use lil_core::lifecycle::IdleBlocker;
use lil_core::meter::{Attribution, UsageCtx, UsageDraft, UsageSink, UsageStatus};
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::telemetry::{HwSources, PowerSettings, Telemetry, TickMode};
use lil_core::workspace::WorkspaceHub;
use lil_platform::fs::{FileIdentity, Probe, SystemProbe, VolumeInfo};
use lil_platform::sockets::OsSockets;
use lil_platform::{Clock, IdSource};
use lil_protocol::CallerKind;
use lil_testkit::{FakeClock, FakeComfy, SeqIds, TestEnv};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// How long a test waits for something before it fails (never a pass condition).
const DEADLINE: Duration = Duration::from_secs(30);
/// Pause between polls of a state that changes on its own.
const POLL: Duration = Duration::from_millis(20);

/// Keeps every usage draft.
#[derive(Default)]
struct Drafts(Mutex<Vec<(UsageDraft, UsageCtx)>>);

impl UsageSink for Drafts {
    fn record<'a>(
        &'a self,
        u: UsageDraft,
        ctx: UsageCtx,
    ) -> futures_util::future::BoxFuture<'a, ()> {
        self.0.lock().unwrap().push((u, ctx));
        Box::pin(std::future::ready(()))
    }
}

impl Drafts {
    fn all(&self) -> Vec<UsageDraft> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(d, _)| d.clone())
            .collect()
    }
}

/// The jobs and the engines of one in-process "core" over `env`, the GPU scheduler their
/// admission (no GPU source: no VRAM check).
struct Rig {
    jobs: Arc<Jobs>,
    engines: Arc<Engines>,
    drafts: Arc<Drafts>,
    _states: Arc<StateRegistry>,
    _telemetry: Arc<Telemetry>,
}

/// `[engines.comfyui]` for the attached `fake`, with its `root` when `root` is true.
fn attach(env: &TestEnv, fake: &FakeComfy, root: bool) {
    let root = if root {
        format!("root = '{}'\n", fake.root().to_str().unwrap())
    } else {
        String::new()
    };
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\n{root}autostart = false\n",
        fake.port()
    ));
}

impl Rig {
    fn new(env: &TestEnv) -> Rig {
        let states = Arc::new(
            StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
        );
        let clock: Arc<dyn Clock> = FakeClock::new();
        let events = Arc::new(EventBus::new(clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let failpoints = FailPoints::none();
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            failpoints.clone(),
            Arc::new(SystemProbe),
            lil_core::watch::WatchConfig::default(),
        );
        let jobs = Jobs::new(
            states.clone(),
            hub,
            events.clone(),
            clock.clone(),
            ids,
            failpoints.clone(),
        );
        let telemetry = Telemetry::start(
            HwSources::none(),
            env.env.local_dir().join("telemetry.db"),
            clock.clone(),
            PowerSettings::default(),
            events.clone(),
            TickMode::Manual,
        );
        let drafts = Arc::new(Drafts::default());
        let engines = Engines::new(
            env.env.clone(),
            jobs.clone(),
            telemetry.clone(),
            events,
            clock,
            failpoints,
            drafts.clone(),
            Arc::new(OsSockets),
        );
        jobs.set_admission(GpuScheduler::new(telemetry.clone(), engines.clone()));
        Rig {
            jobs,
            engines,
            drafts,
            _states: states,
            _telemetry: telemetry,
        }
    }

    fn caller(&self) -> JobCaller {
        JobCaller {
            idem_scope: "cli|none".into(),
            caller: CallerKind::Cli,
            workspace: None,
            trace_id: "t_observe".into(),
            idem_key: None,
            call_id: "c_0000000000000001".into(),
            reservation: None,
            submitted: Arc::new(SubmittedSlot::new()),
            origin: None,
            retry_of: None,
        }
    }

    async fn comfy(&self) -> EngineStatus {
        self.engines.status().await.remove(0)
    }

    /// Waits until the attached engine is ready.
    async fn ready(&self) {
        let deadline = Instant::now() + DEADLINE;
        while self.comfy().await.state != EngineState::Ready {
            assert!(Instant::now() < deadline, "the engine never became ready");
            self.engines.tick().await;
            tokio::time::sleep(POLL).await;
        }
    }

    async fn submit(&self, graph: Value) -> String {
        let spec = prompt_spec(json!({"graph": graph}), Priority::Agent, "test", None, 60);
        self.jobs.submit(&self.caller(), spec).await.unwrap().job_id
    }

    async fn summary(&self, id: &str) -> JobSummary {
        self.jobs
            .get(&self.caller(), &JobId(id.into()))
            .await
            .unwrap()
    }

    async fn done(&self, id: &str) -> JobSummary {
        let (job, done) = self
            .jobs
            .wait(&self.caller(), &JobId(id.into()), DEADLINE)
            .await
            .unwrap();
        assert!(done, "job {id} did not end: {job:?}");
        job
    }

    /// Starts the observation and waits until its first inventory has succeeded.
    async fn observed(&self) -> JobSummary {
        self.engines.observe();
        let deadline = Instant::now() + DEADLINE;
        loop {
            let filter = JobFilter {
                kind: Some(INVENTORY_KIND.to_string()),
                ..JobFilter::default()
            };
            let (jobs, _) = self.jobs.list(&self.caller(), filter).await.unwrap();
            if let Some(job) = jobs.into_iter().find(|j| j.status.is_final()) {
                assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
                return job;
            }
            assert!(
                Instant::now() < deadline,
                "no inventory within {DEADLINE:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }
}

fn bare(path: &str) -> &str {
    path.trim_start_matches("/api")
}

/// The bodies of the `POST /prompt`s the fake saw.
fn posted(fake: &FakeComfy) -> Vec<Value> {
    fake.requests()
        .into_iter()
        .filter(|r| r.method == "POST" && bare(&r.path) == "/prompt")
        .map(|r| r.body.unwrap_or(Value::Null))
        .collect()
}

/// A one-`SaveImage` graph with the extra nodes `more`.
fn graph_with(more: Value) -> Value {
    let mut graph = json!({
        "1": {"class_type": "EmptyImage",
              "inputs": {"width": 64, "height": 64, "batch_size": 1, "color": 0}},
        "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "x"}},
    });
    for (k, v) in more.as_object().unwrap() {
        graph[k] = v.clone();
    }
    graph
}

/// The fake's default object info plus `extra`.
fn object_info(extra: Value) -> Value {
    let mut info = json!({
        "EmptyImage": {"api_node": false},
        "SaveImage": {"api_node": false},
        "KSampler": {"api_node": false},
        "CheckpointLoaderSimple": {"api_node": false},
    });
    for (k, v) in extra.as_object().unwrap() {
        info[k] = v.clone();
    }
    info
}

// ---------------------------------------------------------------------------------------------
// Outside jobs (ARCH-020, ARCH-069).
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-020, ARCH-069
async fn outside_jobs_block_idle_and_are_never_cancelled() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    let outside = fake.add_outside_prompt(true, None);
    let rig = Rig::new(&env);
    rig.ready().await;
    let watch = rig.engines.outside_watch();
    assert!(!watch.busy(), "nothing polled yet");

    rig.engines.poll_outside().await;
    assert_eq!((watch.running(), watch.queued()), (1, 0));
    assert!(watch.busy(), "an outside job keeps the core up");
    let status = rig.comfy().await;
    assert_eq!(
        (status.outside_jobs.running, status.outside_jobs.queued),
        (1, 0)
    );

    // Neither the drain's step nor the shutdown's prompt cancel touches it.
    rig.jobs.persist_interrupted_step().run().await;
    rig.engines.cancel_prompts_step().run().await;
    let touched: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| {
            r.path.ends_with("/interrupt")
                || (r.method == "POST"
                    && (r.path.contains(&outside)
                        || r.body
                            .as_ref()
                            .is_some_and(|b| b.to_string().contains(&outside))))
        })
        .collect();
    assert!(
        touched.is_empty(),
        "the outside job was touched: {touched:?}"
    );
    assert!(watch.busy());
    // A drain is not held by it: an outside job is not the suite's work to wait for.
    let drain = CancellationToken::new();
    watch.stop_on_drain(drain.clone());
    drain.cancel();
    assert!(!watch.busy(), "an outside job holds no drain");
    assert_eq!(watch.running(), 1);

    fake.release(&outside);
    let deadline = Instant::now() + DEADLINE;
    loop {
        rig.engines.poll_outside().await;
        if watch.running() == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "the outside job never ended");
        tokio::time::sleep(POLL).await;
    }
    assert!(!watch.any(), "freed on the next poll after it ended");
}

/// Drives the paused runtime until `pred` holds (a task that never stops yielding keeps the
/// clock from advancing on its own while real sockets are awaited).
async fn until(what: &str, pred: impl Fn() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !pred() {
        assert!(Instant::now() < deadline, "{what}: not within {DEADLINE:?}");
        tokio::task::yield_now().await;
    }
}

/// Lets the paused runtime read the answers in flight (a pause of 200 ms of wall time, with the
/// clock standing still).
async fn settle() {
    let until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < until {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-069
async fn outside_jobs_are_polled_every_2_seconds() {
    // Never idle, so the paused clock moves only by `advance`.
    tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    let rig = Rig::new(&env);
    // Ready at the supervisor's first look, with no time passed.
    let deadline = Instant::now() + DEADLINE;
    while rig.comfy().await.state != EngineState::Ready {
        assert!(Instant::now() < deadline, "the engine never became ready");
        tokio::task::yield_now().await;
    }
    let start = tokio::time::Instant::now();
    let polls = |path: &'static str| {
        let fake = &fake;
        move || {
            fake.requests()
                .iter()
                .filter(|r| r.method == "GET" && r.path == path)
                .count()
        }
    };
    let (queue, jobs) = (polls("/api/queue"), polls("/api/jobs"));
    rig.engines.observe();
    until("the first poll", || queue() == 1 && jobs() == 1).await;
    settle().await;
    for step in 1..=9u32 {
        tokio::time::advance(Duration::from_millis(500)).await;
        let want = 1 + (step / 4) as usize;
        until(&format!("the polls at {} ms", step * 500), || {
            queue() == want && jobs() == want
        })
        .await;
        // The poll's answers are read before the clock moves on.
        settle().await;
    }
    for _ in 0..1000 {
        tokio::task::yield_now().await;
    }
    assert_eq!(start.elapsed(), Duration::from_millis(4500));
    assert_eq!(
        (queue(), jobs()),
        (3, 3),
        "4.5 s after ready: polls at 0, 2 and 4 s"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-069, CORE-123
async fn suite_jobs_wait_for_outside_prompts() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    let outside = fake.add_outside_prompt(true, None);
    let rig = Rig::new(&env);
    rig.ready().await;
    rig.engines.poll_outside().await;

    let id = rig.submit(graph_with(json!({}))).await;
    for _ in 0..5 {
        rig.engines.poll_outside().await;
        let job = rig.summary(&id).await;
        assert_eq!(job.status, JobStatus::Queued, "{job:?}");
        assert_eq!(job.substatus.as_deref(), Some("waiting"), "{job:?}");
        tokio::time::sleep(POLL).await;
    }
    assert!(posted(&fake).is_empty(), "the suite job did not wait");

    fake.release(&outside);
    let deadline = Instant::now() + DEADLINE;
    while posted(&fake).is_empty() {
        assert!(Instant::now() < deadline, "the suite job was never posted");
        rig.engines.poll_outside().await;
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(rig.done(&id).await.status, JobStatus::Succeeded);
    assert_eq!(posted(&fake).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-057, CORE-093
async fn outside_paid_prompt_records_unattributed_usage() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    fake.set_object_info(object_info(json!({"OpenAIImageNode": {"api_node": true}})));
    let graph = json!({"1": {"class_type": "OpenAIImageNode", "inputs": {"prompt": "x"}}});
    let outside = fake.add_outside_prompt(true, Some(graph));
    let rig = Rig::new(&env);
    rig.ready().await;
    rig.engines.poll_outside().await;
    assert!(rig.drafts.all().is_empty(), "recorded once its end is seen");

    fake.release(&outside);
    let deadline = Instant::now() + DEADLINE;
    while rig.drafts.all().is_empty() {
        assert!(Instant::now() < deadline, "no usage draft");
        rig.engines.poll_outside().await;
        tokio::time::sleep(POLL).await;
    }
    for _ in 0..3 {
        rig.engines.poll_outside().await;
    }
    let drafts = rig.drafts.all();
    assert_eq!(
        drafts.len(),
        1,
        "the same prompt adds no second draft: {drafts:?}"
    );
    let d = &drafts[0];
    assert_eq!(d.route, "comfy-partner");
    assert_eq!(d.attribution, Attribution::Unattributed);
    assert_eq!(d.est_cost, None);
    assert_eq!(d.status, UsageStatus::Succeeded, "from what was observed");
    assert!(d.latency_ms.is_some(), "{d:?}");
    assert_eq!(
        d.raw_usage.as_ref().unwrap()["partner_nodes"],
        json!(["OpenAIImageNode"])
    );
}

// ---------------------------------------------------------------------------------------------
// The environment capture and the model inventory (CORE-130, ARCH-060).
// ---------------------------------------------------------------------------------------------

#[test] // spec: CORE-130
fn environment_capture_has_versions_and_node_revisions() {
    let env = TestEnv::new();
    let root = env.root().join("comfy");
    let repo = root.join("custom_nodes").join("pack-git");
    let versioned = root.join("custom_nodes").join("pack-pyproject");
    lil_testkit::files::write_file(&repo.join("__init__.py"), b"");
    env.git_init(&repo);
    let commit = env.git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    lil_testkit::files::write_file(
        &versioned.join("pyproject.toml"),
        b"[project]\nname = \"pack\"\nversion = \"1.2.3\"\n",
    );
    let stats = json!({"system": {"comfyui_version": "0.42.1"}});
    let captured = environment::capture(
        stats.clone(),
        Some("595.10".into()),
        Some(&root),
        "2026-10-19T00:00:00Z".into(),
    );
    assert_eq!(captured.system_stats, stats);
    assert_eq!(captured.driver.as_deref(), Some("595.10"));
    assert_eq!(
        captured.nodes,
        [
            ("pack-git".to_string(), Some(commit), None),
            (
                "pack-pyproject".to_string(),
                None,
                Some("1.2.3".to_string())
            ),
        ]
    );
}

/// The real probe, counting the files it opens to read.
#[derive(Debug, Default)]
struct CountingProbe(AtomicUsize);

impl Probe for CountingProbe {
    fn identity(&self, path: &Path) -> std::io::Result<FileIdentity> {
        SystemProbe.identity(path)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        self.0.fetch_add(1, Ordering::SeqCst);
        SystemProbe.open_read(path)
    }
}

/// Sets `path`'s modification time one hour later.
#[allow(clippy::disallowed_methods)]
fn touch_later(path: &Path) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    let mtime = file.metadata().unwrap().modified().unwrap();
    file.set_modified(mtime + Duration::from_secs(3600))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-130
async fn model_hashes_are_cached_by_path_size_mtime_and_file_id() {
    let env = TestEnv::new();
    let states =
        StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap();
    let models = env.root().join("comfy").join("models");
    lil_testkit::files::write_file(&models.join(r"checkpoints\a.safetensors"), b"aaaa");
    lil_testkit::files::write_file(&models.join(r"loras\b.gguf"), b"bbbb");
    lil_testkit::files::write_file(&models.join(r"loras\notes.txt"), b"not a model");
    let probe = Arc::new(CountingProbe::default());
    let scan = |p: &Arc<CountingProbe>| {
        let p: Arc<dyn Probe> = p.clone();
        inventory::scan(&models, p, states.suite_db(), || false)
    };

    let first = scan(&probe).await.unwrap();
    assert_eq!(
        probe.0.load(Ordering::SeqCst),
        2,
        "each model file read once"
    );
    let paths: Vec<&str> = first.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, [r"checkpoints\a.safetensors", r"loras\b.gguf"]);
    assert_eq!(
        first[0].sha256.as_deref(),
        Some(lil_testkit::files::sha256_bytes(b"aaaa").as_str())
    );

    let counting = Arc::new(CountingProbe::default());
    let second = scan(&counting).await.unwrap();
    assert_eq!(
        counting.0.load(Ordering::SeqCst),
        0,
        "nothing changed: no byte read"
    );
    assert_eq!(second, first);

    touch_later(&models.join(r"loras\b.gguf"));
    let counting = Arc::new(CountingProbe::default());
    let third = scan(&counting).await.unwrap();
    assert_eq!(
        counting.0.load(Ordering::SeqCst),
        1,
        "only the touched file"
    );
    assert_eq!(third, first);
}

/// A default workspace whose `registry/models.yaml` names `sha256` as `zimage-turbo`.
fn registry_naming(env: &TestEnv, sha256: &str) {
    let ws = env.make_workspace("models");
    lil_testkit::files::write_file(
        &ws.join(r"registry\models.yaml"),
        format!(
            "- id: zimage-turbo\n  name: Z-Image Turbo\n  files:\n    - {{path: ComfyUI/models/diffusion_models/z.safetensors, sha256: \"{sha256}\"}}\n- id: no-name\n  files:\n    - {{path: n, sha256: \"{}\"}}\n",
            "0".repeat(64)
        )
        .as_bytes(),
    );
    env.register_workspace(&ws, true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-060, CORE-130
async fn inventory_names_are_trusted_only_when_matched_by_hash() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    let models = fake.root().join("models");
    lil_testkit::files::write_file(&models.join(r"diffusion_models\z.safetensors"), b"zzzz");
    lil_testkit::files::write_file(&models.join("x.safetensors"), b"xxxx");
    let known = lil_testkit::files::sha256_bytes(b"zzzz");
    registry_naming(&env, &format!("sha256:{known}"));
    let rig = Rig::new(&env);
    rig.ready().await;

    let job = rig.observed().await;
    let result = job.result.expect("the inventory's result");
    let models = result.structured["models"].as_array().unwrap().clone();
    let trusted = models
        .iter()
        .find(|m| m.get("model_id").is_some())
        .expect("the matched file");
    assert_eq!(
        trusted,
        &json!({"model_id": "zimage-turbo", "model_name": "Z-Image Turbo", "size": 4,
                "sha256": known})
    );
    let other = models
        .iter()
        .find(|m| m.get("model_id").is_none())
        .expect("the other file");
    assert_eq!(
        other,
        &json!({"name": "x.safetensors", "size": 4, "untrusted": true,
                "sha256": lil_testkit::files::sha256_bytes(b"xxxx")})
    );
    assert_eq!(models.len(), 2);
    assert!(result.structured["environment"]["system_stats"].is_object());
    let status = rig.comfy().await;
    assert_eq!(status.inventory.map(|i| i.models), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-130
async fn inventory_matches_registry_by_sha256() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    lil_testkit::files::write_file(&fake.root().join(r"models\loras\y.safetensors"), b"yy");
    registry_naming(
        &env,
        &lil_testkit::files::sha256_bytes(b"yy").to_ascii_uppercase(),
    );
    let rig = Rig::new(&env);
    rig.ready().await;
    let job = rig.observed().await;
    let models = &job.result.unwrap().structured["models"];
    assert_eq!(models[0]["model_id"], "zimage-turbo", "{models}");
    // An entry with no name is named by its id.
    let files = vec![ModelFile {
        path: "n.safetensors".into(),
        size: 1,
        sha256: Some("0".repeat(64)),
    }];
    let registry = lil_schema::engine_files::read_models_yaml(&format!(
        "- id: no-name\n  files:\n    - {{sha256: \"{}\"}}\n",
        "0".repeat(64)
    ))
    .unwrap();
    assert_eq!(
        inventory::model_entries(&files, &registry)[0]["model_name"],
        "no-name"
    );
}

// ---------------------------------------------------------------------------------------------
// Graph checks before the POST (CORE-018, ARCH-057).
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-057, CORE-018
async fn partner_nodes_refusal_names_the_classes() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    fake.set_object_info(object_info(json!({
        "OpenAIImageNode": {"api_node": true},
        "KlingNode": {"api_node": true},
    })));
    let rig = Rig::new(&env);
    rig.ready().await;
    let id = rig
        .submit(graph_with(json!({
            "3": {"class_type": "OpenAIImageNode", "inputs": {}},
            "4": {"class_type": "KlingNode", "inputs": {}},
            "5": {"class_type": "KSampler", "inputs": {}},
        })))
        .await;
    let done = rig.done(&id).await;
    assert_eq!(done.status, JobStatus::Failed, "{done:?}");
    let error = done.error.unwrap();
    assert_eq!(error.code, "comfyui.partner_nodes");
    let detail = error.detail.unwrap();
    assert!(detail.untrusted);
    assert_eq!(detail.text, "KlingNode, OpenAIImageNode");
    assert!(posted(&fake).is_empty(), "no POST /prompt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-057, CORE-093
async fn suite_jobs_with_partner_nodes_are_refused_until_plan_6b() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    fake.set_object_info(object_info(json!({"OpenAIImageNode": {"api_node": true}})));
    let rig = Rig::new(&env);
    rig.ready().await;
    // Even a job marked as paid work (`external_side_effects`) is refused in plan 3.
    let mut spec = prompt_spec(
        json!({"graph": graph_with(json!({"3": {"class_type": "OpenAIImageNode", "inputs": {}}}))}),
        Priority::Agent,
        "test",
        None,
        60,
    );
    spec.external_side_effects = true;
    let id = rig.jobs.submit(&rig.caller(), spec).await.unwrap().job_id;
    let done = rig.done(&id).await;
    assert_eq!(done.error.unwrap().code, "comfyui.partner_nodes");
    assert!(posted(&fake).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-018
async fn missing_model_files_fail_before_the_post() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    lil_testkit::files::write_file(
        &fake.root().join(r"models\checkpoints\sub\a.safetensors"),
        b"a",
    );
    let rig = Rig::new(&env);
    rig.ready().await;
    rig.observed().await;

    let id = rig
        .submit(graph_with(json!({
            "3": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sub/a.safetensors"}},
            "4": {"class_type": "LoraLoader", "inputs": {"lora_name": "gone.safetensors"}},
        })))
        .await;
    let done = rig.done(&id).await;
    assert_eq!(done.status, JobStatus::Failed, "{done:?}");
    let error = done.error.unwrap();
    assert_eq!(error.code, "comfyui.execution_failed");
    let detail = error.detail.clone().unwrap();
    assert!(detail.untrusted);
    assert_eq!(detail.text, "gone.safetensors");
    let hint = error.fix.unwrap().agent_hint.unwrap();
    assert_eq!(
        (hint.tool.as_str(), hint.arguments),
        ("engines.status", json!({}))
    );
    assert!(posted(&fake).is_empty(), "no POST /prompt");

    let id = rig
        .submit(graph_with(json!({
            "3": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sub/a.safetensors"}},
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "A.SAFETENSORS"}},
        })))
        .await;
    assert_eq!(rig.done(&id).await.status, JobStatus::Succeeded);
    assert_eq!(posted(&fake).len(), 1);
}

/// A graph naming a model file nobody has.
fn gone_graph() -> Value {
    graph_with(json!({
        "3": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "gone.safetensors"}},
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-018
async fn model_check_is_skipped_without_a_refreshed_inventory_or_with_extra_model_paths() {
    // No inventory refreshed yet.
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    let rig = Rig::new(&env);
    rig.ready().await;
    let id = rig.submit(gone_graph()).await;
    assert_eq!(rig.done(&id).await.status, JobStatus::Succeeded);
    assert_eq!(posted(&fake).len(), 1, "no inventory: posted");
    drop(rig);

    // `root` unknown.
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, false);
    let rig = Rig::new(&env);
    rig.ready().await;
    rig.observed().await;
    let id = rig.submit(gone_graph()).await;
    assert_eq!(rig.done(&id).await.status, JobStatus::Succeeded);
    assert_eq!(posted(&fake).len(), 1, "no root: posted");
    drop(rig);

    // `<root>\extra_model_paths.yaml`: models may live elsewhere.
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    lil_testkit::files::write_file(&fake.root().join("extra_model_paths.yaml"), b"a: {}\n");
    let rig = Rig::new(&env);
    rig.ready().await;
    rig.observed().await;
    let id = rig.submit(gone_graph()).await;
    assert_eq!(rig.done(&id).await.status, JobStatus::Succeeded);
    assert_eq!(posted(&fake).len(), 1, "extra model paths: posted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-018
async fn network_nodes_are_not_refused() {
    // Owner decision, 2026-10-18: a node that reaches the network but isn't a partner node runs.
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake, true);
    fake.set_object_info(object_info(
        json!({"CivitaiDiscoveryHubNode": {"api_node": false}}),
    ));
    let rig = Rig::new(&env);
    rig.ready().await;
    let id = rig
        .submit(graph_with(json!({
            "3": {"class_type": "CivitaiDiscoveryHubNode", "inputs": {"query": "x"}},
        })))
        .await;
    let done = rig.done(&id).await;
    assert_eq!(done.status, JobStatus::Succeeded, "{done:?}");
    assert_eq!(posted(&fake).len(), 1);
}
