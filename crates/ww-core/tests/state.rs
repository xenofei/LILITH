//! The `state` module (CORE-004, CORE-005, QA-062): check-before-write opening, migrations,
//! idempotency rows, where the databases live, and how the core's startup reacts to a newer
//! or damaged suite `state.db` (spec Appendix D exit codes 5 and 6).
//!
//! Wall-clock durations here are failure deadlines only, never measurements (QA-031). Files
//! are written through `store::fsops`, like everything else in `ww-core` (ARCH-112).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OpenFlags};
use ww_core::state::{
    IdemBegin, SUITE_MIGRATIONS, StateDb, StateError, StateRegistry, StateScope,
    WORKSPACE_MIGRATIONS,
};
use ww_core::store::fsops;
use ww_core::workspace::WorkspaceRef;
use ww_core::{CoreConfig, CoreExit, start, start_in_thread};
use ww_platform::Env;
use ww_platform::process::SystemInspector;
use ww_testkit::{CoreFixture, FakeClock, SeqIds, TestEnv, sha256_file};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
/// Thirty days, the age after which `done` rows are pruned.
const THIRTY_DAYS: i64 = 30 * 24 * 60 * 60;

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs()
        .try_into()
        .expect("fits i64")
}

fn known() -> i64 {
    i64::try_from(SUITE_MIGRATIONS.len()).expect("fits i64")
}

/// Opens `path` read-only, outside the core (ruling P8).
fn read_only(path: &Path) -> Connection {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|e| panic!("cannot open {} read-only: {e}", path.display()))
}

/// The idempotency rows of `path` as `(scope, key, state)`, read-only.
fn rows(path: &Path) -> Vec<(String, String, String)> {
    let db = read_only(path);
    let mut stmt = db
        .prepare("SELECT scope, key, state FROM idempotency ORDER BY scope, key")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// An in-process core configuration with the fake clock and ids (QA-031).
fn config(env: &TestEnv) -> CoreConfig {
    CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        inspector: Arc::new(SystemInspector),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    }
}

#[test] // spec: CORE-004
fn fresh_db_is_migrated_with_wal_full_sync_and_busy_timeout() {
    let env = TestEnv::with_label("Ünïcödé space");
    let path = env.root().join("fresh").join("state.db");
    assert!(!path.parent().unwrap().exists());

    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("open a fresh db");
    let d = runtime().block_on(db.diagnostics()).expect("diagnostics");
    assert_eq!(d.synchronous, 2, "synchronous=FULL");
    assert_eq!(d.journal_mode, "wal");
    assert_eq!(d.busy_timeout_ms, 5000);
    assert_eq!(d.user_version, known());
    assert_eq!(db.schema_version(), known());
    assert_eq!(db.path(), path);
    assert!(path.is_file());

    let ws = StateDb::open(
        &env.root().join("ws").join("state.db"),
        StateScope::Workspace,
        no_fatal(),
    )
    .expect("open a fresh workspace db");
    let expected = i64::try_from(WORKSPACE_MIGRATIONS.len()).unwrap();
    assert_eq!(ws.schema_version(), expected);
}

#[test] // spec: QA-062
fn newer_schema_is_refused_and_file_untouched() {
    let env = TestEnv::new();
    for wal in [false, true] {
        let path = env
            .root()
            .join(if wal { "wal" } else { "rollback" })
            .join("state.db");
        env.write_state_db_version(&path, known() + 1, wal);
        let before = sha256_file(&path);

        let err = StateDb::open(&path, StateScope::Suite, no_fatal())
            .expect_err("a newer database must be refused");
        assert_eq!(
            err,
            StateError::TooNew {
                found: known() + 1,
                known: known()
            },
            "wal={wal}"
        );
        assert_eq!(
            sha256_file(&path),
            before,
            "wal={wal}: the file was changed"
        );
        let mode: String = read_only(&path)
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, if wal { "wal" } else { "delete" }, "wal={wal}");
    }
}

#[test] // spec: CORE-004
fn corrupt_file_is_refused_and_kept() {
    let env = TestEnv::new();
    let path = env.root().join("damaged").join("state.db");
    fsops::ensure_dir(path.parent().unwrap()).unwrap();
    fsops::write_file_atomic(&path, &[0xAB_u8; 4096]).unwrap();
    let before = sha256_file(&path);

    let err = StateDb::open(&path, StateScope::Suite, no_fatal())
        .expect_err("a damaged database must be refused");
    assert!(matches!(err, StateError::Corrupt(_)), "{err:?}");
    assert_eq!(sha256_file(&path), before, "the file was changed");
    assert!(path.is_file(), "the file must be kept");
}

