//! Unit tests of the `tools` module: the dispatcher with recording stages, fake tools,
//! `SeqIds` and a `FakeClock` (QA-031), over a real suite `state.db` in a `TestEnv`.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use lil_platform::{Clock, IdSource};
use lil_protocol::{
    CallerKind, EffectClass, ErrorCategory, FileRef, Outcome, RetryPolicy, ToolCallParams,
    ToolDescriptor, ToolOutput,
};
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use rusqlite::{Connection, OpenFlags};
use schemars::JsonSchema;
use serde_json::{Value, json};

use super::registry::schema_2020_12;
use super::*;
use crate::config::CoreConfig;
use crate::events::EventBus;
use crate::lifecycle::{DrainReason, IDLE_EXIT_AFTER, Lifecycle};
use crate::session::Session;
use crate::state::{OnFatal, StateRegistry};
use crate::store::FailPoints;
use crate::workspace::WorkspaceHub;
use lil_platform::fs::SystemProbe;

/// The CORE-015 stage names, in order.
const ALL_STAGES: [&str; 9] = [
    "resolve", "validate", "settings", "estimate", "confirm", "reserve", "execute", "meter",
    "record",
];

/// What the stages and fake tools did, in order.
#[derive(Debug, Clone, Default)]
struct Log(Arc<Mutex<Vec<&'static str>>>);

impl Log {
    fn push(&self, stage: &'static str) {
        self.0.lock().unwrap().push(stage);
    }

    fn take(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Hooks that log their stage, then do what the plan-2 stage does.
struct Recording {
    log: Log,
    plan2: PipelineHooks,
}

impl SettingsStage for Recording {
    fn resolve<'a>(
        &'a self,
        input: SettingsInput<'a>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Settings, lil_protocol::ErrorBody>> + Send + 'a,
        >,
    > {
        self.log.push("settings");
        self.plan2.settings.resolve(input)
    }
}

impl EstimateStage for Recording {
    fn estimate(&self, d: &ToolDescriptor, args: &Value, s: &Settings) -> Estimate {
        self.log.push("estimate");
        self.plan2.estimate.estimate(d, args, s)
    }
}

impl ConfirmStage for Recording {
    fn check<'a>(&'a self, i: ConfirmInput<'a>) -> BoxFuture<'a, ConfirmDecision> {
        self.log.push("confirm");
        self.plan2.confirm.check(i)
    }
}

impl ReserveStage for Recording {
    fn reserve<'a>(
        &'a self,
        i: ReserveInput<'a>,
    ) -> BoxFuture<'a, Result<ReserveOutcome, lil_protocol::ErrorBody>> {
        self.log.push("reserve");
        self.plan2.reserve.reserve(i)
    }
}

impl MeterStage for Recording {
    fn settle<'a>(&'a self, i: SettleInput<'a>) -> BoxFuture<'a, ()> {
        self.log.push("meter");
        self.plan2.meter.settle(i)
    }
}

impl RecordStage for Recording {
    fn record<'a>(
        &'a self,
        ctx: &'a CallCtx,
        d: &'a ToolDescriptor,
        plan: Option<crate::pipeline::RecordPlan>,
        out: &'a mut Outcome<ToolOutput>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.log.push("record");
        self.plan2.record.record(ctx, d, plan, out)
    }
}

fn recording_hooks(log: &Log) -> PipelineHooks {
    let rec = Arc::new(Recording {
        log: log.clone(),
        plan2: PipelineHooks::plan2(),
    });
    PipelineHooks {
        settings: rec.clone(),
        estimate: rec.clone(),
        confirm: rec.clone(),
        reserve: rec.clone(),
        meter: rec.clone(),
        record: rec,
    }
}

#[derive(JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "only its schema is used")]
struct RecArgs {
    n: Option<u64>,
}

#[derive(JsonSchema)]
#[expect(dead_code, reason = "only its schema is used")]
struct RecResult {
    executions: u64,
}

/// A fake tool that logs `resolve` (from `visible_to`), `validate` and `execute`, counts its
/// runs, and returns a file reference. Arguments with a `bad` field are invalid.
struct RecTool {
    descriptor: ToolDescriptor,
    log: Log,
    executions: AtomicU64,
    only_for: Option<CallerKind>,
}

impl RecTool {
    fn new(name: &str, effect: EffectClass, log: &Log) -> Arc<RecTool> {
        Arc::new(RecTool {
            descriptor: ToolDescriptor {
                name: name.to_string(),
                description: "A recording test tool.".into(),
                effect,
                module: "test".into(),
                input_schema: schema_2020_12::<RecArgs>(),
                output_schema: schema_2020_12::<RecResult>(),
                annotations: BTreeMap::new(),
            },
            log: log.clone(),
            executions: AtomicU64::new(0),
            only_for: None,
        })
    }

    fn executions(&self) -> u64 {
        self.executions.load(Ordering::SeqCst)
    }
}

impl Tool for RecTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn visible_to(&self, caller: &CallerKind) -> bool {
        self.log.push("resolve");
        self.only_for.as_ref().is_none_or(|only| only == caller)
    }

    fn validate(&self, args: &Value) -> Result<(), String> {
        self.log.push("validate");
        match args.get("bad") {
            Some(_) => Err("`bad` is not an argument".into()),
            None => Ok(()),
        }
    }

    fn run<'a>(&'a self, _ctx: &'a CallCtx, _args: Value) -> ToolFuture<'a> {
        Box::pin(async move {
            self.log.push("execute");
            let n = self.executions.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(ToolOutput {
                structured: json!({"executions": n}),
                artifacts: vec![FileRef {
                    path: r"out\Ünïcödé space.png".into(),
                    sha256: Some("ab".repeat(32)),
                }],
                summary_text: format!("run {n}"),
            })
        })
    }
}

fn as_dyn(tool: &Arc<RecTool>) -> Arc<dyn Tool> {
    tool.clone()
}

/// A dispatcher over the test-channel tools plus `extra`, with recording hooks.
struct Fixture {
    log: Log,
    gates: Arc<TestGates>,
    states: Arc<StateRegistry>,
    dispatcher: Dispatcher,
    /// Last, because fields drop in order: the dispatcher holds the suite `state.db` open,
    /// and the `TestEnv` cannot remove its folder until that handle is closed.
    env: TestEnv,
}

