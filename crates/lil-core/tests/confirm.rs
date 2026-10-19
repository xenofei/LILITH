//! Agent safety confirmations (ARCH-020, ARCH-052–055, CORE-109–115, APP-044–046, APP-051,
//! QA-016; Appendix F "Confirmations", "`app/*` methods"): who is asked, who can answer, what a
//! retry with the token gets, and the `tools/list` marker of `mcp_confirmation`.
//!
//! Most tests build the dispatcher and [`Confirm`] directly with the fake clock and ids
//! (QA-031): an app "connects" by [`UiPresence::opened`]. The router's part (only a verified
//! `ui:*` connection answers, the capability gate, events, a restart) runs over the pipe of an
//! in-process core with the `ui:test` app. Wall-clock durations are failure deadlines only.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lil_core::confirm::{
    AgentConfirm, Confirm, ConfirmRequest, ListState, McpConfirmation, NoNotificationPath,
    UiPresence,
};
use lil_core::identity::TrustedDirs;
use lil_core::lifecycle::{DrainReason, Lifecycle};
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::tools::{Dispatcher, PipelineHooks, TestGates, Tool, core_registry, typed_tool};
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_platform::{Clock, IdSource};
use lil_protocol::app::{Answer, ConfirmationAnswerParams};
use lil_protocol::{CallerKind, Cover, EffectClass, Outcome, ToolCallParams, ToolOutput};
use lil_testkit::{FakeClock, RawPipe, SeqIds, TestApp, TestEnv, ensure_built};
use serde_json::{Value, json};

/// Failure deadline for anything the test waits on.
const DEADLINE: Duration = Duration::from_secs(30);
/// The lifetime of a confirmation (Appendix F).
const LIFETIME: Duration = Duration::from_secs(30 * 60);

// ---------------------------------------------------------------------------------------------
// The dispatcher and `Confirm`, built directly.
// ---------------------------------------------------------------------------------------------

struct Fx {
    confirm: Arc<Confirm>,
    clock: Arc<FakeClock>,
    dispatcher: Dispatcher,
    /// Last: the dispatcher holds the suite `state.db`, which must close before the folder goes.
    env: TestEnv,
}

/// An `exec` tool that counts its runs.
fn exec_tool() -> Arc<dyn Tool> {
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct NoArgs {}
    #[derive(serde::Serialize, schemars::JsonSchema)]
    struct Ran {
        runs: u64,
    }
    let runs = Arc::new(AtomicU64::new(0));
    typed_tool(
        "x.exec",
        "An exec test tool that counts its runs.",
        EffectClass::Exec,
        "test",
        move |_ctx, _args: NoArgs| {
            let runs = runs.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move { Ok((Ran { runs }, format!("run {runs}"))) })
        },
    )
}

