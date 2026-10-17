//! The watcher on a real NTFS folder (ARCH-016, ARCH-073, ARCH-079, ARCH-083, ARCH-085,
//! CORE-034, CORE-038): outside edits are recorded as human edits and never overwritten, work
//! files are only indexed, private folders aren't watched, broken manifests need repair, LFS
//! pointers aren't fetched, git pauses writes until its rescan, folder moves reconcile their
//! subtree, unstable and locked files are parked, a human edit is recorded once, removed files
//! are missing until they come back, the suite's own moves wait for their version lock, and a
//! workspace that can't be watched is still served.
//!
//! Tests wait on the watcher with the fence (a `.worldwright\fence\<name>` file, then
//! `Watcher::fence`), on published events, or on a condition polled until a failure deadline,
//! never on a sleep before an assertion (QA-031). Fixture files are written with
//! `ww_testkit::files::write_file`; the other outside-writer steps carry their own allowance.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use ww_core::events::EventBus;
use ww_core::index::FileState;
use ww_core::pipeline::asset_detail;
use ww_core::state::StateRegistry;
use ww_core::store::{Expect, FailPoints, StoreError, WsPath};
use ww_core::watch::{WatchConfig, Watcher};
use ww_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use ww_platform::fs::{Probe, Share, SystemProbe};
use ww_platform::{Clock, Env};
use ww_testkit::files::write_file;
use ww_testkit::{
    CoreFixture, FakeClock, GitOps, HandleHolder, RawPipe, SeqIds, TestEnv, failpoints,
    sha256_bytes,
};

/// A failure deadline only, never a measurement.
const DEADLINE: Duration = Duration::from_secs(60);
const CRANE: &str = "chr_paper_crane";
const BASE: &str = "projects/sketchbook/assets/chr_paper_crane";
const SEALED: &str = "projects/sketchbook/assets/chr_paper_crane/v1.0.0/crane_b.png";
const SEALED_SHA256: &str = "aaae4dbdb0e697350dac691b26928d34a69061cf320b175c257c103fcaa32766";
/// A file of the crane's open run.
const CANDIDATE: &str =
    "projects/sketchbook/assets/chr_paper_crane/candidates/r_20261003_1400/crane_c.png";
const CANDIDATE_SHA256: &str = "756151086e8b16ebaf3ccff573e1cad2c2cd958ed19d91ce32b2e30a1ca55f90";
const CRANE_MANIFEST: &str =
    "projects/sketchbook/assets/chr_paper_crane/chr_paper_crane.manifest.json";
const LAMP_MANIFEST: &str =
    "projects/sketchbook/assets/prop_signal_lamp/prop_signal_lamp.manifest.json";
/// `FakeClock`'s UTC date.
const TODAY: &str = "2027-01-15";

// ---------------------------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------------------------

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

fn canonical(path: &Path) -> PathBuf {
    let c = std::fs::canonicalize(path).unwrap();
    let text = c.to_string_lossy().into_owned();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Clears a file's read-only bit and writes it, as an editor would.
#[allow(clippy::disallowed_methods)]
fn edit_outside(path: &Path, bytes: &[u8]) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as an editor would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
    write_file(path, bytes);
}

/// Deletes a file as an outside program would (its read-only bit cleared first).
#[allow(clippy::disallowed_methods)]
fn delete_outside(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as Explorer would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
    std::fs::remove_file(path).unwrap();
}

/// Moves a file or folder as an outside program would.
#[allow(clippy::disallowed_methods)]
fn move_outside(from: &Path, to: &Path) {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::rename(from, to).unwrap();
}

/// Creates a folder as an outside program would.
#[allow(clippy::disallowed_methods)]
fn make_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}

/// An in-process hub with one open workspace.
struct Core {
    env: TestEnv,
    ws: PathBuf,
    cfg: WatchConfig,
    hub: Arc<WorkspaceHub>,
    events: Arc<EventBus>,
    open: Arc<OpenWorkspace>,
}

fn workspace_ref(ws: &Path) -> WorkspaceRef {
    let path = canonical(ws);
    WorkspaceRef {
        id: Env::workspace_hash(&std::fs::canonicalize(ws).unwrap()),
        path,
    }
}

impl Core {
    /// A copy of the spec fixture workspace, opened.
    async fn fixture(cfg: WatchConfig) -> Core {
        let env = TestEnv::with_label("Ünïcödé space");
        let ws = env.copy_fixture_workspace("watch");
        Core::open(env, ws, cfg, FailPoints::none()).await
    }

    async fn open(env: TestEnv, ws: PathBuf, cfg: WatchConfig, fp: Arc<FailPoints>) -> Core {
        let clock: Arc<dyn Clock> = FakeClock::new();
        let events = Arc::new(EventBus::new(clock.clone()));
        let hub = WorkspaceHub::new(
            env.env.clone(),
            Arc::new(StateRegistry::open(&env.env, no_fatal()).expect("the suite state.db")),
            events.clone(),
            clock,
            Arc::new(SeqIds::new()),
            fp,
            Arc::new(SystemProbe) as Arc<dyn Probe>,
            cfg.clone(),
        );
        let open = hub
            .open(&workspace_ref(&ws))
            .await
            .expect("the workspace opens");
        open.index.ready().await;
        Core {
            env,
            ws: canonical(&ws),
            cfg,
            hub,
            events,
            open,
        }
    }

    fn watch(&self) -> &Arc<Watcher> {
        self.open
            .watch
            .as_ref()
            .expect("a read-write workspace has a watcher")
    }

    fn path(&self, rel: &str) -> PathBuf {
        file(&self.ws, rel)
    }