fn no_fatal() -> OnFatal {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn fixture(extra: &[Arc<dyn Tool>]) -> Fixture {
    fixture_logged(&Log::default(), extra)
}

/// [`fixture`] whose hooks log to `log`.
fn fixture_logged(log: &Log, extra: &[Arc<dyn Tool>]) -> Fixture {
    fixture_with(recording_hooks(log), log, extra)
}

/// [`fixture`] with `hooks`.
fn fixture_with(hooks: PipelineHooks, log: &Log, extra: &[Arc<dyn Tool>]) -> Fixture {
    let env = TestEnv::with_label("Ünïcödé space");
    let log = log.clone();
    let states = Arc::new(StateRegistry::open(&env.env, no_fatal()).expect("the suite state.db"));
    let gates = Arc::new(TestGates::new());
    let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
    let clock: Arc<dyn Clock> = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let lifecycle = Lifecycle::new(clock.clone(), IDLE_EXIT_AFTER);
    let hub = hub_for(&env, &states, &events, &clock, &ids);
    let mut reg = registry_with(&env, true, &gates, &states, &hub);
    for tool in extra {
        reg.register(tool.clone());
    }
    let dispatcher = Dispatcher::new(
        reg,
        hooks,
        states.clone(),
        events,
        hub,
        ids,
        clock,
        lifecycle,
    );
    Fixture {
        log,
        gates,
        states,
        dispatcher,
        env,
    }
}

/// `core_registry` over `env` with `hub` and the real probe.
fn registry_with(
    env: &TestEnv,
    test_tools: bool,
    gates: &Arc<TestGates>,
    states: &Arc<StateRegistry>,
    hub: &Arc<WorkspaceHub>,
) -> Registry {
    let probe: Arc<dyn lil_platform::fs::Probe> = Arc::new(SystemProbe);
    core_registry(
        test_tools,
        gates,
        states,
        hub,
        &env.env,
        &probe,
        Arc::default(),
    )
}

/// `core_registry` over `env`, with a hub of its own.
fn registry_for(
    env: &TestEnv,
    test_tools: bool,
    gates: &Arc<TestGates>,
    states: &Arc<StateRegistry>,
) -> Registry {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
    let events = Arc::new(EventBus::new(clock.clone()));
    let hub = hub_for(env, states, &events, &clock, &ids);
    registry_with(env, test_tools, gates, states, &hub)
}

/// A workspace hub over `env` (no fail points, the real probe).
fn hub_for(
    env: &TestEnv,
    states: &Arc<StateRegistry>,
    events: &Arc<EventBus>,
    clock: &Arc<dyn Clock>,
    ids: &Arc<dyn IdSource>,
) -> Arc<WorkspaceHub> {
    WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        events.clone(),
        clock.clone(),
        ids.clone(),
        FailPoints::none(),
        Arc::new(SystemProbe),
        crate::watch::WatchConfig::default(),
    )
}

fn session(caller: CallerKind, non_interactive: bool) -> Arc<Session> {
    Arc::new(Session {
        id: "s_1".into(),
        caller,
        non_interactive,
        workspace: None,
        capabilities: Vec::new(),
        client_pid: 1,
        profile_id: None,
    })
}

fn cli() -> Arc<Session> {
    session(CallerKind::Cli, false)
}

fn params(name: &str, args: Value, key: Option<&str>) -> ToolCallParams {
    let Value::Object(arguments) = args else {
        panic!("arguments must be an object");
    };
    ToolCallParams {
        name: name.into(),
        arguments,
        idempotency_key: key.map(str::to_string),
        trace_id: None,
    }
}

impl Fixture {
    async fn call(
        &self,
        s: &Arc<Session>,
        name: &str,
        args: Value,
        key: Option<&str>,
    ) -> Outcome<ToolOutput> {
        self.dispatcher
            .call(s.clone(), params(name, args, key))
            .await
    }

    /// The idempotency rows of the suite `state.db` as `(scope, key, state)`, read-only.
    fn rows(&self) -> Vec<(String, String, String)> {
        rows(&self.env.env.suite_state_db())
    }
}

fn rows(path: &Path) -> Vec<(String, String, String)> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open the state.db read-only");
    let mut stmt = db
        .prepare("SELECT scope, key, state FROM idempotency ORDER BY scope, key")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The error of an error outcome.
fn error(outcome: &Outcome<ToolOutput>) -> &lil_protocol::ErrorBody {
    match outcome {
        Outcome::Error { error } => error,
        other => panic!("expected an error result, got {other:?}"),
    }
}

fn ok(outcome: &Outcome<ToolOutput>) -> &ToolOutput {
    match outcome {
        Outcome::Ok(output) => output,
        other => panic!("expected an ok result, got {other:?}"),
    }
}

#[tokio::test] // spec: CORE-015
async fn stages_run_in_core_015_order() {
    let log = Log::default();
    let write = RecTool::new("rec.write", EffectClass::Write, &log);
    let read = RecTool::new("rec.read", EffectClass::Read, &log);
    let fx = fixture_logged(&log, &[write, read.clone()]);
    let out = fx.call(&cli(), "rec.write", json!({}), Some("k1")).await;
    assert_eq!(ok(&out).structured, json!({"executions": 1}));
    assert_eq!(log.take(), ALL_STAGES);

    // A read runs the same stages, without a key.
    ok(&fx.call(&cli(), "rec.read", json!({}), None).await);
    assert_eq!(log.take(), ALL_STAGES);
    assert_eq!(read.executions(), 1);
}