fn fixture(mcp: McpConfirmation) -> Fx {
    let env = TestEnv::new();
    let states = Arc::new(
        StateRegistry::open(
            &env.env,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("the suite state.db"),
    );
    let clock = FakeClock::new();
    let dyn_clock: Arc<dyn Clock> = clock.clone();
    let events = Arc::new(lil_core::events::EventBus::new(dyn_clock.clone()));
    let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
    let probe: Arc<dyn lil_platform::fs::Probe> = Arc::new(lil_platform::fs::SystemProbe);
    let hub = lil_core::workspace::WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        events.clone(),
        dyn_clock.clone(),
        ids.clone(),
        lil_core::store::FailPoints::none(),
        probe.clone(),
        lil_core::watch::WatchConfig::default(),
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
    reg.register(exec_tool());
    let confirm = Confirm::new(
        states.clone(),
        events.clone(),
        dyn_clock.clone(),
        ids.clone(),
        UiPresence::new(),
        Arc::new(NoNotificationPath),
        LIFETIME,
        mcp,
        lil_core::store::FailPoints::none(),
    );
    let hooks = PipelineHooks {
        confirm: AgentConfirm::new(confirm.clone()),
        ..PipelineHooks::plan2()
    };
    let lifecycle = Lifecycle::new(dyn_clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
    let dispatcher = Dispatcher::new(
        reg,
        hooks,
        states.clone(),
        events,
        hub,
        ids,
        dyn_clock,
        lifecycle,
    );
    Fx {
        confirm,
        clock,
        dispatcher,
        env,
    }
}

const ALL_CAPABILITIES: &[&str] = lil_protocol::CAPABILITIES;

fn session(caller: CallerKind, id: &str, non_interactive: bool) -> Arc<Session> {
    Arc::new(Session {
        id: id.into(),
        caller,
        non_interactive,
        workspace: None,
        capabilities: ALL_CAPABILITIES.iter().map(|c| (*c).to_string()).collect(),
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

impl Fx {
    async fn call(
        &self,
        s: &Arc<Session>,
        name: &str,
        args: Value,
        key: &str,
    ) -> Outcome<ToolOutput> {
        self.dispatcher
            .call(s.clone(), params(name, args, Some(key)))
            .await
    }

    async fn answer(&self, id: &str, answer: Answer) -> lil_core::confirm::ConfirmationRow {
        self.confirm
            .answer(
                &app(),
                ConfirmationAnswerParams {
                    confirmation_id: id.into(),
                    answer,
                    cap: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("the answer was refused: {e:?}"))
    }

    /// How often `test.destructive` has run, by running it once more as you (your own
    /// destructive call runs at once): the count before that run.
    async fn destructive_runs(&self) -> u64 {
        let out = self
            .call(&app(), "test.destructive", json!({}), &next_key())
            .await;
        ok(&out)["count"].as_u64().unwrap() - 1
    }

    async fn history(&self) -> Vec<lil_core::confirm::ConfirmationRow> {
        self.confirm.list(ListState::History, 500).await.unwrap()
    }

    async fn pending(&self) -> Vec<lil_core::confirm::ConfirmationRow> {
        self.confirm.list(ListState::Pending, 500).await.unwrap()
    }
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

#[tokio::test] // spec: ARCH-052, ARCH-055, APP-051, QA-016, CORE-017
async fn agent_destructive_or_exec_is_pending_when_an_app_is_connected_else_unavailable() {
    let fx = fixture(McpConfirmation::Core);
    let agents = [CallerKind::Cli, CallerKind::Mcp, CallerKind::Assistant];
    for tool in ["test.destructive", "x.exec"] {
        for caller in &agents {
            let s = session(caller.clone(), "s_agent", false);
            let out = fx.call(&s, tool, json!({}), &next_key()).await;
            assert_eq!(code(&out), "confirm.unavailable", "{caller} {tool}");
        }
    }
    assert!(fx.history().await.is_empty(), "nothing was asked");

    fx.confirm.presence().opened();
    for tool in ["test.destructive", "x.exec"] {
        for caller in &agents {
            let s = session(caller.clone(), "s_agent", false);
            let key = next_key();
            let p = pending(&fx.call(&s, tool, json!({}), &key).await);
            assert!(p.confirmation_id.starts_with("cf_"), "{p:?}");
            assert_eq!(p.retry_token, key);
            assert_eq!(p.covers, [Cover::Confirm]);
            assert!(!p.expires_at.is_empty());
        }
    }
    assert_eq!(fx.pending().await.len(), 6);
    assert_eq!(fx.destructive_runs().await, 0, "nothing ran");
    // The app closing makes them unanswerable again.
    fx.confirm.presence().closed();
    let out = fx
        .call(&cli(), "test.destructive", json!({}), &next_key())
        .await;
    assert_eq!(code(&out), "confirm.unavailable");
}

#[tokio::test] // spec: CORE-109, ARCH-052
async fn own_actions_never_ask() {
    let fx = fixture(McpConfirmation::Core);
    for _ in 0..2 {
        let out = fx.call(&app(), "x.exec", json!({}), &next_key()).await;
        ok(&out);
    }
    fx.confirm.presence().opened();
    assert_eq!(fx.destructive_runs().await, 0);
    assert_eq!(fx.destructive_runs().await, 1);
    assert!(fx.pending().await.is_empty() && fx.history().await.is_empty());
    // An agent's write never asks either.
    let out = fx
        .call(&cli(), "test.write", json!({"label": "w"}), &next_key())
        .await;
    ok(&out);
}

#[tokio::test] // spec: QA-016, ARCH-055, APP-051
async fn non_interactive_never_gets_pending() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    for caller in [CallerKind::Cli, CallerKind::Mcp] {
        let s = session(caller, "s_script", true);
        let out = fx
            .call(&s, "test.destructive", json!({}), &next_key())
            .await;
        assert_eq!(code(&out), "confirm.unavailable");
    }
    assert!(fx.pending().await.is_empty());
    assert!(fx.history().await.is_empty());
    assert_eq!(fx.destructive_runs().await, 0);

    // I2: a non-interactive retry of an interactive call's pending token gets the error too,
    // and the key stays for the interactive retry.
    let k = next_key();
    let p = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    let script = session(CallerKind::Cli, "s_script", true);
    let out = fx.call(&script, "test.destructive", json!({}), &k).await;
    assert_eq!(code(&out), "confirm.unavailable");
    let again = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    assert_eq!(again, p, "the key was kept");
    // Once you approved it, the call is yours to let run, from wherever it is retried.
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    let ran = fx.call(&script, "test.destructive", json!({}), &k).await;
    assert_eq!(ok(&ran)["count"], 2, "after the probe above");
}

#[tokio::test] // spec: CORE-111
async fn confirm_true_argument_is_never_accepted() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    for tool in ["test.destructive", "x.exec"] {
        let out = fx
            .call(&cli(), tool, json!({"confirm": true}), &next_key())
            .await;
        assert_eq!(code(&out), "params.invalid", "{tool}");
    }
    // And on the wire: `confirm` is no field of `tools/call` nor of an answer.
    let forged = json!({
        "name": "test.destructive", "arguments": {}, "idempotency_key": "k", "confirm": true
    });
    assert!(serde_json::from_value::<ToolCallParams>(forged).is_err());
    assert!(
        serde_json::from_value::<ConfirmationAnswerParams>(
            json!({"confirmation_id": "cf_1", "answer": "approve", "confirm": true})
        )
        .is_err()
    );
    assert_eq!(fx.destructive_runs().await, 0);
}

#[tokio::test] // spec: ARCH-053, APP-044, CORE-111
async fn only_a_verified_ui_session_can_answer_directly() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let p = pending(
        &fx.call(&cli(), "test.destructive", json!({}), &next_key())
            .await,
    );
    let ask = |answer| ConfirmationAnswerParams {
        confirmation_id: p.confirmation_id.clone(),
        answer,
        cap: None,
    };
    // Agents never answer, and an app whose hello shared no confirmation capability can't.
    let mut lacking = (*app()).clone();
    lacking.capabilities = vec!["events.v1".into(), "tools.v1".into()];
    for by in [
        cli(),
        session(CallerKind::Mcp, "s_mcp", false),
        session(CallerKind::Assistant, "s_engine", false),
        Arc::new(lacking),
    ] {
        let e = fx
            .confirm
            .answer(&by, ask(Answer::Approve))
            .await
            .unwrap_err();
        assert_eq!(e.code, "caller.not_allowed", "{}", by.caller);
    }
    assert_eq!(fx.pending().await.len(), 1, "still pending");
    let e = fx
        .confirm
        .answer(
            &app(),
            ConfirmationAnswerParams {
                confirmation_id: "cf_ffffffffffffffff".into(),
                answer: Answer::Approve,
                cap: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "confirm.unknown");
    let cap = fx
        .confirm
        .answer(
            &app(),
            ConfirmationAnswerParams {
                cap: Some(lil_protocol::app::CapAnswer {
                    action: lil_protocol::app::CapAction::AllowOnce,
                    raise_to: None,
                }),
                ..ask(Answer::Approve)
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        cap.code, "params.invalid",
        "a confirm-only prompt takes no cap action"
    );
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    let again = fx
        .confirm
        .answer(&app(), ask(Answer::Deny))
        .await
        .unwrap_err();
    assert_eq!(again.code, "confirm.already_answered");
}

#[tokio::test] // spec: APP-046, CORE-111
async fn answers_record_confirmed_via_and_show_in_history() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let args = json!({"cost": "1"});
    let yes = pending(
        &fx.call(&cli(), "test.destructive", args.clone(), &next_key())
            .await,
    );
    fx.clock.advance(Duration::from_secs(5));
    let no = pending(&fx.call(&cli(), "x.exec", json!({}), &next_key()).await);
    fx.answer(&yes.confirmation_id, Answer::Approve).await;
    fx.answer(&no.confirmation_id, Answer::Deny).await;
    assert!(fx.pending().await.is_empty());
    let history = fx.history().await;
    assert_eq!(history.len(), 2, "{history:?}");
    // Newest first.
    let (denied, approved) = (&history[0], &history[1]);
    assert_eq!(approved.id, yes.confirmation_id);
    assert_eq!(approved.state.wire(), "approved");
    assert_eq!(denied.state.wire(), "denied");
    for row in [approved, denied] {
        let view = row.view();
        assert_eq!(view.confirmed_via.as_deref(), Some("app"), "{view:?}");
        assert_eq!(view.caller_kind, "cli");
        assert!(view.answered.is_some(), "{view:?}");
        assert_eq!(view.covers, [Cover::Confirm]);
    }
    assert_eq!(approved.tool, "test.destructive");
    assert_eq!(approved.arguments, args, "the arguments you saw");
    assert_eq!(denied.tool, "x.exec");
}

#[tokio::test] // spec: CORE-115, CORE-091
async fn deny_and_expiry_return_their_codes_and_release_the_key() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let k = next_key();
    let first = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    // A retry before the answer: the same pending confirmation.
    let same = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    assert_eq!(same, first);
    fx.answer(&first.confirmation_id, Answer::Deny).await;
    let denied = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(code(&denied), "confirm.denied");
    // The key was released: the same key asks again, with a new confirmation.
    let second = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    assert_ne!(second.confirmation_id, first.confirmation_id);

    // 30 minutes later it has expired.
    fx.clock.advance(LIFETIME);
    let expired = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(code(&expired), "confirm.expired");
    let third = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    assert_ne!(third.confirmation_id, second.confirmation_id);
    // Answering an expired one is too late, and the retry gets confirm.expired.
    fx.clock.advance(LIFETIME + Duration::from_secs(1));
    let late = fx
        .confirm
        .answer(
            &app(),
            ConfirmationAnswerParams {
                confirmation_id: third.confirmation_id.clone(),
                answer: Answer::Approve,
                cap: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(late.code, "confirm.expired");
    let expired = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(code(&expired), "confirm.expired");
    let states: Vec<&str> = fx.history().await.iter().map(|r| r.state.wire()).collect();
    assert_eq!(states, ["expired", "expired", "denied"]);
    assert_eq!(fx.destructive_runs().await, 0, "nothing ran");
}

/// I1: an approval holds for the lifetime from its answer; a retry after that needs a fresh
/// confirmation.
#[tokio::test] // spec: CORE-115, CORE-091
async fn an_approval_expires_unused() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let k = next_key();
    let p = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    fx.clock.advance(LIFETIME - Duration::from_secs(1));
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    // Still within the lifetime from the answer (though past the prompt's own).
    fx.clock.advance(LIFETIME - Duration::from_secs(1));
    let k2 = next_key();
    let q = pending(&fx.call(&cli(), "test.destructive", json!({}), &k2).await);
    fx.answer(&q.confirmation_id, Answer::Approve).await;
    fx.clock.advance(LIFETIME);
    let expired = fx.call(&cli(), "test.destructive", json!({}), &k2).await;
    assert_eq!(code(&expired), "confirm.expired");
    let fresh = pending(&fx.call(&cli(), "test.destructive", json!({}), &k2).await);
    assert_ne!(
        fresh.confirmation_id, q.confirmation_id,
        "a fresh confirmation"
    );
    assert_eq!(
        fx.destructive_runs().await,
        0,
        "the stale approval ran nothing"
    );
    // 29 days later (the reviewer's probe): still expired, never run.
    fx.clock.advance(Duration::from_secs(29 * 24 * 3600));
    let stale = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(code(&stale), "confirm.expired");
    assert_eq!(fx.destructive_runs().await, 1, "only the probe ran");
}

/// N1: moving the wall clock back never stretches an approval: an answer dated after now
/// counts as expired.
#[tokio::test] // spec: CORE-115, CORE-012
async fn a_clock_moved_back_expires_an_approval() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let k = next_key();
    let p = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    fx.clock.shift_utc(-3 * 3600);
    let out = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(code(&out), "confirm.expired");
    assert_eq!(fx.destructive_runs().await, 0, "it never ran");
}

/// M6: answered, expired and cancelled confirmations go with the idempotency rows after 30
/// days; pending and recent ones stay.
#[tokio::test] // spec: CORE-016, APP-046
async fn old_confirmations_are_pruned() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let old = pending(&fx.call(&cli(), "x.exec", json!({}), &next_key()).await);
    fx.answer(&old.confirmation_id, Answer::Deny).await;
    fx.clock.advance(Duration::from_secs(29 * 24 * 3600));
    let recent = pending(&fx.call(&cli(), "x.exec", json!({}), &next_key()).await);
    fx.answer(&recent.confirmation_id, Answer::Deny).await;
    fx.clock.advance(Duration::from_secs(2 * 24 * 3600));
    // Any call with a key runs the prune.
    let waiting = pending(&fx.call(&cli(), "x.exec", json!({}), &next_key()).await);
    let history: Vec<String> = fx.history().await.into_iter().map(|r| r.id).collect();
    assert_eq!(history, [recent.confirmation_id]);
    let pending_now: Vec<String> = fx.pending().await.into_iter().map(|r| r.id).collect();
    assert_eq!(pending_now, [waiting.confirmation_id]);
}

/// M5, M2: in a workspace (the usual case) the confirmation lives in the workspace's
/// `state.db`, and the approval is for the call's resolved project: one that now resolves to
/// another project asks again.
#[tokio::test] // spec: ARCH-099, ARCH-088, CORE-091
async fn workspace_confirmations_live_there_and_bind_the_project() {
    let fx = fixture(McpConfirmation::Core);
    let folder = fx.env.make_workspace("ws");
    fx.env.register_workspace(&folder, true);
    fx.dispatcher.hub().refresh_default();
    let canonical = std::fs::canonicalize(&folder).unwrap();
    let text = canonical.to_string_lossy().into_owned();
    let ws = lil_core::workspace::WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: std::path::PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    };
    let agent = Arc::new(Session {
        workspace: Some(ws.clone()),
        ..(*cli()).clone()
    });
    fx.confirm.presence().opened();
    let k = next_key();
    let p = pending(&fx.call(&agent, "test.destructive", json!({}), &k).await);
    let listed = fx.pending().await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].scope, format!("cli|{}", ws.id));
    assert_eq!(listed[0].project.as_deref(), Some("sketchbook"));
    assert!(fx.confirm.busy());
    let suite_rows: i64 = rusqlite::Connection::open_with_flags(
        fx.env.env.suite_state_db(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row("SELECT count(*) FROM confirmations", [], |r| r.get(0))
    .unwrap();
    assert_eq!(suite_rows, 0, "not in the suite state.db");
    fx.answer(&p.confirmation_id, Answer::Approve).await;

    // The call would now run in another project: the approval doesn't cover it.
    fx.dispatcher.active_projects().set(&agent.id, "other");
    let q = pending(&fx.call(&agent, "test.destructive", json!({}), &k).await);
    assert_ne!(q.confirmation_id, p.confirmation_id);
    let listed = fx.pending().await;
    assert_eq!(listed[0].project.as_deref(), Some("other"));
    fx.answer(&q.confirmation_id, Answer::Approve).await;
    let ran = fx.call(&agent, "test.destructive", json!({}), &k).await;
    assert_eq!(ok(&ran)["count"], 1);
    let replay = fx.call(&agent, "test.destructive", json!({}), &k).await;
    assert_eq!(ok(&replay)["count"], 1, "once");
    assert!(!fx.confirm.busy());
}

#[tokio::test] // spec: CORE-016, CORE-091, ARCH-053
async fn retry_with_other_arguments_is_a_conflict() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let k = next_key();
    let p = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    let other = fx
        .call(&cli(), "test.destructive", json!({"cost": "1"}), &k)
        .await;
    assert_eq!(code(&other), "idempotency.conflict");
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    let other = fx
        .call(&cli(), "test.destructive", json!({"cost": "1"}), &k)
        .await;
    assert_eq!(
        code(&other),
        "idempotency.conflict",
        "approval is for what you saw"
    );
    let ran = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(ok(&ran)["count"], 1);
}

#[tokio::test] // spec: ARCH-053, CORE-016, CORE-091
async fn another_caller_kind_cannot_use_the_token() {
    let fx = fixture(McpConfirmation::Core);
    fx.confirm.presence().opened();
    let k = next_key();
    let p = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    // The token is the cli caller's key: another caller kind's call with it is a call of its
    // own, in its own scope, and asks for itself.
    for caller in [CallerKind::Mcp, CallerKind::Assistant] {
        let s = session(caller.clone(), "s_other", false);
        let theirs = pending(&fx.call(&s, "test.destructive", json!({}), &k).await);
        assert_ne!(theirs.confirmation_id, p.confirmation_id, "{caller}");
    }
    assert_eq!(fx.destructive_runs().await, 0, "nothing ran on the token");
    let ran = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(
        ok(&ran)["count"],
        2,
        "the owner's call runs (after the probe above)"
    );
    let replay = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(ok(&replay)["count"], 2, "once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // spec: CORE-016, CORE-091
async fn approve_and_retry_race_runs_once() {
    let fx = Arc::new(fixture(McpConfirmation::Core));
    fx.confirm.presence().opened();
    let k = next_key();
    let p = pending(&fx.call(&cli(), "test.destructive", json!({}), &k).await);
    // Retries race the answer and each other.
    let retry = |fx: Arc<Fx>, k: String| async move {
        fx.call(&cli(), "test.destructive", json!({}), &k).await
    };
    let mut racing: Vec<_> = (0..8)
        .map(|_| tokio::spawn(retry(fx.clone(), k.clone())))
        .collect();
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    racing.extend((0..8).map(|_| tokio::spawn(retry(fx.clone(), k.clone()))));
    let mut ran = Vec::new();
    for out in racing {
        match out.await.unwrap() {
            Outcome::Ok(out) => ran.push(out.structured["count"].as_u64().unwrap()),
            Outcome::PendingConfirmation(q) => assert_eq!(q, p),
            Outcome::Error { error } => {
                assert_eq!(error.code, "idempotency.in_progress", "{error:?}");
            }
        }
    }
    assert!(ran.iter().all(|&n| n == 1), "{ran:?}");
    let last = fx.call(&cli(), "test.destructive", json!({}), &k).await;
    assert_eq!(ok(&last)["count"], 1, "it ran once");
    assert_eq!(fx.destructive_runs().await, 1);
}

#[tokio::test] // spec: ARCH-054, CORE-112, APP-046
async fn mcp_sessions_get_no_marker_and_core_confirmations() {
    let marked = |tools: &[lil_protocol::ToolDescriptor]| -> Vec<String> {
        tools
            .iter()
            .filter(|t| {
                t.annotations
                    .get("_meta")
                    .and_then(|m| m.get("anthropic/requiresUserInteraction"))
                    == Some(&json!(true))
            })
            .map(|t| t.name.clone())
            .collect()
    };
    // "core": no tool of any session carries the marker; an mcp call is any agent call.
    let core = fixture(McpConfirmation::Core);
    let reg = core.dispatcher.registry();
    for caller in [CallerKind::Mcp, CallerKind::Cli] {
        let listed = core.confirm.annotate(&caller, reg.list_for(&caller));
        assert!(
            marked(&listed).is_empty(),
            "{caller}: {:?}",
            marked(&listed)
        );
    }
    let mcp = session(CallerKind::Mcp, "s_mcp", false);
    let out = core
        .call(&mcp, "test.destructive", json!({}), &next_key())
        .await;
    assert_eq!(code(&out), "confirm.unavailable");
    core.confirm.presence().opened();
    pending(
        &core
            .call(&mcp, "test.destructive", json!({}), &next_key())
            .await,
    );
    assert_eq!(core.destructive_runs().await, 0);

    // "claude_code": the mcp session's destructive and exec tools carry it, and its call
    // proceeds, recorded as Claude Code's confirmation; other sessions are as before.
    let cc = fixture(McpConfirmation::ClaudeCode);
    let reg = cc.dispatcher.registry();
    let listed = cc
        .confirm
        .annotate(&CallerKind::Mcp, reg.list_for(&CallerKind::Mcp));
    let names = marked(&listed);
    assert!(names.contains(&"test.destructive".to_string()), "{names:?}");
    assert!(names.contains(&"x.exec".to_string()), "{names:?}");
    for t in &listed {
        assert_eq!(
            names.contains(&t.name),
            t.effect.needs_agent_confirmation(),
            "{}",
            t.name
        );
    }
    let cli_listed = cc
        .confirm
        .annotate(&CallerKind::Cli, reg.list_for(&CallerKind::Cli));
    assert!(marked(&cli_listed).is_empty());
    let out = cc
        .call(&mcp, "test.destructive", json!({}), &next_key())
        .await;
    assert_eq!(ok(&out)["count"], 1);
    let history = cc.history().await;
    assert_eq!(history.len(), 1, "{history:?}");
    let view = history[0].view();
    assert_eq!(view.state, "approved");
    assert_eq!(view.confirmed_via.as_deref(), Some("claude_code"));
    assert_eq!(view.caller_kind, "mcp");
    // A non-interactive mcp call (no person saw a prompt) fails closed (M1).
    let script = session(CallerKind::Mcp, "s_mcp_script", true);
    let out = cc
        .call(&script, "test.destructive", json!({}), &next_key())
        .await;
    assert_eq!(code(&out), "confirm.unavailable");
    assert_eq!(cc.history().await.len(), 1, "nothing recorded for it");
    // A cli call still gets the core's confirmation.
    let out = cc
        .call(&cli(), "test.destructive", json!({}), &next_key())
        .await;
    assert_eq!(code(&out), "confirm.unavailable");
}

#[tokio::test] // spec: CORE-114, APP-054
async fn ui_cap_prompts_are_cancelled_when_the_app_closes() {
    let fx = fixture(McpConfirmation::Core);
    let request = |caller: &str, session: &str, covers: Vec<Cover>| ConfirmRequest {
        scope: format!("{caller}|none"),
        idem_key: next_key(),
        tool: "test.destructive".into(),
        args_hash: "h".into(),
        arguments: json!({}),
        caller_kind: caller.into(),
        session_id: session.into(),
        project: None,
        covers,
        cap: None,
    };
    let cap = fx
        .confirm
        .request(request("ui:test", "s_app", vec![Cover::Cap]))
        .await
        .unwrap();
    let agent = fx
        .confirm
        .request(request("cli", "s_cli", vec![Cover::Confirm]))
        .await
        .unwrap();
    // An agent's session ending leaves its safety confirmation: the agent retries.
    fx.confirm.cancel_for_session("s_cli").await;
    assert_eq!(fx.pending().await.len(), 2);
    fx.confirm.cancel_for_session("s_app").await;
    let pending: Vec<String> = fx.pending().await.into_iter().map(|r| r.id).collect();
    assert_eq!(pending, [agent.confirmation_id]);
    let history = fx.history().await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, cap.confirmation_id);
    assert_eq!(history[0].state.wire(), "cancelled");
}

#[tokio::test] // spec: ARCH-020
async fn pending_confirmations_hold_idle() {
    let fx = fixture(McpConfirmation::Core);
    let idle_after = Duration::from_secs(60);
    let clock: Arc<dyn Clock> = fx.clock.clone();
    let lifecycle = Lifecycle::new(clock, idle_after);
    lifecycle.add_blocker(fx.confirm.clone());
    assert!(!fx.confirm.busy());
    fx.confirm.presence().opened();
    let p = pending(
        &fx.call(&cli(), "test.destructive", json!({}), &next_key())
            .await,
    );
    assert!(fx.confirm.busy());
    for _ in 0..3 {
        fx.clock.advance(idle_after);
        assert_eq!(lifecycle.tick(), None, "a pending confirmation holds idle");
    }
    // Answered, it no longer does; nor does one that expired.
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    assert!(!fx.confirm.busy());
    pending(
        &fx.call(&cli(), "test.destructive", json!({}), &next_key())
            .await,
    );
    assert!(fx.confirm.busy());
    fx.clock.advance(LIFETIME);
    assert!(!fx.confirm.busy(), "an expired confirmation holds nothing");
    assert_eq!(lifecycle.tick(), None, "the idle period starts now");
    fx.clock.advance(idle_after);
    assert_eq!(lifecycle.tick(), Some(DrainReason::Idle));

    // A pending confirmation never holds a drain: it is still there after the restart.
    pending(
        &fx.call(&cli(), "test.destructive", json!({}), &next_key())
            .await,
    );
    assert!(fx.confirm.busy());
    lifecycle.begin_drain(DrainReason::Requested);
    let report = tokio::time::timeout(DEADLINE, lifecycle.run_shutdown())
        .await
        .expect("the drain was not held");
    assert_eq!(report.reason, DrainReason::Requested);
}

// ---------------------------------------------------------------------------------------------
// Over the pipe of an in-process core, with the `ui:test` app.
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

fn connect(env: &TestEnv, hello: Value) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let response = pipe.hello(hello);
    assert_eq!(response["result"]["status"], "ok", "hello: {response}");
    pipe
}

fn pending_id(outcome: &Value) -> String {
    assert_eq!(outcome["status"], "pending_confirmation", "{outcome}");
    outcome["confirmation_id"].as_str().unwrap().to_string()
}

fn answer_with(c: &mut TestApp, id: &str, answer: &str) -> Value {
    c.call(
        "app/confirmation.answer",
        json!({"confirmation_id": id, "answer": answer}),
    )
}

/// The next `confirmations` event of `app` (failure deadline 30 s).
fn next_confirmation_event(app: &mut TestApp) -> Value {
    loop {
        let event = app
            .next_event(DEADLINE)
            .expect("a confirmations event within the deadline");
        if event["topic"] == "confirmations" {
            return event["data"].clone();
        }
    }
}

#[test] // spec: ARCH-053, APP-044, CORE-111
fn only_a_verified_ui_session_can_answer() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut app = TestApp::start(&env, None, &[]);
    assert_eq!(app.hello()["caller_kind"], "ui:test");
    let mut agent = connect(&env, json!({}));
    let k = next_key();
    let id = pending_id(&agent.tool("test.destructive", json!({}), Some(&k)));

    // The agent's own connection, and one that claims to be an app (this test process is
    // no app, so it is `cli`), can't answer.
    let mut claims_app = connect(&env, json!({"caller_kind": "ui:test"}));
    for pipe in [&mut agent, &mut claims_app] {
        let refused = pipe.call(
            "app/confirmation.answer",
            json!({"confirmation_id": id, "answer": "approve"}),
        );
        assert_eq!(
            refused["result"]["error"]["code"], "caller.not_allowed",
            "{refused}"
        );
        // `confirm: true` is never a way in either.
        let forged = pipe.call(
            "tools/call",
            json!({"name": "test.destructive", "arguments": {}, "idempotency_key": k, "confirm": true}),
        );
        assert_eq!(forged["error"]["code"], -32602, "{forged}");
    }
    // `cli` may read the list (Appendix F); it still shows the confirmation pending.
    let listed = agent.call("app/confirmations.list", json!({"state": "pending"}));
    assert_eq!(listed["result"]["status"], "ok", "{listed}");
    assert_eq!(listed["result"]["confirmations"][0]["id"], id.as_str());
    assert_eq!(listed["result"]["confirmations"][0]["state"], "pending");
    let forged = app.call(
        "app/confirmation.answer",
        json!({"confirmation_id": id, "answer": "approve", "confirm": true}),
    );
    assert_eq!(forged["error"]["code"], -32602, "{forged}");

    let answered = answer_with(&mut app, &id, "approve");
    assert_eq!(answered["result"]["status"], "ok", "{answered}");
    assert_eq!(answered["result"]["confirmation"]["state"], "approved");
    assert_eq!(answered["result"]["confirmation"]["confirmed_via"], "app");
    let ran = agent.tool("test.destructive", json!({}), Some(&k));
    assert_eq!(ran["structured"]["count"], 1, "{ran}");
    let replay = agent.tool("test.destructive", json!({}), Some(&k));
    assert_eq!(replay, ran, "it runs once");
}

