//! Document holds (APP-103, APP-104, APP-105; Appendix F "Holds"): the first holder wins, a
//! hold ends with its connection, a core hold lasts one operation, and `doc.list_open` lists
//! them. The marker rows in the workspace `state.db` are emptied when a core opens it.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lil_core::docs::{HoldOwner, Holds, register_doc_tools};
use lil_core::events::EventBus;
use lil_core::state::{HoldMarker, StateDb, StateScope};
use lil_core::tools::Registry;
use lil_protocol::CallerKind;
use lil_testkit::{FakeClock, TestEnv};
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
        .acquire_marked(&state, "d_1", doc_path(), app(7, "image"))
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
