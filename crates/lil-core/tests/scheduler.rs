//! The GPU scheduler (ARCH-071, ARCH-072, CORE-061, CORE-126): the free-VRAM formula, the wait
//! for VRAM and its re-check on every telemetry sample, an engine's own memory, the note
//! without NVML, the unload on a lease switch, the busy flag and the watchdog.
//!
//! The telemetry is the manual sampler over a fake hardware script, kicking the jobs on every
//! sample as the core does; the jobs live in the suite `state.db`. The tests run in real time
//! with no timers of their own beyond failure deadlines (QA-031); the watchdog's time is the
//! fake clock's.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use lil_core::events::EventBus;
use lil_core::jobs::estimate::Estimator;
use lil_core::jobs::record;
use lil_core::jobs::scheduler::{
    EngineControl, GpuScheduler, NoEngines, free_vram_mib, watchdog_limit, watchdog_slow,
};
use lil_core::jobs::{
    Admission, Admit, EngineKind, JobCaller, JobCtx, JobDone, JobFailure, JobFuture, JobHandler,
    JobId, JobSpec, JobStatus, JobSummary, Jobs, Lane, Priority, SubmittedSlot, Substatus,
    cancelled_error,
};
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::telemetry::{HwSources, JobStatsRow, PowerSettings, Telemetry, TickMode, VramView};
use lil_core::workspace::WorkspaceHub;
use lil_platform::fs::SystemProbe;
use lil_platform::{Clock, IdSource};
use lil_protocol::estimate::{EstimateSource, WallPart};
use lil_protocol::{CallerKind, ErrorBody, codes};
use lil_schema::Currency;
use lil_testkit::{FakeClock, FakeHwScript, SeqIds, TestEnv};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

/// How long a test waits for something before it fails (never a pass condition).
const DEADLINE: Duration = Duration::from_secs(30);
const TOTAL: u64 = 16384;

// ---------------------------------------------------------------------------------------------
// The fixture.
// ---------------------------------------------------------------------------------------------

/// Runs `t.gpu` jobs: with `{"gate": g}` it waits until [`Gates::open`]`(g)` (or a cancel);
/// `"gate_1"` is attempt 1's gate instead, and with `"fail_first": true` attempt 1 then fails
/// transiently.
#[derive(Default)]
struct Gates {
    open: watch::Sender<HashSet<String>>,
    started: watch::Sender<Vec<String>>,
}

impl Gates {
    fn open(&self, gate: &str) {
        self.open.send_modify(|s| {
            s.insert(gate.to_string());
        });
    }

    async fn until_started(&self, job: &str) {
        let mut rx = self.started.subscribe();
        tokio::time::timeout(DEADLINE, rx.wait_for(|s| s.iter().any(|j| j == job)))
            .await
            .expect("the job never started")
            .unwrap();
    }
}

impl JobHandler for Gates {
    fn kind(&self) -> &'static str {
        "t.gpu"
    }

    /// Its `key` argument.
    fn estimate_key(&self, args: &Value) -> Option<String> {
        args.get("key").and_then(Value::as_str).map(str::to_string)
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(async move {
            self.started.send_modify(|s| s.push(job.id.to_string()));
            let args = &job.spec.args;
            let gate = match args.get("gate_1") {
                Some(g) if job.attempt == 1 => g.as_str(),
                _ => args.get("gate").and_then(Value::as_str),
            };
            if let Some(gate) = gate {
                let mut open = self.open.subscribe();
                let mut cancel = job.cancel.clone();
                tokio::select! {
                    _ = open.wait_for(|s| s.contains(gate)) => {}
                    _ = cancel.wait_for(|c| *c) => {
                        return Err(JobFailure::Final(cancelled_error(&job.trace_id)));
                    }
                }
            }
            if job.attempt == 1 && args.get("fail_first") == Some(&json!(true)) {
                return Err(JobFailure::Retryable(ErrorBody::new(
                    &codes::STORE_IO,
                    "on purpose",
                    job.trace_id.clone(),
                )));
            }
            Ok(JobDone {
                structured: json!({}),
                artifacts: Vec::new(),
                record: None,
                windows: Vec::new(),
            })
        })
    }

    fn cancel_in_engine<'a>(&'a self, _job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// Engines with fixed processes that record every unload.
#[derive(Default)]
struct Engines {
    pids: HashMap<EngineKind, Vec<u32>>,
    unloads: watch::Sender<Vec<EngineKind>>,
    /// An unload that never finishes.
    stuck: bool,
    /// Unloads finish only once this is `true`.
    gate: Option<watch::Sender<bool>>,
    /// How many unloads finished.
    finished: watch::Sender<u32>,
}

impl EngineControl for Engines {
    fn pids(&self, e: EngineKind) -> Vec<u32> {
        self.pids.get(&e).cloned().unwrap_or_default()
    }

    fn unload<'a>(&'a self, e: EngineKind) -> BoxFuture<'a, ()> {
        self.unloads.send_modify(|u| u.push(e));
        if self.stuck {
            return Box::pin(std::future::pending());
        }
        Box::pin(async move {
            if let Some(gate) = &self.gate {
                let _ = gate.subscribe().wait_for(|open| *open).await;
            }
            self.finished.send_modify(|n| *n += 1);
        })
    }

    fn outside_busy(&self, _e: EngineKind) -> bool {
        false
    }
}

type ReadEvent = (String, Value);

/// One answer of the jobs' admission: the job's need, the telemetry sample it was asked at,
/// and the answer.
type Asked = (Option<u64>, u64, Admit);

/// The scheduler as the jobs' admission, recording each answer it gives them.
struct Recording {
    scheduler: Arc<GpuScheduler>,
    telemetry: Arc<Telemetry>,
    asked: std::sync::Mutex<Vec<Asked>>,
}

impl Admission for Recording {
    fn admit(&self, spec: &JobSpec) -> Admit {
        let answer = self.scheduler.admit(spec);
        self.asked.lock().unwrap().push((
            spec.vram_need_mib,
            self.telemetry.sample_count(),
            answer,
        ));
        answer
    }

    fn busy(&self, busy: bool) {
        self.scheduler.busy(busy);
    }
}

struct Rig {
    jobs: Arc<Jobs>,
    telemetry: Arc<Telemetry>,
    scheduler: Arc<GpuScheduler>,
    recording: Arc<Recording>,
    gates: Arc<Gates>,
    events: Arc<EventBus>,
    clock: Arc<FakeClock>,
    states: Arc<StateRegistry>,
    hub: Arc<WorkspaceHub>,
    rx: tokio::sync::Mutex<(mpsc::Receiver<String>, Vec<ReadEvent>)>,
    /// Last: the folder goes only once nothing holds a file in it.
    env: TestEnv,
}

