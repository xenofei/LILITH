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

use lil_core::events::EventBus;
use lil_core::state::{IdemBegin, StateDb, StateRegistry, StateScope, WORKSPACE_MIGRATIONS};
use lil_core::store::{Expect, FailPoints, StoreError, WsPath, fsops};
use lil_core::workspace::{Access, WorkspaceHub, WorkspaceRef};
use lil_platform::fs::SystemProbe;
use lil_platform::names::LEGACY_WORKSPACE_DIR;
use lil_platform::{Clock, Env};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

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
        lil_core::watch::WatchConfig::default(),
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
    ws.path.join(".lilith").join("core.lock")
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

/// An open racing a close of the same workspace in one core either takes the workspace back
/// or waits until the close has finished: it never finds the old `core.lock` still held (and
/// serves read-only), and the old close never closes the new open's `state.db` (both are keyed
/// by the workspace id).
#[test] // spec: ARCH-028, CORE-005
fn close_racing_open_never_serves_read_only_or_a_closed_state_db() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_workspace("raced"));
    let rt = runtime();
    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);
    rt.block_on(async {
        drop(hub.open(&ws).await.expect("the first open"));
        for round in 0..50 {
            let ((), open) = tokio::join!(hub.close(&ws.id), hub.open(&ws));
            let open = open.unwrap_or_else(|e| panic!("round {round}: {e:?}"));
            assert_eq!(open.access, Access::ReadWrite, "round {round}");
            assert!(
                open.state.diagnostics().await.is_ok(),
                "round {round}: the open's state.db was closed under it"
            );
            drop(open);
        }
    });
    rt.block_on(hub.close_all());
    states.close();
}

#[test] // spec: ARCH-028, ARCH-097
fn second_hub_without_the_lock_is_read_only_and_keeps_rows() {
    // Two registries whose envs map the workspace to one local folder, as two channels'
    // cores share `<volume>\.lilith-local\<hash>\` for a workspace on another volume
    // (ARCH-097). The harness guard keeps tests out of every per-volume local folder, so
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
        .block_on(second.store.read(&WsPath::parse("lilith.yaml").unwrap()))
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
    assert!(matches!(echo, lil_protocol::Outcome::Ok(_)), "{echo:?}");
    let write = rt.block_on(dispatcher.call(
        session,
        params(
            "test.store_write",
            json!({"path": "notes/b.txt", "text": "b", "expect": "new"}),
            Some("k"),
        ),
    ));
    let lil_protocol::Outcome::Error { error } = &write else {
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
            .join("a.txt.lil-conflict-20261016T120000Z"),
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
    let want = [WsPath::parse("notes/a.txt.lil-conflict-20261016T120000Z").unwrap()];
    assert_eq!(a.orphans, want);
    assert!(Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&a, &again));
    rt.block_on(hub.open(&clean)).unwrap();

    // One event on `workspace`, replayed from the history.
    let lines = rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        events.attach(1, tx);
        let since = [("workspace".to_string(), 0)].into_iter().collect();
        let states = events
            .subscribe(1, &["workspace".to_string()], &since)
            .await;
        assert_eq!(states[0].seq, 1, "published once: {states:?}");
        let line = tokio::time::timeout(DEADLINE, rx.recv())
            .await
            .expect("the replayed event")
            .expect("an event line");
        // No second event, without waiting on the clock (QA-031): the replay queued exactly
        // the one event the topic's seq counts, nothing else is in the channel, and the
        // topic's seq is still 1 (a client subscribing now, live only, sees it).
        let more = rx.try_recv();
        assert!(more.is_err(), "a second event: {more:?}");
        let (tx2, _rx2) = tokio::sync::mpsc::channel(16);
        events.attach(2, tx2);
        let now = events
            .subscribe(2, &["workspace".to_string()], &Default::default())
            .await;
        assert_eq!(now[0].seq, 1, "published once: {now:?}");
        line
    });
    let event: Value = serde_json::from_str(&lines).unwrap();
    assert_eq!(event["params"]["topic"], "workspace");
    assert_eq!(
        event["params"]["data"],
        json!({"kind": "orphans", "paths": ["notes/a.txt.lil-conflict-20261016T120000Z"]})
    );
}

