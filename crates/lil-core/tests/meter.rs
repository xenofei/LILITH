//! The `meter` module in process (CORE-013, CORE-064, CORE-065, CORE-069, CORE-071,
//! CORE-081–083, CORE-088, CORE-146, ARCH-081): write-ahead usage rows and their lines, the
//! record's fields, loose and workspace logs, a dead core's rows, per-job energy attribution and
//! idle baselines, local-work records, job outcomes, the node install's one line, stage 8 and
//! summaries.
//!
//! Jobs run through a real `Jobs` with a test handler that reports the execution windows its
//! arguments name; the telemetry is a manual one over a fake hardware script, sampled once per
//! nominal second of a `FakeClock`, so a window `[start, end]` in seconds covers the samples
//! taken at seconds `start + 1` to `end`.
#![allow(
    clippy::result_large_err,
    reason = "JobFailure holds the wire ErrorBody, as in lil-core itself"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::engines::Engines;
use lil_core::engines::comfyui::install::{self, ExactVersion, NodeId};
use lil_core::events::EventBus;
use lil_core::jobs::{
    EngineKind, ExecWindow, JobCaller, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobId,
    JobOrigin, JobSpec, JobStatus, Jobs, Lane, Priority, SubmittedSlot, cancelled_error,
};
use lil_core::meter::record::{self as usage_record};
use lil_core::meter::{
    Attribution, CapCheck, Caps, Meter, MeterSettle, SummaryQuery, UsageCtx, UsageDraft,
    UsageStatus, attribute, register_meter_tools,
};
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::telemetry::{
    EnergySlice, HwSources, JobEnergy, JobEnergyQuery, PowerSettings, Telemetry, TickMode,
};
use lil_core::tools::{
    BillingBasis, CallCtx, Estimate, MeterStage, Registry, SettleInput, register_telemetry_tools,
};
use lil_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_platform::fs::SystemProbe;
use lil_platform::sockets::OsSockets;
use lil_platform::{Clock, IdSource};
use lil_protocol::{CallerKind, ErrorBody, Outcome, ToolOutput, codes};
use lil_schema::money::{Currency, Money};
use lil_testkit::{FakeClock, FakeComfy, FakeHwScript, SeqIds, TestEnv, ensure_built};
use serde_json::{Map, Value, json};

const DEADLINE: Duration = Duration::from_secs(30);
const KIND: &str = "meter.test_job";

// ---------------------------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------------------------

/// Runs `meter.test_job` jobs: reports each `windows` entry (`[start_ms, end_ms, [pids]]`),
/// then ends as `end` says: `ok` (the default), `fail`, `unknown`, `cancel` (waits for its
/// cancel), or `interrupt_once` (the handler's first such attempt is interrupted by its engine,
/// later ones work). A working run with `cached: "all"` was all cached, with `"some"` partly;
/// with `record: true` it leaves a record, which fails its Record step (the rig has no
/// workspace).
#[derive(Default)]
struct WindowHandler {
    started: Mutex<Vec<String>>,
    interrupted: std::sync::atomic::AtomicBool,
}

impl WindowHandler {
    async fn attempt(&self, job: &JobCtx) -> Result<JobDone, JobFailure> {
        let args = &job.spec.args;
        for w in args["windows"].as_array().into_iter().flatten() {
            job.add_window(ExecWindow {
                engine_pids: w[2]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.as_u64().and_then(|p| u32::try_from(p).ok()))
                    .collect(),
                start: Duration::from_millis(w[0].as_u64().unwrap()),
                end: Duration::from_millis(w[1].as_u64().unwrap()),
                ok: true,
            });
        }
        self.started.lock().unwrap().push(job.id.to_string());
        let error = |code: &lil_protocol::CodeSpec| ErrorBody::new(code, "on purpose", "");
        match args["cached"].as_str() {
            Some("all") => {
                job.mark_cached(&["1".to_string(), "2".to_string()]);
                job.set_all_cached();
            }
            Some("some") => job.mark_cached(&["1".to_string()]),
            _ => {}
        }
        let first = || {
            !self
                .interrupted
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        };
        match args["end"].as_str().unwrap_or("ok") {
            "interrupt_once" if first() => Err(JobFailure::Interrupted {
                error: error(&codes::COMFYUI_UNREACHABLE),
                salvaged: Vec::new(),
            }),
            "fail" => Err(JobFailure::Final(error(&codes::COMFYUI_EXECUTION_FAILED))),
            "unknown" => Err(JobFailure::Unknown(error(&codes::COMFYUI_UNREACHABLE))),
            "cancel" => {
                let mut cancel = job.cancel.clone();
                let _ = cancel.wait_for(|c| *c).await;
                Err(JobFailure::Final(cancelled_error(&job.trace_id)))
            }
            _ => Ok(JobDone {
                structured: json!({}),
                artifacts: Vec::new(),
                record: (args["record"] == true).then(record_plan),
                windows: Vec::new(),
            }),
        }
    }
}

/// A candidate run of `prop_meter` in `sketchbook` with no files: a job without a workspace
/// that leaves it fails its Record step.
fn record_plan() -> lil_core::pipeline::RecordPlan {
    lil_core::pipeline::RecordPlan {
        branch: lil_core::pipeline::RecordBranch::CandidateRun {
            run_id: "r_20270115_0800".to_string(),
        },
        project: "sketchbook".to_string(),
        asset_id: "prop_meter".to_string(),
        commit: lil_core::store::CommitOp {
            outputs: Vec::new(),
            moved: Vec::new(),
            removed: Vec::new(),
            manifest: None,
            job_line: None,
            progress: None,
        },
        progress_entry: "a job's run".to_string(),
        refs: Vec::new(),
        version_guard: None,
    }
}

impl JobHandler for WindowHandler {
    fn kind(&self) -> &'static str {
        KIND
    }

    /// Its `key` argument.
    fn estimate_key(&self, args: &Value) -> Option<String> {
        args.get("key").and_then(Value::as_str).map(str::to_string)
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(self.attempt(job))
    }

    fn cancel_in_engine<'a>(&'a self, _job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// The suite's state, the jobs, a manual telemetry and the meter, all on one fake clock.
struct Rig {
    meter: Arc<Meter>,
    jobs: Arc<Jobs>,
    telemetry: Arc<Telemetry>,
    events: Arc<EventBus>,
    clock: Arc<FakeClock>,
    ids: Arc<dyn IdSource>,
    fp: Arc<FailPoints>,
    hub: Arc<WorkspaceHub>,
    handler: Arc<WindowHandler>,
    states: Arc<StateRegistry>,
    power: PowerSettings,
    /// Last: the folder goes only once nothing holds a file in it.
    env: TestEnv,
}

impl Rig {
    fn new(script: &Value) -> Rig {
        Rig::with(TestEnv::new(), script, PowerSettings::default())
    }

