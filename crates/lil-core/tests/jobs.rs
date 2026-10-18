//! The `jobs` module in process (ARCH-020–022, ARCH-034, ARCH-118, CORE-009, CORE-015–016,
//! CORE-117, CORE-121, CORE-124, CORE-125, CORE-128, CORE-146, AI-113): records, lanes and the
//! suite-held queue, retries, recovery, cancel and retry, completions, and the lifecycle hooks.
//!
//! Most tests run on a paused runtime: tokio's clock moves only to the next timer while every
//! task waits, so a job's backoff is measured exactly. They wait for what they need through
//! the `jobs` topic and the test handler's own signals, never through a timer of their own
//! (a timer of the test would let the paused clock jump past the job's). The tests that open a
//! workspace (whose watcher has timers of its own) run in real time; their only time limits
//! are failure deadlines (QA-031).
#![allow(
    clippy::result_large_err,
    reason = "JobFailure holds the wire ErrorBody, as in lil-core itself"
)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::jobs::record::{self, JobRow};
use lil_core::jobs::runner::cancelled_error;
use lil_core::jobs::{
    EngineKind, JobCompletion, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobId, JobSpec,
    JobStatus, JobSummary, Jobs, Lane, MAX_BACKOFF, Priority, SubmittedSlot, Substatus, backoff,
};
use lil_core::lifecycle::{DrainReason, Lifecycle, ShutdownPhase, ShutdownStep};
use lil_core::pipeline::{RecordBranch, RecordPlan, RecordSlot, start_candidate_run};
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::{CommitOp, FailPoints, ManifestEdit, WsPath};
use lil_core::tools::{CallCtx, Settings};
use lil_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_platform::fs::SystemProbe;
use lil_platform::{Clock, IdSource};
use lil_protocol::{CallerKind, ErrorBody, Outcome, ToolOutput, codes};
use lil_schema::Manifest;
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

// ---------------------------------------------------------------------------------------------
// The test handler.
// ---------------------------------------------------------------------------------------------

/// One attempt the handler saw.
#[derive(Debug, Clone)]
struct Run {
    job: String,
    attempt: u32,
    at: tokio::time::Instant,
    trace_id: String,
    /// The job's row when the attempt began.
    status_at_start: JobStatus,
}

#[derive(Default)]
struct Seen {
    open_gates: HashSet<String>,
    runs: Vec<Run>,
    engine_cancels: Vec<String>,
}

/// Runs jobs of kind `t.job`. Arguments: `gate` (wait until [`TestHandler::open`] or a
/// cancel; `gate_self: true` gates on the job's own id), `fail` (`retryable` on attempts 1–2, `retryable_always`, `disk_full`, `unknown`,
/// `interrupted`), `record` (leave a candidate-run record plan).
#[derive(Default)]
struct TestHandler {
    seen: Mutex<Seen>,
    changed: watch::Sender<u64>,
}

impl TestHandler {
    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    fn open(&self, gate: &str) {
        self.seen
            .lock()
            .unwrap()
            .open_gates
            .insert(gate.to_string());
        self.bump();
    }

    fn runs(&self) -> Vec<Run> {
        self.seen.lock().unwrap().runs.clone()
    }

    fn started(&self) -> Vec<String> {
        self.runs().into_iter().map(|r| r.job).collect()
    }

    fn engine_cancels(&self) -> Vec<String> {
        self.seen.lock().unwrap().engine_cancels.clone()
    }

    /// Waits (no timer) until `f` holds for what the handler saw.
    async fn until(&self, f: impl Fn(&Seen) -> bool) {
        let mut rx = self.changed.subscribe();
        loop {
            if f(&self.seen.lock().unwrap()) {
                return;
            }
            rx.changed().await.unwrap();
        }
    }

    async fn until_started(&self, job: &str) {
        self.until(|s| s.runs.iter().any(|r| r.job == job)).await;
    }

    async fn attempt(&self, job: &JobCtx) -> Result<JobDone, JobFailure> {
        let row = record::get(&job.state, job.id.as_str())
            .await
            .unwrap()
            .unwrap();
        self.seen.lock().unwrap().runs.push(Run {
            job: job.id.to_string(),
            attempt: job.attempt,
            at: tokio::time::Instant::now(),
            trace_id: job.trace_id.clone(),
            status_at_start: row.status,
        });
        self.bump();
        let args = &job.spec.args;
        let gate = match args.get("gate").and_then(Value::as_str) {
            Some(gate) => Some(gate.to_string()),
            None => (args.get("gate_self") == Some(&json!(true))).then(|| job.id.to_string()),
        };
        if let Some(gate) = gate {
            let mut changed = self.changed.subscribe();
            let mut cancel = job.cancel.clone();
            loop {
                if self.seen.lock().unwrap().open_gates.contains(&gate) {
                    break;
                }
                if *cancel.borrow() {
                    return Err(JobFailure::Final(cancelled_error(&job.trace_id)));
                }
                tokio::select! {
                    _ = changed.changed() => {}
                    _ = cancel.changed() => {}
                }
            }
        }
        let error = |spec: &lil_protocol::CodeSpec| {
            ErrorBody::new(spec, "on purpose", job.trace_id.clone())
        };
        match args.get("fail").and_then(Value::as_str) {
            Some("retryable") if job.attempt <= 2 => {
                return Err(JobFailure::Retryable(error(&codes::STORE_IO)));
            }
            Some("retryable_always") => return Err(JobFailure::Retryable(error(&codes::STORE_IO))),
            Some("disk_full") => return Err(JobFailure::Final(error(&codes::STORE_DISK_FULL))),
            Some("unknown") => return Err(JobFailure::Unknown(error(&codes::STORE_IO))),
            Some("interrupted") => {
                return Err(JobFailure::Interrupted {
                    error: error(&codes::COMFYUI_UNREACHABLE),
                    salvaged: Vec::new(),
                });
            }
            _ => {}
        }
        let record = args
            .get("record")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            .then(record_plan);
        Ok(JobDone {
            structured: json!({"ran": job.attempt}),
            artifacts: Vec::new(),
            record,
            windows: Vec::new(),
        })
    }
}

