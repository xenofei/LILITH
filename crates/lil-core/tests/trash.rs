//! Trash and retention (ARCH-058, CORE-026, CORE-032, CORE-037, CORE-110): trash entries
//! with restore records, restores that refuse an occupied path and re-seal version files, the
//! 30-day purge (read-only payloads included), rejected runs moved to trash 90 days after
//! sign-off unless pinned, the CORE-110 hard-delete scan, the trash tools, and maintenance at
//! workspace open and every 24 hours. Plan 4b (Task 11; CORE-157, CORE-037, ARCH-125): the
//! whole-asset trash, `asset.trash`, which asks every caller and is refused while the asset is
//! in use, and `trash.restore {to}`, through an in-process dispatcher with the confirmations'
//! stage, over the pipe of an in-process core with the `ui:test` app, and a real core killed at
//! the fail point `store.after_asset_trash`.
//!
//! Every test runs on a `FakeClock` (and tokio's paused clock where the store retries), so the
//! 30- and 90-day purges pass without real sleeps (QA-031). Fixture files are made with
//! `lil_testkit::files::write_file`; the one outside step that clears a read-only bit carries
//! its own allowance.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use lil_core::confirm::{
    AgentConfirm, Confirm, ListState, McpConfirmation, NoNotificationPath, UiPresence,
};
use lil_core::events::EventBus;
use lil_core::identity::TrustedDirs;
use lil_core::index::Scope as IndexScope;
use lil_core::session::Session;
use lil_core::state::{StateDb, StateRegistry, StateScope};
use lil_core::store::{
    ASSET_TRASH_REASON, FailPoints, SealRequest, Store, StoreError, TRASH_KEEP_DAYS, TrashRecord,
    WsPath,
};
use lil_core::tools::{
    AssetServices, AssetUse, Dispatcher, InUse, PipelineHooks, TestGates, core_registry,
};
use lil_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_core::{CoreConfig, Lifecycle, start_in_thread};
use lil_platform::fs::{FileIdentity, Probe, Share, SystemProbe, VolumeInfo};
use lil_platform::{Clock, Env};
use lil_protocol::app::{Answer, ConfirmationAnswerParams};
use lil_protocol::{CallerKind, ErrorBody, Outcome, ToolCallParams, ToolOutput};
use lil_schema::Manifest;
use lil_testkit::files::write_file;
use lil_testkit::{
    CoreFixture, FakeClock, HandleHolder, RawPipe, SeqIds, TestApp, TestEnv, ensure_built,
    sha256_bytes, utc,
};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------------------------

const DAY: u64 = 86_400;
const ASSET: &str = "projects/sketchbook/assets/crane";
const MANIFEST: &str = "projects/sketchbook/assets/crane/crane.manifest.json";

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

fn canonical(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap();
    let text = canonical.to_string_lossy().into_owned();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

fn workspace() -> Ws {
    let env = TestEnv::with_label("Ünïcödé space");
    let root = canonical(&env.make_workspace("trash"));
    let db = env.root().join("local-ws").join("state.db");
    let state = Arc::new(StateDb::open(&db, StateScope::Workspace, no_fatal()).expect("state.db"));
    Ws {
        root,
        state,
        clock: FakeClock::new(),
        _env: env,
    }
}

impl Ws {
    fn store(&self) -> Arc<Store> {
        Arc::new(
            Store::new(
                self.root.clone(),
                self.state.clone(),
                FailPoints::none(),
                self.clock.clone(),
                Arc::new(SeqIds::new()),
                Arc::new(SystemProbe),
            )
            .with_machine("testmachine"),
        )
    }

    fn file(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, c| p.join(c))
    }

    fn read(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.file(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
    }

    fn exists(&self, rel: &str) -> bool {
        self.file(rel).exists()
    }

    fn now(&self) -> i64 {
        secs(self.clock.utc_now())
    }

    fn manifest(&self) -> Value {
        serde_json::from_slice(&self.read(MANIFEST)).unwrap()
    }

    /// Moves the clock on by `days` days and returns the new UTC time.
    fn after_days(&self, days: u64) -> i64 {
        self.clock.advance(Duration::from_secs(days * DAY));
        self.now()
    }
}

fn secs(t: std::time::SystemTime) -> i64 {
    i64::try_from(t.duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap()
}

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .readonly()
}

/// The folder of a trash entry: `.trash\<date>\<entry_id>`.
fn entry_dir(ws: &Ws, record: &TrashRecord) -> PathBuf {
    let date = &record.moved_utc[..10];
    ws.file(&format!(".trash/{date}/{}", record.entry_id))
}

/// A run `r_1` of the fixture asset holding `a.png`, sealed as `v1.0.0`.
async fn sealed_version(ws: &Ws, store: &Store) {
    write_file(&ws.file(&format!("{ASSET}/candidates/r_1/a.png")), b"a");
    let mut m = Manifest::new(
        "sketchbook",
        "crane",
        "image",
        "00000000-0000-4000-8000-0000000000aa",
    );
    m.push(
        "candidate_runs",
        json!({"run_id": "r_1", "path": "candidates/r_1/", "status": "open",
               "files": [{"path": "candidates/r_1/a.png", "role": "candidate"}]}),
    )
    .unwrap();
    write_file(&ws.file(MANIFEST), m.to_text().as_bytes());
    let guard = store.version_lock("sketchbook", "crane").lock_owned().await;
    store
        .seal(
            &guard,
            SealRequest {
                project: "sketchbook".to_string(),
                asset_id: "crane".to_string(),
                version: "1.0.0".parse().unwrap(),
                run_id: "r_1".to_string(),
                files: vec!["a.png".to_string()],
                snapshot: m,
                expected: Default::default(),
            },
        )
        .await
        .expect("the seal");
}

// ---------------------------------------------------------------------------------------------
// Trash and restore.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-058, CORE-037
async fn trash_moves_with_a_restore_record() {
    let ws = workspace();
    write_file(&ws.file("notes/Ünïcödé space.txt"), b"keep me");
    let store = ws.store();
    let now = ws.now();

    let record = store
        .trash(&p("notes/Ünïcödé space.txt"), "test")
        .await
        .expect("the trash");
    let hex = record
        .entry_id
        .strip_prefix("te_")
        .unwrap_or_else(|| panic!("{} is not te_<16 hex>", record.entry_id));
    assert!(
        hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{record:?}"
    );
    assert_eq!(record.original_path, "notes/Ünïcödé space.txt");
    assert_eq!(utc::parse_rfc3339(&record.moved_utc), now);
    assert_eq!(
        utc::parse_rfc3339(&record.purge_after),
        now + TRASH_KEEP_DAYS * 86_400
    );
    assert_eq!(record.reason, "test");
    assert_eq!(
        record.sha256.as_deref(),
        Some(sha256_bytes(b"keep me").as_str())
    );
    assert!(!ws.exists("notes/Ünïcödé space.txt"));

    // <ws>\.trash\<YYYY-MM-DD>\<te_…>\payload\<name>, with restore.json beside payload.
    let dir = entry_dir(&ws, &record);
    assert_eq!(&record.moved_utc[..10], "2027-01-15");
    assert_eq!(
        std::fs::read(dir.join("payload").join("Ünïcödé space.txt")).unwrap(),
        b"keep me"
    );
    let on_disk: Value = serde_json::from_slice(&std::fs::read(dir.join("restore.json")).unwrap())
        .expect("restore.json is JSON");
    assert_eq!(
        on_disk,
        json!({
            "entry_id": record.entry_id,
            "original_path": "notes/Ünïcödé space.txt",
            "moved_utc": record.moved_utc,
            "purge_after": record.purge_after,
            "reason": "test",
            "sha256": sha256_bytes(b"keep me"),
        })
    );
    assert_eq!(
        store.trash_list().await.unwrap(),
        std::slice::from_ref(&record)
    );

    let restored = store
        .trash_restore(&record.entry_id)
        .await
        .expect("the restore");
    assert_eq!(restored.as_str(), "notes/Ünïcödé space.txt");
    assert_eq!(ws.read("notes/Ünïcödé space.txt"), b"keep me");
    assert!(store.trash_list().await.unwrap().is_empty());
    assert!(!dir.exists(), "the entry stays after its restore");

    // A folder goes to trash whole, with no sha256, and comes back whole.
    write_file(&ws.file("notes/dir/a.txt"), b"a");
    write_file(&ws.file("notes/dir/sub/b.txt"), b"b");
    let folder = store.trash(&p("notes/dir"), "test").await.unwrap();
    assert_eq!(folder.sha256, None);
    assert!(!ws.exists("notes/dir"));
    assert_eq!(
        std::fs::read(entry_dir(&ws, &folder).join("payload/dir/sub/b.txt")).unwrap(),
        b"b"
    );
    let back = store.trash_restore(&folder.entry_id).await.unwrap();
    assert_eq!(back.as_str(), "notes/dir");
    assert_eq!(ws.read("notes/dir/sub/b.txt"), b"b");

    // Nothing there, the trash itself and the ledger are never trashed.
    for rel in ["notes/none.txt", ".trash", "registry/ledger.csv"] {
        if rel == "registry/ledger.csv" {
            write_file(&ws.file(rel), b"user's");
        }
        let refused = store.trash(&p(rel), "test").await;
        assert!(refused.is_err(), "{rel}: {refused:?}");
    }
    assert!(ws.exists("registry/ledger.csv"));
}

#[tokio::test(start_paused = true)] // spec: ARCH-058
async fn trash_refuses_both_workspace_folders() {
    use lil_platform::names::{LEGACY_WORKSPACE_DIR, WORKSPACE_DIR};
    let ws = workspace();
    let store = ws.store();
    for dir in [WORKSPACE_DIR, LEGACY_WORKSPACE_DIR] {
        write_file(&ws.file(&format!("{dir}/state.json")), b"{}");
        for rel in [
            dir.to_string(),
            dir.to_uppercase(),
            format!("{dir}/state.json"),
        ] {
            let refused = store.trash(&p(&rel), "test").await;
            assert!(
                matches!(refused, Err(StoreError::PathInvalid { .. })),
                "{rel}: {refused:?}"
            );
        }
        assert_eq!(ws.read(&format!("{dir}/state.json")), b"{}");
    }
    // A folder of that name deeper down is an ordinary folder.
    write_file(
        &ws.file(&format!("notes/{LEGACY_WORKSPACE_DIR}/a.txt")),
        b"a",
    );
    store
        .trash(&p(&format!("notes/{LEGACY_WORKSPACE_DIR}")), "test")
        .await
        .unwrap();
}

/// The rename's cleanup removed the legacy credential and copy-forward code, but the legacy
/// workspace data folder stays reserved for good (owner question 1). The folder's name is
/// written out here, not taken from a constant, so this holds even if the constant goes:
/// trashing it or anything in it, or writing under it, is `store.path_invalid`, and neither the
/// watcher nor the index ever looks inside it.
#[tokio::test(flavor = "multi_thread")] // spec: ARCH-058, ARCH-079
async fn legacy_folder_stays_reserved_after_pr_d() {
    const LEGACY: &str = ".worldwright";
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = canonical(&env.make_workspace("reserved"));
    let inside = format!("{LEGACY}/x.txt");
    write_file(&ws.join(LEGACY).join("x.txt"), b"an old core's");
    let clock: Arc<dyn Clock> = FakeClock::new();
    let hub = WorkspaceHub::new(
        env.env.clone(),
        Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap()),
        Arc::new(EventBus::new(clock.clone())),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        Arc::new(SystemProbe),
        lil_core::watch::WatchConfig::default(),
    );
    let reference = WorkspaceRef {
        id: Env::workspace_hash(&std::fs::canonicalize(&ws).unwrap()),
        path: ws.clone(),
    };
    let open = hub.open(&reference).await.expect("the open");
    open.index.ready().await;

    // The store refuses it, in any case, and leaves the file as it was.
    for rel in [LEGACY.to_string(), LEGACY.to_uppercase(), inside.clone()] {
        let refused = open.store.trash(&p(&rel), "test").await;
        assert!(
            matches!(&refused, Err(StoreError::PathInvalid { .. })),
            "trash {rel}: {refused:?}"
        );
        assert_eq!(
            refused.unwrap_err().to_error("t").code,
            "store.path_invalid"
        );
    }
    for (rel, expect) in [
        (
            inside.clone(),
            lil_core::store::Expect::Hash(sha256_bytes(b"an old core's")),
        ),
        (format!("{LEGACY}/new.txt"), lil_core::store::Expect::New),
    ] {
        let refused = open.store.write(&p(&rel), b"new".to_vec(), expect).await;
        assert!(
            matches!(&refused, Err(StoreError::PathInvalid { .. })),
            "write {rel}: {refused:?}"
        );
        assert_eq!(
            refused.unwrap_err().to_error("t").code,
            "store.path_invalid"
        );
    }
    assert_eq!(
        std::fs::read(ws.join(LEGACY).join("x.txt")).unwrap(),
        b"an old core's"
    );
    assert!(!ws.join(LEGACY).join("new.txt").exists());

    // The index's first reconcile skipped it, and the watcher ignores it.
    let watcher = open
        .watch
        .as_ref()
        .expect("a read-write open has a watcher");
    write_file(&ws.join(LEGACY).join("later.txt"), b"later");
    let report = watcher
        .notify(vec![p(&inside), p(&format!("{LEGACY}/later.txt"))])
        .await;
    assert!(report.reconciled.is_empty(), "{report:?}");
    assert_eq!(report.ignored.len(), 2, "{report:?}");
    let fence = format!("reserved-{}", std::process::id());
    write_file(&ws.join(".lilith").join("fence").join(&fence), b"f");
    tokio::time::timeout(DEADLINE, watcher.fence(&fence))
        .await
        .expect("the fence returns");
    for rel in [inside.as_str(), &format!("{LEGACY}/later.txt")] {
        assert_eq!(
            open.index.files_of_path(&p(rel)).await.unwrap(),
            None,
            "{rel} is indexed"
        );
    }
    drop(open);
    hub.close_all().await;
}

