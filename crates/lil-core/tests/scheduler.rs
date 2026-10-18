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
use lil_core::jobs::record;
use lil_core::jobs::scheduler::{
    EngineControl, GpuScheduler, NoEngines, free_vram_mib, watchdog_slow,
};
use lil_core::jobs::{
    Admission, Admit, EngineKind, JobCaller, JobCtx, JobDone, JobFailure, JobFuture, JobHandler,
    JobSpec, JobStatus, JobSummary, Jobs, Lane, Priority, SubmittedSlot, Substatus,
};
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::telemetry::{HwSources, PowerSettings, Telemetry, TickMode, VramView};
use lil_core::workspace::WorkspaceHub;
use lil_platform::fs::SystemProbe;
use lil_platform::{Clock, IdSource};
use lil_protocol::CallerKind;
use lil_testkit::{FakeClock, FakeHwScript, SeqIds, TestEnv};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

/// How long a test waits for something before it fails (never a pass condition).
const DEADLINE: Duration = Duration::from_secs(30);
const TOTAL: u64 = 16384;

// ---------------------------------------------------------------------------------------------
// The fixture.
// ---------------------------------------------------------------------------------------------

/// Runs `t.gpu` jobs: with `{"gate": g}` it waits until [`Gates::open`]`(g)` (or a cancel).
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

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(async move {
            self.started.send_modify(|s| s.push(job.id.to_string()));
            if let Some(gate) = job.spec.args.get("gate").and_then(Value::as_str) {
                let mut open = self.open.subscribe();
                let mut cancel = job.cancel.clone();
                tokio::select! {
                    _ = open.wait_for(|s| s.contains(gate)) => {}
                    _ = cancel.wait_for(|c| *c) => {}
                }
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
}

impl EngineControl for Engines {
    fn pids(&self, e: EngineKind) -> Vec<u32> {
        self.pids.get(&e).cloned().unwrap_or_default()
    }

    fn unload<'a>(&'a self, e: EngineKind) -> BoxFuture<'a, ()> {
        self.unloads.send_modify(|u| u.push(e));
        Box::pin(std::future::ready(()))
    }
}

type ReadEvent = (String, Value);

struct Rig {
    jobs: Arc<Jobs>,
    telemetry: Arc<Telemetry>,
    scheduler: Arc<GpuScheduler>,
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
        jobs.set_admission(scheduler.clone());
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
        local_by_pid: local.iter().copied().collect(),
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
    assert_eq!(v.local_by_pid.get(&100), Some(&115));
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