#[test] // spec: APP-054, APP-046
fn answering_anywhere_publishes_answered_once() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut first = TestApp::start(&env, None, &["confirmations"]);
    let mut second = TestApp::start(&env, None, &["confirmations"]);
    let mut agent = connect(&env, json!({}));
    let id = pending_id(&agent.tool("test.destructive", json!({}), Some(&next_key())));
    for app in [&mut first, &mut second] {
        let event = next_confirmation_event(app);
        assert_eq!(event["kind"], "pending", "{event}");
        assert_eq!(event["confirmation"]["id"], id.as_str(), "{event}");
    }
    let answered = answer_with(&mut first, &id, "approve");
    assert_eq!(answered["result"]["status"], "ok", "{answered}");
    let late = answer_with(&mut second, &id, "deny");
    assert_eq!(
        late["result"]["error"]["code"], "confirm.already_answered",
        "{late}"
    );
    // A later confirmation marks the end: the events between are all there were.
    let marker = pending_id(&agent.tool("test.destructive", json!({}), Some(&next_key())));
    for app in [&mut first, &mut second] {
        let mut seen = Vec::new();
        loop {
            let event = next_confirmation_event(app);
            if event["confirmation"]["id"] == marker.as_str() {
                break;
            }
            seen.push(event);
        }
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0]["kind"], "answered");
        assert_eq!(seen[0]["confirmation"]["id"], id.as_str());
        assert_eq!(seen[0]["confirmation"]["state"], "approved");
    }
}

