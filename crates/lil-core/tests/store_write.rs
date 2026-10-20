//! The store's write path (ARCH-002, ARCH-074, ARCH-078, ARCH-082, CORE-040, QA-011, QA-024,
//! QA-066): hash checks, the backup check that puts an outside edit back, crash recovery, the
//! retries on held files, the path budget and long paths, the `move_new`-only fallbacks, fail
//! points and the error codes.
//!
//! Every test runs on tokio's paused clock, so the 10 s retry budget passes without real
//! sleeps (QA-031). Fixture files are made with `lil_testkit::files::write_file`; the one
//! outside rename (standing in for what a real error 1177 leaves) carries its own allowance.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::state::{PRUNE_DONE_SQL, StateDb, StateScope};
use lil_core::store::{
    Expect, FailPoints, Phase, Step, Store, StoreError, WsPath, armed, fsops, lfs_pointer_oid,
    next_step,
};
use lil_platform::fs::{
    FileIdentity, PLACEHOLDER_ATTRIBUTES, Probe, RetryClass, Share, SystemProbe, VolumeInfo,
    classify,
};
use lil_platform::{Channel, Clock};
use lil_protocol::{ErrorCategory, RetryPolicy};
use lil_schema::Manifest;
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, HandleHolder, SeqIds, TestEnv, sha256_bytes, sha256_file};

// ---------------------------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------------------------

/// A workspace folder and its `state.db`, both under the test's own root.
struct Ws {
    root: PathBuf,
    state: Arc<StateDb>,
    clock: Arc<FakeClock>,
    /// Last: fields drop in order, and the folder cannot go while `state` holds state.db open.
    _env: TestEnv,
}

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn workspace() -> Ws {
    let env = TestEnv::with_label("Ünïcödé space");
    let root = env.make_workspace("store");
    Ws::at(env, root)
}

impl Ws {
    fn at(env: TestEnv, root: PathBuf) -> Ws {
        let db = env.root().join("local-ws").join("state.db");
        let state =
            Arc::new(StateDb::open(&db, StateScope::Workspace, no_fatal()).expect("state.db"));
        Ws {
            root,
            state,
            clock: FakeClock::new(),
            _env: env,
        }
    }

    /// A store over the workspace with `fp` and the real probe.
    fn store(&self, fp: Arc<FailPoints>) -> Arc<Store> {
        self.store_with_probe(fp, Arc::new(SystemProbe))
    }

    fn store_with_probe(&self, fp: Arc<FailPoints>, probe: Arc<dyn Probe>) -> Arc<Store> {
        Arc::new(Store::new(
            self.root.clone(),
            self.state.clone(),
            fp,
            self.clock.clone(),
            Arc::new(SeqIds::new()),
            probe,
        ))
    }

    fn plain(&self) -> Arc<Store> {
        self.store(FailPoints::none())
    }

    fn file(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, c| p.join(c))
    }

    fn read(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.file(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
    }

    /// The names in `rel_dir` (dot-files included), sorted.
    fn names(&self, rel_dir: &str) -> Vec<String> {
        let dir = if rel_dir.is_empty() {
            self.root.clone()
        } else {
            self.file(rel_dir)
        };
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The temp and backup files in `rel_dir`.
    fn leftovers(&self, rel_dir: &str) -> Vec<String> {
        self.names(rel_dir)
            .into_iter()
            .filter(|n| n.contains(".lil-tmp-") || n.contains(".lil-bak-"))
            .collect()
    }

    /// The `<name>.lil-conflict-*` files in `rel_dir`.
    fn conflicts(&self, rel_dir: &str) -> Vec<String> {
        self.names(rel_dir)
            .into_iter()
            .filter(|n| n.contains(".lil-conflict-"))
            .collect()
    }
}

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn sha(bytes: &[u8]) -> String {
    sha256_bytes(bytes)
}

/// A write of `text` that must succeed.
async fn put(store: &Store, rel: &str, text: &str, expect: Expect) {
    store
        .write(&p(rel), text.as_bytes().to_vec(), expect)
        .await
        .unwrap_or_else(|e| panic!("writing {rel}: {e}"));
}

/// A plain outside-edit conflict: no backup left, not a placeholder, nothing kept aside.
fn conflict_without_backup(result: Result<impl std::fmt::Debug, StoreError>, rel: &str) {
    match result {
        Err(StoreError::HashConflict {
            path,
            backup: None,
            cloud_only: false,
            kept,
        }) if kept.is_empty() => assert_eq!(path.as_str(), rel),
        other => panic!("expected a hash conflict without a backup, got {other:?}"),
    }
}

/// The conflict of a cloud-only placeholder (ruling Q11).
fn cloud_only_conflict(result: Result<impl std::fmt::Debug, StoreError>, rel: &str) {
    let Err(e) = result else {
        panic!("expected a cloud-only conflict, got {result:?}");
    };
    let StoreError::HashConflict {
        cloud_only: true,
        backup: None,
        kept,
        ..
    } = &e
    else {
        panic!("expected a cloud-only conflict, got {e:?}");
    };
    assert!(kept.is_empty(), "{e:?}");
    let body = e.to_error("t");
    assert_eq!(body.code, "store.hash_conflict");
    assert_eq!(body.data.as_ref().unwrap()["path"], rel);
    assert_eq!(body.data.as_ref().unwrap()["cloud_only"], true);
    assert!(body.message.contains("cloud-only"), "{}", body.message);
    assert!(
        body.message.contains("available offline"),
        "{}",
        body.message
    );
}

/// A hook that writes `text` to `path` as an outside editor would.
fn outside_writer(
    path: PathBuf,
    text: &'static str,
) -> Box<dyn Fn() -> io::Result<()> + Send + Sync> {
    Box::new(move || {
        write_file(&path, text.as_bytes());
        Ok(())
    })
}

// ---------------------------------------------------------------------------------------------
// Hash checks and generated hashes.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-002, ARCH-078
async fn new_file_write_records_generated_hash() {
    let ws = workspace();
    let store = ws.plain();
    let written = store
        .write(&p("notes/a.txt"), b"core".to_vec(), Expect::New)
        .await
        .expect("a new file");
    assert_eq!(written.path.as_str(), "notes/a.txt");
    assert_eq!(written.sha256, sha(b"core"));
    assert_eq!(ws.read("notes/a.txt"), b"core");
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"core"))
    );
    let identity = SystemProbe.identity(&ws.file("notes/a.txt")).unwrap();
    assert_eq!(written.identity, identity, "the identity after the write");
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);

    // The next generation replaces it; the generated hash follows.
    let second = store
        .write(&p("notes/a.txt"), b"core 2".to_vec(), Expect::Generated)
        .await
        .expect("a generated file is the core's to replace");
    assert_eq!(ws.read("notes/a.txt"), b"core 2");
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"core 2"))
    );
    assert_eq!(
        second.identity,
        SystemProbe.identity(&ws.file("notes/a.txt")).unwrap()
    );
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(ws.names("notes"), ["a.txt"]);
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn full_write_over_outside_edit_is_hash_conflict() {
    let ws = workspace();
    let store = ws.plain();
    put(&store, "notes/a.txt", "core", Expect::New).await;
    write_file(&ws.file("notes/a.txt"), b"mine");

    let result = store
        .write(&p("notes/a.txt"), b"core 2".to_vec(), Expect::Generated)
        .await;
    conflict_without_backup(result, "notes/a.txt");
    let by_hash = store
        .write(
            &p("notes/a.txt"),
            b"core 2".to_vec(),
            Expect::Hash(sha(b"core")),
        )
        .await;
    conflict_without_backup(by_hash, "notes/a.txt");
    let as_new = store
        .write(&p("notes/a.txt"), b"core 2".to_vec(), Expect::New)
        .await;
    conflict_without_backup(as_new, "notes/a.txt");
    assert_eq!(
        ws.read("notes/a.txt"),
        b"mine",
        "the outside edit was overwritten"
    );
    assert!(ws.leftovers("notes").is_empty());
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn edit_between_check_and_replace_puts_the_outside_version_back() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let fp = FailPoints::none();
    fp.on(
        "store.replace",
        outside_writer(ws.file("notes/a.txt"), "outside"),
    );
    let store = ws.store(fp);

    let result = store
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    conflict_without_backup(result, "notes/a.txt");
    assert_eq!(ws.read("notes/a.txt"), b"outside");
    assert_eq!(ws.names("notes"), ["a.txt"], "no backup or temp file stays");
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"v1")),
        "the failed write recorded nothing"
    );
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn put_back_held_past_budget_names_the_backup_and_recovers() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let target = ws.file("notes/a.txt");
    let held: Arc<Mutex<Option<HandleHolder>>> = Arc::default();
    let fp = FailPoints::none();
    fp.on("store.replace", outside_writer(target.clone(), "outside"));
    // After the swap the target (the core's bytes) is held, so the put-back can't replace it.
    let (slot, path) = (held.clone(), target.clone());
    fp.on(
        "store.after_replace",
        Box::new(move || {
            *slot.lock().unwrap() = Some(HandleHolder::hold(&path, Share::Read));
            Ok(())
        }),
    );
    let store = ws.store(fp);

    let writer = {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
                .await
        })
    };
    tokio::time::advance(Duration::from_secs(11)).await;
    let result = writer.await.unwrap();
    let Err(StoreError::HashConflict {
        path,
        backup: Some(backup),
        cloud_only: false,
        kept,
    }) = result.clone()
    else {
        panic!("expected a conflict that names the backup, got {result:?}");
    };
    assert!(kept.is_empty(), "{kept:?}");
    assert_eq!(path.as_str(), "notes/a.txt");
    assert!(
        backup.as_str().starts_with("notes/.a.txt.lil-bak-"),
        "{backup}"
    );
    assert_eq!(
        ws.read(backup.as_str()),
        b"outside",
        "the backup holds the outside edit"
    );
    let body = result.as_ref().unwrap_err().to_error("t1");
    assert_eq!(body.code, "store.hash_conflict");
    assert_eq!(body.data.as_ref().unwrap()["backup"], backup.as_str());
    assert_eq!(body.trace_id, "t1");
    assert!(
        ws.state.pending("notes/a.txt").await.unwrap().is_some(),
        "the pending row stays for recovery"
    );

    // A recovery while the target is still held changes nothing and keeps the row.
    let early = ws.plain().recover().await.expect("recover while held");
    assert!(early.pending_resolved.is_empty(), "{early:?}");
    assert!(
        early.backups_restored.is_empty() && early.conflicts_kept.is_empty(),
        "{early:?}"
    );
    assert_eq!(ws.read(backup.as_str()), b"outside");
    assert!(ws.state.pending("notes/a.txt").await.unwrap().is_some());

    held.lock()
        .unwrap()
        .take()
        .expect("the hook held the target")
        .release();
    let report = ws.plain().recover().await.expect("recover");
    assert_eq!(
        ws.read("notes/a.txt"),
        b"outside",
        "the outside version is back"
    );
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(report.backups_restored, [backup]);
    assert_eq!(report.pending_resolved, [p("notes/a.txt")]);
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
    // The core's bytes were set aside beside it, never over it.
    let conflicts = ws.conflicts("notes");
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(ws.read(&format!("notes/{}", conflicts[0])), b"v2");
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn crash_after_replace_keeps_outside_edit_visible() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let fp = Arc::new(FailPoints::parse("store.after_replace=io").unwrap());
    fp.on(
        "store.replace",
        outside_writer(ws.file("notes/a.txt"), "outside"),
    );
    let crashed = ws.store(fp);
    let result = crashed
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");
    drop(crashed);
    // What the crash left: the core's bytes at the target, the outside edit in a dot-file.
    assert_eq!(ws.read("notes/a.txt"), b"v2");
    assert_eq!(ws.leftovers("notes").len(), 1, "{:?}", ws.names("notes"));

    let report = ws.plain().recover().await.expect("recover");
    assert_eq!(
        ws.read("notes/a.txt"),
        b"outside",
        "the outside edit is visible again"
    );
    let conflicts = ws.conflicts("notes");
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    let stamp = lil_schema::time::compact(utc_secs(&ws));
    assert_eq!(conflicts[0], format!("a.txt.lil-conflict-{stamp}"));
    assert_eq!(ws.read(&format!("notes/{}", conflicts[0])), b"v2");
    assert_eq!(
        report.conflicts_kept,
        [p(&format!("notes/{}", conflicts[0]))]
    );
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
}