    fn with(env: TestEnv, script: &Value, power: PowerSettings) -> Rig {
        let states = Arc::new(
            StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
        );
        let clock = FakeClock::new();
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let events = Arc::new(EventBus::new(dyn_clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let fp = FailPoints::none();
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            fp.clone(),
            Arc::new(SystemProbe),
            lil_core::watch::WatchConfig::default(),
        );
        let jobs = Jobs::new(
            states.clone(),
            hub.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            fp.clone(),
        );
        let telemetry = Telemetry::start(
            HwSources::from_fake(FakeHwScript::new(script.clone()).sources()),
            env.env.local_dir().join("telemetry.db"),
            dyn_clock.clone(),
            power.clone(),
            events.clone(),
            TickMode::Manual,
        );
        let handler = Arc::new(WindowHandler::default());
        jobs.register(handler.clone());
        let rig = Rig {
            meter: Meter::new(
                states.clone(),
                env.env.clone(),
                dyn_clock,
                ids.clone(),
                events.clone(),
                telemetry.clone(),
                power.clone(),
                fp.clone(),
            ),
            jobs,
            telemetry,
            events,
            clock,
            ids,
            fp,
            hub,
            handler,
            states,
            power,
            env,
        };
        rig.jobs.add_completion(rig.meter.clone());
        rig
    }

    /// Another meter over the same databases: the next core run's.
    fn restarted_meter(&self) -> Arc<Meter> {
        Meter::new(
            self.states.clone(),
            self.env.env.clone(),
            self.clock.clone(),
            self.ids.clone(),
            self.events.clone(),
            self.telemetry.clone(),
            self.power.clone(),
            self.fp.clone(),
        )
    }

    /// `n` samples, a second of the clock after each.
    async fn run(&self, n: u32) {
        for _ in 0..n {
            self.telemetry.tick(1).await;
            self.clock.advance(Duration::from_secs(1));
        }
    }

    fn caller(&self, caller: CallerKind) -> JobCaller {
        JobCaller {
            idem_scope: format!("{caller}|none"),
            caller,
            workspace: None,
            trace_id: "t_meter".into(),
            idem_key: None,
            call_id: "c_0000000000000001".into(),
            reservation: None,
            submitted: Arc::new(SubmittedSlot::new()),
            origin: Some(JobOrigin {
                tool: "test.render".into(),
                arguments: json!({}),
            }),
            retry_of: None,
        }
    }

    /// Submits a job with `args` under a fresh idempotency key (as a tool call's job has) and
    /// waits until it is done; its final summary.
    async fn job(&self, args: Value) -> lil_core::jobs::JobSummary {
        let mut caller = self.caller(CallerKind::Cli);
        caller.idem_key = Some(self.ids.id("k"));
        let job = self.jobs.submit(&caller, spec(args)).await.unwrap();
        self.wait(&job.job_id).await
    }

    async fn wait(&self, id: &str) -> lil_core::jobs::JobSummary {
        let caller = self.caller(CallerKind::Cli);
        let (job, done) = self
            .jobs
            .wait(&caller, &JobId(id.to_string()), DEADLINE)
            .await
            .unwrap();
        assert!(done, "the job did not end: {job:?}");
        job
    }

    fn loose(&self) -> PathBuf {
        self.env.env.local_dir().join("usage-loose.jsonl")
    }

    async fn energy(&self, job_id: &str) -> Option<JobEnergy> {
        self.telemetry
            .job_energy(JobEnergyQuery {
                job_ids: Some(vec![job_id.to_string()]),
                ..JobEnergyQuery::default()
            })
            .await
            .unwrap()
            .into_iter()
            .next()
    }
}

fn spec(args: Value) -> JobSpec {
    JobSpec {
        kind: KIND,
        lane: Lane::Cpu,
        priority: Priority::Agent,
        module: "test".into(),
        args,
        project: None,
        asset_id: None,
        doc_id: None,
        expected_secs: 10,
        expected_output_bytes: 0,
        vram_need_mib: None,
        engine: Some(EngineKind::Test),
        group: None,
        external_side_effects: false,
    }
}

/// Every line of the JSON-lines file `path` (none when it is missing).
fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn lines_of(path: &Path, key: &str, value: &str) -> Vec<Value> {
    lines(path)
        .into_iter()
        .filter(|l| l[key] == value)
        .collect()
}

fn usd(s: &str) -> (Money, Currency) {
    (
        Money::parse_decimal(s).unwrap(),
        Currency::parse("USD").unwrap(),
    )
}

fn draft(provider: &str, basis: BillingBasis, est: Option<(Money, Currency)>) -> UsageDraft {
    let mut units = Map::new();
    units.insert("requests".into(), json!(1));
    UsageDraft {
        tool: "test.external".into(),
        provider: provider.into(),
        model: Some("m-1".into()),
        route: "direct".into(),
        basis,
        units,
        est_cost: est,
        account: None,
        raw_usage: Some(json!({"total_tokens": 12})),
        attribution: Attribution::Suite,
        status: UsageStatus::Pending,
        latency_ms: None,
    }
}

fn uctx(workspace: Option<Arc<OpenWorkspace>>, call_id: &str) -> UsageCtx {
    UsageCtx {
        workspace,
        project: Some("sketchbook".into()),
        asset: None,
        job_id: None,
        call_id: call_id.into(),
        caller_kind: "cli".into(),
        reservation: None,
    }
}

/// A GPU reading at `power_mw` whose counter is at `energy_mj`, with `sm` per pid.
fn gpu(power_mw: u64, energy_mj: u64, util: u64, sm: Value) -> Value {
    json!({"name": "Fake GPU", "driver": "999.99", "util_pct": util, "power_mw": power_mw,
           "energy_mj": energy_mj, "temp_c": 60, "sm_clock_mhz": 1800, "mem_clock_mhz": 9000,
           "throttle": 0, "vram_total_mib": 16384, "vram_used_mib": 2048, "sm_by_pid": sm})
}

/// 40 seconds of a GPU at 150 W (its counter rising 150 000 mJ a sample) with the engine
/// (pid 200) at 750 ‰ of the SMs and 300 ms of CPU a second, and the CPU package metered at
/// 30 W.
fn busy() -> Value {
    let readings: Vec<Value> = (1..=40)
        .map(|n| gpu(150_000, n * 150_000, 90, json!([[200, 750], [100, 250]])))
        .collect();
    let cpu: Vec<Value> = (0..=40_u64)
        .map(|n| json!({"package_energy_pwh": n * 8_333_333_333, "package_power_mw": 30_000}))
        .collect();
    let sys: Vec<Value> = (1..=40_u64)
        .map(|n| {
            json!({"cpu_permille": 500, "per_core_permille": [500, 500],
                   "ram_total_mib": 32768, "ram_used_mib": 8192, "procs": [[200, n * 300, 500]]})
        })
        .collect();
    json!({"gpu": readings, "gpu_mem": [[]], "cpu_energy": cpu, "sys": sys})
}

/// A CallCtx for `caller` in no workspace, on the rig's jobs.
fn call_ctx(rig: &Rig, call_id: &str) -> CallCtx {
    let mut ctx = common::ctx(&rig.env, CallerKind::Cli, &rig.events);
    ctx.jobs = rig.jobs.clone();
    ctx.call_id = call_id.to_string();
    ctx.tool = "test.meter_external".into();
    ctx
}

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

// ---------------------------------------------------------------------------------------------
// Write-ahead (CORE-071).
// ---------------------------------------------------------------------------------------------

/// The row is committed before `begin` returns (the call is sent after it), and there is no
/// line yet; the line comes with `finish`, and only then does the row go.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-071, CORE-069
async fn pending_row_is_written_before_the_call_and_the_line_after() {
    let rig = Rig::new(&busy());
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let (seen, db, loose) = (seen.clone(), rig.env.env.suite_state_db(), rig.loose());
        rig.fp.on(
            "meter.after_pending",
            Box::new(move || {
                let conn = rusqlite::Connection::open(&db).unwrap();
                let rows: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM usage_pending WHERE state = 'pending'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                seen.lock().unwrap().push(format!(
                    "pending rows {rows}, lines {}",
                    lines(&loose).len()
                ));
                Ok(())
            }),
        );
    }
    {
        let (seen, db, loose) = (seen.clone(), rig.env.env.suite_state_db(), rig.loose());
        rig.fp.on(
            "meter.after_usage_line",
            Box::new(move || {
                let conn = rusqlite::Connection::open(&db).unwrap();
                let state: String = conn
                    .query_row("SELECT state FROM usage_pending", [], |r| r.get(0))
                    .unwrap();
                seen.lock()
                    .unwrap()
                    .push(format!("row {state}, lines {}", lines(&loose).len()));
                Ok(())
            }),
        );
    }
    let id = rig
        .meter
        .begin(
            &uctx(None, "c_1"),
            draft("p", BillingBasis::Payg, Some(usd("0.5"))),
        )
        .await
        .unwrap();
    assert!(id.starts_with("us_") && id.len() == 19, "{id}");
    let row = usage_record::get(rig.states.suite(), &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.record["status"], "pending");
    assert!(
        lines(&rig.loose()).is_empty(),
        "no line before the call ends"
    );

    let seq = rig
        .meter
        .finish(&id, Some(usd("0.4")), "provider_reported", "succeeded", 120)
        .await
        .unwrap();
    assert_eq!(seq, 1);
    assert_eq!(
        *seen.lock().unwrap(),
        ["pending rows 1, lines 0", "row logging, lines 1"]
    );
    assert!(
        usage_record::get(rig.states.suite(), &id)
            .await
            .unwrap()
            .is_none()
    );
    let all = lines(&rig.loose());
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["status"], "succeeded");
    assert_eq!(all[0]["cost"], "0.4");
    assert_eq!(all[0]["est_cost"], "0.5");
    assert_eq!(all[0]["latency_ms"], 120);
}