#[test] // spec: APP-043, APP-051
fn app_methods_need_the_capability() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut old = TestApp::start_with_capabilities(&env, None, &[], &["events.v1", "tools.v1"]);
    assert_eq!(old.hello()["caller_kind"], "ui:test");
    for (method, params) in [
        ("app/confirmations.list", json!({"state": "pending"})),
        (
            "app/confirmation.answer",
            json!({"confirmation_id": "cf_1", "answer": "approve"}),
        ),
    ] {
        let refused = old.call(method, params);
        assert_eq!(
            refused["result"]["error"]["code"], "caller.not_allowed",
            "{method}: {refused}"
        );
    }
    // Such an app can't answer, so it doesn't make a confirmation answerable.
    let mut agent = connect(&env, json!({}));
    let out = agent.tool("test.destructive", json!({}), Some(&next_key()));
    assert_eq!(out["error"]["code"], "confirm.unavailable", "{out}");

    let mut app = TestApp::start(&env, None, &[]);
    let listed = app.call("app/confirmations.list", json!({"state": "history"}));
    assert_eq!(listed["result"]["status"], "ok", "{listed}");
    let unknown = app.call("app/caps.set", json!({}));
    assert_eq!(unknown["error"]["code"], -32601, "Task 16's: {unknown}");
    let bad = app.call(
        "app/confirmations.list",
        json!({"state": "pending", "limit": 0}),
    );
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    let out = agent.tool("test.destructive", json!({}), Some(&next_key()));
    pending_id(&out);
}