impl JobHandler for TestHandler {
    fn kind(&self) -> &'static str {
        "t.job"
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(self.attempt(job))
    }

    fn cancel_in_engine<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, ()> {
        self.seen
            .lock()
            .unwrap()
            .engine_cancels
            .push(job.id.to_string());
        self.bump();
        Box::pin(std::future::ready(()))
    }
}

/// A candidate run of a new asset `prop_jobrec` in `sketchbook`, with no output files.
fn record_plan() -> RecordPlan {
    let (project, asset, run_id) = ("sketchbook", "prop_jobrec", "r_20270115_0800");
    let manifest = WsPath::parse(&format!(
        "projects/{project}/assets/{asset}/{asset}.manifest.json"
    ))
    .unwrap();
    let edit: ManifestEdit = Arc::new(move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            *m = Manifest::new(
                project,
                asset,
                "image",
                "00000000-0000-4000-8000-0000000000aa",
            );
        }
        let Value::Object(run) = json!({
            "run_id": run_id,
            "path": format!("candidates/{run_id}/"),
            "status": "open",
            "files": [],
            "pinned": false,
        }) else {
            unreachable!()
        };
        start_candidate_run(m, run, 0)
    });
    RecordPlan {
        branch: RecordBranch::CandidateRun {
            run_id: run_id.to_string(),
        },
        project: project.to_string(),
        asset_id: asset.to_string(),
        commit: CommitOp {
            outputs: Vec::new(),
            moved: Vec::new(),
            removed: Vec::new(),
            manifest: Some((manifest, edit)),
            job_line: None,
            progress: None,
        },
        progress_entry: "a job's run".to_string(),
        refs: Vec::new(),
        version_guard: None,
    }
}

/// A completion that logs its name and what the job ended as.
struct Logging {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
}

impl JobCompletion for Logging {
    fn completed<'a>(
        &'a self,
        job: &'a JobCtx,
        outcome: &'a Outcome<ToolOutput>,
        done: Option<&'a JobDone>,
    ) -> JobFuture<'a, ()> {
        let what = match outcome {
            Outcome::Ok(_) => "ok".to_string(),
            Outcome::Error { error } => error.code.clone(),
            Outcome::PendingConfirmation(_) => "pending".to_string(),
        };
        self.log.lock().unwrap().push(format!(
            "{} {} {what} done={}",
            self.name,
            job.id,
            done.is_some()
        ));
        Box::pin(std::future::ready(()))
    }
}

// ---------------------------------------------------------------------------------------------
// The fixture.
// ---------------------------------------------------------------------------------------------

/// An event read: `(topic, data)`.
type ReadEvent = (String, Value);

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

struct Fx {
    jobs: Arc<Jobs>,
    handler: Arc<TestHandler>,
    events: Arc<EventBus>,
    clock: Arc<FakeClock>,
    failpoints: Arc<FailPoints>,
    hub: Arc<WorkspaceHub>,
    states: Arc<StateRegistry>,
    open: Option<Arc<OpenWorkspace>>,
    ws: Option<PathBuf>,
    /// The events and every `(topic, data)` read from them so far.
    rx: tokio::sync::Mutex<(mpsc::Receiver<String>, Vec<ReadEvent>)>,
    /// Last: the folder can go only once nothing holds a file in it.
    env: TestEnv,
}

impl Fx {
    /// Jobs without a workspace (the suite `state.db`), with the `jobs` topic subscribed.
    async fn suite() -> Fx {
        Fx::build(false).await
    }

    /// Jobs in an opened copy of the fixture workspace.
    async fn workspace() -> Fx {
        Fx::build(true).await
    }