#[tokio::test(start_paused = true)] // spec: ARCH-058, CORE-037
async fn restore_refuses_an_occupied_path() {
    let ws = workspace();
    write_file(&ws.file("notes/a.txt"), b"old");
    let store = ws.store();
    let record = store.trash(&p("notes/a.txt"), "test").await.unwrap();
    write_file(&ws.file("notes/a.txt"), b"new");

    let refused = store.trash_restore(&record.entry_id).await;
    assert_eq!(refused, Err(StoreError::RestoreConflict(p("notes/a.txt"))));
    let error = refused.unwrap_err().to_error("t");
    assert_eq!(error.code, "trash.restore_conflict");
    assert!(error.message.contains("notes/a.txt"), "{}", error.message);
    assert_eq!(ws.read("notes/a.txt"), b"new");
    assert_eq!(
        store.trash_list().await.unwrap(),
        std::slice::from_ref(&record)
    );
    assert_eq!(
        std::fs::read(entry_dir(&ws, &record).join("payload/a.txt")).unwrap(),
        b"old"
    );

    for unknown in ["te_00000000000000ff", "nonsense", "../x", ""] {
        let missing = store.trash_restore(unknown).await;
        assert_eq!(
            missing,
            Err(StoreError::TrashUnknown(unknown.to_string())),
            "{unknown}"
        );
        assert_eq!(missing.unwrap_err().to_error("t").code, "trash.unknown");
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-058, CORE-037
async fn trash_purges_after_30_days_only() {
    let ws = workspace();
    write_file(&ws.file("notes/a.txt"), b"a");
    let store = ws.store();
    let record = store.trash(&p("notes/a.txt"), "test").await.unwrap();
    let dir = entry_dir(&ws, &record);

    let kept = store.maintenance(ws.after_days(29)).await.unwrap();
    assert!(kept.purged.is_empty(), "{kept:?}");
    assert!(dir.exists());
    assert_eq!(
        store.trash_list().await.unwrap(),
        std::slice::from_ref(&record)
    );

    let purged = store.maintenance(ws.after_days(2)).await.unwrap();
    assert_eq!(purged.purged, std::slice::from_ref(&record.entry_id));
    assert!(!dir.exists());
    assert!(
        !dir.parent().unwrap().exists(),
        "the empty day folder stays"
    );
    assert!(store.trash_list().await.unwrap().is_empty());
    assert!(!ws.exists("notes/a.txt"));
}

#[tokio::test(start_paused = true)] // spec: CORE-026, CORE-037, CORE-110
async fn purge_of_a_read_only_payload_succeeds() {
    let ws = workspace();
    let store = ws.store();
    sealed_version(&ws, &store).await;
    // A whole asset goes to the trash with its sealed (read-only) version inside it.
    let record = store.trash(&p(ASSET), "test").await.unwrap();
    let payload = entry_dir(&ws, &record)
        .join("payload")
        .join("crane")
        .join("v1.0.0")
        .join("a.png");
    assert!(
        read_only(&payload),
        "the sealed file lost its read-only bit"
    );

    let report = store.maintenance(ws.after_days(31)).await.unwrap();
    assert_eq!(report.purged, std::slice::from_ref(&record.entry_id));
    assert!(!payload.exists());
    assert!(!entry_dir(&ws, &record).exists());
}

/// Clears the read-only bit as an outside program would.
#[allow(clippy::disallowed_methods)]
fn clear_read_only(path: &Path) {
    lil_platform::fs::set_readonly(path, false).unwrap();
}

/// A trash entry made by hand for `original`, as one made before sealed paths were refused
/// would be: its record, and `payload/<name>` holding `files` (`""` names the file itself,
/// anything else a file in the folder `<name>`).
fn entry_by_hand(ws: &Ws, entry_id: &str, original: &str, files: &[(&str, &[u8])]) {
    let moved = lil_schema::time::rfc3339(ws.now());
    let dir = ws.file(&format!(".trash/{}/{entry_id}", &moved[..10]));
    let name = original.rsplit('/').next().unwrap();
    for (inside, bytes) in files {
        let mut at = dir.join("payload").join(name);
        if !inside.is_empty() {
            at = at.join(inside);
        }
        write_file(&at, bytes);
    }
    let record = json!({
        "entry_id": entry_id,
        "original_path": original,
        "moved_utc": moved,
        "purge_after": lil_schema::time::rfc3339(ws.now() + TRASH_KEEP_DAYS * 86_400),
        "reason": "test",
    });
    write_file(
        &dir.join("restore.json"),
        format!("{}\n", serde_json::to_string_pretty(&record).unwrap()).as_bytes(),
    );
}

/// A `restore.json` edited by hand can name a path the trash never takes: the restore refuses
/// it like a write, before anything moves, and the entry stays exactly as it was.
#[tokio::test(start_paused = true)] // spec: ARCH-058
async fn restore_refuses_reserved_targets() {
    use lil_platform::names::{LEGACY_WORKSPACE_DIR, WORKSPACE_DIR};
    let ws = workspace();
    let store = ws.store();
    write_file(&ws.file("registry/ledger.csv"), b"user's");
    for (n, target) in [
        format!("{LEGACY_WORKSPACE_DIR}/x.txt"),
        format!("{}/x.txt", LEGACY_WORKSPACE_DIR.to_uppercase()),
        format!("{WORKSPACE_DIR}/x.txt"),
        ".git/hooks/x".to_string(),
        ".trash/x.txt".to_string(),
        "registry/ledger.csv".to_string(),
    ]
    .iter()
    .enumerate()
    {
        let id = format!("te_00000000000000c{n}");
        entry_by_hand(&ws, &id, target, &[("", b"payload")]);
        let dir = ws.file(&format!(
            ".trash/{}/{id}",
            &lil_schema::time::rfc3339(ws.now())[..10]
        ));
        let name = target.rsplit('/').next().unwrap();
        let record = std::fs::read(dir.join("restore.json")).unwrap();

        let refused = store.trash_restore(&id).await;
        assert!(
            matches!(&refused, Err(StoreError::PathInvalid { .. })),
            "{target}: {refused:?}"
        );
        assert_eq!(
            refused.unwrap_err().to_error("t").code,
            "store.path_invalid"
        );
        assert_eq!(
            std::fs::read(dir.join("payload").join(name)).unwrap(),
            b"payload",
            "{target}"
        );
        assert_eq!(std::fs::read(dir.join("restore.json")).unwrap(), record);
    }
    assert!(!ws.exists(&format!("{LEGACY_WORKSPACE_DIR}/x.txt")));
    assert!(!ws.exists(&format!("{WORKSPACE_DIR}/x.txt")));
    assert!(!ws.exists(".git/hooks/x"));
    assert_eq!(ws.read("registry/ledger.csv"), b"user's");
    assert_eq!(store.trash_list().await.unwrap().len(), 6);
}

#[tokio::test(start_paused = true)] // spec: CORE-026
async fn restore_under_a_version_folder_reapplies_read_only() {
    let ws = workspace();
    let store = ws.store();
    sealed_version(&ws, &store).await;
    let sealed = format!("{ASSET}/v1.0.0/a.png");
    let snapshot_rel = format!("{ASSET}/v1.0.0/manifest.snapshot.json");
    let (a, snapshot) = (ws.read(&sealed), ws.read(&snapshot_rel));

    // A sealed file or version folder is never moved to the trash (Minor 4 of the plan-2b
    // review); nothing moves.
    for path in [sealed.clone(), format!("{ASSET}/v1.0.0")] {
        let refused = store.trash(&p(&path), "test").await;
        assert!(
            matches!(refused, Err(StoreError::PathInvalid { .. })),
            "{path}: {refused:?}"
        );
    }
    assert!(read_only(&ws.file(&sealed)));
    assert_eq!(ws.read(&sealed), a);

    // A folder above it: only the files in the version folder are made read-only.
    let whole = store.trash(&p(ASSET), "test").await.unwrap();
    let payload = entry_dir(&ws, &whole).join("payload").join("crane");
    for name in ["a.png", "manifest.snapshot.json"] {
        clear_read_only(&payload.join("v1.0.0").join(name));
    }
    store.trash_restore(&whole.entry_id).await.unwrap();
    for name in ["a.png", "manifest.snapshot.json"] {
        assert!(
            read_only(&ws.file(&format!("{ASSET}/v1.0.0/{name}"))),
            "{name}"
        );
    }
    assert!(!read_only(&ws.file(MANIFEST)));

    // An entry for a sealed file, made before that rule (here by hand), restores read-only.
    let away = store.trash(&p(ASSET), "test").await.unwrap();
    entry_by_hand(&ws, "te_00000000000000a1", &sealed, &[("", &a)]);
    let restored = store.trash_restore("te_00000000000000a1").await.unwrap();
    assert_eq!(restored.as_str(), sealed);
    assert!(
        read_only(&ws.file(&sealed)),
        "the restored file is writable"
    );

    // The whole version folder too: every file comes back read-only.
    let version = format!("{ASSET}/v2.0.0");
    entry_by_hand(
        &ws,
        "te_00000000000000a2",
        &version,
        &[("a.png", &a), ("manifest.snapshot.json", &snapshot)],
    );
    store.trash_restore("te_00000000000000a2").await.unwrap();
    for name in ["a.png", "manifest.snapshot.json"] {
        assert!(read_only(&ws.file(&format!("{version}/{name}"))), "{name}");
    }
    assert!(away.entry_id.starts_with("te_"));

    // Elsewhere a restored file keeps the attributes it had.
    write_file(&ws.file("notes/a.txt"), b"a");
    let plain = store.trash(&p("notes/a.txt"), "test").await.unwrap();
    store.trash_restore(&plain.entry_id).await.unwrap();
    assert!(!read_only(&ws.file("notes/a.txt")));
}

#[tokio::test(start_paused = true)] // spec: ARCH-083, ARCH-058
async fn trash_waits_at_the_git_gate() {
    let ws = workspace();
    write_file(&ws.file("notes/a.txt"), b"a");
    write_file(&ws.file(".git/index.lock"), b"");
    let store = ws.store();
    let trashing = {
        let store = store.clone();
        tokio::spawn(async move { store.trash(&p("notes/a.txt"), "test").await })
    };
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(!trashing.is_finished(), "the trash waits while git works");
    assert!(ws.exists("notes/a.txt"));
    let result = trashing.await.unwrap();
    assert!(matches!(result, Err(StoreError::GitBusy)), "{result:?}");
    assert_eq!(ws.read("notes/a.txt"), b"a");
    assert!(store.trash_list().await.unwrap().is_empty());
}

/// Plan-2b Minor 4: `Store::trash` refuses a sealed version folder and anything in it at once,
/// also by an 8.3 short name, even while git works; an ordinary path waits at the git gate.
/// Nothing moves either way.
#[tokio::test(start_paused = true)] // spec: CORE-026, ARCH-083, ARCH-058
async fn store_trash_refuses_sealed_paths_and_waits_at_the_git_gate() {
    let ws = workspace();
    let store = ws.store();
    sealed_version(&ws, &store).await;
    write_file(&ws.file("notes/a.txt"), b"a");
    write_file(&ws.file(".git/index.lock"), b"");
    let sealed = format!("{ASSET}/v1.0.0/a.png");
    let version = ws.file(&format!("{ASSET}/v1.0.0"));
    let short = lil_platform::fs::short_name(&version).unwrap();
    let mut paths = vec![sealed.clone(), format!("{ASSET}/v1.0.0")];
    if short != version {
        let name = short.file_name().unwrap().to_string_lossy().into_owned();
        paths.push(format!("{ASSET}/{name}"));
        paths.push(format!("{ASSET}/{name}/a.png"));
    } else {
        println!("8.3 names are off on this volume: the short-name case is skipped");
    }
    for path in &paths {
        let refused = tokio::time::timeout(Duration::from_secs(1), store.trash(&p(path), "test"))
            .await
            .unwrap_or_else(|_| panic!("{path}: the refusal waited at the git gate"));
        assert!(
            matches!(refused, Err(StoreError::PathInvalid { .. })),
            "{path}: {refused:?}"
        );
    }
    assert!(read_only(&ws.file(&sealed)));
    assert_eq!(ws.read(&sealed), b"a");

    let trashing = {
        let store = store.clone();
        tokio::spawn(async move { store.trash(&p("notes/a.txt"), "test").await })
    };
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(!trashing.is_finished(), "the trash waits while git works");
    let result = trashing.await.unwrap();
    assert!(matches!(result, Err(StoreError::GitBusy)), "{result:?}");
    assert_eq!(ws.read("notes/a.txt"), b"a");
    assert!(store.trash_list().await.unwrap().is_empty());
}

/// Every file under `dir`, workspace-relative to `dir`, with its bytes.
fn files_of(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(at) = todo.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.push((path.strip_prefix(dir).unwrap().to_path_buf(), bytes));
            }
        }
    }
    out.sort();
    out
}

/// The trash's reserved folders named by their 8.3 short names (from `GetShortPathNameW`) are
/// refused like their long names, and nothing under them moves. On a volume with 8.3 names off
/// the real-volume half is skipped (the long names are still checked); the unit test
/// `reserved_check_runs_on_the_long_name` covers the short-name rule everywhere.
#[tokio::test(start_paused = true)] // spec: ARCH-058, CORE-040
async fn trash_of_a_reserved_folder_by_short_name_is_refused() {
    use lil_platform::names::{LEGACY_WORKSPACE_DIR, WORKSPACE_DIR};
    let ws = workspace();
    let store = ws.store();
    let reserved = [WORKSPACE_DIR, LEGACY_WORKSPACE_DIR, ".git", ".trash"];
    for dir in reserved {
        write_file(&ws.file(&format!("{dir}/keep.txt")), dir.as_bytes());
    }
    let before: Vec<_> = reserved.iter().map(|d| files_of(&ws.file(d))).collect();
    let mut shorts = Vec::new();
    for dir in reserved {
        let long = ws.file(dir);
        let short = lil_platform::fs::short_name(&long).unwrap();
        let mut names = vec![dir.to_string()];
        if short != long {
            let name = short.file_name().unwrap().to_string_lossy().into_owned();
            shorts.push((dir, name.clone()));
            names.push(name);
        }
        for name in names {
            for rel in [name.clone(), format!("{name}/keep.txt")] {
                let refused = store.trash(&p(&rel), "test").await;
                assert!(
                    matches!(&refused, Err(StoreError::PathInvalid { .. })),
                    "{dir} as {rel}: {refused:?}"
                );
            }
        }
    }
    if shorts.len() < reserved.len() {
        println!("8.3 names are off on this volume: the real-volume short-name half is skipped");
    }
    let after: Vec<_> = reserved.iter().map(|d| files_of(&ws.file(d))).collect();
    assert_eq!(before, after, "a refused trash moved something");
    assert!(store.trash_list().await.unwrap().is_empty());

    // A restore to a short name of one of them (a hand-edited `restore.json`) is refused too,
    // and the entry stays as it was.
    for (n, (dir, short)) in shorts.iter().enumerate() {
        let id = format!("te_00000000000000d{n}");
        entry_by_hand(
            &ws,
            &id,
            &format!("{short}/restored.txt"),
            &[("", b"payload")],
        );
        let refused = store.trash_restore(&id).await;
        assert!(
            matches!(&refused, Err(StoreError::PathInvalid { .. })),
            "restore to {short}: {refused:?}"
        );
        let entry = ws.file(&format!(
            ".trash/{}/{id}",
            &lil_schema::time::rfc3339(ws.now())[..10]
        ));
        assert_eq!(
            std::fs::read(entry.join("payload").join("restored.txt")).unwrap(),
            b"payload"
        );
        assert!(entry.join("restore.json").is_file());
        assert!(!ws.exists(&format!("{dir}/restored.txt")), "{dir}");
    }
    assert_eq!(store.trash_list().await.unwrap().len(), shorts.len());
}

/// A junction at `link` to the folder `target` (no privilege needed).
#[allow(clippy::disallowed_methods)]
fn junction(link: &Path, target: &Path) {
    lil_platform::fs::create_junction(link, target)
        .unwrap_or_else(|e| panic!("cannot create a junction at {}: {e}", link.display()));
}

