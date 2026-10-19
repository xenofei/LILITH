//! Document holds (APP-103, APP-104, APP-105; Appendix F "Holds"): the first holder wins, a
//! hold ends with its connection, a core hold lasts one operation, and `doc.list_open` lists
//! them. The marker rows in the workspace `state.db` are emptied when a core opens it.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lil_core::docs::{HoldOwner, Holds, register_doc_tools};
use lil_core::events::EventBus;
use lil_core::session::Session;
use lil_core::state::{HoldMarker, StateDb, StateScope};
use lil_core::tools::Registry;
use lil_core::workspace::WorkspaceRef;
use lil_protocol::CallerKind;
use lil_testkit::{CoreFixture, FakeClock, RawPipe, TestApp, TestEnv, ensure_built};
use serde_json::json;

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

fn app(conn: u64, name: &str) -> HoldOwner {
    HoldOwner::Connection(conn, name.to_string())
}

fn doc_path() -> &'static Path {
    Path::new(r"C:\scratch\hero.lildoc")
}

fn open_state(env: &TestEnv) -> Arc<StateDb> {
    let file = env.root().join("state").join("ws.db");
    Arc::new(
        StateDb::open(
            &file,
            StateScope::Workspace,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("a workspace state.db"),
    )
}

/// Waits (by yielding, never by sleeping for a fixed time) until the markers are `want`.
async fn markers_become(state: &StateDb, want: &[&str]) {
    let ids = || async {
        state
            .hold_markers()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.doc_id)
            .collect::<Vec<_>>()
    };
    tokio::time::timeout(DEADLINE, async {
        while ids().await != want {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the markers did not become {want:?}"));
}

#[test] // spec: APP-103
fn first_holder_wins_and_others_get_held_elsewhere() {
    let holds = Holds::new(FakeClock::new());
    let first = holds.acquire("d_1", doc_path(), app(1, "image")).unwrap();

    for other in [app(2, "audio"), HoldOwner::Core] {
        let refused = holds.acquire("d_1", doc_path(), other).unwrap_err();
        assert_eq!(refused.code, "doc.held_elsewhere");
        assert!(refused.message.contains("image"), "{}", refused.message);
    }
    // Another document is free, and the list names each holder.
    let second = holds.acquire("d_2", doc_path(), app(2, "audio")).unwrap();
    let held: Vec<(String, String)> = holds
        .list()
        .into_iter()
        .map(|h| (h.doc_id, h.held_by))
        .collect();
    assert_eq!(
        held,
        [
            ("d_1".to_string(), "image".to_string()),
            ("d_2".to_string(), "audio".to_string())
        ]
    );

    // The same connection may take its own hold again; it lasts until the last guard goes.
    let again = holds.acquire("d_1", doc_path(), app(1, "image")).unwrap();
    drop(first);
    assert!(holds.acquire("d_1", doc_path(), app(2, "audio")).is_err());
    drop(again);
    let taken = holds.acquire("d_1", doc_path(), app(2, "audio")).unwrap();
    drop((second, taken));
    assert!(holds.list().is_empty());
}

#[tokio::test] // spec: APP-104
async fn holds_end_with_the_connection_and_markers_clear_at_startup() {
    let env = TestEnv::new();
    let state = open_state(&env);
    let holds = Holds::new(FakeClock::new());

    let guard = holds
        .acquire_marked(&state, "ws_a", "d_1", doc_path(), app(7, "image"))
        .await
        .unwrap();
    let rows = state.hold_markers().await.unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].doc_id.as_str(), rows[0].holder.as_str()),
        ("d_1", "ui:image")
    );
    // Another connection's hold is not touched when connection 7 ends.
    let other = holds.acquire("d_2", doc_path(), app(8, "audio")).unwrap();
    holds.release_connection(7);
    let held: Vec<String> = holds.list().into_iter().map(|h| h.doc_id).collect();
    assert_eq!(held, ["d_2"]);
    markers_become(&state, &[]).await;

    // Connection 8 takes the document; the stale guard of connection 7's hold, dropped now,
    // does not end it.
    let mine = holds.acquire("d_1", doc_path(), app(8, "audio")).unwrap();
    drop(guard);
    assert!(holds.acquire("d_1", doc_path(), app(7, "image")).is_err());
    drop((mine, other));

    // A core that was killed leaves its markers behind; the next core to open the workspace
    // database empties them.
    state
        .set_hold_marker(&HoldMarker {
            doc_id: "d_9".into(),
            path: r"C:\scratch\left.lildoc".into(),
            holder: "ui:image".into(),
            since_utc: 1_700_000_000,
            token: 99,
        })
        .await
        .unwrap();
    assert_eq!(state.hold_markers().await.unwrap().len(), 1);
    state.close();
    let reopened = open_state(&env);
    assert!(reopened.hold_markers().await.unwrap().is_empty());
}

