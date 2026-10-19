//! Caps in process (ARCH-056, ARCH-099, CORE-008, CORE-012, CORE-015, CORE-016, CORE-085–092,
//! APP-043, APP-045, APP-054, QA-016, QA-047; Appendix F "Caps and money in `suite.toml`"):
//! reservations against a real suite `state.db` in a temp folder, settles and their repair after
//! a crash, local-calendar periods, per-call and provider caps, job reservations (hand-over,
//! release, `interrupted_unknown`, ruling P3-R15), alerts, what never counts, cap confirmations
//! through the dispatcher, invalid and legacy caps, and who may change them.
//!
//! The dispatcher, `Confirm`, `Meter`, `Caps` and the jobs are built directly on a `FakeClock`
//! (QA-031); an app "connects" by `UiPresence::opened`. `app/caps.set`'s capability gate runs
//! over the pipe of an in-process core with the `ui:test` app. Wall-clock durations are failure
//! deadlines only.
#![allow(
    clippy::result_large_err,
    reason = "JobFailure holds the wire ErrorBody, as in lil-core itself"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use lil_core::confirm::{AgentConfirm, Confirm, McpConfirmation, NoNotificationPath, UiPresence};
use lil_core::engines::Engines;
use lil_core::engines::comfyui::install::{self, ExactVersion, NodeId};
use lil_core::events::EventBus;
use lil_core::identity::TrustedDirs;
use lil_core::jobs::{
    EngineKind, JobCaller, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobId, JobOrigin,
    JobSpec, JobStatus, Jobs, Lane, Priority, SubmittedSlot, cancelled_error,
};
use lil_core::lifecycle::Lifecycle;
use lil_core::meter::{
    Attribution, CapCheck, Caps, Meter, MeterSettle, ReservationState, ReserveFor, UsageCtx,
    UsageDraft, UsageStatus, register_caps_tools, register_meter_tools,
};
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::telemetry::{HwSources, PowerSettings, Telemetry, TickMode};
use lil_core::tools::{
    BillingBasis, Dispatcher, Estimate, PipelineHooks, ReserveOutcome, TestGates, ToolEstimates,
    core_registry,
};
use lil_core::workspace::WorkspaceHub;
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_platform::sockets::OsSockets;
use lil_platform::{Clock, IdSource};
use lil_protocol::app::{
    Answer, CapAction, CapAnswer, CapRow, CapsSetParams, CapsView, ConfirmationAnswerParams,
    RaiseTo,
};
use lil_protocol::{CallerKind, Cover, ErrorBody, Outcome, ToolCallParams, ToolOutput, codes};
use lil_schema::money::{Currency, Money};
use lil_testkit::{FakeClock, FakeComfy, FakeHwScript, SeqIds, TestApp, TestEnv, ensure_built};
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(30);
const LIFETIME: Duration = Duration::from_secs(30 * 60);
const KIND: &str = "caps.test_job";
/// 2027-01-15T08:00:00Z, where the fake clock starts.
const START: u64 = 1_800_000_000;

// ---------------------------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------------------------

/// Runs `caps.test_job` jobs, ending as `end` says: `ok` (the default), `fail`, `unknown`,
/// `cancel` (waits for its cancel), or `interrupt_once` (interrupted on its first run, ok on the
/// re-run).
#[derive(Default)]
struct PaidHandler {
    runs: AtomicU64,
}

impl PaidHandler {
    async fn attempt(&self, job: &JobCtx) -> Result<JobDone, JobFailure> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst);
        let error = |code: &lil_protocol::CodeSpec| ErrorBody::new(code, "on purpose", "");
        let done = Ok(JobDone {
            structured: json!({}),
            artifacts: Vec::new(),
            record: None,
            windows: Vec::new(),
        });
        match job.spec.args["end"].as_str().unwrap_or("ok") {
            "fail" => Err(JobFailure::Final(error(&codes::COMFYUI_EXECUTION_FAILED))),
            "unknown" => Err(JobFailure::Unknown(error(&codes::COMFYUI_UNREACHABLE))),
            "cancel" => {
                let mut cancel = job.cancel.clone();
                let _ = cancel.wait_for(|c| *c).await;
                Err(JobFailure::Final(cancelled_error(&job.trace_id)))
            }
            "interrupt_once" if run == 0 => Err(JobFailure::Interrupted {
                error: error(&codes::COMFYUI_UNREACHABLE),
                salvaged: Vec::new(),
            }),
            _ => done,
        }
    }
}

impl JobHandler for PaidHandler {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(self.attempt(job))
    }

    fn cancel_in_engine<'a>(&'a self, _job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// The suite's state, the confirmations, the meter, the caps and a dispatcher with every plan-3
/// stage, on one fake clock.
struct Rig {
    caps: Arc<Caps>,
    meter: Arc<Meter>,
    confirm: Arc<Confirm>,
    dispatcher: Dispatcher,
    jobs: Arc<Jobs>,
    telemetry: Arc<Telemetry>,
    events: Arc<EventBus>,
    clock: Arc<FakeClock>,
    ids: Arc<dyn IdSource>,
    fp: Arc<FailPoints>,
    states: Arc<StateRegistry>,
    /// Last: the folder goes only once nothing holds a file in it.
    env: TestEnv,
}

impl Rig {
    /// A rig whose `suite.toml` is `toml`.
    fn new(toml: &str) -> Rig {
        Rig::with(TestEnv::new(), toml)
    }

