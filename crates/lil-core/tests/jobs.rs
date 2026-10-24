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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::jobs::record::{self, JobRow};
use lil_core::jobs::{
    Admission, Admit, COMPLETION_GRACE, CancelSelector, EngineKind, JobCaller, JobCompletion,
    JobCtx, JobDone, JobFailure, JobFilter, JobFuture, JobHandler, JobId, JobOrigin, JobSpec,
    JobStatus, JobSummary, Jobs, Lane, MAX_BACKOFF, MoveTo, PauseReason, Priority, Reservations,
    SubmittedSlot, Substatus, backoff, cancelled_error,
};
use lil_core::lifecycle::{DrainReason, Lifecycle, ShutdownPhase, ShutdownStep};
use lil_core::pipeline::{RecordBranch, RecordPlan, start_candidate_run};
use lil_core::state::StateRegistry;
use lil_core::store::{CommitOp, FailPoints, ManifestEdit, WsPath};
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
/// cancel; `gate_self: true` gates on the job's own id), `on_cancel` (what a gated attempt
/// that sees its cancel returns: `unknown`, `interrupted`, else the cancelled error), `fail`
/// (`retryable` on attempts 1–2, `retryable_always`, `disk_full`, `unknown`, `interrupted`),
/// `record` (leave a candidate-run record plan), `in_engine: true` (the attempt shows
/// `queued_in_engine` first), `progress_on` (a gate: once it opens, a gated attempt reports
/// one progress).
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
        let error = |spec: &lil_protocol::CodeSpec| {
            ErrorBody::new(spec, "on purpose", job.trace_id.clone())
        };
        let interrupted = || JobFailure::Interrupted {
            error: error(&codes::COMFYUI_UNREACHABLE),
            salvaged: Vec::new(),
        };
        let gate = match args.get("gate").and_then(Value::as_str) {
            Some(gate) => Some(gate.to_string()),
            None => (args.get("gate_self") == Some(&json!(true))).then(|| job.id.to_string()),
        };
        if args.get("in_engine") == Some(&json!(true)) {
            job.set_substatus(Substatus::QueuedInEngine).await;
        }
        if let Some(gate) = gate {
            let mut changed = self.changed.subscribe();
            let mut cancel = job.cancel.clone();
            let progress_on = args.get("progress_on").and_then(Value::as_str);
            let mut reported = false;
            loop {
                if self.seen.lock().unwrap().open_gates.contains(&gate) {
                    break;
                }
                if !reported
                    && progress_on.is_some_and(|p| self.seen.lock().unwrap().open_gates.contains(p))
                {
                    reported = true;
                    job.progress(1, 2, None);
                }
                if *cancel.borrow() {
                    return Err(match args.get("on_cancel").and_then(Value::as_str) {
                        Some("unknown") => JobFailure::Unknown(error(&codes::STORE_IO)),
                        Some("interrupted") => interrupted(),
                        _ => JobFailure::Final(cancelled_error(&job.trace_id)),
                    });
                }
                tokio::select! {
                    _ = changed.changed() => {}
                    _ = cancel.changed() => {}
                }
            }
        }
        match args.get("fail").and_then(Value::as_str) {
            Some("retryable") if job.attempt <= 2 => {
                return Err(JobFailure::Retryable(error(&codes::STORE_IO)));
            }
            Some("retryable_always") => return Err(JobFailure::Retryable(error(&codes::STORE_IO))),
            Some("disk_full") => return Err(JobFailure::Final(error(&codes::STORE_DISK_FULL))),
            Some("unknown") => return Err(JobFailure::Unknown(error(&codes::STORE_IO))),
            Some("interrupted") => return Err(interrupted()),
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

/// Runs director tasks (`assistant_task`, CORE-118) as [`TestHandler`] runs `t.job`: a restart
/// never holds them (ARCH-022 until the director plan).
struct Director(Arc<TestHandler>);

impl JobHandler for Director {
    fn kind(&self) -> &'static str {
        "assistant_task"
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(self.0.attempt(job))
    }

    fn cancel_in_engine<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, ()> {
        self.0.cancel_in_engine(job)
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

/// A completion that logs, for each job, the reservation it got, the outcome's code and
/// whether its cancel flag was set.
struct Reserving {
    log: Arc<Mutex<Ends>>,
}

impl JobCompletion for Reserving {
    fn completed<'a>(
        &'a self,
        job: &'a JobCtx,
        outcome: &'a Outcome<ToolOutput>,
        _done: Option<&'a JobDone>,
    ) -> JobFuture<'a, ()> {
        let code = match outcome {
            Outcome::Ok(_) => "ok".to_string(),
            Outcome::Error { error } => error.code.clone(),
            Outcome::PendingConfirmation(_) => "pending".to_string(),
        };
        self.log.lock().unwrap().push((
            job.id.to_string(),
            job.reservation.clone(),
            code,
            *job.cancel.borrow(),
        ));
        Box::pin(std::future::ready(()))
    }
}

/// Records what happens to reservations.
#[derive(Default)]
struct HandOvers(Mutex<Vec<String>>);

impl HandOvers {
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl Reservations for HandOvers {
    fn hand_over<'a>(
        &'a self,
        reservation: &'a str,
        from: &'a str,
        to: &'a JobId,
    ) -> JobFuture<'a, ()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("hand_over {reservation} {from} {to}"));
        Box::pin(std::future::ready(()))
    }

    fn release<'a>(&'a self, reservation: &'a str, owner: &'a str) -> JobFuture<'a, ()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("release {reservation} {owner}"));
        Box::pin(std::future::ready(()))
    }
}

/// A completion that says when it begins and waits until it is let go.
struct Blocking {
    entered: watch::Sender<bool>,
    go: watch::Sender<bool>,
    finished: Arc<Mutex<Vec<String>>>,
}

impl Blocking {
    fn new() -> Arc<Blocking> {
        Arc::new(Blocking {
            entered: watch::Sender::new(false),
            go: watch::Sender::new(false),
            finished: Arc::default(),
        })
    }
}