/// The plan-2 behaviour through the plan-3 stage signatures (`PipelineHooks::plan2()`).
#[tokio::test] // spec: CORE-015, QA-016, CORE-109
async fn plan2_stage_defaults_still_hold() {
    let fx = fixture(&[]);
    let hooks = PipelineHooks::plan2();
    let settings = Settings::default();
    let reg = fx.dispatcher.registry();
    for d in reg.descriptors() {
        assert_eq!(
            hooks.estimate.estimate(&d, &json!({}), &settings),
            Estimate::Zero,
            "{}",
            d.name
        );
    }
    let destructive = reg.get("test.destructive").unwrap().descriptor().clone();
    let read = reg.get("test.echo").unwrap().descriptor().clone();
    let ui = session(CallerKind::Ui("test".into()), false);
    let check = |s: Arc<Session>, d: ToolDescriptor| {
        let hooks = hooks.clone();
        async move {
            hooks
                .confirm
                .check(ConfirmInput {
                    session: &s,
                    descriptor: &d,
                    estimate: &Estimate::Zero,
                    args: &json!({}),
                    idem: None,
                    workspace: None,
                    retry: false,
                    always_confirm: false,
                })
                .await
        }
    };
    let proceed = ConfirmDecision::Proceed {
        confirmed_via: None,
        cap_override: None,
    };
    assert_eq!(
        check(cli(), destructive.clone()).await,
        ConfirmDecision::Unavailable
    );
    assert_eq!(check(ui.clone(), destructive).await, proceed);
    assert_eq!(check(cli(), read).await, proceed);
    let reserved = hooks
        .reserve
        .reserve(ReserveInput {
            session: &cli(),
            estimate: &Estimate::Zero,
            workspace_id: "none",
            call_id: "c_0000000000000001",
            allow_once: None,
        })
        .await;
    assert_eq!(reserved, Ok(ReserveOutcome::None));

    // `settle` writes nothing: the suite state.db and the workspace logs keep their bytes.
    let ws = fx.env.copy_fixture_workspace("record");
    let snapshot = || {
        let db = fx.env.env.suite_state_db();
        let mut files = vec![std::fs::read(&db).unwrap()];
        let wal = db.with_extension("db-wal");
        if wal.exists() {
            files.push(std::fs::read(&wal).unwrap());
        }
        for log in ["queue", "registry"] {
            let dir = ws.join(log);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut names: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
                names.sort();
                files.extend(names.iter().map(|p| std::fs::read(p).unwrap()));
            }
        }
        files
    };
    let before = snapshot();
    let outcome = Outcome::Ok(ToolOutput {
        structured: json!({}),
        artifacts: Vec::new(),
        summary_text: String::new(),
    });
    hooks
        .meter
        .settle(SettleInput {
            session: &ui,
            reservation: None,
            call_id: "c_0000000000000001",
            outcome: &outcome,
        })
        .await;
    assert_eq!(snapshot(), before);
}

#[tokio::test] // spec: CORE-015
async fn unknown_tool_is_tool_unknown() {
    let fx = fixture(&[]);
    let out = fx.call(&cli(), "no.such", json!({}), None).await;
    let e = error(&out);
    assert_eq!(e.code, "tool.unknown");
    assert_eq!(e.category, ErrorCategory::UserFixable);
    assert_eq!(e.retry, RetryPolicy::AfterResolve);
    assert!(e.trace_id.starts_with("t_"), "{e:?}");
    assert!(e.message.contains("no.such"), "{e:?}");

    // A caller's own trace id is kept; an invalid one is replaced.
    let mut p = params("no.such", json!({}), None);
    p.trace_id = Some("my-trace_1".into());
    let out = fx.dispatcher.call(cli(), p).await;
    assert_eq!(error(&out).trace_id, "my-trace_1");
    let mut p = params("no.such", json!({}), None);
    p.trace_id = Some("not valid!".into());
    let out = fx.dispatcher.call(cli(), p).await;
    assert!(error(&out).trace_id.starts_with("t_"));
}

#[tokio::test] // spec: CORE-015
async fn invalid_arguments_stop_after_validate() {
    let log = Log::default();
    let tool = RecTool::new("rec.write", EffectClass::Write, &log);
    let fx = fixture_logged(&log, &[as_dyn(&tool)]);
    let out = fx
        .call(&cli(), "rec.write", json!({"bad": true}), Some("k1"))
        .await;
    assert_eq!(error(&out).code, "params.invalid");
    assert_eq!(log.take(), ["resolve", "validate"]);
    assert_eq!(tool.executions(), 0);
    // The key was never claimed.
    assert!(fx.rows().is_empty(), "{:?}", fx.rows());
}

#[tokio::test] // spec: ARCH-035
async fn write_without_key_is_refused() {
    let fx = fixture(&[]);
    for key in [None, Some("")] {
        let out = fx
            .call(&cli(), "test.write", json!({"label": "a"}), key)
            .await;
        let e = error(&out);
        assert_eq!(e.code, "idempotency.key_required");
        assert_eq!(e.category, ErrorCategory::UserFixable);
    }
    // Neither refused call ran.
    let out = fx
        .call(&cli(), "test.write", json!({"label": "a"}), Some("k"))
        .await;
    assert_eq!(ok(&out).structured["executions"], 1);
    // A read needs no key.
    ok(&fx
        .call(&cli(), "test.echo", json!({"text": "x"}), None)
        .await);
}

#[tokio::test] // spec: CORE-016
async fn same_key_same_args_replays_without_executing() {
    let log = Log::default();
    let tool = RecTool::new("rec.write", EffectClass::Write, &log);
    let fx = fixture(&[as_dyn(&tool)]);
    let first = fx
        .call(&cli(), "rec.write", json!({"n": 1}), Some("k"))
        .await;
    let second = fx
        .call(&cli(), "rec.write", json!({"n": 1}), Some("k"))
        .await;
    ok(&first);
    assert_eq!(tool.executions(), 1);
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
    assert_eq!(fx.rows(), [("cli|none".into(), "k".into(), "done".into())]);
}

#[tokio::test] // spec: CORE-016
async fn same_key_different_args_conflicts() {
    let fx = fixture(&[]);
    let first = fx
        .call(&cli(), "test.write", json!({"label": "a"}), Some("k"))
        .await;
    ok(&first);
    let second = fx
        .call(&cli(), "test.write", json!({"label": "b"}), Some("k"))
        .await;
    let e = error(&second);
    assert_eq!(e.code, "idempotency.conflict");
    assert_eq!(e.category, ErrorCategory::DataConflict);
    assert_eq!(e.retry, RetryPolicy::Never);
    // The stored result is untouched.
    let again = fx
        .call(&cli(), "test.write", json!({"label": "a"}), Some("k"))
        .await;
    assert_eq!(again, first);
}