/// The real probe, counting the listings of `root` itself: every whole-tree walk (the
/// store's recovery and orphan scans, the index's reconcile) starts by listing the root.
#[derive(Debug)]
struct CountingWalks {
    root: PathBuf,
    walks: std::sync::atomic::AtomicUsize,
}

impl lil_platform::fs::Probe for CountingWalks {
    fn identity(&self, path: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        SystemProbe.identity(path)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }

    fn read_dir(&self, dir: &Path) -> std::io::Result<std::fs::ReadDir> {
        if dir == self.root {
            self.walks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        SystemProbe.read_dir(dir)
    }
}

/// Plan-2b Minor 9: a read-write open walks the tree once (recovery's leftover scan, which
/// also finds the conflict copies the orphan list needs) before the index's own walk.
#[test] // spec: CORE-038, CORE-041
fn workspace_open_walks_the_tree_once() {
    let env = TestEnv::new();
    let ws_path = env.make_workspace("alpha");
    let conflict = "notes/a.txt.lil-conflict-20261016T120000Z";
    write_file(
        &ws_path
            .join("notes")
            .join(conflict.rsplit('/').next().unwrap()),
        b"outside",
    );
    // A conflict copy in the workspace data folder is no orphan.
    write_file(
        &ws_path
            .join(".lilith")
            .join("x.lil-conflict-20261016T120000Z"),
        b"core's own",
    );
    let ws = workspace_ref(&ws_path);
    let probe = Arc::new(CountingWalks {
        root: ws.path.clone(),
        walks: std::sync::atomic::AtomicUsize::new(0),
    });
    let states = registry(&env.env);
    let clock: Arc<dyn Clock> = FakeClock::new();
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        Arc::new(EventBus::new(clock.clone())),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        probe.clone(),
        lil_core::watch::WatchConfig::default(),
    );
    let rt = runtime();

    let open = rt.block_on(hub.open(&ws)).expect("the workspace opens");
    assert_eq!(open.access, Access::ReadWrite);
    rt.block_on(open.index.ready());
    assert_eq!(open.orphans, [WsPath::parse(conflict).unwrap()]);
    assert_eq!(
        probe.walks.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "one walk before the index's own"
    );
    drop(open);
    rt.block_on(hub.close_all());
    states.close();
}

// ---------------------------------------------------------------------------------------------
// Both locks (plan decision D4, Review Focus 2).
// ---------------------------------------------------------------------------------------------

/// The legacy folder's `core.lock`, the one a core from before the rename takes.
fn legacy_lock(ws: &WorkspaceRef) -> PathBuf {
    ws.path.join(LEGACY_WORKSPACE_DIR).join("core.lock")
}

/// Takes `path` the way a core from before the rename takes its `core.lock`: the folder made
/// if missing, then the exclusive try-lock.
fn old_core_takes(path: &Path) -> Option<lil_platform::fs::LockedFile> {
    fsops::ensure_dir(path.parent().unwrap()).unwrap();
    fsops::lock_exclusive(path).unwrap()
}

#[test] // spec: ARCH-028
fn held_legacy_core_lock_keeps_the_new_core_read_only() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_legacy_workspace("plugin era"));
    fsops::ensure_dir(&ws.path.join(LEGACY_WORKSPACE_DIR)).unwrap();
    // A state.db from an earlier open, so the read-only open has a file to open read-only.
    let db_path = env
        .env
        .workspace_local_dir(&ws.path)
        .unwrap()
        .join("state.db");
    drop(StateDb::open(&db_path, StateScope::Workspace, no_fatal()).expect("the state.db"));
    let rt = runtime();

    // An old core serves the workspace: it holds the legacy lock only.
    let old = old_core_takes(&legacy_lock(&ws)).expect("the old core's lock");
    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);
    let open = rt.block_on(hub.open(&ws)).expect("the open");
    assert_eq!(open.access, Access::ReadOnly);
    assert!(rt.block_on(open.state.diagnostics()).unwrap().read_only);
    // It doesn't keep the new lock it could take: a later core isn't kept out by it.
    assert!(
        lock_is_free(&ws),
        "a read-only open keeps .lilith\\core.lock"
    );
    // Nothing is written beside the old core.
    let refused = rt
        .block_on(open.store.write(
            &WsPath::parse("notes/b.txt").unwrap(),
            b"b".to_vec(),
            Expect::New,
        ))
        .unwrap_err();
    assert_eq!(refused, StoreError::ReadOnly);

    // Once the old core has gone, the next open holds both and serves read-write.
    drop(open);
    rt.block_on(hub.close(&ws.id));
    drop(old);
    let open = rt.block_on(hub.open(&ws)).expect("the open");
    assert_eq!(open.access, Access::ReadWrite);
    assert!(!lock_is_free(&ws));
    assert!(old_core_takes(&legacy_lock(&ws)).is_none());
}