#[test] // spec: CORE-004
fn reopen_at_current_version_is_a_no_op() {
    let env = TestEnv::new();
    let path = env.root().join("state.db");
    drop(StateDb::open(&path, StateScope::Suite, no_fatal()).expect("first open"));
    let before = sha256_file(&path);

    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("reopen");
    assert_eq!(db.schema_version(), known());
    drop(db);
    assert_eq!(sha256_file(&path), before, "reopening changed the file");
}

#[test] // spec: CORE-004
fn wal_left_by_a_crashed_core_is_recovered() {
    let env = TestEnv::new();
    let live = env.root().join("live").join("state.db");
    let db = StateDb::open(&live, StateScope::Suite, no_fatal()).expect("open");
    runtime().block_on(async {
        assert_eq!(
            db.idem_begin("cli|none", "k", "h", now_secs())
                .await
                .unwrap(),
            IdemBegin::Fresh
        );
        db.idem_finish("cli|none", "k", Some("{}".into()))
            .await
            .unwrap();
    });

    // A crash: the database and its WAL as they are while the core still runs (nothing
    // checkpointed), without the shared-memory index.
    let crashed = env.root().join("crashed").join("state.db");
    fsops::ensure_dir(crashed.parent().unwrap()).unwrap();
    for suffix in ["", "-wal"] {
        let from = PathBuf::from(format!("{}{suffix}", live.display()));
        let to = PathBuf::from(format!("{}{suffix}", crashed.display()));
        let bytes = std::fs::read(&from).unwrap();
        if suffix == "-wal" {
            assert!(!bytes.is_empty(), "the WAL holds the writes");
        }
        fsops::write_file_atomic(&to, &bytes).unwrap();
    }
    drop(db);

    let db = StateDb::open(&crashed, StateScope::Suite, no_fatal())
        .expect("a database left by a crashed core opens");
    assert_eq!(db.schema_version(), known());
    assert_eq!(
        runtime()
            .block_on(db.idem_begin("cli|none", "k", "h", now_secs()))
            .unwrap(),
        IdemBegin::Replay("{}".into())
    );
}

#[test] // spec: CORE-016
fn idempotency_lifecycle() {
    let env = TestEnv::new();
    let path = env.root().join("state.db");
    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("open");
    let now = now_secs();
    let outcome = r#"{"status":"ok","structured":{"label":"Ünïcödé space"},"artifacts":[],"summary_text":"done"}"#;
    runtime().block_on(async {
        assert_eq!(
            db.idem_begin("cli|none", "k1", "h1", now).await.unwrap(),
            IdemBegin::Fresh
        );
        db.idem_finish("cli|none", "k1", Some(outcome.to_string()))
            .await
            .unwrap();
        match db.idem_begin("cli|none", "k1", "h1", now).await.unwrap() {
            IdemBegin::Replay(stored) => assert_eq!(stored.as_bytes(), outcome.as_bytes()),
            other => panic!("expected Replay, got {other:?}"),
        }
        assert_eq!(
            db.idem_begin("cli|none", "k1", "h2", now).await.unwrap(),
            IdemBegin::Conflict
        );

        // The same key in another scope is another row.
        assert_eq!(
            db.idem_begin("mcp|none", "k1", "h2", now).await.unwrap(),
            IdemBegin::Fresh
        );
        assert_eq!(
            db.idem_begin("mcp|none", "k1", "h2", now).await.unwrap(),
            IdemBegin::InProgress
        );
        assert_eq!(
            db.idem_begin("mcp|none", "k1", "h3", now).await.unwrap(),
            IdemBegin::Conflict
        );
        db.idem_finish("mcp|none", "k1", None).await.unwrap();
        assert_eq!(
            db.idem_begin("mcp|none", "k1", "h3", now).await.unwrap(),
            IdemBegin::Fresh
        );
    });
    assert_eq!(
        rows(&path),
        vec![
            ("cli|none".into(), "k1".into(), "done".into()),
            ("mcp|none".into(), "k1".into(), "running".into()),
        ]
    );
}