/// Removes an empty folder as an outside program would.
#[allow(clippy::disallowed_methods)]
fn remove_empty_outside(path: &Path) {
    std::fs::remove_dir(path).unwrap();
}

/// The long form doesn't see through a junction, so the trash checks every folder on the way
/// for links, as a write does: a junction to a sealed version folder, to `.lilith\` or to
/// `.git\` never lets a file there be moved, and nothing moves (Task 3 review fix round 1).
#[tokio::test(start_paused = true)] // spec: ARCH-002, ARCH-058, CORE-026
async fn trash_refuses_a_path_through_a_linked_folder() {
    let ws = workspace();
    let store = ws.store();
    sealed_version(&ws, &store).await;
    write_file(&ws.file("notes/other.txt"), b"o");
    write_file(&ws.file(".lilith/keep.txt"), b"keep");
    write_file(&ws.file(".git/config"), b"[core]\n");
    let sealed = format!("{ASSET}/v1.0.0/a.png");
    junction(&ws.file("notes/j"), &ws.file(&format!("{ASSET}/v1.0.0")));
    junction(&ws.file("notes/k"), &ws.file(".lilith"));
    junction(&ws.file("notes/g"), &ws.file(".git"));
    for (rel, target) in [
        ("notes/j/a.png", sealed.as_str()),
        ("notes/k/keep.txt", ".lilith/keep.txt"),
        ("notes/g/config", ".git/config"),
    ] {
        let bytes = ws.read(target);
        let refused = store.trash(&p(rel), "test").await;
        assert!(
            matches!(&refused, Err(StoreError::PathInvalid { why, .. }) if why.contains("link")),
            "{rel}: {refused:?}"
        );
        assert_eq!(ws.read(target), bytes, "{rel} moved {target}");
    }
    assert!(read_only(&ws.file(&sealed)));
    assert!(store.trash_list().await.unwrap().is_empty());
}

/// Manifest `locks` are matched against the long form too: a locked file named by an 8.3
/// short name (its own, or one of a folder on the way) is `store.locked` for a write, a trash
/// and a restore, and nothing changes on disk.
#[tokio::test(start_paused = true)] // spec: CORE-035, ARCH-058
async fn locked_files_by_short_name_are_refused() {
    use lil_core::store::Expect;
    let ws = workspace();
    let store = ws.store();
    let locked = format!("{ASSET}/prompts/crane_prompt.txt");
    let mut m = Manifest::new(
        "sketchbook",
        "crane",
        "image",
        "00000000-0000-4000-8000-0000000000aa",
    );
    m.set("locks", json!(["prompts/crane_prompt.txt", "held_folder/"]));
    write_file(&ws.file(MANIFEST), m.to_text().as_bytes());
    write_file(&ws.file(&locked), b"a crane");

    let short_file = lil_platform::fs::short_name(&ws.file(&locked)).unwrap();
    let short_projects = lil_platform::fs::short_name(&ws.file("projects")).unwrap();
    if short_file == ws.file(&locked) || short_projects == ws.file("projects") {
        println!("8.3 names are off on this volume: the short-name cases are skipped");
        return;
    }
    let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let aliases = [
        format!("{ASSET}/prompts/{}", name(&short_file)),
        format!(
            "{}/sketchbook/assets/crane/prompts/crane_prompt.txt",
            name(&short_projects)
        ),
    ];
    for rel in &aliases {
        let written = store
            .write(
                &p(rel),
                b"over".to_vec(),
                Expect::Hash(sha256_bytes(b"a crane")),
            )
            .await;
        assert!(
            matches!(&written, Err(StoreError::Locked(_))),
            "write {rel}: {written:?}"
        );
        let trashed = store.trash(&p(rel), "test").await;
        assert!(
            matches!(&trashed, Err(StoreError::Locked(_))),
            "trash {rel}: {trashed:?}"
        );
        assert_eq!(ws.read(&locked), b"a crane", "{rel}");
    }
    // A restore into a locked folder through a short name on the way: the entry stays as it
    // was, and nothing lands there.
    let target = format!(
        "{}/sketchbook/assets/crane/held_folder/x.txt",
        name(&short_projects)
    );
    entry_by_hand(&ws, "te_00000000000000e1", &target, &[("", b"payload")]);
    let restored = store.trash_restore("te_00000000000000e1").await;
    assert!(
        matches!(&restored, Err(StoreError::Locked(_))),
        "restore {target}: {restored:?}"
    );
    let dir = ws.file(&format!(
        ".trash/{}/te_00000000000000e1",
        &lil_schema::time::rfc3339(ws.now())[..10]
    ));
    assert_eq!(
        std::fs::read(dir.join("payload/x.txt")).unwrap(),
        b"payload"
    );
    assert!(dir.join("restore.json").is_file());
    assert!(!ws.exists(&format!("{ASSET}/held_folder/x.txt")));
    assert_eq!(ws.read(&locked), b"a crane");
}

/// Ruling P3-R7: a trash of a folder is refused when a manifest `locks` entry lies at it or
/// anywhere under it (a whole asset or project too), naming the locked path, and nothing
/// moves. Containment is decided on long forms on both sides, so an entry written as an 8.3
/// alias (`PROMPT~1/`) protects the folder named by its long name. A folder without locks
/// still goes to the trash.
#[tokio::test(start_paused = true)] // spec: CORE-035, ARCH-058
async fn trash_of_a_folder_holding_a_locked_path_is_refused() {
    let ws = workspace();
    let store = ws.store();
    let locked = format!("{ASSET}/prompts/crane_prompt.txt");
    let manifest = |locks: Value| {
        let mut m = Manifest::new(
            "sketchbook",
            "crane",
            "image",
            "00000000-0000-4000-8000-0000000000aa",
        );
        m.set("locks", locks);
        write_file(&ws.file(MANIFEST), m.to_text().as_bytes());
    };
    manifest(json!(["prompts/crane_prompt.txt"]));
    write_file(&ws.file(&locked), b"a crane");
    write_file(&ws.file(&format!("{ASSET}/prompts/other.txt")), b"other");
    write_file(&ws.file(&format!("{ASSET}/free/a.txt")), b"free");

    for rel in [
        format!("{ASSET}/prompts"),
        ASSET.to_string(),
        "projects/sketchbook".to_string(),
        "projects".to_string(),
    ] {
        let refused = store.trash(&p(&rel), "test").await;
        assert!(
            matches!(&refused, Err(StoreError::Locked(at)) if at.as_str().eq_ignore_ascii_case(&locked)),
            "{rel}: {refused:?}"
        );
        assert_eq!(ws.read(&locked), b"a crane", "{rel}");
        assert_eq!(ws.read(&format!("{ASSET}/prompts/other.txt")), b"other");
    }
    assert!(store.trash_list().await.unwrap().is_empty());

    // A folder without locks, in the same asset, goes as before.
    store
        .trash(&p(&format!("{ASSET}/free")), "test")
        .await
        .unwrap();
    assert!(!ws.exists(&format!("{ASSET}/free/a.txt")));

    // A lock entry written as the 8.3 alias of a folder protects it by its long name.
    let folder = ws.file(&format!("{ASSET}/promptsfolder"));
    write_file(&folder.join("x.txt"), b"x");
    let short = lil_platform::fs::short_name(&folder).unwrap();
    if short == folder {
        println!("8.3 names are off on this volume: the short-alias lock entry is skipped");
        return;
    }
    let alias = short.file_name().unwrap().to_string_lossy().into_owned();
    manifest(json!([format!("{alias}/")]));
    for rel in [
        format!("{ASSET}/promptsfolder/x.txt"),
        format!("{ASSET}/promptsfolder"),
        ASSET.to_string(),
    ] {
        let refused = store.trash(&p(&rel), "test").await;
        assert!(
            matches!(&refused, Err(StoreError::Locked(_))),
            "{rel} (locked as {alias}/): {refused:?}"
        );
        assert_eq!(ws.read(&format!("{ASSET}/promptsfolder/x.txt")), b"x");
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-002, ARCH-058
async fn restore_refuses_a_linked_folder_on_the_way() {
    let ws = workspace();
    write_file(&ws.file("notes/sub/a.txt"), b"a");
    let store = ws.store();
    let record = store.trash(&p("notes/sub/a.txt"), "test").await.unwrap();
    // The emptied folder is replaced by a junction to a folder outside the workspace.
    let outside = ws._env.root().join("outside");
    write_file(&outside.join("keep.txt"), b"keep");
    remove_empty_outside(&ws.file("notes/sub"));
    junction(&ws.file("notes/sub"), &outside);
    let refused = store.trash_restore(&record.entry_id).await;
    assert!(
        matches!(&refused, Err(StoreError::PathInvalid { why, .. }) if why.contains("link")),
        "{refused:?}"
    );
    // Nothing landed outside, and the payload stays in the trash.
    assert!(!outside.join("a.txt").exists());
    let listed = store.trash_list().await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(
        entry_dir(&ws, &record)
            .join("payload")
            .join("a.txt")
            .is_file()
    );
}

// ---------------------------------------------------------------------------------------------
// Rejected runs (CORE-032).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: CORE-032
async fn rejected_runs_go_to_trash_90_days_after_signoff_unless_pinned() {
    let ws = workspace();
    let store = ws.store();
    let run = |id: &str, status: &str, extra: Value| {
        let mut run = json!({
            "run_id": id,
            "path": format!("candidates/{id}/"),
            "status": status,
            "created": "2027-01-10T08:00:00Z",
            "target_version": "1.0.0",
            "files": [{"path": format!("candidates/{id}/x.png"), "role": "candidate"}],
            "selected": null,
            "signed_off": null,
        });
        for (k, v) in extra.as_object().unwrap() {
            run[k] = v.clone();
        }
        run
    };
    let manifest = json!({
        "schema_version": 1,
        "asset_id": "crane",
        "project": "sketchbook",
        "approved_version": "1.0.0",
        "candidate_runs": [
            run("r_20270110_0800", "selected", json!({
                "selected": ["x.png"],
                "signed_off": {"by": "user", "date": "2027-01-15", "version": "1.0.0"},
            })),
            run("r_20270110_0801", "rejected", json!({})),
            run("r_20270110_0802", "rejected", json!({"pinned": true})),
            run("r_20270110_0803", "open", json!({})),
            // Rejected at a later sign-off whose version differs from its target.
            run("r_20270120_0800", "rejected", json!({"created": "2027-01-20T08:00:00Z",
                                                     "target_version": "1.1.0"})),
            run("r_20270120_0900", "selected", json!({
                "created": "2027-01-20T09:00:00Z",
                "selected": ["x.png"],
                "signed_off": {"by": "cli", "date": "2027-02-01", "version": "2.0.0"},
            })),
        ],
        "notes": "Ünïcödé space",
    });
    let text = serde_json::to_string_pretty(&manifest).unwrap() + "\n";
    write_file(&ws.file(MANIFEST), text.as_bytes());
    for id in [
        "r_20270110_0801",
        "r_20270110_0802",
        "r_20270110_0803",
        "r_20270120_0800",
    ] {
        write_file(
            &ws.file(&format!("{ASSET}/candidates/{id}/x.png")),
            id.as_bytes(),
        );
    }
    let before = ws.manifest();

    // The clock starts at the first sign-off (2027-01-15): 89 days later nothing moves.
    let early = store.maintenance(ws.after_days(89)).await.unwrap();
    assert!(early.runs_trashed.is_empty(), "{early:?}");
    assert_eq!(ws.manifest(), before);

    let later = store.maintenance(ws.after_days(2)).await.unwrap();
    let rejected = format!("{ASSET}/candidates/r_20270110_0801");
    assert_eq!(
        later
            .runs_trashed
            .iter()
            .map(WsPath::as_str)
            .collect::<Vec<_>>(),
        [rejected.as_str()]
    );
    assert!(!ws.exists(&rejected));
    for kept in ["r_20270110_0802", "r_20270110_0803", "r_20270120_0800"] {
        assert!(
            ws.exists(&format!("{ASSET}/candidates/{kept}/x.png")),
            "{kept}"
        );
    }
    let listed = store.trash_list().await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].original_path, rejected);

    // The run gained `trashed_utc` (last, RFC 3339) and nothing else changed.
    let after = ws.manifest();
    let mut want = before.clone();
    // 91 days after the clock's start, 2027-01-15T08:00:00Z.
    assert_eq!(utc::parse_rfc3339("2027-04-16T08:00:00Z"), ws.now());
    want["candidate_runs"][1]["trashed_utc"] = json!("2027-04-16T08:00:00Z");
    assert_eq!(after, want);
    let keys: Vec<&String> = after["candidate_runs"][1]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys.last().map(|k| k.as_str()), Some("trashed_utc"));

    // The run rejected at the 2027-02-01 sign-off goes 90 days after that one, once.
    let again = store.maintenance(ws.after_days(16)).await.unwrap();
    assert_eq!(
        again
            .runs_trashed
            .iter()
            .map(WsPath::as_str)
            .collect::<Vec<_>>(),
        [format!("{ASSET}/candidates/r_20270120_0800").as_str()]
    );
    let none = store.maintenance(ws.after_days(1)).await.unwrap();
    assert!(none.runs_trashed.is_empty(), "{none:?}");
    assert!(ws.exists(&format!("{ASSET}/candidates/r_20270110_0802/x.png")));
}

// ---------------------------------------------------------------------------------------------
// CORE-110: the only hard deletes.
// ---------------------------------------------------------------------------------------------

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test] // spec: CORE-110
fn hard_deletes_happen_only_in_purge_temp_cleanup_and_index_rebuild() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    // Whole-file and whole-tree deletes: the purge, temps, settled backups, recovery, empty
    // staging handles and the index rebuild.
    let deletes = |rel: &str| {
        matches!(
            rel,
            "store/fsops.rs" | "store/trash.rs" | "store/write.rs" | "store/staging.rs"
        ) || rel.starts_with("index/")
    };
    // Hash-checked deletes of the core's own bytes through one unshared handle: settled
    // backups (the store's and `suite.toml`'s) and a failed seal's own copies and snapshot.
    let checked = |rel: &str| {
        matches!(
            rel,
            "store/fsops.rs" | "store/write.rs" | "store/suite_toml.rs" | "store/versions.rs"
        )
    };
    type Allowed<'a> = &'a dyn Fn(&str) -> bool;
    let tokens: [(&str, Allowed); 4] = [
        ("remove_file(", &deletes),
        ("remove_dir_all(", &deletes),
        ("remove_if(", &checked),
        ("remove_if_hash(", &checked),
    ];
    let mut found = Vec::new();
    let mut outside = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).unwrap();
        for (i, line) in text.lines().enumerate() {
            for (token, allowed) in &tokens {
                if line.contains(token) {
                    let at = format!("{rel}:{}: {}", i + 1, line.trim());
                    if allowed(&rel) {
                        found.push(at);
                    } else {
                        outside.push(at);
                    }
                }
            }
        }
    }
    assert!(
        outside.is_empty(),
        "hard deletes outside the allowed places: {outside:#?}"
    );
    for (place, what) in [
        ("store/trash.rs", "the purge's delete"),
        ("store/versions.rs", "the seal rollback's checked delete"),
    ] {
        assert!(
            found.iter().any(|f| f.starts_with(place)),
            "{what} wasn't found: {found:#?}"
        );
    }
    assert!(!found.is_empty(), "the scan found no delete at all");
}