/// A core that died after the row (as `meter.after_pending=abort` would) leaves it; the next
/// run writes it as one `status: "pending"` line, once. A core that died after the line but
/// before the row went (`meter.after_usage_line`) gets no second line.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-071, CORE-088
async fn meter_after_pending_abort_leaves_a_pending_line_after_restart() {
    let rig = Rig::new(&busy());
    // The first run: a row, and then nothing (it died before sending).
    rig.meter
        .begin(
            &uctx(None, "c_1"),
            draft("p", BillingBasis::Payg, Some(usd("0.5"))),
        )
        .await
        .unwrap();
    // A row this run still owns is never recovered by it.
    assert_eq!(rig.meter.recover().await.unwrap(), 0);
    let next = rig.restarted_meter();
    assert_eq!(next.recover().await.unwrap(), 1);
    let all = lines(&rig.loose());
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0]["status"], "pending");
    assert_eq!(all[0]["est_cost"], "0.5");
    assert_eq!(next.recover().await.unwrap(), 0, "recovered once");
    assert_eq!(rig.restarted_meter().recover().await.unwrap(), 0);

    // A crash between the line and the row's removal.
    rig.fp.on(
        "meter.after_usage_line",
        Box::new(|| Err(std::io::Error::other("as a crash"))),
    );
    let id = next
        .begin(
            &uctx(None, "c_2"),
            draft("p", BillingBasis::Payg, Some(usd("1"))),
        )
        .await
        .unwrap();
    next.finish(&id, Some(usd("1")), "provider_reported", "succeeded", 5)
        .await
        .unwrap();
    assert!(
        usage_record::get(rig.states.suite(), &id)
            .await
            .unwrap()
            .is_some()
    );
    let third = rig.restarted_meter();
    assert_eq!(third.recover().await.unwrap(), 0, "the line was already in");
    assert!(
        usage_record::get(rig.states.suite(), &id)
            .await
            .unwrap()
            .is_none()
    );
    let all = lines(&rig.loose());
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[1]["status"], "succeeded");
}

/// Ruling P3-R27: a paid outside prompt's `outside_paid` row is its write-ahead record, so the
/// meter writes no row of its own, and a core that stopped between the line and the row's mark
/// writes no second line when it sees the prompt's end again.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-071, CORE-093
async fn an_outside_prompt_has_one_line_and_no_row_of_the_meters_own() {
    let rig = Rig::new(&busy());
    let outside = |prompt_id: &str| {
        let mut d = draft("comfy-org", BillingBasis::PrepaidCredits, None);
        d.tool = "comfyui.outside_prompt".into();
        d.route = "comfy-partner".into();
        d.raw_usage = Some(json!({"prompt_id": prompt_id, "partner_nodes": ["PaidNode"]}));
        d.attribution = Attribution::Unattributed;
        d.status = UsageStatus::Succeeded;
        d.latency_ms = Some(40);
        d
    };
    // A begun row would fail here, and its call would get no line.
    rig.fp.on(
        "meter.after_pending",
        Box::new(|| {
            Err(std::io::Error::other(
                "no usage_pending row for an outside prompt",
            ))
        }),
    );
    lil_core::meter::UsageSink::record_outside(
        &*rig.meter,
        "p-1",
        outside("p-1"),
        uctx(None, "c_1"),
    )
    .await
    .unwrap();
    // The next core sees the same end again (its row was never marked).
    let next = rig.restarted_meter();
    lil_core::meter::UsageSink::record_outside(&*next, "p-1", outside("p-1"), uctx(None, "c_2"))
        .await
        .unwrap();
    lil_core::meter::UsageSink::record_outside(&*next, "p-2", outside("p-2"), uctx(None, "c_3"))
        .await
        .unwrap();
    let all = lines(&rig.loose());
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[0]["raw_usage"]["prompt_id"], "p-1");
    assert_eq!(all[0]["status"], "succeeded");
    assert_eq!(all[0]["attribution"], "unattributed");
    assert_eq!(all[1]["raw_usage"]["prompt_id"], "p-2");
    assert_eq!(
        rig.restarted_meter().recover().await.unwrap(),
        0,
        "no row was left"
    );
}

// ---------------------------------------------------------------------------------------------
// The record (CORE-069).
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-069, CORE-013
async fn record_fields_match_appendix_f_and_money_is_decimal_strings() {
    let rig = Rig::new(&busy());
    let id = rig
        .meter
        .begin(
            &uctx(None, "c_1"),
            draft("p", BillingBasis::Payg, Some(usd("0.0087"))),
        )
        .await
        .unwrap();
    rig.meter
        .finish(
            &id,
            Some(usd("0.009")),
            "computed_from_usage",
            "succeeded",
            7,
        )
        .await
        .unwrap();
    let line = lines(&rig.loose()).remove(0);
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
            "ts",
            "workspace",
            "project",
            "asset",
            "job_id",
            "tool",
            "caller_kind",
            "account",
            "provider",
            "model",
            "route",
            "service_tier",
            "region",
            "billing_basis",
            "requests",
            "raw_usage",
            "est_cost",
            "cost",
            "energy_cost",
            "cost_source",
            "price_version",
            "currency",
            "latency_ms",
            "status",
            "attribution"
        ]
    );
    assert_eq!(
        line["est_cost"],
        json!("0.0087"),
        "money is a decimal string"
    );
    assert_eq!(line["cost"], json!("0.009"));
    assert_eq!(line["currency"], "USD");
    assert_eq!(line["cost_source"], "computed_from_usage");
    assert_eq!(line["billing_basis"], "payg");
    assert_eq!(line["workspace"], "loose");
    assert_eq!(line["attribution"], "suite");
    assert_eq!(line["raw_usage"], json!({"total_tokens": 12}));
    assert!(
        lil_core::telemetry::parse_ts(line["ts"].as_str().unwrap()).is_some(),
        "{line}"
    );
}

/// The account is the opaque id the caller gave, never anything else; without one it is null.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-069
async fn account_is_an_opaque_id() {
    let rig = Rig::new(&busy());
    let mut with = draft("p", BillingBasis::Payg, None);
    with.account = Some("key_7f3a".into());
    lil_core::meter::UsageSink::record(&*rig.meter, with, uctx(None, "c_1")).await;
    lil_core::meter::UsageSink::record(
        &*rig.meter,
        draft("p", BillingBasis::Payg, None),
        uctx(None, "c_2"),
    )
    .await;
    let all = lines(&rig.loose());
    assert_eq!(all[0]["account"], "key_7f3a");
    assert_eq!(all[1]["account"], Value::Null);
    assert!(
        all.iter().all(|l| !l.to_string().contains('@')),
        "no address in a usage line: {all:?}"
    );
}

/// No workspace: `usage-loose.jsonl` with `workspace: "loose"`; with one, its
/// `registry/usage.<machine>.<yyyy-mm>.jsonl`, and the row in its `state.db`.
#[tokio::test(flavor = "multi_thread")] // spec: ARCH-081, ARCH-099
async fn loose_calls_go_to_usage_loose() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("meter");
    env.register_workspace(&ws, true);
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    let open = rig.hub.open(&ws_ref(&ws)).await.unwrap();

    lil_core::meter::UsageSink::record(
        &*rig.meter,
        draft("p", BillingBasis::Payg, Some(usd("1"))),
        uctx(None, "c_1"),
    )
    .await;
    let id = rig
        .meter
        .begin(
            &uctx(Some(open.clone()), "c_2"),
            draft("p", BillingBasis::Payg, Some(usd("2"))),
        )
        .await
        .unwrap();
    assert!(
        usage_record::get(&open.state, &id).await.unwrap().is_some(),
        "the row is in the workspace's state.db"
    );
    assert!(
        usage_record::get(rig.states.suite(), &id)
            .await
            .unwrap()
            .is_none()
    );
    rig.meter.finish_call("c_2", "succeeded").await.unwrap();

    let loose = lines(&rig.loose());
    assert_eq!(loose.len(), 1, "{loose:?}");
    assert_eq!(loose[0]["workspace"], "loose");
    assert_eq!(loose[0]["machine"], "testmachine");
    let month = lil_schema::time::year_month(1_800_000_000);
    let log = ws.join(format!("registry/usage.testmachine.{month}.jsonl"));
    let in_ws = lines(&log);
    assert_eq!(in_ws.len(), 1, "{}", log.display());
    assert_eq!(in_ws[0]["workspace"], open.ws.id.as_str());
    assert_eq!(in_ws[0]["cost"], "2", "settled at its estimate");
    assert_eq!(in_ws[0]["cost_source"], "estimated");
    drop(open);
}

// ---------------------------------------------------------------------------------------------
// Attribution (CORE-065).
// ---------------------------------------------------------------------------------------------