#[test] // spec: CORE-091, CORE-115, QA-007
fn pending_survives_core_restart_and_runs_once() {
    let env = TestEnv::new();
    let k = next_key();
    let id = {
        let _core = core(&env);
        let _app = TestApp::start(&env, None, &[]);
        let mut agent = connect(&env, json!({}));
        pending_id(&agent.tool("test.destructive", json!({}), Some(&k)))
    };
    let core = core(&env);
    assert!(
        core.core().confirm().busy(),
        "the new core knows the pending confirmation"
    );
    let mut app = TestApp::start(&env, None, &[]);
    let listed = app.call("app/confirmations.list", json!({"state": "pending"}));
    assert_eq!(
        listed["result"]["confirmations"][0]["id"],
        id.as_str(),
        "{listed}"
    );
    let mut agent = connect(&env, json!({}));
    assert_eq!(
        pending_id(&agent.tool("test.destructive", json!({}), Some(&k))),
        id,
        "the same confirmation"
    );
    let answered = answer_with(&mut app, &id, "approve");
    assert_eq!(answered["result"]["status"], "ok", "{answered}");
    let ran = agent.tool("test.destructive", json!({}), Some(&k));
    assert_eq!(ran["structured"]["count"], 1, "{ran}");
    assert_eq!(agent.tool("test.destructive", json!({}), Some(&k)), ran);
    assert!(!core.core().confirm().busy());
}