impl JobCompletion for Blocking {
    fn completed<'a>(
        &'a self,
        job: &'a JobCtx,
        _outcome: &'a Outcome<ToolOutput>,
        _done: Option<&'a JobDone>,
    ) -> JobFuture<'a, ()> {
        Box::pin(async move {
            self.entered.send_replace(true);
            let mut go = self.go.subscribe();
            let _ = go.wait_for(|g| *g).await;
            self.finished.lock().unwrap().push(job.id.to_string());
        })
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

    /// A caller of kind `caller` in the fixture's scope, with idempotency key `key`.
    fn ctx_as(&self, caller: CallerKind, key: Option<&str>, trace_id: &str) -> JobCaller {
        let ws = self
            .open
            .as_ref()
            .map_or("none".to_string(), |o| o.ws.id.clone());
        JobCaller {
            idem_scope: format!("{caller}|{ws}"),
            caller,
            workspace: self.open.clone(),
            trace_id: trace_id.to_string(),
            idem_key: key.map(str::to_string),
            call_id: "c_0000000000000001".into(),
            reservation: None,
            submitted: Arc::new(SubmittedSlot::new()),
            origin: Some(JobOrigin {
                tool: "t.submit".into(),
                arguments: json!({}),
            }),
            retry_of: None,
        }
    }

    fn ctx(&self) -> JobCaller {
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
    // The topic said queued, then running, then (its result written before its completions,
    // module docs "Crash windows") ingesting, then succeeded.
    fx.handler.open("g");
    let states: Vec<(String, Value)> = fx
        .events_until("jobs", |d| d["job"]["status"] == "succeeded")
        .await
        .iter()
        .map(|d| {
            (
                d["job"]["status"].as_str().unwrap().to_string(),
                d["job"]["substatus"].clone(),
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            ("queued".to_string(), json!("waiting")),
            ("running".to_string(), json!("executing")),
            ("running".to_string(), json!("ingesting")),
            ("succeeded".to_string(), Value::Null),
        ]
    );
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
    // Other arguments under the same key are refused (a new call: one call submits one job).
    let other = fx
        .jobs
        .submit(
            &fx.ctx_as(CallerKind::Cli, Some("k1"), "t_2b"),
            spec(Lane::Cpu, Priority::Agent, json!({"x": 1})),
        )
        .await
        .unwrap_err();
    assert_eq!(other.code, "idempotency.conflict");
    // M6: one call submits at most one job (it hands its one reservation to it).
    let second = fx
        .jobs
        .submit(&ctx, spec(Lane::Cpu, Priority::Agent, json!({"y": 1})))
        .await
        .unwrap_err();
    assert_eq!(second.code, "core.bug");
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
            ("running".to_string(), json!("ingesting")),
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
                "origin",
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
        assert_eq!(line["origin"], json!({"surface": "cli"}));
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

/// Plan 4a (P4a-R41, Appendix G "Restart hold"): the job queued behind the running one never
/// reached an engine, so it is held (`queue_paused`, `pause_reasons: ["restart"]`) instead of
/// interrupted; the running job and the engine job are interrupted as before.
#[tokio::test(start_paused = true)] // spec: ARCH-022, QA-007, CORE-128, CORE-160
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
        .recover_suite()
        .await
        .unwrap()
        .into_iter()
        .map(|id| id.0)
        .collect();
    marked.sort();
    let mut want = vec![running.job_id.clone(), external.job_id.clone()];
    want.sort();
    assert_eq!(marked, want);
    let row = fx.row(&running.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Failed, Some(Substatus::Interrupted))
    );
    assert!(row.finished_utc.is_some());
    // The queued job is held by the restart, not interrupted.
    let held = restarted
        .get(&fx.ctx(), &JobId(queued.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(
        (held.status, held.substatus.as_deref()),
        (JobStatus::Queued, Some("queue_paused"))
    );
    assert_eq!(held.pause_reasons, ["restart"]);
    assert!(held.finished.is_none());
    let row = fx.row(&external.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Running, Some(Substatus::InterruptedUnknown))
    );
    // A job that finished stays as it was; nothing runs again, now or on a second recovery.
    assert_eq!(fx.row(&done.job_id).await.status, JobStatus::Succeeded);
    assert!(restarted.recover_suite().await.unwrap().is_empty());
    tokio::task::yield_now().await;
    assert!(fresh.runs().is_empty(), "{:?}", fresh.runs());
    assert!(!restarted.busy(), "a held job doesn't keep the core up");
    assert_eq!(fx.handler.runs().len(), runs_before);
    // `interrupted_unknown` is never retried.
    let e = restarted
        .retry_origin(&fx.ctx(), &JobId(external.job_id.clone()))
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

/// Fix round 1, I3 (CORE-124, CORE-160; Appendix G "Queue order"): the order key is one
/// core-wide submission sequence, not each `state.db`'s `seq`, so jobs of the suite scope and of
/// a workspace, submitted alternately into one lane and class, start in submission order even
/// when the suite `state.db`'s own sequence runs ahead.
#[tokio::test(start_paused = true)] // spec: CORE-124, CORE-160
async fn fifo_within_a_class_holds_across_the_suite_and_workspace_scopes() {
    let fx = Fx::workspace().await;
    let suite = || {
        let mut c = fx.ctx();
        c.workspace = None;
        c.idem_scope = "cli|none".into();
        c
    };
    // The suite state.db's own sequence runs ahead of the workspace's.
    for _ in 0..5 {
        let job = fx
            .jobs
            .submit(&suite(), spec(Lane::Cpu, Priority::Agent, json!({})))
            .await
            .unwrap();
        fx.finished(&job.job_id).await;
    }
    let first = self_gated(&fx, Priority::Agent, None).await;
    fx.handler.until_started(&first).await;
    let mut want = vec![first];
    for i in 0..4 {
        let caller = if i % 2 == 0 { suite() } else { fx.ctx() };
        let job = fx
            .jobs
            .submit(
                &caller,
                spec(Lane::GpuHeavy, Priority::Agent, json!({"gate_self": true})),
            )
            .await
            .unwrap();
        want.push(job.job_id);
    }
    let skip = fx.handler.started().len() - 1;
    for n in 0..want.len() {
        fx.handler.until(|s| s.runs.len() > skip + n).await;
        let job = fx.handler.runs()[skip + n].job.clone();
        fx.handler.open(&job);
        fx.finished(&job).await;
    }
    assert_eq!(fx.handler.started()[skip..], want[..]);
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
    let flags = reserving(&fx);
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
    // M9: a completion's context is never cancelled itself (completions read the outcome).
    let flags = seen(&flags);
    assert_eq!(flags.len(), 2);
    assert!(flags.iter().all(|(_, _, _, cancel)| !cancel), "{flags:?}");
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

    // A failed job gives back the tool call that made it, for `job.retry` to dispatch again.
    let origin = fx
        .jobs
        .retry_origin(&fx.ctx(), &JobId(failed.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(
        origin.origin,
        JobOrigin {
            tool: "t.submit".into(),
            arguments: json!({})
        }
    );
    assert_eq!(origin.project, None, "a suite job has no project");
    for id in [&ok.job_id, &running.job_id] {
        let e = fx
            .jobs
            .retry_origin(&fx.ctx(), &JobId(id.clone()))
            .await
            .unwrap_err();
        assert_eq!(e.code, "job.not_retryable", "{id}");
    }
    let e = fx
        .jobs
        .retry_origin(&fx.ctx(), &JobId("j_ffffffffffffffff".into()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.unknown");
    // A cancelled job can be retried; an `interrupted_unknown` one can't, nor one no tool call
    // made.
    fx.jobs
        .cancel(&fx.ctx(), &JobId(running.job_id.clone()))
        .await
        .unwrap();
    fx.finished(&running.job_id).await;
    fx.jobs
        .retry_origin(&fx.ctx(), &JobId(running.job_id.clone()))
        .await
        .unwrap();
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
        .retry_origin(&fx.ctx(), &JobId(unknown.job_id.clone()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.not_retryable");
    let mut toolless = fx.ctx();
    toolless.origin = None;
    let bare = fx
        .jobs
        .submit(
            &toolless,
            spec(Lane::Cpu, Priority::Agent, json!({"fail": "disk_full"})),
        )
        .await
        .unwrap();
    fx.finished(&bare.job_id).await;
    let e = fx
        .jobs
        .retry_origin(&fx.ctx(), &JobId(bare.job_id.clone()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.not_retryable");
    fx.shutdown().await;
}

/// I3: a retry's job is in the retrying caller's class (Appendix F: `assistant`, `mcp` and
/// `cli` jobs are `agent`), whatever class the retried job had, and records `retry_of`.
#[tokio::test(start_paused = true)] // spec: CORE-124, CORE-146
async fn a_retry_runs_in_the_retrying_callers_class() {
    let fx = Fx::suite().await;
    let ui = CallerKind::Ui("image".into());
    for (caller, class, want) in [
        (CallerKind::Mcp, Priority::User, Priority::Agent),
        (CallerKind::Cli, Priority::Interactive, Priority::Agent),
        (ui.clone(), Priority::Interactive, Priority::Interactive),
        (ui.clone(), Priority::Agent, Priority::User),
    ] {
        // A job is retried once (Appendix G): a fresh failed one for each retry.
        let mine = fx
            .jobs
            .submit(
                &fx.ctx_as(ui.clone(), None, "t_ui"),
                spec(Lane::Cpu, Priority::User, json!({"fail": "disk_full"})),
            )
            .await
            .unwrap();
        fx.finished(&mine.job_id).await;
        let mut retry = fx.ctx_as(caller.clone(), None, "t_retry");
        retry.retry_of = Some(JobId(mine.job_id.clone()));
        let job = fx
            .jobs
            .submit(&retry, spec(Lane::Cpu, class, json!({})))
            .await
            .unwrap();
        assert_eq!(job.priority, want, "{caller} retrying a {class:?} job");
        assert_eq!(job.retry_of.as_deref(), Some(mine.job_id.as_str()));
        fx.finished(&job.job_id).await;
    }
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

/// The running job is marked after the engine step; the queued one never reached an engine, so
/// it is left queued for the next start's restart hold (P4a-R41).
#[tokio::test(start_paused = true)] // spec: ARCH-021, CORE-160
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
    let row = fx.row(&running.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Failed, Some(Substatus::Interrupted))
    );
    // The queued job never reached an engine: it is left queued for the next start's restart
    // hold (P4a-R41).
    let row = fx.row(&queued.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Queued, Some(Substatus::Waiting))
    );
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

// ---------------------------------------------------------------------------------------------
// Re-runs, reservations, interruptions, the drain and the crash windows.
// ---------------------------------------------------------------------------------------------

/// What [`Reserving`] logged: per job, its reservation, its outcome code, its cancel flag.
type Ends = Vec<(String, Option<String>, String, bool)>;

fn reserving(fx: &Fx) -> Arc<Mutex<Ends>> {
    let log: Arc<Mutex<Ends>> = Arc::default();
    fx.jobs
        .add_completion(Arc::new(Reserving { log: log.clone() }));
    log
}

fn seen(log: &Arc<Mutex<Ends>>) -> Ends {
    log.lock().unwrap().clone()
}

/// C2: preemption never cancels a job with external side effects (a paid prompt would be
/// submitted twice); the interactive job waits for it.
#[tokio::test(start_paused = true)] // spec: CORE-125, CORE-128, ARCH-022
async fn preemption_never_touches_a_job_with_external_side_effects() {
    let fx = Fx::suite().await;
    fx.jobs.set_preempt_agent_batches(true);
    let mut paid = gated(Lane::GpuHeavy, Priority::Agent, "paid");
    paid.engine = Some(EngineKind::ComfyUi);
    paid.external_side_effects = true;
    let paid = fx.submit(paid).await;
    fx.handler.until_started(&paid.job_id).await;
    let urgent = fx
        .submit(gated(Lane::GpuHeavy, Priority::Interactive, "urgent"))
        .await;
    tokio::task::yield_now().await;
    assert!(fx.handler.engine_cancels().is_empty());
    assert_eq!(fx.handler.started(), std::slice::from_ref(&paid.job_id));
    fx.handler.open("paid");
    assert_eq!(fx.finished(&paid.job_id).await.status, JobStatus::Succeeded);
    fx.handler.until_started(&urgent.job_id).await;
    fx.handler.open("urgent");
    fx.finished(&urgent.job_id).await;
    let paid_runs = fx
        .handler
        .runs()
        .iter()
        .filter(|r| r.job == paid.job_id)
        .count();
    assert_eq!(paid_runs, 1, "the paid job ran once");
    fx.shutdown().await;
}

/// C2: an interrupted local `agent` job is re-run once, and the re-run takes its reservation
/// over (its own completions get none, the re-run's settle it); one with external side effects
/// is never re-run and keeps its reservation for its own completions.
#[tokio::test(start_paused = true)] // spec: CORE-128, CORE-088, ARCH-022, QA-009
async fn an_interrupted_rerun_takes_the_reservation_and_external_jobs_never_rerun() {
    let fx = Fx::suite().await;
    let rv = Arc::new(HandOvers::default());
    fx.jobs.set_reservations(rv.clone());
    let log = reserving(&fx);
    let mut caller = fx.ctx();
    caller.reservation = Some("rv_local".into());
    let local = fx
        .jobs
        .submit(
            &caller,
            spec(Lane::Cpu, Priority::Agent, json!({"fail": "interrupted"})),
        )
        .await
        .unwrap();
    let end = fx.finished(&local.job_id).await;
    assert_eq!(
        (end.status, end.substatus.as_deref()),
        (JobStatus::Failed, Some("interrupted"))
    );
    let rerun = fx
        .event_until(|d| d["kind"] == "job" && d["job"]["retry_of"] == local.job_id.as_str())
        .await
        .unwrap();
    let rerun = rerun["job"]["job_id"].as_str().unwrap().to_string();
    // The re-run is interrupted too, and is not re-run again.
    fx.finished(&rerun).await;
    while fx.jobs.busy() {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fx.row(&rerun).await.reservation.as_deref(),
        Some("rv_local")
    );
    assert_eq!(
        rv.lines(),
        [
            format!("hand_over rv_local c_0000000000000001 {}", local.job_id),
            format!("hand_over rv_local {} {rerun}", local.job_id),
        ]
    );
    let lost = "comfyui.unreachable".to_string();
    assert_eq!(
        seen(&log),
        [
            (local.job_id.clone(), None, lost.clone(), false),
            (rerun.clone(), Some("rv_local".into()), lost.clone(), false),
        ]
    );

    // Hosted or paid work: never re-run automatically (CORE-128, ARCH-022).
    let mut caller = fx.ctx();
    caller.call_id = "c_0000000000000002".into();
    caller.reservation = Some("rv_paid".into());
    let mut paid = spec(Lane::Cpu, Priority::Agent, json!({"fail": "interrupted"}));
    paid.external_side_effects = true;
    let paid = fx.jobs.submit(&caller, paid).await.unwrap();
    fx.finished(&paid.job_id).await;
    while fx.jobs.busy() {
        tokio::task::yield_now().await;
    }
    let (all, _) = fx.jobs.list(&fx.ctx(), JobFilter::default()).await.unwrap();
    assert_eq!(all.len(), 3, "no re-run of the paid job: {all:?}");
    assert_eq!(
        seen(&log).last().unwrap(),
        &(paid.job_id.clone(), Some("rv_paid".into()), lost, false)
    );
    fx.shutdown().await;
}

/// I1: a cancel never hides an unknown outcome (the provider may have taken a paid request),
/// and a preemption never requeues a job its engine lost.
#[tokio::test(start_paused = true)] // spec: CORE-117, CORE-128, QA-007, CORE-125
async fn a_cancel_or_preemption_keeps_unknown_and_interrupted_outcomes() {
    let fx = Fx::suite().await;
    let log = reserving(&fx);
    let unknown = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"gate": "u", "on_cancel": "unknown"}),
        ))
        .await;
    fx.handler.until_started(&unknown.job_id).await;
    fx.jobs
        .cancel(&fx.ctx(), &JobId(unknown.job_id.clone()))
        .await
        .unwrap();
    fx.event_until(|d| {
        d["job"]["job_id"] == unknown.job_id.as_str()
            && d["job"]["substatus"] == "interrupted_unknown"
    })
    .await;
    let row = fx.row(&unknown.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Running, Some(Substatus::InterruptedUnknown))
    );

    fx.jobs.set_preempt_agent_batches(true);
    let mut batch = spec(
        Lane::GpuHeavy,
        Priority::Agent,
        json!({"gate": "b", "on_cancel": "interrupted"}),
    );
    batch.engine = Some(EngineKind::ComfyUi);
    let batch = fx.submit(batch).await;
    fx.handler.until_started(&batch.job_id).await;
    let urgent = fx
        .submit(spec(Lane::GpuHeavy, Priority::Interactive, json!({})))
        .await;
    let end = fx.finished(&batch.job_id).await;
    assert_eq!(
        (end.status, end.substatus.as_deref()),
        (JobStatus::Failed, Some("interrupted"))
    );
    fx.finished(&urgent.job_id).await;
    // Its local re-run (a new job) may go on.
    fx.handler.open("b");
    while fx.jobs.busy() {
        tokio::task::yield_now().await;
    }
    let batch_runs = fx
        .handler
        .runs()
        .iter()
        .filter(|r| r.job == batch.job_id)
        .count();
    assert_eq!(batch_runs, 1, "never requeued");
    let ended: Vec<(String, String)> = seen(&log)
        .into_iter()
        .map(|(id, _, code, _)| (id, code))
        .collect();
    assert!(
        !ended.iter().any(|(id, _)| *id == unknown.job_id),
        "an unknown job keeps its reservation: no completion runs: {ended:?}"
    );
    assert!(
        ended.contains(&(batch.job_id.clone(), "comfyui.unreachable".to_string())),
        "{ended:?}"
    );
    fx.shutdown().await;
}

/// I2: a call whose key gives back the job it made before submitted nothing: its own
/// reservation is released, and the job keeps the one it took over.
#[tokio::test(start_paused = true)] // spec: CORE-016, CORE-088
async fn a_replayed_key_releases_the_new_calls_reservation() {
    let fx = Fx::suite().await;
    let rv = Arc::new(HandOvers::default());
    fx.jobs.set_reservations(rv.clone());
    let mut first = fx.ctx_as(CallerKind::Cli, Some("k"), "t_1");
    first.call_id = "c_0000000000000001".into();
    first.reservation = Some("rv_1".into());
    let job = fx
        .jobs
        .submit(&first, gated(Lane::Cpu, Priority::Agent, "g"))
        .await
        .unwrap();
    // The core lost the call's outcome; the client retries the key, and the reserve stage
    // reserved again for the new call.
    let mut again = fx.ctx_as(CallerKind::Cli, Some("k"), "t_2");
    again.call_id = "c_0000000000000002".into();
    again.reservation = Some("rv_2".into());
    let same = fx
        .jobs
        .submit(&again, gated(Lane::Cpu, Priority::Agent, "g"))
        .await
        .unwrap();
    assert_eq!(same.job_id, job.job_id);
    assert_eq!(again.submitted.get(), Some(JobId(job.job_id.clone())));
    assert_eq!(
        rv.lines(),
        [
            format!("hand_over rv_1 c_0000000000000001 {}", job.job_id),
            "release rv_2 c_0000000000000002".to_string(),
        ]
    );
    assert_eq!(
        fx.row(&job.job_id).await.reservation.as_deref(),
        Some("rv_1")
    );
    fx.handler.open("g");
    fx.finished(&job.job_id).await;
    fx.shutdown().await;
}

/// I4, M5: a shutdown that comes while a job's completions run waits for them, so its usage and
/// its row agree; its result is written before them; a cancel meanwhile returns it as it is.
#[tokio::test(start_paused = true)] // spec: ARCH-021, CORE-015, CORE-146
async fn shutdown_waits_for_a_jobs_completions() {
    let fx = Fx::suite().await;
    let blocking = Blocking::new();
    fx.jobs.add_completion(blocking.clone());
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let id = JobId(job.job_id.clone());
    blocking.entered.subscribe().wait_for(|e| *e).await.unwrap();
    let row = fx.row(&job.job_id).await;
    assert_eq!(row.substatus, Some(Substatus::Ingesting));
    assert_eq!(row.result.unwrap().structured["ran"], 1);
    let now = fx.jobs.cancel(&fx.ctx(), &id).await.unwrap();
    assert_eq!(now.substatus.as_deref(), Some("ingesting"), "{now:?}");
    let step = fx.jobs.persist_interrupted_step();
    let persist = tokio::spawn(async move { step.run().await });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!persist.is_finished(), "the step waits for the completions");
    blocking.go.send_replace(true);
    persist.await.unwrap();
    let row = fx.row(&job.job_id).await;
    assert_eq!((row.status, row.substatus), (JobStatus::Succeeded, None));
    assert!(!fx.jobs.busy());
    fx.shutdown().await;
}

/// I4: a job whose completions outlast the shutdown's grace is marked `interrupted`, with its
/// result kept (a retry is an informed choice), and nothing is written for it after that.
#[tokio::test(start_paused = true)] // spec: ARCH-021, ARCH-022
async fn shutdown_marks_a_job_stuck_in_its_completions_interrupted_with_its_result() {
    let fx = Fx::suite().await;
    let blocking = Blocking::new();
    fx.jobs.add_completion(blocking.clone());
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    blocking.entered.subscribe().wait_for(|e| *e).await.unwrap();
    let started = tokio::time::Instant::now();
    fx.jobs.persist_interrupted_step().run().await;
    assert_eq!(started.elapsed(), COMPLETION_GRACE);
    let row = fx.row(&job.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Failed, Some(Substatus::Interrupted))
    );
    assert_eq!(row.result.as_ref().unwrap().structured["ran"], 1);
    blocking.go.send_replace(true);
    while !blocking.finished.lock().unwrap().contains(&job.job_id) {
        tokio::task::yield_now().await;
    }
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let after = fx.row(&job.job_id).await;
    assert_eq!(after, row, "nothing more is written after the step");
    assert!(!fx.jobs.busy());
    fx.shutdown().await;
}

/// I5: at recovery, a job that never started gets its completions with a "not started"
/// outcome and its reservation (nothing was used: it is released); one that had started keeps
/// its reservation (Appendix F's pending usage rule).
#[tokio::test(start_paused = true)] // spec: ARCH-022, CORE-088
async fn recovery_runs_the_completions_of_jobs_that_never_started() {
    let fx = Fx::suite().await;
    // From plan 4a a queued `t.job` is held by the restart (P4a-R41): a director task, which
    // never is, stands for a job that never started.
    fx.jobs.register(Arc::new(Director(fx.handler.clone())));
    let mut caller = fx.ctx();
    caller.reservation = Some("rv_run".into());
    let running = fx
        .jobs
        .submit(&caller, gated(Lane::Cpu, Priority::Agent, "never"))
        .await
        .unwrap();
    let mut caller = fx.ctx();
    caller.reservation = Some("rv_q".into());
    let mut director = spec(Lane::Cpu, Priority::Agent, json!({}));
    director.kind = "assistant_task";
    let queued = fx.jobs.submit(&caller, director).await.unwrap();
    fx.handler.until_started(&running.job_id).await;

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
    restarted.register(Arc::new(Director(fresh)));
    let log: Arc<Mutex<Ends>> = Arc::default();
    restarted.add_completion(Arc::new(Reserving { log: log.clone() }));
    let marked = restarted.recover_suite().await.unwrap();
    assert_eq!(marked.len(), 2);
    assert_eq!(
        seen(&log),
        [(
            queued.job_id.clone(),
            Some("rv_q".into()),
            "job.not_started".into(),
            false
        )]
    );
    fx.shutdown().await;
}

/// I5: an `interrupted_unknown` job is yours to resolve: an agent's cancel is refused; yours
/// makes it `cancelled` and runs its completions once, with its reservation.
#[tokio::test(start_paused = true)] // spec: CORE-117, CORE-088, AI-029
async fn an_interrupted_unknown_job_is_resolved_only_by_you() {
    let fx = Fx::suite().await;
    let log = reserving(&fx);
    let mut caller = fx.ctx();
    caller.reservation = Some("rv_u".into());
    let job = fx
        .jobs
        .submit(
            &caller,
            spec(Lane::Cpu, Priority::Agent, json!({"fail": "unknown"})),
        )
        .await
        .unwrap();
    fx.event_until(|d| {
        d["job"]["job_id"] == job.job_id.as_str() && d["job"]["substatus"] == "interrupted_unknown"
    })
    .await;
    while fx.jobs.busy() {
        tokio::task::yield_now().await;
    }
    let id = JobId(job.job_id.clone());
    let agent = fx.ctx_as(CallerKind::Mcp, None, "t_mcp");
    let refused = fx.jobs.cancel(&agent, &id).await.unwrap_err();
    assert_eq!(refused.code, "confirm.unavailable");
    assert!(seen(&log).is_empty());
    let you = fx.ctx_as(CallerKind::Ui("app".into()), None, "t_ui");
    let resolved = fx.jobs.cancel(&you, &id).await.unwrap();
    assert_eq!(resolved.status, JobStatus::Cancelled);
    assert!(resolved.error.is_none());
    // N4: a distinct outcome, so Caps settles the reservation at its reserved amount (P3-R15)
    // instead of releasing it as for an ordinary cancel.
    let want = [(
        job.job_id.clone(),
        Some("rv_u".into()),
        "job.resolved_unknown".to_string(),
        false,
    )];
    assert_eq!(seen(&log), want);
    let again = fx.jobs.cancel(&you, &id).await.unwrap();
    assert_eq!(again.status, JobStatus::Cancelled);
    assert_eq!(seen(&log), want, "the completions ran once");
    fx.shutdown().await;
}

/// I7: once the core drains, no queued job starts: the drain waits for the running job only.
/// From plan 4a the shutdown step leaves the queued ones queued, keeping their reservations, so
/// the next start holds them (P4a-R41: a restart never fails a job that never started).
#[tokio::test(start_paused = true)] // spec: ARCH-021, ARCH-023
async fn a_drain_starts_no_queued_job_and_waits_only_for_the_running_one() {
    let fx = Fx::suite().await;
    let lifecycle = Lifecycle::new(fx.clock.clone(), Duration::from_secs(600));
    lifecycle.add_blocker(fx.jobs.clone());
    lifecycle.add_shutdown_step(fx.jobs.persist_interrupted_step());
    fx.jobs.stop_on_drain(lifecycle.drain_token());
    let log = reserving(&fx);
    let running = fx.submit(gated(Lane::Cpu, Priority::Agent, "run")).await;
    let a = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let b = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.handler.until_started(&running.job_id).await;
    lifecycle.begin_drain(DrainReason::Requested);
    assert!(fx.jobs.busy(), "the running job holds the drain");
    let shutdown = {
        let lifecycle = lifecycle.clone();
        tokio::spawn(async move { lifecycle.run_shutdown().await })
    };
    fx.handler.open("run");
    let report = shutdown.await.unwrap();
    assert_eq!(report.steps, ["jobs.persist_interrupted"]);
    assert_eq!(fx.handler.started(), std::slice::from_ref(&running.job_id));
    assert_eq!(fx.row(&running.job_id).await.status, JobStatus::Succeeded);
    // The queued ones never reached an engine: they stay queued, keeping their reservations,
    // for the next start's restart hold (P4a-R41).
    for id in [&a.job_id, &b.job_id] {
        let row = fx.row(id).await;
        assert_eq!(
            (row.status, row.substatus),
            (JobStatus::Queued, Some(Substatus::Waiting)),
            "{id}"
        );
    }
    let codes: Vec<(String, String)> = seen(&log)
        .into_iter()
        .map(|(id, _, code, _)| (id, code))
        .collect();
    assert_eq!(codes, [(running.job_id.clone(), "ok".to_string())]);
    fx.shutdown().await;
}

/// I8 (CORE-120): when the state of an attempt can't be persisted, the handler never runs; the
/// job fails with `state.unavailable`, and its completions see that it never started.
#[tokio::test(start_paused = true)] // spec: CORE-120, ARCH-022
async fn an_attempt_that_cant_be_persisted_never_runs() {
    let fx = Fx::suite().await;
    let log = reserving(&fx);
    let db = rusqlite::Connection::open(fx.env.env.suite_state_db()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_attempts BEFORE UPDATE OF substatus ON jobs
         WHEN NEW.substatus IN ('submitting', 'executing')
         BEGIN SELECT RAISE(ABORT, 'refused by the test'); END;",
    )
    .unwrap();
    drop(db);
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let end = fx.finished(&job.job_id).await;
    assert_eq!(end.status, JobStatus::Failed);
    assert_eq!(end.error.unwrap().code, "state.unavailable");
    assert!(fx.handler.runs().is_empty(), "the handler never ran");
    assert_eq!(
        seen(&log),
        [(job.job_id.clone(), None, "job.not_started".into(), false)]
    );
    fx.shutdown().await;
}

/// Says "wait for VRAM" once, then admits.
struct WaitOnce(AtomicBool);

impl Admission for WaitOnce {
    fn admit(&self, _spec: &JobSpec) -> Admit {
        if self.0.swap(false, Ordering::SeqCst) {
            Admit::WaitForVram {
                need_mib: 10,
                free_mib: 0,
            }
        } else {
            Admit::Go
        }
    }
}

/// M1: a job admitted before its `waiting_for_vram` write ran never gets that write.
#[tokio::test(start_paused = true)] // spec: CORE-117, ARCH-071
async fn a_late_waiting_for_vram_write_never_lands_on_a_started_job() {
    let fx = Fx::suite().await;
    fx.jobs
        .set_admission(Arc::new(WaitOnce(AtomicBool::new(true))));
    let job = fx
        .submit(spec(Lane::GpuHeavy, Priority::Agent, json!({})))
        .await;
    // The memory is free at the next sample, before the spawned write has run.
    fx.jobs.kick();
    let states: Vec<Value> = fx
        .events_until("jobs", |d| {
            d["job"]["job_id"] == job.job_id.as_str() && d["job"]["status"] == "succeeded"
        })
        .await
        .iter()
        .filter(|d| d["job"]["job_id"] == job.job_id.as_str())
        .map(|d| d["job"]["substatus"].clone())
        .collect();
    assert_eq!(
        states,
        [
            json!("waiting"),
            json!("executing"),
            json!("ingesting"),
            Value::Null
        ]
    );
    fx.shutdown().await;
}

/// M3: `jobs.list` gives a cursor only when another page exists.
#[tokio::test(start_paused = true)] // spec: CORE-146
async fn jobs_list_gives_no_cursor_on_the_last_page() {
    let fx = Fx::suite().await;
    for _ in 0..2 {
        let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
        fx.finished(&job.job_id).await;
    }
    let page = |limit: u32, cursor: Option<String>| JobFilter {
        limit: Some(limit),
        cursor,
        ..JobFilter::default()
    };
    let (all, next) = fx.jobs.list(&fx.ctx(), page(2, None)).await.unwrap();
    assert_eq!((all.len(), next), (2, None));
    let (first, next) = fx.jobs.list(&fx.ctx(), page(1, None)).await.unwrap();
    assert_eq!(first.len(), 1);
    let (last, next) = fx.jobs.list(&fx.ctx(), page(1, next)).await.unwrap();
    assert_eq!((last.len(), next), (1, None));
    assert_ne!(first[0].job_id, last[0].job_id);
    fx.shutdown().await;
}

/// Earlier behaviour (re-review observation): an agent never retries a job that was already re-run
/// automatically (the re-run is the retry); it is told which job that is. From plan 4a nobody
/// does (Appendix G: only the newest attempt is retried), you neither.
#[tokio::test(start_paused = true)] // spec: CORE-128, CORE-146, QA-009
async fn nobody_retries_a_job_that_was_re_run_automatically() {
    let fx = Fx::suite().await;
    let lost = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"fail": "interrupted"}),
        ))
        .await;
    fx.finished(&lost.job_id).await;
    let rerun = fx
        .event_until(|d| d["kind"] == "job" && d["job"]["retry_of"] == lost.job_id.as_str())
        .await
        .unwrap();
    let rerun = rerun["job"]["job_id"].as_str().unwrap().to_string();
    fx.finished(&rerun).await;
    let id = JobId(lost.job_id.clone());
    for agent in [CallerKind::Cli, CallerKind::Mcp, CallerKind::Assistant] {
        let e = fx
            .jobs
            .retry_origin(&fx.ctx_as(agent.clone(), None, "t_agent"), &id)
            .await
            .unwrap_err();
        assert_eq!(e.code, "job.not_retryable", "{agent}");
        assert_eq!(e.data.as_ref().unwrap()["rerun"], rerun.as_str(), "{agent}");
        assert!(e.message.contains(&rerun), "{}", e.message);
    }
    let you = fx.ctx_as(CallerKind::Ui("app".into()), None, "t_ui");
    let e = fx.jobs.retry_origin(&you, &id).await.unwrap_err();
    assert_eq!(e.code, "job.not_retryable");
    assert!(e.message.contains("already retried as"), "{}", e.message);
    // The re-run itself was never re-run, so it can be retried.
    fx.jobs
        .retry_origin(&fx.ctx(), &JobId(rerun.clone()))
        .await
        .unwrap();
    fx.shutdown().await;
}

