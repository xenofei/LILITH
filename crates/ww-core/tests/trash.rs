//! Trash and retention (ARCH-058, CORE-026, CORE-032, CORE-037, CORE-110): trash entries
//! with restore records, restores that refuse an occupied path and re-seal version files, the
//! 30-day purge (read-only payloads included), rejected runs moved to trash 90 days after
//! sign-off unless pinned, the CORE-110 hard-delete scan, the trash tools, and maintenance at
//! workspace open and every 24 hours.
//!
//! Every test runs on a `FakeClock` (and tokio's paused clock where the store retries), so the
//! 30- and 90-day purges pass without real sleeps (QA-031). Fixture files are made with
//! `ww_testkit::files::write_file`; the one outside step that clears a read-only bit carries
//! its own allowance.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use ww_core::events::EventBus;
use ww_core::state::{StateDb, StateRegistry, StateScope};
use ww_core::store::{
    FailPoints, SealRequest, Store, StoreError, TRASH_KEEP_DAYS, TrashRecord, WsPath,
};
use ww_core::workspace::{WorkspaceHub, WorkspaceRef};
use ww_core::{CoreConfig, start_in_thread};
use ww_platform::fs::{FileIdentity, Probe, SystemProbe, VolumeInfo};
use ww_platform::{Clock, Env};
use ww_schema::Manifest;
use ww_testkit::files::write_file;
use ww_testkit::{FakeClock, RawPipe, SeqIds, TestEnv, sha256_bytes, utc};

// ---------------------------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------------------------

const DAY: u64 = 86_400;
const ASSET: &str = "projects/sketchbook/assets/crane";
const MANIFEST: &str = "projects/sketchbook/assets/crane/crane.manifest.json";

struct Ws {
    _env: TestEnv,
    root: PathBuf,
    state: Arc<StateDb>,
    clock: Arc<FakeClock>,
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
        _env: env,
        root,
        state,
        clock: FakeClock::new(),
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
    let record = store
        .trash(&p(&format!("{ASSET}/v1.0.0/a.png")), "test")
        .await
        .unwrap();
    let payload = entry_dir(&ws, &record).join("payload").join("a.png");
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
    ww_platform::fs::set_readonly(path, false).unwrap();
}

#[tokio::test(start_paused = true)] // spec: CORE-026
async fn restore_under_a_version_folder_reapplies_read_only() {
    let ws = workspace();
    let store = ws.store();
    sealed_version(&ws, &store).await;
    let sealed = format!("{ASSET}/v1.0.0/a.png");

    let record = store.trash(&p(&sealed), "test").await.unwrap();
    let payload = entry_dir(&ws, &record).join("payload").join("a.png");
    clear_read_only(&payload);
    assert!(!read_only(&payload));
    store.trash_restore(&record.entry_id).await.unwrap();
    assert!(
        read_only(&ws.file(&sealed)),
        "the restored file is writable"
    );

    // The whole version folder too: every file comes back read-only.
    let folder = store
        .trash(&p(&format!("{ASSET}/v1.0.0")), "test")
        .await
        .unwrap();
    let payload = entry_dir(&ws, &folder).join("payload").join("v1.0.0");
    for name in ["a.png", "manifest.snapshot.json"] {
        clear_read_only(&payload.join(name));
    }
    store.trash_restore(&folder.entry_id).await.unwrap();
    for name in ["a.png", "manifest.snapshot.json"] {
        assert!(
            read_only(&ws.file(&format!("{ASSET}/v1.0.0/{name}"))),
            "{name}"
        );
    }

    // And a folder above it: only the files in the version folder are made read-only.
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

    // Elsewhere a restored file keeps the attributes it had.
    write_file(&ws.file("notes/a.txt"), b"a");
    let plain = store.trash(&p("notes/a.txt"), "test").await.unwrap();
    store.trash_restore(&plain.entry_id).await.unwrap();
    assert!(!read_only(&ws.file("notes/a.txt")));
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
            id.attributes |= ww_platform::fs::PLACEHOLDER_ATTRIBUTES;
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