#[test] // spec: ARCH-028
fn old_name_core_starting_second_cannot_take_the_legacy_lock() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_legacy_workspace("plugin era"));
    fsops::ensure_dir(&ws.path.join(LEGACY_WORKSPACE_DIR)).unwrap();
    let rt = runtime();
    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);

    // The new core serves first.
    let open = rt.block_on(hub.open(&ws)).expect("the open");
    assert_eq!(open.access, Access::ReadWrite);
    assert!(
        legacy_lock(&ws).is_file(),
        "the legacy lock file was not created"
    );
    // An old core starting now gets nothing, so it would serve read-only.
    assert!(
        old_core_takes(&legacy_lock(&ws)).is_none(),
        "an old core took the legacy lock while the new core serves"
    );
    assert!(!lock_is_free(&ws));

    // The close releases both.
    drop(open);
    rt.block_on(hub.close(&ws.id));
    assert!(lock_is_free(&ws));
    assert!(old_core_takes(&legacy_lock(&ws)).is_some());
}

#[test] // spec: ARCH-028
fn no_legacy_folder_means_no_legacy_lock() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_legacy_workspace("no folder"));
    let rt = runtime();
    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);
    let open = rt.block_on(hub.open(&ws)).expect("the open");
    assert_eq!(open.access, Access::ReadWrite);
    assert!(core_lock(&ws).is_file());
    assert!(
        !ws.path.join(LEGACY_WORKSPACE_DIR).exists(),
        "the legacy folder was created"
    );
    drop(open);
    rt.block_on(hub.close(&ws.id));
    assert!(!ws.path.join(LEGACY_WORKSPACE_DIR).exists());
}

/// D4: with either lock held by someone else the open is read-only, and the other lock is not
/// kept.
#[test] // spec: ARCH-028
fn either_lock_held_elsewhere_makes_the_open_read_only() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_legacy_workspace("plugin era"));
    fsops::ensure_dir(&ws.path.join(LEGACY_WORKSPACE_DIR)).unwrap();
    let rt = runtime();
    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);

    for held in [core_lock(&ws), legacy_lock(&ws)] {
        let other = old_core_takes(&held).expect("the other holder's lock");
        let open = rt.block_on(hub.open(&ws)).expect("the open");
        assert_eq!(open.access, Access::ReadOnly, "{}", held.display());
        // The lock this core could take is not kept.
        let free = if held == core_lock(&ws) {
            legacy_lock(&ws)
        } else {
            core_lock(&ws)
        };
        assert!(
            fsops::lock_exclusive(&free).unwrap().is_some(),
            "{} is kept by a read-only open",
            free.display()
        );
        drop(open);
        rt.block_on(hub.close(&ws.id));
        drop(other);
    }

    // Both free: the open holds both.
    let open = rt.block_on(hub.open(&ws)).expect("the open");
    assert_eq!(open.access, Access::ReadWrite);
}