/// N3: a job whose Record step is still running when the shutdown's grace ends is left to
/// write its own final state, so the row agrees with the run it recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-021, ARCH-075
async fn a_record_step_that_outlasts_the_grace_still_ends_the_row_as_recorded() {
    let fx = Fx::workspace().await;
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (entered_tx, go_rx) = (Mutex::new(entered_tx), Mutex::new(go_rx));
    // The hook runs on a blocking thread, in the Record step, after its manifest edit.
    fx.failpoints.on(
        "store.after_manifest",
        Box::new(move || {
            let _ = entered_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(60));
            Ok(())
        }),
    );
    let job = fx
        .submit(spec(Lane::Cpu, Priority::Agent, json!({"record": true})))
        .await;
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(30)))
        .await
        .unwrap()
        .expect("the Record step began");
    fx.jobs.persist_interrupted_step().run().await;
    let row = fx.row(&job.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Running, Some(Substatus::Ingesting)),
        "the shutdown step never marks a job in its Record step"
    );
    go_tx.send(()).unwrap();
    let end = fx.finished(&job.job_id).await;
    assert_eq!(end.status, JobStatus::Succeeded, "{end:?}");
    assert!(end.result.unwrap().structured["handoff"].is_object());
    fx.shutdown().await;
}

/// N1: a retry runs in the retried job's project, not in the one the retrying session would
/// resolve (its active project).
#[test] // spec: ARCH-088, ARCH-098, CORE-146
fn a_retry_runs_in_the_retried_jobs_project() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("jobs");
    env.register_workspace(&ws, true);
    let _core = lil_core::start_in_thread(lil_core::CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..lil_core::CoreConfig::for_env(env.env.clone()).unwrap()
    });
    let session = |project: &str| {
        let mut pipe = lil_testkit::RawPipe::connect(&env);
        let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
        assert_eq!(hello["result"]["status"], "ok", "{hello}");
        // A call that names a project makes it the session's active project (ARCH-088).
        pipe.tool_ok("test.settings", json!({"project": project}), None);
        pipe
    };
    let mut first = session("castle");
    let job = first.tool_ok(
        "test.job",
        json!({"lane": "cpu", "fail": "final"}),
        Some("make"),
    )["job"]
        .clone();
    assert_eq!(job["project"], "castle", "{job}");
    let id = job["job_id"].as_str().unwrap().to_string();
    let waited = first.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 30}), None);
    assert_eq!(waited["job"]["status"], "failed", "{waited}");

    let mut other = session("sketchbook");
    let retried = other.tool_ok("job.retry", json!({"job_id": id}), Some("again"));
    assert_eq!(retried["job"]["project"], "castle", "{retried}");
}