fn utc_secs(ws: &Ws) -> i64 {
    ws.clock
        .utc_now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn crash_after_replace_does_not_flag_own_write_as_human_edit() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let crashed = ws.store(Arc::new(
        FailPoints::parse("store.after_replace=io").unwrap(),
    ));
    let result = crashed
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");
    drop(crashed);

    let store = ws.plain();
    let rel = p("notes/a.txt");
    assert!(
        store.accepts_as_own(&rel, &sha(b"v1")).await.unwrap(),
        "old"
    );
    assert!(
        store.accepts_as_own(&rel, &sha(b"v2")).await.unwrap(),
        "new"
    );
    assert!(!store.accepts_as_own(&rel, &sha(b"other")).await.unwrap());

    let report = store.recover().await.expect("recover");
    assert_eq!(report.pending_resolved, std::slice::from_ref(&rel));
    assert_eq!(report.backups_removed.len(), 1, "{report:?}");
    assert!(report.conflicts_kept.is_empty(), "{report:?}");
    assert_eq!(ws.read("notes/a.txt"), b"v2");
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"v2"))
    );
    assert!(store.accepts_as_own(&rel, &sha(b"v2")).await.unwrap());
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    put(&store, "notes/a.txt", "v3", Expect::Generated).await;
    assert_eq!(ws.read("notes/a.txt"), b"v3");
}

#[tokio::test(start_paused = true)] // spec: ARCH-078
async fn expect_generated_without_a_row_is_conflict_or_new() {
    let ws = workspace();
    let store = ws.plain();
    write_file(&ws.file("notes/hand.txt"), b"by hand");
    let result = store
        .write(&p("notes/hand.txt"), b"core".to_vec(), Expect::Generated)
        .await;
    conflict_without_backup(result, "notes/hand.txt");
    assert_eq!(ws.read("notes/hand.txt"), b"by hand");

    put(&store, "notes/fresh.txt", "core", Expect::Generated).await;
    assert_eq!(ws.read("notes/fresh.txt"), b"core");
    assert_eq!(
        ws.state.generated("notes/fresh.txt").await.unwrap(),
        Some(sha(b"core"))
    );
}

/// The real probe, except that one path reports the cloud placeholder attributes; every data
/// open is counted.
#[derive(Debug)]
struct CloudProbe {
    placeholder: PathBuf,
    opens: AtomicUsize,
}

impl Probe for CloudProbe {
    fn identity(&self, path: &Path) -> io::Result<FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if path == self.placeholder {
            id.attributes |= PLACEHOLDER_ATTRIBUTES;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        SystemProbe.open_read(path)
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-074, ARCH-079
async fn cloud_only_target_is_a_conflict_and_never_read() {
    let ws = workspace();
    write_file(&ws.file("notes/cloud.json"), b"{\"asset_id\":\"a\"}\n");
    let probe = Arc::new(CloudProbe {
        placeholder: ws.file("notes/cloud.json"),
        opens: AtomicUsize::new(0),
    });
    let store = ws.store_with_probe(FailPoints::none(), probe.clone());
    let rel = p("notes/cloud.json");

    for expect in [
        Expect::Generated,
        Expect::New,
        Expect::Hash(sha(b"{\"asset_id\":\"a\"}\n")),
    ] {
        let result = store.write(&rel, b"x".to_vec(), expect.clone()).await;
        cloud_only_conflict(result, "notes/cloud.json");
    }
    let manifest = store.edit_manifest(&rel, &|_| Ok(())).await;
    cloud_only_conflict(manifest, "notes/cloud.json");
    let text = store.edit_text(&rel, &|t| Ok(format!("{t}x"))).await;
    cloud_only_conflict(text, "notes/cloud.json");
    let read = store.read(&rel).await;
    let Err(e @ StoreError::Io { .. }) = &read else {
        panic!("a read of a placeholder is store.io, got {read:?}");
    };
    assert!(e.to_string().contains("available offline"), "{e}");
    // An outside edit's conflict says so, and doesn't talk about cloud files.
    let edited = StoreError::HashConflict {
        path: rel.clone(),
        backup: None,
        cloud_only: false,
        kept: Vec::new(),
    };
    let message = edited.to_error("t").message;
    assert!(message.contains("changed outside"), "{message}");
    assert!(!message.contains("cloud-only"), "{message}");

    assert_eq!(
        probe.opens.load(Ordering::SeqCst),
        0,
        "a placeholder's data was opened"
    );
    assert_eq!(ws.read("notes/cloud.json"), b"{\"asset_id\":\"a\"}\n");
}

// ---------------------------------------------------------------------------------------------
// Crashes in the middle of a put-back.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn crash_during_put_back_keeps_the_outside_edit() {
    for fallback in [false, true] {
        let ws = workspace();
        put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
        let spec = if fallback {
            "store.replace=unsupported,store.put_back=io"
        } else {
            "store.put_back=io"
        };
        let fp = Arc::new(FailPoints::parse(spec).unwrap());
        fp.on(
            "store.replace",
            outside_writer(ws.file("notes/a.txt"), "outside"),
        );
        let crashed = ws.store(fp);
        let result = crashed
            .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
            .await;
        assert!(
            matches!(result, Err(StoreError::HashConflict { .. })),
            "fallback={fallback}: {result:?}"
        );
        drop(crashed);

        let backups: Vec<Vec<u8>> = ws
            .leftovers("notes")
            .iter()
            .map(|n| ws.read(&format!("notes/{n}")))
            .collect();
        if fallback {
            // Target → second backup happened, first backup → target did not.
            assert!(
                !ws.file("notes/a.txt").exists(),
                "the target is missing mid-way"
            );
            let mut contents = backups.clone();
            contents.sort();
            assert_eq!(contents, [b"outside".to_vec(), b"v2".to_vec()]);
        } else {
            // The ReplaceFileW put-back happened; its second backup holds the core's bytes.
            assert_eq!(ws.read("notes/a.txt"), b"outside");
            assert_eq!(backups, [b"v2".to_vec()]);
        }

        ws.plain().recover().await.expect("recover");
        assert_eq!(ws.read("notes/a.txt"), b"outside", "fallback={fallback}");
        assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
        assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
        for name in ws.names("notes") {
            assert_ne!(
                ws.read(&format!("notes/{name}")),
                b"v2",
                "fallback={fallback}: the failed call's bytes stayed in {name}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Held files.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn held_target_is_retried_until_released() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let holder = HandleHolder::hold(&ws.file("notes/a.txt"), Share::Read);
    let (attempts_tx, mut attempts) = tokio::sync::mpsc::unbounded_channel();
    let fp = FailPoints::none();
    fp.on(
        "store.replace",
        Box::new(move || {
            let _ = attempts_tx.send(());
            Ok(())
        }),
    );
    let store = ws.store(fp);
    let start = tokio::time::Instant::now();
    let mut writer = {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
                .await
        })
    };
    // Each attempt announces itself; the paused clock moves through the pauses between them.
    let mut tries = 0;
    while start.elapsed() < Duration::from_secs(3) {
        tokio::select! {
            got = attempts.recv() => {
                got.expect("the write keeps trying");
                tries += 1;
            }
            done = &mut writer => panic!("the write ended while the file was held: {done:?}"),
        }
    }
    assert!(tries >= 5, "{tries} attempts in 3 s");
    assert!(
        !writer.is_finished(),
        "the write gave up while the file was held"
    );
    holder.release();
    let written = writer.await.unwrap().expect("the write after the release");
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(written.sha256, sha(b"v2"));
    assert_eq!(ws.read("notes/a.txt"), b"v2");
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn target_held_past_budget_fails_with_store_busy_and_no_temp() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let before = sha256_file(&ws.file("notes/a.txt"));
    let holder = HandleHolder::hold(&ws.file("notes/a.txt"), Share::Read);
    let store = ws.plain();
    let writer = {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
                .await
        })
    };
    tokio::time::advance(Duration::from_secs(11)).await;
    let result = writer.await.unwrap();
    assert!(
        matches!(&result, Err(StoreError::Busy(path)) if path.as_str() == "notes/a.txt"),
        "{result:?}"
    );
    assert_eq!(result.unwrap_err().to_error("t").code, "store.busy");
    holder.release();
    assert_eq!(
        sha256_file(&ws.file("notes/a.txt")),
        before,
        "the target changed"
    );
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
}

