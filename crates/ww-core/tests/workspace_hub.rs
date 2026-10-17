//! `WorkspaceHub` and the ARCH-028 `core.lock` (ARCH-028, ARCH-097, CORE-005, CORE-016,
//! CORE-038): the lock holder serves the workspace read-write and releases stale `running`
//! rows; a core without it serves it read-only, never touching the rows, and with no
//! `state.db` on disk an in-memory one stands in and nothing is created. One open per
//! workspace, and its orphans are published once.
//!
//! Wall-clock durations here are failure deadlines only, never measurements (QA-031).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use ww_core::events::EventBus;
use ww_core::state::{IdemBegin, StateRegistry, WORKSPACE_MIGRATIONS};
use ww_core::store::{Expect, FailPoints, StoreError, WsPath, fsops};
use ww_core::workspace::{Access, WorkspaceHub, WorkspaceRef};
use ww_platform::fs::SystemProbe;
use ww_platform::{Clock, Env};
use ww_testkit::files::write_file;
use ww_testkit::{FakeClock, SeqIds, TestEnv};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

/// `ws` as a resolved workspace: its canonical path without `\\?\`, and its id.
fn workspace_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

/// A hub over `env` and `states`, with its own bus (returned too).
fn hub(env: &Env, states: &Arc<StateRegistry>) -> (Arc<WorkspaceHub>, Arc<EventBus>) {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let hub = WorkspaceHub::new(
        env.clone(),
        states.clone(),
        events.clone(),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        Arc::new(SystemProbe),
    );
    (hub, events)
}

fn registry(env: &Env) -> Arc<StateRegistry> {
    Arc::new(StateRegistry::open(env, no_fatal()).expect("the suite state.db"))
}

/// The idempotency rows of `path` as `(scope, key, state)`, read-only.
fn rows(path: &Path) -> Vec<(String, String, String)> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    let mut stmt = db
        .prepare("SELECT scope, key, state FROM idempotency ORDER BY scope, key")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn core_lock(ws: &WorkspaceRef) -> PathBuf {
    ws.path.join(".worldwright").join("core.lock")
}

/// Whether another holder could take `core.lock` now (the probe's lock is released at once).
fn lock_is_free(ws: &WorkspaceRef) -> bool {
    fsops::lock_exclusive(&core_lock(ws)).unwrap().is_some()
}

const NOW: i64 = 1_800_000_000;

#[test] // spec: ARCH-028, CORE-016
fn core_lock_gives_read_write_and_releases_running_rows() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_workspace("alpha"));
    let db = env
        .env
        .workspace_local_dir(&ws.path)
        .unwrap()
        .join("state.db");
    let rt = runtime();

    // A core that died mid-call left a `running` row.
    {
        let states = registry(&env.env);
        let state = rt
            .block_on(states.for_workspace(&ws, Access::ReadWrite))
            .unwrap();
        assert_eq!(
            rt.block_on(state.idem_begin("cli|ws", "k", "h", NOW))
                .unwrap(),
            IdemBegin::Fresh
        );
        states.close();
    }
    assert_eq!(rows(&db), [("cli|ws".into(), "k".into(), "running".into())]);

    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);
    let open = rt.block_on(hub.open(&ws)).expect("the open");
    assert_eq!(open.access, Access::ReadWrite);
    assert_eq!(open.ws, ws);
    assert!(
        rows(&db).is_empty(),
        "the holder of core.lock releases the stale row"
    );
    assert!(core_lock(&ws).is_file());
    assert!(
        !lock_is_free(&ws),
        "core.lock is held while the workspace is open"
    );
    assert!(!rt.block_on(open.state.diagnostics()).unwrap().read_only);

    // The hub lets go of it; with no call holding it, it is closed at once.
    drop(open);
    rt.block_on(hub.close(&ws.id));
    assert!(lock_is_free(&ws), "close releases core.lock");
}