#[tokio::test] // spec: CORE-016
async fn key_scope_is_caller_kind_and_workspace() {
    let log = Log::default();
    let tool = RecTool::new("rec.write", EffectClass::Write, &log);
    let fx = fixture(&[as_dyn(&tool)]);
    let as_cli = fx.call(&cli(), "rec.write", json!({}), Some("k")).await;
    let mcp = session(CallerKind::Mcp, false);
    let as_mcp = fx.call(&mcp, "rec.write", json!({}), Some("k")).await;
    assert_eq!(ok(&as_cli).structured["executions"], 1);
    assert_eq!(ok(&as_mcp).structured["executions"], 2);
    assert_eq!(tool.executions(), 2);
    assert_eq!(
        fx.rows(),
        [
            ("cli|none".into(), "k".into(), "done".into()),
            ("mcp|none".into(), "k".into(), "done".into()),
        ]
    );
}

#[tokio::test] // spec: CORE-016
async fn error_outcomes_release_the_key() {
    let fx = fixture(&[]);
    let failing = json!({"label": "r", "fail": true});
    for _ in 0..2 {
        let out = fx
            .call(&cli(), "test.write", failing.clone(), Some("k"))
            .await;
        assert_eq!(error(&out).code, "params.invalid");
        assert!(fx.rows().is_empty(), "the key was kept: {:?}", fx.rows());
    }
    // The retry with the same key executes: the third run of label "r".
    let out = fx
        .call(&cli(), "test.write", json!({"label": "r"}), Some("k"))
        .await;
    assert_eq!(ok(&out).structured, json!({"label": "r", "executions": 3}));
}

#[tokio::test] // spec: ARCH-052, QA-016, ARCH-055
async fn agent_destructive_is_confirm_unavailable() {
    let log = Log::default();
    let destructive = RecTool::new("rec.destructive", EffectClass::Destructive, &log);
    let exec = RecTool::new("rec.exec", EffectClass::Exec, &log);
    let fx = fixture(&[destructive.clone(), exec.clone()]);
    for caller in [CallerKind::Cli, CallerKind::Mcp, CallerKind::Assistant] {
        for non_interactive in [false, true] {
            let s = session(caller.clone(), non_interactive);
            for name in ["test.destructive", "rec.destructive", "rec.exec"] {
                let out = fx.call(&s, name, json!({}), Some("k1")).await;
                let e = error(&out);
                assert_eq!(e.code, "confirm.unavailable", "{caller} {name}");
                assert_eq!(e.category, ErrorCategory::UserFixable);
                assert_eq!(e.retry, RetryPolicy::AfterResolve);
            }
        }
    }
    assert_eq!(destructive.executions(), 0);
    assert_eq!(exec.executions(), 0);
    assert!(!log.take().contains(&"execute"));
    // The refusals released their keys.
    assert!(fx.rows().is_empty(), "{:?}", fx.rows());
}

#[tokio::test] // spec: CORE-109, ARCH-052
async fn ui_destructive_proceeds() {
    let log = Log::default();
    let destructive = RecTool::new("rec.destructive", EffectClass::Destructive, &log);
    let fx = fixture(&[as_dyn(&destructive)]);
    let ui = session(CallerKind::Ui("image".into()), false);
    let out = fx.call(&ui, "rec.destructive", json!({}), Some("k1")).await;
    ok(&out);
    assert_eq!(destructive.executions(), 1);
    let out = fx
        .call(&ui, "test.destructive", json!({}), Some("k2"))
        .await;
    assert_eq!(ok(&out).structured, json!({"count": 1}));
}

/// `test.destructive {cost?}` counts its runs in this core; `cost` is a decimal string, and a
/// refused call doesn't count.
#[tokio::test] // spec: CORE-109, QA-066
async fn test_destructive_counts_runs_and_takes_a_cost() {
    let fx = fixture(&[]);
    let ui = session(CallerKind::Ui("image".into()), false);
    let out = fx
        .call(&ui, "test.destructive", json!({}), Some("k1"))
        .await;
    assert_eq!(ok(&out).structured, json!({"count": 1}));
    let out = fx
        .call(&ui, "test.destructive", json!({"cost": "2"}), Some("k2"))
        .await;
    assert_eq!(ok(&out).structured, json!({"count": 2}));
    for (key, bad) in [("k3", json!({"cost": 2})), ("k4", json!({"other": 1}))] {
        let out = fx.call(&ui, "test.destructive", bad, Some(key)).await;
        assert_eq!(error(&out).code, "params.invalid", "{key}");
    }
    let out = fx
        .call(&ui, "test.destructive", json!({}), Some("k5"))
        .await;
    assert_eq!(
        ok(&out).structured,
        json!({"count": 3}),
        "the refusals did not count"
    );
}

#[tokio::test] // spec: QA-012, ARCH-060
async fn tool_panic_is_core_bug() {
    let fx = fixture(&[]);
    let out = fx.call(&cli(), "test.panic", json!({}), None).await;
    let e = error(&out);
    assert_eq!(e.code, "core.bug");
    assert_eq!(e.category, ErrorCategory::Bug);
    assert_eq!(e.retry, RetryPolicy::Never);
    let detail = e.detail.as_ref().expect("the panic text is the detail");
    assert!(detail.untrusted);
    assert!(detail.text.contains("test.panic was called"), "{detail:?}");
    assert_eq!(fx.log.take().last(), Some(&"record"));
    // The dispatcher goes on.
    let out = fx
        .call(&cli(), "test.echo", json!({"text": "still here"}), None)
        .await;
    assert_eq!(ok(&out).structured, json!({"text": "still here"}));
}

#[tokio::test] // spec: QA-010
async fn tool_failures_are_results_not_rpc_errors() {
    let fx = fixture(&[]);
    let unknown = fx.call(&cli(), "no.such", json!({}), None).await;
    let invalid = fx
        .call(&cli(), "test.echo", json!({"text": "x", "bogus": 1}), None)
        .await;
    assert_eq!(error(&unknown).code, "tool.unknown");
    assert_eq!(error(&invalid).code, "params.invalid");
    for outcome in [&unknown, &invalid] {
        let wire = serde_json::to_value(outcome).unwrap();
        assert_eq!(wire["status"], "error", "{wire}");
        assert!(wire["error"]["trace_id"].is_string(), "{wire}");
    }
}