#[test] // spec: APP-105
fn core_hold_spans_one_operation() {
    let holds = Holds::new(FakeClock::new());
    let operation = holds.acquire("d_1", doc_path(), HoldOwner::Core).unwrap();
    let refused = holds
        .acquire("d_1", doc_path(), app(1, "image"))
        .unwrap_err();
    assert_eq!(refused.code, "doc.held_elsewhere");
    assert!(refused.message.contains("core"), "{}", refused.message);
    drop(operation);
    let _image = holds.acquire("d_1", doc_path(), app(1, "image")).unwrap();
}

#[tokio::test] // spec: APP-103, APP-105
async fn doc_list_open_lists_holds() {
    let env = TestEnv::new();
    let holds = Holds::new(FakeClock::new());
    let mut reg = Registry::new();
    register_doc_tools(&mut reg, &holds);
    let events = Arc::new(EventBus::new(FakeClock::new()));
    let ctx = common::ctx(&env, CallerKind::Cli, &events);

    let empty = common::call(&reg, &ctx, "doc.list_open", json!({}))
        .await
        .unwrap();
    assert_eq!(empty, json!({"documents": []}));
    let _image = holds.acquire("d_1", doc_path(), app(1, "image")).unwrap();
    let _core = holds
        .acquire("d_2", Path::new(r"C:\scratch\b.lildoc"), HoldOwner::Core)
        .unwrap();
    let listed = common::call(&reg, &ctx, "doc.list_open", json!({}))
        .await
        .unwrap();
    assert_eq!(
        listed,
        json!({"documents": [
            {"doc_id": "d_1", "path": r"C:\scratch\hero.lildoc", "held_by": "image"},
            {"doc_id": "d_2", "path": r"C:\scratch\b.lildoc", "held_by": "core"},
        ]})
    );
    let refused = common::call(&reg, &ctx, "doc.list_open", json!({"x": 1}))
        .await
        .unwrap_err();
    assert_eq!(refused.code, "params.invalid");
}

/// A hold's marker is cleared by a task spawned when its guard drops. That task must not delete
/// the marker of the next holder of the same document, whichever of the two writes lands first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // spec: APP-104
async fn a_dropped_holds_marker_clear_never_deletes_the_next_holders_marker() {
    let env = TestEnv::new();
    let state = open_state(&env);
    let holds = Holds::new(FakeClock::new());
    for round in 0..100 {
        let first = holds
            .acquire_marked(&state, "ws_a", "d_1", doc_path(), app(1, "image"))
            .await
            .unwrap();
        drop(first);
        let second = holds
            .acquire_marked(&state, "ws_a", "d_1", doc_path(), app(2, "audio"))
            .await
            .unwrap();
        // The state actor runs jobs in order: these round trips let the first hold's clear,
        // however late it was spawned, run before the check.
        for _ in 0..8 {
            tokio::task::yield_now().await;
            state.hold_markers().await.unwrap();
        }
        let rows = state.hold_markers().await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "round {round}: the live hold has no marker: {rows:?}"
        );
        assert_eq!(rows[0].holder, "ui:audio", "round {round}");
        drop(second);
        markers_become(&state, &[]).await;
    }
}