// ---------------------------------------------------------------------------------------------
// The trash tools and maintenance at open.
// ---------------------------------------------------------------------------------------------

/// Moves a file as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_move(from: &Path, to: &Path) {
    std::fs::rename(from, to).unwrap();
}

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("trash-tools-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[test] // spec: ARCH-058, CORE-037
fn trash_tools_list_and_restore() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("tools");
    env.register_workspace(&ws, true);
    let _core = start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    });
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    write_file(&ws.join("notes").join("a.txt"), b"keep me");

    let trashed = pipe.tool_ok("test.trash", json!({"path": "notes/a.txt"}), Some(&key()));
    let entry_id = trashed["entry_id"]
        .as_str()
        .expect("an entry_id")
        .to_string();
    assert!(!ws.join("notes").join("a.txt").exists());

    let list = pipe.tool_ok("trash.list", json!({}), None);
    let entries = list["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "{list}");
    assert_eq!(entries[0]["entry_id"], entry_id.as_str());
    assert_eq!(entries[0]["original_path"], "notes/a.txt");
    assert_eq!(entries[0]["sha256"], sha256_bytes(b"keep me").as_str());
    let refused = pipe.tool("trash.list", json!({"extra": 1}), None);
    assert_eq!(refused["error"]["code"], "params.invalid", "{refused}");

    // An occupied original path, then a free one.
    write_file(&ws.join("notes").join("a.txt"), b"new");
    let conflict = pipe.tool("trash.restore", json!({"entry_id": entry_id}), Some(&key()));
    assert_eq!(conflict["status"], "error", "{conflict}");
    assert_eq!(
        conflict["error"]["code"], "trash.restore_conflict",
        "{conflict}"
    );
    outside_move(
        &ws.join("notes").join("a.txt"),
        &ws.join("notes").join("b.txt"),
    );
    let restored = pipe.tool_ok("trash.restore", json!({"entry_id": entry_id}), Some(&key()));
    assert_eq!(restored["path"], "notes/a.txt", "{restored}");
    assert_eq!(
        std::fs::read(ws.join("notes").join("a.txt")).unwrap(),
        b"keep me"
    );

    let gone = pipe.tool("trash.restore", json!({"entry_id": entry_id}), Some(&key()));
    assert_eq!(gone["error"]["code"], "trash.unknown", "{gone}");
    let list = pipe.tool_ok("trash.list", json!({}), None);
    assert_eq!(list["entries"], json!([]), "{list}");
}

/// A trash entry written by hand, purged after `purge_after` (RFC 3339).
fn hand_entry(ws: &Path, id: &str, purge_after: &str) -> PathBuf {
    let dir = ws.join(".trash").join("2027-01-01").join(id);
    write_file(&dir.join("payload").join("old.txt"), b"old");
    let record = json!({
        "entry_id": id,
        "original_path": "notes/old.txt",
        "moved_utc": "2027-01-01T00:00:00Z",
        "purge_after": purge_after,
        "reason": "test",
    });
    write_file(&dir.join("restore.json"), record.to_string().as_bytes());
    dir
}

#[tokio::test(start_paused = true)] // spec: ARCH-058, CORE-032, CORE-037
async fn maintenance_runs_at_open_and_every_24_hours() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = canonical(&env.make_workspace("maint"));
    let clock = FakeClock::new();
    let now = secs(clock.utc_now());
    assert_eq!(utc::parse_rfc3339("2027-01-15T08:00:00Z"), now);
    let due = hand_entry(&ws, "te_00000000000000a1", "2027-01-15T07:59:59Z");
    let later = hand_entry(&ws, "te_00000000000000a2", "2027-01-16T08:00:00Z");

    let states = Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap());
    let dyn_clock: Arc<dyn Clock> = clock.clone();
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states,
        Arc::new(EventBus::new(dyn_clock.clone())),
        dyn_clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        Arc::new(SystemProbe),
        lil_core::watch::WatchConfig::default(),
    );
    let reference = WorkspaceRef {
        id: Env::workspace_hash(&std::fs::canonicalize(&ws).unwrap()),
        path: ws.clone(),
    };
    let open = hub.open(&reference).await.expect("the open");
    // The first pass runs in the background right after the open (on the paused clock, so
    // these waits take no real time).
    for _ in 0..1000 {
        if !due.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!due.exists(), "the first pass didn't purge the due entry");
    assert!(later.exists());

    // A day and a bit later (both clocks), the daily run purges the other one.
    clock.advance(Duration::from_secs(DAY + 60));
    tokio::time::sleep(Duration::from_secs(DAY + 60)).await;
    for _ in 0..1000 {
        if !later.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(!later.exists(), "the daily maintenance didn't run");
    drop(open);
    hub.close_all().await;
}

/// The real probe, except that every `restore.json` it is asked about waits until the gate
/// opens, announcing each wait first: a maintenance pass stalls inside the trash.
#[derive(Debug, Default)]
struct GatedTrash {
    open: Mutex<bool>,
    opened: Condvar,
    waits: Mutex<u32>,
    waited: Condvar,
}

impl GatedTrash {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    /// Blocks until `n` waits have begun, or the deadline passes (`false`).
    fn wait_for_waits(&self, n: u32) -> bool {
        let waits = self.waits.lock().unwrap();
        let (waits, _) = self
            .waited
            .wait_timeout_while(waits, DEADLINE, |w| *w < n)
            .unwrap();
        *waits >= n
    }
}

impl Probe for GatedTrash {
    fn identity(&self, path: &Path) -> std::io::Result<FileIdentity> {
        if path.file_name().is_some_and(|n| n == "restore.json") {
            *self.waits.lock().unwrap() += 1;
            self.waited.notify_all();
            let open = self.open.lock().unwrap();
            let _open = self
                .opened
                .wait_timeout_while(open, DEADLINE, |o| !*o)
                .unwrap();
        }
        SystemProbe.identity(path)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }
}

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-028, ARCH-058
async fn open_and_close_never_wait_for_maintenance() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = canonical(&env.make_workspace("gated"));
    let due = hand_entry(&ws, "te_00000000000000b1", "2027-01-15T07:59:59Z");
    let probe = Arc::new(GatedTrash::default());
    let clock: Arc<dyn Clock> = FakeClock::new();
    let hub = WorkspaceHub::new(
        env.env.clone(),
        Arc::new(StateRegistry::open(&env.env, no_fatal()).unwrap()),
        Arc::new(EventBus::new(clock.clone())),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        probe.clone(),
        lil_core::watch::WatchConfig::default(),
    );
    let reference = WorkspaceRef {
        id: Env::workspace_hash(&std::fs::canonicalize(&ws).unwrap()),
        path: ws.clone(),
    };

    // The open returns while its first maintenance pass is stalled in the trash.
    let open = tokio::time::timeout(DEADLINE, hub.open(&reference))
        .await
        .expect("the open waited for maintenance")
        .expect("the open");
    let gate = probe.clone();
    assert!(
        tokio::task::spawn_blocking(move || gate.wait_for_waits(1))
            .await
            .unwrap(),
        "the first pass never started"
    );
    assert!(due.exists(), "the stalled pass purged something");

    // The close isn't held up by the stalled pass either; the reopen waits for the close to
    // finish, so it returning proves the close did.
    drop(open);
    tokio::time::timeout(DEADLINE, hub.close(&reference.id))
        .await
        .expect("the close waited for maintenance");
    let again = tokio::time::timeout(DEADLINE, hub.open(&reference))
        .await
        .expect("the reopen waited for maintenance")
        .expect("the reopen");

    // Once the trash answers, a pass purges the due entry.
    probe.release();
    tokio::time::timeout(DEADLINE, async {
        while due.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the due entry was never purged");
    drop(again);
    hub.close_all().await;
}

#[tokio::test(start_paused = true)] // spec: CORE-032
async fn a_run_already_in_the_trash_gets_its_trashed_utc() {
    let ws = workspace();
    let store = ws.store();
    let manifest = json!({
        "asset_id": "crane",
        "project": "sketchbook",
        "candidate_runs": [
            {"run_id": "r_1", "status": "selected", "target_version": "1.0.0",
             "signed_off": {"by": "user", "date": "2027-01-15", "version": "1.0.0"}},
            {"run_id": "r_2", "status": "rejected", "target_version": "1.0.0",
             "created": "2027-01-10T08:00:00Z"},
            {"run_id": "r_3", "status": "rejected", "target_version": "1.0.0",
             "created": "2027-01-10T09:00:00Z"},
        ],
    });
    write_file(
        &ws.file(MANIFEST),
        (serde_json::to_string_pretty(&manifest).unwrap() + "\n").as_bytes(),
    );
    for run in ["r_2", "r_3"] {
        write_file(&ws.file(&format!("{ASSET}/candidates/{run}/x.png")), b"x");
    }
    // Each folder was moved, then the manifest edit didn't happen (a crash, or a failed
    // edit): r_2 long ago (its entry is purged in the same pass that records it), r_3 lately.
    let moved = |run: &'static str| {
        let store = store.clone();
        async move {
            store
                .trash(&p(&format!("{ASSET}/candidates/{run}")), "rejected")
                .await
                .unwrap()
        }
    };
    let early = moved("r_2").await;
    ws.after_days(85);
    let late = moved("r_3").await;
    let before = ws.manifest();

    let report = store.maintenance(ws.after_days(6)).await.unwrap();
    assert!(report.runs_trashed.is_empty(), "{report:?}");
    assert_eq!(report.purged, std::slice::from_ref(&early.entry_id));
    let mut want = before.clone();
    want["candidate_runs"][1]["trashed_utc"] = json!(early.moved_utc);
    want["candidate_runs"][2]["trashed_utc"] = json!(late.moved_utc);
    assert_eq!(ws.manifest(), want);
    assert_eq!(store.trash_list().await.unwrap(), [late]);
    // Once recorded, nothing more happens.
    let again = store.maintenance(ws.after_days(1)).await.unwrap();
    assert!(
        again.runs_trashed.is_empty() && again.purged.is_empty(),
        "{again:?}"
    );
    assert_eq!(ws.manifest(), want);
}

/// The real probe, except that every `restore.json` is a cloud-only placeholder, which must
/// never be opened.
#[derive(Debug)]
struct CloudRecords;

impl Probe for CloudRecords {
    fn identity(&self, path: &Path) -> std::io::Result<FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if path.file_name().is_some_and(|n| n == "restore.json") {
            id.attributes |= lil_platform::fs::PLACEHOLDER_ATTRIBUTES;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        assert!(
            path.file_name().is_none_or(|n| n != "restore.json"),
            "a cloud-only restore.json was read: {}",
            path.display()
        );
        SystemProbe.open_read(path)
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-058, ARCH-079
async fn cloud_only_restore_records_are_skipped_never_read() {
    let ws = workspace();
    let dir = hand_entry(&ws.root, "te_00000000000000c1", "2027-01-15T07:59:59Z");
    let store = Arc::new(
        Store::new(
            ws.root.clone(),
            ws.state.clone(),
            FailPoints::none(),
            ws.clock.clone(),
            Arc::new(SeqIds::new()),
            Arc::new(CloudRecords),
        )
        .with_machine("testmachine"),
    );
    assert!(store.trash_list().await.unwrap().is_empty());
    let report = store.maintenance(ws.now()).await.unwrap();
    assert!(report.purged.is_empty(), "{report:?}");
    assert!(dir.join("payload").join("old.txt").exists());
    assert_eq!(
        store.trash_restore("te_00000000000000c1").await,
        Err(StoreError::TrashUnknown("te_00000000000000c1".to_string()))
    );
}

// ---------------------------------------------------------------------------------------------
// Ruling Q19: a folder move retries error 5.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-074, ARCH-058
async fn folder_moves_retry_access_denied_and_file_moves_do_not() {
    let ws = workspace();
    let fp = FailPoints::none();
    let store = Store::new(
        ws.root.clone(),
        ws.state.clone(),
        fp.clone(),
        ws.clock.clone(),
        Arc::new(SeqIds::new()),
        Arc::new(SystemProbe),
    )
    .with_machine("testmachine");
    write_file(&ws.file("notes/dir/a.txt"), b"a");
    write_file(&ws.file("notes/b.txt"), b"b");
    // A handle open inside a folder makes its rename fail with 5, whatever the share mode
    // (MS-FSA 2.1.5.15.12): the first three moves are refused that way.
    let denied = Arc::new(AtomicU64::new(3));
    let left = denied.clone();
    fp.on(
        "store.replace",
        Box::new(move || {
            if left.load(Ordering::SeqCst) > 0 {
                left.fetch_sub(1, Ordering::SeqCst);
                return Err(std::io::Error::from_raw_os_error(5));
            }
            Ok(())
        }),
    );
    store
        .trash(&p("notes/dir"), "test")
        .await
        .expect("a folder move retries error 5");
    assert_eq!(
        denied.load(Ordering::SeqCst),
        0,
        "all three refusals were retried"
    );
    assert!(!ws.exists("notes/dir"));

    // A file move never retries 5: it is a real denial.
    denied.store(1, Ordering::SeqCst);
    let refused = store.trash(&p("notes/b.txt"), "test").await.unwrap_err();
    assert!(matches!(refused, StoreError::Io { .. }), "{refused:?}");
    assert!(ws.exists("notes/b.txt"));
}

/// Ruling P3-R7 for restores: a folder whose manifest now has a `locks` entry beneath it (set
/// after the folder went to the trash) is not restored, as it could not be trashed, with
/// `store.locked`; the entry stays in the trash untouched. Once the lock goes, the restore
/// works.
#[tokio::test(start_paused = true)] // spec: CORE-035, ARCH-058
async fn folder_restore_with_a_locked_entry_beneath_is_refused() {
    let ws = workspace();
    let store = ws.store();
    let manifest = |locks: Value| {
        let mut m = Manifest::new(
            "sketchbook",
            "crane",
            "image",
            "00000000-0000-4000-8000-0000000000aa",
        );
        m.set("locks", locks);
        write_file(&ws.file(MANIFEST), m.to_text().as_bytes());
    };
    manifest(json!([]));
    let inside = format!("{ASSET}/prompts/crane_prompt.txt");
    write_file(&ws.file(&inside), b"a crane");
    let record = store
        .trash(&p(&format!("{ASSET}/prompts")), "test")
        .await
        .unwrap();
    assert!(!ws.exists(&inside));

    for lock in [json!(["prompts/crane_prompt.txt"]), json!(["PROMPTS/"])] {
        manifest(lock.clone());
        let refused = store.trash_restore(&record.entry_id).await;
        assert!(
            matches!(&refused, Err(StoreError::Locked(_))),
            "{lock}: {refused:?}"
        );
        assert!(!ws.exists(&inside), "{lock}");
        assert_eq!(
            std::fs::read(entry_dir(&ws, &record).join("payload/prompts/crane_prompt.txt"))
                .unwrap(),
            b"a crane",
            "{lock}"
        );
        assert!(entry_dir(&ws, &record).join("restore.json").is_file());
    }

    manifest(json!([]));
    store.trash_restore(&record.entry_id).await.unwrap();
    assert_eq!(ws.read(&inside), b"a crane");
}

// ---------------------------------------------------------------------------------------------
// The whole-asset trash (plan 4b, Task 11; CORE-157, CORE-037, ARCH-125; P4b-R9, P4b-R24,
// P4b-R39): `asset.trash` and `trash.restore {to}`.
// ---------------------------------------------------------------------------------------------

/// The fixture workspace's project and assets (the spec fixture).
const P4: &str = "sketchbook";
const CRANE: &str = "chr_paper_crane";
const LAMP: &str = "prop_signal_lamp";
const ORIGAMI: &str = "chr_origami";
/// The crane's folder as `asset.trash`'s touch names it.
const CRANE_TOUCH: &str = "projects/sketchbook/assets/chr_paper_crane/";

fn tkey() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "asset-trash-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn ws_file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

fn asset_path(ws: &Path, asset: &str) -> PathBuf {
    ws_file(ws, &format!("projects/{P4}/assets/{asset}"))
}

fn read_json_file(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Edits `asset`'s manifest by hand, as an editor would.
fn edit_by_hand(ws: &Path, asset: &str, f: impl FnOnce(&mut Value)) {
    let path = asset_path(ws, asset).join(format!("{asset}.manifest.json"));
    let mut m = read_json_file(&path);
    f(&mut m);
    write_file(&path, serde_json::to_string_pretty(&m).unwrap().as_bytes());
}

/// The crane's manifest `locks` go: the fixture locks its prompt, and an asset holding a
/// locked path is never trashed (ruling P3-R7; `asset_trash_is_refused_while_the_asset_has_a_lock`).
fn crane_unlocked(ws: &Path) {
    edit_by_hand(ws, CRANE, |m| m["locks"] = json!([]));
}

/// The lamp takes the crane's first version as an input: the crane's one dependent.
fn lamp_uses_crane(ws: &Path) {
    edit_by_hand(ws, LAMP, |m| {
        m["inputs"] = json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]);
    });
}

/// Every file under `dir`: its path relative to `dir` and its SHA-256.
fn tree(dir: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, sha256_bytes(&std::fs::read(&path).unwrap())));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Renames a folder as Explorer would.
#[allow(clippy::disallowed_methods)]
fn explorer_rename(from: &Path, to: &Path) {
    std::fs::rename(from, to).unwrap_or_else(|e| panic!("cannot rename {}: {e}", from.display()));
}