#[test] // spec: CORE-016
fn running_rows_are_released_on_reopen() {
    let env = TestEnv::new();
    let path = env.root().join("state.db");
    let now = now_secs();
    {
        let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("open");
        runtime().block_on(async {
            assert_eq!(
                db.idem_begin("cli|none", "running", "h", now)
                    .await
                    .unwrap(),
                IdemBegin::Fresh
            );
            for (key, at) in [("old", now - THIRTY_DAYS - 60), ("recent", now - 60)] {
                assert_eq!(
                    db.idem_begin("cli|none", key, "h", at).await.unwrap(),
                    IdemBegin::Fresh
                );
                db.idem_finish("cli|none", key, Some("{}".into()))
                    .await
                    .unwrap();
            }
        });
    }
    assert_eq!(rows(&path).len(), 3);

    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("reopen");
    // The call that died with the earlier core is released. Old outcomes wait for the next
    // `idem_begin`, which prunes by the caller's clock.
    assert_eq!(
        rows(&path),
        vec![
            ("cli|none".into(), "old".into(), "done".into()),
            ("cli|none".into(), "recent".into(), "done".into()),
        ]
    );
    runtime().block_on(async {
        assert_eq!(
            db.idem_begin("cli|none", "running", "h", now)
                .await
                .unwrap(),
            IdemBegin::Fresh
        );
        // The 30-day-old outcome is gone, so its key is fresh again.
        assert_eq!(
            db.idem_begin("cli|none", "old", "h", now).await.unwrap(),
            IdemBegin::Fresh
        );
        assert_eq!(
            db.idem_begin("cli|none", "recent", "h", now).await.unwrap(),
            IdemBegin::Replay("{}".into())
        );
    });
}

#[test] // spec: CORE-016
fn prune_uses_the_callers_clock() {
    // Rows stamped by a clock far ahead of the system clock (a `FakeClock` starts in 2027)
    // are pruned only when that same clock says they are 30 days old.
    let env = TestEnv::new();
    let path = env.root().join("state.db");
    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("open");
    let fake_now = now_secs() + 400 * 24 * 60 * 60;
    runtime().block_on(async {
        assert_eq!(
            db.idem_begin("cli|none", "k", "h", fake_now).await.unwrap(),
            IdemBegin::Fresh
        );
        db.idem_finish("cli|none", "k", Some("{}".into()))
            .await
            .unwrap();
        // 29 days later by the same clock: still replayed.
        let later = fake_now + THIRTY_DAYS - 24 * 60 * 60;
        assert_eq!(
            db.idem_begin("cli|none", "k", "h", later).await.unwrap(),
            IdemBegin::Replay("{}".into())
        );
        // 31 days later: pruned, so the key is fresh.
        let much_later = fake_now + THIRTY_DAYS + 24 * 60 * 60;
        assert_eq!(
            db.idem_begin("cli|none", "k", "h", much_later)
                .await
                .unwrap(),
            IdemBegin::Fresh
        );
    });
    drop(db);
    // Reopening by the system clock prunes nothing the fake clock stamped.
    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("reopen");
    runtime().block_on(async {
        assert_eq!(
            db.idem_begin("cli|none", "j", "h", fake_now).await.unwrap(),
            IdemBegin::Fresh
        );
        db.idem_finish("cli|none", "j", Some("{}".into()))
            .await
            .unwrap();
    });
    drop(db);
    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("reopen");
    drop(db);
    assert_eq!(
        rows(&path),
        vec![("cli|none".into(), "j".into(), "done".into())],
        "the running row k is released on reopen; j is kept"
    );
}

/// `ws` as a resolved workspace: its canonical path without `\\?\`, and its id.
fn workspace_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    let path = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
    WorkspaceRef {
        id: Env::workspace_hash(&canonical),
        path,
    }
}