#[tokio::test] // spec: ARCH-003, ARCH-031
async fn tools_list_is_the_registry_filtered_by_visibility() {
    let log = Log::default();
    let mut only = RecTool::new("rec.assistant_only", EffectClass::Read, &log);
    Arc::get_mut(&mut only).unwrap().only_for = Some(CallerKind::Assistant);
    let fx = fixture(&[only]);
    let names = |caller: &CallerKind| -> Vec<String> {
        fx.dispatcher
            .registry()
            .list_for(caller)
            .into_iter()
            .map(|d| d.name)
            .collect()
    };
    assert!(names(&CallerKind::Assistant).contains(&"rec.assistant_only".to_string()));
    assert!(!names(&CallerKind::Cli).contains(&"rec.assistant_only".to_string()));
    assert!(names(&CallerKind::Cli).contains(&"test.echo".to_string()));
    let out = fx.call(&cli(), "rec.assistant_only", json!({}), None).await;
    assert_eq!(error(&out).code, "tool.unknown");
    let assistant = session(CallerKind::Assistant, false);
    ok(&fx
        .call(&assistant, "rec.assistant_only", json!({}), None)
        .await);
}

fn tool_with(name: &str, description: String, module: &str) -> Arc<dyn Tool> {
    let log = Log::default();
    let mut tool = RecTool::new(name, EffectClass::Read, &log);
    let t = Arc::get_mut(&mut tool).unwrap();
    t.descriptor.description = description;
    t.descriptor.module = module.into();
    tool
}

#[test] // spec: ARCH-037
fn descriptors_use_json_schema_2020_12_and_fit_2000_chars() {
    let env = TestEnv::new();
    let states = Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap());
    let reg = registry_for(&env, true, &Arc::new(TestGates::new()), &states);
    let tools = reg.list_for(&CallerKind::Ui("image".into()));
    assert!(!tools.is_empty());
    for d in &tools {
        for schema in [&d.input_schema, &d.output_schema] {
            assert_eq!(
                schema["$schema"], "https://json-schema.org/draft/2020-12/schema",
                "{}: {schema}",
                d.name
            );
        }
        assert!(d.description.chars().count() <= 2000, "{}", d.name);
    }

    // Exactly 2000 characters is fine; 2001 is refused at registration.
    let mut reg = Registry::new();
    reg.register(tool_with("t.max", "é".repeat(2000), "test"));
    let refused = std::panic::catch_unwind(AssertUnwindSafe(|| {
        reg.register(tool_with("t.long", "é".repeat(2001), "test"));
    }));
    assert!(refused.is_err());
    let duplicate = std::panic::catch_unwind(AssertUnwindSafe(|| {
        reg.register(tool_with("t.max", "again".into(), "test"));
    }));
    assert!(duplicate.is_err());
}

#[test] // spec: CORE-058
fn every_tool_declares_a_module() {
    let env = TestEnv::new();
    let states = Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap());
    let reg = registry_for(&env, true, &Arc::new(TestGates::new()), &states);
    let tools = reg.list_for(&CallerKind::Cli);
    assert!(tools.iter().any(|d| d.module == WORKSPACE_MODULE));
    for d in tools {
        let want = if d.name.starts_with("test.") {
            TEST_MODULE
        } else if d.name.starts_with("job.") || d.name.starts_with("jobs.") {
            crate::jobs::JOBS_MODULE
        } else {
            WORKSPACE_MODULE
        };
        assert_eq!(d.module, want, "{}", d.name);
    }
    let refused = std::panic::catch_unwind(AssertUnwindSafe(|| {
        Registry::new().register(tool_with("t.nomodule", "x".into(), ""));
    }));
    assert!(refused.is_err());
}

#[test] // spec: CORE-014
fn registry_and_dispatcher_live_in_the_tools_module() {
    let _: fn() -> crate::tools::Registry = crate::tools::Registry::new;
    type NewDispatcher = fn(
        crate::tools::Registry,
        PipelineHooks,
        Arc<StateRegistry>,
        Arc<EventBus>,
        Arc<WorkspaceHub>,
        Arc<dyn IdSource>,
        Arc<dyn Clock>,
        Arc<Lifecycle>,
    ) -> crate::tools::Dispatcher;
    let _: NewDispatcher = crate::tools::Dispatcher::new;
    assert!(std::any::type_name::<Registry>().starts_with("lil_core::tools::"));
    assert!(std::any::type_name::<Dispatcher>().starts_with("lil_core::tools::"));
}

#[tokio::test] // spec: QA-040, CORE-014
async fn test_tools_exist_only_on_test_channels() {
    let env = TestEnv::new();
    let cfg = CoreConfig::for_env(env.env.clone()).unwrap();
    assert!(cfg.test_tools, "a test-* channel registers the test tools");
    let states = Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap());
    let gates = Arc::new(TestGates::new());

    let names: Vec<String> = registry_for(&env, true, &gates, &states)
        .list_for(&CallerKind::Cli)
        .into_iter()
        .map(|d| d.name)
        .filter(|name| name.starts_with("test."))
        .collect();
    assert_eq!(
        names,
        [
            "test.block",
            "test.destructive",
            "test.echo",
            "test.emit",
            "test.ingest_run",
            "test.job",
            "test.job_release",
            "test.panic",
            "test.settings",
            "test.state_panic",
            "test.store_write",
            "test.trash",
            "test.watch_fence",
            "test.write"
        ]
    );

    // Off a test channel no `test.*` tool is listed (the product tools are, on every
    // channel), and calling one is `tool.unknown`.
    let reg = registry_for(&env, false, &gates, &states);
    let listed = reg.list_for(&CallerKind::Cli);
    assert!(!listed.is_empty(), "the product tools are registered");
    assert!(
        listed.iter().all(|d| !d.name.starts_with("test.")),
        "{listed:?}"
    );
    let events = Arc::new(EventBus::new(FakeClock::new()));
    let (clock, ids): (Arc<dyn Clock>, Arc<dyn IdSource>) =
        (FakeClock::new(), Arc::new(SeqIds::new()));
    let hub = hub_for(&env, &states, &events, &clock, &ids);
    let dispatcher = Dispatcher::new(
        reg,
        PipelineHooks::plan2(),
        states,
        events,
        hub,
        ids,
        clock,
        Lifecycle::new(FakeClock::new(), IDLE_EXIT_AFTER),
    );
    let out = dispatcher
        .call(cli(), params("test.echo", json!({"text": "x"}), None))
        .await;
    assert_eq!(error(&out).code, "tool.unknown");
}

