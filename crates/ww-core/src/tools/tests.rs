//! Unit tests of the `tools` module: the dispatcher with recording stages, fake tools,
//! `SeqIds` and a `FakeClock` (QA-031), over a real suite `state.db` in a `TestEnv`.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags};
use schemars::JsonSchema;
use serde_json::{Value, json};
use ww_platform::{Clock, IdSource};
use ww_protocol::{
    CallerKind, EffectClass, ErrorCategory, FileRef, Outcome, RetryPolicy, ToolCallParams,
    ToolDescriptor, ToolOutput,
};
use ww_testkit::{FakeClock, SeqIds, TestEnv};

use super::registry::schema_2020_12;
use super::*;
use crate::config::CoreConfig;
use crate::events::EventBus;
use crate::lifecycle::{DrainReason, IDLE_EXIT_AFTER, Lifecycle};
use crate::session::Session;
use crate::state::{OnFatal, StateRegistry};

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
    fn resolve(&self, s: &Session) -> Settings {
        self.log.push("settings");
        self.plan2.settings.resolve(s)
    }
}

impl EstimateStage for Recording {
    fn estimate(&self, d: &ToolDescriptor) -> Estimate {
        self.log.push("estimate");
        self.plan2.estimate.estimate(d)
    }
}

impl ConfirmStage for Recording {
    fn check(&self, s: &Session, d: &ToolDescriptor, e: &Estimate) -> ConfirmDecision {
        self.log.push("confirm");
        self.plan2.confirm.check(s, d, e)
    }
}

impl ReserveStage for Recording {
    fn reserve(&self, s: &Session, e: &Estimate) -> Result<(), ww_protocol::ErrorBody> {
        self.log.push("reserve");
        self.plan2.reserve.reserve(s, e)
    }
}

impl MeterStage for Recording {
    fn settle(&self, s: &Session, out: &Outcome<ToolOutput>) {
        self.log.push("meter");
        self.plan2.meter.settle(s, out);
    }
}

impl RecordStage for Recording {
    fn record(&self, s: &Session, d: &ToolDescriptor, out: &mut Outcome<ToolOutput>) {
        self.log.push("record");
        self.plan2.record.record(s, d, out);
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
    env: TestEnv,
    log: Log,
    gates: Arc<TestGates>,
    dispatcher: Dispatcher,
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
    let mut reg = core_registry(true, &gates, &states);
    for tool in extra {
        reg.register(tool.clone());
    }
    let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
    let clock: Arc<dyn Clock> = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let lifecycle = Lifecycle::new(clock.clone(), IDLE_EXIT_AFTER);
    let dispatcher = Dispatcher::new(reg, hooks, states, events, ids, clock, lifecycle);
    Fixture {
        env,
        log,
        gates,
        dispatcher,
    }
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
fn error(outcome: &Outcome<ToolOutput>) -> &ww_protocol::ErrorBody {
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
    assert_eq!(ok(&out).structured, json!({"executed": true}));
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
    let reg = core_registry(true, &Arc::new(TestGates::new()), &states);
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
    let reg = core_registry(true, &Arc::new(TestGates::new()), &states);
    for d in reg.list_for(&CallerKind::Cli) {
        assert_eq!(d.module, TEST_MODULE, "{}", d.name);
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
        Arc<dyn IdSource>,
        Arc<dyn Clock>,
        Arc<Lifecycle>,
    ) -> crate::tools::Dispatcher;
    let _: NewDispatcher = crate::tools::Dispatcher::new;
    assert!(std::any::type_name::<Registry>().starts_with("ww_core::tools::"));
    assert!(std::any::type_name::<Dispatcher>().starts_with("ww_core::tools::"));
}

#[tokio::test] // spec: QA-040, CORE-014
async fn test_tools_exist_only_on_test_channels() {
    let env = TestEnv::new();
    let cfg = CoreConfig::for_env(env.env.clone()).unwrap();
    assert!(cfg.test_tools, "a test-* channel registers the test tools");
    let states = Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap());
    let gates = Arc::new(TestGates::new());

    let names: Vec<String> = core_registry(true, &gates, &states)
        .list_for(&CallerKind::Cli)
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(
        names,
        [
            "test.block",
            "test.destructive",
            "test.echo",
            "test.emit",
            "test.panic",
            "test.state_panic",
            "test.write"
        ]
    );

    // Off a test channel there are none, and calling one is `tool.unknown`.
    let reg = core_registry(false, &gates, &states);
    assert!(reg.list_for(&CallerKind::Cli).is_empty());
    let dispatcher = Dispatcher::new(
        reg,
        PipelineHooks::plan2(),
        states,
        Arc::new(EventBus::new(FakeClock::new())),
        Arc::new(SeqIds::new()),
        FakeClock::new(),
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
    fn reserve(&self, _s: &Session, _e: &Estimate) -> Result<(), ww_protocol::ErrorBody> {
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