    /// The watcher fence: a new `.worldwright\fence\<name>`, then `Watcher::fence`.
    async fn fence(&self) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let name = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        write_file(
            &self.ws.join(".worldwright").join("fence").join(&name),
            name.as_bytes(),
        );
        tokio::time::timeout(DEADLINE, self.watch().fence(&name))
            .await
            .expect("the fence returns");
    }

    /// `asset.get` of a sketchbook asset.
    async fn detail(&self, asset: &str) -> Value {
        let detail = asset_detail(
            &self.open.store,
            &self.open.index,
            &self.open.ws.id,
            "sketchbook",
            asset,
        )
        .await
        .unwrap_or_else(|e| panic!("asset.get {asset}: {e:?}"));
        serde_json::to_value(detail).unwrap()
    }

    /// The `files[].state` of `inside` (asset-relative) in `detail`.
    fn state(detail: &Value, inside: &str) -> String {
        detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == inside)
            .and_then(|f| f["state"].as_str())
            .unwrap_or_else(|| panic!("asset.get lists no {inside}: {detail}"))
            .to_string()
    }

    /// The index row's hash and state of `rel`.
    async fn row(&self, rel: &str) -> Option<(Option<String>, FileState)> {
        self.open
            .index
            .files_of_path(&p(rel))
            .await
            .unwrap()
            .map(|r| (r.sha256, r.state))
    }

    fn asset_topic(&self, asset: &str) -> String {
        format!("asset:ww://{}/sketchbook/{asset}", self.open.ws.id)
    }

    /// Every event published on `topic` so far (replayed from its history).
    async fn published(&self, topic: &str) -> Vec<Value> {
        published(&self.events, topic).await
    }

    /// Polls `check` until it is true, failing at the deadline.
    async fn until<F, Fut>(&self, what: &str, mut check: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = std::time::Instant::now() + DEADLINE;
        while !check().await {
            assert!(
                std::time::Instant::now() < deadline,
                "{what} didn't happen within {DEADLINE:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn published(events: &EventBus, topic: &str) -> Vec<Value> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let client = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
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

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| e["kind"].as_str().map(str::to_string))
        .collect()
}

/// The `human_edits` entries of `detail` for `inside`.
fn edits_of(detail: &Value, inside: &str) -> Vec<Value> {
    detail["human_edits"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["path"] == inside)
        .cloned()
        .collect()
}

/// The crane manifest's `human_edits` entries for `inside`, read from disk (while a sealed
/// file is held, `asset.get` can't hash its version folder).
fn crane_edits_on_disk(core: &Core, inside: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(core.path(CRANE_MANIFEST)).unwrap();
    let m: Value = serde_json::from_str(&text).unwrap();
    edits_of(&m, inside)
}

fn fast() -> WatchConfig {
    WatchConfig::default()
}

// ---------------------------------------------------------------------------------------------
// Human edits, work files, private folders, manifests, LFS.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-034, ARCH-079
async fn outside_edit_to_candidate_or_sealed_file_is_recorded_never_overwritten() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let (sealed, candidate) = (b"an outside edit of v1.0.0", b"an outside candidate edit");
    edit_outside(&core.path(SEALED), sealed);
    edit_outside(&core.path(CANDIDATE), candidate);
    core.fence().await;

    let detail = core.detail(CRANE).await;
    let on_sealed = edits_of(&detail, "v1.0.0/crane_b.png");
    assert_eq!(
        on_sealed,
        [
            json!({"path": "v1.0.0/crane_b.png", "sha256": sha256_bytes(sealed),
                "previous_sha256": SEALED_SHA256, "date": TODAY})
        ],
        "{detail}"
    );
    let on_candidate = edits_of(&detail, "candidates/r_20261003_1400/crane_c.png");
    assert_eq!(on_candidate.len(), 1, "{detail}");
    assert_eq!(on_candidate[0]["sha256"], sha256_bytes(candidate));
    assert_eq!(on_candidate[0]["previous_sha256"], CANDIDATE_SHA256);
    assert_eq!(Core::state(&detail, "v1.0.0/crane_b.png"), "human_edit");
    assert_eq!(
        Core::state(&detail, "candidates/r_20261003_1400/crane_c.png"),
        "human_edit"
    );
    let asset_events = core.published(&core.asset_topic(CRANE)).await;
    assert_eq!(
        kinds(&asset_events)
            .iter()
            .filter(|k| *k == "human_edit")
            .count(),
        2,
        "{asset_events:?}"
    );

    // Never overwritten: the files keep the outside bytes, and a write that expects the
    // recorded content is refused.
    assert_eq!(std::fs::read(core.path(SEALED)).unwrap(), sealed);
    assert_eq!(std::fs::read(core.path(CANDIDATE)).unwrap(), candidate);
    let refused = core
        .open
        .store
        .write(
            &p(CANDIDATE),
            b"the core's".to_vec(),
            Expect::Hash(CANDIDATE_SHA256.into()),
        )
        .await;
    assert!(
        matches!(refused, Err(StoreError::HashConflict { .. })),
        "{refused:?}"
    );
    assert_eq!(std::fs::read(core.path(CANDIDATE)).unwrap(), candidate);
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079
async fn work_folder_edits_are_indexed_not_human_edits() {
    let core = Core::fixture(fast()).await;
    let work = format!("{BASE}/work/sketch.psd");
    write_file(&core.path(&work), b"first draft");
    core.fence().await;
    assert_eq!(
        core.row(&work).await,
        Some((Some(sha256_bytes(b"first draft")), FileState::Ok))
    );
    write_file(&core.path(&work), b"second draft");
    core.fence().await;
    assert_eq!(
        core.row(&work).await,
        Some((Some(sha256_bytes(b"second draft")), FileState::Ok))
    );
    let detail = core.detail(CRANE).await;
    assert_eq!(detail["human_edits"], json!([]), "{detail}");
    assert_eq!(Core::state(&detail, "work/sketch.psd"), "ok");
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079
async fn staging_trash_and_git_internals_are_not_watched() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let unwatched = [
        ".worldwright/staging/sh_00000000000000a1/a.png",
        ".trash/2027-01-15/te_00000000000000a1/payload/a.txt",
        ".git/objects/ab/cdef0123",
        ".git/index",
        "projects/sketchbook/.git/index.lock",
        "projects/sketchbook/.git/HEAD",
    ];
    for rel in unwatched {
        write_file(&core.path(rel), b"private");
    }
    write_file(&core.path("notes/seen.txt"), b"watched");
    core.fence().await;
    for rel in unwatched {
        assert_eq!(core.row(rel).await, None, "{rel} is not indexed");
    }
    assert!(core.row("notes/seen.txt").await.is_some());
    // Neither a `.git` folder deeper down nor `.git/index` is git working on the workspace.
    let workspace = core.published("workspace").await;
    assert!(
        !kinds(&workspace).contains(&"git_paused".to_string()),
        "{workspace:?}"
    );
    assert!(!core.open.store.git_gate().is_busy());
    // Notifying them changes nothing either.
    let report = core
        .watch()
        .notify(unwatched.iter().map(|r| p(r)).collect())
        .await;
    assert!(report.reconciled.is_empty(), "{report:?}");
    assert_eq!(report.ignored.len(), unwatched.len(), "{report:?}");
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079, ARCH-076
async fn invalid_manifest_is_needs_repair_and_edits_refuse() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    write_file(&core.path(LAMP_MANIFEST), b"{");
    core.fence().await;

    let detail = core.detail("prop_signal_lamp").await;
    assert_eq!(detail["needs_repair"], true, "{detail}");
    assert_eq!(
        Core::state(&detail, "prop_signal_lamp.manifest.json"),
        "needs_repair"
    );
    let events = core.published(&core.asset_topic("prop_signal_lamp")).await;
    assert!(
        kinds(&events).contains(&"needs_repair".to_string()),
        "{events:?}"
    );
    // Edits refuse it, and nothing rewrites it.
    let refused = core
        .open
        .store
        .edit_manifest(&p(LAMP_MANIFEST), &|m| {
            m.push("human_edits", json!({"path": "x", "sha256": SEALED_SHA256}))
                .map_err(|e| StoreError::manifest_edit(&p(LAMP_MANIFEST), &e))
        })
        .await;
    let error = refused
        .map(|_| ())
        .expect_err("an edit of a broken manifest");
    assert_eq!(error.to_error("t").code, "manifest.needs_repair");
    assert_eq!(std::fs::read(core.path(LAMP_MANIFEST)).unwrap(), b"{");
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-085
async fn lfs_pointer_with_matching_oid_is_not_fetched() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let pointer = |oid: &str| {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 120\n")
    };
    edit_outside(&core.path(SEALED), pointer(SEALED_SHA256).as_bytes());
    // A pointer to other content than the run recorded is a human edit, by its oid.
    let other = sha256_bytes(b"other content");
    edit_outside(&core.path(CANDIDATE), pointer(&other).as_bytes());
    core.fence().await;

    let detail = core.detail(CRANE).await;
    assert_eq!(Core::state(&detail, "v1.0.0/crane_b.png"), "not_fetched");
    assert!(
        edits_of(&detail, "v1.0.0/crane_b.png").is_empty(),
        "{detail}"
    );
    let edits = edits_of(&detail, "candidates/r_20261003_1400/crane_c.png");
    assert_eq!(edits.len(), 1, "{detail}");
    assert_eq!(edits[0]["sha256"], other);
    assert_eq!(
        std::fs::read(core.path(SEALED)).unwrap(),
        pointer(SEALED_SHA256).as_bytes(),
        "a pointer is never replaced"
    );
}

// ---------------------------------------------------------------------------------------------
// Git.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-083
async fn index_lock_pauses_store_writes_until_the_rescan() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let ops = GitOps::begin(&core.ws);
    core.fence().await;
    assert!(kinds(&core.published("workspace").await).contains(&"git_paused".to_string()));
    assert!(core.open.store.git_gate().is_busy());

    let write = {
        let (store, events) = (core.open.store.clone(), core.events.clone());
        tokio::spawn(async move {
            let written = store
                .write(
                    &p("notes/after_git.txt"),
                    b"after git".to_vec(),
                    Expect::New,
                )
                .await;
            // What was published when the write came back.
            (written, kinds(&published(&events, "workspace").await))
        })
    };
    // A file git changes meanwhile is decided by the rescan after it.
    ops.write("notes/from_git.txt", b"checked out");
    core.fence().await;
    assert!(!write.is_finished(), "the write waits while git works");
    assert!(!core.path("notes/after_git.txt").exists());
    assert_eq!(
        core.row("notes/from_git.txt").await,
        None,
        "deferred to the rescan"
    );

    ops.finish();
    let (written, seen) = tokio::time::timeout(DEADLINE, write)
        .await
        .expect("the write ends once git has finished")
        .unwrap();
    written.expect("the write succeeds after the pause");
    let resumed = seen.iter().position(|k| k == "git_resumed");
    assert!(
        resumed.is_some(),
        "git_resumed before the write's answer: {seen:?}"
    );
    assert_eq!(
        std::fs::read(core.path("notes/after_git.txt")).unwrap(),
        b"after git"
    );
    assert_eq!(
        core.row("notes/from_git.txt").await,
        Some((Some(sha256_bytes(b"checked out")), FileState::Ok)),
        "the post-git rescan indexed it"
    );
    assert!(!core.open.store.git_gate().is_busy());
}