/// Copies a folder as Explorer would (read-only files stay read-only).
#[allow(clippy::disallowed_methods)]
fn explorer_copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            explorer_copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// What a fake [`AssetUse`] answers, and how often it was asked.
#[derive(Default)]
struct FakeUse {
    answer: Mutex<Option<Result<Option<InUse>, String>>>,
    calls: AtomicU64,
    /// On this call (1-based) the lamp first takes the crane as an input (0: never).
    dependent_at: AtomicU64,
}

impl FakeUse {
    fn set(&self, answer: Result<Option<InUse>, String>) {
        *self.answer.lock().unwrap() = Some(answer);
    }
}

impl AssetUse for FakeUse {
    fn in_use<'a>(
        &'a self,
        workspace: &'a OpenWorkspace,
        _project: &'a str,
        _asset_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<InUse>, ErrorBody>> + Send + 'a>,
    > {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n == self.dependent_at.load(Ordering::SeqCst) {
                lamp_uses_crane(&workspace.ws.path);
                workspace
                    .index
                    .reconcile(IndexScope::Paths(vec![p(&format!(
                        "projects/{P4}/assets/{LAMP}"
                    ))]))
                    .await
                    .unwrap();
            }
            match self.answer.lock().unwrap().clone().unwrap_or(Ok(None)) {
                Ok(answer) => Ok(answer),
                Err(why) => Err(ErrorBody::new(
                    &lil_protocol::codes::STATE_UNAVAILABLE,
                    why,
                    "",
                )),
            }
        })
    }
}

/// An in-process core's dispatcher over a copy of the fixture workspace, with the
/// confirmations' stage (an app is connected, so they can be answered) and the asset services
/// given (QA-031: the fake clock and ids).
struct AssetFx {
    dispatcher: Dispatcher,
    confirm: Arc<Confirm>,
    hub: Arc<WorkspaceHub>,
    ws_ref: WorkspaceRef,
    ws: PathBuf,
    states: Arc<StateRegistry>,
    env: TestEnv,
}

impl AssetFx {
    /// With `services` wired (or not) as given. The crane's lock goes first ([`crane_unlocked`]);
    /// `setup` may put one back.
    async fn with(setup: impl FnOnce(&Path), services: AssetServices) -> AssetFx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("asset-trash");
        crane_unlocked(&ws);
        setup(&ws);
        env.register_workspace(&ws, true);
        let canonical = std::fs::canonicalize(&ws).expect("canonicalize");
        let text = canonical.to_string_lossy().into_owned();
        let ws_ref = WorkspaceRef {
            id: lil_platform::Env::workspace_hash(&canonical),
            path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
        };
        let states =
            Arc::new(StateRegistry::open(&env.env, no_fatal()).expect("the suite state.db"));
        let clock: Arc<dyn Clock> = FakeClock::new();
        let events = Arc::new(EventBus::new(clock.clone()));
        let ids: Arc<dyn lil_platform::IdSource> = Arc::new(SeqIds::new());
        let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            FailPoints::none(),
            probe.clone(),
            lil_core::watch::WatchConfig::default(),
        );
        let reg = core_registry(
            true,
            &Arc::new(TestGates::new()),
            &states,
            &hub,
            &env.env,
            &probe,
            Arc::default(),
            &services,
        );
        let confirm = Confirm::new(
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            UiPresence::new(),
            Arc::new(NoNotificationPath),
            Duration::from_secs(30 * 60),
            McpConfirmation::Core,
            FailPoints::none(),
        );
        confirm.presence().opened();
        let hooks = PipelineHooks {
            confirm: AgentConfirm::new(confirm.clone()),
            ..PipelineHooks::plan2()
        };
        let lifecycle = Lifecycle::new(clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
        let dispatcher = Dispatcher::new(
            reg,
            hooks,
            states.clone(),
            events,
            hub.clone(),
            ids,
            clock,
            lifecycle,
        );
        let fx = AssetFx {
            dispatcher,
            confirm,
            hub,
            ws_ref,
            ws,
            states,
            env,
        };
        fx.open().await.index.ready().await;
        fx
    }

    /// With a fake [`AssetUse`] wired.
    async fn faked(setup: impl FnOnce(&Path)) -> (AssetFx, Arc<FakeUse>) {
        let fake = Arc::new(FakeUse::default());
        let services = AssetServices::unwired();
        let wired: Arc<dyn AssetUse> = fake.clone();
        let _ = services.asset_use.set(wired);
        (AssetFx::with(setup, services).await, fake)
    }

    async fn shutdown(self) {
        let AssetFx {
            dispatcher,
            hub,
            states,
            env,
            confirm,
            ..
        } = self;
        hub.close_all().await;
        states.close();
        drop((dispatcher, hub, states, confirm));
        if let Err(left) = env.close() {
            panic!("the test's folder was left behind: {left:?}");
        }
    }

    async fn open(&self) -> Arc<OpenWorkspace> {
        self.hub
            .open(&self.ws_ref)
            .await
            .expect("the workspace opens")
    }

    async fn reconcile(&self) {
        let open = self.open().await;
        open.index.ready().await;
        open.index
            .reconcile(IndexScope::All)
            .await
            .expect("a reconcile");
    }

    fn session(&self, caller: CallerKind, id: &str) -> Arc<Session> {
        Arc::new(Session {
            id: id.into(),
            caller,
            non_interactive: false,
            workspace: Some(self.ws_ref.clone()),
            capabilities: lil_protocol::CAPABILITIES
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
            client_pid: 1,
            profile_id: None,
        })
    }

    fn app(&self) -> Arc<Session> {
        self.session(CallerKind::Ui("test".into()), "s_app")
    }

    fn cli(&self) -> Arc<Session> {
        self.session(CallerKind::Cli, "s_cli")
    }

    /// `name` called by `s` with `args` and the idempotency key `key` (none for a read).
    async fn call_as(
        &self,
        s: &Arc<Session>,
        name: &str,
        args: Value,
        key: Option<&str>,
    ) -> Outcome<ToolOutput> {
        let Value::Object(arguments) = args else {
            panic!("arguments must be an object");
        };
        self.dispatcher
            .call(
                s.clone(),
                ToolCallParams {
                    name: name.into(),
                    arguments,
                    idempotency_key: key.map(str::to_string),
                    trace_id: None,
                },
            )
            .await
    }

    /// `name` called by the cli session (a fresh key for a tool that isn't `read`).
    async fn call(&self, name: &str, args: Value) -> Outcome<ToolOutput> {
        let read = matches!(
            self.dispatcher
                .registry()
                .get(name)
                .map(|t| t.descriptor().effect),
            Some(lil_protocol::EffectClass::Read)
        );
        let k = (!read).then(tkey);
        self.call_as(&self.cli(), name, args, k.as_deref()).await
    }

    async fn ok(&self, name: &str, args: Value) -> Value {
        ok_of(
            &self.call(name, args.clone()).await,
            &format!("{name} {args}"),
        )
    }

    async fn err(&self, name: &str, args: Value) -> ErrorBody {
        err_of(
            &self.call(name, args.clone()).await,
            &format!("{name} {args}"),
        )
    }

    async fn answer(&self, id: &str, answer: Answer) {
        self.confirm
            .answer(
                &self.app(),
                ConfirmationAnswerParams {
                    confirmation_id: id.into(),
                    answer,
                    toast_token: None,
                    cap: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("the answer was refused: {e:?}"));
    }

    /// The pending confirmation `id`, as the app lists it, as JSON.
    async fn pending_view(&self, id: &str) -> Value {
        let row = self
            .confirm
            .list(ListState::Pending, 500)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} is not pending"));
        serde_json::to_value(row.view()).unwrap()
    }

    async fn pending_count(&self) -> usize {
        self.confirm
            .list(ListState::Pending, 500)
            .await
            .unwrap()
            .len()
    }

    /// `asset.trash` of `asset` by `s`, approved by the app when it asks: the last outcome.
    async fn trash_approved(&self, s: &Arc<Session>, asset: &str) -> Outcome<ToolOutput> {
        let args = json!({"project": P4, "asset_id": asset});
        let k = tkey();
        let first = self.call_as(s, "asset.trash", args.clone(), Some(&k)).await;
        let Outcome::PendingConfirmation(p) = &first else {
            return first;
        };
        self.answer(&p.confirmation_id, Answer::Approve).await;
        self.call_as(s, "asset.trash", args, Some(&k)).await
    }

    /// The app trashes `asset` (approved): its `entry_id`.
    async fn trash_ok(&self, asset: &str) -> String {
        let out = self.trash_approved(&self.app(), asset).await;
        ok_of(&out, &format!("asset.trash {asset}"))["entry_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn ws_id(&self) -> String {
        self.ws_ref.id.clone()
    }

    fn sealed(&self, asset: &str) -> String {
        format!("lil://{}/{P4}/{asset}@1.0.0", self.ws_id())
    }

    async fn trash_entries(&self) -> Vec<Value> {
        self.ok("trash.list", json!({"scope": "workspace"})).await["entries"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn redirects(&self) -> Option<String> {
        std::fs::read_to_string(ws_file(&self.ws, "registry/redirects.yaml")).ok()
    }

    async fn status_needs_repair(&self) -> Vec<String> {
        let status = self.ok("project.status", json!({ "project": P4 })).await;
        status["needs_repair"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }
}

fn ok_of(out: &Outcome<ToolOutput>, what: &str) -> Value {
    match out {
        Outcome::Ok(out) => out.structured.clone(),
        other => panic!("{what}: expected ok, got {other:?}"),
    }
}

fn err_of(out: &Outcome<ToolOutput>, what: &str) -> ErrorBody {
    match out {
        Outcome::Error { error } => error.clone(),
        other => panic!("{what}: expected an error, got {other:?}"),
    }
}

fn pending_of(out: &Outcome<ToolOutput>) -> lil_protocol::PendingConfirmation {
    match out {
        Outcome::PendingConfirmation(p) => p.clone(),
        other => panic!("expected pending_confirmation, got {other:?}"),
    }
}

fn trash_args(asset: &str) -> Value {
    json!({"project": P4, "asset_id": asset})
}

/// CORE-157, APP-115 (ruling 2): `asset.trash` asks the app too, and agents; its confirmation
/// lists the folder it deletes (`files`), the dependents and the sealed versions (`facts`,
/// present, with empty lists, for an asset that has neither); nothing moves before the answer;
/// approved, the retry moves the folder as one entry and returns the dependents. Every other
/// tool still runs at once for the app.
#[tokio::test] // spec: CORE-157, APP-115, ARCH-052, CORE-109
async fn asset_trash_asks_even_the_app_and_lists_dependents_and_versions() {
    let (fx, _fake) = AssetFx::faked(lamp_uses_crane).await;
    let k = tkey();
    let p = pending_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
            .await,
    );
    let view = fx.pending_view(&p.confirmation_id).await;
    assert_eq!(view["caller_kind"], "ui:test", "{view}");
    assert_eq!(
        view["files"],
        json!([{"path": CRANE_TOUCH, "access": "delete"}]),
        "{view}"
    );
    assert_eq!(
        view["facts"],
        json!({"dependents": [format!("{P4}/{LAMP}")], "dependents_total": 1, "versions": ["1.0.0"]}),
        "{view}"
    );
    assert!(
        asset_path(&fx.ws, CRANE).is_dir(),
        "nothing moves before the answer"
    );
    // An agent is asked too; the project falls back to the call's in the touch.
    let agent = pending_of(
        &fx.call_as(
            &fx.cli(),
            "asset.trash",
            json!({"asset_id": CRANE}),
            Some(&tkey()),
        )
        .await,
    );
    let agent_view = fx.pending_view(&agent.confirmation_id).await;
    assert_eq!(
        agent_view["files"],
        json!([{"path": CRANE_TOUCH, "access": "delete"}]),
        "{agent_view}"
    );

    // An asset with no dependents and no versions: the facts are there, with empty lists.
    let source = fx.env.root().join("sources").join("empty.png");
    write_file(&source, b"empty");
    fx.ok(
        "asset.import",
        json!({"project": P4, "asset_id": "prop_empty", "type": "image",
               "files": [source.to_string_lossy()], "mode": "copy"}),
    )
    .await;
    let empty = pending_of(
        &fx.call_as(
            &fx.app(),
            "asset.trash",
            trash_args("prop_empty"),
            Some(&tkey()),
        )
        .await,
    );
    assert_eq!(
        fx.pending_view(&empty.confirmation_id).await["facts"],
        json!({"dependents": [], "dependents_total": 0, "versions": []})
    );

    fx.answer(&p.confirmation_id, Answer::Approve).await;
    let ran = ok_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
            .await,
        "the approved retry",
    );
    assert_eq!(ran["dependents"], json!([format!("{P4}/{LAMP}")]), "{ran}");
    let entry_id = ran["entry_id"].as_str().unwrap();
    assert!(!asset_path(&fx.ws, CRANE).exists());
    let entries = fx.trash_entries().await;
    assert_eq!(
        entries.len(),
        1,
        "one entry for the whole folder: {entries:?}"
    );
    assert_eq!(entries[0]["entry_id"], entry_id);
    assert_eq!(entries[0]["reason"], "asset.trash");
    assert_eq!(
        entries[0]["original_path"],
        format!("projects/{P4}/assets/{CRANE}")
    );
    // The replay is the same outcome; it ran once.
    let replay = fx
        .call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
        .await;
    assert_eq!(ok_of(&replay, "the replay"), ran);

    // The app's other calls never ask (CORE-109).
    let marked = fx
        .call_as(
            &fx.app(),
            "asset.mark",
            json!({"project": P4, "asset_id": LAMP, "rating": 3}),
            Some(&tkey()),
        )
        .await;
    ok_of(&marked, "asset.mark");
    fx.shutdown().await;
}

/// CORE-157, CORE-037: the whole folder moves as one entry, its sealed files unmodified, and
/// the restore brings every file back as it was, read-only re-applied to the sealed files (a
/// sealed file whose hash no longer matches its manifest's `hashes` is left writable).
#[tokio::test] // spec: CORE-157, CORE-037, CORE-026, ARCH-058
async fn trash_moves_the_folder_with_sealed_versions_unmodified() {
    let (fx, _fake) = AssetFx::faked(|_| {}).await;
    let crane = asset_path(&fx.ws, CRANE);
    let before = tree(&crane);
    let entry_id = fx.trash_ok(CRANE).await;
    let record: TrashRecord = serde_json::from_value(fx.trash_entries().await[0].clone()).unwrap();
    let payload = ws_file(
        &fx.ws,
        &format!(
            ".trash/{}/{entry_id}/payload/{CRANE}",
            &record.moved_utc[..10]
        ),
    );
    assert_eq!(
        tree(&payload),
        before,
        "the payload is the folder, unmodified"
    );

    let restored = fx
        .ok("trash.restore", json!({ "entry_id": entry_id }))
        .await;
    assert_eq!(restored["path"], format!("projects/{P4}/assets/{CRANE}"));
    assert_eq!(tree(&crane), before, "every file back, unmodified");
    for sealed in ["crane_b.png", "manifest.snapshot.json"] {
        assert!(
            read_only(&crane.join("v1.0.0").join(sealed)),
            "{sealed} is sealed again"
        );
    }
    assert!(!read_only(&crane.join(format!("{CRANE}.manifest.json"))));
    assert!(fx.trash_entries().await.is_empty());
    let detail = fx.ok("asset.get", trash_args(CRANE)).await;
    assert_eq!(detail["approved_version"], "1.0.0", "{detail}");

    // A sealed file changed while in the trash isn't sealed again; the rest is.
    let entry_id = fx.trash_ok(CRANE).await;
    let record: TrashRecord = serde_json::from_value(fx.trash_entries().await[0].clone()).unwrap();
    let payload = ws_file(
        &fx.ws,
        &format!(
            ".trash/{}/{entry_id}/payload/{CRANE}",
            &record.moved_utc[..10]
        ),
    );
    let sealed = payload.join("v1.0.0").join("crane_b.png");
    clear_read_only(&sealed);
    write_file(&sealed, b"changed in the trash");
    fx.ok("trash.restore", json!({ "entry_id": entry_id }))
        .await;
    assert!(
        !read_only(&crane.join("v1.0.0").join("crane_b.png")),
        "a mismatch stays writable"
    );
    assert!(read_only(
        &crane.join("v1.0.0").join("manifest.snapshot.json")
    ));
    fx.shutdown().await;
}

/// P4b-R9, P4b-R24: the approved retry recomputes the facts; a dependent added since you
/// approved makes a new confirmation instead of running, and that one, approved, runs.
#[tokio::test] // spec: CORE-157, CORE-091
async fn a_retry_asks_again_when_dependents_changed() {
    let (fx, _fake) = AssetFx::faked(|_| {}).await;
    let k = tkey();
    let first = pending_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
            .await,
    );
    assert_eq!(
        fx.pending_view(&first.confirmation_id).await["facts"]["dependents"],
        json!([])
    );
    fx.answer(&first.confirmation_id, Answer::Approve).await;

    lamp_uses_crane(&fx.ws);
    fx.reconcile().await;
    let second = pending_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
            .await,
    );
    assert_ne!(second.confirmation_id, first.confirmation_id);
    assert_eq!(
        fx.pending_view(&second.confirmation_id).await["facts"]["dependents"],
        json!([format!("{P4}/{LAMP}")])
    );
    assert!(asset_path(&fx.ws, CRANE).is_dir(), "nothing moved");
    fx.answer(&second.confirmation_id, Answer::Approve).await;
    let ran = fx
        .call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
        .await;
    assert_eq!(
        ok_of(&ran, "the second approval")["dependents"],
        json!([format!("{P4}/{LAMP}")])
    );
    assert!(!asset_path(&fx.ws, CRANE).exists());
    fx.shutdown().await;
}