#[test] // spec: CORE-005
fn databases_live_where_core_005_says() {
    let env = TestEnv::with_label("Ünïcödé space");
    let reg = StateRegistry::open(&env.env, no_fatal()).expect("open the registry");
    assert_eq!(reg.suite().path(), env.env.suite_state_db());
    assert_eq!(
        env.env.suite_state_db(),
        env.env.local_dir().join("state.db")
    );
    assert!(env.env.suite_state_db().is_file());

    let ws = workspace_ref(&env.make_workspace("alpha"));
    let rt = runtime();
    let a = rt
        .block_on(reg.for_workspace(&ws, ww_core::workspace::Access::ReadWrite))
        .expect("the workspace db");
    let expected = env
        .env
        .workspace_local_dir(&ws.path)
        .expect("workspace_local_dir")
        .join("state.db");
    assert_eq!(a.path(), expected);
    assert!(expected.is_file());
    // Cached: one actor per database (QA-012).
    let again = rt
        .block_on(reg.for_workspace(&ws, ww_core::workspace::Access::ReadWrite))
        .expect("the workspace db again");
    assert!(Arc::ptr_eq(&a, &again));
}

#[test] // spec: ARCH-026, CORE-016
fn losing_core_never_touches_state_db() {
    let env = TestEnv::new();
    let winner = start_in_thread(config(&env));
    let path = env.env.suite_state_db();
    let state = winner.core().state();
    assert_eq!(state.suite().path(), path);
    assert_eq!(
        runtime()
            .block_on(state.suite().idem_begin("cli|none", "k", "h", now_secs()))
            .unwrap(),
        IdemBegin::Fresh
    );
    let running = vec![("cli|none".into(), "k".into(), "running".into())];
    assert_eq!(rows(&path), running);
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    // The row is in the WAL (nothing checkpointed yet): a loser that touched the database
    // would change the `-wal` file too.
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    let wal_mtime = std::fs::metadata(&wal).unwrap().modified().unwrap();

    // A second in-process `start` on the same env loses the pipe and touches nothing.
    let lost = runtime()
        .block_on(start(config(&env)))
        .expect_err("a second core must not start");
    assert_eq!(lost, CoreExit::AlreadyRunning);

    // So does a real `ww-core` process: exit 3, before its log folder exists.
    let loser = CoreFixture::spawn_raw(&env);
    assert_eq!(loser.wait_exit(DEADLINE), Some(3));
    assert!(
        !env.env.logs_dir().exists(),
        "the losing core created {}",
        env.env.logs_dir().display()
    );

    assert_eq!(rows(&path), running, "the winner's running row was deleted");
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        mtime,
        "the losing cores changed {}",
        path.display()
    );
    assert_eq!(
        std::fs::metadata(&wal).unwrap().modified().unwrap(),
        wal_mtime,
        "the losing cores changed {}",
        wal.display()
    );
}

#[test] // spec: QA-062, CORE-004
fn startup_maps_state_errors_to_exit_codes() {
    // Newer → 6.
    let env = TestEnv::new();
    let db = env.env.suite_state_db();
    env.write_state_db_version(&db, known() + 1, true);
    let before = sha256_file(&db);
    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(core.wait_exit(DEADLINE), Some(6), "a newer state.db");
    assert_eq!(sha256_file(&db), before);
    assert_log_names(&env, "newer");

    // Damaged → 5.
    let env = TestEnv::new();
    let db = env.env.suite_state_db();
    fsops::ensure_dir(db.parent().unwrap()).unwrap();
    fsops::write_file_atomic(&db, &[0xAB_u8; 4096]).unwrap();
    let before = sha256_file(&db);
    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(core.wait_exit(DEADLINE), Some(5), "a damaged state.db");
    assert_eq!(sha256_file(&db), before);
    assert_log_names(&env, "damaged");

    // Anything else → 2: here a folder where the database should be.
    let env = TestEnv::new();
    fsops::ensure_dir(&env.env.suite_state_db()).unwrap();
    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(core.wait_exit(DEADLINE), Some(2), "an unopenable state.db");
    assert_log_names(&env, "cannot open");
}

/// Some log file of `env`'s core names the suite `state.db` in a line containing `what`.
fn assert_log_names(env: &TestEnv, what: &str) {
    let logs = env.env.logs_dir();
    let text: String = std::fs::read_dir(&logs)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", logs.display()))
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert!(
        text.lines()
            .any(|l| l.contains("state.db") && l.contains(what)),
        "no log line names state.db with `{what}`:\n{text}"
    );
}