// ---------------------------------------------------------------------------------------------
// Paths.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: CORE-040
async fn relative_path_over_180_chars_is_refused_before_any_write() {
    let ws = workspace();
    let over = format!("over/deep/{}.txt", "x".repeat(181 - "over/deep/.txt".len()));
    assert_eq!(over.chars().count(), 181);
    let refused = WsPath::parse(&over).expect_err("181 characters");
    assert_eq!(refused.to_error("t").code, "store.path_invalid");
    assert!(!ws.root.join("over").exists());

    // At the budget the write goes through.
    let fits = format!(
        "under/deep/{}.txt",
        "y".repeat(180 - "under/deep/.txt".len())
    );
    put(&ws.plain(), &fits, "fits", Expect::New).await;
    assert_eq!(ws.read(&fits), b"fits");

    // A path the store refuses creates no folder either.
    let sealed = ws
        .plain()
        .write(
            &p("projects/p/assets/a/v1.0.0/new/x.txt"),
            b"x".to_vec(),
            Expect::New,
        )
        .await;
    assert!(
        matches!(sealed, Err(StoreError::PathInvalid { .. })),
        "{sealed:?}"
    );
    assert!(!ws.root.join("projects").exists());
    for reserved in ["con.txt", "notes/NUL", "a/b./c", "x:y"] {
        assert!(WsPath::parse(reserved).is_err(), "{reserved}");
    }
}

#[tokio::test(start_paused = true)] // spec: CORE-040
async fn deep_workspace_past_260_chars_writes_through_long_paths() {
    let env = TestEnv::new();
    let tail = r"\notes\deep.txt";
    let units = |p: &Path| p.as_os_str().to_string_lossy().encode_utf16().count();
    let base = env.root().join("deep");
    let missing = 320 - units(&base) - tail.len();
    // Folder names of at most 200 characters, each with its `\`.
    let mut root = base;
    let mut left = missing;
    while left > 0 {
        let mut take = left.min(201);
        if left - take == 1 {
            take -= 1;
        }
        root = root.join("d".repeat(take - 1));
        left -= take;
    }
    let target = root.join("notes").join("deep.txt");
    assert_eq!(units(&target), 320, "{}", target.display());
    write_file(&root.join("lilith.yaml"), b"name: deep\n");
    let ws = Ws::at(env, root);
    let store = ws.plain();

    put(&store, "notes/deep.txt", "far down", Expect::New).await;
    assert_eq!(
        store.read(&p("notes/deep.txt")).await.unwrap().as_deref(),
        Some(&b"far down"[..])
    );
    put(&store, "notes/deep.txt", "again", Expect::Generated).await;
    assert_eq!(
        store.read(&p("notes/deep.txt")).await.unwrap().as_deref(),
        Some(&b"again"[..])
    );
    assert_eq!(store.recover().await.unwrap(), Default::default());
}

#[tokio::test(start_paused = true)] // spec: ARCH-082, CORE-026
async fn ledger_csv_and_sealed_folders_are_refused() {
    let ws = workspace();
    let ledger = b"date,amount\n2026-10-01,0\n";
    write_file(&ws.file("registry/ledger.csv"), ledger);
    write_file(&ws.file("projects/p/assets/a/v1.0.0/x.png"), b"sealed");
    let store = ws.plain();

    for rel in ["registry/ledger.csv", "Registry/Ledger.CSV"] {
        let result = store
            .write(
                &p(rel),
                b"date,amount\n".to_vec(),
                Expect::Hash(sha(ledger)),
            )
            .await;
        assert!(
            matches!(result, Err(StoreError::PathInvalid { .. })),
            "{result:?}"
        );
        let edit = store.edit_text(&p(rel), &|t| Ok(format!("{t}x\n"))).await;
        assert!(
            matches!(edit, Err(StoreError::PathInvalid { .. })),
            "{edit:?}"
        );
    }
    for rel in [
        "projects/p/assets/a/v1.0.0/x.png",
        "projects/p/assets/a/v1.0.0/y.png",
        "projects/p/assets/a/V1.0.0/x.png",
    ] {
        let result = store
            .write(&p(rel), b"new".to_vec(), Expect::Hash(sha(b"sealed")))
            .await;
        let err = result.expect_err(rel);
        assert_eq!(err.to_error("t").code, "store.path_invalid", "{rel}");
    }
    assert_eq!(ws.read("registry/ledger.csv"), ledger);
    assert_eq!(ws.read("projects/p/assets/a/v1.0.0/x.png"), b"sealed");
    assert_eq!(ws.names("projects/p/assets/a/v1.0.0"), ["x.png"]);
}

// ---------------------------------------------------------------------------------------------
// The fallback and 1177.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn replace_falls_back_to_two_renames_where_unsupported() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let store = ws.store(Arc::new(
        FailPoints::parse("store.replace=unsupported").unwrap(),
    ));
    let written = store
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await
        .expect("the two-rename fallback");
    assert_eq!(ws.read("notes/a.txt"), b"v2");
    assert_eq!(
        written.identity,
        SystemProbe.identity(&ws.file("notes/a.txt")).unwrap()
    );
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"v2"))
    );
    assert_eq!(ws.names("notes"), ["a.txt"]);
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn fallback_crash_between_renames_is_restored_by_recover() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let fp = Arc::new(FailPoints::parse("store.replace=unsupported").unwrap());
    fp.on(
        "store.between_renames",
        Box::new(|| Err(io::Error::other("the process stops here"))),
    );
    let crashed = ws.store(fp);
    let result = crashed
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");
    drop(crashed);
    assert!(!ws.file("notes/a.txt").exists(), "only the backup is left");
    assert_eq!(ws.leftovers("notes").len(), 1, "{:?}", ws.names("notes"));

    let report = ws.plain().recover().await.expect("recover");
    assert_eq!(ws.read("notes/a.txt"), b"v1", "the old content is back");
    assert_eq!(report.backups_restored.len(), 1, "{report:?}");
    assert_eq!(ws.names("notes"), ["a.txt"]);
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"v1"))
    );
}

/// What a real error 1177 leaves: the target under the first backup name (the temp file's
/// name with `lil-tmp` read as `lil-bak`), then an outside writer takes the freed name.
#[allow(clippy::disallowed_methods)]
fn act_out_1177(dir: &Path, name: &str) -> io::Result<()> {
    let temps: Vec<String> = std::fs::read_dir(dir)?
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!(".{name}.lil-tmp-")))
        .collect();
    assert_eq!(temps.len(), 1, "{temps:?}");
    let backup = temps[0].replace("lil-tmp", "lil-bak");
    std::fs::rename(dir.join(name), dir.join(backup))?;
    write_file(&dir.join(name), b"outside");
    Err(io::Error::from_raw_os_error(1177))
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn fallback_second_rename_never_replaces_a_new_outside_file() {
    for variant in ["fallback", "1177"] {
        let ws = workspace();
        put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
        let fp = if variant == "fallback" {
            let fp = Arc::new(FailPoints::parse("store.replace=unsupported").unwrap());
            fp.on(
                "store.between_renames",
                outside_writer(ws.file("notes/a.txt"), "outside"),
            );
            fp
        } else {
            let fp = FailPoints::none();
            let dir = ws.file("notes");
            fp.on(
                "store.replace",
                Box::new(move || act_out_1177(&dir, "a.txt")),
            );
            fp
        };
        let result = ws
            .store(fp)
            .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
            .await;
        let Err(StoreError::HashConflict {
            backup: None,
            cloud_only: false,
            kept,
            ..
        }) = &result
        else {
            panic!("{variant}: expected the set-aside copy to be named, got {result:?}");
        };
        assert_eq!(kept.len(), 1, "{variant}: {kept:?}");
        let kept = &kept[0];
        assert_eq!(ws.read("notes/a.txt"), b"outside", "{variant}");
        let conflicts = ws.conflicts("notes");
        assert_eq!(conflicts.len(), 1, "{variant}: {:?}", ws.names("notes"));
        assert_eq!(
            ws.read(&format!("notes/{}", conflicts[0])),
            b"v1",
            "{variant}"
        );
        assert_eq!(
            kept.as_str(),
            format!("notes/{}", conflicts[0]),
            "{variant}"
        );
        assert!(
            ws.leftovers("notes").is_empty(),
            "{variant}: {:?}",
            ws.names("notes")
        );
        assert_eq!(
            ws.state.pending("notes/a.txt").await.unwrap(),
            None,
            "{variant}"
        );
    }
}