/// A store, an index and a watcher over `ws`, without a hub: no verification or maintenance
/// timers, so tokio's paused clock only moves to the watcher's own (its batch window and the
/// git quiet time) while the test waits for the folder's events.
async fn bare(
    ws: &Path,
) -> (
    Arc<ww_core::store::Store>,
    Arc<ww_core::index::Index>,
    Arc<Watcher>,
    Arc<EventBus>,
    String,
) {
    let root = canonical(ws);
    let ws_id = Env::workspace_hash(&std::fs::canonicalize(ws).unwrap());
    let state = Arc::new(
        ww_core::state::StateDb::open(
            &root.parent().unwrap().join("bare-local").join("state.db"),
            ww_core::state::StateScope::Workspace,
            no_fatal(),
        )
        .unwrap(),
    );
    let clock: Arc<dyn Clock> = FakeClock::new();
    let store = Arc::new(ww_core::store::Store::new(
        root.clone(),
        state,
        FailPoints::none(),
        clock.clone(),
        Arc::new(SeqIds::new()),
        Arc::new(SystemProbe),
    ));
    let index =
        Arc::new(ww_core::index::Index::open_in_memory(&root, Arc::new(SystemProbe)).unwrap());
    store.set_sink(index.clone());
    let events = Arc::new(EventBus::new(clock));
    let watcher = Watcher::start(
        store.clone(),
        index.clone(),
        events.clone(),
        ws_id.clone(),
        WatchConfig::default(),
    )
    .expect("the folder can be watched");
    index.start_first();
    index.ready().await;
    (store, index, watcher, events, ws_id)
}

/// Writes a new fence file and waits for the watcher to pass it (no timeout: on the paused
/// clock a timeout would fire while the folder's events are still on their way).
async fn bare_fence(ws: &Path, watcher: &Watcher) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let name = format!("bare-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    write_file(&ws.join(".worldwright").join("fence").join(&name), b"f");
    watcher.fence(&name).await;
}