#[tokio::test] // spec: CORE-022, ARCH-038
async fn tool_results_are_json_text_with_file_paths() {
    let log = Log::default();
    let tool = RecTool::new("rec.read", EffectClass::Read, &log);
    let fx = fixture(&[tool]);
    let out = fx.call(&cli(), "rec.read", json!({}), None).await;
    let wire = serde_json::to_value(&out).unwrap();
    assert_eq!(wire["status"], "ok");
    assert!(wire["structured"].is_object(), "{wire}");
    assert_eq!(wire["artifacts"][0]["path"], r"out\Ünïcödé space.png");
    assert!(wire["artifacts"][0]["path"].is_string());
    assert!(wire["summary_text"].is_string());
    // The whole result is JSON text that reads back as the same outcome.
    let text = serde_json::to_string(&out).unwrap();
    let back: Outcome<ToolOutput> = serde_json::from_str(&text).unwrap();
    assert_eq!(back, out);
}

impl Fixture {
    /// Waits (a failure deadline, not a measurement) until the suite `state.db` rows are
    /// `want`.
    async fn wait_for_rows(&self, want: &[(&str, &str, &str)]) {
        let want: Vec<(String, String, String)> = want
            .iter()
            .map(|(a, b, c)| ((*a).to_string(), (*b).to_string(), (*c).to_string()))
            .collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while self.rows() != want {
            assert!(
                std::time::Instant::now() < deadline,
                "rows {:?}, want {want:?}",
                self.rows()
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test] // spec: CORE-016
async fn a_cancelled_caller_still_finishes_the_row() {
    let fx = fixture(&[]);
    let args = json!({"gate": "g"});
    // The caller gives up while the call is blocked; its future is dropped.
    let caller = cli();
    tokio::select! {
        out = fx.call(&caller, "test.block", args.clone(), Some("k")) => {
            panic!("test.block returned before its gate opened: {out:?}")
        }
        () = fx.wait_for_rows(&[("cli|none", "k", "running")]) => {}
    }
    let busy = fx.call(&cli(), "test.block", args.clone(), Some("k")).await;
    assert_eq!(error(&busy).code, "idempotency.in_progress");
    assert_eq!(error(&busy).retry, RetryPolicy::SameIdempotencyKey);

    // The abandoned call runs to the end and stores its result for the retry.
    fx.gates.open("g");
    fx.wait_for_rows(&[("cli|none", "k", "done")]).await;
    let retry = fx.call(&cli(), "test.block", args, Some("k")).await;
    assert_eq!(ok(&retry).structured, json!({"gate": "g", "executions": 1}));
}

/// A reserve stage with a bug.
struct PanickingReserve;

impl ReserveStage for PanickingReserve {
    fn reserve<'a>(
        &'a self,
        _i: ReserveInput<'a>,
    ) -> BoxFuture<'a, Result<ReserveOutcome, lil_protocol::ErrorBody>> {
        panic!("the reserve stage has a bug")
    }
}

#[tokio::test] // spec: QA-012, CORE-016
async fn a_panicking_stage_releases_the_key() {
    let log = Log::default();
    let tool = RecTool::new("rec.write", EffectClass::Write, &log);
    let hooks = PipelineHooks {
        reserve: Arc::new(PanickingReserve),
        ..PipelineHooks::plan2()
    };
    let fx = fixture_with(hooks, &log, &[as_dyn(&tool)]);
    let out = fx.call(&cli(), "rec.write", json!({}), Some("k")).await;
    let e = error(&out);
    assert_eq!(e.code, "core.bug");
    assert!(
        e.detail
            .as_ref()
            .is_some_and(|d| d.untrusted && d.text.contains("reserve stage has a bug")),
        "{e:?}"
    );
    assert_eq!(tool.executions(), 0);
    // The claim releases the row as it is dropped, in a task that stays in flight until the
    // release is done: a drain waits for it, and the row is gone once the drain ends.
    let lifecycle = fx.dispatcher.lifecycle();
    lifecycle.begin_drain(DrainReason::Requested);
    let report = lifecycle.run_shutdown().await;
    assert_eq!(report.abandoned_calls, 0);
    assert_eq!(fx.rows(), Vec::new());
}

#[derive(JsonSchema)]
#[expect(dead_code, reason = "only its schema is used")]
struct LooseArgs {
    n: Option<u64>,
}

#[test] // spec: CORE-058, ARCH-037
fn registry_refuses_schemas_that_allow_unknown_properties() {
    let log = Log::default();
    // An object schema without `"additionalProperties": false` is a programming error.
    let mut loose = RecTool::new("t.loose", EffectClass::Read, &log);
    Arc::get_mut(&mut loose).unwrap().descriptor.input_schema = schema_2020_12::<LooseArgs>();
    assert_ne!(
        loose.descriptor.input_schema.get("additionalProperties"),
        Some(&json!(false))
    );
    let refused = std::panic::catch_unwind(AssertUnwindSafe(|| {
        Registry::new().register(loose);
    }));
    let text = refused
        .expect_err("a schema allowing unknown properties was registered")
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(text.contains("unknown properties"), "{text}");

    // `deny_unknown_fields` gives it, so `RecTool` (and every core tool) registers.
    let strict = RecTool::new("t.strict", EffectClass::Read, &log);
    assert_eq!(
        strict.descriptor.input_schema["additionalProperties"],
        json!(false)
    );
    let mut reg = Registry::new();
    reg.register(strict);
    // A schema that isn't an object schema is not this rule's business.
    let mut any = RecTool::new("t.any", EffectClass::Read, &log);
    Arc::get_mut(&mut any).unwrap().descriptor.input_schema = schema_2020_12::<Value>();
    reg.register(any);
    let env = TestEnv::new();
    let states = Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap());
    for d in
        registry_for(&env, true, &Arc::new(TestGates::new()), &states).list_for(&CallerKind::Cli)
    {
        assert_eq!(
            d.input_schema["additionalProperties"],
            json!(false),
            "{}",
            d.name
        );
    }
}