#[test] // spec: QA-062, CORE-004
fn newer_schema_is_refused_and_untouched_with_a_live_wal() {
    let env = TestEnv::new();
    let path = env.root().join("live-wal").join("state.db");
    env.write_state_db_version(&path, known() + 1, true);
    // Another connection keeps committed frames in the WAL, uncheckpointed, while the core
    // checks the file: the newer `user_version` is only in those frames.
    let live = Connection::open(&path).expect("a live connection");
    live.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    live.execute_batch("CREATE TABLE held (x); INSERT INTO held VALUES (1);")
        .unwrap();
    live.pragma_update(None, "user_version", known() + 2)
        .unwrap();
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    assert!(
        std::fs::metadata(&wal).unwrap().len() > 0,
        "the WAL holds the frames"
    );
    let (db_before, wal_before) = (sha256_file(&path), sha256_file(&wal));

    let err = StateDb::open(&path, StateScope::Suite, no_fatal())
        .expect_err("a newer database must be refused");
    assert_eq!(
        err,
        StateError::TooNew {
            found: known() + 2,
            known: known()
        }
    );
    assert_eq!(sha256_file(&path), db_before, "the database file changed");
    assert_eq!(sha256_file(&wal), wal_before, "the WAL changed");
    let held: i64 = live
        .query_row("SELECT count(*) FROM held", [], |r| r.get(0))
        .expect("the live connection still sees its frames");
    assert_eq!(held, 1);
    drop(live);
}

#[test] // spec: CORE-016, QA-012
fn replay_of_a_null_outcome_is_core_bug() {
    let env = TestEnv::new();
    let path = env.env.suite_state_db();
    drop(StateDb::open(&path, StateScope::Suite, no_fatal()).expect("create the suite db"));
    // A `done` row whose outcome is NULL (a damaged row): `idem_begin` reads it as `Replay("")`.
    let args = serde_json::json!({"label": "null"});
    let hash =
        ww_protocol::args_hash(&serde_json::json!({"name": "test.write", "arguments": args}));
    let fake_now = 1_800_000_000_i64; // FakeClock's UTC start, so the row isn't pruned.
    {
        let db = Connection::open(&path).unwrap();
        db.execute(
            "INSERT INTO idempotency (scope, key, args_hash, state, outcome, created_utc)
             VALUES ('cli|none', 'k-null', ?1, 'done', NULL, ?2)",
            rusqlite::params![hash, fake_now],
        )
        .unwrap();
    }
    {
        let db = StateDb::open(&path, StateScope::Suite, no_fatal()).unwrap();
        assert_eq!(
            runtime()
                .block_on(db.idem_begin("cli|none", "k-null", &hash, fake_now))
                .unwrap(),
            IdemBegin::Replay(String::new())
        );
    }

    let _core = start_in_thread(config(&env));
    let mut pipe = ww_testkit::RawPipe::connect(&env);
    let hello = pipe.hello(serde_json::json!({}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let replayed = pipe.tool("test.write", args, Some("k-null"));
    assert_eq!(replayed["status"], "error", "{replayed}");
    assert_eq!(replayed["error"]["code"], "core.bug", "{replayed}");
    // The tool did not run: a fresh key is its first execution.
    let fresh = pipe.tool(
        "test.write",
        serde_json::json!({"label": "null"}),
        Some("k-fresh"),
    );
    assert_eq!(fresh["structured"]["executions"], 1, "{fresh}");
}

#[test] // spec: CORE-016
fn idem_finish_on_a_missing_row_is_an_error() {
    let env = TestEnv::new();
    let path = env.root().join("state.db");
    let db = StateDb::open(&path, StateScope::Suite, no_fatal()).expect("open");
    let now = now_secs();
    runtime().block_on(async {
        for outcome in [None, Some("{}".to_string())] {
            let err = db
                .idem_finish("cli|none", "never-begun", outcome.clone())
                .await
                .expect_err("finishing a row that doesn't exist is an error");
            let StateError::Io(why) = &err else {
                panic!("expected StateError::Io, got {err:?}");
            };
            assert!(why.starts_with("no idempotency row"), "{why}");
        }
        // A row that is `done` already isn't `running` either.
        assert_eq!(
            db.idem_begin("cli|none", "k", "h", now).await.unwrap(),
            IdemBegin::Fresh
        );
        db.idem_finish("cli|none", "k", Some("{}".into()))
            .await
            .unwrap();
        assert!(matches!(
            db.idem_finish("cli|none", "k", None).await,
            Err(StateError::Io(_))
        ));
    });
    assert_eq!(
        rows(&path),
        vec![("cli|none".into(), "k".into(), "done".into())]
    );
}