/// Ruling 4 (Task 10 audit L6): the result's dependents are those found at the move, under the
/// version lock, not those the confirmation showed.
#[tokio::test] // spec: CORE-157
async fn the_result_names_the_dependents_found_at_the_move() {
    let (fx, fake) = AssetFx::faked(|_| {}).await;
    // Calls 1 and 2 are the facts' (the question, the approved retry); call 3 is the run's.
    fake.dependent_at.store(3, Ordering::SeqCst);
    let out = fx.trash_approved(&fx.app(), CRANE).await;
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        ok_of(&out, "the trash")["dependents"],
        json!([format!("{P4}/{LAMP}")])
    );
    let history = fx.confirm.list(ListState::History, 10).await.unwrap();
    assert_eq!(history.len(), 1, "it was asked once");
    assert_eq!(history[0].facts.as_ref().unwrap().dependents_total, 0);
    fx.shutdown().await;
}

/// P4b-R32, PF-11: with the `asset_use` slot empty, `asset.trash` fails closed with
/// `state.unavailable` before anything is asked, and nothing moves.
#[tokio::test] // spec: CORE-157
async fn an_unwired_asset_use_refuses_asset_trash() {
    let fx = AssetFx::with(|_| {}, AssetServices::unwired()).await;
    for s in [fx.app(), fx.cli()] {
        let out = fx
            .call_as(&s, "asset.trash", trash_args(CRANE), Some(&tkey()))
            .await;
        assert_eq!(err_of(&out, "unwired").code, "state.unavailable", "{out:?}");
    }
    assert_eq!(fx.pending_count().await, 0, "nothing was asked");
    assert!(asset_path(&fx.ws, CRANE).is_dir());
    assert!(fx.trash_entries().await.is_empty());
    fx.shutdown().await;
}

/// Ruling P3-R7 (controller ruling for Task 11): an asset holding a path its manifest `locks`
/// is never trashed. `asset.trash` is `store.locked` naming the path and saying to remove the
/// lock first, before anything is asked, and nothing moves; once the lock is removed, the trash
/// asks and works.
#[tokio::test] // spec: CORE-157, CORE-035
async fn asset_trash_is_refused_while_the_asset_has_a_lock() {
    let (fx, _fake) = AssetFx::faked(|ws| {
        edit_by_hand(ws, CRANE, |m| m["locks"] = json!(["prompts/crane.txt"]));
    })
    .await;
    let before = tree(&asset_path(&fx.ws, CRANE));
    for s in [fx.app(), fx.cli()] {
        let e = err_of(
            &fx.call_as(&s, "asset.trash", trash_args(CRANE), Some(&tkey()))
                .await,
            "a locked asset",
        );
        assert_eq!(e.code, "store.locked", "{e:?}");
        let locked = format!("projects/{P4}/assets/{CRANE}/prompts/crane.txt");
        assert!(e.message.contains(&locked), "names the path: {}", e.message);
        assert!(
            e.message
                .contains("Remove that path from the manifest's locks first"),
            "{}",
            e.message
        );
        assert_eq!(e.data.as_ref().unwrap()["path"], locked.as_str(), "{e:?}");
    }
    assert_eq!(fx.pending_count().await, 0, "nothing was asked");
    assert_eq!(tree(&asset_path(&fx.ws, CRANE)), before, "nothing moved");
    assert!(fx.trash_entries().await.is_empty());

    // The lock is removed: the trash asks, and runs once approved.
    edit_by_hand(&fx.ws, CRANE, |m| m["locks"] = json!([]));
    let k = tkey();
    let p = pending_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
            .await,
    );
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    let ran = fx
        .call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
        .await;
    ok_of(&ran, "the trash after the unlock");
    assert!(!asset_path(&fx.ws, CRANE).exists());
    fx.shutdown().await;
}

/// P4b-R24: the in-use check runs before the question (no confirmation is made) and again at
/// the approved retry (nothing moves); an error from the check refuses with it.
#[tokio::test] // spec: CORE-157
async fn the_in_use_check_runs_before_the_question_and_at_the_retry() {
    let (fx, fake) = AssetFx::faked(|_| {}).await;
    for (answer, by) in [
        (InUse::Job, "job"),
        (InUse::Signoff, "signoff"),
        (InUse::Hold, "hold"),
    ] {
        fake.set(Ok(Some(answer)));
        let e = err_of(
            &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&tkey()))
                .await,
            "in use",
        );
        assert_eq!(e.code, "asset.in_use", "{e:?}");
        assert_eq!(
            e.message,
            "This asset is in use by a job, a sign-off or an open document. Try again when that is done."
        );
        assert_eq!(e.data.as_ref().unwrap()["by"], by);
    }
    fake.set(Err("the jobs can't be read".into()));
    let e = err_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&tkey()))
            .await,
        "failing check",
    );
    assert_eq!(e.code, "state.unavailable", "{e:?}");
    assert_eq!(fx.pending_count().await, 0, "nothing was asked");

    fake.set(Ok(None));
    let k = tkey();
    let p = pending_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
            .await,
    );
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    fake.set(Ok(Some(InUse::Hold)));
    let e = err_of(
        &fx.call_as(&fx.app(), "asset.trash", trash_args(CRANE), Some(&k))
            .await,
        "in use at the retry",
    );
    assert_eq!(e.code, "asset.in_use", "{e:?}");
    assert!(asset_path(&fx.ws, CRANE).is_dir(), "nothing moved");
    assert!(fx.trash_entries().await.is_empty());
    fx.shutdown().await;
}

/// Ruling 3 (Task 10 audit L4): `asset.trash` moves exactly the folder its arguments name. A
/// name that only redirects to the crane is refused (`asset.unknown`), and the crane stays; a
/// name already in the trash answers `asset.trashed`; nothing is asked either time.
#[tokio::test] // spec: CORE-157, ARCH-125, APP-115
async fn a_redirected_name_is_refused_and_its_target_stays() {
    let (fx, _fake) = AssetFx::faked(|ws| {
        write_file(
            &ws_file(ws, "registry/redirects.yaml"),
            format!(
                "redirects:\n  - from: {P4}/chr_old_crane\n    to: {P4}/{CRANE}\n    moved_utc: 2026-11-01T10:00:00Z\n    by: cli\n"
            )
            .as_bytes(),
        );
    })
    .await;
    let e = err_of(
        &fx.call_as(
            &fx.app(),
            "asset.trash",
            trash_args("chr_old_crane"),
            Some(&tkey()),
        )
        .await,
        "a redirected name",
    );
    assert_eq!(e.code, "asset.unknown", "{e:?}");
    assert_eq!(
        e.data.as_ref().unwrap()["redirects_to"],
        format!("{P4}/{CRANE}")
    );
    assert!(
        asset_path(&fx.ws, CRANE).is_dir(),
        "the redirect's target stays"
    );
    assert_eq!(fx.pending_count().await, 0);

    let entry_id = fx.trash_ok(CRANE).await;
    for name in [CRANE, "chr_old_crane"] {
        let e = err_of(
            &fx.call_as(&fx.app(), "asset.trash", trash_args(name), Some(&tkey()))
                .await,
            "a trashed name",
        );
        if name == CRANE {
            assert_eq!(e.code, "asset.trashed", "{e:?}");
            assert_eq!(e.data.as_ref().unwrap()["entry_id"], entry_id.as_str());
        } else {
            // The redirect's end is in the trash: as the resolver answers.
            assert_eq!(e.code, "asset.trashed", "{e:?}");
        }
    }
    assert_eq!(fx.trash_entries().await.len(), 1);
    fx.shutdown().await;
}