    fn with(env: TestEnv, toml: &str) -> Rig {
        lil_testkit::files::write_file(&env.env.suite_toml(), toml.as_bytes());
        let states = Arc::new(
            StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
        );
        let clock = FakeClock::new();
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let events = Arc::new(EventBus::new(dyn_clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let fp = FailPoints::none();
        let probe: Arc<dyn lil_platform::fs::Probe> = Arc::new(lil_platform::fs::SystemProbe);
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            fp.clone(),
            probe.clone(),
            lil_core::watch::WatchConfig::default(),
        );
        let telemetry = Telemetry::start(
            HwSources::from_fake(
                FakeHwScript::new(json!({"gpu": null, "gpu_mem": null, "cpu_energy": null,
                    "sys": [{"cpu_permille": 0, "ram_total_mib": 1024, "ram_used_mib": 512}]}))
                .sources(),
            ),
            env.env.local_dir().join("telemetry.db"),
            dyn_clock.clone(),
            PowerSettings::default(),
            events.clone(),
            TickMode::Manual,
        );
        let meter = Meter::new(
            states.clone(),
            env.env.clone(),
            dyn_clock.clone(),
            ids.clone(),
            events.clone(),
            telemetry.clone(),
            PowerSettings::default(),
            fp.clone(),
        );
        let caps = Caps::new(
            states.clone(),
            env.env.clone(),
            dyn_clock.clone(),
            events.clone(),
            fp.clone(),
            ids.clone(),
        );
        let gates = Arc::new(TestGates::new());
        let mut reg = core_registry(
            true,
            &gates,
            &states,
            &hub,
            &env.env,
            &probe,
            Arc::default(),
        );
        register_meter_tools(&mut reg, &meter, true);
        register_caps_tools(&mut reg, &caps);
        let confirm = Confirm::new(
            states.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            UiPresence::new(),
            Arc::new(NoNotificationPath),
            LIFETIME,
            McpConfirmation::Core,
            fp.clone(),
        );
        confirm.set_caps(caps.clone());
        let display = {
            let caps = caps.clone();
            Arc::new(move || caps.display_currency())
        };
        let hooks = PipelineHooks {
            estimate: Arc::new(ToolEstimates::new(display)),
            confirm: AgentConfirm::new(confirm.clone()),
            reserve: caps.clone(),
            meter: Arc::new(MeterSettle::new(meter.clone()).with_caps(caps.clone())),
            ..PipelineHooks::plan2()
        };
        let lifecycle = Lifecycle::new(dyn_clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
        let dispatcher = Dispatcher::new(
            reg,
            hooks,
            states.clone(),
            events.clone(),
            hub,
            ids.clone(),
            dyn_clock,
            lifecycle,
        );
        let jobs = dispatcher.jobs().clone();
        jobs.register(Arc::new(PaidHandler::default()));
        jobs.add_completion(meter.clone());
        jobs.add_completion(caps.clone());
        jobs.set_reservations(caps.clone());
        Rig {
            caps,
            meter,
            confirm,
            dispatcher,
            jobs,
            telemetry,
            events,
            clock,
            ids,
            fp,
            states,
            env,
        }
    }

    async fn call(
        &self,
        s: &Arc<Session>,
        name: &str,
        args: Value,
        key: &str,
    ) -> Outcome<ToolOutput> {
        let Value::Object(arguments) = args else {
            panic!("arguments must be an object");
        };
        self.dispatcher
            .call(
                s.clone(),
                ToolCallParams {
                    name: name.into(),
                    arguments,
                    idempotency_key: Some(key.into()),
                    trace_id: None,
                },
            )
            .await
    }

    async fn view(&self) -> CapsView {
        self.caps.view().await.unwrap()
    }

    /// The `cap` row of `caps.get`.
    async fn row(&self, cap: &str) -> CapRow {
        let view = self.view().await;
        view.caps
            .iter()
            .find(|c| c.cap == cap)
            .cloned()
            .unwrap_or_else(|| panic!("no {cap} cap: {view:?}"))
    }

    /// `(settled, held)` of the monthly cap.
    async fn monthly(&self) -> (String, String) {
        let row = self.row("monthly").await;
        (row.settled, row.held)
    }

    async fn reserve(&self, amount: &str, owner: &str) -> ReserveOutcome {
        self.caps
            .reserve(
                &usd(amount),
                ReserveFor {
                    owner,
                    workspace: "none",
                },
                None,
            )
            .await
            .unwrap()
    }

    async fn reserved(&self, amount: &str, owner: &str) -> String {
        match self.reserve(amount, owner).await {
            ReserveOutcome::Reserved(id) => id,
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    fn caller(&self, caller: CallerKind, call_id: &str, reservation: Option<String>) -> JobCaller {
        JobCaller {
            idem_scope: format!("{caller}|none"),
            caller,
            workspace: None,
            trace_id: "t_caps".into(),
            idem_key: Some(self.ids.id("k")),
            call_id: call_id.into(),
            reservation,
            submitted: Arc::new(SubmittedSlot::new()),
            origin: Some(JobOrigin {
                tool: "x.paid".into(),
                arguments: json!({}),
            }),
            retry_of: None,
        }
    }

    async fn wait(&self, id: &str) -> lil_core::jobs::JobSummary {
        let caller = self.caller(CallerKind::Cli, "c_wait", None);
        let (job, done) = self
            .jobs
            .wait(&caller, &JobId(id.to_string()), DEADLINE)
            .await
            .unwrap();
        assert!(done, "the job did not end: {job:?}");
        job
    }

    fn period(&self) -> String {
        self.caps.period(self.clock.utc_now())
    }

    fn loose(&self) -> PathBuf {
        self.env.env.local_dir().join("usage-loose.jsonl")
    }

    /// A live subscription to the `caps` topic (from now on).
    async fn watch_caps(&self) -> Watch {
        static CLIENT: AtomicU64 = AtomicU64::new(1000);
        let client = CLIENT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        self.events.attach(client, tx);
        self.events
            .subscribe(client, &["caps".to_string()], &BTreeMap::new())
            .await;
        Watch {
            rx: tokio::sync::Mutex::new(rx),
        }
    }
}

/// The `caps` events a [`Rig::watch_caps`] subscription has received.
struct Watch {
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<String>>,
}

impl Watch {
    /// The events published since the last call (a marker event ends the read).
    async fn events(&self, rig: &Rig) -> Vec<Value> {
        rig.events.publish("caps", json!({"kind": "end_of_read"}));
        let mut rx = self.rx.lock().await;
        let mut out = Vec::new();
        let read = async {
            while let Some(line) = rx.recv().await {
                let event: Value = serde_json::from_str(&line).unwrap();
                let data = event["params"]["data"].clone();
                if data["kind"] == "end_of_read" {
                    break;
                }
                out.push(data);
            }
        };
        tokio::time::timeout(DEADLINE, read).await.unwrap();
        out
    }
}

/// Polls `f` until it gives `Some` (a failure deadline, never a timing assertion).
async fn until<T, F, Fut>(what: &str, f: F) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let poll = async {
        loop {
            if let Some(v) = f().await {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(DEADLINE, poll)
        .await
        .unwrap_or_else(|_| panic!("{what}: not reached"))
}

fn money(m: &str) -> Money {
    Money::parse_decimal(m).unwrap()
}

/// A pay-as-you-go estimate of `amount` USD from `fakeprov`.
fn usd(amount: &str) -> Estimate {
    external("fakeprov", amount, "USD", BillingBasis::Payg)
}

fn external(provider: &str, amount: &str, currency: &str, basis: BillingBasis) -> Estimate {
    Estimate::External {
        provider: provider.into(),
        amount: money(amount),
        currency: Currency::parse(currency).unwrap(),
        basis,
    }
}

fn session(caller: CallerKind, id: &str, non_interactive: bool) -> Arc<Session> {
    Arc::new(Session {
        id: id.into(),
        caller,
        non_interactive,
        workspace: None,
        capabilities: lil_protocol::CAPABILITIES
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        client_pid: 1,
        profile_id: None,
    })
}

fn cli() -> Arc<Session> {
    session(CallerKind::Cli, "s_cli", false)
}

fn app() -> Arc<Session> {
    session(CallerKind::Ui("test".into()), "s_app", false)
}

fn next_key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("k-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn code(outcome: &Outcome<ToolOutput>) -> String {
    match outcome {
        Outcome::Error { error } => error.code.clone(),
        other => panic!("expected an error, got {other:?}"),
    }
}

fn ok(outcome: &Outcome<ToolOutput>) -> Value {
    match outcome {
        Outcome::Ok(out) => out.structured.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn pending(outcome: &Outcome<ToolOutput>) -> lil_protocol::PendingConfirmation {
    match outcome {
        Outcome::PendingConfirmation(p) => p.clone(),
        other => panic!("expected pending_confirmation, got {other:?}"),
    }
}

fn answer(id: &str, answer: Answer, cap: Option<CapAnswer>) -> ConfirmationAnswerParams {
    ConfirmationAnswerParams {
        confirmation_id: id.into(),
        answer,
        cap,
    }
}

fn allow_once() -> Option<CapAnswer> {
    Some(CapAnswer {
        action: CapAction::AllowOnce,
        raise_to: None,
    })
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

const MONTHLY_10: &str = "[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"10\"\n";
const MONTHLY_1: &str = "[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n";

// ---------------------------------------------------------------------------------------------
// Reserve, settle and repair.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-088, CORE-071, QA-047
async fn crash_between_usage_line_and_settle_is_repaired_once() {
    let rig = Rig::new(MONTHLY_10);
    // As `meter.after_usage_line=abort` in a spawned core: the line is in, then nothing more
    // happens (no settle, the row stays).
    rig.fp.on(
        "meter.after_usage_line",
        Box::new(|| Err(std::io::Error::other("crash after the usage line"))),
    );
    let s = session(CallerKind::Cli, "s_cli", true);
    let out = rig
        .call(
            &s,
            "test.meter_external",
            json!({"provider": "fakeprov", "amount": "1"}),
            &next_key(),
        )
        .await;
    ok(&out);
    assert_eq!(lines(&rig.loose()).len(), 1);
    assert_eq!(
        rig.monthly().await,
        ("0".into(), "1".into()),
        "not settled yet"
    );

    // The next start: a new run of the caps and the meter on the same state.
    let caps = Caps::new(
        rig.states.clone(),
        rig.env.env.clone(),
        rig.clock.clone(),
        rig.events.clone(),
        FailPoints::none(),
        rig.ids.clone(),
    );
    let meter = Meter::new(
        rig.states.clone(),
        rig.env.env.clone(),
        rig.clock.clone(),
        rig.ids.clone(),
        rig.events.clone(),
        rig.telemetry.clone(),
        PowerSettings::default(),
        FailPoints::none(),
    );
    assert_eq!(caps.repair().await.unwrap(), 1);
    assert_eq!(meter.recover().await.unwrap(), 0, "the line was in already");
    assert_eq!(
        rig.monthly().await,
        ("1".into(), "0".into()),
        "settled once"
    );
    assert_eq!(lines(&rig.loose()).len(), 1, "one line");
    // Another start finds nothing to repair.
    assert_eq!(caps.repair().await.unwrap(), 0);
    let again = Caps::new(
        rig.states.clone(),
        rig.env.env.clone(),
        rig.clock.clone(),
        rig.events.clone(),
        FailPoints::none(),
        rig.ids.clone(),
    );
    assert_eq!(again.repair().await.unwrap(), 0);
    assert_eq!(rig.monthly().await, ("1".into(), "0".into()));
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-088, CORE-071
async fn a_dead_call_without_a_usage_row_is_released_and_one_with_a_pending_row_kept() {
    let rig = Rig::new(MONTHLY_10);
    let nothing_sent = rig.reserved("2", "c_0000000000000aaa").await;
    // A call whose write-ahead row is still pending when the core dies: its request may have
    // been sent.
    let held = rig.reserved("3", "c_0000000000000bbb").await;
    let ctx = UsageCtx {
        workspace: None,
        project: None,
        asset: None,
        job_id: None,
        call_id: "c_0000000000000bbb".into(),
        caller_kind: "cli".into(),
        reservation: Some(held.clone()),
    };
    let draft = UsageDraft {
        tool: "test.meter_external".into(),
        provider: "fakeprov".into(),
        model: None,
        route: "test".into(),
        basis: BillingBasis::Payg,
        units: serde_json::Map::new(),
        est_cost: Some((money("3"), Currency::parse("USD").unwrap())),
        account: None,
        raw_usage: None,
        attribution: Attribution::Suite,
        status: UsageStatus::Pending,
        latency_ms: None,
    };
    rig.meter.begin(&ctx, draft).await.unwrap();
    assert_eq!(rig.monthly().await, ("0".into(), "5".into()));

    let caps = Caps::new(
        rig.states.clone(),
        rig.env.env.clone(),
        rig.clock.clone(),
        rig.events.clone(),
        FailPoints::none(),
        rig.ids.clone(),
    );
    assert_eq!(caps.repair().await.unwrap(), 2);
    let gone = caps.reservation(&nothing_sent).await.unwrap().unwrap();
    assert_eq!(gone.state, ReservationState::Released);
    let kept = caps.reservation(&held).await.unwrap().unwrap();
    assert_eq!((kept.state, kept.kept), (ReservationState::Held, true));
    assert_eq!(
        rig.monthly().await,
        ("0".into(), "3".into()),
        "the pending one stays held"
    );
    // Later starts keep it too (it is `kept`), after the meter wrote its pending line.
    let meter = Meter::new(
        rig.states.clone(),
        rig.env.env.clone(),
        rig.clock.clone(),
        rig.ids.clone(),
        rig.events.clone(),
        rig.telemetry.clone(),
        PowerSettings::default(),
        FailPoints::none(),
    );
    assert_eq!(meter.recover().await.unwrap(), 1);
    let later = Caps::new(
        rig.states.clone(),
        rig.env.env.clone(),
        rig.clock.clone(),
        rig.events.clone(),
        FailPoints::none(),
        rig.ids.clone(),
    );
    assert_eq!(later.repair().await.unwrap(), 0);
    assert_eq!(rig.monthly().await, ("0".into(), "3".into()));
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-012, QA-047
async fn month_boundary_and_dst_follow_local_calendar() {
    let rig = Rig::new(MONTHLY_1);
    let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
    // CET (+1 h) until the DST change on 2027-03-28T01:00Z, then CEST (+2 h).
    let dst = 1_806_195_600;
    rig.clock
        .set_offsets(&[(UNIX_EPOCH, 3600), (at(dst), 7200)]);
    // 2027-01-31T22:30Z is 23:30 local, 2027-01-31T23:30Z is 00:30 on 1 February.
    let jan_end = 1_801_434_600;
    assert_eq!(rig.caps.period(at(jan_end)), "2027-01");
    assert_eq!(rig.caps.period(at(jan_end + 3600)), "2027-02");
    // 2027-03-31T22:30Z is 00:30 on 1 April in summer time (it would still be March in CET).
    assert_eq!(rig.caps.period(at(1_806_532_200)), "2027-04");
    assert_eq!(rig.caps.period(at(1_806_532_200 - 3600)), "2027-03");

    // January's spend doesn't count once February begins locally.
    rig.clock.shift_utc(i64::try_from(jan_end - START).unwrap());
    let rv = rig.reserved("0.9", "c_jan").await;
    rig.caps.settle(&rv, None, true).await.unwrap();
    let held = rig.reserved("0.05", "c_jan_held").await;
    assert!(matches!(
        rig.caps.check(&usd("0.2")).await.unwrap(),
        CapCheck::Exceeds(_)
    ));
    rig.clock.shift_utc(3600);
    assert_eq!(rig.period(), "2027-02");
    assert_eq!(rig.caps.check(&usd("0.9")).await.unwrap(), CapCheck::Within);
    assert_eq!(rig.monthly().await, ("0".into(), "0".into()));
    // A January reservation settled in February counts in January, its month.
    rig.caps.settle(&held, None, true).await.unwrap();
    assert_eq!(rig.monthly().await, ("0".into(), "0".into()));
    let jan = rig.caps.list("2027-01").await.unwrap();
    assert_eq!(jan.len(), 2);
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-085
async fn per_call_max_and_provider_caps_apply() {
    let rig = Rig::new(
        "[money]\ndisplay_currency = \"USD\"\n[caps]\nper_call_max = \"1\"\n\
         [caps.provider_monthly]\nfakeprov = \"2\"\n",
    );
    let CapCheck::Exceeds(hits) = rig.caps.check(&usd("1.5")).await.unwrap() else {
        panic!("1.5 passes the per-call maximum");
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(
        (hits[0].cap.as_str(), hits[0].limit),
        ("per_call_max", money("1"))
    );
    for owner in ["c_1", "c_2"] {
        rig.reserved("1", owner).await;
    }
    let ReserveOutcome::NeedsCap(hits) = rig.reserve("0.5", "c_3").await else {
        panic!("the provider's cap is used up");
    };
    assert_eq!(hits[0].cap, "provider:fakeprov");
    assert_eq!((hits[0].held, hits[0].estimate), (money("2"), money("0.5")));
    // Another provider has no cap of its own.
    let other = external("otherprov", "0.5", "USD", BillingBasis::Payg);
    assert_eq!(rig.caps.check(&other).await.unwrap(), CapCheck::Within);
    let row = rig.row("provider:fakeprov").await;
    assert_eq!((row.held.as_str(), row.ratio_permille), ("2", 1000));
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-085, CORE-088
async fn reservation_records_rate_workspace_and_job() {
    let rig = Rig::new(
        "[money]\ndisplay_currency = \"USD\"\n\
         [[money.rates]]\ncurrency = \"EUR\"\nin_display = \"1.1\"\ndate = \"2026-01-01\"\n\
         [[money.rates]]\ncurrency = \"EUR\"\nin_display = \"1.2\"\ndate = \"2099-01-01\"\n",
    );
    let eur = external("fakeprov", "2", "EUR", BillingBasis::Payg);
    let ReserveOutcome::Reserved(id) = rig
        .caps
        .reserve(
            &eur,
            ReserveFor {
                owner: "c_0000000000000042",
                workspace: "ws_abc",
            },
            None,
        )
        .await
        .unwrap()
    else {
        panic!("a reservation");
    };
    let r = rig.caps.reservation(&id).await.unwrap().unwrap();
    assert_eq!(
        (r.workspace.as_str(), r.owner.as_str()),
        ("ws_abc", "c_0000000000000042")
    );
    assert_eq!((r.currency.as_str(), r.amount), ("EUR", money("2")));
    // The newest rate dated on or before today, recorded with the reservation.
    assert_eq!(
        (r.rate_micros, r.rate_date.as_deref()),
        (1_100_000, Some("2026-01-01"))
    );
    assert_eq!(
        (r.display_currency.as_str(), r.display),
        ("USD", money("2.2"))
    );

    // A job takes its call's reservation once its row is in; an automatic re-run takes it over.
    let rv = rig.reserved("1", "c_0000000000000043").await;
    let caller = rig.caller(CallerKind::Cli, "c_0000000000000043", Some(rv.clone()));
    let job = rig
        .jobs
        .submit(&caller, spec(json!({"end": "interrupt_once"})))
        .await
        .unwrap();
    rig.wait(&job.job_id).await;
    let owner = until("the re-run takes the reservation", || async {
        let owner = rig.caps.reservation(&rv).await.unwrap().unwrap().owner;
        (owner != job.job_id).then_some(owner)
    })
    .await;
    assert!(owner.starts_with("j_"), "the re-run owns it: {owner}");
    let rerun = rig.wait(&owner).await;
    assert_eq!(rerun.retry_of.as_deref(), Some(job.job_id.as_str()));
    assert_eq!(rerun.status, JobStatus::Succeeded);
    let r = rig.caps.reservation(&rv).await.unwrap().unwrap();
    assert_eq!(
        (r.state, r.settled),
        (ReservationState::Settled, Some(money("1")))
    );
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-088
async fn a_job_takes_its_calls_reservation_once_its_row_is_in() {
    let rig = Rig::new(MONTHLY_10);
    let rv = rig.reserved("1", "c_0000000000000051").await;
    let caller = rig.caller(CallerKind::Cli, "c_0000000000000051", Some(rv.clone()));
    let job = rig
        .jobs
        .submit(&caller, spec(json!({"end": "cancel"})))
        .await
        .unwrap();
    let r = rig.caps.reservation(&rv).await.unwrap().unwrap();
    assert_eq!(r.owner, job.job_id);
    let ui = rig.caller(CallerKind::Ui("test".into()), "c_cancel", None);
    rig.jobs
        .cancel(&ui, &JobId(job.job_id.clone()))
        .await
        .unwrap();
    rig.wait(&job.job_id).await;
    let r = rig.caps.reservation(&rv).await.unwrap().unwrap();
    assert_eq!(r.state, ReservationState::Released, "a cancel releases it");
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-088, AI-029
async fn failed_unbilled_call_releases_and_interrupted_unknown_keeps() {
    let rig = Rig::new(MONTHLY_10);
    let s = session(CallerKind::Cli, "s_cli", true);
    let out = rig
        .call(
            &s,
            "test.meter_external",
            json!({"provider": "fakeprov", "amount": "2", "fail": true}),
            &next_key(),
        )
        .await;
    assert_eq!(code(&out), "provider.rate_limited");
    assert_eq!(rig.monthly().await, ("0".into(), "0".into()), "released");

    let rv = rig.reserved("2", "c_0000000000000061").await;
    let caller = rig.caller(CallerKind::Cli, "c_0000000000000061", Some(rv.clone()));
    let job = rig
        .jobs
        .submit(&caller, spec(json!({"end": "unknown"})))
        .await
        .unwrap();
    let unknown = rig.wait(&job.job_id).await;
    assert_eq!(unknown.status, JobStatus::Running, "{unknown:?}");
    assert_eq!(
        rig.monthly().await,
        ("0".into(), "2".into()),
        "held at the reserved amount"
    );
    let CapCheck::Exceeds(hits) = rig.caps.check(&usd("9")).await.unwrap() else {
        panic!("2 held + 9 pass the cap of 10");
    };
    assert_eq!((hits[0].held, hits[0].settled), (money("2"), money("0")));
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-088, AI-029
async fn resolving_an_unknown_job_settles_at_the_reservation() {
    let rig = Rig::new(MONTHLY_10);
    let rv = rig.reserved("2", "c_0000000000000071").await;
    let caller = rig.caller(CallerKind::Cli, "c_0000000000000071", Some(rv.clone()));
    let job = rig
        .jobs
        .submit(&caller, spec(json!({"end": "unknown"})))
        .await
        .unwrap();
    rig.wait(&job.job_id).await;
    let id = JobId(job.job_id.clone());
    // An agent can't resolve it.
    let agent = rig.caller(CallerKind::Cli, "c_agent", None);
    let refused = rig.jobs.cancel(&agent, &id).await.unwrap_err();
    assert_eq!(refused.code, "confirm.unavailable");
    assert_eq!(rig.monthly().await, ("0".into(), "2".into()));
    // You do, from the app: it may have been charged, so it settles at the reservation.
    let ui = rig.caller(CallerKind::Ui("test".into()), "c_ui", None);
    rig.jobs.cancel(&ui, &id).await.unwrap();
    let resolved = rig.wait(&job.job_id).await;
    assert_eq!(resolved.status, JobStatus::Cancelled);
    assert_eq!(rig.monthly().await, ("2".into(), "0".into()));
    let r = rig.caps.reservation(&rv).await.unwrap().unwrap();
    assert_eq!(
        r.state,
        ReservationState::Settled,
        "settled, never released"
    );

    // An ordinary cancel of a queued paid job releases its reservation.
    let blocker = rig
        .jobs
        .submit(
            &rig.caller(CallerKind::Cli, "c_block", None),
            spec(json!({"end": "cancel"})),
        )
        .await
        .unwrap();
    let rv = rig.reserved("3", "c_0000000000000072").await;
    let caller = rig.caller(CallerKind::Cli, "c_0000000000000072", Some(rv.clone()));
    let queued = rig.jobs.submit(&caller, spec(json!({}))).await.unwrap();
    assert_eq!(queued.status, JobStatus::Queued, "{queued:?}");
    rig.jobs
        .cancel(&ui, &JobId(queued.job_id.clone()))
        .await
        .unwrap();
    assert_eq!(rig.wait(&queued.job_id).await.status, JobStatus::Cancelled);
    rig.jobs
        .cancel(&ui, &JobId(blocker.job_id.clone()))
        .await
        .unwrap();
    rig.wait(&blocker.job_id).await;
    assert_eq!(
        rig.monthly().await,
        ("2".into(), "0".into()),
        "held 0, nothing more settled"
    );
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-016, CORE-088
async fn a_replayed_key_releases_its_own_reservation() {
    let rig = Rig::new(MONTHLY_10);
    let rv_1 = rig.reserved("2", "c_0000000000000081").await;
    let mut first = rig.caller(CallerKind::Cli, "c_0000000000000081", Some(rv_1.clone()));
    first.idem_key = Some("K".into());
    let job = rig
        .jobs
        .submit(&first, spec(json!({"end": "cancel"})))
        .await
        .unwrap();
    // The core lost that call's outcome; the client retries K, and the new call reserves too.
    let rv_2 = rig.reserved("2", "c_0000000000000082").await;
    assert_eq!(rig.monthly().await, ("0".into(), "4".into()));
    let mut retry = rig.caller(CallerKind::Cli, "c_0000000000000082", Some(rv_2.clone()));
    retry.idem_key = Some("K".into());
    let again = rig
        .jobs
        .submit(&retry, spec(json!({"end": "cancel"})))
        .await
        .unwrap();
    assert_eq!(again.job_id, job.job_id, "the key gives back the first job");
    assert_eq!(
        rig.monthly().await,
        ("0".into(), "2".into()),
        "only the job's is held"
    );
    let r = rig.caps.reservation(&rv_2).await.unwrap().unwrap();
    assert_eq!(r.state, ReservationState::Released);
    assert_eq!(
        rig.caps.reservation(&rv_1).await.unwrap().unwrap().owner,
        job.job_id
    );
    assert!(
        lines(&rig.loose()).is_empty(),
        "the replay wrote no usage line"
    );
    // A release by someone else, or of a settled one, changes nothing.
    rig.caps.release(&rv_1, "c_0000000000000082").await;
    assert_eq!(rig.monthly().await, ("0".into(), "2".into()));
    let ui = rig.caller(CallerKind::Ui("test".into()), "c_ui", None);
    rig.jobs
        .cancel(&ui, &JobId(job.job_id.clone()))
        .await
        .unwrap();
    rig.wait(&job.job_id).await;
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-089
async fn alerts_once_per_level_per_period() {
    let rig = Rig::new(MONTHLY_1);
    let watch = rig.watch_caps().await;
    // 60 %, 70 %, 85 %, 100 %, 115 %.
    for amount in ["0.6", "0.1", "0.15", "0.15", "0.15"] {
        let rv = match rig
            .caps
            .reserve(
                &usd(amount),
                ReserveFor {
                    owner: "c_a",
                    workspace: "none",
                },
                Some("cf_x"),
            )
            .await
            .unwrap()
        {
            ReserveOutcome::Reserved(id) => id,
            other => panic!("{other:?}"),
        };
        rig.caps.settle(&rv, None, true).await.unwrap();
        rig.caps.settle(&rv, None, true).await.unwrap();
    }
    let alerts = |events: &[Value]| -> Vec<(String, u64, String)> {
        events
            .iter()
            .filter(|e| e["kind"] == "alert")
            .map(|e| {
                (
                    e["cap"].as_str().unwrap().to_string(),
                    e["threshold_pct"].as_u64().unwrap(),
                    e["period"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let jan = rig.period();
    let want: Vec<(String, u64, String)> = [50, 80, 100]
        .iter()
        .map(|l| ("monthly".to_string(), *l, jan.clone()))
        .collect();
    assert_eq!(alerts(&watch.events(&rig).await), want);
    assert_eq!(rig.monthly().await.0, "1.15", "settling twice counted once");
    // A new month starts again.
    rig.clock.shift_utc(31 * 86_400);
    let feb = rig.period();
    assert_ne!(feb, jan);
    let rv = rig.reserved("0.5", "c_b").await;
    rig.caps.settle(&rv, None, true).await.unwrap();
    let feb_alerts = alerts(&watch.events(&rig).await);
    assert_eq!(feb_alerts, [("monthly".to_string(), 50, feb)]);
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-090, CORE-083
async fn quota_local_and_fixed_never_count() {
    let rig = Rig::new(
        "[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n\
         [[fixed_costs]]\nname = \"storage\"\namount = \"100\"\ncurrency = \"USD\"\nperiod = \"monthly\"\n",
    );
    for basis in [
        BillingBasis::SubscriptionQuota,
        BillingBasis::FreeTier,
        BillingBasis::Local,
    ] {
        let est = external("fakeprov", "100", "USD", basis);
        assert_eq!(
            rig.caps.check(&est).await.unwrap(),
            CapCheck::Within,
            "{basis:?}"
        );
        assert_eq!(
            rig.reserve_est(&est).await,
            ReserveOutcome::None,
            "{basis:?}"
        );
    }
    for est in [Estimate::Zero, Estimate::Local { gpu: true }] {
        assert_eq!(rig.reserve_est(&est).await, ReserveOutcome::None);
    }
    // The fixed cost of the month doesn't count either.
    assert_eq!(rig.caps.check(&usd("0.5")).await.unwrap(), CapCheck::Within);
    // Prepaid credits and BYOK money do.
    for basis in [BillingBasis::PrepaidCredits, BillingBasis::Byok] {
        let est = external("fakeprov", "2", "USD", basis);
        assert!(
            matches!(rig.caps.check(&est).await.unwrap(), CapCheck::Exceeds(_)),
            "{basis:?}"
        );
    }
    assert_eq!(rig.monthly().await, ("0".into(), "0".into()));
}

impl Rig {
    async fn reserve_est(&self, est: &Estimate) -> ReserveOutcome {
        self.caps
            .reserve(
                est,
                ReserveFor {
                    owner: "c_x",
                    workspace: "none",
                },
                None,
            )
            .await
            .unwrap()
    }
}

// ---------------------------------------------------------------------------------------------
// Cap confirmations, through the dispatcher.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-015, APP-045, APP-054, ARCH-056
async fn one_prompt_covers_confirm_and_cap() {
    let rig = Rig::new(MONTHLY_1);
    rig.confirm.presence().opened();
    let k = next_key();
    let out = rig
        .call(&cli(), "test.destructive", json!({"cost": "2"}), &k)
        .await;
    let p = pending(&out);
    assert_eq!(p.covers, [Cover::Confirm, Cover::Cap]);
    let open = rig
        .confirm
        .list(lil_core::confirm::ListState::Pending, 100)
        .await
        .unwrap();
    assert_eq!(open.len(), 1, "one confirmation, not two");
    let cap = open[0].cap.clone().unwrap();
    assert_eq!(cap[0]["cap"], "monthly");
    assert_eq!(
        (cap[0]["limit"].as_str(), cap[0]["estimate"].as_str()),
        (Some("1"), Some("2"))
    );
    // An approval of a cap needs a cap action.
    let bare = rig
        .confirm
        .answer(&app(), answer(&p.confirmation_id, Answer::Approve, None))
        .await;
    assert_eq!(bare.unwrap_err().code, "params.invalid");
    rig.confirm
        .answer(
            &app(),
            answer(&p.confirmation_id, Answer::Approve, allow_once()),
        )
        .await
        .unwrap();
    let ran = rig
        .call(&cli(), "test.destructive", json!({"cost": "2"}), &k)
        .await;
    assert_eq!(ok(&ran)["count"], 1);
    assert_eq!(rig.monthly().await, ("2".into(), "0".into()));

    // Your own action over a cap asks about the cap only, inline (APP-054).
    let own = rig
        .call(
            &app(),
            "test.destructive",
            json!({"cost": "1"}),
            &next_key(),
        )
        .await;
    assert_eq!(pending(&own).covers, [Cover::Cap]);
    // Your own action within the caps never asks.
    let caps_off = Rig::new("");
    let mine = caps_off
        .call(
            &app(),
            "test.destructive",
            json!({"cost": "1"}),
            &next_key(),
        )
        .await;
    assert_eq!(ok(&mine)["count"], 1);
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-087, CORE-091, APP-045
async fn race_after_stage_5_asks_for_the_cap_only() {
    let rig = Rig::new(MONTHLY_10);
    rig.confirm.presence().opened();
    let k = next_key();
    let p = pending(
        &rig.call(&cli(), "test.destructive", json!({"cost": "2"}), &k)
            .await,
    );
    assert_eq!(p.covers, [Cover::Confirm], "within the cap at stage 5");
    rig.confirm
        .answer(&app(), answer(&p.confirmation_id, Answer::Approve, None))
        .await
        .unwrap();
    // Before the retry another call takes the room.
    rig.reserved("9", "c_other").await;
    let again = pending(
        &rig.call(&cli(), "test.destructive", json!({"cost": "2"}), &k)
            .await,
    );
    assert_eq!(
        again.covers,
        [Cover::Cap],
        "the confirmation was given; the cap is asked"
    );
    assert_eq!(again.retry_token, k);
    assert_ne!(again.confirmation_id, p.confirmation_id);
    rig.confirm
        .answer(
            &app(),
            answer(&again.confirmation_id, Answer::Approve, allow_once()),
        )
        .await
        .unwrap();
    let ran = rig
        .call(&cli(), "test.destructive", json!({"cost": "2"}), &k)
        .await;
    assert_eq!(ok(&ran)["count"], 1, "it ran once");
    let again = rig
        .call(&cli(), "test.destructive", json!({"cost": "2"}), &k)
        .await;
    assert_eq!(ok(&again)["count"], 1, "a replay runs nothing");
    assert_eq!(rig.monthly().await, ("2".into(), "9".into()));
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-091, CORE-008
async fn allow_once_and_raise_answers() {
    let rig = Rig::new(MONTHLY_1);
    rig.confirm.presence().opened();
    let toml = || std::fs::read_to_string(rig.env.env.suite_toml()).unwrap();
    let before = toml();
    let args = json!({"provider": "fakeprov", "amount": "2"});

    // Allow once: that call only, and the cap stays as it is.
    let k = next_key();
    let p = pending(
        &rig.call(&cli(), "test.meter_external", args.clone(), &k)
            .await,
    );
    assert_eq!(p.covers, [Cover::Cap]);
    rig.confirm
        .answer(
            &app(),
            answer(&p.confirmation_id, Answer::Approve, allow_once()),
        )
        .await
        .unwrap();
    ok(&rig
        .call(&cli(), "test.meter_external", args.clone(), &k)
        .await);
    assert_eq!(toml(), before, "allow once never changes the cap");
    assert_eq!(rig.monthly().await, ("2".into(), "0".into()));
    let next = rig
        .call(&cli(), "test.meter_external", args.clone(), &next_key())
        .await;
    assert_eq!(
        pending(&next).covers,
        [Cover::Cap],
        "the next call asks again"
    );

    // Raise: the cap changes first, then the call runs within it.
    let k = next_key();
    let p = pending(
        &rig.call(&cli(), "test.meter_external", args.clone(), &k)
            .await,
    );
    let raise = Some(CapAnswer {
        action: CapAction::Raise,
        raise_to: Some(RaiseTo {
            cap: "monthly".into(),
            amount: "20".into(),
        }),
    });
    rig.confirm
        .answer(&app(), answer(&p.confirmation_id, Answer::Approve, raise))
        .await
        .unwrap();
    assert_eq!(rig.row("monthly").await.limit, "20");
    ok(&rig
        .call(&cli(), "test.meter_external", args.clone(), &k)
        .await);
    assert_eq!(rig.monthly().await, ("4".into(), "0".into()));

    // A raise without raise_to, and a deny.
    rig.caps
        .set(
            &app(),
            CapsSetParams {
                monthly: Some(Some("1".into())),
                ..CapsSetParams::default()
            },
        )
        .await
        .unwrap();
    let k = next_key();
    let p = pending(
        &rig.call(&cli(), "test.meter_external", args.clone(), &k)
            .await,
    );
    let half = Some(CapAnswer {
        action: CapAction::Raise,
        raise_to: None,
    });
    let refused = rig
        .confirm
        .answer(&app(), answer(&p.confirmation_id, Answer::Approve, half))
        .await;
    assert_eq!(refused.unwrap_err().code, "params.invalid");
    rig.confirm
        .answer(&app(), answer(&p.confirmation_id, Answer::Deny, None))
        .await
        .unwrap();
    let denied = rig.call(&cli(), "test.meter_external", args, &k).await;
    assert_eq!(code(&denied), "confirm.denied");
    assert_eq!(rig.monthly().await, ("4".into(), "0".into()));
}

#[tokio::test(flavor = "multi_thread")] // spec: QA-016, CORE-091
async fn non_interactive_is_cap_exceeded() {
    let rig = Rig::new(MONTHLY_1);
    rig.confirm.presence().opened();
    let args = json!({"provider": "fakeprov", "amount": "2"});
    let script = session(CallerKind::Cli, "s_script", true);
    let out = rig
        .call(&script, "test.meter_external", args.clone(), &next_key())
        .await;
    assert_eq!(code(&out), "cap.exceeded");
    let Outcome::Error { error } = out else {
        unreachable!()
    };
    assert_eq!(error.data.unwrap()["caps"][0]["cap"], "monthly");
    // Both needed: the safety confirmation's error.
    let both = rig
        .call(
            &script,
            "test.destructive",
            json!({"cost": "2"}),
            &next_key(),
        )
        .await;
    assert_eq!(code(&both), "confirm.unavailable");
    assert!(
        rig.confirm
            .list(lil_core::confirm::ListState::History, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        lines(&rig.loose()).is_empty(),
        "a refused call writes no usage line"
    );
    // An interactive caller with no app to answer gets the error too.
    let lonely = Rig::new(MONTHLY_1);
    let out = lonely
        .call(&cli(), "test.meter_external", args, &next_key())
        .await;
    assert_eq!(code(&out), "cap.exceeded");
    assert_eq!(lonely.monthly().await, ("0".into(), "0".into()));
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-015, CORE-088
async fn inline_settle_writes_the_line_before_settling() {
    let rig = Rig::new(MONTHLY_10);
    let seen: Arc<Mutex<Vec<(usize, CapRow)>>> = Arc::default();
    {
        let (seen, caps, loose) = (seen.clone(), rig.caps.clone(), rig.loose());
        rig.fp.on(
            "meter.after_usage_line",
            Box::new(move || {
                let view = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(caps.view())
                })
                .unwrap();
                let monthly = view.caps.into_iter().find(|c| c.cap == "monthly").unwrap();
                seen.lock().unwrap().push((lines(&loose).len(), monthly));
                Ok(())
            }),
        );
    }
    let s = session(CallerKind::Cli, "s_cli", true);
    ok(&rig
        .call(
            &s,
            "test.meter_external",
            json!({"provider": "fakeprov", "amount": "1"}),
            &next_key(),
        )
        .await);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, 1, "the line is in");
    assert_eq!(
        (seen[0].1.settled.as_str(), seen[0].1.held.as_str()),
        ("0", "1"),
        "not settled yet"
    );
    assert_eq!(rig.monthly().await, ("1".into(), "0".into()));
    let rv = rig.caps.list(&rig.period()).await.unwrap()[0].id.clone();
    rig.caps.settle(&rv, Some(money("5")), true).await.unwrap();
    rig.caps.settle(&rv, None, false).await.unwrap();
    assert_eq!(
        rig.monthly().await,
        ("1".into(), "0".into()),
        "a second settle changes nothing"
    );
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-088, QA1 I5
async fn node_install_settles_its_reservation_once() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    let path = |p: &Path| p.to_str().unwrap().to_string();
    let toml = format!(
        "{MONTHLY_10}[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\ncomfy_cli = '{}'\npython = '{}'\n",
        fake.port(),
        path(&fake.root()),
        path(&ensure_built().fake_comfy_cli),
        path(&ensure_built().fake_comfyui),
    );
    let rig = Rig::with(env, &toml);
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
    let rv = rig.reserved("1.5", "c_0000000000000091").await;
    let caller = rig.caller(CallerKind::Cli, "c_0000000000000091", Some(rv.clone()));
    let spec = install::job_spec(
        &NodeId("comfyui-test-node".into()),
        &ExactVersion("1.2.3".into()),
        "cli",
        Priority::Agent,
        None,
    );
    let job = rig.jobs.submit(&caller, spec).await.unwrap();
    let job = rig.wait(&job.job_id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    let all = lines(&rig.loose());
    assert_eq!(all.len(), 1, "one usage line, the install's own: {all:?}");
    let r = rig.caps.reservation(&rv).await.unwrap().unwrap();
    assert_eq!(
        (r.state, r.settled),
        (ReservationState::Settled, Some(money("1.5")))
    );
    assert_eq!(
        rig.monthly().await,
        ("1.5".into(), "0".into()),
        "settled once"
    );
}

// ---------------------------------------------------------------------------------------------
// The caps in suite.toml.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-085, ARCH-056
async fn invalid_cap_fails_closed_and_is_reported() {
    let rig = Rig::new("[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"ten\"\n");
    let watch = rig.watch_caps().await;
    let CapCheck::Exceeds(hits) = rig.caps.check(&usd("0.01")).await.unwrap() else {
        panic!("an invalid cap counts as exceeded");
    };
    assert_eq!(
        (hits[0].cap.as_str(), hits[0].limit),
        ("monthly", Money::ZERO)
    );
    let row = rig.row("monthly").await;
    assert_eq!(row.invalid, Some(true));
    assert!(matches!(
        rig.reserve("0.01", "c_1").await,
        ReserveOutcome::NeedsCap(_)
    ));
    let events = watch.events(&rig).await;
    let invalid: Vec<&Value> = events.iter().filter(|e| e["kind"] == "invalid").collect();
    assert_eq!(invalid.len(), 1, "reported once: {events:?}");
    assert_eq!(invalid[0]["caps"], json!(["monthly"]));
    // A rate that is missing while a cap applies fails closed too.
    let eur = Rig::new(MONTHLY_10);
    let err = eur
        .caps
        .check(&external("fakeprov", "1", "EUR", BillingBasis::Payg))
        .await
        .unwrap_err();
    assert_eq!(err.code, "caps.invalid");
    let caps_off = Rig::new("");
    let est = external("fakeprov", "1", "EUR", BillingBasis::Payg);
    assert_eq!(
        caps_off.caps.check(&est).await.unwrap(),
        CapCheck::Within,
        "no cap applies"
    );
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-085
async fn legacy_usd_keys_read_only_when_display_is_usd() {
    let legacy = "[caps]\nmonthly_usd = 3\nper_call_max_usd = \"1\"\n[caps.provider_monthly_usd]\nfakeprov = \"2\"\n";
    let usd_rig = Rig::new(&format!("[money]\ndisplay_currency = \"USD\"\n{legacy}"));
    let view = usd_rig.view().await;
    let caps: Vec<(&str, &str)> = view
        .caps
        .iter()
        .map(|c| (c.cap.as_str(), c.limit.as_str()))
        .collect();
    assert_eq!(
        caps,
        [
            ("monthly", "3"),
            ("per_call_max", "1"),
            ("provider:fakeprov", "2")
        ]
    );
    let eur_rig = Rig::new(&format!("[money]\ndisplay_currency = \"EUR\"\n{legacy}"));
    assert!(eur_rig.view().await.caps.is_empty(), "read only in USD");
    let eur = external("fakeprov", "50", "EUR", BillingBasis::Payg);
    assert_eq!(eur_rig.caps.check(&eur).await.unwrap(), CapCheck::Within);
    // The new key wins.
    let both = Rig::new("[caps]\nmonthly = \"5\"\nmonthly_usd = 3\n");
    assert_eq!(both.row("monthly").await.limit, "5");
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-008, ARCH-056
async fn caps_set_needs_a_verified_ui_session_and_keeps_comments() {
    let text = "# my caps, set by hand\n[money]\ndisplay_currency = \"USD\"\n\
                [caps]\nmonthly_usd = 9 # the old key\nper_call_max_usd = \"2\"\n";
    let rig = Rig::new(text);
    let watch = rig.watch_caps().await;
    let file = rig.env.env.suite_toml();
    let set = CapsSetParams {
        monthly: Some(Some("5".into())),
        ..CapsSetParams::default()
    };
    let mut no_caps_v1 = (*app()).clone();
    no_caps_v1.capabilities.retain(|c| c != "caps.v1");
    for who in [
        cli(),
        session(CallerKind::Mcp, "s_mcp", false),
        Arc::new(no_caps_v1),
    ] {
        let refused = rig.caps.set(&who, set.clone()).await.unwrap_err();
        assert_eq!(refused.code, "caller.not_allowed", "{}", who.caller);
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "unchanged");
    let bad = rig
        .caps
        .set(
            &app(),
            CapsSetParams {
                monthly: Some(Some("-1".into())),
                ..CapsSetParams::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, "caps.invalid");

    let view = rig.caps.set(&app(), set).await.unwrap();
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.starts_with("# my caps, set by hand\n"), "{after}");
    assert!(!after.contains("_usd"), "the legacy keys go: {after}");
    let rows: Vec<(&str, &str)> = view
        .caps
        .iter()
        .map(|c| (c.cap.as_str(), c.limit.as_str()))
        .collect();
    assert_eq!(
        rows,
        [("monthly", "5"), ("per_call_max", "2")],
        "carried, not lost"
    );
    let changed: Vec<Value> = watch
        .events(&rig)
        .await
        .into_iter()
        .filter(|e| e["kind"] == "changed")
        .collect();
    assert_eq!(changed.len(), 1);
    // Clearing a cap.
    let view = rig
        .caps
        .set(
            &app(),
            CapsSetParams {
                per_call_max: Some(None),
                ..CapsSetParams::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(view.caps.len(), 1);
    // No tool changes the caps: `caps.get` is the only caps tool, and it only reads.
    let names: Vec<String> = rig
        .dispatcher
        .registry()
        .descriptors()
        .into_iter()
        .map(|d| d.name)
        .filter(|n| n.starts_with("caps."))
        .collect();
    assert_eq!(names, ["caps.get"]);
}

// ---------------------------------------------------------------------------------------------
// `app/caps.set` over the pipe.
// ---------------------------------------------------------------------------------------------

/// An in-process core that trusts the test binaries' folder for apps (`lil-test-app.exe`).
fn core(env: &TestEnv) -> CoreThread {
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        trusted: TrustedDirs {
            app_dirs: vec![ensure_built().dir.clone()],
            test_app: true,
            ..base.trusted.clone()
        },
        ..base
    })
}

#[test] // spec: APP-043, CORE-008
fn app_caps_set_needs_caps_v1() {
    let env = TestEnv::new();
    lil_testkit::files::write_file(&env.env.suite_toml(), MONTHLY_1.as_bytes());
    let file = env.env.suite_toml();
    let before = std::fs::read_to_string(&file).unwrap();
    let _core = core(&env);
    let mut old = TestApp::start_with_capabilities(
        &env,
        None,
        &[],
        &["events.v1", "tools.v1", "jobs.v1", "confirmations.v1"],
    );
    assert_eq!(old.hello()["caller_kind"], "ui:test");
    let refused = old.call("app/caps.set", json!({"monthly": "5"}));
    assert_eq!(
        refused["result"]["error"]["code"], "caller.not_allowed",
        "{refused}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

    let mut app = TestApp::start(&env, None, &[]);
    let set = app.call("app/caps.set", json!({"monthly": "5"}));
    assert_eq!(set["result"]["status"], "ok", "{set}");
    assert_eq!(set["result"]["caps"][0]["limit"], "5", "{set}");
    let bad = app.call("app/caps.set", json!({"monthly_usd": "5"}));
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
}

#[test] // spec: CORE-091, APP-054
fn ui_cap_prompt_is_cancelled_when_its_app_closes() {
    let env = TestEnv::new();
    lil_testkit::files::write_file(&env.env.suite_toml(), MONTHLY_1.as_bytes());
    let _core = core(&env);
    let mut app = TestApp::start(&env, None, &[]);
    let args = json!({"provider": "fakeprov", "amount": "2"});
    let own = app.tool("test.meter_external", args, Some(&next_key()));
    assert_eq!(own["status"], "pending_confirmation", "{own}");
    assert_eq!(
        own["covers"],
        json!(["cap"]),
        "your own action asks about the cap: {own}"
    );
    let id = own["confirmation_id"].as_str().unwrap().to_string();
    drop(app);

    let mut other = TestApp::start(&env, None, &[]);
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let history = other.call("app/confirmations.list", json!({"state": "history"}));
        let state = history["result"]["confirmations"]
            .as_array()
            .and_then(|list| list.iter().find(|c| c["id"] == id.as_str()))
            .map(|c| c["state"].clone());
        if state == Some(json!("cancelled")) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the app's cap prompt was not cancelled: {history}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