#[tokio::test(start_paused = true)] // spec: ARCH-083, CORE-034
async fn post_git_human_edit_is_recorded_without_waiting_for_the_gate() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = canonical(&env.copy_fixture_workspace("post-git"));
    let (store, index, watcher, events, ws_id) = bare(&ws).await;
    bare_fence(&ws, &watcher).await;
    let ops = GitOps::begin(&ws);
    bare_fence(&ws, &watcher).await;
    assert!(store.git_gate().is_busy());
    let edited = b"edited while git worked";
    edit_outside(&file(&ws, SEALED), edited);
    bare_fence(&ws, &watcher).await;
    let crane_edits = || async {
        let detail = asset_detail(&store, &index, &ws_id, "sketchbook", CRANE)
            .await
            .unwrap();
        edits_of(&serde_json::to_value(detail).unwrap(), "v1.0.0/crane_b.png")
    };
    assert!(crane_edits().await.is_empty(), "deferred while git works");

    // Live events from here on.
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    // The failure deadline runs on the real clock (a tokio timeout would fire on the paused
    // one while the folder's events are still on their way).
    let deadline = tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DEADLINE);
        let _ = deadline.blocking_send("deadline".to_string());
    });
    events.attach(9_000, tx);
    let topics = [
        "workspace".to_string(),
        format!("asset:ww://{ws_id}/sketchbook/{CRANE}"),
    ];
    events
        .subscribe(9_000, &topics, &std::collections::BTreeMap::new())
        .await;
    let finished = tokio::time::Instant::now();
    ops.finish();
    let mut order = Vec::new();
    while !order.contains(&"human_edit".to_string()) {
        let line = rx.recv().await.expect("the event stream goes on");
        assert_ne!(
            line, "deadline",
            "no human_edit within {DEADLINE:?}: {order:?}"
        );
        let event: Value = serde_json::from_str(&line).unwrap();
        if let Some(kind) = event["params"]["data"]["kind"].as_str() {
            order.push(kind.to_string());
        }
    }
    let waited = finished.elapsed();
    let resumed = order.iter().position(|k| k == "git_resumed");
    let recorded = order.iter().position(|k| k == "human_edit");
    assert!(
        resumed.is_some() && resumed < recorded,
        "git_resumed comes before the human edit: {order:?}"
    );
    // On the paused clock the gate's 60 s wait would have passed at once and ended in
    // store.git_busy, with no human edit at all.
    assert!(
        waited < ww_core::store::GIT_WAIT / 2,
        "recorded {waited:?} after git finished: well inside the gate's 60 s wait"
    );
    let edits = crane_edits().await;
    assert_eq!(edits.len(), 1, "recorded, not store.git_busy: {edits:?}");
    assert_eq!(edits[0]["sha256"], sha256_bytes(edited));
    watcher.stop();
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079, ARCH-083, CORE-034
async fn checkout_of_3000_files_overflows_then_rescans_without_false_human_edits() {
    const PLAIN: usize = 2985;
    const ASSETS: usize = 5;
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("checkout");
    let asset = |k: usize| format!("projects/sketchbook/assets/asset_{k}");
    let run = "r_20270101_0800";
    let manifest = |k: usize, sealed: &[u8], candidate: &[u8]| {
        json!({
            "schema_version": 1,
            "asset_id": format!("asset_{k}"),
            "project": "sketchbook",
            "status": "candidate",
            "approved_version": "1.0.0",
            "candidate_runs": [{
                "run_id": run, "path": format!("candidates/{run}/"), "status": "open",
                "files": [{"path": format!("candidates/{run}/c.png"), "role": "candidate",
                           "sha256": sha256_bytes(candidate)}],
            }],
            "hashes": {"v1.0.0/s.png": format!("sha256:{}", sha256_bytes(sealed))},
            "human_edits": [],
        })
        .to_string()
    };
    let old = |k: usize, what: &str| format!("old {what} {k}").into_bytes();
    let new = |k: usize, what: &str| format!("new {what} {k}").into_bytes();
    for k in 0..PLAIN {
        write_file(&file(&ws, &format!("notes/n{k:04}.txt")), &old(k, "plain"));
    }
    for k in 0..ASSETS {
        let (s, c) = (old(k, "sealed"), old(k, "candidate"));
        write_file(&file(&ws, &format!("{}/v1.0.0/s.png", asset(k))), &s);
        write_file(
            &file(&ws, &format!("{}/candidates/{run}/c.png", asset(k))),
            &c,
        );
        write_file(
            &file(&ws, &format!("{}/asset_{k}.manifest.json", asset(k))),
            manifest(k, &s, &c).as_bytes(),
        );
    }
    let cfg = WatchConfig {
        buffer_bytes: 4096,
        ..WatchConfig::default()
    };
    let core = Core::open(env, ws, cfg, FailPoints::none()).await;
    core.fence().await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    core.events.attach(9_001, tx);
    core.events
        .subscribe(
            9_001,
            &["workspace".to_string()],
            &std::collections::BTreeMap::new(),
        )
        .await;
    let ops = GitOps::begin(&core.ws);
    for k in 0..PLAIN {
        ops.write(&format!("notes/n{k:04}.txt"), &new(k, "plain"));
    }
    for k in 0..ASSETS {
        let (s, c) = (new(k, "sealed"), new(k, "candidate"));
        ops.write(&format!("{}/v1.0.0/s.png", asset(k)), &s);
        ops.write(&format!("{}/candidates/{run}/c.png", asset(k)), &c);
        ops.write(
            &format!("{}/asset_{k}.manifest.json", asset(k)),
            manifest(k, &s, &c).as_bytes(),
        );
    }
    ops.finish();
    let mut seen = Vec::new();
    while !seen.contains(&"git_resumed".to_string()) {
        let line = tokio::time::timeout(DEADLINE, rx.recv())
            .await
            .expect("git_resumed within the deadline")
            .expect("an event line");
        let event: Value = serde_json::from_str(&line).unwrap();
        if let Some(kind) = event["params"]["data"]["kind"].as_str() {
            seen.push(kind.to_string());
        }
    }
    core.fence().await;
    let workspace = kinds(&core.published("workspace").await);
    assert!(workspace.contains(&"rescan".to_string()), "{workspace:?}");
    assert!(
        workspace.contains(&"git_resumed".to_string()),
        "{workspace:?}"
    );

    for k in 0..PLAIN {
        let rel = format!("notes/n{k:04}.txt");
        assert_eq!(
            core.row(&rel).await.and_then(|r| r.0),
            Some(sha256_bytes(&new(k, "plain"))),
            "{rel}"
        );
    }
    for k in 0..ASSETS {
        for (rel, what) in [
            (format!("{}/v1.0.0/s.png", asset(k)), "sealed"),
            (format!("{}/candidates/{run}/c.png", asset(k)), "candidate"),
        ] {
            let row = core.row(&rel).await;
            assert_eq!(
                row,
                Some((Some(sha256_bytes(&new(k, what))), FileState::Ok)),
                "{rel}"
            );
        }
        let text = std::fs::read_to_string(file(
            &core.ws,
            &format!("{}/asset_{k}.manifest.json", asset(k)),
        ))
        .unwrap();
        let m: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(m["human_edits"], json!([]), "asset_{k}: {text}");
    }
    core.open
        .store
        .write(&p("notes/after.txt"), b"after".to_vec(), Expect::New)
        .await
        .expect("a write after the checkout");
}