#[test] // spec: ARCH-028, ARCH-097
fn second_hub_without_the_lock_is_read_only_and_keeps_rows() {
    // Two registries whose envs map the workspace to one local folder, as two channels'
    // cores share `<volume>\.worldwright-local\<hash>\` for a workspace on another volume
    // (ARCH-097). The harness guard keeps tests out of every `.worldwright-local` folder, so
    // the two envs here are one test channel's.
    let a = TestEnv::new();
    let (env_a, env_b) = (a.env.clone(), a.env.clone());
    let ws = workspace_ref(&a.make_workspace("shared"));
    let local = env_a.workspace_local_dir(&ws.path).unwrap();
    assert_eq!(
        local,
        env_b.workspace_local_dir(&ws.path).unwrap(),
        "one local folder"
    );
    let db = local.join("state.db");
    let rt = runtime();

    let states_a = registry(&env_a);
    let (hub_a, _) = hub(&env_a, &states_a);
    let first = rt.block_on(hub_a.open(&ws)).unwrap();
    assert_eq!(first.access, Access::ReadWrite);
    assert_eq!(
        rt.block_on(first.state.idem_begin("cli|ws", "k", "h", NOW))
            .unwrap(),
        IdemBegin::Fresh
    );

    let states_b = registry(&env_b);
    let (hub_b, _) = hub(&env_b, &states_b);
    let second = rt.block_on(hub_b.open(&ws)).unwrap();
    assert_eq!(second.access, Access::ReadOnly);
    assert_eq!(second.state.path(), db, "the shared state.db");
    assert!(
        rt.block_on(second.state.diagnostics()).unwrap().read_only,
        "its connection is read-only"
    );
    assert_eq!(
        rows(&db),
        [("cli|ws".into(), "k".into(), "running".into())],
        "the first core's running row survives"
    );
    // Its store never writes.
    let refused = rt
        .block_on(second.store.write(
            &WsPath::parse("notes/b.txt").unwrap(),
            b"b".to_vec(),
            Expect::New,
        ))
        .unwrap_err();
    assert_eq!(refused, StoreError::ReadOnly);
    assert!(!ws.path.join("notes").exists());
    // And its database refuses writes too.
    assert!(
        rt.block_on(second.state.idem_begin("cli|ws", "j", "h", NOW))
            .is_err()
    );

    // Once the first core has let go, a new open takes the lock. (A hub closes a workspace
    // once nothing holds it.)
    drop((first, second));
    rt.block_on(hub_a.close(&ws.id));
    rt.block_on(hub_b.close(&ws.id));
    let third = rt.block_on(hub_b.open(&ws)).unwrap();
    assert_eq!(third.access, Access::ReadWrite);
    assert!(!rt.block_on(third.state.diagnostics()).unwrap().read_only);
}

#[test] // spec: ARCH-028, ARCH-097
fn read_only_open_without_a_state_db_uses_memory_and_creates_nothing() {
    let (a, b) = (TestEnv::new(), TestEnv::new());
    let ws_path = a.make_workspace("shared");
    let ws = workspace_ref(&ws_path);
    a.register_workspace(&ws_path, true);
    b.register_workspace(&ws_path, true);
    let rt = runtime();

    let states_a = registry(&a.env);
    let (hub_a, _) = hub(&a.env, &states_a);
    let first = rt.block_on(hub_a.open(&ws)).unwrap();
    assert_eq!(first.access, Access::ReadWrite);

    // Another channel: its local folder for the workspace is another one, and empty.
    let local_b = b.env.workspace_local_dir(&ws.path).unwrap();
    assert_ne!(local_b, a.env.workspace_local_dir(&ws.path).unwrap());
    let states_b = registry(&b.env);
    let (hub_b, events_b) = hub(&b.env, &states_b);
    let second = rt.block_on(hub_b.open(&ws)).unwrap();
    assert_eq!(second.access, Access::ReadOnly);
    assert_eq!(
        second.state.schema_version(),
        i64::try_from(WORKSPACE_MIGRATIONS.len()).unwrap(),
        "the migrations are applied in memory"
    );
    // Reads work.
    let yaml = rt
        .block_on(
            second
                .store
                .read(&WsPath::parse("worldwright.yaml").unwrap()),
        )
        .unwrap();
    assert_eq!(yaml.as_deref(), Some(&b"name: shared\n"[..]));
    rt.block_on(second.store.orphans()).unwrap();

    // Through the dispatcher: a read tool runs, a write tool gets `workspace.read_only`.
    let dispatcher = dispatcher(&b.env, &states_b, &events_b, &hub_b);
    let session = session(Some(ws.clone()));
    let echo = rt.block_on(dispatcher.call(
        session.clone(),
        params("test.echo", json!({"text": "read"}), None),
    ));
    assert!(matches!(echo, ww_protocol::Outcome::Ok(_)), "{echo:?}");
    let write = rt.block_on(dispatcher.call(
        session,
        params(
            "test.store_write",
            json!({"path": "notes/b.txt", "text": "b", "expect": "new"}),
            Some("k"),
        ),
    ));
    let ww_protocol::Outcome::Error { error } = &write else {
        panic!("expected workspace.read_only: {write:?}");
    };
    assert_eq!(error.code, "workspace.read_only", "{error:?}");
    assert!(!ws.path.join("notes").exists());
    assert!(
        !local_b.exists(),
        "the read-only core created {}",
        local_b.display()
    );
}