/// Review I1: the marker the core sets (`annotations["_meta"]`) reaches the top level of the
/// MCP `_meta` that Claude Code reads, never a nested `_meta`.
#[tokio::test] // spec: ARCH-054, CORE-112, AI-111
async fn claude_code_marker_lowers_to_the_top_level_of_mcp_meta() {
    const MARKER: &str = "anthropic/requiresUserInteraction";
    let cc = fixture(McpConfirmation::ClaudeCode);
    let reg = cc.dispatcher.registry();
    let listed = cc
        .confirm
        .annotate(&CallerKind::Mcp, reg.list_for(&CallerKind::Mcp));
    assert!(listed.iter().any(|t| t.effect.needs_agent_confirmation()));
    for t in &listed {
        let meta = lil_protocol::lowering::mcp_meta(t);
        assert!(
            !meta.contains_key("_meta"),
            "{}: nested _meta {meta:?}",
            t.name
        );
        let marked = meta.get(MARKER) == Some(&json!(true));
        assert_eq!(
            marked,
            t.effect.needs_agent_confirmation(),
            "{}: {meta:?}",
            t.name
        );
    }
    // With "core" no lowered tool carries it.
    let core = fixture(McpConfirmation::Core);
    let reg = core.dispatcher.registry();
    for t in core
        .confirm
        .annotate(&CallerKind::Mcp, reg.list_for(&CallerKind::Mcp))
    {
        let meta = lil_protocol::lowering::mcp_meta(&t);
        assert!(
            !meta.contains_key(MARKER) && !meta.contains_key("_meta"),
            "{}",
            t.name
        );
    }
}