// ---------------------------------------------------------------------------------------------
// Folders, unstable and locked files.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079
async fn folder_moved_into_the_workspace_is_indexed() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let incoming = core.env.root().join("incoming");
    let mut rels = Vec::new();
    for a in 0..4 {
        for b in 0..5 {
            let inside = format!("level {a}/file {b}.txt");
            write_file(
                &inside.split('/').fold(incoming.clone(), |p, c| p.join(c)),
                inside.as_bytes(),
            );
            rels.push(inside);
        }
    }
    move_outside(&incoming, &core.path("notes/incoming"));
    core.fence().await;
    for inside in &rels {
        assert_eq!(
            core.row(&format!("notes/incoming/{inside}")).await,
            Some((Some(sha256_bytes(inside.as_bytes())), FileState::Ok)),
            "{inside}"
        );
    }
    assert_eq!(rels.len(), 20);
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079
#[allow(clippy::disallowed_methods)]
async fn hash_of_a_file_being_written_is_not_a_human_edit() {
    use std::io::Write as _;
    use std::os::windows::fs::OpenOptionsExt as _;

    let core = Core::fixture(fast()).await;
    core.fence().await;
    edit_outside(&core.path(SEALED), b"");
    core.fence().await;
    let before = edits_of(&core.detail(CRANE).await, "v1.0.0/crane_b.png");
    assert_eq!(before.len(), 1, "the emptied file is one edit");

    // A writer that shares nothing appends in chunks: no read is stable meanwhile.
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .share_mode(0)
        .open(core.path(SEALED))
        .unwrap();
    writer.write_all(b"first chunk, ").unwrap();
    writer.flush().unwrap();
    core.fence().await;
    assert_eq!(crane_edits_on_disk(&core, "v1.0.0/crane_b.png").len(), 1);
    writer.write_all(b"second chunk").unwrap();
    writer.flush().unwrap();
    core.fence().await;
    assert_eq!(crane_edits_on_disk(&core, "v1.0.0/crane_b.png").len(), 1);
    assert_eq!(
        core.row(SEALED).await,
        Some((Some(sha256_bytes(b"")), FileState::HumanEdit)),
        "the row keeps the last stable read"
    );
    drop(writer);
    core.fence().await;

    let after = edits_of(&core.detail(CRANE).await, "v1.0.0/crane_b.png");
    assert_eq!(
        after.len(),
        2,
        "exactly one more, once the writer finished: {after:?}"
    );
    assert_eq!(
        after[1]["sha256"],
        sha256_bytes(b"first chunk, second chunk")
    );
    assert_eq!(after[1]["previous_sha256"], sha256_bytes(b""));
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079
async fn locked_file_is_parked_not_removed() {
    // A long batch window, so the handle is surely taken before the batch reads the file.
    let core = Core::fixture(WatchConfig {
        batch: Duration::from_secs(1),
        ..WatchConfig::default()
    })
    .await;
    core.fence().await;
    let edited = b"edited, then held";
    edit_outside(&core.path(SEALED), edited);
    let held = HandleHolder::hold(&core.path(SEALED), Share::None);
    core.fence().await;
    // Parked: the row is as it was (neither missing nor a human edit).
    assert_eq!(
        core.row(SEALED).await,
        Some((Some(SEALED_SHA256.to_string()), FileState::Ok))
    );
    assert!(crane_edits_on_disk(&core, "v1.0.0/crane_b.png").is_empty());

    held.release();
    core.fence().await;
    let detail = core.detail(CRANE).await;
    let edits = edits_of(&detail, "v1.0.0/crane_b.png");
    assert_eq!(edits.len(), 1, "{detail}");
    assert_eq!(edits[0]["sha256"], sha256_bytes(edited));
}

// ---------------------------------------------------------------------------------------------
// Once only; missing files; the suite's own moves.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")] // spec: CORE-034, ARCH-079
async fn human_edit_is_recorded_once_across_notify_rescan_and_rebuild() {
    let core = Core::fixture(WatchConfig {
        buffer_bytes: 4096,
        ..WatchConfig::default()
    })
    .await;
    core.fence().await;
    let edited = b"one outside edit";
    edit_outside(&core.path(SEALED), edited);
    core.fence().await;
    let once = |detail: &Value| {
        let edits = edits_of(detail, "v1.0.0/crane_b.png");
        assert_eq!(edits.len(), 1, "{detail}");
        assert_eq!(edits[0]["sha256"], sha256_bytes(edited));
    };
    once(&core.detail(CRANE).await);

    // ww notify.
    let report = core.watch().notify(vec![p(SEALED)]).await;
    assert_eq!(report.reconciled, [SEALED]);
    once(&core.detail(CRANE).await);

    // An overflow rescan: a burst far larger than the 4 KiB buffer.
    for k in 0..400 {
        write_file(&core.path(&format!("notes/burst/file {k:03}.txt")), b"b");
    }
    core.until("a rescan", || async {
        kinds(&core.published("workspace").await).contains(&"rescan".to_string())
    })
    .await;
    core.fence().await;
    once(&core.detail(CRANE).await);

    // A restart with a damaged index.db: a cold rebuild trusts the manifest's hash, and the
    // background verification finds the file different.
    let index_db = core.open.index.path().expect("an index.db").to_path_buf();
    let core = {
        let ws = core.ws.clone();
        let core = core.reopen_damaged(&index_db).await;
        assert_eq!(core.ws, ws);
        core
    };
    core.until("the verification's decision", || async {
        matches!(core.row(SEALED).await, Some((_, FileState::HumanEdit)))
    })
    .await;
    once(&core.detail(CRANE).await);
    assert_eq!(
        core.row(SEALED).await.and_then(|r| r.0),
        Some(sha256_bytes(edited))
    );
}

impl Core {
    /// Closes the hub, damages `index_db`, and opens the workspace in a new hub.
    async fn reopen_damaged(self, index_db: &Path) -> Core {
        let Core {
            env, ws, cfg, hub, ..
        } = self;
        hub.close_all().await;
        drop(hub);
        write_file(index_db, &[0xAB; 4096]);
        Core::open(env, ws, cfg, FailPoints::none()).await
    }
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079, CORE-038
async fn removed_sealed_file_is_missing_then_recovers() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let bytes = std::fs::read(core.path(SEALED)).unwrap();
    delete_outside(&core.path(SEALED));
    core.fence().await;
    let detail = core.detail(CRANE).await;
    assert_eq!(
        Core::state(&detail, "v1.0.0/crane_b.png"),
        "missing",
        "{detail}"
    );
    let events = core.published(&core.asset_topic(CRANE)).await;
    assert!(
        events.contains(&json!({"kind": "missing", "path": "v1.0.0/crane_b.png"})),
        "{events:?}"
    );

    write_file(&core.path(SEALED), &bytes);
    core.fence().await;
    let detail = core.detail(CRANE).await;
    assert_eq!(Core::state(&detail, "v1.0.0/crane_b.png"), "ok", "{detail}");
    assert_eq!(detail["human_edits"], json!([]), "{detail}");
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-079, CORE-038
async fn removals_under_a_held_version_lock_wait_for_its_release() {
    let core = Core::fixture(fast()).await;
    let moved = format!("{BASE}/v1.1.0/crane_c.png");
    // The version folder exists first, so the move below is one rename whose two halves come
    // in one batch.
    make_dir(&core.path(&format!("{BASE}/v1.1.0")));
    core.fence().await;
    let guard = core
        .open
        .store
        .version_lock("sketchbook", CRANE)
        .lock_owned()
        .await;
    // What a sign-off does under the lock: first the chosen file into v1.1.0/ (the manifest
    // still lists it in an open run, so on its own the removal would be `missing`) ...
    move_outside(&core.path(CANDIDATE), &core.path(&moved));
    // ... and the batches with that move run while the lock is held (a file written after it,
    // outside the asset, is indexed only once they have): everything in the asset folder waits.
    write_file(&core.path("notes/after the move.txt"), b"marker");
    core.until("the marker indexed", || async {
        core.row("notes/after the move.txt").await.is_some()
    })
    .await;
    assert_eq!(
        core.row(CANDIDATE).await,
        Some((Some(CANDIDATE_SHA256.to_string()), FileState::Ok)),
        "never missing while the sign-off runs"
    );
    assert_eq!(core.row(&moved).await, None, "decided after the sign-off");
    let fence = {
        let core_watch = core.watch().clone();
        let name = "held-lock".to_string();
        write_file(
            &core.ws.join(".worldwright").join("fence").join(&name),
            b"held-lock",
        );
        tokio::spawn(async move { core_watch.fence(&name).await })
    };
    // ... then the run selected and the file in `hashes`, still under the lock.
    let text = std::fs::read_to_string(core.path(CRANE_MANIFEST)).unwrap();
    let mut m: Value = serde_json::from_str(&text).unwrap();
    m["candidate_runs"][1]["status"] = json!("selected");
    m["hashes"]["v1.1.0/crane_c.png"] = json!(format!("sha256:{CANDIDATE_SHA256}"));
    write_file(
        &core.path(CRANE_MANIFEST),
        serde_json::to_string_pretty(&m).unwrap().as_bytes(),
    );
    // The removal waits for the lock, so the fence can't return yet.
    assert!(
        !fence.is_finished(),
        "the fence waits for the deferred removal"
    );
    drop(guard);
    tokio::time::timeout(DEADLINE, fence)
        .await
        .expect("the fence returns once the lock is free")
        .unwrap();
    assert_eq!(
        core.row(CANDIDATE).await,
        None,
        "the old path is gone from the index"
    );
    let detail = core.detail(CRANE).await;
    assert_eq!(Core::state(&detail, "v1.1.0/crane_c.png"), "ok", "{detail}");
    let events = core.published(&core.asset_topic(CRANE)).await;
    assert!(
        !kinds(&events).contains(&"missing".to_string()),
        "{events:?}"
    );

    // A removal under an asset whose lock is free is decided at once.
    delete_outside(&core.path(SEALED));
    core.fence().await;
    assert_eq!(
        Core::state(&core.detail(CRANE).await, "v1.0.0/crane_b.png"),
        "missing"
    );
}

// ---------------------------------------------------------------------------------------------
// No watcher.
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-016, ARCH-079, ARCH-083
fn unsupported_watcher_leaves_the_workspace_served() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.copy_fixture_workspace("unwatched");
    env.register_workspace(&ws, true);
    let points = failpoints::spec(&[("watch.start", "unsupported")]);
    let _core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &points)]);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");

    let written = pipe.tool_ok(
        "test.store_write",
        json!({"path": "notes/served.txt", "text": "still served", "expect": "new"}),
        Some("unwatched-1"),
    );
    assert_eq!(written["sha256"], sha256_bytes(b"still served"));
    let subscribed = pipe.call(
        "ww/subscribe",
        json!({"topics": ["workspace"], "since_seq": {"workspace": 0}}),
    );
    assert_eq!(subscribed["result"]["status"], "ok", "{subscribed}");
    let unavailable = loop {
        let event = pipe
            .next_event(DEADLINE)
            .expect("a watcher_unavailable event");
        if event["topic"] == "workspace" && event["data"]["kind"] == "watcher_unavailable" {
            break event;
        }
    };
    assert!(
        unavailable["data"]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "{unavailable}"
    );

    let sealed = file(&ws, SEALED);
    let edited = b"edited without a watcher";
    edit_outside(&sealed, edited);
    let out = env.ww(&["notify", sealed.to_str().unwrap()]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    let detail = pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": CRANE}),
        None,
    );
    let edits = edits_of(&detail, "v1.0.0/crane_b.png");
    assert_eq!(edits.len(), 1, "{detail}");
    assert_eq!(edits[0]["sha256"], sha256_bytes(edited));
}

