//! The index and the asset read tools (ARCH-079, ARCH-089, ARCH-096, ARCH-097, CORE-005,
//! CORE-006, CORE-038, CORE-041, CORE-047; APP-025 paging): a disposable `index.db` (WAL,
//! `user_version` 1, rebuilt when damaged), reconciled by size, last write, FileId and
//! ChangeTime where they are stable, cloud-safe, with stable-snapshot hashing, parked reads,
//! `missing` files, manifests parsed first, edges, and `assets.query`/`asset.get`.
//!
//! Fixture files are made with `lil_testkit::files::write_file`; the outside steps (a remove, a
//! junction, a set modification time) carry their own allowance. Time-dependent rules run on a
//! `FakeClock` with fake file times; wall-clock durations here are failure deadlines only
//! (QA-031).
#![allow(
    clippy::result_large_err,
    reason = "the tool calls return the wire ErrorBody, as lil-core's own signatures do"
)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use lil_core::events::EventBus;
use lil_core::index::{
    AssetQuery, FileKind, FileState, Index, ReconcileOrder, Scope, SortDir, SortField,
    decode_cursor, encode_cursor, kind_of,
};
use lil_core::session::Session;
use lil_core::state::{StateDb, StateRegistry, StateScope};
use lil_core::store::{ChangeKind, ChangeSink, Expect, FailPoints, Store, StoreEvent, WsPath};
use lil_core::tools::{CallCtx, Registry, Settings, register_asset_read_tools};
use lil_core::workspace::{Access, OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_platform::fs::{FileIdentity, Probe, SystemProbe, VolumeInfo};
use lil_platform::{Clock, Env};
use lil_protocol::{CallerKind, ErrorBody, codes};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv, sha256_bytes};
use rusqlite::Connection;
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(60);
/// The `FakeClock`'s start, in Unix seconds (2027-01-15T08:00:00Z).
const NOW: i64 = 1_800_000_000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

// ---------------------------------------------------------------------------------------------
// Fixture helpers.
// ---------------------------------------------------------------------------------------------

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

/// Unix seconds as a FILETIME (100 ns intervals since 1601).
fn filetime(unix_secs: i64) -> i64 {
    (unix_secs + 11_644_473_600) * 10_000_000
}

fn canonical(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap();
    let text = canonical.to_string_lossy().into_owned();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn native(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Creates a folder as an outside program would.
#[allow(clippy::disallowed_methods)]
fn make_dir(path: &Path) {
    std::fs::create_dir_all(path)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", path.display()));
}

/// Removes a file as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_remove(path: &Path) {
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("cannot remove {}: {e}", path.display()));
}

/// Moves a file as an outside program (or the store) would.
#[allow(clippy::disallowed_methods)]
fn outside_move(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(from, to).unwrap();
}

/// Sets a file's last write time as a copy tool would.
#[allow(clippy::disallowed_methods)]
fn set_mtime(path: &Path, unix_secs: i64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    let at = UNIX_EPOCH + Duration::from_secs(u64::try_from(unix_secs).unwrap());
    file.set_modified(at).unwrap();
}

/// A junction at `link` to the folder `target` (no privilege needed).
#[allow(clippy::disallowed_methods)]
fn junction(link: &Path, target: &Path) {
    lil_platform::fs::create_junction(link, target)
        .unwrap_or_else(|e| panic!("cannot create a junction at {}: {e}", link.display()));
}

/// A manifest of `project`/`asset` with `extra` keys added (made up; I5 shape).
fn manifest_json(project: &str, asset: &str, extra: Value) -> Vec<u8> {
    let mut m = json!({
        "schema_version": 1,
        "asset_id": asset,
        "project": project,
        "creation_uuid": format!("00000000-0000-4000-8000-{:012x}", asset.len()),
        "type": "image",
        "status": "candidate",
    });
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        m[k] = v;
    }
    (serde_json::to_string_pretty(&m).unwrap() + "\n").into_bytes()
}

/// A plain workspace folder with a local folder for its index, opened with `probe` on a
/// `FakeClock` (the scan start times of the 2 s rule).
struct Fx {
    _env: TestEnv,
    root: PathBuf,
    db: PathBuf,
    clock: Arc<FakeClock>,
}

fn fx() -> Fx {
    let env = TestEnv::with_label("Ünïcödé space");
    // A bare folder: only the files each test writes.
    let root = env.root().join("ws-index");
    make_dir(&root);
    let root = canonical(&root);
    let db = env.root().join("local-ws").join("index.db");
    Fx {
        _env: env,
        root,
        db,
        clock: FakeClock::new(),
    }
}

impl Fx {
    fn file(&self, rel: &str) -> PathBuf {
        native(&self.root, rel)
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        write_file(&self.file(rel), bytes);
    }

