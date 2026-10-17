//! The watcher's unit tests: path mapping for `watch.notify`, and the drop to no-watcher mode.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::unbounded_channel;
use ww_platform::fs::SystemProbe;
use ww_platform::watcher::DirChange;
use ww_testkit::files::write_file;
use ww_testkit::{FakeClock, SeqIds, TestEnv};

use super::*;
use crate::state::{StateDb, StateScope};
use crate::store::{Expect, FailPoints};

/// A failure deadline only.
const DEADLINE: Duration = Duration::from_secs(20);

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap()
}

#[test] // spec: ARCH-016, ARCH-073
fn notify_maps_paths_and_ignores_outside_ones() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé space");
    write_file(&ws.join("Notes").join("A.txt"), b"a");
    let outside = env.make_workspace("other");
    write_file(&outside.join("x.txt"), b"x");
    let upper = PathBuf::from(ws.to_string_lossy().to_uppercase());
    let given: Vec<String> = [
        ws.join("Notes")
            .join("A.txt")
            .to_string_lossy()
            .into_owned(),
        // Another case of the folder; the spelling on disk is kept.
        upper.join(r"notes\a.txt").to_string_lossy().into_owned(),
        // Removed already: the name as given.
        ws.join("Notes")
            .join("gone.png")
            .to_string_lossy()
            .into_owned(),
        r"projects\sketchbook\progress.md".to_string(),
        "notes/b.txt".to_string(),
        outside.join("x.txt").to_string_lossy().into_owned(),
        ws.to_string_lossy().into_owned(),
        "../escape.txt".to_string(),
        "notes/CON".to_string(),
    ]
    .into_iter()
    .collect();
    let (paths, ignored) = notify_paths(&ws, &given);
    assert_eq!(
        paths,
        [
            p("Notes/A.txt"),
            p("Notes/A.txt"),
            p("Notes/gone.png"),
            p("projects/sketchbook/progress.md"),
            p("notes/b.txt"),
        ]
    );
    assert_eq!(ignored, given[5..].to_vec());

    // Of the workspace paths, what the watcher doesn't watch is ignored too.
    for (path, watched) in [
        ("notes/b.txt", true),
        (".worldwright/staging/sh_0000000000000001/a.png", false),
        (".trash/2027-01-01/te_0000000000000001/payload/a.txt", false),
        (".git/index", false),
        (".worldwright/fence/1-1", false),
    ] {
        assert_eq!(classify(&p(path)) == Class::Path, watched, "{path}");
    }
}

/// The events published on `topic` so far, replayed from its history.
async fn published(events: &EventBus, topic: &str) -> Vec<Value> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);
    let client = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    events.attach(client, tx);
    let since = [(topic.to_string(), 0)].into_iter().collect();
    let states = events.subscribe(client, &[topic.to_string()], &since).await;
    let mut out = Vec::new();
    for _ in 0..states[0].seq {
        let line = tokio::time::timeout(DEADLINE, rx.recv())
            .await
            .expect("a replayed event")
            .expect("an event line");
        let event: Value = serde_json::from_str(&line).unwrap();
        out.push(event["params"]["data"].clone());
    }
    events.drop_client(client);
    out
}

#[tokio::test] // spec: ARCH-079, ARCH-083
async fn watcher_failure_drops_to_no_watcher_mode() {
    let env = TestEnv::new();
    let root = std::fs::canonicalize(env.make_workspace("lost")).unwrap();
    let root = ww_platform::paths::strip_verbatim(&root);
    let state = Arc::new(
        StateDb::open(
            &env.root().join("local").join("state.db"),
            StateScope::Workspace,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .unwrap(),
    );
    let store = Arc::new(Store::new(
        root.clone(),
        state,
        FailPoints::none(),
        FakeClock::new(),
        Arc::new(SeqIds::new()),
        Arc::new(SystemProbe),
    ));
    let index = Arc::new(Index::open_in_memory(&root, Arc::new(SystemProbe)).unwrap());
    store.set_sink(index.clone());
    index.start_first();
    let events = Arc::new(EventBus::new(FakeClock::new()));
    let (tx, rx) = unbounded_channel();
    let watcher = Watcher::start_from(
        store.clone(),
        index.clone(),
        events.clone(),
        "0123456789abcdef".to_string(),
        WatchConfig::default(),
        rx,
    );
    assert!(watcher.watching());

    tx.send(DirChange::Failed(21)).unwrap();
    // The fence stops waiting for events once the watch is lost: it returns although no fence
    // file was ever seen.
    tokio::time::timeout(DEADLINE, watcher.fence("never-written"))
        .await
        .expect("a fence returns without a watcher");
    assert!(!watcher.watching());
    // The watcher let go of its source; it ending publishes nothing more.
    assert!(
        tx.send(DirChange::Failed(21)).is_err(),
        "the source is dropped after a failure"
    );
    drop(tx);
    let first = published(&events, "workspace").await;
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0]["kind"], "watcher_unavailable");
    assert_eq!(first[0]["code"], 21);
    assert!(first[0]["reason"].as_str().is_some_and(|r| !r.is_empty()));

    // Writes keep working, and `ww notify` still decides.
    let written = store
        .write(&p("notes/a.txt"), b"still served".to_vec(), Expect::New)
        .await
        .expect("a write without a watcher");
    assert_eq!(written.path, p("notes/a.txt"));
    let report = watcher.notify(vec![p("notes/a.txt"), p(".git/HEAD")]).await;
    assert_eq!(report.reconciled, ["notes/a.txt"]);
    assert_eq!(report.ignored, [".git/HEAD"]);
    let all = published(&events, "workspace").await;
    assert_eq!(all.len(), 1, "{all:?}");
    watcher.stop();
    // The watcher's loop task holds the store, and with it state.db, until it sees the stop.
    // Wait for that here: this runtime never polls the task again once the test returns, and
    // the TestEnv cannot remove its folder while the file is open.
    let weak = Arc::downgrade(&store);
    drop((store, index, watcher));
    tokio::time::timeout(DEADLINE, async {
        while weak.strong_count() > 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the watcher's loop lets go of the store");
}