/// QA1 I1, QA-004, QA-007: a finished job's `error.retry` is about the job, never about sending
/// its call again (the call's idempotency key gives back the same job): `after_resolve` with a
/// `job.retry` hint for a failed job, `auto` only for the one automatic re-run, and
/// `after_resolve` without a hint for `interrupted_unknown`. A recovered row's stale transient
/// error is replaced by the core's own.
#[tokio::test(start_paused = true)] // spec: QA-004, QA-007, CORE-128
async fn a_finished_jobs_error_says_how_to_retry_the_job() {
    use lil_protocol::RetryPolicy;
    let fx = Fx::suite().await;
    let hint = |e: &ErrorBody| {
        e.fix
            .as_ref()
            .and_then(|f| f.agent_hint.as_ref())
            .map(|h| (h.tool.clone(), h.arguments.clone()))
    };
    // `store.io` says `same_idempotency_key` for a call; the job that failed with it says
    // `job.retry`.
    let failed = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"fail": "retryable_always"}),
        ))
        .await;
    let end = fx.finished(&failed.job_id).await;
    let error = end.error.unwrap();
    assert_eq!(error.code, "store.io");
    assert_eq!(error.retry, RetryPolicy::AfterResolve);
    assert_eq!(
        hint(&error),
        Some(("job.retry".to_string(), json!({"job_id": failed.job_id})))
    );

    // An interrupted agent job is re-run once by the core: `auto` there, and only there.
    let rerun = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"fail": "interrupted"}),
        ))
        .await;
    let end = fx.finished(&rerun.job_id).await;
    assert_eq!(end.error.unwrap().retry, RetryPolicy::Auto);
    let mut external = spec(Lane::Cpu, Priority::Agent, json!({"fail": "interrupted"}));
    external.external_side_effects = true;
    let external = fx.submit(external).await;
    let error = fx.finished(&external.job_id).await.error.unwrap();
    assert_eq!(
        error.retry,
        RetryPolicy::AfterResolve,
        "not re-run (P3-R21)"
    );
    assert_eq!(hint(&error), None, "only you can retry it");

    let unknown = fx
        .submit(spec(Lane::Cpu, Priority::Agent, json!({"fail": "unknown"})))
        .await;
    fx.event_until(|d| {
        d["job"]["job_id"] == unknown.job_id.as_str()
            && d["job"]["substatus"] == "interrupted_unknown"
    })
    .await;
    let error = fx.row(&unknown.job_id).await.error.unwrap();
    assert_eq!(error.retry, RetryPolicy::AfterResolve);
    assert_eq!(hint(&error), None);

    // An earlier core: a job that kept an earlier attempt's transient error, and an engine job
    // with external side effects stuck in `submitting`.
    let retrying = fx.submit(gated(Lane::Cpu, Priority::Agent, "never")).await;
    fx.handler.until_started(&retrying.job_id).await;
    let stale = record::Change {
        error: Some(Some(ErrorBody::new(
            &codes::STORE_IO,
            "an earlier attempt",
            "t",
        ))),
        ..record::Change::default()
    };
    record::update(&fx.states.suite_db(), &retrying.job_id, stale, 0)
        .await
        .unwrap()
        .unwrap();
    let mut stuck = gated(Lane::GpuHeavy, Priority::Agent, "never");
    stuck.engine = Some(EngineKind::ComfyUi);
    stuck.external_side_effects = true;
    let stuck = fx.submit(stuck).await;
    fx.handler.until_started(&stuck.job_id).await;
    let restarted = Jobs::new(
        fx.states.clone(),
        fx.hub.clone(),
        fx.events.clone(),
        fx.clock.clone(),
        Arc::new(SeqIds::new()),
        fx.failpoints.clone(),
    );
    restarted.register(Arc::new(TestHandler::default()));
    restarted.recover_suite().await.unwrap();
    let row = fx.row(&retrying.job_id).await;
    assert_eq!(row.substatus, Some(Substatus::Interrupted));
    let error = row.error.unwrap();
    assert_eq!(
        (error.code.as_str(), error.retry),
        ("core.draining", RetryPolicy::AfterResolve)
    );
    assert_eq!(
        hint(&error),
        Some(("job.retry".to_string(), json!({"job_id": retrying.job_id})))
    );
    let row = fx.row(&stuck.job_id).await;
    assert_eq!(row.substatus, Some(Substatus::InterruptedUnknown));
    let error = row.error.unwrap();
    assert_eq!(
        (error.code.as_str(), error.retry),
        ("core.draining", RetryPolicy::AfterResolve)
    );
    assert_eq!(hint(&error), None);
    fx.shutdown().await;
}

/// QA1 I2: `job.wait` on an `interrupted_unknown` job returns at once with `done`: only you can
/// resolve it, so a wait loop on it would never end.
#[tokio::test(start_paused = true)] // spec: AI-113, QA-007
async fn job_wait_is_done_for_an_interrupted_unknown_job() {
    let fx = Fx::suite().await;
    let job = fx
        .submit(spec(Lane::Cpu, Priority::Agent, json!({"fail": "unknown"})))
        .await;
    fx.event_until(|d| {
        d["job"]["job_id"] == job.job_id.as_str() && d["job"]["substatus"] == "interrupted_unknown"
    })
    .await;
    let started = tokio::time::Instant::now();
    let (summary, done) = fx
        .jobs
        .wait(
            &fx.ctx(),
            &JobId(job.job_id.clone()),
            Duration::from_secs(90),
        )
        .await
        .unwrap();
    assert!(done, "{summary:?}");
    assert_eq!(summary.substatus.as_deref(), Some("interrupted_unknown"));
    assert_eq!(tokio::time::Instant::now(), started, "it did not wait");
    fx.shutdown().await;
}