    fn open(&self, probe: Arc<dyn Probe>) -> Arc<Index> {
        Arc::new(
            Index::open(&self.db, &self.root, probe)
                .expect("the index opens")
                .with_clock(self.clock.clone()),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// The fake probe.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Volume {
    Real,
    Fs(&'static str),
    Fails,
    /// `volume_info` panics: the reconcile itself fails.
    Panics,
}

/// What the fake probe changes on top of the real one. Files are named by their file name.
#[derive(Debug)]
struct Behaviour {
    volume: Volume,
    /// A new FileId on every identity call, and a new ChangeTime at every [`FakeProbe::next_epoch`].
    churn: bool,
    /// File ids built from the parent folder and the name, as on ReFS.
    refs_ids: bool,
    last_write: HashMap<String, i64>,
    file_ids: HashMap<String, [u8; 16]>,
    placeholders: HashSet<String>,
    /// `open_read` fails with error 32.
    locked: HashSet<String>,
    /// Every identity call reports a later last write.
    unstable: HashSet<String>,
}

impl Default for Behaviour {
    fn default() -> Self {
        Behaviour {
            volume: Volume::Real,
            churn: false,
            refs_ids: false,
            last_write: HashMap::new(),
            file_ids: HashMap::new(),
            placeholders: HashSet::new(),
            locked: HashSet::new(),
            unstable: HashSet::new(),
        }
    }
}

type Hook = Box<dyn Fn(&Path) + Send + Sync>;

#[derive(Default)]
struct FakeProbe {
    b: Mutex<Behaviour>,
    n: AtomicU64,
    epoch: AtomicU64,
    reads: Mutex<HashMap<String, usize>>,
    /// The file names `identity` was asked about, in order.
    order: Mutex<Vec<String>>,
    hook: OnceLock<Hook>,
    /// `volume_info` waits while the gate is closed.
    gated: AtomicBool,
    open: Mutex<bool>,
    opened: Condvar,
    waiting: Mutex<u32>,
    waited: Condvar,
}

impl std::fmt::Debug for FakeProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProbe").finish_non_exhaustive()
    }
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

impl FakeProbe {
    fn new() -> Arc<FakeProbe> {
        Arc::new(FakeProbe::default())
    }

    fn with(f: impl FnOnce(&mut Behaviour)) -> Arc<FakeProbe> {
        let probe = FakeProbe::new();
        probe.set(f);
        probe
    }

    fn set(&self, f: impl FnOnce(&mut Behaviour)) {
        f(&mut self.b.lock().unwrap());
    }

    /// A new ChangeTime for every file (with `churn`), as between two scans.
    fn next_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn reads(&self, name: &str) -> usize {
        self.reads.lock().unwrap().get(name).copied().unwrap_or(0)
    }

    fn gated() -> Arc<FakeProbe> {
        let probe = FakeProbe::new();
        probe.gated.store(true, Ordering::SeqCst);
        probe
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    /// Blocks until a `volume_info` call waits at the gate, or the deadline passes (`false`).
    fn wait_until_gated(&self) -> bool {
        let waiting = self.waiting.lock().unwrap();
        let (waiting, _) = self
            .waited
            .wait_timeout_while(waiting, DEADLINE, |w| *w == 0)
            .unwrap();
        *waiting > 0
    }
}

impl Probe for FakeProbe {
    fn identity(&self, path: &Path) -> std::io::Result<FileIdentity> {
        let name = name_of(path);
        self.order.lock().unwrap().push(name.clone());
        if let Some(hook) = self.hook.get() {
            hook(path);
        }
        let mut id = SystemProbe.identity(path)?;
        let b = self.b.lock().unwrap();
        if let Some(lw) = b.last_write.get(&name) {
            id.last_write = *lw;
        }
        if b.unstable.contains(&name) {
            id.last_write += i64::try_from(self.n.fetch_add(1, Ordering::SeqCst)).unwrap() + 1;
        }
        if b.churn {
            let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
            let mut fid = [0u8; 16];
            fid[..8].copy_from_slice(&n.to_le_bytes());
            fid[15] = 0x5a;
            id.file_id = Some(fid);
            let epoch = self.epoch.load(Ordering::SeqCst);
            id.change_time = Some(filetime(NOW) + i64::try_from(epoch).unwrap() + 1);
        }
        if b.refs_ids {
            let parent = path
                .parent()
                .map(|p| p.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let a = sha256_bytes(parent.as_bytes());
            let c = sha256_bytes(name.as_bytes());
            let mut fid = [0u8; 16];
            fid[..8].copy_from_slice(&a.as_bytes()[..8]);
            fid[8..].copy_from_slice(&c.as_bytes()[..8]);
            id.file_id = Some(fid);
        }
        if let Some(fid) = b.file_ids.get(&name) {
            id.file_id = Some(*fid);
        }
        if b.placeholders.contains(&name) {
            id.attributes |= FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<VolumeInfo> {
        if self.gated.load(Ordering::SeqCst) {
            *self.waiting.lock().unwrap() += 1;
            self.waited.notify_all();
            let open = self.open.lock().unwrap();
            let _open = self
                .opened
                .wait_timeout_while(open, DEADLINE, |o| !*o)
                .unwrap();
        }
        let volume = self.b.lock().unwrap().volume;
        match volume {
            Volume::Real => SystemProbe.volume_info(path),
            Volume::Fs(fs_name) => Ok(VolumeInfo {
                fs_name: fs_name.to_string(),
                remote: false,
                cloud_sync_root: false,
            }),
            Volume::Fails => Err(std::io::Error::other("the volume facts can't be read")),
            Volume::Panics => panic!("a volume_info that fails the reconcile"),
        }
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        let name = name_of(path);
        *self.reads.lock().unwrap().entry(name.clone()).or_default() += 1;
        if self.b.lock().unwrap().locked.contains(&name) {
            return Err(std::io::Error::from_raw_os_error(32));
        }
        SystemProbe.open_read(path)
    }
}

// ---------------------------------------------------------------------------------------------
// Hubs and tool calls.
// ---------------------------------------------------------------------------------------------

fn workspace_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

fn hub(env: &TestEnv, probe: Arc<dyn Probe>) -> Arc<WorkspaceHub> {
    let clock: Arc<dyn Clock> = FakeClock::new();
    WorkspaceHub::new(
        env.env.clone(),
        Arc::new(StateRegistry::open(&env.env, no_fatal()).expect("the suite state.db")),
        Arc::new(EventBus::new(clock.clone())),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        probe,
        lil_core::watch::WatchConfig::default(),
    )
}

fn ctx(open: &Arc<OpenWorkspace>, hub: &Arc<WorkspaceHub>) -> CallCtx {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let jobs = lil_core::jobs::Jobs::new(
        hub.states().clone(),
        hub.clone(),
        events.clone(),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
    );
    CallCtx {
        session: Arc::new(Session {
            id: "s_0000000000000001".into(),
            caller: CallerKind::Cli,
            non_interactive: true,
            workspace: Some(open.ws.clone()),
            capabilities: Vec::new(),
            client_pid: 0,
            profile_id: None,
        }),
        trace_id: "t_index".into(),
        settings: Settings::default(),
        events,
        workspace: Some(open.clone()),
        project: None,
        record: Arc::new(lil_core::pipeline::RecordSlot::new()),
        jobs,
        submitted: Arc::new(lil_core::jobs::SubmittedSlot::new()),
        reservation: None,
        confirmed_via: None,
        idem_key: None,
        call_id: String::new(),
        tool: String::new(),
        arguments: serde_json::Value::Null,
        retry_of: None,
        redispatch: None,
    }
}

/// Runs the read tool `name` as the dispatcher would: arguments checked first.
async fn call(ctx: &CallCtx, name: &str, args: Value) -> Result<Value, ErrorBody> {
    let mut reg = Registry::new();
    register_asset_read_tools(&mut reg);
    let tool = reg
        .get(name)
        .unwrap_or_else(|| panic!("{name} is registered"));
    if let Err(why) = tool.validate(&args) {
        return Err(ErrorBody::new(&codes::PARAMS_INVALID, why, "t_index"));
    }
    tool.run(ctx, args).await.map(|out| out.structured)
}

async fn call_ok(ctx: &CallCtx, name: &str, args: Value) -> Value {
    call(ctx, name, args.clone())
        .await
        .unwrap_or_else(|e| panic!("{name} {args}: {e:?}"))
}

fn ids(page: &Value) -> Vec<String> {
    page["assets"]
        .as_array()
        .unwrap_or_else(|| panic!("no assets list: {page}"))
        .iter()
        .map(|a| {
            format!(
                "{}/{}",
                a["project"].as_str().unwrap(),
                a["asset_id"].as_str().unwrap()
            )
        })
        .collect()
}

fn query() -> AssetQuery {
    AssetQuery {
        project: None,
        status: None,
        asset_type: None,
        text: None,
        sort: (SortField::AssetId, SortDir::Asc),
        limit: 200,
        after: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Open, schema and rebuilds (ARCH-089).
// ---------------------------------------------------------------------------------------------

#[tokio::test] // spec: ARCH-089, CORE-005
async fn fresh_index_is_wal_and_versioned() {
    let fx = fx();
    let index = fx.open(Arc::new(SystemProbe));
    assert!(
        index.needs_rebuild(),
        "a new index.db starts with a cold rebuild"
    );
    index.rebuild().await.unwrap();
    let db = Connection::open(&fx.db).unwrap();
    let mode: String = db
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
    let version: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, lil_core::index::INDEX_SCHEMA_VERSION);
    assert_eq!(version, 1);
    drop(db);
    drop(index);

    // A healthy index.db is kept: the next open reconciles it instead.
    let again = fx.open(Arc::new(SystemProbe));
    assert!(!again.needs_rebuild());
}

/// An index.db with one asset in it, closed.
async fn indexed_once(fx: &Fx) {
    fx.write(
        "projects/p/assets/a/a.manifest.json",
        &manifest_json("p", "a", json!({})),
    );
    let index = fx.open(Arc::new(SystemProbe));
    index.rebuild().await.unwrap();
    assert!(index.asset("p", "a").await.unwrap().is_some());
}

#[tokio::test] // spec: ARCH-089
async fn version_mismatch_or_failed_quick_check_rebuilds() {
    let fx = fx();

    // Another user_version.
    indexed_once(&fx).await;
    Connection::open(&fx.db)
        .unwrap()
        .pragma_update(None, "user_version", 7)
        .unwrap();
    let index = fx.open(Arc::new(SystemProbe));
    assert!(index.needs_rebuild(), "a user_version mismatch rebuilds");
    assert_eq!(index.asset("p", "a").await.unwrap(), None, "created afresh");
    index.rebuild().await.unwrap();
    assert!(index.asset("p", "a").await.unwrap().is_some());
    drop(index);
    let version: i64 = Connection::open(&fx.db)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);

    // A damaged page: quick_check fails (or SQLite refuses the file).
    {
        let db = Connection::open(&fx.db).unwrap();
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
    }
    let mut bytes = std::fs::read(&fx.db).unwrap();
    assert!(bytes.len() > 8192, "{} bytes", bytes.len());
    for b in &mut bytes[4096..8192] {
        *b = 0xAB;
    }
    write_file(&fx.db, &bytes);
    let index = fx.open(Arc::new(SystemProbe));
    assert!(index.needs_rebuild(), "a failed quick_check rebuilds");
    assert_eq!(index.asset("p", "a").await.unwrap(), None);
    drop(index);

    // A file that isn't a database at all.
    write_file(&fx.db, &[0xAB; 4096]);
    let index = fx.open(Arc::new(SystemProbe));
    assert!(index.needs_rebuild());
    index.rebuild().await.unwrap();
    assert!(index.asset("p", "a").await.unwrap().is_some());
}

// ---------------------------------------------------------------------------------------------
// Reconcile (CORE-041).
// ---------------------------------------------------------------------------------------------

#[tokio::test] // spec: CORE-041
async fn reconcile_hashes_only_changed_files() {
    let fx = fx();
    fx.write("notes/a.txt", b"a");
    fx.write("notes/b.txt", b"b");
    fx.write("notes/c.txt", b"c");
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    let first = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(first.hashed, 3, "{first:?}");
    assert_eq!(first.changed.len(), 3, "{first:?}");

    let second = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(second.hashed, 0, "{second:?}");
    assert_eq!(second.unchanged, 3, "{second:?}");
    assert!(second.changed.is_empty());

    fx.write("notes/b.txt", b"bigger b");
    let third = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(third.hashed, 1, "{third:?}");
    assert_eq!(third.changed, [p("notes/b.txt")]);
    let rows = index.files_of_path(&p("notes/b.txt")).await.unwrap();
    assert_eq!(rows.and_then(|r| r.sha256), Some(sha256_bytes(b"bigger b")));
}

#[tokio::test] // spec: CORE-041, ARCH-075
async fn store_writes_feed_the_index_without_rehashing() {
    let fx = fx();
    let state = Arc::new(
        StateDb::open(
            &fx.root.parent().unwrap().join("local-st").join("state.db"),
            StateScope::Workspace,
            no_fatal(),
        )
        .unwrap(),
    );
    let store = Store::new(
        fx.root.clone(),
        state,
        FailPoints::none(),
        fx.clock.clone(),
        Arc::new(SeqIds::new()),
        Arc::new(SystemProbe),
    );
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    index.rebuild().await.unwrap();
    store.set_sink(index.clone());

    let written = store
        .write(&p("notes/new.txt"), b"from the store".to_vec(), Expect::New)
        .await
        .unwrap();
    assert_eq!(
        probe.reads("new.txt"),
        0,
        "the index never re-read the store's file"
    );
    let row = index
        .files_of_path(&p("notes/new.txt"))
        .await
        .unwrap()
        .expect("a row");
    assert_eq!(row.sha256.as_deref(), Some(written.sha256.as_str()));
    assert_eq!(row.identity, Some(written.identity));
    assert_eq!(row.state, FileState::Ok);

    // A manifest written by the store becomes an asset row.
    let manifest = p("projects/p/assets/a/a.manifest.json");
    store
        .write(&manifest, manifest_json("p", "a", json!({})), Expect::New)
        .await
        .unwrap();
    let asset = index.asset("p", "a").await.unwrap().expect("an asset row");
    assert_eq!(asset.manifest, manifest);
    assert_eq!(asset.status.as_deref(), Some("candidate"));

    // Nothing left to hash at the next reconcile.
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.hashed, 0, "{report:?}");
}

#[tokio::test] // spec: CORE-041, ARCH-089
async fn volume_without_stable_ids_ignores_id_churn() {
    let fx = fx();
    fx.write("notes/a.txt", b"a");
    fx.write("notes/b.txt", b"b");
    let old = filetime(NOW - 3600);
    let probe = FakeProbe::with(|b| {
        b.volume = Volume::Fs("FAT32");
        b.churn = true;
        b.last_write.insert("a.txt".into(), old);
        b.last_write.insert("b.txt".into(), old);
    });
    let index = fx.open(probe.clone());
    index.reconcile(Scope::All).await.unwrap();
    fx.clock.advance(Duration::from_secs(60));
    probe.next_epoch();
    let second = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(
        second.hashed, 0,
        "FileId and ChangeTime churn is ignored: {second:?}"
    );
    assert_eq!(second.unchanged, 2);

    // On a stable volume a FileId change alone is a change.
    probe.set(|b| {
        b.volume = Volume::Fs("NTFS");
        b.churn = false;
    });
    index.reconcile(Scope::All).await.unwrap();
    let settled = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(settled.hashed, 0, "{settled:?}");
    probe.set(|b| {
        b.file_ids.insert("a.txt".into(), [7; 16]);
    });
    let moved = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(moved.hashed, 1, "{moved:?}");
}

#[tokio::test] // spec: CORE-041, ARCH-089
async fn recent_mtime_is_hashed_on_volumes_without_stable_ids() {
    let fx = fx();
    fx.write("notes/recent.txt", b"first");
    fx.write("notes/older.txt", b"first");
    let probe = FakeProbe::with(|b| {
        b.volume = Volume::Fs("FAT32");
        // Written 1 s before the scan, and an hour before it.
        b.last_write.insert("recent.txt".into(), filetime(NOW - 1));
        b.last_write
            .insert("older.txt".into(), filetime(NOW - 3600));
    });
    let index = fx.open(probe.clone());
    index.reconcile(Scope::All).await.unwrap();

    // Rewritten with the same size inside the same 2-second FAT tick: size and last write
    // match the row, but the recent one is hashed anyway.
    fx.write("notes/recent.txt", b"other");
    fx.write("notes/older.txt", b"other");
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.hashed, 1, "{report:?}");
    assert_eq!(report.changed, [p("notes/recent.txt")]);
    let recent = index
        .files_of_path(&p("notes/recent.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recent.sha256, Some(sha256_bytes(b"other")));
}

#[tokio::test] // spec: CORE-041, ARCH-079
async fn cloud_placeholder_is_indexed_but_never_read() {
    let fx = fx();
    fx.write("notes/cloud.png", b"only in the cloud");
    fx.write("notes/local.png", b"here");
    let probe = FakeProbe::with(|b| {
        b.placeholders.insert("cloud.png".into());
    });
    let index = fx.open(probe.clone());
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.cloud_only, [p("notes/cloud.png")]);
    index.rebuild().await.unwrap();
    index.verify_pending(100).await.unwrap();
    index.reconcile(Scope::All).await.unwrap();
    assert_eq!(
        probe.reads("cloud.png"),
        0,
        "a placeholder is never opened for reading"
    );
    let row = index
        .files_of_path(&p("notes/cloud.png"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, FileState::CloudOnly);
    assert_eq!(row.size, 17);
}

#[tokio::test] // spec: CORE-041
async fn refs_cross_folder_move_changes_id_without_a_false_change() {
    let fx = fx();
    let from = "projects/p/assets/a/candidates/r_20270115_0800/x.png";
    let to = "projects/p/assets/a/v1.0.0/x.png";
    fx.write(from, b"chosen");
    let probe = FakeProbe::with(|b| {
        b.volume = Volume::Fs("ReFS");
        b.refs_ids = true;
    });
    let index = fx.open(probe.clone());
    index.reconcile(Scope::All).await.unwrap();
    let before = index.files_of_path(&p(from)).await.unwrap().unwrap();

    // The store moves the file and reports the identity it has after the move.
    outside_move(&fx.file(from), &fx.file(to));
    let identity = probe.identity(&fx.file(to)).unwrap();
    assert_ne!(Some(identity.file_id), before.identity.map(|i| i.file_id));
    index
        .applied(&[StoreEvent {
            path: p(to),
            kind: ChangeKind::MovedFrom(p(from)),
            sha256: Some(sha256_bytes(b"chosen")),
            identity: Some(identity),
        }])
        .await;
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.hashed, 0, "{report:?}");
    assert!(report.changed.is_empty(), "{report:?}");
    assert!(index.files_of_path(&p(from)).await.unwrap().is_none());
}

#[tokio::test] // spec: CORE-041, ARCH-089
async fn volume_info_error_counts_as_no_stable_ids() {
    let fx = fx();
    fx.write("notes/a.txt", b"a");
    let probe = FakeProbe::with(|b| {
        b.volume = Volume::Fails;
        b.churn = true;
        b.last_write.insert("a.txt".into(), filetime(NOW - 3600));
    });
    let index = fx.open(probe.clone());
    assert_eq!(index.reconcile(Scope::All).await.unwrap().hashed, 1);
    probe.next_epoch();
    let second = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(second.hashed, 0, "{second:?}");
    fx.write("notes/a.txt", b"longer a");
    let third = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(third.hashed, 1, "a size change still counts: {third:?}");
}

#[tokio::test] // spec: ARCH-079
async fn unstable_read_is_parked_not_hashed() {
    let fx = fx();
    fx.write("notes/busy.txt", b"old");
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    index.reconcile(Scope::All).await.unwrap();

    fx.write("notes/busy.txt", b"new bytes");
    probe.set(|b| {
        b.unstable.insert("busy.txt".into());
    });
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.parked, [p("notes/busy.txt")], "{report:?}");
    assert_eq!(report.hashed, 0, "{report:?}");
    assert!(report.removed.is_empty() && report.missing.is_empty());
    let row = index
        .files_of_path(&p("notes/busy.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.sha256,
        Some(sha256_bytes(b"old")),
        "the old row is kept"
    );

    probe.set(|b| {
        b.unstable.clear();
    });
    let retried = index.retry_parked().await.unwrap();
    assert_eq!(retried.changed, [p("notes/busy.txt")], "{retried:?}");
    assert!(retried.parked.is_empty());
    let row = index
        .files_of_path(&p("notes/busy.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.sha256, Some(sha256_bytes(b"new bytes")));
}

#[tokio::test] // spec: ARCH-079
async fn locked_read_is_parked_not_removed() {
    let fx = fx();
    fx.write("notes/held.txt", b"old");
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    index.reconcile(Scope::All).await.unwrap();
    fx.write("notes/held.txt", b"rewritten");
    probe.set(|b| {
        b.locked.insert("held.txt".into());
    });
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.parked, [p("notes/held.txt")], "{report:?}");
    assert!(report.removed.is_empty(), "{report:?}");
    let row = index
        .files_of_path(&p("notes/held.txt"))
        .await
        .unwrap()
        .expect("never removed");
    assert_eq!(row.sha256, Some(sha256_bytes(b"old")));
    assert_eq!(row.state, FileState::Ok);

    probe.set(|b| {
        b.locked.clear();
    });
    index.retry_parked().await.unwrap();
    let row = index
        .files_of_path(&p("notes/held.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.sha256, Some(sha256_bytes(b"rewritten")));
}

const ASSET: &str = "projects/p/assets/a";

#[tokio::test] // spec: ARCH-079, CORE-038
async fn removed_recorded_files_are_missing_and_recover() {
    let fx = fx();
    let sealed = b"sealed bytes";
    let open_file = b"open run file";
    let manifest = manifest_json(
        "p",
        "a",
        json!({
            "hashes": {"v1.0.0/s.png": format!("sha256:{}", sha256_bytes(sealed))},
            "candidate_runs": [
                {"run_id": "r_20270101_0800", "status": "selected", "target_version": "1.0.0",
                 "files": [{"path": "candidates/r_20270101_0800/chosen.png", "role": "candidate",
                            "sha256": sha256_bytes(b"chosen")}]},
                {"run_id": "r_20270102_0800", "status": "rejected", "target_version": "1.0.0",
                 "files": [{"path": "candidates/r_20270102_0800/no.png", "role": "candidate",
                            "sha256": sha256_bytes(b"no")}]},
                {"run_id": "r_20270103_0800", "status": "open", "target_version": "1.1.0",
                 "files": [{"path": "candidates/r_20270103_0800/c.png", "role": "candidate",
                            "sha256": sha256_bytes(open_file)}]},
            ],
        }),
    );
    let paths = [
        format!("{ASSET}/a.manifest.json"),
        format!("{ASSET}/v1.0.0/s.png"),
        format!("{ASSET}/candidates/r_20270103_0800/c.png"),
        format!("{ASSET}/candidates/r_20270101_0800/chosen.png"),
        format!("{ASSET}/candidates/r_20270102_0800/no.png"),
        format!("{ASSET}/work/scratch.txt"),
    ];
    fx.write(&paths[0], &manifest);
    fx.write(&paths[1], sealed);
    fx.write(&paths[2], open_file);
    fx.write(&paths[3], b"chosen");
    fx.write(&paths[4], b"no");
    fx.write(&paths[5], b"scratch");
    let index = fx.open(Arc::new(SystemProbe));
    index.reconcile(Scope::All).await.unwrap();
    for path in &paths {
        outside_remove(&fx.file(path));
    }

    let report = index.reconcile(Scope::All).await.unwrap();
    let mut missing: Vec<&str> = report.missing.iter().map(WsPath::as_str).collect();
    missing.sort_unstable();
    let mut want_missing = vec![paths[0].as_str(), paths[1].as_str(), paths[2].as_str()];
    want_missing.sort_unstable();
    assert_eq!(missing, want_missing, "{report:?}");
    let mut removed: Vec<&str> = report.removed.iter().map(WsPath::as_str).collect();
    removed.sort_unstable();
    let mut want_removed = vec![paths[3].as_str(), paths[4].as_str(), paths[5].as_str()];
    want_removed.sort_unstable();
    assert_eq!(removed, want_removed, "{report:?}");

    let s = index.files_of_path(&p(&paths[1])).await.unwrap().unwrap();
    assert_eq!(s.state, FileState::Missing);
    assert_eq!(
        s.sha256,
        Some(sha256_bytes(sealed)),
        "the last SHA-256 is kept"
    );
    for gone in &paths[3..] {
        assert!(
            index.files_of_path(&p(gone)).await.unwrap().is_none(),
            "{gone}"
        );
    }
    let asset = index.asset("p", "a").await.unwrap();
    assert!(asset.is_some(), "the asset row stays with its last values");
    assert_eq!(asset.unwrap().status.as_deref(), Some("candidate"));

    // The sealed file comes back with the same bytes: `ok` again.
    fx.write(&paths[1], sealed);
    index.reconcile(Scope::All).await.unwrap();
    let s = index.files_of_path(&p(&paths[1])).await.unwrap().unwrap();
    assert_eq!(s.state, FileState::Ok);
    let report = index.reconcile(Scope::All).await.unwrap();
    assert!(
        report.missing.is_empty(),
        "missing is reported once: {report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-097, CORE-005, ARCH-028
async fn read_only_workspace_uses_an_in_memory_index_and_leaves_index_db_alone() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.copy_fixture_workspace("ro"));
    let index_db = env.workspace_local_dir(&ws.path).join("index.db");
    let wal = index_db.with_file_name("index.db-wal");

    let rw_hub = hub(&env, Arc::new(SystemProbe));
    let rw = rw_hub.open(&ws).await.unwrap();
    assert_eq!(rw.access, Access::ReadWrite);
    rw.index.ready().await;
    assert!(rw.index.verify_pending(10_000).await.unwrap().is_empty());
    {
        let db = Connection::open(&index_db).unwrap();
        db.pragma_update(None, "user_version", 99).unwrap();
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
    }
    let snapshot = || {
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        (
            std::fs::read(&index_db).unwrap(),
            std::fs::read(&wal).ok(),
            mtime(&index_db),
            mtime(&wal),
        )
    };
    let before = snapshot();

    let ro_hub = hub(&env, Arc::new(SystemProbe));
    let ro = ro_hub.open(&ws).await.unwrap();
    assert_eq!(ro.access, Access::ReadOnly);
    let page = call_ok(&ctx(&ro, &ro_hub), "assets.query", json!({})).await;
    assert_eq!(page["total"], 2, "{page}");
    assert_eq!(
        ids(&page),
        ["sketchbook/chr_paper_crane", "sketchbook/prop_signal_lamp"]
    );
    ro.index.verify_pending(10_000).await.unwrap();
    assert!(
        before == snapshot(),
        "the read-only core changed index.db or its -wal"
    );

    drop(ro);
    ro_hub.close_all().await;
    drop(rw);
    rw_hub.close_all().await;
}

#[tokio::test] // spec: CORE-041, ARCH-079
async fn cold_rebuild_reads_hashes_with_or_without_prefix() {
    let fx = fx();
    let (x, y) = (b"x bytes".as_slice(), b"y bytes".as_slice());
    fx.write(&format!("{ASSET}/v1.0.0/x.png"), x);
    fx.write(&format!("{ASSET}/v1.0.0/y.png"), y);
    fx.write(
        &format!("{ASSET}/a.manifest.json"),
        &manifest_json(
            "p",
            "a",
            json!({"hashes": {
                "v1.0.0/x.png": format!("sha256:{}", sha256_bytes(x)),
                "v1.0.0/y.png": sha256_bytes(y),
            }}),
        ),
    );
    let index = fx.open(Arc::new(SystemProbe));
    index.rebuild().await.unwrap();
    assert!(index.verify_pending(100).await.unwrap().is_empty());
}

#[tokio::test] // spec: CORE-047, ARCH-079
async fn manifest_disagreeing_with_its_folder_is_needs_repair() {
    let env = TestEnv::new();
    let ws = canonical(&env.copy_fixture_workspace("copy"));
    let assets = ws.join("projects").join("sketchbook").join("assets");
    let original = assets
        .join("chr_paper_crane")
        .join("chr_paper_crane.manifest.json");
    let copy = assets
        .join("chr_paper_crane_copy")
        .join("chr_paper_crane.manifest.json");
    let bytes = std::fs::read(&original).unwrap();
    write_file(&copy, &bytes);
    let index = Arc::new(
        Index::open(
            &env.root().join("local").join("index.db"),
            &ws,
            Arc::new(SystemProbe),
        )
        .unwrap(),
    );
    let report = index.rebuild().await.unwrap();
    assert_eq!(
        report.needs_repair,
        [p(
            "projects/sketchbook/assets/chr_paper_crane_copy/chr_paper_crane.manifest.json"
        )]
    );
    let first = index
        .asset("sketchbook", "chr_paper_crane")
        .await
        .unwrap()
        .unwrap();
    let second = index
        .asset("sketchbook", "chr_paper_crane_copy")
        .await
        .unwrap()
        .unwrap();
    assert!(!first.needs_repair);
    assert!(second.needs_repair);
    let uuid = Some("6f1c2a3e-8b4d-4c5e-9a7b-1d2e3f405162".to_string());
    assert_eq!(first.creation_uuid, uuid);
    assert_eq!(second.creation_uuid, uuid);
    assert_eq!(std::fs::read(&original).unwrap(), bytes, "never rewritten");
    assert_eq!(std::fs::read(&copy).unwrap(), bytes, "never rewritten");
}

#[tokio::test] // spec: CORE-041
async fn reconcile_never_follows_a_junction() {
    let fx = fx();
    fx.write("notes/a.txt", b"a");
    let outside = fx.root.parent().unwrap().join("outside");
    write_file(&outside.join("secret.txt"), b"outside the workspace");
    write_file(&outside.join("deeper").join("x.txt"), b"x");
    junction(&fx.file("notes/link"), &outside);
    let index = fx.open(Arc::new(SystemProbe));
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.changed, [p("notes/a.txt")], "{report:?}");
    let report = index
        .reconcile(Scope::Paths(vec![p("notes/link")]))
        .await
        .unwrap();
    assert!(report.changed.is_empty(), "{report:?}");
    assert!(
        index
            .files_of_path(&p("notes/link/secret.txt"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-041, ARCH-079
async fn manifests_first_reconcile_judges_files_against_the_new_manifest() {
    let fx = fx();
    let run = "candidates/r_20270115_0800";
    let candidate = format!("{ASSET}/{run}/c.png");
    let with_hash = |bytes: &[u8]| {
        manifest_json(
            "p",
            "a",
            json!({"candidate_runs": [{"run_id": "r_20270115_0800", "status": "open",
                "target_version": "1.0.0",
                "files": [{"path": format!("{run}/c.png"), "role": "candidate",
                           "sha256": sha256_bytes(bytes)}]}]}),
        )
    };
    fx.write(&candidate, b"first");
    fx.write(&format!("{ASSET}/a.manifest.json"), &with_hash(b"first"));
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    index.reconcile(Scope::All).await.unwrap();
    assert_eq!(
        index.recorded_hash(&p(&candidate)).await.unwrap(),
        Some(sha256_bytes(b"first"))
    );

    // The candidate and its manifest change together. When the candidate is looked at, the
    // index already records the new manifest's hash for it.
    let seen = Arc::new(Mutex::new(None));
    // Weak: the index holds the probe, which holds this hook, so a strong reference would be a
    // cycle that keeps index.db open after the test.
    let (hook_index, hook_seen, target) =
        (Arc::downgrade(&index), seen.clone(), fx.file(&candidate));
    let handle = tokio::runtime::Handle::current();
    let installed = probe.hook.set(Box::new(move |path: &Path| {
        if path == target {
            let Some(hook_index) = hook_index.upgrade() else {
                return;
            };
            let recorded = handle
                .block_on(hook_index.recorded_hash(&WsPath::parse(&candidate).unwrap()))
                .unwrap();
            *hook_seen.lock().unwrap() = Some(recorded);
        }
    }));
    assert!(installed.is_ok());
    fx.write(&format!("{ASSET}/{run}/c.png"), b"second!");
    fx.write(&format!("{ASSET}/a.manifest.json"), &with_hash(b"second!"));
    probe.order.lock().unwrap().clear();
    index
        .reconcile_ordered(Scope::All, ReconcileOrder::ManifestsFirst)
        .await
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        Some(Some(sha256_bytes(b"second!"))),
        "the candidate was judged against the old manifest"
    );
    let order = probe.order.lock().unwrap().clone();
    let first_other = order.iter().position(|n| !n.ends_with(".manifest.json"));
    let last_manifest = order.iter().rposition(|n| n.ends_with(".manifest.json"));
    assert!(last_manifest < first_other, "manifests first: {order:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-089, CORE-041
async fn read_tools_wait_for_the_first_reconcile() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.copy_fixture_workspace("gate"));
    let probe = FakeProbe::gated();
    let hub = hub(&env, probe.clone());
    let open = tokio::time::timeout(DEADLINE, hub.open(&ws))
        .await
        .expect("the open never waits for the first reconcile")
        .unwrap();
    let gate = probe.clone();
    assert!(
        tokio::task::spawn_blocking(move || gate.wait_until_gated())
            .await
            .unwrap(),
        "the first reconcile never started"
    );

    let call_ctx = ctx(&open, &hub);
    let pending = tokio::spawn(async move { call(&call_ctx, "assets.query", json!({})).await });
    // The index itself answers meanwhile; the tool waits.
    let direct = open.index.assets(query()).await.unwrap();
    assert_eq!(direct.total, 0);
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(
        !pending.is_finished(),
        "assets.query answered before the first reconcile"
    );

    probe.release();
    let page = tokio::time::timeout(DEADLINE, pending)
        .await
        .expect("assets.query never answered")
        .unwrap()
        .unwrap();
    assert_eq!(page["total"], 2, "{page}");
    assert_eq!(
        ids(&page),
        ["sketchbook/chr_paper_crane", "sketchbook/prop_signal_lamp"]
    );
    drop(open);
    hub.close_all().await;
}

#[tokio::test] // spec: CORE-041
async fn cold_rebuild_trusts_manifest_hashes_then_verifies() {
    let fx = fx();
    let (x, y) = (b"x bytes".as_slice(), b"y bytes".as_slice());
    fx.write(&format!("{ASSET}/v1.0.0/x.png"), x);
    fx.write(&format!("{ASSET}/v1.0.0/y.png"), y);
    fx.write(&format!("{ASSET}/notes.txt"), b"not in the manifest");
    fx.write(
        &format!("{ASSET}/a.manifest.json"),
        &manifest_json(
            "p",
            "a",
            json!({"hashes": {
                "v1.0.0/x.png": format!("sha256:{}", sha256_bytes(x)),
                "v1.0.0/y.png": format!("sha256:{}", sha256_bytes(y)),
            }}),
        ),
    );
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    let report = index.rebuild().await.unwrap();
    assert_eq!(
        probe.reads("x.png") + probe.reads("y.png"),
        0,
        "trusted, not read"
    );
    assert_eq!(
        report.hashed, 2,
        "the manifest and the unlisted file: {report:?}"
    );
    let x_row = index
        .files_of_path(&p(&format!("{ASSET}/v1.0.0/x.png")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(x_row.sha256, Some(sha256_bytes(x)));

    // y changed before the background check: it is reported, and stays unverified.
    fx.write(&format!("{ASSET}/v1.0.0/y.png"), b"edited y");
    let mismatches = index.verify_pending(200).await.unwrap();
    assert_eq!(mismatches.len(), 1, "{mismatches:?}");
    assert_eq!(mismatches[0].path, p(&format!("{ASSET}/v1.0.0/y.png")));
    assert_eq!(mismatches[0].sha256, sha256_bytes(b"edited y"));
    assert_eq!(mismatches[0].recorded, sha256_bytes(y));
    assert_eq!(probe.reads("x.png"), 1);
    assert_eq!(probe.reads("y.png"), 1);
    // x is verified now and is not read again.
    index.verify_pending(200).await.unwrap();
    assert_eq!(probe.reads("x.png"), 1);
}

#[tokio::test] // spec: CORE-010, CORE-047
async fn manifest_inputs_become_edges_and_dependents() {
    let fx = fx();
    fx.write(
        &format!("{ASSET}/a.manifest.json"),
        &manifest_json(
            "p",
            "a",
            json!({
                "inputs": [
                    {"ref": "b@1.0.0", "role": "reference"},
                    {"ref": "proj:other/c@2.1.0", "role": "reference"},
                ],
                "pinned_inputs": [{"ref": "lib:shared_d@1.0.0", "reason": "kept"}],
            }),
        ),
    );
    fx.write(
        "projects/p/assets/b/b.manifest.json",
        &manifest_json("p", "b", json!({})),
    );
    let index = fx.open(Arc::new(SystemProbe));
    index.reconcile(Scope::All).await.unwrap();
    let a = ("p".to_string(), "a".to_string());
    assert_eq!(
        index.dependents("p", "b").await.unwrap(),
        std::slice::from_ref(&a)
    );
    assert_eq!(
        index.dependents("other", "c").await.unwrap(),
        std::slice::from_ref(&a)
    );
    assert!(index.dependents("p", "a").await.unwrap().is_empty());

    // An edit that drops the input drops the edge.
    fx.write(
        &format!("{ASSET}/a.manifest.json"),
        &manifest_json("p", "a", json!({"inputs": [{"ref": "proj:other/c@2.1.0"}]})),
    );
    index.reconcile(Scope::All).await.unwrap();
    assert!(index.dependents("p", "b").await.unwrap().is_empty());
    assert_eq!(index.dependents("other", "c").await.unwrap(), [a]);
}

#[tokio::test] // spec: ARCH-079
async fn invalid_manifest_marks_needs_repair() {
    let fx = fx();
    let manifest = format!("{ASSET}/a.manifest.json");
    fx.write(&manifest, &manifest_json("p", "a", json!({})));
    let index = fx.open(Arc::new(SystemProbe));
    index.reconcile(Scope::All).await.unwrap();
    assert!(!index.asset("p", "a").await.unwrap().unwrap().needs_repair);

    fx.write(&manifest, b"{");
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.needs_repair, [p(&manifest)]);
    let row = index.asset("p", "a").await.unwrap().unwrap();
    assert!(row.needs_repair);
    assert_eq!(
        row.status.as_deref(),
        Some("candidate"),
        "the last values stay"
    );
    let file = index.files_of_path(&p(&manifest)).await.unwrap().unwrap();
    assert_eq!(file.state, FileState::NeedsRepair);
    assert_eq!(
        std::fs::read(fx.file(&manifest)).unwrap(),
        b"{",
        "never rewritten"
    );
}

/// Every file under `dir` whose name contains `.db`.
fn db_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            db_files(&path, out);
        } else if path.to_string_lossy().contains(".db") {
            out.push(path);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-096, CORE-005, CORE-006
async fn machine_local_files_never_land_in_the_workspace() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.copy_fixture_workspace("local"));
    let hub = hub(&env, Arc::new(SystemProbe));
    let open = hub.open(&ws).await.unwrap();
    open.index.ready().await;
    open.store
        .write(&p("notes/a.txt"), b"a".to_vec(), Expect::New)
        .await
        .unwrap();
    open.index.reconcile(Scope::All).await.unwrap();
    open.index.verify_pending(1000).await.unwrap();
    let mut found = Vec::new();
    db_files(&ws.path, &mut found);
    assert!(
        found.is_empty(),
        "machine-local files in the workspace: {found:?}"
    );
    let local = env.workspace_local_dir(&ws.path);
    assert!(local.join("index.db").is_file(), "{}", local.display());
    assert!(!local.starts_with(&ws.path));
    drop(open);
    hub.close_all().await;
}

// ---------------------------------------------------------------------------------------------
// The read tools.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-033, CORE-027, CORE-047
async fn asset_get_reports_legacy_versions_and_next_versions() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.copy_fixture_workspace("get"));
    let hub = hub(&env, Arc::new(SystemProbe));
    let open = hub.open(&ws).await.unwrap();
    let c = ctx(&open, &hub);

    let lamp = call_ok(
        &c,
        "asset.get",
        json!({"project": "sketchbook", "asset_id": "prop_signal_lamp"}),
    )
    .await;
    assert_eq!(lamp["asset_id"], "prop_signal_lamp", "{lamp}");
    assert_eq!(lamp["needs_repair"], false, "{lamp}");
    let versions = lamp["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 1, "{lamp}");
    assert_eq!(versions[0]["version"], "1.0.0");
    assert_eq!(versions[0]["legacy"], true);
    assert!(
        versions[0]["ref"].as_str().unwrap().starts_with(&format!(
            "lil://{}/sketchbook/prop_signal_lamp@1.0.0#sha256=",
            ws.id
        )),
        "{lamp}"
    );
    // No approved version: every next version is 1.0.0.
    assert_eq!(
        lamp["next_versions"],
        json!({"major": "1.0.0", "minor": "1.0.0", "patch": "1.0.0"})
    );

    let crane = call_ok(&c, "asset.get", json!({"asset_id": "chr_paper_crane"})).await;
    assert_eq!(
        crane["project"], "sketchbook",
        "an omitted project is sketchbook"
    );
    assert_eq!(crane["versions"][0]["legacy"], false, "{crane}");
    assert_eq!(
        crane["next_versions"],
        json!({"major": "2.0.0", "minor": "1.1.0", "patch": "1.0.1"})
    );
    assert_eq!(crane["proposed_bump"], "minor");
    assert_eq!(crane["approved_version"], "1.0.0");
    assert_eq!(crane["type"], "image");
    assert_eq!(crane["status"], "candidate");
    assert!(crane["updated"].as_str().unwrap().ends_with('Z'), "{crane}");
    assert_eq!(crane["candidate_runs"].as_array().unwrap().len(), 2);
    assert_eq!(crane["human_edits"], json!([]));
    let sealed = "projects/sketchbook/assets/chr_paper_crane/v1.0.0/crane_b.png";
    let state_of = |detail: &Value, file: &str| {
        detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == file)
            .map(|f| f["state"].clone())
    };
    assert_eq!(
        state_of(&crane, "v1.0.0/crane_b.png"),
        Some(json!("ok")),
        "{crane}"
    );

    // A needs_repair file state is `needs_repair` on the wire.
    open.index
        .set_state(&p(sealed), FileState::NeedsRepair)
        .await
        .unwrap();
    let crane = call_ok(&c, "asset.get", json!({"asset_id": "chr_paper_crane"})).await;
    assert_eq!(
        state_of(&crane, "v1.0.0/crane_b.png"),
        Some(json!("needs_repair"))
    );

    // An unknown asset, and a missing manifest.
    let unknown = call(&c, "asset.get", json!({"asset_id": "no_such_asset"}))
        .await
        .unwrap_err();
    assert_eq!(unknown.code, "asset.unknown", "{unknown:?}");
    let manifest = "projects/sketchbook/assets/prop_signal_lamp/prop_signal_lamp.manifest.json";
    outside_remove(&native(&ws.path, manifest));
    open.index.reconcile(Scope::All).await.unwrap();
    let missing = call(&c, "asset.get", json!({"asset_id": "prop_signal_lamp"}))
        .await
        .unwrap_err();
    assert_eq!(missing.code, "manifest.needs_repair", "{missing:?}");
    assert_eq!(missing.data.unwrap()["missing"], manifest);
    let page = call_ok(&c, "assets.query", json!({})).await;
    assert_eq!(page["total"], 2, "the asset stays listed: {page}");
    drop(open);
    hub.close_all().await;
}

/// An asset of [`small_workspace`]: (project, asset, status, type, notes).
type Spec<'a> = (&'a str, &'a str, Option<&'a str>, Option<&'a str>, &'a str);

/// A workspace with these assets.
fn small_workspace(env: &TestEnv, assets: &[Spec<'_>]) -> WorkspaceRef {
    let ws = env.make_workspace("query");
    for (project, asset, status, kind, notes) in assets {
        let mut m = json!({"asset_id": asset, "project": project, "notes": notes});
        if let Some(status) = status {
            m["status"] = json!(status);
        }
        if let Some(kind) = kind {
            m["type"] = json!(kind);
        }
        write_file(
            &native(
                &ws,
                &format!("projects/{project}/assets/{asset}/{asset}.manifest.json"),
            ),
            serde_json::to_string_pretty(&m).unwrap().as_bytes(),
        );
    }
    workspace_ref(&ws)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: APP-025, CORE-047
async fn assets_query_filters_by_project_status_type_and_text() {
    let env = TestEnv::new();
    let ws = small_workspace(
        &env,
        &[
            (
                "alpha",
                "chr_paper_crane",
                Some("candidate"),
                Some("image"),
                "origami bird",
            ),
            (
                "alpha",
                "prop_lamp",
                Some("approved"),
                Some("prop_2d"),
                "signal light",
            ),
            (
                "beta",
                "chr_knight",
                Some("candidate"),
                Some("image"),
                "Ünïcödé space armour",
            ),
            ("beta", "env_wall", None, None, "harbor stones"),
        ],
    );
    let hub = hub(&env, Arc::new(SystemProbe));
    let open = hub.open(&ws).await.unwrap();
    let c = ctx(&open, &hub);
    let q = |args: Value| {
        let c = c.clone();
        async move { ids(&call_ok(&c, "assets.query", args).await) }
    };
    assert_eq!(
        q(json!({})).await,
        [
            "beta/chr_knight",
            "alpha/chr_paper_crane",
            "beta/env_wall",
            "alpha/prop_lamp"
        ]
    );
    assert_eq!(
        q(json!({"project": "alpha"})).await,
        ["alpha/chr_paper_crane", "alpha/prop_lamp"]
    );
    assert_eq!(
        q(json!({"status": "candidate"})).await,
        ["beta/chr_knight", "alpha/chr_paper_crane"]
    );
    assert_eq!(q(json!({"type": "prop_2d"})).await, ["alpha/prop_lamp"]);
    assert_eq!(q(json!({"text": "crane"})).await, ["alpha/chr_paper_crane"]);
    assert_eq!(
        q(json!({"text": "harb"})).await,
        ["beta/env_wall"],
        "prefix match"
    );
    assert_eq!(q(json!({"text": "ünïcödé"})).await, ["beta/chr_knight"]);
    assert_eq!(
        q(json!({"project": "beta", "status": "candidate", "type": "image", "text": "knight"}))
            .await,
        ["beta/chr_knight"]
    );
    assert!(q(json!({"project": "gamma"})).await.is_empty());
    assert!(
        q(json!({"text": "- \""})).await.is_empty(),
        "punctuation matches nothing"
    );
    assert_eq!(
        q(json!({"text": "  "})).await.len(),
        4,
        "blank text is no filter"
    );
    let page = call_ok(&c, "assets.query", json!({"status": "candidate"})).await;
    assert_eq!(page["total"], 2);
    assert_eq!(page["next_cursor"], Value::Null);
    let crane = &page["assets"][1];
    assert_eq!(
        crane["ref"],
        format!("lil://{}/alpha/chr_paper_crane", ws.id)
    );
    assert_eq!(crane["needs_repair"], false);
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 1001}),
        json!({"extra": 1}),
        json!({"sort": {"field": "size", "dir": "asc"}}),
    ] {
        let refused = call(&c, "assets.query", bad.clone()).await.unwrap_err();
        assert_eq!(refused.code, "params.invalid", "{bad}: {refused:?}");
    }
    drop(open);
    hub.close_all().await;
}

const STATUSES: [&str; 5] = [
    "planned",
    "in_progress",
    "candidate",
    "in_review",
    "approved",
];
const TYPES: [&str; 4] = ["image", "prop_2d", "audio", "model_3d"];

#[derive(Debug, Clone, PartialEq)]
struct Row {
    project: String,
    asset: String,
    status: Option<String>,
    kind: Option<String>,
    updated: i64,
}

impl Row {
    fn sort_value(&self, field: SortField) -> Value {
        match field {
            SortField::AssetId => json!(self.asset),
            SortField::Updated => json!(self.updated),
            SortField::Status => json!(self.status.clone().unwrap_or_default()),
            SortField::Type => json!(self.kind.clone().unwrap_or_default()),
        }
    }
}

fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_i64().cmp(&y.as_i64()),
        _ => a.as_str().unwrap().cmp(b.as_str().unwrap()),
    }
}

#[tokio::test] // spec: APP-025, APP-138
async fn assets_query_pages_every_asset_exactly_once_in_order() {
    let fx = fx();
    let projects = ["alpha", "beta", "gamma"];
    let (mut no_status, mut no_type) = (0, 0);
    let mut rows = Vec::new();
    for i in 0..2500_usize {
        let project = projects[i % 3];
        let asset = format!("a{:04}", i / 3);
        let status = if i % 7 == 0 && no_status < 40 {
            no_status += 1;
            None
        } else {
            Some(STATUSES[i % 5])
        };
        let kind = if i % 11 == 1 && no_type < 40 {
            no_type += 1;
            None
        } else {
            Some(TYPES[(i / 3) % 4])
        };
        let updated = 1_700_000_000 + i64::try_from(i % 13).unwrap();
        let mut m = json!({"asset_id": asset, "project": project});
        if let Some(s) = status {
            m["status"] = json!(s);
        }
        if let Some(t) = kind {
            m["type"] = json!(t);
        }
        let rel = format!("projects/{project}/assets/{asset}/{asset}.manifest.json");
        fx.write(&rel, m.to_string().as_bytes());
        set_mtime(&fx.file(&rel), updated);
        rows.push(Row {
            project: project.into(),
            asset,
            status: status.map(str::to_string),
            kind: kind.map(str::to_string),
            updated,
        });
    }
    assert_eq!((no_status, no_type), (40, 40));
    let index = fx.open(Arc::new(SystemProbe));
    index.rebuild().await.unwrap();

    for field in [
        SortField::AssetId,
        SortField::Updated,
        SortField::Status,
        SortField::Type,
    ] {
        for dir in [SortDir::Asc, SortDir::Desc] {
            let mut want = rows.clone();
            want.sort_by(|a, b| {
                let by_value = cmp_values(&a.sort_value(field), &b.sort_value(field));
                let by_value = if dir == SortDir::Desc {
                    by_value.reverse()
                } else {
                    by_value
                };
                by_value
                    .then_with(|| a.project.cmp(&b.project))
                    .then_with(|| a.asset.cmp(&b.asset))
            });
            let want: Vec<(String, String)> =
                want.into_iter().map(|r| (r.project, r.asset)).collect();

            let mut q = query();
            q.sort = (field, dir);
            q.limit = 97;
            let mut got: Vec<(String, String)> = Vec::new();
            let mut cursor: Option<String> = None;
            let mut pages = 0;
            loop {
                q.after = cursor.as_deref().map(|c| decode_cursor(&q, c).unwrap());
                let page = index.assets(q.clone()).await.unwrap();
                assert_eq!(page.total, 2500, "{field:?} {dir:?}");
                assert!(page.rows.len() <= 97);
                got.extend(
                    page.rows
                        .iter()
                        .map(|r| (r.project.clone(), r.asset_id.clone())),
                );
                pages += 1;
                match page.next {
                    Some(k) => cursor = Some(encode_cursor(&q, &k)),
                    None => break,
                }
            }
            assert_eq!(pages, 26, "{field:?} {dir:?}");
            let unique: HashSet<&(String, String)> = got.iter().collect();
            assert_eq!(unique.len(), got.len(), "repeats for {field:?} {dir:?}");
            assert!(
                got == want,
                "{field:?} {dir:?}: the pages differ from the sorted list"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: APP-025
async fn assets_query_cursor_allows_a_new_limit() {
    let env = TestEnv::new();
    let list: Vec<(String, String)> = (0..300)
        .map(|i| ("p".to_string(), format!("a{i:03}")))
        .collect();
    let specs: Vec<Spec<'_>> = list
        .iter()
        .map(|(p, a)| (p.as_str(), a.as_str(), Some("planned"), Some("image"), ""))
        .collect();
    let ws = small_workspace(&env, &specs);
    let hub = hub(&env, Arc::new(SystemProbe));
    let open = hub.open(&ws).await.unwrap();
    let c = ctx(&open, &hub);
    let first = call_ok(&c, "assets.query", json!({"limit": 97})).await;
    assert_eq!(ids(&first).len(), 97);
    assert_eq!(first["total"], 300);
    let cursor = first["next_cursor"].as_str().expect("a cursor").to_string();
    let second = call_ok(&c, "assets.query", json!({"limit": 50, "cursor": cursor})).await;
    let want: Vec<String> = (97..147).map(|i| format!("p/a{i:03}")).collect();
    assert_eq!(ids(&second), want);
    let rest = call_ok(
        &c,
        "assets.query",
        json!({"limit": 1000, "cursor": second["next_cursor"]}),
    )
    .await;
    assert_eq!(ids(&rest).len(), 153);
    assert_eq!(
        rest["next_cursor"],
        Value::Null,
        "the last page has no cursor"
    );
    drop(open);
    hub.close_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: APP-025, ARCH-089
async fn assets_query_cursor_survives_an_unrelated_write() {
    let env = TestEnv::new();
    let names: Vec<String> = (0..10).map(|i| format!("a{i:02}")).collect();
    let specs: Vec<Spec<'_>> = names
        .iter()
        .map(|a| ("p", a.as_str(), Some("planned"), Some("image"), ""))
        .collect();
    let ws = small_workspace(&env, &specs);
    let hub = hub(&env, Arc::new(SystemProbe));
    let open = hub.open(&ws).await.unwrap();
    let c = ctx(&open, &hub);
    let all = call_ok(&c, "assets.query", json!({"limit": 8})).await;
    let want: Vec<String> = ids(&all)[4..8].to_vec();
    let first = call_ok(&c, "assets.query", json!({"limit": 4})).await;
    let cursor = first["next_cursor"].as_str().unwrap().to_string();

    // A write to an asset already returned, a manifest edit that keeps the sort value, and a
    // rebuild of the whole index.
    open.store
        .write(
            &p("projects/p/assets/a01/notes.txt"),
            b"n".to_vec(),
            Expect::New,
        )
        .await
        .unwrap();
    open.store
        .edit_manifest(&p("projects/p/assets/a05/a05.manifest.json"), &|m| {
            m.set("status", json!("in_progress"));
            Ok(())
        })
        .await
        .unwrap();
    open.index.rebuild().await.unwrap();
    let second = call_ok(&c, "assets.query", json!({"limit": 4, "cursor": cursor})).await;
    assert_eq!(ids(&second), want);
    assert_eq!(second["total"], 10);
    drop(open);
    hub.close_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: APP-025
async fn assets_query_cursor_of_another_query_is_params_invalid() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.copy_fixture_workspace("cursor"));
    let hub = hub(&env, Arc::new(SystemProbe));
    let open = hub.open(&ws).await.unwrap();
    let c = ctx(&open, &hub);
    let first = call_ok(
        &c,
        "assets.query",
        json!({"status": "candidate", "limit": 1}),
    )
    .await;
    let cursor = first["next_cursor"].as_str().expect("a cursor").to_string();
    let ok = call_ok(
        &c,
        "assets.query",
        json!({"status": "candidate", "limit": 1, "cursor": cursor}),
    )
    .await;
    assert_eq!(ids(&ok), ["sketchbook/prop_signal_lamp"]);

    let truncated = &cursor[..cursor.len() - 4];
    for args in [
        json!({"status": "planned", "cursor": cursor}),
        json!({"status": "candidate", "sort": {"field": "updated", "dir": "asc"}, "cursor": cursor}),
        json!({"status": "candidate", "cursor": truncated}),
        json!({"status": "candidate", "cursor": "not a cursor"}),
    ] {
        let refused = call(&c, "assets.query", args.clone()).await.unwrap_err();
        assert_eq!(refused.code, "params.invalid", "{args}: {refused:?}");
    }
    // The pure check agrees.
    let mut q = query();
    q.status = Some("candidate".into());
    assert!(decode_cursor(&q, &cursor).is_ok());
    q.status = Some("planned".into());
    assert!(decode_cursor(&q, &cursor).is_err());
    drop(open);
    hub.close_all().await;
}

#[test] // spec: CORE-010, CORE-041
fn kind_of_reads_the_path_alone() {
    let cases = [
        ("projects/p/assets/a/a.manifest.json", FileKind::Manifest),
        (
            "projects/p/assets/a/other.manifest.json",
            FileKind::Manifest,
        ),
        (
            "projects/p/assets/a/candidates/r_20270101_0800/x.png",
            FileKind::Candidate,
        ),
        ("projects/p/assets/a/v1.0.0/x.png", FileKind::Sealed),
        ("projects/p/assets/a/V2.1.0/sub/x.png", FileKind::Sealed),
        ("projects/p/assets/a/work/x.psd", FileKind::Work),
        ("projects/p/assets/a/prompts/x.txt", FileKind::Other),
        ("projects/p/project.yaml", FileKind::Other),
        ("notes/a.manifest.json", FileKind::Other),
    ];
    for (path, want) in cases {
        assert_eq!(kind_of(&p(path)), want, "{path}");
    }
}

#[test] // spec: APP-025
fn cursor_round_trips_and_is_bound_to_the_query() {
    let mut q = query();
    q.sort = (SortField::Updated, SortDir::Desc);
    let k = lil_core::index::Keyset {
        sort_value: json!(1_700_000_005),
        project: "p".into(),
        asset_id: "a".into(),
    };
    let cursor = encode_cursor(&q, &k);
    assert!(
        cursor
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(decode_cursor(&q, &cursor), Ok(k.clone()));
    let mut other = q.clone();
    other.limit = 5;
    assert_eq!(
        decode_cursor(&other, &cursor),
        Ok(k),
        "limit isn't part of it"
    );
    other.text = Some("x".into());
    assert!(decode_cursor(&other, &cursor).is_err());
}

// ---------------------------------------------------------------------------------------------
// Fix round 1: concurrent store changes, closed-core edits, failed first reconciles.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-089, ARCH-075
async fn store_changes_during_a_cold_rebuild_are_kept() {
    let fx = fx();
    fx.write("notes/a.txt", b"a");
    fx.write("notes/b.txt", b"b");
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    index.rebuild().await.unwrap();

    // While the next rebuild looks at b.txt (it has already looked at a.txt), the store
    // writes a file and a manifest, and moves a.txt away.
    let fired = Arc::new(AtomicBool::new(false));
    // Weak: the index holds the probe, which holds this hook, so a strong reference would be a
    // cycle that keeps index.db open after the test.
    let (hook_index, root) = (Arc::downgrade(&index), fx.root.clone());
    let handle = tokio::runtime::Handle::current();
    let installed = probe.hook.set(Box::new(move |path: &Path| {
        if name_of(path) != "b.txt" || fired.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(hook_index) = hook_index.upgrade() else {
            return;
        };
        let new = native(&root, "notes/new.txt");
        write_file(&new, b"from the store");
        let manifest = native(&root, "projects/p/assets/m/m.manifest.json");
        let manifest_bytes = manifest_json("p", "m", json!({}));
        write_file(&manifest, &manifest_bytes);
        outside_remove(&native(&root, "notes/a.txt"));
        let events = [
            StoreEvent {
                path: p("notes/new.txt"),
                kind: ChangeKind::Created,
                sha256: Some(sha256_bytes(b"from the store")),
                identity: Some(SystemProbe.identity(&new).unwrap()),
            },
            StoreEvent {
                path: p("projects/p/assets/m/m.manifest.json"),
                kind: ChangeKind::Created,
                sha256: Some(sha256_bytes(&manifest_bytes)),
                identity: Some(SystemProbe.identity(&manifest).unwrap()),
            },
            StoreEvent {
                path: p("notes/a.txt"),
                kind: ChangeKind::Removed,
                sha256: None,
                identity: None,
            },
        ];
        handle.block_on(hook_index.applied(&events));
    }));
    assert!(installed.is_ok());
    index.rebuild().await.unwrap();

    let new = index
        .files_of_path(&p("notes/new.txt"))
        .await
        .unwrap()
        .expect("the store's new file is kept");
    assert_eq!(new.sha256, Some(sha256_bytes(b"from the store")));
    assert!(
        index.asset("p", "m").await.unwrap().is_some(),
        "the store's new manifest is kept"
    );
    assert!(
        index
            .files_of_path(&p("notes/a.txt"))
            .await
            .unwrap()
            .is_none(),
        "the store's removal is kept"
    );
}

#[tokio::test] // spec: ARCH-079, CORE-034
async fn outside_edit_while_the_core_was_closed_is_surfaced() {
    let fx = fx();
    let sealed = b"sealed bytes";
    let path = format!("{ASSET}/v1.0.0/s.png");
    fx.write(&path, sealed);
    fx.write(
        &format!("{ASSET}/a.manifest.json"),
        &manifest_json(
            "p",
            "a",
            json!({"hashes": {"v1.0.0/s.png": format!("sha256:{}", sha256_bytes(sealed))}}),
        ),
    );
    {
        let index = fx.open(Arc::new(SystemProbe));
        index.rebuild().await.unwrap();
        assert!(index.verify_pending(100).await.unwrap().is_empty());
    }

    // No core runs while the sealed file is edited outside.
    fx.write(&path, b"edited while closed");
    let index = fx.open(Arc::new(SystemProbe));
    assert!(!index.needs_rebuild());
    index.start_first();
    index.ready().await;
    let row = index.files_of_path(&p(&path)).await.unwrap().unwrap();
    assert_eq!(row.sha256, Some(sha256_bytes(b"edited while closed")));
    let mismatches = index.verify_pending(100).await.unwrap();
    assert_eq!(mismatches.len(), 1, "{mismatches:?}");
    assert_eq!(mismatches[0].path, p(&path));
    assert_eq!(mismatches[0].sha256, sha256_bytes(b"edited while closed"));
    assert_eq!(mismatches[0].recorded, sha256_bytes(sealed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-089
async fn read_tools_answer_store_io_until_a_whole_reconcile_succeeds() {
    let env = TestEnv::new();
    let ws = workspace_ref(&env.copy_fixture_workspace("failed"));
    let probe = FakeProbe::with(|b| b.volume = Volume::Panics);
    let hub = hub(&env, probe.clone());
    let open = hub.open(&ws).await.unwrap();
    let c = ctx(&open, &hub);
    let refused = tokio::time::timeout(DEADLINE, call(&c, "assets.query", json!({})))
        .await
        .expect("the query answered")
        .unwrap_err();
    assert_eq!(refused.code, "store.io", "{refused:?}");
    let refused = call(&c, "asset.get", json!({"asset_id": "chr_paper_crane"}))
        .await
        .unwrap_err();
    assert_eq!(refused.code, "store.io", "{refused:?}");

    probe.set(|b| b.volume = Volume::Real);
    open.index.rebuild().await.unwrap();
    let page = call_ok(&c, "assets.query", json!({})).await;
    assert_eq!(page["total"], 2, "{page}");
    drop(open);
    hub.close_all().await;
}

#[tokio::test] // spec: CORE-041
async fn scoped_reconciles_keep_the_scan_start() {
    let fx = fx();
    fx.write("notes/recent.txt", b"first");
    fx.write("notes/other.txt", b"other");
    let probe = FakeProbe::with(|b| {
        b.volume = Volume::Fs("FAT32");
        b.last_write.insert("recent.txt".into(), filetime(NOW - 1));
        b.last_write
            .insert("other.txt".into(), filetime(NOW - 3600));
    });
    let index = fx.open(probe.clone());
    index.reconcile(Scope::All).await.unwrap();

    // Rewritten in the same FAT tick; then, later, a scoped re-check of another file.
    fx.write("notes/recent.txt", b"again");
    fx.clock.advance(Duration::from_secs(60));
    index
        .reconcile(Scope::Paths(vec![p("notes/other.txt")]))
        .await
        .unwrap();
    // The scoped run didn't move the scan start: the whole scan still hashes the file.
    let report = index.reconcile(Scope::All).await.unwrap();
    assert_eq!(report.changed, [p("notes/recent.txt")], "{report:?}");
}

#[tokio::test] // spec: CORE-041
async fn verify_pending_examines_at_most_max_files_and_moves_on() {
    let fx = fx();
    let (x, y) = (b"x bytes".as_slice(), b"y bytes".as_slice());
    fx.write(&format!("{ASSET}/v1.0.0/x.png"), x);
    fx.write(&format!("{ASSET}/v1.0.0/y.png"), y);
    fx.write(
        &format!("{ASSET}/a.manifest.json"),
        &manifest_json(
            "p",
            "a",
            json!({"hashes": {
                "v1.0.0/x.png": format!("sha256:{}", sha256_bytes(x)),
                "v1.0.0/y.png": format!("sha256:{}", sha256_bytes(y)),
            }}),
        ),
    );
    let probe = FakeProbe::new();
    let index = fx.open(probe.clone());
    index.rebuild().await.unwrap();
    fx.write(&format!("{ASSET}/v1.0.0/x.png"), b"edited x");

    // One row per call: x (a mismatch, reported once), then y, then x again (skipped: it
    // hasn't changed since it was reported).
    let first = index.verify_pending(1).await.unwrap();
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!((probe.reads("x.png"), probe.reads("y.png")), (1, 0));
    assert!(index.verify_pending(1).await.unwrap().is_empty());
    assert_eq!((probe.reads("x.png"), probe.reads("y.png")), (1, 1));
    assert!(index.verify_pending(1).await.unwrap().is_empty());
    assert!(index.verify_pending(1).await.unwrap().is_empty());
    assert_eq!(
        (probe.reads("x.png"), probe.reads("y.png")),
        (1, 1),
        "the reported file isn't read again, and y is verified"
    );
}