    async fn build(with_ws: bool) -> Fx {
        let env = TestEnv::new();
        let ws = with_ws.then(|| {
            let ws = env.copy_fixture_workspace("jobs");
            env.register_workspace(&ws, true);
            ws
        });
        let states = Arc::new(
            StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
        );
        let fake = FakeClock::new();
        let clock: Arc<dyn Clock> = fake.clone();
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
            hub.clone(),
            events.clone(),
            clock,
            ids.clone(),
            failpoints.clone(),
        );
        let handler = Arc::new(TestHandler::default());
        jobs.register(handler.clone());
        let open = match &ws {
            Some(ws) => Some(hub.open(&ws_ref(ws)).await.unwrap()),
            None => None,
        };
        let (tx, rx) = mpsc::channel(8192);
        events.attach(1, tx);
        events
            .subscribe(1, &["jobs".to_string()], &BTreeMap::new())
            .await;
        Fx {
            jobs,
            handler,
            events,
            clock: fake,
            failpoints,
            hub,
            states,
            open,
            ws,
            rx: tokio::sync::Mutex::new((rx, Vec::new())),
            env,
        }
    }

    /// A call context of `caller` in the fixture's scope, with idempotency key `key`.
    fn ctx_as(&self, caller: CallerKind, key: Option<&str>, trace_id: &str) -> CallCtx {
        CallCtx {
            session: Arc::new(Session {
                id: "s_0000000000000001".into(),
                caller,
                non_interactive: false,
                workspace: self.open.as_ref().map(|o| o.ws.clone()),
                capabilities: Vec::new(),
                client_pid: 0,
                profile_id: None,
            }),
            trace_id: trace_id.to_string(),
            settings: Settings::default(),
            events: self.events.clone(),
            workspace: self.open.clone(),
            project: self.open.as_ref().map(|_| "sketchbook".to_string()),
            record: Arc::new(RecordSlot::new()),
            jobs: self.jobs.clone(),
            submitted: Arc::new(SubmittedSlot::new()),
            reservation: None,
            confirmed_via: None,
            idem_key: key.map(str::to_string),
        }
    }

    fn ctx(&self) -> CallCtx {
        self.ctx_as(CallerKind::Cli, None, "t_jobs")
    }

    async fn submit(&self, spec: JobSpec) -> JobSummary {
        self.jobs.submit(&self.ctx(), spec).await.unwrap()
    }

    async fn row(&self, id: &str) -> JobRow {
        let db = match &self.open {
            Some(open) => open.state.clone(),
            None => self.states.suite_db(),
        };
        record::get(&db, id).await.unwrap().unwrap()
    }

    /// Reads the `jobs` topic until `id` reaches a final status (no timer): its summary.
    async fn finished(&self, id: &str) -> JobSummary {
        self.event_until(|data| {
            data["kind"] == "job"
                && data["job"]["job_id"] == id
                && matches!(
                    data["job"]["status"].as_str(),
                    Some("succeeded" | "failed" | "cancelled")
                )
        })
        .await
        .map(|data| serde_json::from_value(data["job"].clone()).unwrap())
        .unwrap()
    }

    /// The first event (read before or now) whose `data` satisfies `f`.
    async fn event_until(&self, f: impl Fn(&Value) -> bool) -> Option<Value> {
        let mut guard = self.rx.lock().await;
        let (rx, seen) = &mut *guard;
        if let Some((_, data)) = seen.iter().find(|(_, d)| f(d)) {
            return Some(data.clone());
        }
        while let Some(line) = rx.recv().await {
            let event: Value = serde_json::from_str(&line).unwrap();
            let (topic, data) = (
                event["params"]["topic"].as_str().unwrap().to_string(),
                event["params"]["data"].clone(),
            );
            seen.push((topic, data.clone()));
            if f(&data) {
                return Some(data);
            }
        }
        None
    }

    /// Every event of `topic` (read before or now) up to the first for which `f` holds.
    async fn events_until(&self, topic: &str, f: impl Fn(&Value) -> bool) -> Vec<Value> {
        let stop = |t: &str, d: &Value| t == topic && f(d);
        let mut guard = self.rx.lock().await;
        let (rx, seen) = &mut *guard;
        while !seen.iter().any(|(t, d)| stop(t, d)) {
            let Some(line) = rx.recv().await else {
                break;
            };
            let event: Value = serde_json::from_str(&line).unwrap();
            seen.push((
                event["params"]["topic"].as_str().unwrap().to_string(),
                event["params"]["data"].clone(),
            ));
        }
        let mut out = Vec::new();
        for (t, d) in seen.iter().filter(|(t, _)| t == topic) {
            out.push(d.clone());
            if stop(t, d) {
                break;
            }
        }
        out
    }

    async fn subscribe(&self, topic: &str) {
        self.events
            .subscribe(1, &[topic.to_string()], &BTreeMap::new())
            .await;
    }

    /// The workspace's jobs log lines, in file order.
    fn log_lines(&self) -> Vec<Value> {
        let dir = self.ws.as_ref().unwrap().join("queue");
        let mut lines = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("jobs.") && name.ends_with(".jsonl") {
                for line in std::fs::read_to_string(&path).unwrap().lines() {
                    lines.push(serde_json::from_str(line).unwrap());
                }
            }
        }
        lines
    }

    async fn shutdown(self) {
        let Fx {
            jobs,
            hub,
            states,
            open,
            env,
            ..
        } = self;
        drop((jobs, open));
        hub.close_all().await;
        tokio::task::spawn_blocking(move || states.close())
            .await
            .unwrap();
        drop(hub);
        drop(env);
    }
}