/// QA1 M2: an engine that loses a job while you cancel it ends the job `interrupted`, with the
/// outputs it salvaged, and it is not run again.
#[tokio::test(start_paused = true)] // spec: CORE-128, QA-009
async fn an_engine_loss_during_a_cancel_is_an_interruption_that_is_not_rerun() {
    let fx = Fx::suite().await;
    let job = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"gate": "c", "on_cancel": "interrupted"}),
        ))
        .await;
    fx.handler.until_started(&job.job_id).await;
    fx.jobs
        .cancel(&fx.ctx(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    let end = fx.finished(&job.job_id).await;
    assert_eq!(
        (end.status, end.substatus.as_deref()),
        (JobStatus::Failed, Some("interrupted"))
    );
    while fx.jobs.busy() {
        tokio::task::yield_now().await;
    }
    let (all, _) = fx.jobs.list(&fx.ctx(), JobFilter::default()).await.unwrap();
    assert_eq!(all.len(), 1, "no re-run: {all:?}");
    fx.shutdown().await;
}

/// Task 12 re-review N4 (ruling P3-R21): a job with external side effects whose row holds an
/// engine id is never plain `interrupted`, even when the recovery can't take it up (here: the
/// core is draining) or it was waiting for its turn after an earlier take-up.
#[tokio::test(start_paused = true)] // spec: CORE-128, QA-007, ARCH-022
async fn an_external_job_with_an_engine_id_is_never_plain_interrupted() {
    let fx = Fx::suite().await;
    let mut paid = gated(Lane::GpuHeavy, Priority::Agent, "never");
    paid.engine = Some(EngineKind::ComfyUi);
    paid.external_side_effects = true;
    let in_engine = fx.submit(paid.clone()).await;
    let waiting = fx.submit(paid).await;
    fx.handler.until_started(&in_engine.job_id).await;
    let db = fx.states.suite_db();
    for (id, substatus) in [
        (&in_engine.job_id, Substatus::Executing),
        (&waiting.job_id, Substatus::Waiting),
    ] {
        let c = record::Change {
            external_id: Some(Some(format!("prompt-of-{id}"))),
            ..record::Change::to(substatus)
        };
        record::update(&db, id, c, 0).await.unwrap().unwrap();
    }
    let restarted = Jobs::new(
        fx.states.clone(),
        fx.hub.clone(),
        fx.events.clone(),
        fx.clock.clone(),
        Arc::new(SeqIds::new()),
        fx.failpoints.clone(),
    );
    restarted.register(Arc::new(TestHandler::default()));
    let drain = tokio_util::sync::CancellationToken::new();
    drain.cancel();
    restarted.stop_on_drain(drain);
    restarted.recover_suite().await.unwrap();
    for id in [&in_engine.job_id, &waiting.job_id] {
        let row = fx.row(id).await;
        assert_eq!(
            (row.status, row.substatus),
            (JobStatus::Running, Some(Substatus::InterruptedUnknown)),
            "{id}"
        );
    }
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Plan 4a: job fields, queue order and `job.move`, batch cancels, Pause GPU work, the restart
// hold and `job.retry {batch_id}` (CORE-117, CORE-120, CORE-160, ARCH-022, ARCH-072;
// Appendix G "Jobs"; P4a-R5, P4a-R12, P4a-R13, P4a-R41).
// ---------------------------------------------------------------------------------------------

/// Waits (no timer) for a `jobs` event of `id` with `substatus`; its summary.
async fn until_substatus(fx: &Fx, id: &str, substatus: &str) -> JobSummary {
    let data = fx
        .event_until(|d| {
            d["kind"] == "job" && d["job"]["job_id"] == id && d["job"]["substatus"] == substatus
        })
        .await
        .unwrap();
    serde_json::from_value(data["job"].clone()).unwrap()
}

/// A summary of `id` from `jobs`, until `f` holds for it (yielding meanwhile, no timer).
async fn until_summary(
    jobs: &Jobs,
    fx: &Fx,
    id: &str,
    f: impl Fn(&JobSummary) -> bool,
) -> JobSummary {
    loop {
        let job = jobs.get(&fx.ctx(), &JobId(id.to_string())).await.unwrap();
        if f(&job) {
            return job;
        }
        tokio::task::yield_now().await;
    }
}

/// Ids for a restarted "core", apart from the fixture's (its rows keep theirs).
#[derive(Debug)]
struct LaterIds(std::sync::atomic::AtomicU64);

impl IdSource for LaterIds {
    fn id(&self, prefix: &str) -> String {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        format!("{prefix}_{:016}", 9_000_000 + n)
    }

    fn uuid(&self) -> String {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        format!("00000000-0000-4000-9000-{n:012x}")
    }
}

fn later_ids() -> Arc<dyn IdSource> {
    Arc::new(LaterIds(std::sync::atomic::AtomicU64::new(0)))
}

/// A new "core" on the fixture's state: a new `Jobs` with fresh handlers (`t.job` and director
/// tasks), recovered. The earlier `Jobs` is left as a killed core leaves its jobs: its running
/// job never ends.
async fn restart(fx: &Fx) -> (Arc<Jobs>, Arc<TestHandler>, Vec<String>) {
    let restarted = Jobs::new(
        fx.states.clone(),
        fx.hub.clone(),
        fx.events.clone(),
        fx.clock.clone(),
        later_ids(),
        fx.failpoints.clone(),
    );
    let fresh = Arc::new(TestHandler::default());
    restarted.register(fresh.clone());
    restarted.register(Arc::new(Director(fresh.clone())));
    let marked = restarted
        .recover_suite()
        .await
        .unwrap()
        .into_iter()
        .map(|id| id.0)
        .collect();
    (restarted, fresh, marked)
}

/// A `t.job` running gated on `gate` in `lane`, started.
async fn running_gate(fx: &Fx, lane: Lane, gate: &str) -> JobSummary {
    let job = fx.submit(gated(lane, Priority::Agent, gate)).await;
    fx.handler.until_started(&job.job_id).await;
    job
}

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn move_next_and_last_stay_within_the_class() {
    let fx = Fx::suite().await;
    let a0 = self_gated(&fx, Priority::Agent, None).await;
    fx.handler.until_started(&a0).await;
    let a1 = self_gated(&fx, Priority::Agent, None).await;
    let a2 = self_gated(&fx, Priority::Agent, None).await;
    let a3 = self_gated(&fx, Priority::Agent, None).await;
    let u1 = self_gated(&fx, Priority::User, None).await;
    let moved = fx
        .jobs
        .move_job(&fx.ctx(), &JobId(a3.clone()), MoveTo::Next)
        .await
        .unwrap();
    assert_eq!(
        moved.priority,
        Priority::Agent,
        "a move never changes the class"
    );
    fx.jobs
        .move_job(&fx.ctx(), &JobId(a1.clone()), MoveTo::Last)
        .await
        .unwrap();
    // The user job still goes first (a higher class); then the agent class in its new order.
    assert_eq!(start_order(&fx, 5).await, [a0, u1, a3, a2, a1]);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn move_refuses_engine_running_and_finished_jobs() {
    let fx = Fx::suite().await;
    let mut in_engine = spec(
        Lane::GpuHeavy,
        Priority::Agent,
        json!({"gate": "engine", "in_engine": true}),
    );
    in_engine.engine = Some(EngineKind::ComfyUi);
    let in_engine = fx.submit(in_engine).await;
    until_substatus(&fx, &in_engine.job_id, "queued_in_engine").await;
    let running = running_gate(&fx, Lane::Cpu, "run").await;
    let done = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.handler.open("run");
    fx.finished(&running.job_id).await;
    assert_eq!(fx.finished(&done.job_id).await.status, JobStatus::Succeeded);
    let still = running_gate(&fx, Lane::Cpu, "still").await;
    for id in [&in_engine.job_id, &still.job_id, &done.job_id] {
        for to in [MoveTo::Next, MoveTo::Last] {
            let e = fx
                .jobs
                .move_job(&fx.ctx(), &JobId(id.clone()), to)
                .await
                .unwrap_err();
            assert_eq!(e.code, "job.not_queued", "{id}");
            assert_eq!(e.message, codes::JOB_NOT_QUEUED_MESSAGE);
        }
    }
    let e = fx
        .jobs
        .move_job(&fx.ctx(), &JobId("j_00000000000000ff".into()), MoveTo::Next)
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.unknown");
    // Nothing in the engine was touched.
    assert!(fx.handler.engine_cancels().is_empty());
    fx.handler.open("engine");
    fx.handler.open("still");
    fx.finished(&in_engine.job_id).await;
    fx.finished(&still.job_id).await;
    fx.shutdown().await;
}

/// A job moved to the front goes first without overtaking anyone (CORE-124's limit counts
/// only group moves): the older job of another group is still overtaken by the lane's group
/// 8 times, then runs.
#[tokio::test(start_paused = true)] // spec: CORE-160, CORE-124
async fn moved_jobs_never_count_as_overtaken_by_groups() {
    let fx = Fx::suite().await;
    let g0 = self_gated(&fx, Priority::Agent, Some("a")).await;
    fx.handler.until_started(&g0).await;
    let x = self_gated(&fx, Priority::Agent, Some("b")).await;
    let mut moved = Vec::new();
    for _ in 0..10 {
        let m = self_gated(&fx, Priority::Agent, Some("a")).await;
        fx.jobs
            .move_job(&fx.ctx(), &JobId(m.clone()), MoveTo::Next)
            .await
            .unwrap();
        moved.push(m);
    }
    let mut grouped = Vec::new();
    for _ in 0..9 {
        grouped.push(self_gated(&fx, Priority::Agent, Some("a")).await);
    }
    let order = start_order(&fx, 21).await;
    let mut want = vec![g0];
    want.extend(moved.iter().rev().cloned());
    want.extend(grouped[..8].iter().cloned());
    want.push(x);
    want.push(grouped[8].clone());
    assert_eq!(order, want);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn all_mine_is_scoped_by_caller_kind_and_app() {
    let fx = Fx::suite().await;
    // Everything waits behind a `cli` job that runs until it is cancelled.
    let blocker = running_gate(&fx, Lane::Cpu, "never").await;
    let app = fx.ctx_as(CallerKind::Ui("test".into()), None, "t_app");
    let other_app = fx.ctx_as(CallerKind::Ui("other".into()), None, "t_other");
    let theirs = fx
        .jobs
        .submit(&app, spec(Lane::Cpu, Priority::User, json!({})))
        .await
        .unwrap();
    let other = fx
        .jobs
        .submit(&other_app, spec(Lane::Cpu, Priority::User, json!({})))
        .await
        .unwrap();
    let mine = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    // The ui:test app's all_mine is its own jobs: not another app's, not the cli's.
    let cancelled = fx
        .jobs
        .cancel_selected(&app, CancelSelector::AllMine)
        .await
        .unwrap();
    let ids: Vec<&str> = cancelled.iter().map(|j| j.job_id.as_str()).collect();
    assert_eq!(ids, [theirs.job_id.as_str()]);
    assert_eq!(
        fx.finished(&theirs.job_id).await.status,
        JobStatus::Cancelled
    );
    // The cli's all_mine is the cli jobs: a ui:test job survives it.
    let cancelled = fx
        .jobs
        .cancel_selected(&fx.ctx(), CancelSelector::AllMine)
        .await
        .unwrap();
    let mut ids: Vec<String> = cancelled.iter().map(|j| j.job_id.clone()).collect();
    ids.sort();
    let mut want = vec![blocker.job_id.clone(), mine.job_id.clone()];
    want.sort();
    assert_eq!(ids, want, "a cli caller's all_mine is the cli jobs");
    assert_eq!(fx.finished(&mine.job_id).await.status, JobStatus::Cancelled);
    assert_eq!(
        fx.finished(&blocker.job_id).await.status,
        JobStatus::Cancelled
    );
    let kept = fx.finished(&other.job_id).await;
    assert_eq!(
        kept.status,
        JobStatus::Succeeded,
        "another app's job is not mine"
    );
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160, CORE-117
async fn cancel_by_batch_and_by_ids() {
    let fx = Fx::suite().await;
    let blocker = running_gate(&fx, Lane::Cpu, "never").await;
    let batch = fx
        .jobs
        .submit_batch(
            &fx.ctx(),
            (0..3)
                .map(|_| spec(Lane::Cpu, Priority::Agent, json!({})))
                .collect(),
        )
        .await
        .unwrap();
    let batch_id = batch[0].batch_id.clone();
    let loose: Vec<JobSummary> = {
        let mut v = Vec::new();
        for _ in 0..3 {
            v.push(fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await);
        }
        v
    };
    // {ids}: exactly those; an unknown id refuses the whole call first.
    let unknown = fx
        .jobs
        .cancel_selected(
            &fx.ctx(),
            CancelSelector::Ids(vec![
                JobId(loose[0].job_id.clone()),
                JobId("j_00000000000000ff".into()),
            ]),
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code, "job.unknown");
    assert_eq!(fx.row(&loose[0].job_id).await.status, JobStatus::Queued);
    for bad in [Vec::new(), vec![JobId(loose[0].job_id.clone()); 501]] {
        let e = fx
            .jobs
            .cancel_selected(&fx.ctx(), CancelSelector::Ids(bad))
            .await
            .unwrap_err();
        assert_eq!(e.code, "params.invalid");
    }
    let by_ids = fx
        .jobs
        .cancel_selected(
            &fx.ctx(),
            CancelSelector::Ids(vec![
                JobId(loose[0].job_id.clone()),
                JobId(loose[1].job_id.clone()),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(
        by_ids.iter().map(|j| j.job_id.as_str()).collect::<Vec<_>>(),
        [loose[0].job_id.as_str(), loose[1].job_id.as_str()]
    );
    assert!(by_ids.iter().all(|j| j.status == JobStatus::Cancelled));
    assert_eq!(fx.row(&loose[2].job_id).await.status, JobStatus::Queued);
    // {batch_id}: the batch's jobs, by index.
    let by_batch = fx
        .jobs
        .cancel_selected(&fx.ctx(), CancelSelector::Batch(batch_id.clone()))
        .await
        .unwrap();
    assert_eq!(
        by_batch.iter().map(|j| j.index).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(by_batch.iter().all(|j| j.status == JobStatus::Cancelled));
    let e = fx
        .jobs
        .cancel_selected(
            &fx.ctx(),
            CancelSelector::Batch("b_00000000000000ff".into()),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.unknown");
    fx.jobs
        .cancel(&fx.ctx(), &JobId(blocker.job_id.clone()))
        .await
        .unwrap();
    fx.finished(&blocker.job_id).await;
    fx.finished(&loose[2].job_id).await;
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160, CORE-117
async fn pause_holds_every_class_and_caller_as_queue_paused() {
    let fx = Fx::suite().await;
    let gate = running_gate(&fx, Lane::Cpu, "gate").await;
    let mut queued = Vec::new();
    for (caller, class) in [
        (CallerKind::Cli, Priority::Agent),
        (CallerKind::Mcp, Priority::Agent),
        (CallerKind::Ui("image".into()), Priority::User),
        (CallerKind::Ui("image".into()), Priority::Interactive),
    ] {
        let ctx = fx.ctx_as(caller, None, "t_q");
        let job = fx
            .jobs
            .submit(&ctx, spec(Lane::Cpu, class, json!({})))
            .await
            .unwrap();
        queued.push(job.job_id);
    }
    let state = fx.jobs.pause(Lane::Cpu, PauseReason::Manual).await;
    assert_eq!(
        (state.paused, state.reasons.clone()),
        (true, vec!["manual".to_string()])
    );
    for id in &queued {
        let held = until_substatus(&fx, id, "queue_paused").await;
        assert_eq!(held.status, JobStatus::Queued);
        assert_eq!(held.pause_reasons, ["manual"]);
    }
    let state = fx.jobs.queue_state(Lane::Cpu);
    assert_eq!((state.held, state.restart_held), (4, 0));
    // The other lane isn't paused.
    assert!(!fx.jobs.queue_state(Lane::GpuHeavy).paused);
    // A job submitted while paused is held at once.
    let late = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    until_substatus(&fx, &late.job_id, "queue_paused").await;
    // The paused lane dispatches nothing, even once its running job ended.
    fx.handler.open("gate");
    fx.finished(&gate.job_id).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(fx.handler.started(), std::slice::from_ref(&gate.job_id));
    let state = fx.jobs.resume(Lane::Cpu, PauseReason::Manual).await;
    assert!(!state.paused && state.reasons.is_empty(), "{state:?}");
    for id in queued.iter().chain(std::iter::once(&late.job_id)) {
        assert_eq!(fx.finished(id).await.status, JobStatus::Succeeded);
    }
    // In class order: interactive, user, then the agent jobs.
    assert_eq!(
        fx.handler.started()[1..3],
        [queued[3].clone(), queued[2].clone()]
    );
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn pause_never_interrupts_the_executing_prompt() {
    let fx = Fx::suite().await;
    let mut engine = gated(Lane::GpuHeavy, Priority::Agent, "exec");
    engine.engine = Some(EngineKind::ComfyUi);
    let running = fx.submit(engine).await;
    fx.handler.until_started(&running.job_id).await;
    let before = fx.row(&running.job_id).await;
    fx.jobs.pause(Lane::GpuHeavy, PauseReason::Manual).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    let row = fx.row(&running.job_id).await;
    assert_eq!(
        (row.status, row.substatus, row.updated_utc),
        (before.status, before.substatus, before.updated_utc),
        "the running job is left alone: {row:?}"
    );
    assert!(fx.handler.engine_cancels().is_empty());
    assert_eq!(fx.jobs.queue_state(Lane::GpuHeavy).withdrawn, 0);
    fx.handler.open("exec");
    assert_eq!(
        fx.finished(&running.job_id).await.status,
        JobStatus::Succeeded
    );
    fx.jobs.resume(Lane::GpuHeavy, PauseReason::Manual).await;
    fx.shutdown().await;
}

/// Two reasons: `resume(manual)` removes only `manual`, so the lane stays paused by
/// `fullscreen` and its jobs list it (P4a-R5; Appendix G `pause_reasons`).
#[tokio::test(start_paused = true)] // spec: CORE-160
async fn resume_removes_only_its_reason() {
    let fx = Fx::suite().await;
    let gate = running_gate(&fx, Lane::Cpu, "gate").await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.jobs.pause(Lane::Cpu, PauseReason::Manual).await;
    let state = fx.jobs.pause(Lane::Cpu, PauseReason::Fullscreen).await;
    assert_eq!(state.reasons, ["manual", "fullscreen"]);
    let state = fx.jobs.resume(Lane::Cpu, PauseReason::Manual).await;
    assert!(state.paused, "{state:?}");
    assert_eq!(state.reasons, ["fullscreen"]);
    fx.handler.open("gate");
    fx.finished(&gate.job_id).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(fx.row(&job.job_id).await.status, JobStatus::Queued);
    let state = fx.jobs.resume(Lane::Cpu, PauseReason::Fullscreen).await;
    assert!(!state.paused);
    assert_eq!(fx.finished(&job.job_id).await.status, JobStatus::Succeeded);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160, CORE-117
async fn queue_paused_jobs_list_their_reasons() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    assert!(job.pause_reasons.is_empty());
    fx.jobs.pause(Lane::Cpu, PauseReason::Manual).await;
    fx.jobs.pause(Lane::Cpu, PauseReason::Fullscreen).await;
    until_substatus(&fx, &job.job_id, "queue_paused").await;
    let both = until_summary(&fx.jobs, &fx, &job.job_id, |j| j.pause_reasons.len() == 2).await;
    assert_eq!(both.pause_reasons, ["manual", "fullscreen"]);
    fx.jobs.resume(Lane::Cpu, PauseReason::Manual).await;
    let one = until_summary(&fx.jobs, &fx, &job.job_id, |j| j.pause_reasons.len() == 1).await;
    assert_eq!(one.pause_reasons, ["fullscreen"]);
    assert_eq!(one.substatus.as_deref(), Some("queue_paused"));
    // The change of reasons is published.
    let announced = fx
        .event_until(|d| {
            d["kind"] == "job"
                && d["job"]["job_id"] == job.job_id.as_str()
                && d["job"]["pause_reasons"] == json!(["fullscreen"])
        })
        .await;
    assert!(announced.is_some());
    fx.shutdown().await;
}

/// P4a-R5: a pause lives in memory, so a restart ends it; the jobs it held are then held by
/// the restart (P4a-R41).
#[tokio::test(start_paused = true)] // spec: CORE-160
async fn a_restart_ends_a_pause() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.jobs.pause(Lane::Cpu, PauseReason::Manual).await;
    until_substatus(&fx, &job.job_id, "queue_paused").await;
    let (restarted, _, _) = restart(&fx).await;
    let state = restarted.queue_state(Lane::Cpu);
    assert!(!state.paused && state.reasons.is_empty(), "{state:?}");
    let held = restarted
        .get(&fx.ctx(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(held.pause_reasons, ["restart"]);
    fx.shutdown().await;
}

/// P4a-R41, ARCH-022's exception: a batch of 3 waiting jobs is held by the restart; a job that
/// reached its engine (`queued_in_engine`, with an engine id) is interrupted as before; a job
/// submitted after the restart runs, the lane isn't paused.
#[tokio::test(start_paused = true)] // spec: ARCH-022, CORE-160
async fn a_restart_holds_never_started_jobs() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let batch = fx
        .jobs
        .submit_batch(
            &fx.ctx(),
            (0..3)
                .map(|_| spec(Lane::Cpu, Priority::Agent, json!({})))
                .collect(),
        )
        .await
        .unwrap();
    let in_engine = running_gate(&fx, Lane::GpuHeavy, "never").await;
    let c = record::Change {
        external_id: Some(Some("prompt-of-the-dead-core".into())),
        ..record::Change::to(Substatus::QueuedInEngine)
    };
    record::update(&fx.states.suite_db(), &in_engine.job_id, c, 0)
        .await
        .unwrap()
        .unwrap();

    let (restarted, fresh, marked) = restart(&fx).await;
    assert!(marked.contains(&in_engine.job_id), "{marked:?}");
    for job in &batch {
        assert!(!marked.contains(&job.job_id), "{marked:?}");
        let held = restarted
            .get(&fx.ctx(), &JobId(job.job_id.clone()))
            .await
            .unwrap();
        assert_eq!(
            (held.status, held.substatus.as_deref()),
            (JobStatus::Queued, Some("queue_paused"))
        );
        assert_eq!(held.pause_reasons, ["restart"]);
        assert_eq!(held.batch_id, batch[0].batch_id);
    }
    let lost = fx.row(&in_engine.job_id).await;
    assert_eq!(lost.substatus, Some(Substatus::Interrupted));
    let state = restarted.queue_state(Lane::Cpu);
    assert!(!state.paused, "a restart never pauses the lane: {state:?}");
    assert_eq!((state.held, state.restart_held), (3, 3));
    // New jobs run.
    let new = restarted
        .submit(&fx.ctx(), spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    fresh.until_started(&new.job_id).await;
    assert_eq!(fresh.started(), std::slice::from_ref(&new.job_id));
    fx.shutdown().await;
}

/// `resume(manual)` releases the restart's jobs too (P4a-R41); a released job still waits
/// while another reason (`fullscreen`) covers it.
#[tokio::test(start_paused = true)] // spec: CORE-160, ARCH-022
async fn resume_releases_restart_held_jobs() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let (restarted, fresh, _) = restart(&fx).await;
    restarted.pause(Lane::Cpu, PauseReason::Fullscreen).await;
    let state = restarted.resume(Lane::Cpu, PauseReason::Manual).await;
    assert_eq!(state.restart_held, 0, "{state:?}");
    let waiting = until_summary(&restarted, &fx, &job.job_id, |j| {
        j.pause_reasons == ["fullscreen"]
    })
    .await;
    assert_eq!(waiting.substatus.as_deref(), Some("queue_paused"));
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(fresh.started().is_empty());
    restarted.resume(Lane::Cpu, PauseReason::Fullscreen).await;
    fresh.until_started(&job.job_id).await;
    assert_eq!(fx.finished(&job.job_id).await.status, JobStatus::Succeeded);
    fx.shutdown().await;
}

/// The hold lives in memory: another restart holds the same jobs again.
#[tokio::test(start_paused = true)] // spec: CORE-160, ARCH-022
async fn a_second_restart_holds_them_again() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let (first, _, _) = restart(&fx).await;
    assert_eq!(first.queue_state(Lane::Cpu).restart_held, 1);
    let (second, fresh, marked) = restart(&fx).await;
    assert!(marked.is_empty(), "{marked:?}");
    let held = second
        .get(&fx.ctx(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(held.pause_reasons, ["restart"]);
    assert_eq!(second.queue_state(Lane::Cpu).restart_held, 1);
    second.release_restart_held(Lane::Cpu).await;
    fresh.until_started(&job.job_id).await;
    fx.shutdown().await;
}

/// ARCH-022 for director tasks until the director plan: never held, interrupted.
#[tokio::test(start_paused = true)] // spec: ARCH-022, CORE-118
async fn a_director_task_is_never_held() {
    let fx = Fx::suite().await;
    fx.jobs.register(Arc::new(Director(fx.handler.clone())));
    running_gate(&fx, Lane::Cpu, "never").await;
    let mut task = spec(Lane::Cpu, Priority::Agent, json!({}));
    task.kind = "assistant_task";
    let task = fx.submit(task).await;
    let (restarted, _, marked) = restart(&fx).await;
    assert!(marked.contains(&task.job_id), "{marked:?}");
    let row = fx.row(&task.job_id).await;
    assert_eq!(
        (row.status, row.substatus),
        (JobStatus::Failed, Some(Substatus::Interrupted))
    );
    assert_eq!(restarted.queue_state(Lane::Cpu).restart_held, 0);
    fx.shutdown().await;
}

/// A held job keeps its cap reservation (no completion runs at the restart); it settles when
/// the job runs after a resume.
#[tokio::test(start_paused = true)] // spec: CORE-160, CORE-088
async fn a_held_job_keeps_its_reservation() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let mut caller = fx.ctx();
    caller.reservation = Some("rv_held".into());
    let job = fx
        .jobs
        .submit(&caller, spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    let restarted = Jobs::new(
        fx.states.clone(),
        fx.hub.clone(),
        fx.events.clone(),
        fx.clock.clone(),
        later_ids(),
        fx.failpoints.clone(),
    );
    let fresh = Arc::new(TestHandler::default());
    restarted.register(fresh.clone());
    let log: Arc<Mutex<Ends>> = Arc::default();
    restarted.add_completion(Arc::new(Reserving { log: log.clone() }));
    restarted.recover_suite().await.unwrap();
    assert!(
        seen(&log).is_empty(),
        "no completion at the restart: {:?}",
        seen(&log)
    );
    assert_eq!(
        fx.row(&job.job_id).await.reservation.as_deref(),
        Some("rv_held")
    );
    restarted.release_restart_held(Lane::Cpu).await;
    fx.finished(&job.job_id).await;
    while seen(&log).is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        seen(&log),
        [(
            job.job_id.clone(),
            Some("rv_held".into()),
            "ok".into(),
            false
        )]
    );
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn queue_state_counts_restart_held() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    for _ in 0..2 {
        fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    }
    let (restarted, _, _) = restart(&fx).await;
    let state = restarted.queue_state(Lane::Cpu);
    assert_eq!(
        (
            state.paused,
            state.held,
            state.restart_held,
            state.withdrawn
        ),
        (false, 2, 2, 0)
    );
    // A job submitted to the restarted core and held by your pause counts in `held` only.
    let state = restarted.pause(Lane::Cpu, PauseReason::Manual).await;
    assert_eq!((state.held, state.restart_held), (2, 2));
    let late = restarted
        .submit(&fx.ctx(), spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    until_substatus(&fx, &late.job_id, "queue_paused").await;
    let state = restarted.queue_state(Lane::Cpu);
    assert_eq!((state.held, state.restart_held), (3, 2));
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        json!({"lane": "cpu", "paused": true, "reasons": ["manual"], "held": 3,
               "restart_held": 2, "withdrawn": 0})
    );
    fx.shutdown().await;
}

/// P4a-R41: "Resume held jobs" (`app/queue.resume {held_only: true}`) releases the restart's
/// jobs and keeps your pause: nothing runs until a plain resume.
#[tokio::test(start_paused = true)] // spec: CORE-160, ARCH-022
async fn held_only_resume_keeps_your_pause() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(
            fx.submit(spec(Lane::Cpu, Priority::Agent, json!({})))
                .await
                .job_id,
        );
    }
    let (restarted, fresh, _) = restart(&fx).await;
    restarted.pause(Lane::Cpu, PauseReason::Manual).await;
    let state = restarted.release_restart_held(Lane::Cpu).await;
    assert!(state.paused, "{state:?}");
    assert_eq!(state.reasons, ["manual"]);
    assert_eq!((state.held, state.restart_held), (3, 0));
    for id in &ids {
        let job = until_summary(&restarted, &fx, id, |j| j.pause_reasons == ["manual"]).await;
        assert_eq!(job.substatus.as_deref(), Some("queue_paused"));
    }
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(fresh.started().is_empty(), "nothing runs while you pause");
    restarted.resume(Lane::Cpu, PauseReason::Manual).await;
    for id in &ids {
        assert_eq!(fx.finished(id).await.status, JobStatus::Succeeded);
    }
    fx.shutdown().await;
}

/// The test retry of [`Jobs::retry_batch`]: `job.retry {job_id}`'s rules (the retry origin),
/// then the tool call again, as the dispatcher runs it.
fn retry_with<'a>(
    fx: &'a Fx,
    caller: &'a JobCaller,
) -> impl Fn(JobId) -> JobFuture<'a, Result<JobSummary, ErrorBody>> {
    move |id: JobId| {
        Box::pin(async move {
            fx.jobs.retry_origin(caller, &id).await?;
            let row = fx.row(id.as_str()).await;
            let mut retry = fx.ctx_as(caller.caller.clone(), None, "t_retry");
            retry.retry_of = Some(id.clone());
            fx.jobs
                .submit(
                    &retry,
                    spec(row.lane, row.priority, json!({"gate_self": true})),
                )
                .await
        })
    }
}

/// `job.retry {batch_id}` (Appendix G): only the newest attempt at each index that ended
/// failed or cancelled; the retries keep the batch and the index; an `interrupted_unknown`
/// job and one that succeeded are refused with `job.not_retryable`.
#[tokio::test(start_paused = true)] // spec: CORE-160, CORE-146
async fn retry_by_batch_retries_only_ended_jobs() {
    let fx = Fx::suite().await;
    let batch = fx
        .jobs
        .submit_batch(
            &fx.ctx(),
            [
                json!({"fail": "disk_full"}),
                json!({}),
                json!({"fail": "unknown"}),
                json!({"gate": "never_0"}),
            ]
            .into_iter()
            .map(|args| spec(Lane::Cpu, Priority::Agent, args))
            .collect(),
        )
        .await
        .unwrap();
    let ids: Vec<String> = batch.iter().map(|j| j.job_id.clone()).collect();
    fx.finished(&ids[0]).await;
    fx.finished(&ids[1]).await;
    until_substatus(&fx, &ids[2], "interrupted_unknown").await;
    fx.jobs
        .cancel(&fx.ctx(), &JobId(ids[3].clone()))
        .await
        .unwrap();
    fx.finished(&ids[3]).await;
    let caller = fx.ctx();
    let out = fx
        .jobs
        .retry_batch(&caller, &batch[0].batch_id, retry_with(&fx, &caller))
        .await
        .unwrap();
    let retried: Vec<(&str, u32)> = out
        .retried
        .iter()
        .map(|r| (r.retry_of.as_str(), r.job.index))
        .collect();
    assert_eq!(retried, [(ids[0].as_str(), 0), (ids[3].as_str(), 3)]);
    for r in &out.retried {
        assert_eq!(r.job.batch_id, batch[0].batch_id, "a retry keeps its batch");
        assert_eq!(r.job.retry_of.as_deref(), Some(r.retry_of.as_str()));
    }
    let refused: Vec<(&str, &str)> = out
        .refused
        .iter()
        .map(|r| (r.job_id.as_str(), r.error.code.as_str()))
        .collect();
    assert_eq!(
        refused,
        [
            (ids[1].as_str(), "job.not_retryable"),
            (ids[2].as_str(), "job.not_retryable"),
        ]
    );
    assert!(
        out.refused[0].error.message.contains("succeeded"),
        "{:?}",
        out.refused[0]
    );
    assert!(
        out.refused[1].error.message.contains("interrupted_unknown"),
        "{:?}",
        out.refused[1]
    );
    let e = fx
        .jobs
        .retry_batch(&caller, "b_00000000000000ff", retry_with(&fx, &caller))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.unknown");
    fx.shutdown().await;
}

/// A second `job.retry {batch_id}` creates nothing new: the first call's originals are refused
/// as already retried, its still-running retries with their status; `job.retry {job_id}` of an
/// already retried job is refused too.
#[tokio::test(start_paused = true)] // spec: CORE-160, CORE-146
async fn retry_by_batch_twice_retries_nothing_new() {
    let fx = Fx::suite().await;
    let batch = fx
        .jobs
        .submit_batch(
            &fx.ctx(),
            (0..2)
                .map(|_| spec(Lane::Cpu, Priority::Agent, json!({"fail": "disk_full"})))
                .collect(),
        )
        .await
        .unwrap();
    for job in &batch {
        fx.finished(&job.job_id).await;
    }
    let caller = fx.ctx();
    let first = fx
        .jobs
        .retry_batch(&caller, &batch[0].batch_id, retry_with(&fx, &caller))
        .await
        .unwrap();
    assert_eq!(first.retried.len(), 2);
    assert!(first.refused.is_empty(), "{:?}", first.refused);
    let retries: Vec<String> = first.retried.iter().map(|r| r.job.job_id.clone()).collect();
    fx.handler.until_started(&retries[0]).await;
    let all_before = fx
        .jobs
        .list(&fx.ctx(), JobFilter::default())
        .await
        .unwrap()
        .0
        .len();
    let second = fx
        .jobs
        .retry_batch(&caller, &batch[0].batch_id, retry_with(&fx, &caller))
        .await
        .unwrap();
    assert!(second.retried.is_empty(), "{:?}", second.retried);
    let all_after = fx
        .jobs
        .list(&fx.ctx(), JobFilter::default())
        .await
        .unwrap()
        .0
        .len();
    assert_eq!(all_before, all_after, "nothing new");
    let refused: Vec<&str> = second.refused.iter().map(|r| r.job_id.as_str()).collect();
    assert_eq!(
        refused,
        [
            batch[0].job_id.as_str(),
            retries[0].as_str(),
            batch[1].job_id.as_str(),
            retries[1].as_str()
        ]
    );
    for r in &second.refused {
        assert_eq!(r.error.code, "job.not_retryable", "{r:?}");
    }
    assert!(
        second.refused[0]
            .error
            .message
            .contains(&format!("already retried as {}", retries[0])),
        "{:?}",
        second.refused[0]
    );
    assert!(
        second.refused[1].error.message.contains("executing"),
        "{:?}",
        second.refused[1]
    );
    assert!(
        second.refused[3].error.message.contains("waiting"),
        "{:?}",
        second.refused[3]
    );
    let e = fx
        .jobs
        .retry_origin(&fx.ctx(), &JobId(batch[1].job_id.clone()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "job.not_retryable");
    for id in &retries {
        fx.handler.open(id);
        fx.finished(id).await;
    }
    fx.shutdown().await;
}

/// P4a-R13: every job has a batch: one submit is a batch of one (`b_<16 hex>`, index 0); a
/// batch's jobs share it, in order.
#[tokio::test(start_paused = true)] // spec: CORE-117, CORE-160
async fn every_job_has_a_batch_and_an_index() {
    let fx = Fx::suite().await;
    let one = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let hex = one.batch_id.strip_prefix("b_").unwrap();
    assert!(
        hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{}",
        one.batch_id
    );
    assert_eq!(one.index, 0);
    let batch = fx
        .jobs
        .submit_batch(
            &fx.ctx(),
            (0..3)
                .map(|_| spec(Lane::Cpu, Priority::Agent, json!({})))
                .collect(),
        )
        .await
        .unwrap();
    assert_eq!(batch.iter().map(|j| j.index).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(batch.iter().all(|j| j.batch_id == batch[0].batch_id));
    assert_ne!(batch[0].batch_id, one.batch_id);
    let listed = fx
        .jobs
        .list(
            &fx.ctx(),
            JobFilter {
                batch_id: Some(batch[0].batch_id.clone()),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap()
        .0;
    assert_eq!(listed.len(), 3);
    for job in batch.iter().chain(std::iter::once(&one)) {
        fx.finished(&job.job_id).await;
    }
    fx.shutdown().await;
}

/// P4a-R12: `origin` is derived from the caller and typed.
#[tokio::test(start_paused = true)] // spec: CORE-117
async fn origin_follows_the_caller() {
    let fx = Fx::suite().await;
    let mut core = fx.ctx_as(CallerKind::Cli, None, "t_core");
    // A job the core submits itself has no tool call (as `comfy.inventory`'s).
    core.origin = None;
    for (caller, want) in [
        (
            fx.ctx_as(CallerKind::Ui("test".into()), None, "t_ui"),
            json!({"surface": "panel", "id": "test"}),
        ),
        (
            fx.ctx_as(CallerKind::Cli, None, "t_cli"),
            json!({"surface": "cli"}),
        ),
        (
            fx.ctx_as(CallerKind::Mcp, None, "t_mcp"),
            json!({"surface": "mcp"}),
        ),
        (
            fx.ctx_as(CallerKind::Assistant, None, "t_as"),
            json!({"surface": "agent_session"}),
        ),
        (core, json!({"surface": "core"})),
    ] {
        let job = fx
            .jobs
            .submit(&caller, spec(Lane::Cpu, Priority::Agent, json!({})))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&job.origin).unwrap(),
            want,
            "{}",
            caller.caller
        );
        let done = fx.finished(&job.job_id).await;
        assert_eq!(serde_json::to_value(&done.origin).unwrap(), want);
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-117, CORE-009
async fn jobs_log_lines_carry_origin() {
    let fx = Fx::workspace().await;
    let app = fx.ctx_as(CallerKind::Ui("image".into()), None, "t_log");
    let job = fx
        .jobs
        .submit(&app, spec(Lane::Cpu, Priority::User, json!({})))
        .await
        .unwrap();
    fx.finished(&job.job_id).await;
    let lines: Vec<Value> = fx
        .log_lines()
        .into_iter()
        .filter(|l| l["job_id"] == job.job_id.as_str())
        .collect();
    assert!(!lines.is_empty());
    for line in &lines {
        assert_eq!(
            line["origin"],
            json!({"surface": "panel", "id": "image"}),
            "{line}"
        );
        let keys: Vec<&str> = line
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let at = keys.iter().position(|k| *k == "origin").unwrap();
        assert_eq!(
            keys[at - 1],
            "priority",
            "origin follows priority: {keys:?}"
        );
    }
    fx.shutdown().await;
}

/// A surface this build doesn't know (a later plan's) reads back unchanged.
#[tokio::test(start_paused = true)] // spec: CORE-117
async fn an_unknown_surface_round_trips() {
    let fx = Fx::suite().await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    fx.finished(&job.job_id).await;
    let stored = json!({"surface": "later_thing", "id": "x", "step": "2"});
    let db = rusqlite::Connection::open(fx.env.env.suite_state_db()).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute(
        "UPDATE jobs SET origin = ?1 WHERE job_id = ?2",
        rusqlite::params![stored.to_string(), job.job_id],
    )
    .unwrap();
    drop(db);
    let read = fx
        .jobs
        .get(&fx.ctx(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(
        read.origin.as_ref().map(|o| o.surface.clone()),
        Some(lil_core::jobs::Surface::Other("later_thing".into()))
    );
    assert_eq!(serde_json::to_value(&read.origin).unwrap(), stored);
    fx.shutdown().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-117
async fn status_seq_keeps_the_first_and_newest_63() {
    let fx = Fx::suite().await;
    running_gate(&fx, Lane::Cpu, "never").await;
    let job = fx.submit(spec(Lane::Cpu, Priority::Agent, json!({}))).await;
    let db = fx.states.suite_db();
    for n in 1..=100_i64 {
        let to = if n % 2 == 1 {
            Substatus::QueuePaused
        } else {
            Substatus::Waiting
        };
        record::update(&db, &job.job_id, record::Change::to(to), n)
            .await
            .unwrap()
            .unwrap();
    }
    let row = fx.row(&job.job_id).await;
    assert_eq!(row.status_seq.len(), 64);
    assert_eq!(row.status_seq[0].status, JobStatus::Queued);
    assert_eq!(row.status_seq[0].substatus.as_deref(), Some("waiting"));
    assert_eq!(
        row.status_seq[0].at,
        lil_schema::time::rfc3339(row.created_utc)
    );
    // The newest 63: changes 38 to 100.
    assert_eq!(row.status_seq[1].at, lil_schema::time::rfc3339(38));
    assert_eq!(row.status_seq[63].at, lil_schema::time::rfc3339(100));
    assert_eq!(row.status_seq[63].substatus.as_deref(), Some("waiting"));
    // A change that keeps the status and substatus adds nothing.
    let c = record::Change {
        watchdog: Some("slow".into()),
        ..record::Change::default()
    };
    record::update(&db, &job.job_id, c, 101)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fx.row(&job.job_id).await.status_seq, row.status_seq);
    fx.shutdown().await;
}

/// The stall rule (ARCH-072): a running job with no sign of life for 30 s is `unresponsive`
/// (published as `{kind: "unresponsive", on}` on `job:<id>`) and keeps running; a progress
/// ends it.
#[tokio::test(start_paused = true)] // spec: ARCH-072, CORE-117
async fn unresponsive_flips_after_30_s_and_back() {
    let fx = Fx::suite().await;
    let job = fx
        .submit(spec(
            Lane::Cpu,
            Priority::Agent,
            json!({"gate": "end", "progress_on": "tick"}),
        ))
        .await;
    let topic = format!("job:{}", job.job_id);
    fx.subscribe(&topic).await;
    fx.handler.until_started(&job.job_id).await;
    fx.clock.advance(Duration::from_secs(29));
    fx.jobs.check_unresponsive();
    let summary = fx
        .jobs
        .get(&fx.ctx(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert!(!summary.unresponsive, "29 s is not yet 30");
    fx.clock.advance(Duration::from_secs(1));
    fx.jobs.check_unresponsive();
    let on = fx
        .event_until(|d| d["kind"] == "unresponsive" && d["on"] == true)
        .await
        .unwrap();
    assert_eq!(on, json!({"kind": "unresponsive", "on": true}));
    let summary = fx
        .jobs
        .get(&fx.ctx(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert!(summary.unresponsive);
    assert_eq!(summary.status, JobStatus::Running, "nothing is cancelled");
    fx.handler.open("tick");
    fx.event_until(|d| d["kind"] == "unresponsive" && d["on"] == false)
        .await
        .unwrap();
    let summary = fx
        .jobs
        .get(&fx.ctx(), &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert!(!summary.unresponsive);
    assert!(summary.last_progress_at.is_some());
    fx.handler.open("end");
    assert_eq!(fx.finished(&job.job_id).await.status, JobStatus::Succeeded);
    fx.shutdown().await;
}

/// The job tools' plan-4a arguments over the pipe of an in-process core: `job.cancel` takes
/// exactly one selector (`params.invalid` for none or two), `job.retry` one of `job_id` and
/// `batch_id`, and `test.job {batch}` makes one batch that `job.retry {batch_id}` retries.
#[test] // spec: CORE-160, CORE-146
fn job_tools_take_exactly_one_selector() {
    let env = TestEnv::new();
    let _core = lil_core::start_in_thread(lil_core::CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..lil_core::CoreConfig::for_env(env.env.clone()).unwrap()
    });
    let mut pipe = lil_testkit::RawPipe::connect(&env);
    assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
    let made = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "gated": true, "batch": 2}),
        Some("b1"),
    );
    let jobs = made["jobs"].as_array().unwrap().clone();
    assert_eq!(jobs.len(), 2, "{made}");
    let batch_id = jobs[0]["batch_id"].as_str().unwrap().to_string();
    let ids: Vec<String> = jobs
        .iter()
        .map(|j| j["job_id"].as_str().unwrap().to_string())
        .collect();
    let again = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "gated": true, "batch": 2}),
        Some("b1"),
    );
    assert_eq!(again["jobs"], made["jobs"], "one key, one batch");
    pipe.tool_ok(
        "job.cancel",
        json!({ "batch_id": batch_id }),
        Some("c-first"),
    );
    for id in &ids {
        let waited = pipe.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 30}), None);
        assert_eq!(waited["job"]["status"], "cancelled", "{waited}");
    }
    for (n, args) in [
        json!({}),
        json!({"job_id": ids[0], "ids": [ids[0]]}),
        json!({"batch_id": batch_id, "all_mine": true}),
        json!({"all_mine": false}),
    ]
    .into_iter()
    .enumerate()
    {
        let refused = pipe.tool("job.cancel", args.clone(), Some(&format!("c{n}")));
        assert_eq!(
            refused["error"]["code"], "params.invalid",
            "{args}: {refused}"
        );
    }
    for (n, args) in [json!({}), json!({"job_id": ids[0], "batch_id": batch_id})]
        .into_iter()
        .enumerate()
    {
        let refused = pipe.tool("job.retry", args.clone(), Some(&format!("r{n}")));
        assert_eq!(
            refused["error"]["code"], "params.invalid",
            "{args}: {refused}"
        );
    }
    let retried = pipe.tool_ok("job.retry", json!({ "batch_id": batch_id }), Some("rb"));
    let retried_of: Vec<&str> = retried["retried"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["retry_of"].as_str().unwrap())
        .collect();
    assert_eq!(retried_of, [ids[0].as_str(), ids[1].as_str()], "{retried}");
    for (n, r) in retried["retried"].as_array().unwrap().iter().enumerate() {
        assert_eq!(r["job"]["batch_id"], batch_id.as_str());
        assert_eq!(r["job"]["index"], n);
    }
    // The same key gives back the same answer; a new one retries nothing new.
    let again = pipe.tool_ok("job.retry", json!({ "batch_id": batch_id }), Some("rb"));
    assert_eq!(again, retried);
    let twice = pipe.tool_ok("job.retry", json!({ "batch_id": batch_id }), Some("rb2"));
    assert_eq!(twice["retried"], json!([]), "{twice}");
    let alone = pipe.tool("job.retry", json!({"job_id": ids[0]}), Some("r1"));
    assert_eq!(alone["error"]["code"], "job.not_retryable", "{alone}");
    let cancelled = pipe.tool_ok("job.cancel", json!({ "batch_id": batch_id }), Some("cb"));
    assert_eq!(
        cancelled["jobs"].as_array().unwrap().len(),
        4,
        "{cancelled}"
    );
    let listed = pipe.tool_ok("jobs.list", json!({}), None);
    let lanes: Vec<&str> = listed["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["lane"].as_str().unwrap())
        .collect();
    assert_eq!(lanes, ["gpu-heavy", "cpu"], "{listed}");
}

/// A drain and a restart (a reboot for an update, P4a-R41's case): the shutdown step leaves a
/// queued job that never reached an engine queued, with its reservation, and the next start
/// holds it instead of failing it; a resume then runs it.
#[tokio::test(start_paused = true)] // spec: ARCH-022, ARCH-021, CORE-160
async fn a_drain_and_restart_holds_a_queued_job() {
    let fx = Fx::suite().await;
    let lifecycle = Lifecycle::new(fx.clock.clone(), Duration::from_secs(600));
    lifecycle.add_blocker(fx.jobs.clone());
    lifecycle.add_shutdown_step(fx.jobs.persist_interrupted_step());
    fx.jobs.stop_on_drain(lifecycle.drain_token());
    let running = running_gate(&fx, Lane::Cpu, "run").await;
    let mut caller = fx.ctx();
    caller.reservation = Some("rv_drain".into());
    let queued = fx
        .jobs
        .submit(&caller, spec(Lane::Cpu, Priority::Agent, json!({})))
        .await
        .unwrap();
    lifecycle.begin_drain(DrainReason::Requested);
    let shutdown = {
        let lifecycle = lifecycle.clone();
        tokio::spawn(async move { lifecycle.run_shutdown().await })
    };
    fx.handler.open("run");
    shutdown.await.unwrap();
    assert_eq!(fx.row(&running.job_id).await.status, JobStatus::Succeeded);
    let left = fx.row(&queued.job_id).await;
    assert_eq!(
        (left.status, left.substatus),
        (JobStatus::Queued, Some(Substatus::Waiting))
    );
    let (restarted, fresh, marked) = restart(&fx).await;
    assert!(marked.is_empty(), "nothing failed: {marked:?}");
    let held = restarted
        .get(&fx.ctx(), &JobId(queued.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(
        (held.status, held.substatus.as_deref()),
        (JobStatus::Queued, Some("queue_paused"))
    );
    assert_eq!(held.pause_reasons, ["restart"]);
    assert_eq!(
        fx.row(&queued.job_id).await.reservation.as_deref(),
        Some("rv_drain")
    );
    restarted.release_restart_held(Lane::Cpu).await;
    fresh.until_started(&queued.job_id).await;
    assert_eq!(
        fx.finished(&queued.job_id).await.status,
        JobStatus::Succeeded
    );
    fx.shutdown().await;
}