/// A write tool that closes the suite `state.db` while it runs, so its idempotency row can't
/// be finished; with `fail`, it then fails.
struct ClosesState {
    descriptor: ToolDescriptor,
    states: Mutex<Option<Arc<StateRegistry>>>,
    fail: bool,
}

impl Tool for ClosesState {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn validate(&self, _args: &Value) -> Result<(), String> {
        Ok(())
    }

    fn run<'a>(&'a self, ctx: &'a CallCtx, _args: Value) -> ToolFuture<'a> {
        Box::pin(async move {
            let states = self.states.lock().unwrap().take().expect("the states");
            tokio::task::spawn_blocking(move || states.suite().close())
                .await
                .unwrap();
            if self.fail {
                return Err(lil_protocol::ErrorBody::new(
                    &lil_protocol::codes::PARAMS_INVALID,
                    "failed on purpose",
                    ctx.trace_id.clone(),
                ));
            }
            Ok(ToolOutput {
                structured: json!({"ran": true}),
                artifacts: Vec::new(),
                summary_text: "ran".into(),
            })
        })
    }
}

#[tokio::test] // spec: CORE-016, QA-012
async fn idem_finish_failure_says_what_happens() {
    for fail in [false, true] {
        let name = if fail {
            "rec.closes_and_fails"
        } else {
            "rec.closes"
        };
        let tool = Arc::new(ClosesState {
            descriptor: ToolDescriptor {
                name: name.into(),
                description: "Closes the suite state.db.".into(),
                effect: EffectClass::Write,
                module: "test".into(),
                input_schema: schema_2020_12::<RecArgs>(),
                output_schema: schema_2020_12::<RecResult>(),
                annotations: BTreeMap::new(),
            },
            states: Mutex::new(None),
            fail,
        });
        let as_tool: Arc<dyn Tool> = tool.clone();
        let fx = fixture(std::slice::from_ref(&as_tool));
        *tool.states.lock().unwrap() = Some(fx.states.clone());
        let out = fx.call(&cli(), name, json!({}), Some("k")).await;
        let e = error(&out);
        assert_eq!(e.code, "state.unavailable", "{name}: {e:?}");
        if fail {
            // The tool failed, so nothing was to be stored: the plain message.
            assert!(e.data.is_none(), "{name}: {e:?}");
        } else {
            assert_eq!(
                e.message,
                "The call ran, but the core could not save its result. Until the core \
                 restarts, a retry with the same idempotency key reports it as still running; \
                 check what it did before you retry with a new key."
            );
            assert_eq!(e.data, Some(json!({"ran": true})), "{e:?}");
            assert_eq!(e.retry, RetryPolicy::SameIdempotencyKey);
        }
        // The row stays `running`: the tool ran and must not run again for this key.
        assert_eq!(
            fx.rows(),
            [(
                "cli|none".to_string(),
                "k".to_string(),
                "running".to_string()
            )],
            "{name}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// `job.retry` dispatches the call that made the job again (ARCH-052, CORE-146).
// ---------------------------------------------------------------------------------------------

/// A tool of effect `effect` that submits a `test.job` job with its arguments (`test.job`'s:
/// `{lane: "cpu", fail?}`), in the caller's class.
struct JobTool {
    descriptor: ToolDescriptor,
}

fn job_tool(name: &str, effect: EffectClass) -> Arc<dyn Tool> {
    Arc::new(JobTool {
        descriptor: ToolDescriptor {
            name: name.to_string(),
            description: "Submits a test.job job.".into(),
            effect,
            module: "test".into(),
            input_schema: schema_2020_12::<RecArgs>(),
            output_schema: schema_2020_12::<RecResult>(),
            annotations: BTreeMap::new(),
        },
    })
}

impl Tool for JobTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn validate(&self, _args: &Value) -> Result<(), String> {
        Ok(())
    }

    fn run<'a>(&'a self, ctx: &'a CallCtx, args: Value) -> ToolFuture<'a> {
        Box::pin(async move {
            let spec = crate::jobs::JobSpec {
                kind: TEST_JOB_KIND,
                lane: crate::jobs::Lane::Cpu,
                priority: crate::jobs::Priority::for_caller(&ctx.session.caller, false),
                module: "test".into(),
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
            };
            let job = ctx.jobs.submit(&ctx.job_caller(), spec).await?;
            Ok(ToolOutput {
                structured: json!({ "job": job }),
                artifacts: Vec::new(),
                summary_text: String::new(),
            })
        })
    }
}

impl Fixture {
    /// Runs `test.job` jobs (the core registers the handler on test channels).
    fn with_job_handler(self) -> Fixture {
        self.dispatcher
            .jobs()
            .register(Arc::new(TestJobHandler::new(self.gates.clone())));
        self
    }

    /// Waits (failure deadline 30 s) until job `id` ends: its summary.
    async fn job_done(&self, id: &str) -> Value {
        let out = self
            .call(
                &cli(),
                "job.wait",
                json!({"job_id": id, "timeout_s": 30}),
                None,
            )
            .await;
        let waited = ok(&out).structured.clone();
        assert_eq!(waited["done"], true, "{waited}");
        waited["job"].clone()
    }

    async fn job_count(&self) -> usize {
        let out = self.call(&cli(), "jobs.list", json!({}), None).await;
        ok(&out).structured["jobs"].as_array().unwrap().len()
    }
}

/// C1: a retry is never a way around a confirmation. An agent retrying the job of a
/// `destructive` or `exec` tool gets `confirm.unavailable` and nothing runs; your retry from the
/// app runs the tool again.
#[tokio::test] // spec: ARCH-052, CORE-116, QA-016, CORE-146
async fn an_agent_retry_of_a_destructive_or_exec_job_is_confirm_unavailable() {
    let fx = fixture(&[
        job_tool("x.exec_job", EffectClass::Exec),
        job_tool("x.destructive_job", EffectClass::Destructive),
    ])
    .with_job_handler();
    let ui = session(CallerKind::Ui("image".into()), false);
    for (n, name) in ["x.exec_job", "x.destructive_job"].into_iter().enumerate() {
        let made = fx
            .call(
                &ui,
                name,
                json!({"lane": "cpu", "fail": "final"}),
                Some(&format!("make-{n}")),
            )
            .await;
        let id = ok(&made).structured["job"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(fx.job_done(&id).await["status"], "failed");
        let jobs = fx.job_count().await;
        for caller in [CallerKind::Cli, CallerKind::Mcp, CallerKind::Assistant] {
            let out = fx
                .call(
                    &session(caller.clone(), false),
                    "job.retry",
                    json!({"job_id": id}),
                    Some(&format!("retry-{n}-{caller}")),
                )
                .await;
            assert_eq!(error(&out).code, "confirm.unavailable", "{caller} {name}");
        }
        assert_eq!(fx.job_count().await, jobs, "no job was submitted");
        // From the app it runs again, as a new job with `retry_of`.
        let out = fx
            .call(
                &ui,
                "job.retry",
                json!({"job_id": id}),
                Some(&format!("yours-{n}")),
            )
            .await;
        let retried = ok(&out).structured.clone();
        assert_eq!(retried["retry_of"], id.as_str(), "{retried}");
        assert_eq!(retried["job"]["retry_of"], id.as_str(), "{retried}");
        let again = retried["job"]["job_id"].as_str().unwrap();
        assert_eq!(fx.job_done(again).await["status"], "failed");
    }
}

/// Estimates `x.paid_job` as paid and reserves for it, logging each stage with the call id.
struct Paid {
    log: Arc<Mutex<Vec<String>>>,
    next: AtomicU64,
}

impl EstimateStage for Paid {
    fn estimate(&self, d: &ToolDescriptor, _args: &Value, _s: &Settings) -> Estimate {
        if d.name != "x.paid_job" {
            return Estimate::Zero;
        }
        self.log
            .lock()
            .unwrap()
            .push(format!("estimate {}", d.name));
        Estimate::External {
            provider: "test".into(),
            amount: lil_schema::Money::from_micros(1_000_000),
            currency: lil_schema::Currency::parse("USD").unwrap(),
            basis: BillingBasis::Payg,
        }
    }
}

impl ReserveStage for Paid {
    fn reserve<'a>(
        &'a self,
        i: ReserveInput<'a>,
    ) -> BoxFuture<'a, Result<ReserveOutcome, lil_protocol::ErrorBody>> {
        let out = if *i.estimate == Estimate::Zero {
            ReserveOutcome::None
        } else {
            let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            let rv = format!("rv_{n:016x}");
            self.log
                .lock()
                .unwrap()
                .push(format!("reserve {rv} {}", i.call_id));
            ReserveOutcome::Reserved(rv)
        };
        Box::pin(std::future::ready(Ok(out)))
    }
}

impl crate::jobs::Reservations for Paid {
    fn hand_over<'a>(
        &'a self,
        reservation: &'a str,
        from: &'a str,
        to: &'a crate::jobs::JobId,
    ) -> crate::jobs::JobFuture<'a, ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("hand_over {reservation} {from} {to}"));
        Box::pin(std::future::ready(()))
    }

    fn release<'a>(
        &'a self,
        reservation: &'a str,
        owner: &'a str,
    ) -> crate::jobs::JobFuture<'a, ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("release {reservation} {owner}"));
        Box::pin(std::future::ready(()))
    }
}