#[test] // spec: ARCH-074
fn replace_errors_map_to_retry_recover_or_fail() {
    let class = |code| classify(&io::Error::from_raw_os_error(code));
    let replace = |code| next_step(class(code), Phase::Replace);
    assert_eq!(replace(32), Step::Retry);
    assert_eq!(replace(33), Step::Retry);
    assert_eq!(replace(1224), Step::Retry);
    assert_eq!(replace(1175), Step::Retry);
    assert_eq!(replace(1176), Step::Retry);
    assert_eq!(
        replace(1177),
        Step::MoveReplacement,
        "1177 goes to move_new"
    );
    assert_eq!(replace(50), Step::TwoRenames);
    assert_eq!(replace(1), Step::TwoRenames);
    assert_eq!(replace(112), Step::DiskFull);
    assert_eq!(replace(39), Step::DiskFull);
    assert_eq!(replace(183), Step::Fail);
    assert_eq!(replace(5), Step::Fail);

    // A new target: 80 and 183 are an outside writer's file.
    let new = |code| next_step(class(code), Phase::MoveNew);
    assert_eq!(new(183), Step::Conflict);
    assert_eq!(new(80), Step::Conflict);
    assert_eq!(new(32), Step::Retry);
    assert_eq!(new(1175), Step::Fail, "no ReplaceFileW, no 1175 retry");
    assert_eq!(new(112), Step::DiskFull);

    // The fallback's second rename and the move after 1177 (both `move_new` onto a freed
    // name): 80 and 183 keep the outside file.
    let freed = |code| next_step(class(code), Phase::FreedName);
    assert_eq!(freed(183), Step::KeepOutsideFile);
    assert_eq!(freed(80), Step::KeepOutsideFile);
    assert_eq!(freed(32), Step::Retry);
    assert_eq!(freed(1177), Step::Fail);
    assert_eq!(freed(50), Step::Fail);
    assert_eq!(freed(112), Step::DiskFull);

    let first = |code| next_step(class(code), Phase::FallbackFirst);
    assert_eq!(first(32), Step::Retry);
    assert_eq!(first(183), Step::Fail);
    assert_eq!(first(112), Step::DiskFull);
    assert_eq!(class(1177), RetryClass::ReplacementNotMoved);
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn new_target_created_by_an_outside_writer_is_hash_conflict() {
    let ws = workspace();
    let fp = FailPoints::none();
    fp.on(
        "store.replace",
        outside_writer(ws.file("notes/new.txt"), "outside"),
    );
    let result = ws
        .store(fp)
        .write(&p("notes/new.txt"), b"core".to_vec(), Expect::New)
        .await;
    conflict_without_backup(result, "notes/new.txt");
    assert_eq!(ws.read("notes/new.txt"), b"outside");
    assert_eq!(ws.names("notes"), ["new.txt"], "no temp file stays");
    assert_eq!(ws.state.pending("notes/new.txt").await.unwrap(), None);
    assert_eq!(ws.state.generated("notes/new.txt").await.unwrap(), None);
}

#[tokio::test(start_paused = true)] // spec: QA-011
async fn disk_full_leaves_no_temp_and_the_target_unchanged() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let store = ws.store(Arc::new(
        FailPoints::parse("store.write_temp=disk_full").unwrap(),
    ));
    let result = store
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    assert!(
        matches!(&result, Err(StoreError::DiskFull(path)) if path.as_str() == "notes/a.txt"),
        "{result:?}"
    );
    assert_eq!(result.unwrap_err().to_error("t").code, "store.disk_full");
    assert_eq!(ws.read("notes/a.txt"), b"v1");
    let fresh = store
        .write(&p("notes/b.txt"), b"v2".to_vec(), Expect::New)
        .await;
    assert!(matches!(fresh, Err(StoreError::DiskFull(_))), "{fresh:?}");
    assert_eq!(ws.names("notes"), ["a.txt"], "no temp file stays");
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"v1"))
    );
}

#[tokio::test(start_paused = true)] // spec: ARCH-078, ARCH-085
async fn lfs_pointer_compares_by_oid() {
    let ws = workspace();
    let real = sha(b"the real image bytes");
    let pointer =
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{real}\nsize 20\n");
    assert_eq!(lfs_pointer_oid(pointer.as_bytes()), Some(real.clone()));
    write_file(&ws.file("art/a.png"), pointer.as_bytes());
    ws.state.set_generated("art/a.png", &real, 0).await.unwrap();
    let store = ws.plain();
    put(&store, "art/a.png", "regenerated", Expect::Generated).await;
    assert_eq!(ws.read("art/a.png"), b"regenerated");
    assert_eq!(ws.names("art"), ["a.png"]);

    // A pointer to other content is not the core's.
    let other = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 5\n",
        sha(b"other")
    );
    write_file(&ws.file("art/b.png"), other.as_bytes());
    ws.state.set_generated("art/b.png", &real, 0).await.unwrap();
    let result = store
        .write(&p("art/b.png"), b"x".to_vec(), Expect::Generated)
        .await;
    conflict_without_backup(result, "art/b.png");
}

// ---------------------------------------------------------------------------------------------
// Edits.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-074, ARCH-076
async fn manifest_edit_rereads_and_reapplies_after_an_outside_change() {
    let ws = workspace();
    let rel = "projects/p/assets/a/a.manifest.json";
    write_file(
        &ws.file(rel),
        b"{\n  \"asset_id\": \"a\",\n  \"n\": 1,\n  \"log\": []\n}\n",
    );
    let fp = FailPoints::none();
    let once = AtomicBool::new(false);
    let target = ws.file(rel);
    fp.on(
        "store.replace",
        Box::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                write_file(
                    &target,
                    b"{\n  \"asset_id\": \"a\",\n  \"n\": 2,\n  \"log\": []\n}\n",
                );
            }
            Ok(())
        }),
    );
    let store = ws.store(fp);
    let calls = AtomicUsize::new(0);
    let (manifest, written) = store
        .edit_manifest(&p(rel), &|m: &mut Manifest| {
            calls.fetch_add(1, Ordering::SeqCst);
            m.push("log", serde_json::json!("edited"))
                .map_err(|e| StoreError::manifest_edit(&p(rel), &e))
        })
        .await
        .expect("the edit after re-reading");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "read, apply, conflict, re-read, re-apply"
    );
    assert_eq!(
        manifest.get("n"),
        Some(&serde_json::json!(2)),
        "the outside change is kept"
    );
    assert_eq!(manifest.get("log"), Some(&serde_json::json!(["edited"])));
    assert_eq!(ws.read(rel), manifest.to_text().as_bytes());
    assert_eq!(written.sha256, sha256_file(&ws.file(rel)));
    assert_eq!(
        ws.names("projects/p/assets/a"),
        ["a.manifest.json"],
        "no backup or temp file stays"
    );
}

#[tokio::test(start_paused = true)] // spec: ARCH-079, CORE-148
async fn invalid_manifest_is_needs_repair_and_untouched() {
    let ws = workspace();
    let store = ws.plain();
    let rel = "projects/p/assets/a/a.manifest.json";
    write_file(&ws.file(rel), b"{ not json");
    let result = store.edit_manifest(&p(rel), &|_| Ok(())).await;
    assert!(
        matches!(&result, Err(StoreError::NeedsRepair(path)) if path.as_str() == rel),
        "{result:?}"
    );
    assert_eq!(
        result.unwrap_err().to_error("t").code,
        "manifest.needs_repair"
    );
    assert_eq!(ws.read(rel), b"{ not json");

    // A push onto a value that isn't a list (ruling Q5) is needs-repair too, and not written.
    let text = b"{\n  \"asset_id\": \"a\",\n  \"human_edits\": \"oops\"\n}\n";
    write_file(&ws.file(rel), text);
    let result = store
        .edit_manifest(&p(rel), &|m: &mut Manifest| {
            m.push("human_edits", serde_json::json!({"path": "x"}))
                .map_err(|e| StoreError::manifest_edit(&p(rel), &e))
        })
        .await;
    assert!(
        matches!(result, Err(StoreError::NeedsRepair(_))),
        "{result:?}"
    );
    assert_eq!(ws.read(rel), text);
    assert_eq!(ws.names("projects/p/assets/a"), ["a.manifest.json"]);
}

#[tokio::test(start_paused = true)] // spec: ARCH-076, ARCH-074
async fn yaml_edit_keeps_comments_and_is_hash_checked() {
    let ws = workspace();
    let rel = "projects/p/project.yaml";
    write_file(
        &ws.file(rel),
        b"# The project.\nname: Lamp # shown in the app\nstatus: active\nlinks: []\n",
    );
    let fp = FailPoints::none();
    let once = AtomicBool::new(false);
    let target = ws.file(rel);
    // An outside edit lands between the read and the replace: it is kept and the edit is
    // applied on top of it.
    fp.on(
        "store.replace",
        Box::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                write_file(
                    &target,
                    b"# The project.\nname: Lamp # shown in the app\nstatus: active\nlinks: []\nowner: x\n",
                );
            }
            Ok(())
        }),
    );
    let store = ws.store(fp);
    let edit = lil_schema::YamlEdit::SetScalar {
        path: vec!["status".to_string()],
        value: "paused".to_string(),
    };
    store
        .edit_yaml(&p(rel), &[edit])
        .await
        .expect("the YAML edit");
    assert_eq!(
        String::from_utf8(ws.read(rel)).unwrap(),
        "# The project.\nname: Lamp # shown in the app\nstatus: paused\nlinks: []\nowner: x\n"
    );

    let nested = lil_schema::YamlEdit::SetScalar {
        path: vec!["missing".to_string(), "deeper".to_string()],
        value: "x".to_string(),
    };
    let refused = ws.plain().edit_yaml(&p(rel), &[nested]).await;
    let err = refused.expect_err("a missing nested key");
    assert_eq!(err.to_error("t").code, "yaml.unsupported_edit");
    assert_eq!(ws.names("projects/p"), ["project.yaml"]);
}