/// Deletes `path` as an outside program would.
#[allow(clippy::disallowed_methods)]
fn delete_outside(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

/// Renames `from` to `to` as an outside program would.
#[allow(clippy::disallowed_methods)]
fn rename_outside(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// A held `core.lock` is opened without `FILE_SHARE_DELETE`: while it is held its name can't
/// be deleted or renamed away (error 32), so a second core always finds the held lock and
/// serves read-only. Opening it again for the lock check still works.
#[test] // spec: ARCH-028
fn core_lock_cannot_be_deleted_or_renamed_while_held() {
    use lil_core::workspace::take_core_locks;
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_legacy_workspace("plugin era"));
    fsops::ensure_dir(&ws.path.join(LEGACY_WORKSPACE_DIR)).unwrap();
    let held = take_core_locks(&ws.path, &SystemProbe)
        .expect("the locks open")
        .expect("both locks are free");

    for (label, path) in [
        (".lilith\\core.lock", core_lock(&ws)),
        (".worldwright\\core.lock", legacy_lock(&ws)),
    ] {
        let deleted = delete_outside(&path);
        assert_eq!(
            deleted.as_ref().map_err(std::io::Error::raw_os_error),
            Err(Some(32)),
            "deleting {label}: {deleted:?}"
        );
        let renamed = rename_outside(&path, &path.with_file_name("core.lock.moved"));
        assert_eq!(
            renamed.as_ref().map_err(std::io::Error::raw_os_error),
            Err(Some(32)),
            "renaming {label}: {renamed:?}"
        );
        assert!(path.is_file(), "{label} is gone");
        assert!(!path.with_file_name("core.lock.moved").exists(), "{label}");
    }
    // A second core opens both files and finds the lock held.
    assert!(
        take_core_locks(&ws.path, &SystemProbe)
            .expect("the locks open")
            .is_none(),
        "a second core took the locks"
    );
    drop(held);
    assert!(take_core_locks(&ws.path, &SystemProbe).unwrap().is_some());
}

/// Opens `path` the way another program can: with `DELETE` access and every share mode.
#[allow(clippy::disallowed_methods)]
fn open_with_delete_access(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    const DELETE: u32 = 0x0001_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    std::fs::OpenOptions::new()
        .access_mode(DELETE | FILE_READ_ATTRIBUTES)
        .share_mode(0x7) // read, write and delete
        .open(path)
        .expect("the outside handle")
}

/// A handle elsewhere with delete access makes the lock open fail with error 32 (the lock is
/// opened without delete sharing). That counts as held elsewhere: the workspace opens
/// read-only, never `store.io`, and the other lock isn't kept.
#[test] // spec: ARCH-028
fn delete_access_held_elsewhere_makes_the_open_read_only() {
    use lil_core::workspace::take_core_locks;
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_legacy_workspace("plugin era"));
    fsops::ensure_dir(&ws.path.join(LEGACY_WORKSPACE_DIR)).unwrap();
    // Both lock files exist (an earlier open made them).
    drop(take_core_locks(&ws.path, &SystemProbe).unwrap().unwrap());
    let rt = runtime();
    let states = registry(&env.env);
    let (hub, _) = hub(&env.env, &states);
    for path in [core_lock(&ws), legacy_lock(&ws)] {
        let outside = open_with_delete_access(&path);
        assert!(
            take_core_locks(&ws.path, &SystemProbe)
                .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()))
                .is_none(),
            "{}",
            path.display()
        );
        let open = rt
            .block_on(hub.open(&ws))
            .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        assert_eq!(open.access, Access::ReadOnly, "{}", path.display());
        drop(open);
        rt.block_on(hub.close(&ws.id));
        drop(outside);
    }
    assert!(take_core_locks(&ws.path, &SystemProbe).unwrap().is_some());
}

/// The real probe, except that `identity` of `fail_at` is access denied (error 5).
#[derive(Debug)]
struct FailingAt {
    fail_at: PathBuf,
}

impl lil_platform::fs::Probe for FailingAt {
    fn identity(&self, path: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        if path == self.fail_at {
            return Err(std::io::Error::from_raw_os_error(5));
        }
        SystemProbe.identity(path)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }
}

/// D4 fails closed: when whether the legacy folder exists can't be read (here access
/// denied), the open is never read-write, and the new lock isn't kept.
#[test] // spec: ARCH-028
fn unreadable_legacy_folder_makes_the_open_read_only() {
    use lil_core::workspace::take_core_locks;
    let env = TestEnv::new();
    let ws = workspace_ref(&env.make_legacy_workspace("plugin era"));
    fsops::ensure_dir(&ws.path.join(LEGACY_WORKSPACE_DIR)).unwrap();
    let probe = FailingAt {
        fail_at: ws.path.join(LEGACY_WORKSPACE_DIR),
    };
    let taken = take_core_locks(&ws.path, &probe).expect("not an open error");
    assert!(
        taken.is_none(),
        "an unreadable legacy folder gave read-write"
    );
    assert!(lock_is_free(&ws), "the new lock was kept");
    assert!(take_core_locks(&ws.path, &SystemProbe).unwrap().is_some());
}