#[test] // spec: CORE-065
fn attribution_splits_gpu_by_sm_share_and_cpu_by_cpu_time() {
    let window = ExecWindow {
        engine_pids: vec![200, 201],
        start: Duration::from_secs(100),
        end: Duration::from_secs(110),
        ok: true,
    };
    let slice = EnergySlice {
        gpu_mj: 2_000_000,
        cpu_mj: 300_000,
        sm_by_pid: [(200, 0), (201, 7500), (100, 2500)].into(),
        // Key 0 is every untracked process together, as `energy_between` reports it.
        cpu_ms_by_pid: [(201, 2000), (0, 6000)].into(),
        idle_gpu_mw: Some(40_000),
        idle_cpu_mw: Some(10_000),
        gpu_measured: true,
        cpu_measured: true,
    };
    let e = attribute(
        "j_1",
        "loose",
        &[window],
        &[slice],
        &PowerSettings::default(),
    );
    assert_eq!(e.gpu_mj_gross, 1_500_000);
    assert_eq!(e.gpu_mj_above_idle, 1_200_000);
    assert_eq!(e.gpu_seconds_milli, 7_500);
    assert_eq!(
        e.cpu_mj_gross, 75_000,
        "the CPU share divides by the map's sum, pid 0 included"
    );
    assert_eq!(e.cpu_mj_above_idle, 50_000);
    assert!(e.measured.gpu && e.measured.cpu);
    assert!(!e.idle_unknown);
    // (1 500 000 + 75 000) / 0.9.
    assert_eq!(e.system_mj, 1_750_000);
}

/// A GPU script: `lead` seconds before a 10-second window at 200 W, with `quiet` samples
/// (index, mW) under 5 % utilisation and the rest at 150 W and 50 %.
fn window_script(lead: usize, quiet: &[(usize, u64)]) -> Value {
    let mut counter = 0;
    let mut readings = Vec::new();
    for i in 0..lead + 10 {
        let idle = quiet.iter().find(|(at, _)| *at == i);
        let (util, power) = match (i >= lead, idle) {
            (true, _) => (50, 200_000),
            (false, Some((_, mw))) => (2, *mw),
            (false, None) => (50, 150_000),
        };
        counter += power;
        readings.push(gpu(power, counter, util, json!([[200, 750], [100, 250]])));
    }
    json!({"gpu": readings, "gpu_mem": [[]], "cpu_energy": null, "sys": [{"cpu_permille": 0}]})
}