/// C1, I3, ruling 3: a retry of a paid job runs the estimate and reserve stages again under a
/// call id of its own; the new job takes the new reservation over; and an agent's retry of your
/// job runs in the `agent` class.
#[tokio::test] // spec: CORE-015, CORE-088, CORE-124, CORE-146
async fn a_retry_estimates_and_reserves_again() {
    let paid = Arc::new(Paid {
        log: Arc::default(),
        next: AtomicU64::new(0),
    });
    let hooks = PipelineHooks {
        estimate: paid.clone(),
        reserve: paid.clone(),
        ..PipelineHooks::plan2()
    };
    let fx = fixture_with(
        hooks,
        &Log::default(),
        &[job_tool("x.paid_job", EffectClass::Write)],
    )
    .with_job_handler();
    fx.dispatcher.jobs().set_reservations(paid.clone());
    let ui = session(CallerKind::Ui("image".into()), false);
    let made = fx
        .call(
            &ui,
            "x.paid_job",
            json!({"lane": "cpu", "fail": "final"}),
            Some("make"),
        )
        .await;
    let first = ok(&made).structured["job"].clone();
    let id = first["job_id"].as_str().unwrap().to_string();
    assert_eq!(first["priority"], "user");
    fx.job_done(&id).await;
    let out = fx
        .call(&cli(), "job.retry", json!({"job_id": id}), Some("again"))
        .await;
    let job = ok(&out).structured["job"].clone();
    let second = job["job_id"].as_str().unwrap().to_string();
    assert_eq!(
        job["priority"], "agent",
        "the retrying caller's class: {job}"
    );
    fx.job_done(&second).await;
    let log = paid.log.lock().unwrap().clone();
    assert_eq!(log.len(), 6, "{log:?}");
    assert_eq!(log[0], "estimate x.paid_job");
    assert_eq!(log[3], "estimate x.paid_job", "{log:?}");
    let (rv1, rv2) = ("rv_0000000000000001", "rv_0000000000000002");
    for (at, rv, job) in [(1, rv1, &id), (4, rv2, &second)] {
        let call = log[at]
            .strip_prefix(&format!("reserve {rv} "))
            .unwrap_or_else(|| panic!("{log:?}"));
        assert!(call.starts_with("c_"), "{log:?}");
        assert_eq!(log[at + 1], format!("hand_over {rv} {call} {job}"));
    }
    assert_ne!(log[1], log[4], "a call id of its own");
}