// ---------------------------------------------------------------------------------------------
// A dispatcher over a hub.
// ---------------------------------------------------------------------------------------------

fn dispatcher(
    env: &Env,
    states: &Arc<StateRegistry>,
    events: &Arc<EventBus>,
    hub: &Arc<WorkspaceHub>,
) -> lil_core::tools::Dispatcher {
    use lil_core::tools::{PipelineHooks, TestGates, core_registry};
    let clock: Arc<dyn Clock> = FakeClock::new();
    let probe: Arc<dyn lil_platform::fs::Probe> = Arc::new(SystemProbe);
    lil_core::tools::Dispatcher::new(
        core_registry(
            true,
            &Arc::new(TestGates::new()),
            states,
            hub,
            env,
            &probe,
            Arc::default(),
        ),
        PipelineHooks::plan2(),
        states.clone(),
        events.clone(),
        hub.clone(),
        Arc::new(SeqIds::new()),
        clock.clone(),
        lil_core::Lifecycle::new(clock, lil_core::lifecycle::IDLE_EXIT_AFTER),
    )
}

fn session(workspace: Option<WorkspaceRef>) -> Arc<lil_core::session::Session> {
    Arc::new(lil_core::session::Session {
        id: "s_1".into(),
        caller: lil_protocol::CallerKind::Cli,
        non_interactive: false,
        workspace,
        capabilities: Vec::new(),
        client_pid: 1,
        profile_id: None,
    })
}

fn params(name: &str, args: Value, key: Option<&str>) -> lil_protocol::ToolCallParams {
    let Value::Object(arguments) = args else {
        panic!("arguments must be an object");
    };
    lil_protocol::ToolCallParams {
        name: name.into(),
        arguments,
        idempotency_key: key.map(str::to_string),
        trace_id: None,
    }
}

/// `workspace.doctor`'s offers run only for the person (owner question 1): a verified `mcp`
/// session (Claude Code) and an engine's `assistant` session get `caller.not_allowed`, and the
/// legacy workspace file stays where it is.
#[test] // spec: ARCH-047, ARCH-052 (owner question 1)
fn an_agent_caller_cannot_run_a_doctor_offer() {
    use lil_platform::names::{LEGACY_WORKSPACE_FILE, WORKSPACE_FILE};
    let env = TestEnv::new();
    let ws_path = env.make_legacy_workspace("plugin era");
    env.register_workspace(&ws_path, true);
    let ws = workspace_ref(&ws_path);
    let states = registry(&env.env);
    let (hub, events) = hub(&env.env, &states);
    let dispatcher = dispatcher(&env.env, &states, &events, &hub);
    let rt = runtime();
    for (n, caller) in [
        lil_protocol::CallerKind::Mcp,
        lil_protocol::CallerKind::Assistant,
    ]
    .into_iter()
    .enumerate()
    {
        let session = Arc::new(lil_core::session::Session {
            caller: caller.clone(),
            ..(*session(Some(ws.clone()))).clone()
        });
        let out = rt.block_on(dispatcher.call(
            session,
            params(
                "workspace.doctor",
                json!({"fix": true, "offers": ["legacy_names"]}),
                Some(&format!("agent-offer-{n}")),
            ),
        ));
        let lil_protocol::Outcome::Error { error } = &out else {
            panic!("{caller:?} ran the offer: {out:?}");
        };
        assert_eq!(error.code, "caller.not_allowed", "{caller:?}: {error:?}");
        assert!(ws_path.join(LEGACY_WORKSPACE_FILE).is_file(), "{caller:?}");
        assert!(!ws_path.join(WORKSPACE_FILE).exists(), "{caller:?}");
    }
    drop(dispatcher);
    rt.block_on(hub.close_all());
    states.close();
}