impl Rig {
    /// Jobs whose admission is a [`GpuScheduler`] over the fake hardware `hw` (`None`: no
    /// hardware at all) and `engines`, kicked on every sample as the core does.
    async fn new(hw: Option<Value>, engines: Arc<dyn EngineControl>) -> Rig {
        let env = TestEnv::new();
        let states = Arc::new(
            StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
        );
        let clock = FakeClock::new();
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let events = Arc::new(EventBus::new(dyn_clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            FailPoints::none(),
            Arc::new(SystemProbe),
            lil_core::watch::WatchConfig::default(),
        );
        let jobs = Jobs::new(
            states.clone(),
            hub.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids,
            FailPoints::none(),
        );
        let gates = Arc::new(Gates::default());
        jobs.register(gates.clone());
        let sources = match hw {
            Some(script) => HwSources::from_fake(FakeHwScript::new(script).sources()),
            None => HwSources::none(),
        };
        let telemetry = Telemetry::start(
            sources,
            env.env.local_dir().join("telemetry.db"),
            dyn_clock,
            PowerSettings::default(),
            events.clone(),
            TickMode::Manual,
        );
        let scheduler = GpuScheduler::new(telemetry.clone(), engines);
        let recording = Arc::new(Recording {
            scheduler: scheduler.clone(),
            telemetry: telemetry.clone(),
            asked: std::sync::Mutex::default(),
        });
        jobs.set_admission(recording.clone());
        let kicked = Arc::downgrade(&jobs);
        telemetry.on_sample(Box::new(move || {
            if let Some(jobs) = kicked.upgrade() {
                jobs.kick();
            }
        }));
        let (tx, rx) = mpsc::channel(8192);
        events.attach(1, tx);
        events
            .subscribe(1, &["jobs".to_string()], &BTreeMap::new())
            .await;
        Rig {
            jobs,
            telemetry,
            scheduler,
            recording,
            gates,
            events,
            clock,
            states,
            hub,
            rx: tokio::sync::Mutex::new((rx, Vec::new())),
            env,
        }
    }

    fn caller(&self) -> JobCaller {
        JobCaller {
            caller: CallerKind::Cli,
            idem_scope: "cli|none".into(),
            workspace: None,
            trace_id: "t_sched".into(),
            idem_key: None,
            call_id: "c_0000000000000001".into(),
            reservation: None,
            submitted: Arc::new(SubmittedSlot::new()),
            origin: None,
            retry_of: None,
        }
    }

    async fn submit(&self, spec: JobSpec) -> JobSummary {
        self.jobs.submit(&self.caller(), spec).await.unwrap()
    }

    async fn subscribe(&self, topic: String) {
        self.events.subscribe(1, &[topic], &BTreeMap::new()).await;
    }

    /// The first event (read before or now) of `topic` whose data satisfies `f`.
    async fn event(&self, topic: &str, f: impl Fn(&Value) -> bool) -> Value {
        let mut guard = self.rx.lock().await;
        let (rx, seen) = &mut *guard;
        if let Some((_, data)) = seen.iter().find(|(t, d)| t == topic && f(d)) {
            return data.clone();
        }
        let read = async {
            while let Some(line) = rx.recv().await {
                let event: Value = serde_json::from_str(&line).unwrap();
                let (t, data) = (
                    event["params"]["topic"].as_str().unwrap().to_string(),
                    event["params"]["data"].clone(),
                );
                seen.push((t.clone(), data.clone()));
                if t == topic && f(&data) {
                    return data;
                }
            }
            panic!("the events ended");
        };
        tokio::time::timeout(DEADLINE, read)
            .await
            .unwrap_or_else(|_| panic!("no such event on {topic}"))
    }

    /// The job's summary once it is in `substatus` (a `jobs` event).
    async fn in_substatus(&self, id: &str, substatus: &str) -> Value {
        self.event("jobs", |d| {
            d["job"]["job_id"] == id && d["job"]["substatus"] == substatus
        })
        .await["job"]
            .clone()
    }

    /// The job's summary (JSON, as the wire has it) once it is final.
    async fn finished(&self, id: &str) -> Value {
        self.event("jobs", |d| {
            d["job"]["job_id"] == id
                && matches!(
                    d["job"]["status"].as_str(),
                    Some("succeeded" | "failed" | "cancelled")
                )
        })
        .await["job"]
            .clone()
    }

    /// Waits until no job is queued, running or in its completions.
    async fn idle(&self) {
        tokio::time::timeout(DEADLINE, async {
            while self.jobs.busy() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the jobs never went idle");
    }

    async fn shutdown(self) {
        let Rig {
            jobs,
            telemetry,
            scheduler,
            hub,
            states,
            env,
            ..
        } = self;
        drop((jobs, scheduler));
        telemetry.shutdown().await;
        drop(telemetry);
        hub.close_all().await;
        tokio::task::spawn_blocking(move || states.close())
            .await
            .unwrap();
        drop(hub);
        drop(env);
    }
}

/// A fake GPU reading of a 16384 MiB card that NVML says has `nvml_free` MiB free.
fn gpu(nvml_free: u64) -> Value {
    json!({"name": "Fake GPU", "vram_total_mib": TOTAL, "vram_used_mib": TOTAL - nvml_free,
           "util_pct": 0, "power_mw": 20000})
}

/// One per-process GPU memory reading: `(pid, Local Usage)`.
fn mem(local: &[(u32, u64)]) -> Value {
    local
        .iter()
        .map(|(pid, mib)| json!({"pid": pid, "local_mib": mib}))
        .collect()
}

/// A script with these GPU and per-process readings, one per sample.
fn script(gpus: Vec<Value>, mems: Vec<Value>) -> Value {
    json!({"gpu": gpus, "gpu_mem": mems, "cpu_energy": null, "sys": [{"cpu_permille": 100}]})
}

/// A fake GPU with plenty free.
fn roomy() -> Value {
    script(vec![gpu(15000)], vec![mem(&[])])
}

fn spec(lane: Lane, args: Value) -> JobSpec {
    JobSpec {
        kind: "t.gpu",
        lane,
        priority: Priority::Agent,
        module: "test".to_string(),
        args,
        project: None,
        asset_id: None,
        doc_id: None,
        expected_secs: 10,
        expected_output_bytes: 0,
        vram_need_mib: None,
        engine: None,
        group: None,
        external_side_effects: false,
    }
}

fn heavy(need: u64, engine: Option<EngineKind>) -> JobSpec {
    JobSpec {
        vram_need_mib: Some(need),
        engine,
        ..spec(Lane::GpuHeavy, json!({}))
    }
}

fn view(nvml_free: u64, local: &[(u32, u64)]) -> VramView {
    VramView {
        total_mib: TOTAL,
        nvml_free_mib: nvml_free,
        local_by_pid: Some(local.iter().copied().collect()),
    }
}

// ---------------------------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-071
fn free_vram_worked_examples() {
    // min(16384 − 2000, 10000 + 4000) − 512
    assert_eq!(
        free_vram_mib(&view(10000, &[(100, 2000), (200, 4000)]), &[200]),
        13488
    );
    // min(16384 − 15000, 1000) − 512
    assert_eq!(free_vram_mib(&view(1000, &[(100, 15000)]), &[]), 488);
    // Others past the card: floored at 0.
    assert_eq!(free_vram_mib(&view(0, &[(100, 16200)]), &[]), 0);
    // An engine of two processes: min(16384 − 1000, 3000 + 3000 + 9000) − 512
    assert_eq!(
        free_vram_mib(
            &view(3000, &[(100, 1000), (200, 3000), (201, 9000)]),
            &[200, 201]
        ),
        14488
    );
    // Saturating: readings that would overflow never wrap.
    assert_eq!(
        free_vram_mib(&view(u64::MAX, &[(1, u64::MAX), (2, u64::MAX)]), &[1]),
        0
    );
}

#[tokio::test] // spec: ARCH-071, CORE-126
async fn waits_for_vram_then_runs_on_the_next_sample() {
    let rig = Rig::new(
        Some(script(
            vec![gpu(1000), gpu(14000)],
            vec![mem(&[(100, 15000)]), mem(&[(100, 2000)])],
        )),
        Arc::new(NoEngines),
    )
    .await;
    rig.telemetry.tick(1).await;
    assert_eq!(
        rig.scheduler.admit(&heavy(4000, None)),
        Admit::WaitForVram {
            need_mib: 4000,
            free_mib: 488
        }
    );
    let job = rig.submit(heavy(4000, None)).await;
    let waiting = rig.in_substatus(&job.job_id, "waiting_for_vram").await;
    assert_eq!(waiting["status"], "queued", "{waiting}");
    assert!(
        rig.telemetry.busy(),
        "a job waiting for VRAM keeps per-process memory sampled every second"
    );
    // The next sample reads the other process at 2000 MiB, and its hook asks again.
    rig.telemetry.tick(1).await;
    assert_eq!(rig.scheduler.admit(&heavy(4000, None)), Admit::Go);
    let done = rig.finished(&job.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(done.get("vram_unchecked").is_none(), "{done}");
    rig.shutdown().await;
}

#[tokio::test] // spec: ARCH-071, CORE-126
async fn engine_own_local_usage_counts_as_free() {
    let engines = Engines {
        pids: HashMap::from([(EngineKind::ComfyUi, vec![200, 201])]),
        ..Engines::default()
    };
    let rig = Rig::new(
        Some(script(
            vec![gpu(3000)],
            vec![mem(&[(100, 1000), (200, 3000), (201, 9000)])],
        )),
        Arc::new(engines),
    )
    .await;
    rig.telemetry.tick(1).await;
    // min(15384, 3000 + 12000) − 512 = 14488 for the engine's job.
    assert_eq!(
        rig.scheduler
            .admit(&heavy(14000, Some(EngineKind::ComfyUi))),
        Admit::Go
    );
    // The engine's 12000 MiB are another's to a job of no engine: min(3384, 3000) − 512.
    assert_eq!(
        rig.scheduler.admit(&heavy(14000, None)),
        Admit::WaitForVram {
            need_mib: 14000,
            free_mib: 2488
        }
    );
    rig.shutdown().await;
}

#[tokio::test] // spec: ARCH-071
async fn dedicated_usage_is_never_used_for_budgets() {
    let rig = Rig::new(
        Some(script(
            vec![gpu(16000)],
            vec![json!([{"pid": 100, "local_mib": 115, "dedicated_mib": 23000}])],
        )),
        Arc::new(NoEngines),
    )
    .await;
    rig.telemetry.tick(1).await;
    let v = rig.telemetry.vram().unwrap();
    assert_eq!(v.local_by_pid.as_ref().unwrap().get(&100), Some(&115));
    // min(16384 − 115, 16000) − 512; the 23000 MiB of Dedicated Usage would leave 0.
    assert_eq!(free_vram_mib(&v, &[]), 15488);
    assert_eq!(rig.scheduler.admit(&heavy(15000, None)), Admit::Go);
    rig.shutdown().await;
}

#[tokio::test] // spec: ARCH-071
async fn without_nvml_jobs_run_unchecked_with_a_note() {
    let rig = Rig::new(None, Arc::new(NoEngines)).await;
    rig.telemetry.tick(1).await;
    assert_eq!(rig.telemetry.vram(), None);
    assert_eq!(rig.scheduler.admit(&heavy(4000, None)), Admit::GoUnchecked);
    let job = rig.submit(heavy(4000, None)).await;
    let done = rig.finished(&job.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["vram_unchecked"], true, "{done}");
    rig.shutdown().await;

    // With a VRAM check the key is absent.
    let rig = Rig::new(Some(roomy()), Arc::new(NoEngines)).await;
    rig.telemetry.tick(1).await;
    let job = rig.submit(heavy(4000, None)).await;
    let done = rig.finished(&job.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(done.get("vram_unchecked").is_none(), "{done}");
    rig.shutdown().await;
}

#[tokio::test] // spec: ARCH-071
async fn lease_switch_asks_the_previous_engine_to_unload() {
    let engines = Arc::new(Engines::default());
    let rig = Rig::new(Some(roomy()), engines.clone()).await;
    rig.telemetry.tick(1).await;
    for _ in 0..2 {
        let job = rig.submit(heavy(1000, Some(EngineKind::ComfyUi))).await;
        assert_eq!(rig.finished(&job.job_id).await["status"], "succeeded");
    }
    // A job of no engine leaves the lease where it is.
    let job = rig.submit(heavy(1000, None)).await;
    assert_eq!(rig.finished(&job.job_id).await["status"], "succeeded");
    rig.idle().await;
    assert!(
        engines.unloads.borrow().is_empty(),
        "{:?}",
        engines.unloads.borrow()
    );
    let job = rig.submit(heavy(1000, Some(EngineKind::Test))).await;
    assert_eq!(rig.finished(&job.job_id).await["status"], "succeeded");
    tokio::time::timeout(
        DEADLINE,
        engines.unloads.subscribe().wait_for(|u| !u.is_empty()),
    )
    .await
    .expect("no unload")
    .unwrap();
    rig.idle().await;
    assert_eq!(*engines.unloads.borrow(), [EngineKind::ComfyUi]);
    rig.shutdown().await;
}

#[tokio::test] // spec: ARCH-072, CORE-126
async fn watchdog_flags_slow_at_three_times_and_never_cancels() {
    assert!(!watchdog_slow(10, Duration::from_secs(30)));
    assert!(watchdog_slow(10, Duration::from_secs(31)));
    assert!(
        !watchdog_slow(0, Duration::from_secs(3600)),
        "no expected time"
    );

    let rig = Rig::new(Some(roomy()), Arc::new(NoEngines)).await;
    rig.telemetry.tick(1).await;
    let job = rig
        .submit(spec(Lane::GpuHeavy, json!({"gate": "slow"})))
        .await;
    rig.subscribe(format!("job:{}", job.job_id)).await;
    rig.gates.until_started(&job.job_id).await;
    // Measured from the moment the attempt became `executing`.
    rig.clock.advance(Duration::from_secs(30));
    rig.telemetry.tick(1).await;
    let row = record::get(&rig.states.suite_db(), &job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.watchdog, None, "not past 3× yet");
    rig.clock.advance(Duration::from_secs(1));
    rig.telemetry.tick(1).await;
    let flagged = rig
        .event(&format!("job:{}", job.job_id), |d| d["kind"] == "watchdog")
        .await;
    assert_eq!(flagged["watchdog"], "slow", "{flagged}");
    let row = record::get(&rig.states.suite_db(), &job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.watchdog.as_deref(), Some("slow"));
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Running, Some(Substatus::Executing)),
        "it keeps running"
    );
    rig.gates.open("slow");
    let done = rig.finished(&job.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["watchdog"], "slow", "{done}");
    rig.shutdown().await;
}

#[tokio::test] // spec: CORE-061
async fn busy_flag_follows_running_jobs() {
    let rig = Rig::new(Some(roomy()), Arc::new(NoEngines)).await;
    rig.telemetry.tick(1).await;
    assert!(!rig.telemetry.busy());
    let cpu = rig.submit(spec(Lane::Cpu, json!({"gate": "cpu"}))).await;
    assert!(rig.telemetry.busy(), "set when a job starts");
    let gpu = rig
        .submit(spec(Lane::GpuHeavy, json!({"gate": "gpu"})))
        .await;
    rig.gates.until_started(&cpu.job_id).await;
    rig.gates.until_started(&gpu.job_id).await;
    rig.gates.open("cpu");
    rig.finished(&cpu.job_id).await;
    assert!(rig.telemetry.busy(), "a job still runs");
    rig.gates.open("gpu");
    rig.finished(&gpu.job_id).await;
    rig.idle().await;
    assert!(!rig.telemetry.busy(), "cleared when the last one ends");
    rig.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Fix round 1.
// ---------------------------------------------------------------------------------------------

/// The row of job `id` in the suite `state.db`.
async fn row(rig: &Rig, id: &str) -> record::JobRow {
    record::get(&rig.states.suite_db(), id)
        .await
        .unwrap()
        .unwrap()
}

/// I1: with NVML present, a job is never let through unchecked for want of a reading. Before
/// the first sample it waits, and runs checked once the sample is in; while the GPU reading is
/// lost it waits again.
#[tokio::test] // spec: ARCH-071
async fn with_nvml_a_job_waits_for_a_reading_instead_of_running_unchecked() {
    let rig = Rig::new(
        Some(script(
            vec![gpu(15000), json!({"error": "GPU lost"}), gpu(15000)],
            vec![mem(&[])],
        )),
        Arc::new(NoEngines),
    )
    .await;
    assert!(rig.telemetry.has_gpu() && rig.telemetry.vram().is_none());
    let job = rig.submit(heavy(1000, None)).await;
    rig.in_substatus(&job.job_id, "waiting_for_vram").await;
    rig.telemetry.tick(1).await;
    let done = rig.finished(&job.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(done.get("vram_unchecked").is_none(), "{done}");
    // The GPU reading is lost: wait for the next one.
    rig.telemetry.tick(1).await;
    assert_eq!(rig.telemetry.vram(), None);
    assert_eq!(
        rig.scheduler.admit(&heavy(1000, None)),
        Admit::WaitForVram {
            need_mib: 1000,
            free_mib: 0
        }
    );
    rig.telemetry.tick(1).await;
    assert_eq!(rig.scheduler.admit(&heavy(1000, None)), Admit::Go);
    rig.shutdown().await;
}

/// I2a, ruling P3-R16 as amended: a need above the card counts as total − 512 MiB, and on a card
/// that never empties (600 MiB always held by another process) the job runs once the lane is
/// idle, the other engine holding VRAM has been unloaded, and its free VRAM has not risen for
/// 3 consecutive samples; a rise starts the count again. The lane then goes on.
#[tokio::test] // spec: ARCH-071
async fn a_need_above_the_card_runs_once_free_vram_stops_rising() {
    let engines = Arc::new(Engines {
        pids: HashMap::from([(EngineKind::ComfyUi, vec![200])]),
        ..Engines::default()
    });
    // free = min(16384 − 600 − 2000, nvml_free) − 512 = nvml_free − 512 throughout.
    let nvml = [11000, 11500, 11500, 12000, 12000, 12000, 12000];
    let rig = Rig::new(
        Some(script(
            nvml.iter().map(|f| gpu(*f)).collect(),
            vec![mem(&[(100, 600), (200, 2000)])],
        )),
        engines.clone(),
    )
    .await;
    rig.telemetry.tick(1).await;
    let big = rig.submit(heavy(20000, None)).await;
    let small = rig.submit(heavy(100, None)).await;
    rig.in_substatus(&big.job_id, "waiting_for_vram").await;
    // ComfyUI holds VRAM, so it is asked to unload.
    tokio::time::timeout(
        DEADLINE,
        engines.unloads.subscribe().wait_for(|u| !u.is_empty()),
    )
    .await
    .expect("the other engine was never asked to unload")
    .unwrap();
    for _ in 2..=nvml.len() {
        rig.telemetry.tick(1).await;
    }
    assert_eq!(rig.finished(&big.job_id).await["status"], "succeeded");
    assert_eq!(rig.finished(&small.job_id).await["status"], "succeeded");
    assert_eq!(*engines.unloads.borrow(), [EngineKind::ComfyUi]);
    // Samples 1–7 read 10488, 10988 (rises: 0), 10988 (1), 11488 (rises: 0), then 1, 2, 3.
    let asked: Vec<(u64, Admit)> = rig
        .recording
        .asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(need, _, _)| *need == Some(20000))
        .map(|(_, sample, answer)| (*sample, *answer))
        .collect();
    let first_go = asked.iter().find(|(_, a)| *a == Admit::Go).map(|(s, _)| *s);
    assert_eq!(first_go, Some(7), "{asked:?}");
    let free = |nvml_free: u64| Admit::WaitForVram {
        need_mib: TOTAL - 512,
        free_mib: nvml_free - 512,
    };
    for (sample, answer) in &asked {
        if *sample < 7 {
            let at = usize::try_from(*sample).unwrap() - 1;
            assert_eq!(*answer, free(nvml[at]), "sample {sample}: {asked:?}");
        }
    }
    rig.shutdown().await;
}

/// P3-R18: a GPU whose reading stays lost keeps a `gpu-heavy` job waiting, visibly, with no
/// fallback to unchecked; you can cancel it.
#[tokio::test] // spec: ARCH-071
async fn a_lost_gpu_keeps_the_job_waiting_until_you_cancel_it() {
    let rig = Rig::new(
        Some(script(
            vec![gpu(15000), json!({"error": "GPU lost"})],
            vec![mem(&[])],
        )),
        Arc::new(NoEngines),
    )
    .await;
    rig.telemetry.tick(2).await;
    assert!(rig.telemetry.has_gpu() && rig.telemetry.vram().is_none());
    let job = rig.submit(heavy(1000, None)).await;
    rig.in_substatus(&job.job_id, "waiting_for_vram").await;
    rig.telemetry.tick(5).await;
    let still = row(&rig, &job.job_id).await;
    assert_eq!(
        (still.status, still.substatus, still.vram_unchecked),
        (JobStatus::Queued, Some(Substatus::WaitingForVram), false)
    );
    assert!(rig.gates.started.borrow().is_empty());
    let cancelled = rig
        .jobs
        .cancel(&rig.caller(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    rig.idle().await;
    rig.shutdown().await;
}

/// I2b, ruling P3-R17: without per-process GPU memory, a job of the engine that holds the lease
/// runs unchecked; a job of another engine is checked (no per-process figures) and the holder
/// is asked to unload.
#[tokio::test] // spec: ARCH-071
async fn unknown_per_process_memory_runs_the_lease_holders_job_unchecked() {
    let engines = Arc::new(Engines {
        pids: HashMap::from([(EngineKind::ComfyUi, vec![200])]),
        ..Engines::default()
    });
    let rig = Rig::new(
        Some(
            json!({"gpu": [gpu(3000)], "gpu_mem": null, "cpu_energy": null,
                    "sys": [{"cpu_permille": 100}]}),
        ),
        engines.clone(),
    )
    .await;
    rig.telemetry.tick(1).await;
    assert_eq!(rig.telemetry.vram().unwrap().local_by_pid, None);
    // The first ComfyUI job takes the lease, checked: min(16384, 3000) − 512 = 2488.
    let first = rig.submit(heavy(1000, Some(EngineKind::ComfyUi))).await;
    let done = rig.finished(&first.job_id).await;
    assert!(done.get("vram_unchecked").is_none(), "{done}");
    // ComfyUI holds the lease and may hold the card: its next job runs unchecked.
    let next = rig.submit(heavy(14000, Some(EngineKind::ComfyUi))).await;
    let done = rig.finished(&next.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["vram_unchecked"], true, "{done}");
    rig.idle().await;
    // Another engine's job is checked, and ComfyUI is asked to unload.
    assert_eq!(
        rig.scheduler.admit(&heavy(14000, Some(EngineKind::Test))),
        Admit::WaitForVram {
            need_mib: 14000,
            free_mib: 2488
        }
    );
    tokio::time::timeout(
        DEADLINE,
        engines.unloads.subscribe().wait_for(|u| !u.is_empty()),
    )
    .await
    .expect("no unload")
    .unwrap();
    assert_eq!(*engines.unloads.borrow(), [EngineKind::ComfyUi]);
    rig.shutdown().await;
}

/// I3: the watchdog times only the current attempt while it executes: not the backoff, not the
/// attempts before it.
#[tokio::test] // spec: ARCH-072, CORE-126
async fn watchdog_ignores_backoff_and_earlier_attempts() {
    let rig = Rig::new(Some(roomy()), Arc::new(NoEngines)).await;
    rig.telemetry.tick(1).await;
    let job = rig
        .submit(spec(
            Lane::GpuHeavy,
            json!({"gate_1": "one", "gate": "two", "fail_first": true}),
        ))
        .await;
    rig.gates.until_started(&job.job_id).await;
    // Attempt 1 executes 25 s (not slow), then fails transiently.
    rig.clock.advance(Duration::from_secs(25));
    rig.jobs.kick();
    rig.gates.open("one");
    rig.in_substatus(&job.job_id, "retrying_1_of_5").await;
    // In the 1 s backoff, 31 s after attempt 1 began executing.
    rig.clock.advance(Duration::from_secs(6));
    rig.jobs.kick();
    assert_eq!(
        rig.gates.started.borrow().len(),
        1,
        "the kick fell in the backoff"
    );
    // Attempt 2 starts its own clock.
    let mut started = rig.gates.started.subscribe();
    tokio::time::timeout(DEADLINE, started.wait_for(|s| s.len() == 2))
        .await
        .expect("attempt 2 never started")
        .unwrap();
    rig.clock.advance(Duration::from_secs(29));
    rig.jobs.kick();
    assert_eq!(row(&rig, &job.job_id).await.watchdog, None);
    rig.gates.open("two");
    let done = rig.finished(&job.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["attempt"], 2, "{done}");
    assert!(done.get("watchdog").is_none(), "{done}");
    rig.shutdown().await;
}

/// M1: the lease passes when the job is asked about, so a job for another engine that waits
/// for the previous engine's memory gets that engine unloaded while it waits, and runs once the
/// memory is free (deviation 1: passing it only at `Go` would leave it waiting for good).
#[tokio::test] // spec: ARCH-071
async fn a_job_waiting_on_the_previous_engines_memory_gets_it_unloaded() {
    let engines = Arc::new(Engines {
        pids: HashMap::from([(EngineKind::ComfyUi, vec![200])]),
        ..Engines::default()
    });
    let rig = Rig::new(
        Some(script(
            vec![gpu(15000), gpu(2000), gpu(15000)],
            vec![mem(&[(200, 1000)]), mem(&[])],
        )),
        engines.clone(),
    )
    .await;
    rig.telemetry.tick(1).await;
    let comfy = rig.submit(heavy(1000, Some(EngineKind::ComfyUi))).await;
    assert_eq!(rig.finished(&comfy.job_id).await["status"], "succeeded");
    rig.idle().await;
    // ComfyUI now holds the card: min(16384 − 1000, 2000) − 512 = 1488 < 8000.
    rig.telemetry.tick(1).await;
    let other = rig.submit(heavy(8000, Some(EngineKind::Test))).await;
    rig.in_substatus(&other.job_id, "waiting_for_vram").await;
    tokio::time::timeout(
        DEADLINE,
        engines.unloads.subscribe().wait_for(|u| !u.is_empty()),
    )
    .await
    .expect("the previous engine was never asked to unload")
    .unwrap();
    let waiting = row(&rig, &other.job_id).await;
    assert_eq!(waiting.substatus, Some(Substatus::WaitingForVram));
    assert_eq!(*engines.unloads.borrow(), [EngineKind::ComfyUi]);
    // The unload freed it.
    rig.telemetry.tick(1).await;
    assert_eq!(rig.finished(&other.job_id).await["status"], "succeeded");
    rig.shutdown().await;
}

/// M2: a job that waited for VRAM and is overtaken by a better class waits for its turn again.
#[tokio::test] // spec: ARCH-071, CORE-124
async fn an_overtaken_vram_wait_goes_back_to_waiting() {
    let rig = Rig::new(
        Some(script(vec![gpu(1000)], vec![mem(&[])])),
        Arc::new(NoEngines),
    )
    .await;
    rig.telemetry.tick(1).await;
    let batch = rig.submit(heavy(4000, None)).await;
    rig.in_substatus(&batch.job_id, "waiting_for_vram").await;
    // An interactive job that fits (488 MiB free) is picked instead.
    let quick = rig
        .submit(JobSpec {
            priority: Priority::Interactive,
            vram_need_mib: Some(100),
            ..spec(Lane::GpuHeavy, json!({"gate": "quick"}))
        })
        .await;
    rig.gates.until_started(&quick.job_id).await;
    // (Its first `waiting` event came at its submit, so the row tells.)
    tokio::time::timeout(DEADLINE, async {
        while row(&rig, &batch.job_id).await.substatus != Some(Substatus::Waiting) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the overtaken job still says waiting_for_vram");
    assert_eq!(row(&rig, &batch.job_id).await.status, JobStatus::Queued);
    rig.gates.open("quick");
    rig.finished(&quick.job_id).await;
    rig.jobs
        .cancel(&rig.caller(), &JobId(batch.job_id.clone()))
        .await
        .unwrap();
    rig.finished(&batch.job_id).await;
    rig.idle().await;
    rig.shutdown().await;
}

/// P3-R16 as amended: flat samples alone don't let an oversized job run while another engine
/// that holds VRAM hasn't finished unloading.
#[tokio::test] // spec: ARCH-071
async fn an_oversized_job_waits_for_the_other_engines_unload() {
    let engines = Arc::new(Engines {
        pids: HashMap::from([(EngineKind::ComfyUi, vec![200])]),
        stuck: true,
        ..Engines::default()
    });
    let rig = Rig::new(
        Some(script(
            vec![gpu(12000)],
            vec![mem(&[(100, 600), (200, 2000)])],
        )),
        engines.clone(),
    )
    .await;
    rig.telemetry.tick(1).await;
    let big = rig.submit(heavy(20000, None)).await;
    rig.in_substatus(&big.job_id, "waiting_for_vram").await;
    rig.telemetry.tick(6).await;
    let asked = rig.recording.asked.lock().unwrap().clone();
    assert!(
        asked
            .iter()
            .all(|(_, _, a)| matches!(a, Admit::WaitForVram { .. })),
        "{asked:?}"
    );
    assert_eq!(asked.last().map(|(_, sample, _)| *sample), Some(7));
    assert_eq!(
        *engines.unloads.borrow(),
        [EngineKind::ComfyUi],
        "asked once"
    );
    rig.jobs
        .cancel(&rig.caller(), &JobId(big.job_id.clone()))
        .await
        .unwrap();
    rig.idle().await;
    rig.shutdown().await;
}

/// P3-R16 (round 3): free VRAM creeping up 10 MiB a sample is drift, not a rise, so the
/// oversized job still runs after 3 samples.
#[tokio::test] // spec: ARCH-071
async fn free_vram_drift_does_not_restart_the_flat_count() {
    let nvml = [12000, 12010, 12020, 12030, 12040];
    let rig = Rig::new(
        Some(script(
            nvml.iter().map(|f| gpu(*f)).collect(),
            vec![mem(&[(100, 600)])],
        )),
        Arc::new(NoEngines),
    )
    .await;
    rig.telemetry.tick(1).await;
    let big = rig.submit(heavy(20000, None)).await;
    rig.in_substatus(&big.job_id, "waiting_for_vram").await;
    rig.telemetry.tick(3).await;
    assert_eq!(rig.finished(&big.job_id).await["status"], "succeeded");
    let first_go = rig
        .recording
        .asked
        .lock()
        .unwrap()
        .iter()
        .find(|(_, _, a)| *a == Admit::Go)
        .map(|(_, sample, _)| *sample);
    assert_eq!(
        first_go,
        Some(4),
        "baseline at sample 1, then 3 samples of drift"
    );
    rig.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Round 4.
// ---------------------------------------------------------------------------------------------

/// The samples at which the jobs' admission let a job needing `need` MiB go.
fn go_samples(rig: &Rig, need: u64) -> Vec<u64> {
    rig.recording
        .asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, _, a)| *n == Some(need) && *a == Admit::Go)
        .map(|(_, sample, _)| *sample)
        .collect()
}

/// N1, ruling P3-R16b: free VRAM wobbling by 100 MiB (12000, 12100, 12000, …) rises only once,
/// to its first high, so the oversized job runs (the reviewer's probe: before, it waited for
/// all 30 samples).
#[tokio::test] // spec: ARCH-071
async fn free_vram_wobble_does_not_keep_an_oversized_job_waiting() {
    // free = min(16384 − 600, nvml_free) − 512.
    let nvml: Vec<Value> = (0..30)
        .map(|i| gpu(if i % 2 == 0 { 12512 } else { 12612 }))
        .collect();
    let rig = Rig::new(
        Some(script(nvml, vec![mem(&[(100, 600)])])),
        Arc::new(NoEngines),
    )
    .await;
    rig.telemetry.tick(1).await;
    let big = rig.submit(heavy(20000, None)).await;
    rig.in_substatus(&big.job_id, "waiting_for_vram").await;
    rig.telemetry.tick(29).await;
    assert_eq!(rig.finished(&big.job_id).await["status"], "succeeded");
    // Sample 1 is the baseline (12000), sample 2 the one rise (12100), then 3 flat samples.
    assert_eq!(go_samples(&rig, 20000).first(), Some(&5));
    rig.shutdown().await;
}

/// N2, ruling P3-R16b: the flat count starts only with the first sample after the needed
/// unload finished, so samples taken while the other engine still held its memory don't count.
#[tokio::test] // spec: ARCH-071
async fn the_flat_count_starts_after_the_unloads_finish() {
    let engines = Arc::new(Engines {
        pids: HashMap::from([(EngineKind::ComfyUi, vec![200])]),
        gate: Some(watch::Sender::new(false)),
        ..Engines::default()
    });
    let rig = Rig::new(
        Some(script(
            vec![gpu(12000)],
            vec![mem(&[(100, 600), (200, 2000)])],
        )),
        engines.clone(),
    )
    .await;
    rig.telemetry.tick(1).await;
    let big = rig.submit(heavy(20000, None)).await;
    rig.in_substatus(&big.job_id, "waiting_for_vram").await;
    // Samples 2–5, flat, while ComfyUI's unload is under way.
    rig.telemetry.tick(4).await;
    assert!(go_samples(&rig, 20000).is_empty());
    engines.gate.as_ref().unwrap().send_replace(true);
    tokio::time::timeout(DEADLINE, engines.finished.subscribe().wait_for(|n| *n == 1))
        .await
        .expect("the unload never finished")
        .unwrap();
    // Sample 6 is the first after it: the baseline; 7, 8 and 9 are flat.
    rig.telemetry.tick(4).await;
    assert_eq!(rig.finished(&big.job_id).await["status"], "succeeded");
    assert_eq!(go_samples(&rig, 20000).first(), Some(&9));
    rig.shutdown().await;
}

/// N3: when the lease changes hands, the unload state of the previous holder goes with it. A
/// lease that goes back and forth while the unloads are slow sends each switch's unload, and an
/// old unload that finishes late doesn't mark an engine that took the lease back as unloaded.
#[tokio::test] // spec: ARCH-071
async fn a_lease_ping_pong_carries_no_stale_unload_state() {
    let engines = Arc::new(Engines {
        pids: HashMap::from([(EngineKind::ComfyUi, vec![200])]),
        gate: Some(watch::Sender::new(false)),
        ..Engines::default()
    });
    let rig = Rig::new(
        Some(script(vec![gpu(15000)], vec![mem(&[(200, 2000)])])),
        engines.clone(),
    )
    .await;
    rig.telemetry.tick(1).await;
    let (c, t) = (EngineKind::ComfyUi, EngineKind::Test);
    for e in [c, t, c, t, c] {
        assert_eq!(rig.scheduler.admit(&heavy(100, Some(e))), Admit::Go);
    }
    // Each switch asks the previous holder, even while its earlier unload is still under way.
    tokio::time::timeout(
        DEADLINE,
        engines.unloads.subscribe().wait_for(|u| u.len() == 4),
    )
    .await
    .expect("a switch's unload was swallowed by an earlier one still under way")
    .unwrap();
    assert_eq!(*engines.unloads.borrow(), [c, t, c, t]);
    engines.gate.as_ref().unwrap().send_replace(true);
    tokio::time::timeout(DEADLINE, engines.finished.subscribe().wait_for(|n| *n == 4))
        .await
        .expect("the unloads never finished")
        .unwrap();
    // ComfyUI holds the lease (and 2000 MiB) again: its old unloads finishing don't count, so
    // an oversized job asks it again.
    assert!(matches!(
        rig.scheduler.admit(&heavy(20000, None)),
        Admit::WaitForVram { .. }
    ));
    tokio::time::timeout(
        DEADLINE,
        engines.unloads.subscribe().wait_for(|u| u.len() == 5),
    )
    .await
    .expect("a stale unload marked ComfyUI unloaded")
    .unwrap();
    assert_eq!(engines.unloads.borrow().last(), Some(&c));
    rig.shutdown().await;
}

/// M3: input names only real engines; the row column reads the test kind back.
#[test] // spec: ARCH-064
fn the_test_engine_kind_is_not_input() {
    assert_eq!(EngineKind::parse("comfyui"), Some(EngineKind::ComfyUi));
    assert_eq!(EngineKind::parse("test"), None);
    assert_eq!(EngineKind::from_column("test"), Some(EngineKind::Test));
    assert_eq!(
        EngineKind::from_column("comfyui"),
        Some(EngineKind::ComfyUi)
    );
}

/// M5: a job that ran unchecked stays marked after it is preempted and runs again checked.
#[tokio::test] // spec: ARCH-071, CORE-125
async fn a_job_that_ran_unchecked_stays_marked_after_a_preemption() {
    let rig = Rig::new(
        Some(
            json!({"gpu": [gpu(15000)], "gpu_mem": null, "cpu_energy": null,
                    "sys": [{"cpu_permille": 100}]}),
        ),
        Arc::new(Engines::default()),
    )
    .await;
    rig.jobs.set_preempt_agent_batches(true);
    rig.telemetry.tick(1).await;
    // ComfyUI takes the lease, checked.
    let first = rig.submit(heavy(100, Some(EngineKind::ComfyUi))).await;
    assert!(
        rig.finished(&first.job_id)
            .await
            .get("vram_unchecked")
            .is_none()
    );
    // With no per-process figures, its next job runs unchecked (P3-R17) ...
    let batch = rig
        .submit(JobSpec {
            engine: Some(EngineKind::ComfyUi),
            vram_need_mib: Some(100),
            ..spec(Lane::GpuHeavy, json!({"gate": "batch"}))
        })
        .await;
    rig.gates.until_started(&batch.job_id).await;
    // ... an interactive job of another engine preempts it ...
    let quick = rig
        .submit(JobSpec {
            priority: Priority::Interactive,
            engine: Some(EngineKind::Test),
            vram_need_mib: Some(100),
            ..spec(Lane::GpuHeavy, json!({}))
        })
        .await;
    assert_eq!(rig.finished(&quick.job_id).await["status"], "succeeded");
    // ... and it runs again, checked this time: the lease comes back to ComfyUI.
    let mut started = rig.gates.started.subscribe();
    tokio::time::timeout(
        DEADLINE,
        started.wait_for(|s| s.iter().filter(|j| **j == batch.job_id).count() == 2),
    )
    .await
    .expect("the preempted job never ran again")
    .unwrap();
    rig.gates.open("batch");
    let done = rig.finished(&batch.job_id).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["vram_unchecked"], true, "{done}");
    let asked = rig.recording.asked.lock().unwrap().clone();
    let batch_answers: Vec<Admit> = asked
        .iter()
        .filter(|(need, _, _)| *need == Some(100))
        .map(|(_, _, a)| *a)
        .collect();
    assert!(
        batch_answers.contains(&Admit::GoUnchecked) && batch_answers.last() == Some(&Admit::Go),
        "{asked:?}"
    );
    rig.shutdown().await;
}

/// Whether job `id` was flagged by now: reads the events that arrive for a moment (a short
/// grace for the watchdog's own task and its row write, never a measurement), then looks for
/// a `watchdog` event on `job:<id>` among every event read so far and at the row. For what must
/// not have happened: a flag that came early is still among the events at a later check.
async fn flagged(rig: &Rig, id: &str) -> bool {
    let topic = format!("job:{id}");
    {
        let mut guard = rig.rx.lock().await;
        let (rx, seen) = &mut *guard;
        let until = tokio::time::Instant::now() + Duration::from_millis(300);
        while let Ok(Some(line)) = tokio::time::timeout_at(until, rx.recv()).await {
            let event: Value = serde_json::from_str(&line).unwrap();
            seen.push((
                event["params"]["topic"].as_str().unwrap().to_string(),
                event["params"]["data"].clone(),
            ));
        }
        if seen
            .iter()
            .any(|(t, d)| *t == topic && d["kind"] == "watchdog")
        {
            return true;
        }
    }
    row(rig, id).await.watchdog.is_some()
}

/// The watchdog's bound (ARCH-072, Appendix G): `max(3 × p50, 2 × p90)` of the job's measured
/// `wall_s` when it has one; else 3 × `expected_secs`. A history of p50 10 s and p90 30 s
/// flags at 60 s, not at the 30 s its expected time of 10 s would; a job with no history and
/// an expected 20 s at 60 s too.
#[tokio::test] // spec: ARCH-072, CORE-161
async fn watchdog_uses_the_history_bound() {
    let wall = WallPart {
        source: EstimateSource::MeasuredHistory,
        samples: 3,
        p50: "10.0".into(),
        p90: "30.0".into(),
    };
    assert_eq!(
        watchdog_limit(10, Some(&wall)),
        Some(Duration::from_secs(60))
    );
    assert_eq!(watchdog_limit(20, None), Some(Duration::from_secs(60)));
    assert_eq!(
        watchdog_limit(0, None),
        None,
        "no expected time, no history"
    );
    let unmeasured = WallPart {
        p50: "0.0".into(),
        p90: "0.0".into(),
        ..wall.clone()
    };
    assert_eq!(
        watchdog_limit(10, Some(&unmeasured)),
        Some(Duration::from_secs(30)),
        "a history that measured no time counts as none"
    );

    let rig = Rig::new(Some(roomy()), Arc::new(NoEngines)).await;
    rig.jobs.set_estimator(Estimator::new(
        rig.telemetry.clone(),
        &PowerSettings::default(),
        Arc::new(|| Currency::parse("USD").unwrap()),
    ));
    for (n, wall_ms) in [10_000, 10_000, 30_000].into_iter().enumerate() {
        rig.telemetry
            .put_job_stats(&JobStatsRow {
                job_id: format!("j_{n:016x}"),
                kind: "t.gpu".into(),
                estimate_key: "k".into(),
                finished_ms: 1_800_000_000_000,
                wall_ms,
                gpu_seconds_milli: 0,
                system_mj: 0,
            })
            .await
            .unwrap();
    }
    rig.telemetry.tick(1).await;

    // p50 10 s, p90 30 s: the bound is 60 s.
    let job = rig
        .submit(spec(Lane::GpuHeavy, json!({"gate": "known", "key": "k"})))
        .await;
    let est = job.estimate.as_ref().expect("an estimate at submission");
    assert_eq!(est.wall_s, Some(wall.clone()), "{est:?}");
    rig.subscribe(format!("job:{}", job.job_id)).await;
    rig.gates.until_started(&job.job_id).await;
    rig.clock.advance(Duration::from_secs(31));
    rig.telemetry.tick(1).await;
    assert!(
        !flagged(&rig, &job.job_id).await,
        "3 × expected is not the bound"
    );
    rig.clock.advance(Duration::from_secs(29));
    rig.telemetry.tick(1).await;
    assert!(!flagged(&rig, &job.job_id).await, "60 s is not past 60 s");
    rig.clock.advance(Duration::from_secs(1));
    rig.telemetry.tick(1).await;
    // Past max(3 × p50, 2 × p90).
    rig.event(&format!("job:{}", job.job_id), |d| d["kind"] == "watchdog")
        .await;
    rig.gates.open("known");
    assert_eq!(rig.finished(&job.job_id).await["watchdog"], "slow");

    // No history (`unknown` only): 3 × its expected 20 s.
    let mut fresh = spec(Lane::GpuHeavy, json!({"gate": "fresh", "key": "fresh"}));
    fresh.expected_secs = 20;
    let job = rig.submit(fresh).await;
    let est = job.estimate.as_ref().expect("an estimate at submission");
    assert!(est.wall_s.is_none(), "{est:?}");
    rig.subscribe(format!("job:{}", job.job_id)).await;
    rig.gates.until_started(&job.job_id).await;
    rig.clock.advance(Duration::from_secs(60));
    rig.telemetry.tick(1).await;
    assert!(!flagged(&rig, &job.job_id).await);
    rig.clock.advance(Duration::from_secs(1));
    rig.telemetry.tick(1).await;
    rig.event(&format!("job:{}", job.job_id), |d| d["kind"] == "watchdog")
        .await;
    rig.gates.open("fresh");
    rig.finished(&job.job_id).await;
    rig.shutdown().await;
}