// ---------------------------------------------------------------------------------------------
// Review fixes: a stale verification, and a lock taken before the open.
// ---------------------------------------------------------------------------------------------

/// The real probe, with one hook: armed on a file, the first `identity` of it after an
/// `open_read` of it (the verification's identity taken after its read) writes `then` there
/// first and answers with the identity from before that write. So the read of the old bytes
/// counts as stable, and the file already holds newer ones when the decision comes.
#[derive(Debug, Default)]
struct SaveAfterRead {
    armed: std::sync::Mutex<Option<(PathBuf, Vec<u8>, bool)>>,
}

impl Probe for SaveAfterRead {
    fn identity(&self, path: &Path) -> std::io::Result<ww_platform::fs::FileIdentity> {
        let before = SystemProbe.identity(path)?;
        let mut armed = self.armed.lock().unwrap();
        if let Some((at, then, read)) = armed.as_ref()
            && *read
            && at == path
        {
            write_file(path, then);
            *armed = None;
        }
        Ok(before)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<ww_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        if let Some((at, _, read)) = self.armed.lock().unwrap().as_mut()
            && at == path
        {
            *read = true;
        }
        SystemProbe.open_read(path)
    }
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-034, ARCH-079
async fn stale_verification_never_overrides_a_newer_edit() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = canonical(&env.copy_fixture_workspace("stale"));
    let sealed = file(&ws, SEALED);
    // Edit A while no core runs: the cold rebuild trusts the manifest's hash, so the
    // verification reads A and reports it.
    let (a, b) = (
        b"edit A, read by the verification",
        b"edit B, saved right after",
    );
    edit_outside(&sealed, a);