/// A `t.job` spec in `lane` of class `priority` with handler arguments `args`.
fn spec(lane: Lane, priority: Priority, args: Value) -> JobSpec {
    JobSpec {
        kind: "t.job",
        lane,
        priority,
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

fn gated(lane: Lane, priority: Priority, gate: &str) -> JobSpec {
    spec(lane, priority, json!({"gate": gate}))
}

fn is_job_id(id: &str) -> bool {
    id.strip_prefix("j_")
        .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------------------------
// Records, keys, statuses.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-034, CORE-117
async fn submit_returns_queued_and_persists_before_running() {
    let fx = Fx::suite().await;
    let job = fx.submit(gated(Lane::Cpu, Priority::Agent, "g")).await;
    assert!(is_job_id(&job.job_id), "{}", job.job_id);
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.substatus.as_deref(), Some("waiting"));
    assert_eq!(job.attempt, 1);
    // The row was there before the handler ran, and the handler saw it running.
    let row = fx.row(&job.job_id).await;
    assert_eq!(row.job_id, job.job_id);
    fx.handler.until_started(&job.job_id).await;
    assert_eq!(fx.handler.runs()[0].status_at_start, JobStatus::Running);
    // The topic said queued, then running, then succeeded.
    fx.handler.open("g");
    let statuses: Vec<String> = fx
        .events_until("jobs", |d| d["job"]["status"] == "succeeded")
        .await
        .iter()
        .map(|d| d["job"]["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(statuses, ["queued", "running", "succeeded"]);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-016, ARCH-035
async fn same_key_same_job_and_handler_runs_once() {
    let fx = Fx::suite().await;
    let ctx = fx.ctx_as(CallerKind::Cli, Some("k1"), "t_1");
    let first = fx
        .jobs
        .submit(&ctx, spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    assert_eq!(ctx.submitted.get(), Some(JobId(first.job_id.clone())));
    fx.finished(&first.job_id).await;
    let again = fx.ctx_as(CallerKind::Cli, Some("k1"), "t_2");
    let second = fx
        .jobs
        .submit(&again, spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    assert_eq!(second.job_id, first.job_id);
    assert_eq!(
        second.status,
        JobStatus::Succeeded,
        "the same job, as it is now"
    );
    assert_eq!(fx.handler.runs().len(), 1, "the handler ran once");
    // Other arguments under the same key are refused.
    let other = fx
        .jobs
        .submit(&again, spec(Lane::Cpu, Priority::Agent, json!({"x": 1})))
        .await
        .unwrap_err();
    assert_eq!(other.code, "idempotency.conflict");
    // Another caller kind's key is a different scope.
    let ui = fx.ctx_as(CallerKind::Ui("image".into()), Some("k1"), "t_3");
    let theirs = fx
        .jobs
        .submit(&ui, spec(Lane::Cpu, Priority::User, json!({})))
        .await
        .unwrap();
    assert_ne!(theirs.job_id, first.job_id);
    fx.finished(&theirs.job_id).await;
    fx.shutdown().await;
}

#[test] // spec: CORE-117
fn substatus_maps_to_status() {
    use Substatus as S;
    for (s, status) in [
        (S::Waiting, JobStatus::Queued),
        (S::WaitingForVram, JobStatus::Queued),
        (S::Submitting, JobStatus::Queued),
        (S::QueuedInEngine, JobStatus::Queued),
        (S::Executing, JobStatus::Running),
        (S::Ingesting, JobStatus::Running),
        (S::Retrying { attempt: 1, of: 5 }, JobStatus::Running),
        (S::Cancelling, JobStatus::Running),
        (S::InterruptedUnknown, JobStatus::Running),
        (S::Interrupted, JobStatus::Failed),
    ] {
        assert_eq!(s.status(), status, "{}", s.wire());
    }
}

#[tokio::test] // spec: CORE-009, CORE-117, ARCH-118
async fn log_line_per_state_change_with_contract_fields() {
    let fx = Fx::workspace().await;
    let ctx = fx.ctx_as(CallerKind::Cli, Some("key-log"), "t_log");
    let job = fx
        .jobs
        .submit(&ctx, spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    fx.finished(&job.job_id).await;
    let lines: Vec<Value> = fx
        .log_lines()
        .into_iter()
        .filter(|l| l["job_id"] == job.job_id.as_str())
        .collect();
    let states: Vec<(String, Value)> = lines
        .iter()
        .map(|l| {
            (
                l["status"].as_str().unwrap().to_string(),
                l["substatus"].clone(),
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            ("queued".to_string(), json!("waiting")),
            ("running".to_string(), json!("executing")),
            ("succeeded".to_string(), Value::Null),
        ]
    );
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    for line in &lines {
        let keys: Vec<&str> = line
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "machine",
                "seq",
                "job_id",
                "kind",
                "lane",
                "priority",
                "project",
                "asset_id",
                "module",
                "run_id",
                "doc_id",
                "status",
                "substatus",
                "attempt",
                "submitted",
                "updated",
                "outputs",
                "error",
                "idempotency_key",
                "trace_id",
                "usage",
                "hardware"
            ]
        );
        assert_eq!(line["kind"], "t.job");
        assert_eq!(line["idempotency_key"], "key-log");
        assert_eq!(line["trace_id"], "t_log");
        assert_eq!(line["lane"], "cpu");
        assert_eq!(line["priority"], "agent");
    }
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: ARCH-118
async fn trace_id_reaches_the_job_record() {
    let fx = Fx::suite().await;
    let ctx = fx.ctx_as(CallerKind::Cli, None, "trace-from-the-app_1");
    let job = fx
        .jobs
        .submit(&ctx, spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    assert_eq!(job.trace_id, "trace-from-the-app_1");
    let done = fx.finished(&job.job_id).await;
    assert_eq!(done.trace_id, "trace-from-the-app_1");
    assert_eq!(fx.row(&job.job_id).await.trace_id, "trace-from-the-app_1");
    assert_eq!(fx.handler.runs()[0].trace_id, "trace-from-the-app_1");
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Recovery (ARCH-022, QA-007, Review Focus 1).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-022, QA-007, CORE-128
async fn startup_marks_unfinished_jobs_interrupted_and_runs_nothing() {
    let fx = Fx::suite().await;
    // An earlier core: a finished job, a running one, one queued behind it, and an engine job
    // with external side effects stuck in `submitting` (its handler never moves on).
    let done = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.finished(&done.job_id).await;
    let running = fx.submit(gated(Lane::Cpu, Priority::Agent, "never")).await;
    let queued = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let mut external = gated(Lane::GpuHeavy, Priority::Agent, "never");
    external.engine = Some(EngineKind::ComfyUi);
    external.external_side_effects = true;
    let external = fx.submit(external).await;
    fx.handler.until_started(&running.job_id).await;
    fx.handler.until_started(&external.job_id).await;
    assert_eq!(
        fx.row(&external.job_id).await.substatus,
        Some(Substatus::Submitting)
    );
    let runs_before = fx.handler.runs().len();

    // The core dies; a new one starts on the same state.db with a handler of its own.
    let restarted = Jobs::new(
        fx.states.clone(),
        fx.hub.clone(),
        fx.events.clone(),
        fx.clock.clone(),
        Arc::new(SeqIds::new()),
        fx.failpoints.clone(),
    );
    let fresh = Arc::new(TestHandler::default());
    restarted.register(fresh.clone());
    let mut marked: Vec<String> = restarted
        .recover(fx.states.suite())
        .await
        .unwrap()
        .into_iter()
        .map(|id| id.0)
        .collect();
    marked.sort();
    let mut want = vec![
        running.job_id.clone(),
        queued.job_id.clone(),
        external.job_id.clone(),
    ];
    want.sort();
    assert_eq!(marked, want);
    for id in [&running.job_id, &queued.job_id] {
        let row = fx.row(id).await;
        assert_eq!(
            (row.status, row.substatus),
            (JobStatus::Failed, Some(Substatus::Interrupted)),
            "{id}"
        );
        assert!(row.finished_utc.is_some());
    }
    let row = fx.row(&external.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Running, Some(Substatus::InterruptedUnknown))
    );
    // A job that finished stays as it was; nothing runs again, now or on a second recovery.
    assert_eq!(fx.row(&done.job_id).await.status, JobStatus::Succeeded);
    assert!(
        restarted
            .recover(fx.states.suite())
            .await
            .unwrap()
            .is_empty()
    );
    tokio::task::yield_now().await;
    assert!(fresh.runs().is_empty(), "{:?}", fresh.runs());
    assert!(!restarted.busy());
    assert_eq!(fx.handler.runs().len(), runs_before);
    // `interrupted_unknown` is never retried.
    let e = restarted
        .retry(&fx.ctx(), &JobId(external.job_id.clone()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.not_retryable");
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Lanes and the queue (CORE-121, CORE-123–125, ARCH-071).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: CORE-121, ARCH-071, CORE-123
async fn one_job_per_lane_in_flight_and_lanes_independent() {
    let fx = Fx::suite().await;
    let a = fx.submit(gated(Lane::GpuHeavy, Priority::Agent, "a")).await;
    let b = fx.submit(gated(Lane::GpuHeavy, Priority::Agent, "b")).await;
    let c = fx.submit(gated(Lane::Cpu, Priority::Agent, "c")).await;
    fx.handler.until_started(&a.job_id).await;
    fx.handler.until_started(&c.job_id).await;
    assert!(fx.jobs.busy());
    assert_eq!(fx.handler.started().len(), 2, "b waits for a");
    let waiting = fx.row(&b.job_id).await;
    assert_eq!(
        (waiting.status, waiting.substatus),
        (JobStatus::Queued, Some(Substatus::Waiting))
    );
    fx.handler.open("a");
    fx.finished(&a.job_id).await;
    fx.handler.until_started(&b.job_id).await;
    fx.handler.open("b");
    fx.handler.open("c");
    fx.finished(&b.job_id).await;
    fx.finished(&c.job_id).await;
    fx.shutdown().await;
}

/// Starts `lane`'s jobs one by one: opens each started job's gate and returns the start order.
async fn start_order(fx: &Fx, count: usize) -> Vec<String> {
    for n in 1..=count {
        fx.handler.until(|s| s.runs.len() >= n).await;
        let job = fx.handler.runs()[n - 1].job.clone();
        fx.handler.open(&job);
        fx.finished(&job).await;
    }
    fx.handler.started()
}

/// A `gpu-heavy` job gated on its own id (the gate is opened by [`start_order`]).
async fn self_gated(fx: &Fx, priority: Priority, group: Option<&str>) -> String {
    let mut s = spec(Lane::GpuHeavy, priority, json!({"gate_self": true}));
    s.group = group.map(str::to_string);
    fx.submit(s).await.job_id
}

#[tokio::test(start_paused = true)] // spec: CORE-124
async fn class_rank_then_fifo() {
    let fx = Fx::suite().await;
    let a1 = self_gated(&fx, Priority::Agent, None).await;
    fx.handler.until_started(&a1).await;
    let a2 = self_gated(&fx, Priority::Agent, None).await;
    let u1 = self_gated(&fx, Priority::User, None).await;
    let i1 = self_gated(&fx, Priority::Interactive, None).await;
    let a3 = self_gated(&fx, Priority::Agent, None).await;
    let u2 = self_gated(&fx, Priority::User, None).await;
    // Interactive goes after the running job, before everything else.
    assert_eq!(start_order(&fx, 6).await, [a1, i1, u1, u2, a2, a3]);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-124
async fn grouping_never_overtakes_a_job_more_than_8_times() {
    let fx = Fx::suite().await;
    let g0 = self_gated(&fx, Priority::Agent, Some("a")).await;
    fx.handler.until_started(&g0).await;
    let x = self_gated(&fx, Priority::Agent, Some("b")).await;
    let mut grouped = Vec::new();
    for _ in 0..12 {
        grouped.push(self_gated(&fx, Priority::Agent, Some("a")).await);
    }
    let order = start_order(&fx, 14).await;
    let mut want = vec![g0];
    want.extend(grouped[..8].iter().cloned());
    want.push(x);
    want.extend(grouped[8..].iter().cloned());
    assert_eq!(order, want);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-125
async fn preemption_cancels_only_agent_prompts_when_enabled() {
    let fx = Fx::suite().await;
    let engine_job = |priority: Priority, gate: &str| {
        let mut s = gated(Lane::GpuHeavy, priority, gate);
        s.engine = Some(EngineKind::ComfyUi);
        s
    };
    // Off (the default): an interactive job waits for the running agent job.
    let agent = fx.submit(engine_job(Priority::Agent, "agent")).await;
    fx.handler.until_started(&agent.job_id).await;
    let first = fx
        .submit(gated(Lane::GpuHeavy, Priority::Interactive, "first"))
        .await;
    tokio::task::yield_now().await;
    assert!(fx.handler.engine_cancels().is_empty());
    fx.handler.open("agent");
    fx.finished(&agent.job_id).await;
    fx.handler.until_started(&first.job_id).await;
    fx.handler.open("first");
    fx.finished(&first.job_id).await;

    // On: the running agent engine job is cancelled in its engine and requeued at the head.
    fx.jobs.set_preempt_agent_batches(true);
    let batch = fx.submit(engine_job(Priority::Agent, "batch")).await;
    fx.handler.until_started(&batch.job_id).await;
    let later = fx.submit(engine_job(Priority::Agent, "later")).await;
    let urgent = fx
        .submit(gated(Lane::GpuHeavy, Priority::Interactive, "urgent"))
        .await;
    fx.handler.until_started(&urgent.job_id).await;
    assert_eq!(
        fx.handler.engine_cancels(),
        std::slice::from_ref(&batch.job_id)
    );
    let requeued = fx.row(&batch.job_id).await;
    assert_eq!(requeued.status, JobStatus::Queued);
    assert_eq!(requeued.attempt, 1, "a preemption is not a failed attempt");
    fx.handler.open("urgent");
    fx.finished(&urgent.job_id).await;
    // The preempted batch goes before the agent job queued after it.
    fx.handler
        .until(|s| s.runs.iter().filter(|r| r.job == batch.job_id).count() == 2)
        .await;
    fx.handler.open("batch");
    assert_eq!(
        fx.finished(&batch.job_id).await.status,
        JobStatus::Succeeded
    );
    fx.handler.until_started(&later.job_id).await;
    fx.handler.open("later");
    fx.finished(&later.job_id).await;

    // Never a `user` job.
    let mine = fx.submit(engine_job(Priority::User, "mine")).await;
    fx.handler.until_started(&mine.job_id).await;
    let again = fx
        .submit(gated(Lane::GpuHeavy, Priority::Interactive, "again"))
        .await;
    tokio::task::yield_now().await;
    assert_eq!(fx.handler.engine_cancels().len(), 1);
    fx.handler.open("mine");
    assert_eq!(fx.finished(&mine.job_id).await.status, JobStatus::Succeeded);
    fx.handler.open("again");
    fx.finished(&again.job_id).await;
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Retries and the preflight (CORE-128, QA-004).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: CORE-128, QA-004
async fn transient_backoff_1_2_4_capped_30_five_attempts_with_events() {
    let fx = Fx::suite().await;
    let job = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"fail": "retryable_always"}),
        ))
        .await;
    let topic = format!("job:{}", job.job_id);
    fx.subscribe(&topic).await;
    let end = fx.finished(&job.job_id).await;
    assert_eq!(end.status, JobStatus::Failed);
    assert_eq!(end.attempt, 5);
    assert_eq!(end.error.as_ref().unwrap().code, "store.io");
    let runs = fx.handler.runs();
    assert_eq!(
        runs.iter().map(|r| r.attempt).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    let gaps: Vec<u64> = runs
        .windows(2)
        .map(|w| (w[1].at - w[0].at).as_secs())
        .collect();
    assert_eq!(gaps, [1, 2, 4, 8]);
    assert_eq!(backoff(6), MAX_BACKOFF);
    assert_eq!(MAX_BACKOFF, Duration::from_secs(30));

    // A retry that works: `retrying` 1/5 and 2/5 on the job's topic, then success on attempt 3.
    let flaky = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"fail": "retryable"}),
        ))
        .await;
    let topic = format!("job:{}", flaky.job_id);
    fx.subscribe(&topic).await;
    let events = fx
        .events_until(&topic, |d| {
            d["kind"] == "status" && d["job"]["status"] == "succeeded"
        })
        .await;
    let retrying: Vec<(u64, u64)> = events
        .iter()
        .filter(|d| d["kind"] == "retrying")
        .map(|d| (d["attempt"].as_u64().unwrap(), d["of"].as_u64().unwrap()))
        .collect();
    assert_eq!(retrying, [(1, 5), (2, 5)]);
    let last = events.last().unwrap();
    assert_eq!(last["job"]["attempt"], 3);
    assert!(last["job"].get("error").is_none(), "{last}");
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-128, QA-004
async fn disk_full_is_final() {
    let fx = Fx::suite().await;
    let job = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"fail": "disk_full"}),
        ))
        .await;
    let end = fx.finished(&job.job_id).await;
    assert_eq!(end.status, JobStatus::Failed);
    assert_eq!(end.attempt, 1);
    assert_eq!(end.error.unwrap().code, "store.disk_full");
    assert_eq!(fx.handler.runs().len(), 1);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-128
async fn disk_preflight_refuses_before_submitting() {
    let fx = Fx::suite().await;
    let mut big = spec(Lane::GpuHeavy, Priority::Agent, json!({}));
    big.engine = Some(EngineKind::ComfyUi);
    // Twice this plus 64 MiB is more than any volume has.
    big.expected_output_bytes = u64::MAX / 4;
    let job = fx.submit(big).await;
    let end = fx.finished(&job.job_id).await;
    assert_eq!(end.status, JobStatus::Failed);
    let error = end.error.unwrap();
    assert_eq!(error.code, "store.disk_full");
    assert!(error.data.unwrap()["need_bytes"].as_u64().unwrap() > 0);
    assert!(fx.handler.runs().is_empty(), "the handler never ran");
    // It never reached `submitting`.
    assert_eq!(fx.row(&job.job_id).await.started_utc, None);
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// The job tools' operations (AI-113, CORE-146).
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: AI-113
async fn job_wait_returns_at_timeout_or_completion() {
    let fx = Fx::suite().await;
    let job = fx.submit(gated(Lane::Cpu, Priority::Agent, "w")).await;
    let id = JobId(job.job_id.clone());
    fx.handler.until_started(&job.job_id).await;
    let (still, done) = fx
        .jobs
        .wait(&fx.ctx(), &id, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(!done);
    assert_eq!(still.status, JobStatus::Running);
    // A wait that is going on when the job ends returns with it.
    let waiting = {
        let (jobs, ctx, id) = (fx.jobs.clone(), fx.ctx(), id.clone());
        tokio::spawn(async move { jobs.wait(&ctx, &id, Duration::from_secs(90)).await })
    };
    fx.handler.open("w");
    let (end, done) = waiting.await.unwrap().unwrap();
    assert!(done, "{end:?}");
    assert_eq!(end.status, JobStatus::Succeeded);
    let unknown = fx
        .jobs
        .wait(
            &fx.ctx(),
            &JobId("j_ffffffffffffffff".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code, "job.unknown");
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: AI-113
async fn timeout_over_90_is_params_invalid() {
    let fx = Fx::suite().await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.finished(&job.job_id).await;
    let id = JobId(job.job_id.clone());
    for bad in [Duration::from_secs(91), Duration::ZERO] {
        let e = fx.jobs.wait(&fx.ctx(), &id, bad).await.unwrap_err();
        assert_eq!(e.code, "params.invalid", "{bad:?}");
    }
    let (end, done) = fx
        .jobs
        .wait(&fx.ctx(), &id, Duration::from_secs(90))
        .await
        .unwrap();
    assert!(done);
    assert_eq!(end.status, JobStatus::Succeeded);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-146, ARCH-021
async fn cancel_queued_is_immediate_running_goes_through_the_handler() {
    let fx = Fx::suite().await;
    let log = Arc::new(Mutex::new(Vec::new()));
    fx.jobs.add_completion(Arc::new(Logging {
        name: "meter",
        log: log.clone(),
    }));
    let running = fx.submit(gated(Lane::Cpu, Priority::Agent, "r")).await;
    let queued = fx.submit(gated(Lane::Cpu, Priority::Agent, "q")).await;
    fx.handler.until_started(&running.job_id).await;

    let q = fx
        .jobs
        .cancel(&fx.ctx(), &JobId(queued.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(q.status, JobStatus::Cancelled);
    assert!(q.error.is_none(), "a cancelled job has no error");

    let r = fx
        .jobs
        .cancel(&fx.ctx(), &JobId(running.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(r.status, JobStatus::Running);
    assert_eq!(r.substatus.as_deref(), Some("cancelling"));
    assert_eq!(
        fx.handler.engine_cancels(),
        std::slice::from_ref(&running.job_id)
    );
    // A second cancel changes nothing and doesn't reach the engine again.
    fx.jobs
        .cancel(&fx.ctx(), &JobId(running.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(fx.handler.engine_cancels().len(), 1);
    assert_eq!(
        fx.finished(&running.job_id).await.status,
        JobStatus::Cancelled
    );
    // A finished job is returned as it is.
    let again = fx
        .jobs
        .cancel(&fx.ctx(), &JobId(running.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(again.status, JobStatus::Cancelled);
    assert_eq!(fx.handler.engine_cancels().len(), 1);
    // The queued one never ran; each job's completions ran once.
    assert_eq!(fx.handler.started(), std::slice::from_ref(&running.job_id));
    let mut lines = log.lock().unwrap().clone();
    lines.sort();
    let mut want = vec![
        format!("meter {} job.cancelled done=false", queued.job_id),
        format!("meter {} job.cancelled done=false", running.job_id),
    ];
    want.sort();
    assert_eq!(lines, want);
    let unknown = fx
        .jobs
        .cancel(&fx.ctx(), &JobId("j_ffffffffffffffff".into()))
        .await;
    assert_eq!(unknown.unwrap_err().code, "job.unknown");
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-146
async fn retry_only_failed_or_cancelled() {
    let fx = Fx::suite().await;
    let failed = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"fail": "disk_full"}),
        ))
        .await;
    fx.finished(&failed.job_id).await;
    let ok = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.finished(&ok.job_id).await;
    let running = fx
        .submit(gated(Lane::GpuHeavy, Priority::Agent, "hold"))
        .await;
    fx.handler.until_started(&running.job_id).await;

    let ctx = fx.ctx_as(CallerKind::Cli, Some("retry-1"), "t_retry");
    let retried = fx
        .jobs
        .retry(&ctx, &JobId(failed.job_id.clone()))
        .await
        .unwrap();
    assert_ne!(retried.job_id, failed.job_id);
    assert_eq!(retried.retry_of.as_deref(), Some(failed.job_id.as_str()));
    assert_eq!(retried.status, JobStatus::Queued);
    assert_eq!(ctx.submitted.get(), Some(JobId(retried.job_id.clone())));
    assert_eq!(fx.finished(&retried.job_id).await.status, JobStatus::Failed);
    for id in [&ok.job_id, &running.job_id] {
        let e = fx
            .jobs
            .retry(&fx.ctx(), &JobId(id.clone()))
            .await
            .unwrap_err();
        assert_eq!(e.code, "job.not_retryable", "{id}");
    }
    let e = fx
        .jobs
        .retry(&fx.ctx(), &JobId("j_ffffffffffffffff".into()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.unknown");
    // A cancelled job can be retried; an `interrupted_unknown` one can't.
    fx.jobs
        .cancel(&fx.ctx(), &JobId(running.job_id.clone()))
        .await
        .unwrap();
    fx.finished(&running.job_id).await;
    let again = fx
        .jobs
        .retry(&fx.ctx(), &JobId(running.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(again.retry_of.as_deref(), Some(running.job_id.as_str()));
    fx.handler.open("hold");
    fx.finished(&again.job_id).await;
    let unknown = fx
        .submit(spec(Lane::Cpu, Priority::Agent, json!({"fail": "unknown"})))
        .await;
    fx.event_until(|d| {
        d["job"]["job_id"] == unknown.job_id.as_str()
            && d["job"]["substatus"] == "interrupted_unknown"
    })
    .await;
    let e = fx
        .jobs
        .retry(&fx.ctx(), &JobId(unknown.job_id.clone()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.not_retryable");
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Completion order (CORE-015 step 7, ARCH-075).
// ---------------------------------------------------------------------------------------------

#[tokio::test] // spec: CORE-015, ARCH-075
async fn completion_runs_meter_then_caps_then_record_in_write_order() {
    let fx = Fx::workspace().await;
    let log = Arc::new(Mutex::new(Vec::new()));
    fx.jobs.add_completion(Arc::new(Logging {
        name: "meter",
        log: log.clone(),
    }));
    fx.jobs.add_completion(Arc::new(Logging {
        name: "caps",
        log: log.clone(),
    }));
    let record_log = log.clone();
    fx.failpoints.on(
        "store.after_manifest",
        Box::new(move || {
            record_log.lock().unwrap().push("record".to_string());
            Ok(())
        }),
    );
    let job = fx
        .submit(spec(Lane::Cpu, Priority::Agent, json!({"record": true})))
        .await;
    let end = fx.finished(&job.job_id).await;
    assert_eq!(end.status, JobStatus::Succeeded, "{end:?}");
    assert_eq!(
        *log.lock().unwrap(),
        [
            format!("meter {} ok done=true", job.job_id),
            format!("caps {} ok done=true", job.job_id),
            "record".to_string(),
        ]
    );
    let result = end.result.unwrap();
    assert_eq!(result.structured["ran"], 1);
    assert!(result.structured["handoff"].is_object(), "{result:?}");
    let manifest = fx
        .ws
        .as_ref()
        .unwrap()
        .join(r"projects\sketchbook\assets\prop_jobrec\prop_jobrec.manifest.json");
    assert!(manifest.is_file(), "the Record step wrote the manifest");
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// The lifecycle (ARCH-020, ARCH-021).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-020, ARCH-021
async fn jobs_blocker_holds_idle_and_drain() {
    let fx = Fx::suite().await;
    let lifecycle = Lifecycle::new(fx.clock.clone(), Duration::from_secs(10));
    lifecycle.add_blocker(fx.jobs.clone());
    let job = fx.submit(gated(Lane::Cpu, Priority::Agent, "idle")).await;
    fx.handler.until_started(&job.job_id).await;
    for _ in 0..3 {
        fx.clock.advance(Duration::from_secs(60));
        assert!(
            !lifecycle.drain_if_idle(),
            "a running job holds the idle exit"
        );
    }
    fx.handler.open("idle");
    fx.finished(&job.job_id).await;
    while fx.jobs.busy() {
        tokio::task::yield_now().await;
    }
    assert!(!lifecycle.drain_if_idle(), "the idle period starts now");
    fx.clock.advance(Duration::from_secs(11));
    assert!(lifecycle.drain_if_idle());

    // A requested drain waits for a running job.
    let lifecycle = Lifecycle::new(fx.clock.clone(), Duration::from_secs(600));
    lifecycle.add_blocker(fx.jobs.clone());
    let job = fx.submit(gated(Lane::Cpu, Priority::Agent, "drain")).await;
    fx.handler.until_started(&job.job_id).await;
    lifecycle.begin_drain(DrainReason::Requested);
    let shutdown = {
        let lifecycle = lifecycle.clone();
        tokio::spawn(async move { lifecycle.run_shutdown().await })
    };
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!shutdown.is_finished(), "the drain waits for the job");
    }
    fx.handler.open("drain");
    fx.finished(&job.job_id).await;
    let report = shutdown.await.unwrap();
    assert_eq!(report.abandoned_calls, 0);
    fx.shutdown().await;
}

/// The engine-cancel step of a shutdown, recording the job's state when it runs.
struct CancelEnginePrompts {
    seen: Arc<Mutex<Vec<String>>>,
    db: Arc<lil_core::state::StateDb>,
    job: String,
}

impl ShutdownStep for CancelEnginePrompts {
    fn phase(&self) -> ShutdownPhase {
        ShutdownPhase::CancelEnginePrompts
    }

    fn name(&self) -> &'static str {
        "test.cancel_engine_prompts"
    }

    fn run(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let row = record::get(&self.db, &self.job).await.unwrap().unwrap();
            self.seen
                .lock()
                .unwrap()
                .push(format!("cancel_engine {}", row.status.wire()));
        })
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-021
async fn shutdown_persists_interrupted_after_engine_cancel() {
    let fx = Fx::suite().await;
    let lifecycle = Lifecycle::new(fx.clock.clone(), Duration::from_secs(600));
    lifecycle.add_blocker(fx.jobs.clone());
    // Registered before the engine step: the phases decide the order.
    lifecycle.add_shutdown_step(fx.jobs.persist_interrupted_step());
    let running = fx.submit(gated(Lane::Cpu, Priority::Agent, "stuck")).await;
    let queued = fx.submit(gated(Lane::Cpu, Priority::Agent, "behind")).await;
    fx.handler.until_started(&running.job_id).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    lifecycle.add_shutdown_step(Arc::new(CancelEnginePrompts {
        seen: seen.clone(),
        db: fx.states.suite_db(),
        job: running.job_id.clone(),
    }));
    // A fatal drain waits 5 s for the job, then goes on.
    lifecycle.begin_drain(DrainReason::Fatal("test".into()));
    let report = lifecycle.run_shutdown().await;
    assert_eq!(
        report.steps,
        ["test.cancel_engine_prompts", "jobs.persist_interrupted"]
    );
    assert_eq!(*seen.lock().unwrap(), ["cancel_engine running"]);
    for id in [&running.job_id, &queued.job_id] {
        let row = fx.row(id).await;
        assert_eq!(
            (row.status, row.substatus),
            (JobStatus::Failed, Some(Substatus::Interrupted)),
            "{id}"
        );
    }
    assert!(!fx.jobs.busy());
    // Nothing starts after the step.
    let late = fx
        .jobs
        .submit(&fx.ctx(), spec(Lane::Cpu, Priority::Agent, json!({})))
        .await;
    assert_eq!(late.unwrap_err().code, "core.draining");
    assert_eq!(fx.handler.started(), std::slice::from_ref(&running.job_id));
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// The job tools through a running core.
// ---------------------------------------------------------------------------------------------

/// `test.job` and the job tools over the pipe of an in-process core: the server registers the
/// test handler and the jobs, and one key makes one job (the pipe calls' deadlines are failure
/// deadlines).
#[test] // spec: ARCH-034, AI-113, CORE-016, CORE-146
fn job_tools_run_through_the_core() {
    let env = TestEnv::new();
    let _core = lil_core::start_in_thread(lil_core::CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..lil_core::CoreConfig::for_env(env.env.clone()).unwrap()
    });
    let mut pipe = lil_testkit::RawPipe::connect(&env);
    assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
    let args = json!({"lane": "cpu", "gated": true});
    let job = pipe.tool_ok("test.job", args.clone(), Some("k1"))["job"].clone();
    let id = job["job_id"].as_str().unwrap().to_string();
    assert!(is_job_id(&id), "{job}");
    assert_eq!(job["status"], "queued");
    let again = pipe.tool_ok("test.job", args, Some("k1"));
    assert_eq!(again["job"]["job_id"], id.as_str(), "one key, one job");

    let waited = pipe.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 1}), None);
    assert_eq!(waited["done"], false, "{waited}");
    let refused = pipe.tool("job.wait", json!({"job_id": id, "timeout_s": 91}), None);
    assert_eq!(refused["error"]["code"], "params.invalid", "{refused}");
    pipe.tool_ok("test.job_release", json!({"job_id": id}), Some("k2"));
    let waited = pipe.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 30}), None);
    assert_eq!(waited["done"], true, "{waited}");
    assert_eq!(waited["job"]["status"], "succeeded");
    assert_eq!(waited["job"]["result"]["structured"]["attempt"], 1);

    let listed = pipe.tool_ok("jobs.list", json!({}), None);
    assert_eq!(listed["jobs"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["next_cursor"], Value::Null);
    let retried = pipe.tool("job.retry", json!({"job_id": id}), Some("k3"));
    assert_eq!(retried["error"]["code"], "job.not_retryable", "{retried}");
    let cancelled = pipe.tool_ok("job.cancel", json!({"job_id": id}), Some("k4"));
    assert_eq!(
        cancelled["job"]["status"], "succeeded",
        "a finished job stays as it is"
    );
}