// ---------------------------------------------------------------------------------------------
// Recovery of temp files, fail points, codes and the state index.
// ---------------------------------------------------------------------------------------------

/// A process id that has certainly exited: a short child (this test binary, running no test)
/// that was waited for. The returned handle keeps the id from being reused.
fn exited_pid() -> (u32, std::process::Child) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "no_such_test_runs_here", "--quiet"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the test binary");
    child.wait().expect("wait for it");
    (child.id(), child)
}

#[tokio::test(start_paused = true)] // spec: ARCH-074, QA-011
async fn stale_temps_of_dead_processes_are_removed_by_recover() {
    let ws = workspace();
    let (dead, _child) = exited_pid();
    let live = std::process::id();
    let stale = format!(".a.txt.lil-tmp-{dead}-1");
    let ours = format!(".a.txt.lil-tmp-{live}-1");
    write_file(&ws.file(&format!("notes/{stale}")), b"half");
    write_file(&ws.file(&format!("notes/{ours}")), b"in use");
    write_file(&ws.file("notes/a.txt"), b"kept");

    let report = ws.plain().recover().await.expect("recover");
    assert_eq!(report.temps_removed, [p(&format!("notes/{stale}"))]);
    let mut want = vec![ours.clone(), "a.txt".to_string()];
    want.sort();
    assert_eq!(ws.names("notes"), want);
    assert_eq!(ws.read("notes/a.txt"), b"kept");
}