fn workspace(id: &str) -> WorkspaceRef {
    WorkspaceRef {
        id: id.to_string(),
        path: std::path::PathBuf::from(r"C:\scratch\ws"),
    }
}

#[tokio::test] // spec: APP-103
async fn doc_list_open_is_scoped_to_the_sessions_workspace() {
    let env = TestEnv::new();
    let state = open_state(&env);
    let holds = Holds::new(FakeClock::new());
    let a = "aaaaaaaaaaaaaaaa";
    let b = "bbbbbbbbbbbbbbbb";
    let _in_a = holds
        .acquire_marked(
            &state,
            a,
            "d_a",
            Path::new(r"C:\ws_a\x.lildoc"),
            app(1, "image"),
        )
        .await
        .unwrap();
    let _in_b = holds
        .acquire_marked(
            &state,
            b,
            "d_b",
            Path::new(r"C:\ws_b\y.lildoc"),
            app(2, "image"),
        )
        .await
        .unwrap();
    let _loose = holds
        .acquire("d_loose", Path::new(r"C:\loose\z.lildoc"), app(3, "image"))
        .unwrap();
    let ids = |views: Vec<lil_core::docs::HoldView>| -> Vec<String> {
        views.into_iter().map(|v| v.doc_id).collect()
    };
    assert_eq!(ids(holds.list_for(Some(a))), ["d_a", "d_loose"]);
    assert_eq!(ids(holds.list_for(Some(b))), ["d_b", "d_loose"]);
    assert_eq!(ids(holds.list_for(None)), ["d_loose"]);
    assert_eq!(ids(holds.list()).len(), 3);

    // The tool lists by the session's workspace.
    let mut reg = Registry::new();
    register_doc_tools(&mut reg, &holds);
    let events = Arc::new(EventBus::new(FakeClock::new()));
    let mut ctx = common::ctx(&env, CallerKind::Cli, &events);
    ctx.session = Arc::new(Session {
        workspace: Some(workspace(a)),
        ..(*ctx.session).clone()
    });
    let listed = common::call(&reg, &ctx, "doc.list_open", json!({}))
        .await
        .unwrap();
    let seen: Vec<&str> = listed["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["doc_id"].as_str().unwrap())
        .collect();
    assert_eq!(seen, ["d_a", "d_loose"], "{listed}");
    assert!(!listed.to_string().contains("ws_b"), "{listed}");
}

/// An app that holds a document, then dies: through a real connection, the hold is gone and
/// the app no longer counts as running.
#[test] // spec: APP-104, APP-063
fn a_killed_app_releases_its_holds_and_stops_running() {
    let env = TestEnv::new();
    let registration = format!(
        "app = \"test\"\nexe = '{}'\n[[opens]]\ntypes = [\"image\"]\nformats = [\"png\"]\nintents = [\"open\"]\n",
        ensure_built().test_app.display()
    );
    lil_testkit::files::write_file(
        &env.env.data_dir().join("apps").join("test.toml"),
        registration.as_bytes(),
    );
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
    let mut app = TestApp::start(&env, None, &[]);

    let doc = json!({"doc_id": "d_1", "path": r"C:\scratch\a.lildoc"});
    let held = app.tool("test.hold", doc, Some("k1"));
    assert_eq!(held["status"], "ok", "{held}");
    let listed = pipe.tool_ok("doc.list_open", json!({}), None);
    assert_eq!(listed["documents"][0]["held_by"], "test", "{listed}");
    let open = json!({"app": "test", "target": "a.png", "intent": "open"});
    assert_eq!(pipe.tool_ok("open_in", open.clone(), None)["running"], true);

    drop(app);
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let listed = pipe.tool_ok("doc.list_open", json!({}), None);
        let running = pipe.tool_ok("open_in", open.clone(), None)["running"].clone();
        if listed["documents"] == json!([]) && running == false {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the killed app still holds or runs: {listed} {running}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