    let probe = Arc::new(SaveAfterRead::default());
    let state = Arc::new(
        ww_core::state::StateDb::open(
            &env.root().join("stale-local").join("state.db"),
            ww_core::state::StateScope::Workspace,
            no_fatal(),
        )
        .unwrap(),
    );
    let clock: Arc<dyn Clock> = FakeClock::new();
    let store = Arc::new(ww_core::store::Store::new(
        ws.clone(),
        state,
        FailPoints::none(),
        clock.clone(),
        Arc::new(SeqIds::new()),
        probe.clone(),
    ));
    let index = Arc::new(ww_core::index::Index::open_in_memory(&ws, probe.clone()).unwrap());
    store.set_sink(index.clone());
    let events = Arc::new(EventBus::new(clock));
    let ws_id = Env::workspace_hash(&std::fs::canonicalize(&ws).unwrap());
    // No folder watch: this test plays the batch itself (`notify`).
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let watcher = Watcher::start_from(
        store.clone(),
        index.clone(),
        events,
        ws_id.clone(),
        WatchConfig::default(),
        rx,
    );
    index.start_first();
    index.ready().await;

    *probe.armed.lock().unwrap() = Some((sealed.clone(), b.to_vec(), false));
    let mismatches = index.verify_pending(10_000).await.unwrap();
    let stale: Vec<_> = mismatches
        .iter()
        .filter(|m| m.path.as_str() == SEALED)
        .collect();
    assert_eq!(stale.len(), 1, "{mismatches:?}");
    assert_eq!(stale[0].sha256, sha256_bytes(a), "the verification read A");
    assert_eq!(
        std::fs::read(&sealed).unwrap(),
        b,
        "B was saved after the read"
    );

    // A batch (here `ww notify`) sees B first and records it ...
    watcher.notify(vec![p(SEALED)]).await;
    // ... then the verification's result about A arrives.
    watcher.mismatches(mismatches).await;

    let detail = asset_detail(&store, &index, &ws_id, "sketchbook", CRANE)
        .await
        .unwrap();
    let detail = serde_json::to_value(detail).unwrap();
    let edits = edits_of(&detail, "v1.0.0/crane_b.png");
    assert_eq!(edits.len(), 1, "only the newer edit: {detail}");
    assert_eq!(edits[0]["sha256"], sha256_bytes(b));
    let row = index.files_of_path(&p(SEALED)).await.unwrap().unwrap();
    assert_eq!(
        row.sha256,
        Some(sha256_bytes(b)),
        "the row holds what the file holds"
    );
    assert_eq!(row.state, FileState::HumanEdit);
    watcher.stop();
}

#[tokio::test(flavor = "multi_thread")] // spec: ARCH-083, CORE-034
async fn index_lock_taken_before_the_open_defers_checkout_writes() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.copy_fixture_workspace("lock-first");
    // Git is already working when the workspace opens: no `index.lock` event will come.
    let ops = GitOps::begin(&ws);
    let core = Core::open(env, ws, fast(), FailPoints::none()).await;
    core.fence().await;
    let checked_out = b"v1.0.0 as the checkout writes it";
    ops.write(SEALED, checked_out);
    core.fence().await;
    assert!(
        edits_of(&core.detail(CRANE).await, "v1.0.0/crane_b.png").is_empty(),
        "nothing is decided while git holds index.lock"
    );
    assert!(core.open.store.git_gate().is_busy());

    ops.finish();
    core.until("git_resumed", || async {
        kinds(&core.published("workspace").await).contains(&"git_resumed".to_string())
    })
    .await;
    core.fence().await;
    // The checkout left bytes the manifest doesn't record (the test's checkout didn't update
    // it), so the rescan after git records one human edit, once.
    let edits = edits_of(&core.detail(CRANE).await, "v1.0.0/crane_b.png");
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0]["sha256"], sha256_bytes(checked_out));
    let workspace = kinds(&core.published("workspace").await);
    assert!(
        workspace.contains(&"git_paused".to_string()),
        "{workspace:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: QA-031, ARCH-079
async fn a_fence_written_before_the_watch_is_armed_is_handled() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = canonical(&env.make_workspace("early fence"));
    // Written before `Watcher::start`: no event of it ever comes.
    write_file(&ws.join(".worldwright").join("fence").join("early"), b"f");
    let (_store, _index, watcher, _events, _ws_id) = bare(&ws).await;
    tokio::time::timeout(Duration::from_secs(30), watcher.fence("early"))
        .await
        .expect("a fence that predates the watch is covered by the first reconcile");
}