/// What a core from before the rename left in its own folder stays there (R4): recovery and
/// reads never move, restore or delete anything in the legacy workspace data folder, even
/// with a copied-forward pending row for it.
#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn recovery_leaves_the_legacy_workspace_folder_alone() {
    use lil_core::state::Pending;
    use lil_platform::names::{LEGACY_BAK_MARKER, LEGACY_TMP_MARKER, LEGACY_WORKSPACE_DIR};
    let ws = workspace();
    let (dead, _child) = exited_pid();
    let dir = LEGACY_WORKSPACE_DIR;
    let temp = format!(".state.json{LEGACY_TMP_MARKER}{dead}-1");
    let backup = format!(".state.json{LEGACY_BAK_MARKER}{dead}-2");
    write_file(&ws.file(&format!("{dir}/{temp}")), b"half");
    write_file(
        &ws.file(&format!("{dir}/{backup}")),
        br#"{"active_project":"p2"}"#,
    );
    ws.state
        .set_pending(&Pending {
            path: format!("{dir}/state.json"),
            old_sha256: Some(sha(br#"{"active_project":"p2"}"#)),
            new_sha256: sha(br#"{"active_project":"p3"}"#),
            started_utc: 1,
        })
        .await
        .unwrap();
    let mut before = vec![temp, backup];
    before.sort();

    let store = ws.plain();
    let report = store.recover().await.expect("recover");
    assert!(report.temps_removed.is_empty(), "{report:?}");
    assert!(report.backups_restored.is_empty(), "{report:?}");
    assert_eq!(ws.names(dir), before);
    // A read of the missing file finds nothing and puts nothing back.
    let read = store.read(&p(&format!("{dir}/state.json"))).await.unwrap();
    assert_eq!(read, None);
    assert_eq!(ws.names(dir), before);
}

#[tokio::test(start_paused = true)] // spec: QA-066
async fn failpoints_arm_only_on_test_channels_of_non_stable_builds() {
    let test = Channel::Test("x".to_string());
    for (channel, build, want) in [
        (&Channel::Stable, "stable", false),
        (&Channel::Stable, "dev", false),
        (&Channel::Dev, "stable", false),
        (&Channel::Dev, "dev", false),
        (&test, "stable", false),
        (&test, "dev", true),
    ] {
        assert_eq!(armed(channel, build), want, "{channel} on a {build} build");
    }

    let env = TestEnv::new();
    let var = |spec: Option<&'static str>| {
        move |name: &str| {
            (name == "LIL_TEST_FAILPOINTS")
                .then(|| spec.map(str::to_string))
                .flatten()
        }
    };
    let points = FailPoints::from_env(&env.env, &var(Some("store.replace=io")));
    assert!(
        points.hit("store.replace").is_err(),
        "armed on a test channel"
    );
    assert!(points.hit("store.after_replace").is_ok());
    let none = FailPoints::from_env(&env.env, &var(None));
    assert!(none.hit("store.replace").is_ok());
    let typo = FailPoints::from_env(&env.env, &var(Some("store.replcae=io")));
    assert!(
        typo.hit("store.replace").is_ok(),
        "a spec that doesn't parse arms nothing"
    );
}

#[test] // spec: QA-003, QA-005
fn store_errors_map_to_appendix_d_codes() {
    use ErrorCategory as C;
    use RetryPolicy as R;
    let path = p("notes/a.txt");
    let cases = [
        (
            StoreError::HashConflict {
                path: path.clone(),
                backup: None,
                cloud_only: false,
                kept: Vec::new(),
            },
            "store.hash_conflict",
            C::DataConflict,
            R::AfterResolve,
        ),
        (
            StoreError::HashConflict {
                path: path.clone(),
                backup: None,
                cloud_only: true,
                kept: Vec::new(),
            },
            "store.hash_conflict",
            C::DataConflict,
            R::AfterResolve,
        ),
        (
            StoreError::Busy(path.clone()),
            "store.busy",
            C::Transient,
            R::SameIdempotencyKey,
        ),
        (
            StoreError::PathInvalid {
                path: "con.txt".into(),
                why: "reserved".into(),
            },
            "store.path_invalid",
            C::UserFixable,
            R::AfterResolve,
        ),
        (
            StoreError::DiskFull(path.clone()),
            "store.disk_full",
            C::UserFixable,
            R::AfterResolve,
        ),
        (
            StoreError::Io {
                path: "notes/a.txt".into(),
                why: "x".into(),
            },
            "store.io",
            C::Transient,
            R::SameIdempotencyKey,
        ),
        (
            StoreError::Locked(path.clone()),
            "store.locked",
            C::DataConflict,
            R::AfterResolve,
        ),
        (
            StoreError::GitBusy,
            "store.git_busy",
            C::Transient,
            R::SameIdempotencyKey,
        ),
        (
            StoreError::ReadOnly,
            "workspace.read_only",
            C::UserFixable,
            R::AfterResolve,
        ),
        (
            StoreError::NeedsRepair(path.clone()),
            "manifest.needs_repair",
            C::UserFixable,
            R::AfterResolve,
        ),
        (
            StoreError::YamlUnsupported("anchor".into()),
            "yaml.unsupported_edit",
            C::UserFixable,
            R::AfterResolve,
        ),
        (
            StoreError::VersionExists("1.0.0".into()),
            "version.exists",
            C::DataConflict,
            R::AfterResolve,
        ),
    ];
    for (error, code, category, retry) in cases {
        let body = error.to_error("trace-1");
        assert_eq!(body.code, code, "{error:?}");
        assert_eq!(body.category, category, "{code}");
        assert_eq!(body.retry, retry, "{code}");
        assert_eq!(body.trace_id, "trace-1");
        assert!(!body.message.is_empty(), "{code}");
    }
    let plain = StoreError::HashConflict {
        path: path.clone(),
        backup: None,
        cloud_only: false,
        kept: Vec::new(),
    }
    .to_error("t");
    let data = plain.data.as_ref().unwrap();
    assert!(data.get("backup").is_none() && data.get("kept").is_none());
    assert_eq!(data["cloud_only"], false);
    let backed = StoreError::HashConflict {
        path: path.clone(),
        backup: Some(p("notes/.a.txt.lil-bak-1-1")),
        cloud_only: false,
        kept: Vec::new(),
    }
    .to_error("t");
    assert_eq!(backed.data.unwrap()["backup"], "notes/.a.txt.lil-bak-1-1");
    let kept = StoreError::HashConflict {
        path,
        backup: None,
        cloud_only: false,
        kept: vec![
            p("notes/a.txt.lil-conflict-20270115T080000Z"),
            p("notes/a.txt.lil-conflict-20270115T080000Z-2"),
        ],
    }
    .to_error("t");
    assert_eq!(
        kept.data.as_ref().unwrap()["kept"],
        serde_json::json!([
            "notes/a.txt.lil-conflict-20270115T080000Z",
            "notes/a.txt.lil-conflict-20270115T080000Z-2"
        ])
    );
    assert!(
        kept.message.contains("a.txt.lil-conflict-20270115T080000Z"),
        "{}",
        kept.message
    );
}

#[test] // spec: CORE-016, CORE-004
fn idempotency_prune_uses_its_index() {
    let dir = tempfile::tempdir().unwrap();
    for scope in [StateScope::Suite, StateScope::Workspace] {
        let path = dir.path().join(format!("{scope:?}.db"));
        StateDb::open(&path, scope, no_fatal()).unwrap().close();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {PRUNE_DONE_SQL}"))
            .unwrap()
            .query_map([0_i64], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|d| d.contains("USING INDEX idempotency_prune")),
            "{scope:?}: {plan:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Review fixes (Task 6 fix round 1).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn second_outside_save_during_put_back_is_kept_visibly() {
    for fallback in [false, true] {
        let ws = workspace();
        put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
        let fp = if fallback {
            Arc::new(FailPoints::parse("store.replace=unsupported").unwrap())
        } else {
            FailPoints::none()
        };
        fp.on(
            "store.replace",
            outside_writer(ws.file("notes/a.txt"), "outside"),
        );
        // The user saves again after the swap, before the put-back runs: that save sits at the
        // target and the put-back moves it into the second backup.
        fp.on(
            "store.after_replace",
            outside_writer(ws.file("notes/a.txt"), "outside 2"),
        );
        let result = ws
            .store(fp)
            .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
            .await;
        let Err(StoreError::HashConflict {
            backup: None,
            cloud_only: false,
            kept: kept_copies,
            ..
        }) = &result
        else {
            panic!("fallback={fallback}: expected a conflict naming the kept save, got {result:?}");
        };
        assert_eq!(kept_copies.len(), 1, "fallback={fallback}: {kept_copies:?}");
        let kept_copy = &kept_copies[0];
        assert_eq!(
            ws.read(kept_copy.as_str()),
            b"outside 2",
            "fallback={fallback}"
        );
        let body = result.as_ref().unwrap_err().to_error("t");
        assert_eq!(
            body.data.as_ref().unwrap()["kept"],
            serde_json::json!([kept_copy.as_str()])
        );
        assert_eq!(ws.read("notes/a.txt"), b"outside", "fallback={fallback}");
        let kept: Vec<String> = ws
            .names("notes")
            .into_iter()
            .filter(|n| ws.read(&format!("notes/{n}")) == b"outside 2")
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "fallback={fallback}: {:?}",
            ws.names("notes")
        );
        assert!(
            kept[0].starts_with("a.txt.lil-conflict-"),
            "fallback={fallback}: the newest save is in a dot-file: {kept:?}"
        );
        assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
        assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
    }
}

/// A store whose write stopped between the fallback's renames: the target is missing and its
/// old content (`v1`) sits in the backup, with the pending row.
async fn stopped_between_renames(ws: &Ws) -> StoreError {
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let fp = Arc::new(FailPoints::parse("store.replace=unsupported").unwrap());
    fp.on(
        "store.between_renames",
        Box::new(|| Err(io::Error::other("the process stops here"))),
    );
    let err = ws
        .store(fp)
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await
        .expect_err("the write stops");
    assert!(!ws.file("notes/a.txt").exists());
    err
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn read_after_a_stopped_swap_resolves_the_backup() {
    let ws = workspace();
    let err = stopped_between_renames(&ws).await;
    assert!(
        err.to_string().contains("notes/.a.txt.lil-bak-"),
        "the stop names the backup: {err}"
    );
    let store = ws.plain();
    assert_eq!(
        store.read(&p("notes/a.txt")).await.unwrap().as_deref(),
        Some(&b"v1"[..]),
        "never 'not found' while a backup holds the content"
    );
    assert_eq!(ws.read("notes/a.txt"), b"v1");
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);

    // An edit after another stopped swap starts from the restored content, not from nothing.
    let ws = workspace();
    stopped_between_renames(&ws).await;
    ws.plain()
        .edit_text(&p("notes/a.txt"), &|t| Ok(format!("{t}+edit")))
        .await
        .expect("the edit");
    assert_eq!(ws.read("notes/a.txt"), b"v1+edit");
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn read_names_the_backup_when_it_cannot_be_restored() {
    let ws = workspace();
    stopped_between_renames(&ws).await;
    let backup = ws.leftovers("notes").pop().expect("the backup");
    let holder = HandleHolder::hold(&ws.file(&format!("notes/{backup}")), Share::None);
    let result = ws.plain().read(&p("notes/a.txt")).await;
    holder.release();
    let Err(e @ StoreError::Io { .. }) = &result else {
        panic!("expected store.io naming the backup, got {result:?}");
    };
    assert!(e.to_string().contains(&format!("notes/{backup}")), "{e}");
    assert_eq!(e.to_error("t").code, "store.io");
    assert!(ws.state.pending("notes/a.txt").await.unwrap().is_some());
}

/// The real probe, except that `long_name` expands the folder name `PROJEC~1` to `projects`,
/// as Windows does for an 8.3 short name, or fails for every path.
#[derive(Debug)]
struct ShortNameProbe {
    fail: bool,
}

impl Probe for ShortNameProbe {
    fn identity(&self, path: &Path) -> io::Result<FileIdentity> {
        SystemProbe.identity(path)
    }

    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }

    fn long_name(&self, path: &Path) -> io::Result<PathBuf> {
        if self.fail {
            return Err(io::Error::other("the long-name lookup failed"));
        }
        let long = SystemProbe.long_name(path)?;
        Ok(PathBuf::from(
            long.to_string_lossy().replace(r"\PROJEC~1", r"\projects"),
        ))
    }
}

#[tokio::test(start_paused = true)] // spec: CORE-026, CORE-040
async fn short_names_are_checked_by_their_long_form_and_fail_closed() {
    let ws = workspace();
    // A folder whose name is what an 8.3 short name of `projects` looks like; the fake probe
    // expands it as Windows would for the real short name.
    write_file(&ws.file("PROJEC~1/p/assets/a/v1.0.0/x.png"), b"sealed");
    write_file(&ws.file("notes/a.txt"), b"plain");
    let short = ws.store_with_probe(FailPoints::none(), Arc::new(ShortNameProbe { fail: false }));
    for rel in [
        "PROJEC~1/p/assets/a/v1.0.0/x.png",
        "PROJEC~1/p/assets/a/v1.0.0/new.png",
    ] {
        let result = short
            .write(&p(rel), b"new".to_vec(), Expect::Hash(sha(b"sealed")))
            .await;
        assert!(
            matches!(result, Err(StoreError::PathInvalid { .. })),
            "{rel}: {result:?}"
        );
    }
    assert_eq!(ws.read("PROJEC~1/p/assets/a/v1.0.0/x.png"), b"sealed");
    assert_eq!(ws.names("PROJEC~1/p/assets/a/v1.0.0"), ["x.png"]);
    put(&short, "notes/b.txt", "fine", Expect::New).await;

    // A lookup that fails refuses the write instead of letting it through.
    let failing = ws.store_with_probe(FailPoints::none(), Arc::new(ShortNameProbe { fail: true }));
    let result = failing
        .write(
            &p("notes/a.txt"),
            b"x".to_vec(),
            Expect::Hash(sha(b"plain")),
        )
        .await;
    assert!(
        matches!(result, Err(StoreError::PathInvalid { .. })),
        "{result:?}"
    );
    assert_eq!(ws.read("notes/a.txt"), b"plain");
}

#[tokio::test(start_paused = true)] // spec: ARCH-078, ARCH-074
async fn another_spelling_finds_the_generated_and_pending_rows() {
    let ws = workspace();
    let store = ws.plain();
    put(&store, "notes/a.txt", "v1", Expect::New).await;
    assert_eq!(
        ws.state.generated("Notes/A.TXT").await.unwrap(),
        Some(sha(b"v1"))
    );
    // The core's own file, under another spelling, is still the core's.
    put(&store, "Notes/A.txt", "v2", Expect::Generated).await;
    assert_eq!(ws.read("notes/a.txt"), b"v2");
    assert!(
        store
            .accepts_as_own(&p("NOTES/a.TXT"), &sha(b"v2"))
            .await
            .unwrap()
    );

    // A pending row left under one spelling is found under another.
    let crashed = ws.store(Arc::new(
        FailPoints::parse("store.after_replace=io").unwrap(),
    ));
    let result = crashed
        .write(&p("notes/a.txt"), b"v3".to_vec(), Expect::Generated)
        .await;
    assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");
    let pending = ws
        .state
        .pending("NOTES/A.TXT")
        .await
        .unwrap()
        .expect("the pending row");
    assert_eq!(pending.new_sha256, sha(b"v3"));
    assert!(
        store
            .accepts_as_own(&p("Notes/a.txt"), &sha(b"v3"))
            .await
            .unwrap()
    );
    put(&store, "NOTES/A.TXT", "v4", Expect::Generated).await;
    assert_eq!(ws.read("notes/a.txt"), b"v4");
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn unchecked_backup_is_named_and_left_for_recovery() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let held: Arc<Mutex<Option<HandleHolder>>> = Arc::default();
    let fp = FailPoints::none();
    // After the swap someone holds the backup with no sharing, so it can't be checked.
    let (slot, dir) = (held.clone(), ws.file("notes"));
    fp.on(
        "store.after_replace",
        Box::new(move || {
            let backup = std::fs::read_dir(&dir)?
                .map(|e| e.unwrap().path())
                .find(|p| p.to_string_lossy().contains(".lil-bak-"))
                .expect("the backup");
            *slot.lock().unwrap() = Some(HandleHolder::hold(&backup, Share::None));
            Ok(())
        }),
    );
    let result = ws
        .store(fp)
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    let Err(StoreError::HashConflict {
        backup: Some(backup),
        ..
    }) = &result
    else {
        panic!("expected the unchecked backup to be named, got {result:?}");
    };
    assert!(
        backup.as_str().starts_with("notes/.a.txt.lil-bak-"),
        "{backup}"
    );
    assert!(ws.state.pending("notes/a.txt").await.unwrap().is_some());
    held.lock().unwrap().take().unwrap().release();

    let report = ws.plain().recover().await.unwrap();
    assert_eq!(
        report.backups_removed,
        std::slice::from_ref(backup),
        "it held the old content"
    );
    assert_eq!(ws.read("notes/a.txt"), b"v2");
    assert_eq!(
        ws.state.generated("notes/a.txt").await.unwrap(),
        Some(sha(b"v2"))
    );
    assert_eq!(ws.names("notes"), ["a.txt"]);
}

// ---------------------------------------------------------------------------------------------
// Review fixes (Task 6 fix round 2, ruling Q14).
// ---------------------------------------------------------------------------------------------

/// A file symbolic link at `link` to `target`, or `None` where this account may not create one
/// (no Developer Mode and no elevation: error 1314).
fn file_symlink(target: &Path, link: &Path) -> Option<()> {
    match std::os::windows::fs::symlink_file(target, link) {
        Ok(()) => Some(()),
        Err(e) if e.raw_os_error() == Some(1314) => None,
        Err(e) => panic!("cannot create a symbolic link: {e}"),
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-074, CORE-040
async fn link_targets_are_refused_and_never_followed() {
    let ws = workspace();
    let outside = ws.root.parent().unwrap().join("outside-real.txt");
    write_file(&outside, b"real");
    write_file(&ws.file("notes/keep.txt"), b"x");
    let link = ws.file("notes/link.txt");
    let Some(()) = file_symlink(&outside, &link) else {
        eprintln!("skipped: this account can't create symbolic links");
        return;
    };
    for spec in ["", "store.replace=unsupported"] {
        let store = ws.store(Arc::new(FailPoints::parse(spec).unwrap()));
        let result = store
            .write(
                &p("notes/link.txt"),
                b"core".to_vec(),
                Expect::Hash(sha(b"real")),
            )
            .await;
        assert!(
            matches!(result, Err(StoreError::PathInvalid { .. })),
            "{spec}: {result:?}"
        );
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"real",
            "{spec}: the real file changed"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{spec}: the link itself is still there"
        );
        assert!(
            ws.leftovers("notes").is_empty(),
            "{spec}: {:?}",
            ws.names("notes")
        );
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-074, ARCH-078
async fn non_ascii_case_variant_finds_the_pending_row() {
    let ws = workspace();
    put(&ws.plain(), "notes/Ärger.txt", "v1", Expect::New).await;
    let fp = Arc::new(FailPoints::parse("store.replace=unsupported").unwrap());
    fp.on(
        "store.between_renames",
        Box::new(|| Err(io::Error::other("the process stops here"))),
    );
    let stopped = ws
        .store(fp)
        .write(&p("notes/Ärger.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    assert!(stopped.is_err());
    assert!(!ws.file("notes/Ärger.txt").exists());
    assert!(
        ws.state.pending("notes/ärger.txt").await.unwrap().is_some(),
        "the row under another spelling"
    );
    assert_eq!(
        ws.state.generated("NOTES/äRGER.TXT").await.unwrap(),
        Some(sha(b"v1"))
    );
    let store = ws.plain();
    assert_eq!(
        store.read(&p("notes/ärger.txt")).await.unwrap().as_deref(),
        Some(&b"v1"[..]),
        "the read under the other spelling recovers the content"
    );
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(ws.state.pending("notes/Ärger.txt").await.unwrap(), None);
}

#[allow(clippy::disallowed_methods)]
fn make_read_only(path: &Path) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms).unwrap();
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn read_only_target_is_refused_on_both_paths() {
    for spec in ["", "store.replace=unsupported"] {
        let ws = workspace();
        put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
        make_read_only(&ws.file("notes/a.txt"));
        let result = ws
            .store(Arc::new(FailPoints::parse(spec).unwrap()))
            .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
            .await;
        let Err(e @ StoreError::Io { .. }) = &result else {
            panic!("{spec:?}: a read-only target is store.io, got {result:?}");
        };
        assert!(e.to_string().contains("read-only"), "{spec:?}: {e}");
        assert_eq!(ws.read("notes/a.txt"), b"v1", "{spec:?}");
        assert!(
            std::fs::metadata(ws.file("notes/a.txt"))
                .unwrap()
                .permissions()
                .readonly(),
            "{spec:?}: the attribute stays"
        );
        assert_eq!(ws.names("notes"), ["a.txt"], "{spec:?}");
        assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
    }
}

/// The real probe, except that one path reports `FILE_ATTRIBUTE_REPARSE_POINT` with the tag
/// `tag`: a file symbolic link (`IO_REPARSE_TAG_SYMLINK`) or a hydrated cloud file
/// (`IO_REPARSE_TAG_CLOUD_1`), which is a reparse point but not a link.
#[derive(Debug)]
struct ReparseProbe {
    path: PathBuf,
    tag: u32,
}

impl Probe for ReparseProbe {
    fn identity(&self, path: &Path) -> io::Result<FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if path == self.path {
            id.attributes |= 0x400; // FILE_ATTRIBUTE_REPARSE_POINT
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }

    fn reparse_tag(&self, path: &Path) -> io::Result<Option<u32>> {
        if path == self.path {
            Ok(Some(self.tag))
        } else {
            SystemProbe.reparse_tag(path)
        }
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn a_link_at_the_target_is_refused_at_the_hash_check() {
    let ws = workspace();
    write_file(&ws.file("notes/link.txt"), b"through the link");
    let probe = Arc::new(ReparseProbe {
        path: ws.file("notes/link.txt"),
        tag: 0xA000_000C, // IO_REPARSE_TAG_SYMLINK
    });
    let store = ws.store_with_probe(
        Arc::new(FailPoints::parse("store.replace=unsupported").unwrap()),
        probe,
    );
    let result = store
        .write(
            &p("notes/link.txt"),
            b"core".to_vec(),
            Expect::Hash(sha(b"through the link")),
        )
        .await;
    let Err(e @ StoreError::PathInvalid { .. }) = &result else {
        panic!("a link target is store.path_invalid, got {result:?}");
    };
    assert!(e.to_string().contains("link"), "{e}");
    assert_eq!(e.to_error("t").code, "store.path_invalid");
    assert_eq!(ws.read("notes/link.txt"), b"through the link");
    assert_eq!(ws.names("notes"), ["link.txt"]);
    for refused in [
        store.read(&p("notes/link.txt")).await.map(drop),
        store
            .edit_text(&p("notes/link.txt"), &|t| Ok(format!("{t}!")))
            .await
            .map(drop),
    ] {
        assert!(
            matches!(refused, Err(StoreError::PathInvalid { .. })),
            "{refused:?}"
        );
    }

    // A hydrated cloud file is a reparse point too, but holds its own data: written as usual.
    let cloud = ws.store_with_probe(
        FailPoints::none(),
        Arc::new(ReparseProbe {
            path: ws.file("notes/link.txt"),
            tag: 0x9000_101A, // IO_REPARSE_TAG_CLOUD_1
        }),
    );
    put(
        &cloud,
        "notes/link.txt",
        "core",
        Expect::Hash(sha(b"through the link")),
    )
    .await;
    assert_eq!(ws.read("notes/link.txt"), b"core");
}

#[tokio::test(start_paused = true)] // spec: ARCH-074
async fn put_back_names_every_outside_version_it_sets_aside() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let target = ws.file("notes/a.txt");
    let fp = Arc::new(FailPoints::parse("store.replace=unsupported").unwrap());
    // First outside edit before the swap, a second save after it (moved into the second
    // backup by the put-back), and a third file at the freed name before the first backup
    // goes back.
    fp.on("store.replace", outside_writer(target.clone(), "outside 1"));
    fp.on(
        "store.after_replace",
        outside_writer(target.clone(), "outside 2"),
    );
    fp.on("store.put_back", outside_writer(target, "outside 3"));
    let result = ws
        .store(fp)
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    let Err(StoreError::HashConflict {
        backup: None, kept, ..
    }) = &result
    else {
        panic!("expected a conflict naming both copies, got {result:?}");
    };
    let contents: Vec<Vec<u8>> = kept.iter().map(|k| ws.read(k.as_str())).collect();
    assert_eq!(contents, [b"outside 1".to_vec(), b"outside 2".to_vec()]);
    assert_eq!(ws.read("notes/a.txt"), b"outside 3");
    assert_eq!(ws.conflicts("notes").len(), 2, "{:?}", ws.names("notes"));
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
}

/// A junction at `link` to the folder `target` (no privilege needed).
#[allow(clippy::disallowed_methods)]
fn junction(link: &Path, target: &Path) {
    lil_platform::fs::create_junction(link, target)
        .unwrap_or_else(|e| panic!("cannot create a junction at {}: {e}", link.display()));
}

#[tokio::test(start_paused = true)] // spec: ARCH-074, CORE-040
async fn a_junction_at_the_target_is_refused_through_the_real_reparse_tag() {
    let ws = workspace();
    let outside = ws.root.parent().unwrap().join("outside-folder");
    write_file(&outside.join("inside.txt"), b"outside");
    write_file(&ws.file("notes/keep.txt"), b"x");
    junction(&ws.file("notes/link"), &outside);
    let store = ws.plain();
    let rel = p("notes/link");
    let write = store.write(&rel, b"core".to_vec(), Expect::New).await;
    let generated = store.write(&rel, b"core".to_vec(), Expect::Generated).await;
    let read = store.read(&rel).await.map(drop);
    let edit = store
        .edit_text(&rel, &|t| Ok(format!("{t}!")))
        .await
        .map(drop);
    for (what, result) in [
        ("write", write.map(drop)),
        ("generated write", generated.map(drop)),
        ("read", read),
        ("edit", edit),
    ] {
        let Err(e @ StoreError::PathInvalid { .. }) = &result else {
            panic!("{what}: a junction is store.path_invalid, got {result:?}");
        };
        assert!(e.to_string().contains("link or junction"), "{what}: {e}");
    }
    assert_eq!(
        std::fs::read(outside.join("inside.txt")).unwrap(),
        b"outside"
    );
    assert_eq!(
        lil_platform::fs::reparse_tag(&ws.file("notes/link")).unwrap(),
        Some(lil_platform::fs::IO_REPARSE_TAG_MOUNT_POINT),
        "the junction is still there"
    );
    assert_eq!(ws.names("notes"), ["keep.txt", "link"]);
}

/// Whether the volume holding `dir` treats the names `a` and `b` as one file.
fn volume_says_same(dir: &Path, a: &str, b: &str) -> bool {
    write_file(&dir.join(format!("probe-{a}")), b"x");
    dir.join(format!("probe-{b}")).exists()
}

#[tokio::test(start_paused = true)] // spec: CORE-040, ARCH-074
async fn path_keys_agree_with_the_volume() {
    let env = TestEnv::new();
    let cases = [
        ("a", "A"),
        ("\u{e4}", "\u{c4}"),     // ä Ä
        ("\u{1fb3}", "\u{1fbc}"), // ᾳ ᾼ: one name on NTFS
        ("\u{3c2}", "\u{3c3}"),   // ς σ: two names
        ("\u{131}", "I"),         // ı I
        ("\u{17f}", "S"),         // ſ S
        ("\u{b5}", "\u{39c}"),    // µ Μ
        ("\u{1c5}", "\u{1c4}"),   // ǅ Ǆ
        ("\u{10d0}", "\u{1c90}"), // Georgian
        ("\u{ab70}", "\u{13a0}"), // Cherokee
    ];
    for (i, (a, b)) in cases.iter().enumerate() {
        let dir = env.root().join(format!("case-{i}"));
        let same = volume_says_same(&dir, a, b);
        assert_eq!(
            lil_core::store::path_key(&format!("notes/{a}.txt"))
                == lil_core::store::path_key(&format!("NOTES/{b}.TXT")),
            same,
            "{a:?} / {b:?}: the volume says same={same}"
        );
    }
}

/// The real probe, except that the first data open of a `.lil-bak-` file reads a copy of it as
/// it was, while an outside writer saves `then` into the real one right after: what was
/// hashed is no longer what is there when recovery removes it.
#[derive(Debug)]
struct SaveAfterHash {
    copy: PathBuf,
    then: Vec<u8>,
    done: AtomicBool,
}

impl Probe for SaveAfterHash {
    fn identity(&self, path: &Path) -> std::io::Result<FileIdentity> {
        SystemProbe.identity(path)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        let backup = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains(".lil-bak-"));
        if backup && !self.done.swap(true, Ordering::SeqCst) {
            write_file(&self.copy, &std::fs::read(path)?);
            write_file(path, &self.then);
            return SystemProbe.open_read(&self.copy);
        }
        SystemProbe.open_read(path)
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-074, CORE-110
async fn recovery_never_deletes_a_backup_that_changed_after_its_hash() {
    let ws = workspace();
    put(&ws.plain(), "notes/a.txt", "v1", Expect::New).await;
    let crashed = ws.store(Arc::new(
        FailPoints::parse("store.after_replace=io").unwrap(),
    ));
    let result = crashed
        .write(&p("notes/a.txt"), b"v2".to_vec(), Expect::Generated)
        .await;
    assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");
    drop(crashed);
    // The backup holds the old bytes (rule 3 would remove it), but an outside save lands in
    // it between recovery's hash and its delete.
    let probe = Arc::new(SaveAfterHash {
        copy: ws._env.root().join("hashed-copy.txt"),
        then: b"saved outside".to_vec(),
        done: AtomicBool::new(false),
    });
    let store = ws.store_with_probe(FailPoints::none(), probe.clone());
    let report = store.recover().await.expect("recover");
    assert!(probe.done.load(Ordering::SeqCst), "the save never happened");
    // The outside save is kept, visible at the target; the core's bytes are set aside.
    assert_eq!(ws.read("notes/a.txt"), b"saved outside");
    let conflicts = ws.conflicts("notes");
    assert_eq!(conflicts.len(), 1, "{:?}", ws.names("notes"));
    assert_eq!(ws.read(&format!("notes/{}", conflicts[0])), b"v2");
    assert!(report.backups_removed.is_empty(), "{report:?}");
    assert!(ws.leftovers("notes").is_empty(), "{:?}", ws.names("notes"));
    assert_eq!(ws.state.pending("notes/a.txt").await.unwrap(), None);
}

#[tokio::test(start_paused = true)] // spec: ARCH-002, CORE-040
async fn a_linked_folder_above_the_target_is_refused_for_every_write() {
    // Minor 3 of the plan-2b review: plain writes, edits and manifest edits check every
    // folder on the way, as imports and sign-offs do, so a junction under the workspace never
    // lets a write land outside it.
    let ws = workspace();
    let outside = ws.root.parent().unwrap().join("outside-target");
    write_file(&outside.join("progress.md"), b"# Progress\n\n## Done\n");
    write_file(&ws.file("projects/keep.txt"), b"x");
    junction(&ws.file("projects/linked"), &outside);
    let store = ws.plain();
    let new = store
        .write(&p("projects/linked/new.txt"), b"core".to_vec(), Expect::New)
        .await
        .map(drop);
    let edit = store
        .edit_text(&p("projects/linked/progress.md"), &|t| {
            Ok(format!("{t}- 2027-01-15: an entry\n"))
        })
        .await
        .map(drop);
    let manifest = store
        .edit_manifest(&p("projects/linked/assets/kite/kite.manifest.json"), &|m| {
            m.set("status", serde_json::Value::from("planned"));
            Ok(())
        })
        .await
        .map(drop);
    for (what, result) in [("write", new), ("edit", edit), ("manifest edit", manifest)] {
        let Err(e @ StoreError::PathInvalid { path, .. }) = &result else {
            panic!("{what}: a linked folder is store.path_invalid, got {result:?}");
        };
        assert_eq!(path, "projects/linked", "{what}: {e}");
        assert!(e.to_string().contains("link or junction"), "{what}: {e}");
    }
    // Nothing landed outside the workspace.
    assert_eq!(
        std::fs::read(outside.join("progress.md")).unwrap(),
        b"# Progress\n\n## Done\n"
    );
    assert!(!outside.join("new.txt").exists());
    assert!(!outside.join("assets").exists());
}

/// A manifest `locks` entry recorded with an 8.3 short alias still locks the file a write names
/// by its long name (a file entry, or a folder entry above a new file), and an entry recorded
/// by the long name locks the file written by its alias: both sides are compared by their long
/// forms. An unlocked sibling is written as before.
#[tokio::test(start_paused = true)] // spec: CORE-035
async fn writes_resolve_short_alias_lock_entries() {
    use serde_json::json;
    const ASSET: &str = "projects/sketchbook/assets/crane";
    let ws = workspace();
    let store = ws.plain();
    let manifest = |locks: serde_json::Value| {
        let mut m = Manifest::new(
            "sketchbook",
            "crane",
            "image",
            "00000000-0000-4000-8000-0000000000aa",
        );
        m.set("locks", locks);
        write_file(
            &ws.file(&format!("{ASSET}/crane.manifest.json")),
            m.to_text().as_bytes(),
        );
    };
    let file = format!("{ASSET}/prompts/crane_prompt.txt");
    write_file(&ws.file(&file), b"a crane");
    let folder = format!("{ASSET}/promptsfolder");
    write_file(&ws.file(&format!("{folder}/old.txt")), b"old");
    let short_file = lil_platform::fs::short_name(&ws.file(&file)).unwrap();
    let short_folder = lil_platform::fs::short_name(&ws.file(&folder)).unwrap();
    if short_file == ws.file(&file) || short_folder == ws.file(&folder) {
        println!("8.3 names are off on this volume: the short-alias cases are skipped");
        return;
    }
    let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let over = || Expect::Hash(sha256_bytes(b"a crane"));

    // A file entry by its alias, written by the long name.
    manifest(json!([format!("prompts/{}", name(&short_file))]));
    let written = store.write(&p(&file), b"over".to_vec(), over()).await;
    assert!(
        matches!(&written, Err(StoreError::Locked(_))),
        "{written:?}"
    );
    assert_eq!(ws.read(&file), b"a crane");

    // A folder entry by its alias, and a new file beneath it by the long name.
    manifest(json!([format!("{}/", name(&short_folder))]));
    let new = format!("{folder}/new.txt");
    let written = store.write(&p(&new), b"new".to_vec(), Expect::New).await;
    assert!(
        matches!(&written, Err(StoreError::Locked(_))),
        "{written:?}"
    );
    assert!(!ws.file(&new).exists());

    // The reverse: a long-name entry, written by the alias.
    manifest(json!(["prompts/crane_prompt.txt"]));
    let alias = format!("{ASSET}/prompts/{}", name(&short_file));
    let written = store.write(&p(&alias), b"over".to_vec(), over()).await;
    assert!(
        matches!(&written, Err(StoreError::Locked(_))),
        "{written:?}"
    );
    assert_eq!(ws.read(&file), b"a crane");

    // A sibling no entry covers.
    store
        .write(
            &p(&format!("{ASSET}/prompts/other.txt")),
            b"free".to_vec(),
            Expect::New,
        )
        .await
        .unwrap();
}

/// Moved here from the store's unit tests, which name no `state` type (CORE-010).
#[tokio::test(start_paused = true)] // spec: ARCH-083
async fn git_gate_sees_index_lock_without_the_watcher() {
    let env = TestEnv::new();
    let root = env.make_workspace("gate");
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
    let lock = root.join(".git").join("index.lock");
    write_file(&lock, b"");
    // No watcher sets the flag: the file alone holds writes back.
    assert!(store.git_gate().is_busy());
    let rel = WsPath::parse("notes/after.txt").unwrap();
    let waiting = {
        let (store, rel) = (store.clone(), rel.clone());
        tokio::spawn(async move { store.write(&rel, b"after".to_vec(), Expect::New).await })
    };
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(
        !waiting.is_finished(),
        "a write waits while index.lock exists"
    );
    assert!(!rel.abs(&root).exists(), "nothing is written meanwhile");
    // Git ends a command by renaming `index.lock` onto `index`.
    fsops::move_new(&lock, &root.join(".git").join("index")).unwrap();
    assert!(!store.git_gate().is_busy());
    let written = waiting
        .await
        .unwrap()
        .expect("the write goes on once the lock is gone");
    assert_eq!(written.path, rel);
    assert_eq!(std::fs::read(rel.abs(&root)).unwrap(), b"after");
}