#[test] // spec: CORE-005, QA-012
fn concurrent_first_opens_share_one_open_workspace() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_workspace("alpha"));
    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);
    let rt = runtime();
    let opens = rt.block_on(async {
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let (hub, ws) = (hub.clone(), ws.clone());
                tokio::spawn(async move { hub.open(&ws).await })
            })
            .collect();
        let mut opens = Vec::new();
        for task in tasks {
            opens.push(task.await.unwrap().expect("an open"));
        }
        opens
    });
    for open in &opens {
        assert!(Arc::ptr_eq(open, &opens[0]), "one OpenWorkspace per id");
        assert!(Arc::ptr_eq(&open.state, &opens[0].state));
    }
    let state = rt
        .block_on(states.for_workspace(&ws, Access::ReadWrite))
        .unwrap();
    assert!(Arc::ptr_eq(&state, &opens[0].state), "one state actor");
}

#[test] // spec: CORE-038, ARCH-032
fn open_lists_orphans_once_on_the_workspace_topic() {
    let env = TestEnv::new();
    let ws_path = env.make_workspace("alpha");
    write_file(
        &ws_path
            .join("notes")
            .join("a.txt.ww-conflict-20261016T120000Z"),
        b"outside",
    );
    let ws = workspace_ref(&ws_path);
    let clean = workspace_ref(&env.make_workspace("clean"));
    let states = registry(&env.env);
    let (hub, events) = hub(&env.env, &states);
    let rt = runtime();

    let (a, b) = rt.block_on(async { tokio::join!(hub.open(&ws), hub.open(&ws)) });
    let (a, b) = (a.unwrap(), b.unwrap());
    let again = rt.block_on(hub.open(&ws)).unwrap();
    let want = [WsPath::parse("notes/a.txt.ww-conflict-20261016T120000Z").unwrap()];
    assert_eq!(a.orphans, want);
    assert!(Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&a, &again));
    rt.block_on(hub.open(&clean)).unwrap();

    // One event on `workspace`, replayed from the history.
    let lines = rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        events.attach(1, tx);
        let since = [("workspace".to_string(), 0)].into_iter().collect();
        let states = events.subscribe(1, &["workspace".to_string()], &since);
        assert_eq!(states[0].seq, 1, "published once: {states:?}");
        let line = tokio::time::timeout(DEADLINE, rx.recv())
            .await
            .expect("the replayed event")
            .expect("an event line");
        let more = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(more.is_err(), "a second event: {more:?}");
        line
    });
    let event: Value = serde_json::from_str(&lines).unwrap();
    assert_eq!(event["params"]["topic"], "workspace");
    assert_eq!(
        event["params"]["data"],
        json!({"kind": "orphans", "paths": ["notes/a.txt.ww-conflict-20261016T120000Z"]})
    );
}

// ---------------------------------------------------------------------------------------------
// A dispatcher over a hub.
// ---------------------------------------------------------------------------------------------

fn dispatcher(
    env: &Env,
    states: &Arc<StateRegistry>,
    events: &Arc<EventBus>,
    hub: &Arc<WorkspaceHub>,
) -> ww_core::tools::Dispatcher {
    use ww_core::tools::{PipelineHooks, TestGates, core_registry};
    let clock: Arc<dyn Clock> = FakeClock::new();
    let probe: Arc<dyn ww_platform::fs::Probe> = Arc::new(SystemProbe);
    ww_core::tools::Dispatcher::new(
        core_registry(true, &Arc::new(TestGates::new()), states, hub, env, &probe),
        PipelineHooks::plan2(),
        states.clone(),
        events.clone(),
        hub.clone(),
        Arc::new(SeqIds::new()),
        clock.clone(),
        ww_core::Lifecycle::new(clock, ww_core::lifecycle::IDLE_EXIT_AFTER),
    )
}

fn session(workspace: Option<WorkspaceRef>) -> Arc<ww_core::session::Session> {
    Arc::new(ww_core::session::Session {
        id: "s_1".into(),
        caller: ww_protocol::CallerKind::Cli,
        non_interactive: false,
        workspace,
        capabilities: Vec::new(),
        client_pid: 1,
        profile_id: None,
    })
}

fn params(name: &str, args: Value, key: Option<&str>) -> ww_protocol::ToolCallParams {
    let Value::Object(arguments) = args else {
        panic!("arguments must be an object");
    };
    ww_protocol::ToolCallParams {
        name: name.into(),
        arguments,
        idempotency_key: key.map(str::to_string),
        trace_id: None,
    }
}