/// Runs one job with the window `[from_s, to_s]` on `rig` after `samples` samples; its energy
/// and its usage line.
async fn job_over(rig: &Rig, samples: u32, from_s: u64, to_s: u64) -> (JobEnergy, Value) {
    rig.run(samples).await;
    let job = rig
        .job(json!({"windows": [[from_s * 1000, to_s * 1000, [200]]]}))
        .await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    let energy = rig.energy(&job.job_id).await.expect("the job's energy");
    let line = lines_of(&rig.loose(), "job_id", &job.job_id).remove(0);
    (energy, line)
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-065
async fn idle_baseline_cases() {
    // (a) Three quiet samples in the 60 s before the window, at 40, 42 and 44 W: 42 W. The one
    // at 90 W is 65 s before it.
    let a = Rig::new(&window_script(
        70,
        &[(10, 40_000), (30, 42_000), (50, 44_000), (5, 90_000)],
    ));
    let (e, line) = job_over(&a, 80, 69, 79).await;
    assert_eq!(e.gpu_mj_gross, 1_500_000);
    assert_eq!(e.gpu_mj_above_idle, (2_000_000 - 420_000) * 3 / 4);
    assert!(!e.idle_unknown);
    assert!(
        serde_json::to_value(&e)
            .unwrap()
            .get("idle_unknown")
            .is_none()
    );
    assert!(line.get("idle_unknown").is_none(), "{line}");

    // (b) No quiet sample in those 60 s; 1-minute averages of 80, 55 and 70 W in the hour
    // before: 55 W.
    let mut readings = Vec::new();
    let mut counter = 0;
    for minute_power in [80_000_u64, 55_000, 70_000] {
        for _ in 0..60 {
            counter += minute_power;
            readings.push(gpu(minute_power, counter, 50, json!([[200, 1000]])));
        }
    }
    for _ in 0..11 {
        counter += 200_000;
        readings.push(gpu(200_000, counter, 50, json!([[200, 1000]])));
    }
    let b = Rig::new(
        &json!({"gpu": readings, "gpu_mem": [[]], "cpu_energy": null,
                            "sys": [{"cpu_permille": 0}]}),
    );
    let (e, line) = job_over(&b, 191, 180, 190).await;
    assert_eq!(e.gpu_mj_gross, 2_000_000);
    assert_eq!(e.gpu_mj_above_idle, 2_000_000 - 550_000);
    assert!(!e.idle_unknown);
    assert!(line.get("idle_unknown").is_none(), "{line}");

    // (c) A fresh telemetry.db: no baseline at all.
    let c = Rig::new(&window_script(0, &[]));
    let (e, line) = job_over(&c, 10, 0, 9).await;
    assert!(e.gpu_mj_gross > 0);
    assert_eq!(
        e.gpu_mj_above_idle, e.gpu_mj_gross,
        "above idle is the gross figure"
    );
    assert!(e.idle_unknown);
    assert_eq!(serde_json::to_value(&e).unwrap()["idle_unknown"], true);
    assert_eq!(line["idle_unknown"], true, "{line}");
    assert_eq!(
        line["energy_wh_gross"], line["energy_wh_above_idle"],
        "{line}"
    );
}

// ---------------------------------------------------------------------------------------------
// Local work (CORE-064, CORE-082) and job outcomes.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-064, CORE-082, CORE-069
async fn local_job_record_has_energy_and_cost_zero_with_energy_cost_apart() {
    let power = PowerSettings {
        electricity: Some((
            Money::parse_decimal("0.3").unwrap(),
            Currency::parse("EUR").unwrap(),
        )),
        ..PowerSettings::default()
    };
    let rig = Rig::with(TestEnv::new(), &busy(), power);
    rig.telemetry.track(200, "comfyui");
    rig.run(30).await;
    let job = rig.job(json!({"windows": [[10_000, 20_000, [200]]]})).await;
    assert_eq!(job.status, JobStatus::Succeeded);

    let e = rig.energy(&job.job_id).await.expect("the job's energy");
    // Ten samples at 150 W, the engine's share 750 ‰.
    assert_eq!(e.gpu_mj_gross, 1_125_000);
    assert_eq!(e.gpu_seconds_milli, 7_500);
    assert!(e.measured.gpu && e.measured.cpu, "{e:?}");
    assert!(e.cpu_mj_gross > 0, "{e:?}");
    assert_eq!(e.workspace, "loose");
    // The window (10 s, 20 s] holds the samples of seconds 11 to 20: from and to are theirs.
    let t0 = 1_800_000_000_000;
    assert_eq!(
        (e.from.clone(), e.to.clone()),
        (
            lil_core::telemetry::format_ts(t0 + 11_000),
            lil_core::telemetry::format_ts(t0 + 20_000)
        ),
        "{e:?}"
    );

    let found = lines_of(&rig.loose(), "job_id", &job.job_id);
    assert_eq!(found.len(), 1, "{found:?}");
    let line = &found[0];
    assert_eq!(line["billing_basis"], "local");
    assert_eq!(line["provider"], "local");
    assert_eq!(line["cost"], "0", "local work costs no money");
    assert_eq!(line["est_cost"], Value::Null);
    assert_eq!(line["tool"], "test.render");
    assert_eq!(line["caller_kind"], "cli");
    assert_eq!(line["status"], "succeeded");
    assert_eq!(line["gpu_seconds"], "7.5");
    // The energy cost is apart, at the electricity rate: system mJ / 3.6e9 × 0.3 EUR.
    let wh: f64 = line["energy_wh_gross"].as_str().unwrap().parse().unwrap();
    let mj = e.system_mj as f64;
    assert!((wh - mj / 3_600_000.0).abs() < 0.000_001, "{line}");
    // Rounded half to even to a micro-unit, as money is.
    let (p, d) = (u128::from(e.system_mj) * 300_000, 3_600_000_000_u128);
    let (q, rem) = (p / d, p % d);
    let q = if 2 * rem > d || (2 * rem == d && q % 2 == 1) {
        q + 1
    } else {
        q
    };
    let expected_cost = Money::from_micros(i64::try_from(q).unwrap());
    assert_eq!(line["energy_cost"], expected_cost.to_decimal_string());
    assert_eq!(line["currency"], "EUR");
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-088, AI-029, CORE-069
async fn job_outcomes_map_to_usage_status() {
    let rig = Rig::new(&busy());
    rig.run(10).await;
    let window = json!([[1000, 5000, [200]]]);
    let status_of = |id: &str| -> Vec<Value> {
        lines_of(&rig.loose(), "job_id", id)
            .into_iter()
            .map(|l| l["status"].clone())
            .collect()
    };

    let failed = rig.job(json!({"windows": window, "end": "fail"})).await;
    assert_eq!(failed.status, JobStatus::Failed);
    assert_eq!(status_of(&failed.job_id), [json!("failed")]);

    // A cancel while it runs.
    let caller = rig.caller(CallerKind::Cli);
    let job = rig
        .jobs
        .submit(&caller, spec(json!({"windows": window, "end": "cancel"})))
        .await
        .unwrap();
    for _ in 0..1000 {
        if rig.handler.started.lock().unwrap().contains(&job.job_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rig.jobs
        .cancel(&caller, &JobId(job.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(rig.wait(&job.job_id).await.status, JobStatus::Cancelled);
    assert_eq!(status_of(&job.job_id), [json!("cancelled")]);
    // Submitted without an idempotency key, its caller is still known (CORE-069).
    let cancelled = lines_of(&rig.loose(), "job_id", &job.job_id).remove(0);
    assert_eq!(cancelled["caller_kind"], "cli", "{cancelled}");

    // An interrupted_unknown job you resolve from the app.
    let unknown = rig.job(json!({"windows": window, "end": "unknown"})).await;
    assert_eq!(unknown.substatus.as_deref(), Some("interrupted_unknown"));
    assert!(
        status_of(&unknown.job_id).is_empty(),
        "no line until you resolve it"
    );
    let app = rig.caller(CallerKind::Ui("test".into()));
    rig.jobs
        .cancel(&app, &JobId(unknown.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(status_of(&unknown.job_id), [json!("cancelled")]);
    // This core run saw its windows: the line and the job's energy have them.
    let resolved = lines_of(&rig.loose(), "job_id", &unknown.job_id).remove(0);
    assert!(resolved.get("gpu_seconds").is_some(), "{resolved}");
    assert!(rig.energy(&unknown.job_id).await.is_some());

    // A job with no execution window used no engine time: no line.
    let plain = rig.job(json!({})).await;
    assert_eq!(plain.status, JobStatus::Succeeded);
    assert!(status_of(&plain.job_id).is_empty());

    // A job that never started (its first attempt can't be persisted): no line, no energy.
    let db = rusqlite::Connection::open(rig.env.env.suite_state_db()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_attempts BEFORE UPDATE OF substatus ON jobs
         WHEN NEW.substatus IN ('submitting', 'executing')
         BEGIN SELECT RAISE(ABORT, 'refused by the test'); END;",
    )
    .unwrap();
    drop(db);
    let never = rig.job(json!({"windows": window})).await;
    assert_eq!(never.status, JobStatus::Failed);
    assert!(status_of(&never.job_id).is_empty());
    assert!(rig.energy(&never.job_id).await.is_none());
}

/// `telemetry.jobs {job_ids: [id]}` lists the job's energy; `limit: 0` is `params.invalid`.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-146, CORE-065
async fn telemetry_jobs_lists_job_energy() {
    let rig = Rig::new(&busy());
    rig.telemetry.track(200, "comfyui");
    rig.run(20).await;
    let job = rig.job(json!({"windows": [[5000, 15_000, [200]]]})).await;
    let mut reg = Registry::new();
    register_telemetry_tools(&mut reg, &rig.telemetry);
    let ctx = call_ctx(&rig, "c_1");
    let got = common::call(
        &reg,
        &ctx,
        "telemetry.jobs",
        json!({"job_ids": [job.job_id]}),
    )
    .await
    .unwrap();
    let jobs = got["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1, "{got}");
    assert_eq!(jobs[0]["job_id"], job.job_id.as_str());
    assert_eq!(jobs[0]["workspace"], "loose");
    assert_eq!(jobs[0]["measured"], json!({"gpu": true, "cpu": true}));
    let zero = common::call(&reg, &ctx, "telemetry.jobs", json!({"limit": 0}))
        .await
        .unwrap_err();
    assert_eq!(zero.code, "params.invalid");
}

// ---------------------------------------------------------------------------------------------
// The node install's one line (QA1 I5).
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-069, CORE-071
async fn node_install_has_one_usage_line() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    let path = |p: &Path| p.to_str().unwrap().to_string();
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\ncomfy_cli = '{}'\npython = '{}'\n",
        fake.port(),
        path(&fake.root()),
        path(&ensure_built().fake_comfy_cli),
        path(&ensure_built().fake_comfyui),
    ));
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    // Write-ahead: when the row commits, comfy-cli has not run yet.
    let ran_before: Arc<Mutex<Vec<bool>>> = Arc::default();
    {
        let (seen, cli_log) = (ran_before.clone(), fake.root().join("fake-comfy-cli.log"));
        rig.fp.on(
            "meter.after_pending",
            Box::new(move || {
                seen.lock().unwrap().push(cli_log.exists());
                Ok(())
            }),
        );
    }
    let _engines = Engines::new(
        rig.env.env.clone(),
        rig.jobs.clone(),
        rig.telemetry.clone(),
        rig.events.clone(),
        rig.clock.clone(),
        FailPoints::none(),
        rig.meter.clone(),
        Arc::new(OsSockets),
    );
    let spec = install::job_spec(
        &NodeId("comfyui-test-node".into()),
        &ExactVersion("1.2.3".into()),
        "cli",
        Priority::Agent,
        None,
    );
    let caller = rig.caller(CallerKind::Cli);
    let job = rig.jobs.submit(&caller, spec).await.unwrap();
    let job = rig.wait(&job.job_id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    assert_eq!(
        *ran_before.lock().unwrap(),
        [false],
        "pending before comfy-cli ran"
    );

    let all = lines(&rig.loose());
    assert_eq!(all.len(), 1, "one line, the install's own: {all:?}");
    let line = &all[0];
    assert_eq!(line["job_id"], job.job_id.as_str());
    assert_eq!(line["tool"], "engines.comfyui.install_node");
    assert_eq!(line["provider"], "comfy-registry");
    assert_eq!(line["billing_basis"], "free_tier");
    assert_eq!(line["status"], "succeeded");
    assert!(line["latency_ms"].is_u64(), "{line}");
    assert!(
        rig.telemetry
            .job_energy(JobEnergyQuery::default())
            .await
            .unwrap()
            .is_empty(),
        "no execution window, no energy"
    );
}

// ---------------------------------------------------------------------------------------------
// Stage 8 for inline calls.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-015, CORE-071
async fn meter_stage_finishes_inline_calls_once() {
    let rig = Rig::new(&busy());
    let mut reg = Registry::new();
    register_meter_tools(&mut reg, &rig.meter, true);
    let stage = MeterSettle::new(rig.meter.clone());
    let session = Session {
        id: "s_0000000000000001".into(),
        caller: CallerKind::Cli,
        non_interactive: true,
        workspace: None,
        capabilities: Vec::new(),
        client_pid: 1,
        profile_id: None,
    };
    let settle = |call_id: &'static str, outcome: Outcome<ToolOutput>| {
        let (stage, session) = (&stage, &session);
        async move {
            stage
                .settle(SettleInput {
                    session,
                    reservation: None,
                    call_id,
                    outcome: &outcome,
                })
                .await;
        }
    };

    let ctx = call_ctx(&rig, "c_0000000000000001");
    let tool = reg.get("test.meter_external").unwrap();
    let out = tool
        .run(&ctx, json!({"provider": "p", "amount": "0.5"}))
        .await
        .unwrap();
    assert!(
        lines(&rig.loose()).is_empty(),
        "the tool left its row pending"
    );
    settle("c_0000000000000001", Outcome::Ok(out.clone())).await;
    let found = lines_of(&rig.loose(), "tool", "test.meter_external");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["status"], "succeeded");
    assert_eq!(found[0]["est_cost"], "0.5");
    assert_eq!(found[0]["currency"], "USD");
    settle("c_0000000000000001", Outcome::Ok(out)).await;
    assert_eq!(lines(&rig.loose()).len(), 1, "a second settle adds no line");

    let ctx = call_ctx(&rig, "c_0000000000000002");
    let error = tool
        .run(
            &ctx,
            json!({"provider": "p", "amount": "0.5", "fail": true}),
        )
        .await
        .unwrap_err();
    settle("c_0000000000000002", Outcome::Error { error }).await;
    let all = lines(&rig.loose());
    assert_eq!(all.len(), 2);
    assert_eq!(all[1]["status"], "failed");
    // Settled at their estimates, both count as estimated (CORE-082).
    let q: SummaryQuery = serde_json::from_value(json!({"period": "month"})).unwrap();
    let s = rig.meter.summary(q, None).await.unwrap();
    assert_eq!(s["bases"]["money"]["records"], 2, "{s}");
    assert_eq!(s["bases"]["money"]["estimated"], 2, "{s}");
}

// ---------------------------------------------------------------------------------------------
// Summaries (CORE-081–083).
// ---------------------------------------------------------------------------------------------

/// The money settings of these tests: USD display, EUR at 1.1 from 1 January and 1.2 from
/// 20 January 2027, a 5 USD storage plan and a GBP plan without a rate.
fn money_toml(env: &TestEnv) {
    env.suite_toml_set(
        "[money]\ndisplay_currency = \"USD\"\n\
         [[money.rates]]\ncurrency = \"EUR\"\nin_display = \"1.1\"\ndate = \"2027-01-01\"\n\
         [[money.rates]]\ncurrency = \"EUR\"\nin_display = \"1.2\"\ndate = \"2027-01-20\"\n\
         [[fixed_costs]]\nname = \"storage\"\namount = \"5\"\ncurrency = \"USD\"\nperiod = \"monthly\"\n\
         [[fixed_costs]]\nname = \"vps\"\namount = 4\ncurrency = \"GBP\"\nperiod = \"monthly\"\n",
    );
}

async fn spend(rig: &Rig, mut d: UsageDraft, cost: Option<(&str, &str)>) {
    let cost = cost.map(|(m, c)| {
        (
            Money::parse_decimal(m).unwrap(),
            Currency::parse(c).unwrap(),
        )
    });
    d.est_cost = d.est_cost.or_else(|| cost.clone());
    let id = rig.meter.begin(&uctx(None, "c_x"), d).await.unwrap();
    rig.meter
        .finish(&id, cost, "provider_reported", "succeeded", 1)
        .await
        .unwrap();
}

async fn summary(rig: &Rig, q: Value) -> Value {
    let q: SummaryQuery = serde_json::from_value(q).unwrap();
    rig.meter.summary(q, None).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-081, CORE-082, CORE-070
async fn summary_never_adds_bases_together_and_converts_with_dated_rates() {
    let env = TestEnv::new();
    money_toml(&env);
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    // 2027-01-15.
    spend(
        &rig,
        draft("p-usd", BillingBasis::Payg, None),
        Some(("1", "USD")),
    )
    .await;
    spend(
        &rig,
        draft("p-eur", BillingBasis::Payg, None),
        Some(("2", "EUR")),
    )
    .await;
    let mut quota = draft("p-quota", BillingBasis::SubscriptionQuota, None);
    quota.units.insert("requests".into(), json!(40));
    spend(&rig, quota, Some(("100", "USD"))).await;
    let mut local = draft("local", BillingBasis::Local, None);
    local.units.insert("gpu_seconds".into(), json!("1.5"));
    spend(&rig, local, None).await;
    let mut outside = draft("comfy-org", BillingBasis::Payg, None);
    outside.attribution = Attribution::Unattributed;
    spend(&rig, outside, Some(("3", "USD"))).await;
    // 2027-01-21: the newer EUR rate.
    rig.clock.advance(Duration::from_secs(6 * 86_400));
    spend(
        &rig,
        draft("p-eur", BillingBasis::Payg, None),
        Some(("1", "EUR")),
    )
    .await;

    let s = summary(&rig, json!({"period": "month", "group_by": ["provider"]})).await;
    assert_eq!(s["by_currency"], json!({"EUR": "3", "USD": "1"}), "{s}");
    // 1 + 2 × 1.1 + 1 × 1.2 + the 5 USD plan; the GBP plan has no rate.
    assert_eq!(s["display_total"], "9.4", "{s}");
    assert_eq!(s["unconverted"], json!(["GBP"]), "{s}");
    assert_eq!(s["bases"]["money"]["records"], 3, "{s}");
    assert_eq!(s["bases"]["subscription_quota"]["records"], 1, "{s}");
    assert_eq!(
        s["bases"]["subscription_quota"]["units"]["requests"], 40,
        "quota keeps its own units: {s}"
    );
    assert_eq!(s["bases"]["local"]["records"], 1, "{s}");
    assert_eq!(s["bases"]["local"]["gpu_seconds"], "1.5", "{s}");
    assert_eq!(s["unattributed"]["records"], 1, "{s}");
    assert_eq!(s["unattributed"]["by_currency"], json!({"USD": "3"}), "{s}");
    let eur_row = s["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["provider"] == "p-eur")
        .cloned()
        .unwrap();
    assert_eq!(eur_row["period"], "2027-01");
    assert_eq!(eur_row["basis"], "money");
    assert_eq!(eur_row["records"], 2);
    assert_eq!(eur_row["by_currency"], json!({"EUR": "3"}));
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-083
async fn fixed_costs_are_in_month_totals() {
    let env = TestEnv::new();
    money_toml(&env);
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    spend(
        &rig,
        draft("p", BillingBasis::Payg, None),
        Some(("1", "USD")),
    )
    .await;

    let month = summary(&rig, json!({"period": "month"})).await;
    assert_eq!(month["display_total"], "6", "{month}");
    assert_eq!(
        month["fixed_costs"][0],
        json!({"name": "storage", "amount": "5", "currency": "USD", "display_amount": "5"})
    );
    assert_eq!(month["fixed_costs"][1]["display_amount"], Value::Null);
    let day = summary(&rig, json!({"period": "day"})).await;
    assert_eq!(day["display_total"], "1", "a day has no fixed costs: {day}");
    assert_eq!(day["fixed_costs"], json!([]));
    // Two months of a range: the plan twice.
    let two = summary(
        &rig,
        json!({"period": "month", "from": "2027-01-01T00:00:00Z", "to": "2027-02-10T00:00:00Z"}),
    )
    .await;
    assert_eq!(two["display_total"], "11", "{two}");
    let backwards = rig
        .meter
        .summary(
            serde_json::from_value(json!({"period": "day", "from": "2027-02-01T00:00:00Z",
                                           "to": "2027-01-01T00:00:00Z"}))
            .unwrap(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(backwards.code, "params.invalid");
}

/// Checkpoint A conformance M-2 (CORE-117): a `comfy.prompt` job that held the GPU lease names
/// the GPU adapter of the newest reading as its `engine_device`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-117
async fn a_comfy_job_names_its_gpu_as_engine_device() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\n",
        fake.port(),
        fake.root().to_str().unwrap(),
    ));
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    rig.run(1).await;
    let _engines = Engines::new(
        rig.env.env.clone(),
        rig.jobs.clone(),
        rig.telemetry.clone(),
        rig.events.clone(),
        rig.clock.clone(),
        FailPoints::none(),
        rig.meter.clone(),
        Arc::new(OsSockets),
    );
    let graph = json!({
        "1": {"class_type": "EmptyImage",
              "inputs": {"width": 64, "height": 64, "batch_size": 1, "color": 0}},
        "2": {"class_type": "SaveImage",
              "inputs": {"images": ["1", 0], "filename_prefix": "ComfyUI"}},
    });
    let spec = lil_core::engines::comfyui::handler::prompt_spec(
        json!({"graph": graph}),
        Priority::Agent,
        "test",
        None,
        60,
    );
    let job = rig
        .jobs
        .submit(&rig.caller(CallerKind::Cli), spec)
        .await
        .unwrap();
    let job = rig.wait(&job.job_id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    assert_eq!(job.engine_device.as_deref(), Some("Fake GPU"), "{job:?}");
}

// ---------------------------------------------------------------------------------------------
// Fix round 1.
// ---------------------------------------------------------------------------------------------

/// I1 (CORE-071): when the write-ahead row can't be written, the install fails before comfy-cli
/// is started, and nothing is recorded after the fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-071, CORE-069
async fn an_install_without_its_usage_row_never_starts_comfy_cli() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    let path = |p: &Path| p.to_str().unwrap().to_string();
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\ncomfy_cli = '{}'\npython = '{}'\n",
        fake.port(),
        path(&fake.root()),
        path(&ensure_built().fake_comfy_cli),
        path(&ensure_built().fake_comfyui),
    ));
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    let db = rusqlite::Connection::open(rig.env.env.suite_state_db()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_usage BEFORE INSERT ON usage_pending
         BEGIN SELECT RAISE(ABORT, 'refused by the test'); END;",
    )
    .unwrap();
    drop(db);
    let _engines = Engines::new(
        rig.env.env.clone(),
        rig.jobs.clone(),
        rig.telemetry.clone(),
        rig.events.clone(),
        rig.clock.clone(),
        FailPoints::none(),
        rig.meter.clone(),
        Arc::new(OsSockets),
    );
    let spec = install::job_spec(
        &NodeId("comfyui-test-node".into()),
        &ExactVersion("1.2.3".into()),
        "cli",
        Priority::Agent,
        None,
    );
    let caller = rig.caller(CallerKind::Cli);
    let job = rig.jobs.submit(&caller, spec).await.unwrap();
    let job = rig.wait(&job.job_id).await;
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert_eq!(
        job.error.as_ref().unwrap().code,
        "state.unavailable",
        "{job:?}"
    );
    assert!(
        !fake.root().join("fake-comfy-cli.log").exists(),
        "comfy-cli ran without its usage row"
    );
    assert!(lines(&rig.loose()).is_empty());
}

/// I1: a `begin` whose next step fails (`meter.after_pending` as an error) sends nothing and
/// leaves no row, so no later start writes a line for it.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-071
async fn a_failed_begin_leaves_no_row() {
    let rig = Rig::new(&busy());
    rig.fp.on(
        "meter.after_pending",
        Box::new(|| Err(std::io::Error::other("the step after the row"))),
    );
    let failed = rig
        .meter
        .begin(
            &uctx(None, "c_1"),
            draft("p", BillingBasis::Payg, Some(usd("1"))),
        )
        .await;
    assert!(failed.is_err());
    let conn = rusqlite::Connection::open(rig.env.env.suite_state_db()).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM usage_pending", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(rig.restarted_meter().recover().await.unwrap(), 0);
    assert!(lines(&rig.loose()).is_empty());
}

/// M1 and M10: a `logging` row whose line went to the next month's log (the append began just
/// before a month's end) is found there and not written again; a `logging` row without a line
/// (a local-work line a dead core never appended) is written once.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-071, CORE-069
async fn a_logging_row_is_written_once_across_a_month_end() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("meter");
    env.register_workspace(&ws, true);
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    let open = rig.hub.open(&ws_ref(&ws)).await.unwrap();
    // 2027-01-31T23:59:59.999Z, then the clock moves to 1 February.
    let jan_31_last_ms = 1_801_439_999_999;
    rig.clock.advance(Duration::from_secs(1_440_000));
    let row = |id: &str, ts: &str| usage_record::PendingRow {
        id: id.into(),
        run: "a dead core".into(),
        call_id: "c_1".into(),
        job_id: None,
        reservation: None,
        record: {
            let mut r = usage_record::draft_record(
                ts,
                &uctx(Some(open.clone()), "c_1"),
                &draft("p", BillingBasis::Payg, Some(usd("1"))),
            );
            usage_record::finish(
                &mut r,
                Some(usd("1")),
                "provider_reported",
                "succeeded",
                Some(3),
            );
            r
        },
        logging: true,
        created_ms: jan_31_last_ms,
        logging_ms: Some(jan_31_last_ms),
    };
    let logged = row("us_0000000000000001", "2027-01-31T23:59:59.990Z");
    usage_record::insert(&open.state, &logged).await.unwrap();
    open.store
        .append_log(lil_core::store::LogKind::Usage, logged.record.clone())
        .await
        .unwrap();
    let lost = row("us_0000000000000002", "2027-01-31T23:59:59.995Z");
    usage_record::insert(&open.state, &lost).await.unwrap();

    let next = rig.restarted_meter();
    assert_eq!(
        next.recover_workspace(&open).await.unwrap(),
        1,
        "only the lost line"
    );
    let feb = lines(&ws.join("registry/usage.testmachine.2027-02.jsonl"));
    assert_eq!(feb.len(), 2, "{feb:?}");
    assert_eq!(feb[0]["ts"], "2027-01-31T23:59:59.990Z");
    assert_eq!(feb[1]["ts"], "2027-01-31T23:59:59.995Z");
    assert!(
        usage_record::get(&open.state, &logged.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        usage_record::get(&open.state, &lost.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        rig.restarted_meter()
            .recover_workspace(&open)
            .await
            .unwrap(),
        0
    );
    drop(open);
}

/// M5: a sum past the micro-unit range is never shown as exact.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-013, CORE-081
async fn summary_totals_report_overflow() {
    let rig = Rig::new(&busy());
    let huge = "9223372036854";
    spend(
        &rig,
        draft("p", BillingBasis::Payg, None),
        Some((huge, "USD")),
    )
    .await;
    spend(
        &rig,
        draft("p", BillingBasis::Payg, None),
        Some((huge, "USD")),
    )
    .await;
    let s = summary(&rig, json!({"period": "month"})).await;
    assert_eq!(s["overflow"], true, "{s}");
    let none = summary(
        &rig,
        json!({"period": "month", "from": "2030-01-01T00:00:00Z", "to": "2030-01-02T00:00:00Z"}),
    )
    .await;
    assert!(none.get("overflow").is_none(), "{none}");
}

/// M11: with a workspace session the summary reads that workspace's logs of every machine (for
/// the months of its range) as well as the loose log.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-082, ARCH-081
async fn summary_reads_the_workspace_logs_of_every_machine() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("meter");
    env.register_workspace(&ws, true);
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    let open = rig.hub.open(&ws_ref(&ws)).await.unwrap();
    let id = rig
        .meter
        .begin(
            &uctx(Some(open.clone()), "c_1"),
            draft("p", BillingBasis::Payg, None),
        )
        .await
        .unwrap();
    rig.meter
        .finish(&id, Some(usd("2")), "provider_reported", "succeeded", 1)
        .await
        .unwrap();
    spend(
        &rig,
        draft("p", BillingBasis::Payg, None),
        Some(("1", "USD")),
    )
    .await;
    // Another machine's line in this month's log, and one in the log of a month the range
    // doesn't touch.
    let theirs = |ts: &str| {
        let mut r = usage_record::draft_record(
            ts,
            &uctx(None, "c_9"),
            &draft("p", BillingBasis::Payg, None),
        );
        usage_record::finish(
            &mut r,
            Some(usd("4")),
            "provider_reported",
            "succeeded",
            Some(1),
        );
        let mut line = Map::new();
        line.insert("machine".into(), json!("othermachine"));
        line.insert("seq".into(), json!(1));
        line.extend(r);
        format!("{}\n", Value::Object(line))
    };
    lil_testkit::files::write_file(
        &ws.join("registry/usage.othermachine.2027-01.jsonl"),
        theirs("2027-01-15T07:00:00.000Z").as_bytes(),
    );
    lil_testkit::files::write_file(
        &ws.join("registry/usage.othermachine.2026-06.jsonl"),
        theirs("2027-01-15T07:30:00.000Z").as_bytes(),
    );
    let q = |v: Value| serde_json::from_value::<SummaryQuery>(v).unwrap();
    let with = rig
        .meter
        .summary(q(json!({"period": "month"})), Some(&open))
        .await
        .unwrap();
    assert_eq!(with["bases"]["money"]["records"], 3, "{with}");
    assert_eq!(with["by_currency"], json!({"USD": "7"}), "{with}");
    let without = rig
        .meter
        .summary(q(json!({"period": "month"})), None)
        .await
        .unwrap();
    assert_eq!(without["by_currency"], json!({"USD": "1"}), "{without}");
    drop(open);
}

/// Caps are enforced on each machine (CORE-092): spend synced from another machine shows in
/// `usage.summary`, for information, but never counts against this machine's caps.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-092, CORE-082
async fn spend_synced_from_another_machine_never_counts_against_the_caps() {
    let env = TestEnv::new();
    let ws = env.make_workspace("synced");
    env.register_workspace(&ws, true);
    let suite = env.env.suite_toml();
    let text = std::fs::read_to_string(&suite).unwrap();
    let caps_toml = "[money]
display_currency = \"USD\"
[caps]
monthly = \"5\"
";
    lil_testkit::files::write_file(
        &suite,
        format!(
            "{text}
{caps_toml}"
        )
        .as_bytes(),
    );
    let rig = Rig::with(env, &busy(), PowerSettings::default());
    let caps = Caps::new(
        rig.states.clone(),
        rig.env.env.clone(),
        rig.clock.clone(),
        rig.events.clone(),
        rig.fp.clone(),
        rig.ids.clone(),
    );
    let open = rig.hub.open(&ws_ref(&ws)).await.unwrap();
    // Another machine's 4 USD, synced into this month's log of the workspace.
    let period = caps.period(rig.clock.utc_now());
    let mut record = usage_record::draft_record(
        &format!("{period}-15T07:00:00.000Z"),
        &uctx(None, "c_9"),
        &draft("p", BillingBasis::Payg, None),
    );
    usage_record::finish(
        &mut record,
        Some(usd("4")),
        "provider_reported",
        "succeeded",
        Some(1),
    );
    let mut line = Map::new();
    line.insert("machine".into(), json!("othermachine"));
    line.insert("seq".into(), json!(1));
    line.extend(record);
    lil_testkit::files::write_file(
        &ws.join(format!("registry/usage.othermachine.{period}.jsonl")),
        format!(
            "{}
",
            Value::Object(line)
        )
        .as_bytes(),
    );

    let q = serde_json::from_value::<SummaryQuery>(json!({"period": "month"})).unwrap();
    let shown = rig.meter.summary(q, Some(&open)).await.unwrap();
    assert_eq!(shown["by_currency"], json!({"USD": "4"}), "{shown}");
    let view = caps.view().await.unwrap();
    let monthly = view.caps.iter().find(|c| c.cap == "monthly").unwrap();
    assert_eq!(
        (monthly.settled.as_str(), monthly.held.as_str()),
        ("0", "0")
    );
    let estimate = |amount: &str| Estimate::External {
        provider: "p".into(),
        amount: Money::parse_decimal(amount).unwrap(),
        currency: Currency::parse("USD").unwrap(),
        basis: BillingBasis::Payg,
    };
    // 3 more passes this machine's cap of 5 (4 + 3 would not); 6 doesn't, so the cap is live.
    assert_eq!(caps.check(&estimate("3")).await.unwrap(), CapCheck::Within);
    assert!(matches!(
        caps.check(&estimate("6")).await.unwrap(),
        CapCheck::Exceeds(_)
    ));
    drop(open);
}

/// Every `job_stats` row of the rig's `telemetry.db`: `(job_id, kind, estimate_key, wall_ms,
/// gpu_seconds_milli, system_mj)`.
fn job_stats(rig: &Rig) -> Vec<(String, String, String, i64, i64, i64)> {
    let conn = rusqlite::Connection::open(rig.env.env.local_dir().join("telemetry.db")).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT job_id, kind, estimate_key, wall_ms, gpu_seconds_milli, system_mj \
             FROM job_stats ORDER BY job_id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Waits until no job is queued, running or ending: its `succeeded` completions (the
/// history row) are done (a failure deadline, never a measurement).
async fn idle(rig: &Rig) {
    tokio::time::timeout(DEADLINE, async {
        while rig.jobs.busy() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the jobs never went idle");
}

/// [`Rig::job`], then [`idle`].
async fn job_done(rig: &Rig, args: Value) -> lil_core::jobs::JobSummary {
    let job = rig.job(args).await;
    idle(rig).await;
    job
}

/// `meter` writes a job's history row (Appendix G) only when the job ends `succeeded` and its
/// handler gave it an estimate key: its execution windows' time, its SM-weighted GPU-seconds
/// and its whole-system energy, as its `JobEnergy` has them. An all-cached run adds none, a
/// partly cached one does (plan 4a ruling, 2026-10-24).
#[tokio::test(flavor = "multi_thread")] // spec: CORE-161, CORE-065
async fn job_stats_written_only_for_succeeded_jobs_with_a_key() {
    let rig = Rig::new(&busy());
    rig.telemetry.track(200, "comfyui");
    rig.run(30).await;
    let windows = json!([[2_000, 6_000, [200]], [10_000, 13_000, [200]]]);
    let ok = job_done(&rig, json!({"key": "k", "windows": windows})).await;
    assert_eq!(ok.status, JobStatus::Succeeded);
    let failed = job_done(&rig, json!({"key": "k", "windows": windows, "end": "fail"})).await;
    assert_eq!(failed.status, JobStatus::Failed);
    let keyless = job_done(&rig, json!({"windows": windows})).await;
    assert_eq!(keyless.status, JobStatus::Succeeded);
    // A job with a key and no window has a row too: no time or energy was measured.
    let bare = job_done(&rig, json!({"key": "bare"})).await;
    assert_eq!(bare.status, JobStatus::Succeeded);
    let cached = job_done(
        &rig,
        json!({"key": "k", "windows": [[20_000, 20_200, [200]]], "cached": "all"}),
    )
    .await;
    assert!(cached.all_cached, "{cached:?}");
    let partly = job_done(
        &rig,
        json!({"key": "partly", "windows": [[20_000, 21_000, [200]]], "cached": "some"}),
    )
    .await;
    assert_eq!(partly.status, JobStatus::Succeeded);

    let e = rig.energy(&ok.job_id).await.expect("the job's energy");
    let p = rig.energy(&partly.job_id).await.expect("its energy");
    let wanted = |id: &str, key: &str, wall: i64, gpu: u64, system: u64| {
        (
            id.to_string(),
            KIND.to_string(),
            key.to_string(),
            wall,
            i64::try_from(gpu).unwrap(),
            i64::try_from(system).unwrap(),
        )
    };
    let mut want = vec![
        wanted(&ok.job_id, "k", 7_000, e.gpu_seconds_milli, e.system_mj),
        wanted(&bare.job_id, "bare", 0, 0, 0),
        wanted(
            &partly.job_id,
            "partly",
            1_000,
            p.gpu_seconds_milli,
            p.system_mj,
        ),
    ];
    want.sort();
    assert_eq!(job_stats(&rig), want);
    let history = rig.telemetry.history(KIND, "k", 20).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].job_id, ok.job_id);
    assert!(e.gpu_seconds_milli > 0 && e.system_mj > 0, "{e:?}");
}

/// 18 all-cached runs among similar jobs move neither the estimate nor the watchdog's bound
/// (plan 4a ruling, 2026-10-24): without the rule they would be 18 of the newest 20, and the
/// p50 and the p90 would both be theirs.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-161, ARCH-072
async fn all_cached_runs_never_move_the_estimate_or_the_watchdog_bound() {
    let rig = Rig::new(&busy());
    rig.telemetry.track(200, "comfyui");
    rig.run(30).await;
    let estimator = lil_core::jobs::Estimator::new(
        rig.telemetry.clone(),
        &PowerSettings::default(),
        std::sync::Arc::new(|| Currency::parse("USD").unwrap()),
    );
    for to in [12_000, 13_000, 14_000] {
        job_done(&rig, json!({"key": "k", "windows": [[2_000, to, [200]]]})).await;
    }
    let before = estimator.estimate(KIND, Some("k")).await;
    assert_eq!(
        before
            .wall_s
            .as_ref()
            .map(|w| (w.p50.as_str(), w.p90.as_str())),
        Some(("11.0", "12.0")),
        "{before:?}"
    );
    for _ in 0..18 {
        let cached = job_done(
            &rig,
            json!({"key": "k", "windows": [[20_000, 20_500, [200]]], "cached": "all"}),
        )
        .await;
        assert!(cached.all_cached);
    }
    let after = estimator.estimate(KIND, Some("k")).await;
    assert_eq!(after, before);
    let bound = |e: &lil_protocol::estimate::Estimate| {
        lil_core::jobs::scheduler::watchdog_limit(10, e.wall_s.as_ref())
    };
    assert_eq!(bound(&after), bound(&before));
    assert_eq!(bound(&after), Some(Duration::from_secs(33)));
}

/// The row is written once the job is `succeeded`, once per job id: a job whose Record step
/// fails, and an agent job its engine interrupted (which runs once more as a new job), add no
/// row of their own; the re-run that succeeds adds one (plan 4a ruling, 2026-10-24).
#[tokio::test(flavor = "multi_thread")] // spec: CORE-161, CORE-128
async fn a_failed_record_or_an_interrupted_run_never_counts() {
    let rig = Rig::new(&busy());
    rig.telemetry.track(200, "comfyui");
    rig.run(30).await;
    let windows = json!([[2_000, 6_000, [200]]]);
    let unrecorded = job_done(
        &rig,
        json!({"key": "k", "windows": windows, "record": true}),
    )
    .await;
    assert_eq!(unrecorded.status, JobStatus::Failed, "{unrecorded:?}");
    assert_eq!(job_stats(&rig), [], "a failed Record step adds no row");

    let interrupted = job_done(
        &rig,
        json!({"key": "k", "windows": windows, "end": "interrupt_once"}),
    )
    .await;
    assert_eq!(interrupted.status, JobStatus::Failed, "{interrupted:?}");
    // Its re-run is queued as it ends: wait for the re-run's row, then for the jobs to go
    // idle (a failure deadline).
    let deadline = std::time::Instant::now() + DEADLINE;
    while job_stats(&rig).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the re-run added no row"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    idle(&rig).await;
    let rows = job_stats(&rig);
    assert_eq!(rows.len(), 1, "only the re-run: {rows:?}");
    assert_ne!(rows[0].0, interrupted.job_id);
    assert_eq!(rows[0].3, 4_000);
}