/// CORE-157, ARCH-125: while the crane is in the trash, refs to it resolve to `asset.trashed`
/// naming the entry, its dependent lists that input as missing with that error, and the crane
/// is gone from the index.
#[tokio::test] // spec: CORE-157, ARCH-125
async fn refs_and_dependents_see_asset_trashed() {
    let (fx, _fake) = AssetFx::faked(lamp_uses_crane).await;
    let entry_id = fx.trash_ok(CRANE).await;
    let e = fx
        .err("ref.resolve", json!({ "ref": fx.sealed(CRANE) }))
        .await;
    assert_eq!(e.code, "asset.trashed", "{e:?}");
    assert_eq!(e.data.unwrap()["entry_id"], entry_id.as_str());
    let lamp = fx.ok("asset.get", trash_args(LAMP)).await;
    let uses = lamp["uses"].as_array().unwrap();
    let input = uses
        .iter()
        .find(|u| u["ref"] == format!("{CRANE}@1.0.0"))
        .unwrap_or_else(|| panic!("{lamp}"));
    assert!(input.get("resolved").is_none_or(Value::is_null), "{input}");
    assert_eq!(input["error"]["code"], "asset.trashed", "{input}");
    let listed = fx.ok("assets.query", json!({ "project": P4 })).await;
    assert!(
        listed["assets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["asset_id"] != CRANE),
        "{listed}"
    );
    fx.shutdown().await;
}

/// P4b-R39, CORE-037: `trash.restore {to}` of a whole asset restores it under the free id,
/// renames its manifest and sets its `asset_id`, re-applies read-only, and appends one redirect
/// from the old id (`by` the caller), so old refs reach it.
#[tokio::test] // spec: CORE-037, ARCH-125, CORE-157
async fn restore_to_a_free_id_renames_the_manifest_and_redirects_the_old_id() {
    let (fx, _fake) = AssetFx::faked(|_| {}).await;
    let before = tree(&asset_path(&fx.ws, CRANE));
    let entry_id = fx.trash_ok(CRANE).await;
    let restored = fx
        .ok(
            "trash.restore",
            json!({"entry_id": entry_id, "to": ORIGAMI}),
        )
        .await;
    assert_eq!(restored["path"], format!("projects/{P4}/assets/{ORIGAMI}"));
    let origami = asset_path(&fx.ws, ORIGAMI);
    assert!(!asset_path(&fx.ws, CRANE).exists());
    assert!(!origami.join(format!("{CRANE}.manifest.json")).exists());
    let manifest = read_json_file(&origami.join(format!("{ORIGAMI}.manifest.json")));
    assert_eq!(manifest["asset_id"], ORIGAMI);
    assert_eq!(manifest["project"], P4);
    let after = tree(&origami);
    for (rel, sha) in before.iter().filter(|(rel, _)| rel.starts_with("v1.0.0/")) {
        assert!(
            after.contains(&(rel.clone(), sha.clone())),
            "{rel}: {after:?}"
        );
        assert!(read_only(&ws_file(&origami, rel)), "{rel} is sealed again");
    }
    let redirects = fx.redirects().expect("registry/redirects.yaml");
    assert_eq!(redirects.matches("- from:").count(), 1, "{redirects}");
    assert!(
        redirects.contains(&format!("from: {P4}/{CRANE}")),
        "{redirects}"
    );
    assert!(
        redirects.contains(&format!("to: {P4}/{ORIGAMI}")),
        "{redirects}"
    );
    assert!(redirects.contains("by: cli"), "{redirects}");

    let r = fx
        .ok("ref.resolve", json!({ "ref": fx.sealed(CRANE) }))
        .await;
    assert_eq!(r["asset_id"], ORIGAMI, "{r}");
    assert_eq!(r["redirected_from"], json!([format!("{P4}/{CRANE}")]));
    let detail = fx.ok("asset.get", trash_args(ORIGAMI)).await;
    assert!(detail["repair"].is_null(), "not a moved folder: {detail}");
    assert!(fx.trash_entries().await.is_empty());
    let needs = fx.status_needs_repair().await;
    assert!(
        needs
            .iter()
            .all(|n| !n.contains(ORIGAMI) && !n.contains("redirects")),
        "{needs:?}"
    );
    fx.shutdown().await;
}

/// CORE-037: without `to`, a taken original id is `trash.restore_conflict`; nothing is
/// replaced and the entry stays.
#[tokio::test] // spec: CORE-037, ARCH-058
async fn restore_without_to_onto_a_taken_id_conflicts() {
    let (fx, _fake) = AssetFx::faked(|_| {}).await;
    let entry_id = fx.trash_ok(CRANE).await;
    let source = fx.env.root().join("sources").join("new_crane.png");
    write_file(&source, b"a new crane");
    fx.ok(
        "asset.import",
        json!({"project": P4, "asset_id": CRANE, "type": "image",
               "files": [source.to_string_lossy()], "mode": "copy"}),
    )
    .await;
    let new_crane = tree(&asset_path(&fx.ws, CRANE));
    let e = fx
        .err("trash.restore", json!({ "entry_id": entry_id }))
        .await;
    assert_eq!(e.code, "trash.restore_conflict", "{e:?}");
    assert_eq!(
        tree(&asset_path(&fx.ws, CRANE)),
        new_crane,
        "nothing replaced"
    );
    assert_eq!(fx.trash_entries().await.len(), 1, "the entry stays");
    fx.shutdown().await;
}

/// CORE-037: `to` naming a taken asset id is `trash.restore_conflict`, and nothing is replaced
/// or written (no redirect); a `to` that isn't an asset id is `params.invalid`. The entry stays.
/// `to` naming the asset's own id is a plain restore; a `to` for a path inside an asset folder
/// is `params.invalid`.
#[tokio::test] // spec: CORE-037, ARCH-125
async fn restore_to_a_taken_name_conflicts_and_replaces_nothing() {
    let (fx, _fake) = AssetFx::faked(|_| {}).await;
    let lamp = tree(&asset_path(&fx.ws, LAMP));
    let entry_id = fx.trash_ok(CRANE).await;
    let e = fx
        .err("trash.restore", json!({"entry_id": entry_id, "to": LAMP}))
        .await;
    assert_eq!(e.code, "trash.restore_conflict", "{e:?}");
    assert_eq!(tree(&asset_path(&fx.ws, LAMP)), lamp, "nothing replaced");
    assert!(fx.redirects().is_none(), "no redirect written");
    for bad in ["Chr_Upper", "a/b", "..", ""] {
        let e = fx
            .err("trash.restore", json!({"entry_id": entry_id, "to": bad}))
            .await;
        assert_eq!(e.code, "params.invalid", "{bad}: {e:?}");
    }
    assert_eq!(fx.trash_entries().await.len(), 1, "the entry stays");
    assert!(fx.redirects().is_none());
    // `to` naming its own id is a plain restore: no redirect to itself.
    let back = fx
        .ok("trash.restore", json!({"entry_id": entry_id, "to": CRANE}))
        .await;
    assert_eq!(back["path"], format!("projects/{P4}/assets/{CRANE}"));
    assert!(fx.redirects().is_none(), "no redirect");

    // Something inside an asset folder is restored only under its own name.
    let inside = format!("projects/{P4}/assets/{LAMP}/prompts/signal_lamp.txt");
    let trashed = fx.ok("test.trash", json!({ "path": inside })).await;
    let entry_id = trashed["entry_id"].as_str().unwrap();
    let e = fx
        .err(
            "trash.restore",
            json!({"entry_id": entry_id, "to": "other.txt"}),
        )
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    assert!(
        !ws_file(
            &fx.ws,
            &format!("projects/{P4}/assets/{LAMP}/prompts/other.txt")
        )
        .exists()
    );
    fx.ok("trash.restore", json!({ "entry_id": entry_id }))
        .await;
    assert!(ws_file(&fx.ws, &inside).exists());
    fx.shutdown().await;
}

/// Ruling 8 (Task 8 audit A-L1): a `trash.restore {to}` round trip makes a redirect loop (A →
/// B, then B → A); once the asset is trashed again, both names answer `asset.trashed` with the
/// newest entry instead of `cycle`, and neither needs repair. A live name resolves to itself.
#[tokio::test] // spec: ARCH-125, CORE-037, CORE-157
async fn a_restore_round_trip_then_a_trash_answers_asset_trashed() {
    let (fx, _fake) = AssetFx::faked(|_| {}).await;
    let first = fx.trash_ok(CRANE).await;
    fx.ok("trash.restore", json!({"entry_id": first, "to": ORIGAMI}))
        .await;
    let second = fx.trash_ok(ORIGAMI).await;
    fx.ok("trash.restore", json!({"entry_id": second, "to": CRANE}))
        .await;
    // Live again under its first name: it resolves to itself, through no redirect.
    let r = fx
        .ok("ref.resolve", json!({ "ref": fx.sealed(CRANE) }))
        .await;
    assert_eq!(r["asset_id"], CRANE, "{r}");
    assert_eq!(r["redirected_from"], json!([]), "{r}");
    let r = fx
        .ok("ref.resolve", json!({ "ref": fx.sealed(ORIGAMI) }))
        .await;
    assert_eq!(r["asset_id"], CRANE, "{r}");

    let third = fx.trash_ok(CRANE).await;
    for name in [CRANE, ORIGAMI] {
        let e = fx
            .err("ref.resolve", json!({ "ref": fx.sealed(name) }))
            .await;
        assert_eq!(e.code, "asset.trashed", "{name}: {e:?}");
        assert_eq!(e.data.unwrap()["entry_id"], third.as_str(), "{name}");
    }
    let needs = fx.status_needs_repair().await;
    assert!(
        needs
            .iter()
            .all(|n| !n.starts_with("registry/redirects.yaml")),
        "{needs:?}"
    );
    // Restored once more: live, itself.
    fx.ok("trash.restore", json!({ "entry_id": third })).await;
    let r = fx
        .ok("ref.resolve", json!({ "ref": fx.sealed(CRANE) }))
        .await;
    assert_eq!(
        (r["asset_id"].clone(), r["redirected_from"].clone()),
        (json!(CRANE), json!([]))
    );
    fx.shutdown().await;
}

/// Ruling 7 (P4b-R34): a `trash.restore {to}` cut short after the folder moved and the redirect
/// was appended (a crash before the manifest edit) leaves a `moved` asset, and
/// `asset.accept_move` finishes it without a second redirect.
#[tokio::test] // spec: CORE-037, CORE-154, ARCH-125
async fn a_restore_to_cut_short_is_finished_by_accept_move_without_a_second_redirect() {
    let (fx, _fake) = AssetFx::faked(|ws| {
        // The state the crash leaves: the folder under its new id, the manifest as it was, and
        // the redirect step (1) appended.
        explorer_rename(&asset_path(ws, CRANE), &asset_path(ws, ORIGAMI));
        write_file(
            &ws_file(ws, "registry/redirects.yaml"),
            format!(
                "redirects:\n  - from: {P4}/{CRANE}\n    to: {P4}/{ORIGAMI}\n    moved_utc: 2027-01-15T08:00:00Z\n    by: user\n"
            )
            .as_bytes(),
        );
    })
    .await;
    let detail = fx.ok("asset.get", trash_args(ORIGAMI)).await;
    assert_eq!(detail["repair"]["kind"], "moved", "{detail}");
    let done = fx.ok("asset.accept_move", trash_args(ORIGAMI)).await;
    assert_eq!(done["redirect"]["from"], format!("{P4}/{CRANE}"), "{done}");
    let redirects = fx.redirects().unwrap();
    assert_eq!(
        redirects.matches("- from:").count(),
        1,
        "no second redirect: {redirects}"
    );
    let origami = asset_path(&fx.ws, ORIGAMI);
    assert_eq!(
        read_json_file(&origami.join(format!("{ORIGAMI}.manifest.json")))["asset_id"],
        ORIGAMI
    );
    fx.shutdown().await;
}

/// Ruling 10 (Task 8): the accept-move refusal for a name in the trash says to restore that
/// entry or empty it. Restoring it puts the older asset back at its name, and the moved folder
/// then shows as a copy.
#[tokio::test] // spec: CORE-154, CORE-157, ARCH-125
async fn the_accept_move_refusal_advice_holds_after_asset_trash() {
    let (fx, _fake) = AssetFx::faked(|ws| {
        // A copy of the crane (same creation UUID) under another folder name.
        explorer_copy(&asset_path(ws, CRANE), &asset_path(ws, ORIGAMI));
        explorer_rename(
            &asset_path(ws, ORIGAMI).join(format!("{CRANE}.manifest.json")),
            &asset_path(ws, ORIGAMI).join(format!("{ORIGAMI}.manifest.json")),
        );
    })
    .await;
    let entry_id = fx.trash_ok(CRANE).await;
    fx.reconcile().await;
    assert_eq!(
        fx.ok("asset.get", trash_args(ORIGAMI)).await["repair"]["kind"],
        "moved"
    );
    let e = fx.err("asset.accept_move", trash_args(ORIGAMI)).await;
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message
            .contains(&format!("empty trash entry {entry_id} first"))
            && e.message
                .contains("Restoring that entry with trash.restore"),
        "{}",
        e.message
    );
    fx.ok("trash.restore", json!({ "entry_id": entry_id }))
        .await;
    fx.reconcile().await;
    let r = fx
        .ok("ref.resolve", json!({ "ref": fx.sealed(CRANE) }))
        .await;
    assert_eq!(
        (r["asset_id"].clone(), r["redirected_from"].clone()),
        (json!(CRANE), json!([]))
    );
    let copy = fx.ok("asset.get", trash_args(ORIGAMI)).await;
    assert_eq!(copy["repair"]["kind"], "copy", "{copy}");
    assert_eq!(copy["repair"]["other"], format!("{P4}/{CRANE}"), "{copy}");
    fx.shutdown().await;
}

/// CORE-157 with ARCH-074: a handle held open inside the asset folder for longer than the
/// retry budget makes the move `store.busy`, and the folder stays whole: no file moved, no
/// entry left.
#[tokio::test(start_paused = true)] // spec: CORE-157, ARCH-074, ARCH-058
async fn a_held_handle_leaves_the_folder_whole() {
    let ws = workspace();
    let store = ws.store();
    sealed_version(&ws, &store).await;
    write_file(
        &ws.file(&format!("{ASSET}/work/held.txt")),
        b"open in an editor",
    );
    let before = files_of(&ws.file(ASSET));
    let holder = HandleHolder::hold(
        &ws.file(&format!("{ASSET}/work/held.txt")),
        Share::ReadWrite,
    );
    let refused = store
        .trash(&p(ASSET), ASSET_TRASH_REASON)
        .await
        .unwrap_err();
    assert!(matches!(refused, StoreError::Busy(_)), "{refused:?}");
    holder.release();
    assert_eq!(files_of(&ws.file(ASSET)), before, "the folder is whole");
    assert!(
        store.trash_list().await.unwrap().is_empty(),
        "no entry left"
    );
    assert!(read_only(&ws.file(&format!("{ASSET}/v1.0.0/a.png"))));
}

/// CORE-157, ARCH-058: a whole asset's entry is purged after 30 days like any entry, its
/// read-only sealed files included (the purge clears read-only first); not a day before.
#[tokio::test(start_paused = true)] // spec: CORE-157, ARCH-058, CORE-110
async fn purge_clears_read_only_after_30_days() {
    let ws = workspace();
    let store = ws.store();
    sealed_version(&ws, &store).await;
    let record = store.trash(&p(ASSET), ASSET_TRASH_REASON).await.unwrap();
    assert_eq!(record.reason, "asset.trash");
    let sealed = entry_dir(&ws, &record).join("payload/crane/v1.0.0/a.png");
    assert!(read_only(&sealed));
    let report = store.maintenance(ws.after_days(29)).await.unwrap();
    assert!(report.purged.is_empty(), "{report:?}");
    assert!(sealed.exists());
    let report = store.maintenance(ws.after_days(1)).await.unwrap();
    assert_eq!(report.purged, std::slice::from_ref(&record.entry_id));
    assert!(!sealed.exists());
    assert!(!entry_dir(&ws, &record).exists());
}

// ---------------------------------------------------------------------------------------------
// The whole-asset trash over the pipe of a core: the server's in-use check, an app's question
// ending with the app, a crash after the move, and a resource restored under a new id.
// ---------------------------------------------------------------------------------------------

/// An in-process core on `env` that trusts the test app (`lil-test-app.exe`), with `failpoints`.
fn app_core(env: &TestEnv, failpoints: Arc<FailPoints>) -> lil_core::CoreThread {
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        failpoints,
        trusted: TrustedDirs {
            app_dirs: vec![ensure_built().dir.clone()],
            test_app: true,
            ..base.trusted.clone()
        },
        ..base
    })
}

/// A copy of the fixture workspace registered as `env`'s default, the crane unlocked.
fn served_workspace(env: &TestEnv, name: &str) -> PathBuf {
    let ws = env.copy_fixture_workspace(name);
    crane_unlocked(&ws);
    env.register_workspace(&ws, true);
    ws
}