// ---------------------------------------------------------------------------------------------
// Ruling Q32: files added to a sealed version.
// ---------------------------------------------------------------------------------------------

/// The crane's v1.0.0 ref, as `asset.get` gives it.
fn crane_ref(detail: &Value) -> String {
    detail["versions"][0]["ref"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-026, ARCH-042, ARCH-079
async fn os_metadata_in_a_sealed_version_is_ignored() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let before = core.detail(CRANE).await;
    for name in ["Thumbs.db", "desktop.ini", ".DS_Store"] {
        write_file(&core.path(&format!("{BASE}/v1.0.0/{name}")), b"os metadata");
    }
    core.fence().await;
    // `ww notify` of them changes nothing either.
    let notified = core
        .watch()
        .notify(vec![p(&format!("{BASE}/v1.0.0/Thumbs.db"))])
        .await;
    assert!(notified.ignored.is_empty(), "{notified:?}");

    let after = core.detail(CRANE).await;
    assert_eq!(crane_ref(&after), crane_ref(&before), "{after}");
    assert_eq!(after["human_edits"], before["human_edits"], "{after}");
    assert_eq!(after["files"], before["files"], "never indexed: {after}");
    for name in ["Thumbs.db", "desktop.ini", ".DS_Store"] {
        assert!(
            core.row(&format!("{BASE}/v1.0.0/{name}")).await.is_none(),
            "{name} has an index row"
        );
    }
    let events = core.published(&core.asset_topic(CRANE)).await;
    assert!(
        !kinds(&events).contains(&"human_edit".to_string()),
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-026, CORE-034, ARCH-042, ARCH-079
async fn a_file_dropped_into_a_sealed_version_is_a_human_edit() {
    let core = Core::fixture(fast()).await;
    core.fence().await;
    let before = core.detail(CRANE).await;
    let extra = format!("{BASE}/v1.0.0/extra.png");
    write_file(&core.path(&extra), b"dropped in by hand");
    core.fence().await;

    let after = core.detail(CRANE).await;
    // The version's identity doesn't move.
    assert_eq!(crane_ref(&after), crane_ref(&before), "{after}");
    assert_eq!(
        edits_of(&after, "v1.0.0/extra.png"),
        [
            json!({"path": "v1.0.0/extra.png", "sha256": sha256_bytes(b"dropped in by hand"),
                "previous_sha256": "", "date": TODAY})
        ],
        "{after}"
    );
    assert_eq!(Core::state(&after, "v1.0.0/extra.png"), "human_edit");
    let events = core.published(&core.asset_topic(CRANE)).await;
    let flagged: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"] == "human_edit" && e["path"] == "v1.0.0/extra.png")
        .collect();
    assert_eq!(flagged.len(), 1, "{events:?}");
    assert_eq!(flagged[0]["previous_sha256"], "", "{events:?}");
    // Seen again (`ww notify`), it is recorded once; the file itself is never touched.
    core.watch().notify(vec![p(&extra)]).await;
    let again = core.detail(CRANE).await;
    assert_eq!(edits_of(&again, "v1.0.0/extra.png").len(), 1, "{again}");
    assert_eq!(
        std::fs::read(core.path(&extra)).unwrap(),
        b"dropped in by hand"
    );
    // The snapshot itself is no outside addition.
    assert!(edits_of(&again, "v1.0.0/manifest.snapshot.json").is_empty());
}

#[tokio::test(flavor = "multi_thread")] // spec: CORE-034, ARCH-079
async fn a_metadata_only_change_keeps_the_verifications_result() {
    // The staleness check compares the skip key (size, last write, ChangeTime), not the whole
    // identity: an attribute or file-id difference alone (a FAT volume, Explorer setting the
    // archive bit) never hides the verification's human edit.
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = canonical(&env.copy_fixture_workspace("metadata"));
    let sealed = file(&ws, SEALED);
    let edit = b"edited while no core ran";
    edit_outside(&sealed, edit);
    let state = Arc::new(
        ww_core::state::StateDb::open(
            &env.root().join("metadata-local").join("state.db"),
            ww_core::state::StateScope::Workspace,
            no_fatal(),
        )
        .unwrap(),
    );
    let clock: Arc<dyn Clock> = FakeClock::new();
    let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
    let store = Arc::new(ww_core::store::Store::new(
        ws.clone(),
        state,
        FailPoints::none(),
        clock.clone(),
        Arc::new(SeqIds::new()),
        probe.clone(),
    ));
    let index = Arc::new(ww_core::index::Index::open_in_memory(&ws, probe).unwrap());
    store.set_sink(index.clone());
    let events = Arc::new(EventBus::new(clock));
    let ws_id = Env::workspace_hash(&std::fs::canonicalize(&ws).unwrap());
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let watcher = Watcher::start_from(
        store.clone(),
        index.clone(),
        events,
        ws_id.clone(),
        WatchConfig::default(),
        rx,
    );
    index.start_first();
    index.ready().await;

    let mut mismatches = index.verify_pending(10_000).await.unwrap();
    let found = mismatches
        .iter_mut()
        .find(|m| m.path.as_str() == SEALED)
        .unwrap_or_else(|| panic!("{SEALED} isn't reported"));
    assert_eq!(found.sha256, sha256_bytes(edit));
    // What the file reports now differs from the read's identity only outside the skip key.
    found.identity.attributes ^= 0x20;
    found.identity.file_id = Some([7; 16]);
    watcher.mismatches(mismatches).await;

    let detail = asset_detail(&store, &index, &ws_id, "sketchbook", CRANE)
        .await
        .unwrap();
    let detail = serde_json::to_value(detail).unwrap();
    let edits = edits_of(&detail, "v1.0.0/crane_b.png");
    assert_eq!(edits.len(), 1, "the human edit is recorded: {detail}");
    assert_eq!(edits[0]["sha256"], sha256_bytes(edit));
    assert_eq!(edits[0]["previous_sha256"], SEALED_SHA256);
    watcher.stop();
}
