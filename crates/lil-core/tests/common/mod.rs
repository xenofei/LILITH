//! A tool-call context without a core, for tests of the tools that need no workspace.
#![allow(
    dead_code,
    clippy::result_large_err,
    reason = "each test binary uses some of these; the calls return the wire ErrorBody"
)]

use std::sync::Arc;

use lil_core::events::EventBus;
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::tools::{CallCtx, Registry, Settings};
use lil_core::workspace::WorkspaceHub;
use lil_platform::Clock;
use lil_platform::fs::SystemProbe;
use lil_protocol::{CallerKind, ErrorBody, codes};
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::Value;

/// A call by `caller` in no workspace, publishing on `events`.
pub fn ctx(env: &TestEnv, caller: CallerKind, events: &Arc<EventBus>) -> CallCtx {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let states = Arc::new(
        StateRegistry::open(
            &env.env,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("the suite state.db"),
    );
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        events.clone(),
        clock.clone(),
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        Arc::new(SystemProbe),
        lil_core::watch::WatchConfig::default(),
    );
    let jobs = lil_core::jobs::Jobs::new(
        states,
        hub,
        events.clone(),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
    );
    CallCtx {
        session: Arc::new(Session {
            id: "s_0000000000000001".into(),
            caller,
            non_interactive: false,
            workspace: None,
            capabilities: Vec::new(),
            client_pid: 1,
            profile_id: None,
        }),
        trace_id: "t_test".into(),
        settings: Settings::default(),
        events: events.clone(),
        workspace: None,
        project: None,
        record: Arc::new(lil_core::pipeline::RecordSlot::new()),
        jobs,
        submitted: Arc::new(lil_core::jobs::SubmittedSlot::new()),
        reservation: None,
        confirmed_via: None,
        idem_key: None,
        call_id: String::new(),
        tool: String::new(),
        arguments: Value::Null,
        retry_of: None,
        redispatch: None,
    }
}

/// Runs tool `name` of `reg` as the dispatcher would: arguments checked first.
pub async fn call(
    reg: &Registry,
    ctx: &CallCtx,
    name: &str,
    args: Value,
) -> Result<Value, ErrorBody> {
    let tool = reg
        .get(name)
        .unwrap_or_else(|| panic!("{name} is registered"));
    if let Err(why) = tool.validate(&args) {
        return Err(ErrorBody::new(&codes::PARAMS_INVALID, why, "t_test"));
    }
    tool.run(ctx, args).await.map(|out| out.structured)
}