/// A `cli` connection whose hello names `ws`.
fn cli_pipe(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

/// The app's `asset.trash` of the crane, approved by the app when it asks: the last outcome.
fn app_trash(app: &mut TestApp) -> Value {
    let k = tkey();
    let first = app.tool("asset.trash", trash_args(CRANE), Some(&k));
    if first["status"] != "pending_confirmation" {
        return first;
    }
    let id = first["confirmation_id"].as_str().unwrap().to_string();
    let answered = app.call(
        "app/confirmation.answer",
        json!({"confirmation_id": id, "answer": "approve"}),
    );
    assert_eq!(answered["result"]["status"], "ok", "{answered}");
    app.tool("asset.trash", trash_args(CRANE), Some(&k))
}

fn asset_trash_entries(pipe: &mut RawPipe) -> Vec<Value> {
    let list = pipe.tool_ok("trash.list", json!({"scope": "workspace"}), None);
    list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["reason"] == "asset.trash")
        .cloned()
        .collect()
}

/// Waits (failure deadline only) until `doc.list_open` shows no hold.
fn wait_no_holds(pipe: &mut RawPipe) {
    let until = std::time::Instant::now() + DEADLINE;
    loop {
        let open = pipe.tool_ok("doc.list_open", json!({}), None);
        if open["documents"].as_array().is_some_and(Vec::is_empty) {
            return;
        }
        assert!(std::time::Instant::now() < until, "holds stayed: {open}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A fail point hook that, the first time it runs, says so and then waits until released.
#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Gate {
    fn enter_and_wait(&self) {
        let mut s = self.state.lock().unwrap();
        if s.0 {
            return;
        }
        s.0 = true;
        self.changed.notify_all();
        let until = std::time::Instant::now() + DEADLINE;
        while !s.1 && std::time::Instant::now() < until {
            s = self
                .changed
                .wait_timeout(s, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }

    fn wait_entered(&self) {
        let mut s = self.state.lock().unwrap();
        let until = std::time::Instant::now() + DEADLINE;
        while !s.0 {
            assert!(
                std::time::Instant::now() < until,
                "the gate was never entered"
            );
            s = self
                .changed
                .wait_timeout(s, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}

/// P4b-R24, P4b-R32 (ruling 5): the server's in-use check refuses `asset.trash` with
/// `asset.in_use`, nothing moved, while an unfinished job names the asset, while a sign-off
/// holds its version lock, and while a document hold lies inside its folder, by a relative and
/// by an absolute path; once all of that is done, the trash goes through.
#[test] // spec: CORE-157
fn trash_is_refused_while_a_job_or_hold_uses_the_asset() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = served_workspace(&env, "in-use");
    let fp = FailPoints::none();
    let gate = Arc::new(Gate::default());
    let hook = gate.clone();
    fp.on(
        "store.after_seal",
        Box::new(move || {
            hook.enter_and_wait();
            Ok(())
        }),
    );
    let _core = app_core(&env, fp);
    let mut pipe = cli_pipe(&env, &ws);
    let mut app = TestApp::start(&env, Some(&ws), &[]);
    let crane = asset_path(&ws, CRANE);
    let refused = |app: &mut TestApp, pipe: &mut RawPipe, what: &str| {
        let out = app_trash(app);
        assert_eq!(out["error"]["code"], "asset.in_use", "{what}: {out}");
        assert_eq!(out["error"]["data"]["by"], what, "{out}");
        assert!(crane.is_dir(), "{what}: nothing moved");
        assert!(asset_trash_entries(pipe).is_empty(), "{what}: no entry");
    };

    // An unfinished job of the asset.
    let job = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "gated": true, "asset": {"project": P4, "asset_id": CRANE}}),
        Some(&tkey()),
    )["job"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    refused(&mut app, &mut pipe, "job");
    pipe.tool_ok("test.job_release", json!({ "job_id": job }), Some(&tkey()));
    let done = pipe.tool_ok("job.wait", json!({"job_id": job, "timeout_s": 30}), None);
    assert_eq!(done["done"], true, "{done}");

    // A sign-off, held between its seal and its manifest edit (the version lock is held).
    let signer = {
        let mut signer_pipe = cli_pipe(&env, &ws);
        std::thread::spawn(move || {
            signer_pipe.tool(
                "asset.signoff",
                json!({"project": P4, "asset_id": CRANE, "run_id": "r_20261003_1400",
                       "files": ["crane_c.png"]}),
                Some(&tkey()),
            )
        })
    };
    gate.wait_entered();
    refused(&mut app, &mut pipe, "signoff");
    gate.release();
    let signed = signer.join().unwrap();
    assert_eq!(signed["status"], "ok", "{signed}");

    // A document hold inside the folder: as given, relative, then absolute.
    let canonical = std::fs::canonicalize(&ws).unwrap();
    let text = canonical.to_string_lossy().into_owned();
    let absolute = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
        .join("projects")
        .join(P4)
        .join("assets")
        .join(CRANE.to_uppercase())
        .join("work")
        .join("sketch.lil");
    for path in [
        format!("projects/{P4}/assets/{CRANE}/work/sketch.lil"),
        absolute.to_string_lossy().into_owned(),
    ] {
        let mut holder = TestApp::start(&env, Some(&ws), &[]);
        let held = holder.tool(
            "test.hold",
            json!({"doc_id": "doc_crane", "path": path}),
            Some(&tkey()),
        );
        assert_eq!(held["status"], "ok", "{held}");
        refused(&mut app, &mut pipe, "hold");
        drop(holder);
        wait_no_holds(&mut pipe);
    }
    // A hold outside the folder is not a use.
    let mut holder = TestApp::start(&env, Some(&ws), &[]);
    let held = holder.tool(
        "test.hold",
        json!({"doc_id": "doc_lamp", "path": format!("projects/{P4}/assets/{CRANE}_2/x.lil")}),
        Some(&tkey()),
    );
    assert_eq!(held["status"], "ok", "{held}");

    let ran = app_trash(&mut app);
    assert_eq!(ran["status"], "ok", "{ran}");
    assert!(!crane.exists());
    assert_eq!(asset_trash_entries(&mut pipe).len(), 1);
}

/// Ruling 6 (Task 10 review M5): an app's `asset.trash` question ends with the app: once its
/// process is gone the confirmation is cancelled, and can't be approved. A retry with its key
/// then answers `confirm.expired` (Appendix F: a cancelled confirmation releases the key), and
/// the next call with that key gets a fresh question. An agent's question outlives its
/// connection, as before.
#[test] // spec: CORE-157, APP-044, ARCH-055
fn an_app_trash_question_ends_with_its_app() {
    let env = TestEnv::new();
    let ws = served_workspace(&env, "app-ends");
    let _core = app_core(&env, FailPoints::none());
    let mut pipe = cli_pipe(&env, &ws);
    let mut other = TestApp::start(&env, Some(&ws), &[]);
    let k = tkey();
    let id = {
        let mut app = TestApp::start(&env, Some(&ws), &[]);
        let first = app.tool("asset.trash", trash_args(CRANE), Some(&k));
        assert_eq!(first["status"], "pending_confirmation", "{first}");
        first["confirmation_id"].as_str().unwrap().to_string()
    };
    let until = std::time::Instant::now() + DEADLINE;
    loop {
        let listed = pipe.call("app/confirmations.list", json!({"state": "pending"}));
        let pending = listed["result"]["confirmations"]
            .as_array()
            .unwrap()
            .clone();
        if pending.iter().all(|c| c["id"] != id.as_str()) {
            break;
        }
        assert!(std::time::Instant::now() < until, "still pending: {listed}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let history = pipe.call("app/confirmations.list", json!({"state": "history"}));
    let row = history["result"]["confirmations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("{history}"));
    assert_eq!(row["state"], "cancelled", "{row}");
    let late = other.call(
        "app/confirmation.answer",
        json!({"confirmation_id": id, "answer": "approve"}),
    );
    assert_ne!(late["result"]["status"], "ok", "not approvable: {late}");
    assert!(asset_path(&ws, CRANE).is_dir());

    // A new session's retry with the key: the cancelled question's end, then a fresh one.
    let retry = other.tool("asset.trash", trash_args(CRANE), Some(&k));
    assert_eq!(retry["error"]["code"], "confirm.expired", "{retry}");
    assert!(
        retry["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cancelled"),
        "{retry}"
    );
    let fresh = other.tool("asset.trash", trash_args(CRANE), Some(&k));
    assert_eq!(fresh["status"], "pending_confirmation", "{fresh}");
    assert_ne!(fresh["confirmation_id"], id.as_str());

    // An agent's question stays when its connection closes.
    let agent_id = {
        let mut agent = cli_pipe(&env, &ws);
        let asked = agent.tool("asset.trash", trash_args(LAMP), Some(&tkey()));
        assert_eq!(asked["status"], "pending_confirmation", "{asked}");
        asked["confirmation_id"].as_str().unwrap().to_string()
    };
    std::thread::sleep(Duration::from_millis(200));
    let listed = pipe.call("app/confirmations.list", json!({"state": "pending"}));
    assert!(
        listed["result"]["confirmations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == agent_id.as_str()),
        "{listed}"
    );
}

/// CORE-037 (and P4b-R32's hook): a resource restored under a new id takes that id inside the
/// file, a targeted edit (every other byte as it was), in the workspace and the personal scope;
/// a taken id conflicts and a bad one is `params.invalid`, each leaving the entry in the trash.
/// Driven from the app (your own `resource.delete` doesn't ask).
#[test] // spec: CORE-037, APP-078
fn a_restored_resource_takes_its_new_id_inside_the_file() {
    let env = TestEnv::new();
    let ws = served_workspace(&env, "resource");
    let _core = app_core(&env, FailPoints::none());
    let mut app = TestApp::start(&env, Some(&ws), &[]);
    let app_ok = |app: &mut TestApp, name: &str, args: Value, write: bool| -> Value {
        let k = tkey();
        let out = app.tool(name, args.clone(), write.then_some(k.as_str()));
        assert_eq!(out["status"], "ok", "{name} {args}: {out}");
        out["structured"].clone()
    };
    for (kind, scope, root) in [
        ("swatch", "workspace", ws.join("library").join("resources")),
        (
            "model_profile",
            "personal",
            env.env.data_dir().join("resources"),
        ),
    ] {
        for id in ["res_dawn", "res_taken"] {
            app_ok(
                &mut app,
                "resource.save",
                json!({"kind": kind, "id": id, "name": "Dawn ünïcödé", "scope": scope,
                       "data": {"id": "not the resource's", "nested": {"id": "x"}}}),
                true,
            );
        }
        let file = root.join(kind).join("res_dawn.json");
        let original = std::fs::read_to_string(&file).unwrap();
        let deleted = app_ok(
            &mut app,
            "resource.delete",
            json!({"kind": kind, "id": "res_dawn", "scope": scope}),
            true,
        );
        let entry_id = deleted["entry_id"].as_str().unwrap().to_string();

        for (to, code) in [
            ("res_taken", "trash.restore_conflict"),
            ("Res Bad", "params.invalid"),
        ] {
            let out = app.tool(
                "trash.restore",
                json!({"entry_id": entry_id, "scope": scope, "to": to}),
                Some(&tkey()),
            );
            assert_eq!(out["error"]["code"], code, "{kind} {to}: {out}");
        }
        let restored = app_ok(
            &mut app,
            "trash.restore",
            json!({"entry_id": entry_id, "scope": scope, "to": "res_dusk"}),
            true,
        );
        let want_path = if scope == "workspace" {
            format!("library/resources/{kind}/res_dusk.json")
        } else {
            format!("{kind}/res_dusk.json")
        };
        assert_eq!(restored["path"], want_path.as_str(), "{restored}");
        let text = std::fs::read_to_string(root.join(kind).join("res_dusk.json")).unwrap();
        assert_eq!(
            text,
            original.replacen("\"id\": \"res_dawn\"", "\"id\": \"res_dusk\"", 1),
            "only the id changed"
        );
        assert!(!file.exists());
        let got = app_ok(
            &mut app,
            "resource.get",
            json!({"kind": kind, "id": "res_dusk", "scope": scope}),
            false,
        );
        assert_eq!(got["resource"]["id"], "res_dusk", "{got}");
        assert_eq!(got["resource"]["data"]["id"], "not the resource's");
        let list = app_ok(&mut app, "trash.list", json!({ "scope": scope }), false);
        assert!(
            list["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["entry_id"] != entry_id.as_str()),
            "the entry is gone: {list}"
        );
    }
}

/// CORE-157, QA-055: a core killed right after the whole asset folder moved (fail point
/// `store.after_asset_trash`, before the index heard of it): after a restart the entry is
/// listed, the asset leaves the index (the maintenance pass at the open settles it, instead of
/// the index keeping it as `missing`), and the entry restores cleanly, sealed files read-only
/// again.
#[test] // spec: CORE-157, QA-055, ARCH-058
fn crash_after_the_move_restores_cleanly() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = served_workspace(&env, "crash");
    let crane = asset_path(&ws, CRANE);
    let before = tree(&crane);
    {
        let spec = lil_testkit::failpoints::spec(&[("store.after_asset_trash", "abort")]);
        let mut core = CoreFixture::start_with_env(&env, &[(lil_testkit::failpoints::ENV, &spec)]);
        let mut app = TestApp::start(&env, Some(&ws), &[]);
        let k = tkey();
        let first = app.tool("asset.trash", trash_args(CRANE), Some(&k));
        let id = first["confirmation_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{first}"))
            .to_string();
        let answered = app.call(
            "app/confirmation.answer",
            json!({"confirmation_id": id, "answer": "approve"}),
        );
        assert_eq!(answered["result"]["status"], "ok", "{answered}");
        // The approved retry: the core dies at the fail point, so no answer comes.
        let retry = std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                app.tool("asset.trash", trash_args(CRANE), Some(&k))
            }));
        });
        assert_eq!(
            core.wait_exit(DEADLINE),
            Some(0xDEAD),
            "the core died at the fail point"
        );
        let _ = retry.join();
    }
    assert!(!crane.exists(), "the folder moved before the crash");

    let _core = CoreFixture::start(&env);
    let mut pipe = cli_pipe(&env, &ws);
    let entries = asset_trash_entries(&mut pipe);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry_id = entries[0]["entry_id"].as_str().unwrap().to_string();
    // The maintenance pass that starts with the open settles it in the index.
    let until = std::time::Instant::now() + DEADLINE;
    loop {
        let listed = pipe.tool_ok("assets.query", json!({ "project": P4 }), None);
        let assets = listed["assets"].as_array().unwrap();
        if assets.iter().all(|a| a["asset_id"] != CRANE) {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "still in the index: {listed}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let resolved = pipe.tool(
        "ref.resolve",
        json!({"ref": format!("lil://{}/{P4}/{CRANE}@1.0.0", env.workspace_id(&ws))}),
        None,
    );
    assert_eq!(resolved["error"]["code"], "asset.trashed", "{resolved}");
    let restored = pipe.tool_ok(
        "trash.restore",
        json!({ "entry_id": entry_id }),
        Some(&tkey()),
    );
    assert_eq!(restored["path"], format!("projects/{P4}/assets/{CRANE}"));
    assert_eq!(tree(&crane), before);
    assert!(read_only(&crane.join("v1.0.0").join("crane_b.png")));
    let detail = pipe.tool_ok("asset.get", trash_args(CRANE), None);
    assert_eq!(detail["approved_version"], "1.0.0", "{detail}");
}
