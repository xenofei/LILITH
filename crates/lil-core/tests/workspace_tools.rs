//! The workspace tools over the real pipe of an in-process core (ARCH-003, ARCH-090, ARCH-094,
//! ARCH-098, CORE-007, CORE-040, QA-003; owner decisions 1 and 2): `workspace.create`,
//! `workspace.set_default`, `suite.toml` writes and the `workspace.unsupported` hint.
//!
//! Every workspace is under the test's `TestEnv` root, and every `suite.toml` is the test
//! channel's (QA-040). Volumes are faked through the core's `Probe` (Review Focus 4).
//! Wall-clock durations here are failure deadlines only, never measurements (QA-031).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lil_core::events::EventBus;
use lil_core::state::StateRegistry;
use lil_core::store::{FailPoints, StoreError, register_workspace};
use lil_core::tools::workspace_tools::{NETWORK_WARNING, NO_FILE_IDS_WARNING};
use lil_core::workspace::{Access, WorkspaceHub, WorkspaceRef};
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_platform::fs::{FileIdentity, Probe, SystemProbe, VolumeInfo};
use lil_platform::{Clock, Env};
use lil_testkit::files::{sha256_file, write_file};
use lil_testkit::{FakeClock, RawPipe, SeqIds, TestEnv};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// The five `.gitignore` entries (Global Constraints, CORE-040).
const GITIGNORE: [&str; 5] = [
    ".lilith/",
    ".trash/",
    "inbox/*",
    "projects/*/assets/*/candidates/",
    "projects/*/assets/*/work/",
];

/// What the fake volume says.
#[derive(Debug, Clone)]
enum Volume {
    /// The real facts.
    Real,
    /// These facts.
    Facts(VolumeInfo),
    /// The facts can't be read.
    Unreadable,
    /// The real facts, and the folder carries `FILE_ATTRIBUTE_PINNED` (a synced folder).
    Pinned,
}

/// The real probe with a fake volume.
#[derive(Debug)]
struct FakeVolume(Mutex<Volume>);

impl FakeVolume {
    fn new() -> Arc<FakeVolume> {
        Arc::new(FakeVolume(Mutex::new(Volume::Real)))
    }

    fn set(&self, volume: Volume) {
        *self.0.lock().unwrap() = volume;
    }

    fn get(&self) -> Volume {
        self.0.lock().unwrap().clone()
    }
}

impl Probe for FakeVolume {
    fn identity(&self, path: &Path) -> io::Result<FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if matches!(self.get(), Volume::Pinned) && path.is_dir() {
            id.attributes |= 0x0008_0000;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        match self.get() {
            Volume::Real | Volume::Pinned => SystemProbe.volume_info(path),
            Volume::Facts(info) => Ok(info),
            Volume::Unreadable => Err(io::Error::from_raw_os_error(1)),
        }
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }
}

fn facts(fs_name: &str, remote: bool, cloud_sync_root: bool) -> Volume {
    Volume::Facts(VolumeInfo {
        fs_name: fs_name.to_string(),
        remote,
        cloud_sync_root,
    })
}

/// An in-process core with the fake clock and ids, `probe` and `fp`.
fn core_with(env: &TestEnv, probe: Arc<dyn Probe>, fp: Arc<FailPoints>) -> CoreThread {
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        probe,
        failpoints: fp,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    })
}

fn core(env: &TestEnv) -> CoreThread {
    core_with(env, Arc::new(SystemProbe), FailPoints::none())
}

/// A fresh idempotency key.
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("ws-tools-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// A connection whose hello names no workspace.
fn none(env: &TestEnv) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

/// A connection whose hello names the workspace `ws`, checked to succeed.
fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

fn arg(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 test path")
}

fn create(pipe: &mut RawPipe, args: Value) -> Value {
    pipe.tool("workspace.create", args, Some(&key()))
}

fn canonical(path: &Path) -> PathBuf {
    let c = std::fs::canonicalize(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let text = c.to_string_lossy().into_owned();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

fn same(a: &Path, b: &Path) -> bool {
    a.exists()
        && b.exists()
        && canonical(a)
            .to_string_lossy()
            .eq_ignore_ascii_case(&canonical(b).to_string_lossy())
}

/// Whether `ws` is registered in the test channel's `suite.toml`, and whether it is the
/// default.
fn registration(env: &TestEnv, ws: &Path) -> (bool, bool) {
    let (default, listed) = env.registered();
    (
        listed.iter().any(|p| same(p, ws)),
        default.is_some_and(|d| same(&d, ws)),
    )
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

/// The workspace `ws` as the core resolves it.
fn workspace_ref(ws: &Path) -> WorkspaceRef {
    let raw = std::fs::canonicalize(ws).expect("canonicalize");
    WorkspaceRef {
        id: Env::workspace_hash(&raw),
        path: canonical(ws),
    }
}

/// Sends a `tools/call` without waiting for its response.
fn send_call(pipe: &mut RawPipe, id: u64, name: &str, args: Value, key: &str) {
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": args, "idempotency_key": key},
    });
    pipe.send_line(&request.to_string());
}

/// The `result` of the response with `id`, reading past anything else.
fn response_for(pipe: &mut RawPipe, id: u64) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = pipe
            .read_line(left)
            .unwrap_or_else(|| panic!("no response to request {id} within {DEADLINE:?}"));
        let message: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
        if message["id"] == json!(id) {
            return message["result"].clone();
        }
    }
}

/// The state of the idempotency row `key` in `db`, read-only; `None` without one.
fn row_state(db: &Path, key: &str) -> Option<String> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row("SELECT state FROM idempotency WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .ok()
}

/// Waits (a failure deadline) until `db` holds a `running` row for `key`.
fn wait_for_running_row(db: &Path, key: &str) {
    let deadline = Instant::now() + DEADLINE;
    while row_state(db, key).as_deref() != Some("running") {
        assert!(
            Instant::now() < deadline,
            "no running row for {key:?} within {DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether another holder could take `ws`'s `core.lock` now.
fn lock_is_free(ws: &Path) -> bool {
    lil_core::store::fsops::lock_exclusive(&ws.join(".lilith").join("core.lock"))
        .unwrap()
        .is_some()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

// ---------------------------------------------------------------------------------------------
// workspace.create
// ---------------------------------------------------------------------------------------------

#[test] // spec: CORE-007, ARCH-098, ARCH-075
fn create_writes_layout_and_registers_last() {
    let env = TestEnv::with_label("Ünïcödé space");
    // `store.suite_toml=io`, once: the next attempt goes through.
    let fp = FailPoints::none();
    let failed = Arc::new(AtomicBool::new(false));
    let once = failed.clone();
    fp.on(
        "store.suite_toml",
        Box::new(move || {
            if once.swap(true, Ordering::SeqCst) {
                Ok(())
            } else {
                Err(io::Error::other("fail point store.suite_toml: io"))
            }
        }),
    );
    let _core = core_with(&env, Arc::new(SystemProbe), fp);
    let ws = env.root().join("Ünïcödé space ws");
    let mut pipe = none(&env);

    let first = create(&mut pipe, json!({"path": arg(&ws)}));
    assert_eq!(first["status"], "error", "{first}");
    assert_eq!(first["error"]["code"], "store.io", "{first}");
    assert!(failed.load(Ordering::SeqCst));
    // The folder has its files, lilith.yaml included, and isn't registered.
    for rel in [
        r"projects\sketchbook\project.yaml",
        r"projects\sketchbook\progress.md",
        ".gitignore",
        ".gitattributes",
        "lilith.yaml",
    ] {
        assert!(ws.join(rel).is_file(), "{rel} is missing");
    }
    assert_eq!(registration(&env, &ws), (false, false));
    assert!(
        lock_is_free(&ws),
        "the failed create still holds the workspace's core.lock"
    );
    let progress = std::fs::read_to_string(ws.join(r"projects\sketchbook\progress.md")).unwrap();
    assert!(progress.contains("## Done"), "{progress}");
    let project = lines(&ws.join(r"projects\sketchbook\project.yaml"));
    assert!(
        project.iter().any(|l| l == "slug: sketchbook"),
        "{project:?}"
    );
    assert!(project.iter().any(|l| l == "status: active"), "{project:?}");
    let yaml = lines(&ws.join("lilith.yaml"));
    assert!(
        yaml.iter().any(|l| l == "name: Ünïcödé space ws"),
        "{yaml:?}"
    );

    // A retry adopts the folder and registers it, as the default (none is set).
    let yaml_hash = sha256_file(&ws.join("lilith.yaml"));
    let retry = create(&mut pipe, json!({"path": arg(&ws)}));
    assert_eq!(retry["status"], "ok", "{retry}");
    let s = &retry["structured"];
    assert_eq!(s["created"], false, "{retry}");
    assert_eq!(s["default"], true, "{retry}");
    assert_eq!(s["id"], json!(env.workspace_id(&ws)), "{retry}");
    assert_eq!(
        s["path"],
        json!(canonical(&ws).display().to_string()),
        "{retry}"
    );
    assert_eq!(sha256_file(&ws.join("lilith.yaml")), yaml_hash);
    assert_eq!(registration(&env, &ws), (true, true));
}

#[test] // spec: ARCH-003, CORE-007
fn create_adopts_an_existing_workspace_without_writing() {
    let env = TestEnv::new();
    let ws = env.make_workspace("Ünïcödé adopted");
    let before = sha256_file(&ws.join("lilith.yaml"));
    let _core = core(&env);
    let mut pipe = none(&env);

    let out = create(&mut pipe, json!({"path": arg(&ws), "default": false}));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["structured"]["created"], false, "{out}");
    assert_eq!(out["structured"]["default"], false, "{out}");
    assert_eq!(sha256_file(&ws.join("lilith.yaml")), before);
    for absent in [".gitignore", ".gitattributes", "projects", ".lilith"] {
        assert!(!ws.join(absent).exists(), "{absent} was written");
    }
    assert_eq!(registration(&env, &ws), (true, false));
}

#[test] // spec: ARCH-003, CORE-007
fn create_writes_only_lilith_yaml_and_adopts_a_legacy_workspace() {
    use lil_platform::names::{LEGACY_WORKSPACE_DIR, LEGACY_WORKSPACE_FILE, WORKSPACE_FILE};
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = none(&env);

    // A new workspace gets only the new file, and no legacy folder.
    let fresh = env.root().join("fresh");
    let out = create(&mut pipe, json!({"path": arg(&fresh), "default": false}));
    assert_eq!(out["structured"]["created"], true, "{out}");
    assert!(fresh.join(WORKSPACE_FILE).is_file());
    for absent in [LEGACY_WORKSPACE_FILE, LEGACY_WORKSPACE_DIR] {
        assert!(!fresh.join(absent).exists(), "{absent} was written");
    }

    // A legacy workspace is adopted as it is: registered, nothing written or renamed.
    let legacy = env.make_legacy_workspace("legacy");
    let before = sha256_file(&legacy.join(LEGACY_WORKSPACE_FILE));
    let out = create(&mut pipe, json!({"path": arg(&legacy), "default": false}));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["structured"]["created"], false, "{out}");
    assert_eq!(sha256_file(&legacy.join(LEGACY_WORKSPACE_FILE)), before);
    for absent in [WORKSPACE_FILE, ".gitignore", "projects", ".lilith"] {
        assert!(!legacy.join(absent).exists(), "{absent} was written");
    }
    assert_eq!(registration(&env, &legacy), (true, false));

    // `workspace.set_default` accepts it too; serving it never creates the legacy folder.
    let out = pipe.tool_ok(
        "workspace.set_default",
        json!({"path": arg(&legacy)}),
        Some(&key()),
    );
    assert_eq!(out["id"], json!(env.workspace_id(&legacy)), "{out}");
    assert_eq!(registration(&env, &legacy), (true, true));
    let mut s = session(&env, &legacy);
    let query = s.tool_ok("assets.query", json!({}), None);
    assert_eq!(query["total"], 0, "{query}");
    assert_eq!(sha256_file(&legacy.join(LEGACY_WORKSPACE_FILE)), before);
    assert!(!legacy.join(WORKSPACE_FILE).exists());
    assert!(!legacy.join(LEGACY_WORKSPACE_DIR).exists());
}

#[test] // spec: ARCH-094, QA-040
fn create_refuses_nested_workspace_and_files() {
    let env = TestEnv::new();
    let outer = env.make_workspace("outer");
    env.register_workspace(&outer, true);
    let file = env.root().join("a file.txt");
    write_file(&file, b"not a folder\n");
    let _core = core(&env);
    let mut pipe = none(&env);

    let inner = outer.join("Ünïcödé inner");
    let nested = create(&mut pipe, json!({"path": arg(&inner)}));
    assert_eq!(nested["status"], "error", "{nested}");
    assert_eq!(
        nested["error"]["code"], "workspace.path_invalid",
        "{nested}"
    );
    assert!(
        nested["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("inside the registered workspace")),
        "{nested}"
    );
    assert!(!inner.exists(), "the nested folder was created");

    let refused = create(&mut pipe, json!({"path": arg(&file)}));
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(
        refused["error"]["code"], "workspace.path_invalid",
        "{refused}"
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"not a folder\n");

    // Neither was registered.
    let (_, listed) = env.registered();
    assert_eq!(listed.len(), 1, "{listed:?}");
}

/// The nested check runs before anything is created, also when the new folder is spelled
/// another way than the registered one (here through a junction; on CI the temp folder's
/// short name `RUNNER~1` did the same): the part that exists is compared canonically.
#[test] // spec: ARCH-094, QA-040
fn create_refuses_a_nested_folder_spelled_through_a_link_without_creating_it() {
    let env = TestEnv::new();
    let outer = env.make_workspace("outer");
    env.register_workspace(&outer, true);
    let link = env.root().join("Ünïcödé link");
    junction(&link, &outer);
    let _core = core(&env);
    let mut pipe = none(&env);

    let inner = link.join("inner").join("deeper");
    let nested = create(&mut pipe, json!({"path": arg(&inner)}));
    assert_eq!(nested["status"], "error", "{nested}");
    assert_eq!(
        nested["error"]["code"], "workspace.path_invalid",
        "{nested}"
    );
    assert!(
        nested["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("inside the registered workspace")),
        "{nested}"
    );
    assert!(
        !outer.join("inner").exists(),
        "the nested folder was created"
    );
    let (_, listed) = env.registered();
    assert_eq!(listed.len(), 1, "{listed:?}");
}

/// A junction at `link` to the folder `target` (no privilege needed).
#[allow(clippy::disallowed_methods)]
fn junction(link: &Path, target: &Path) {
    lil_platform::fs::create_junction(link, target)
        .unwrap_or_else(|e| panic!("cannot create a junction at {}: {e}", link.display()));
}

#[test] // spec: CORE-041, ARCH-089
fn create_warns_on_network_or_cloud_volumes() {
    let env = TestEnv::new();
    let volume = FakeVolume::new();
    let _core = core_with(&env, volume.clone(), FailPoints::none());
    let mut pipe = none(&env);

    for (name, fake) in [
        ("network", facts("NTFS", true, false)),
        ("cloud", facts("NTFS", false, true)),
        ("pinned", Volume::Pinned),
    ] {
        volume.set(fake);
        let ws = env.root().join(name);
        let out = create(&mut pipe, json!({"path": arg(&ws)}));
        assert_eq!(out["status"], "ok", "{name}: {out}");
        let warnings = &out["structured"]["warnings"];
        assert_eq!(warnings, &json!([NETWORK_WARNING]), "{name}: {out}");
        assert!(ws.join("lilith.yaml").is_file(), "{name}");
    }
    // A local NTFS or ReFS folder has no warning.
    volume.set(facts("NTFS", false, false));
    let out = create(&mut pipe, json!({"path": arg(&env.root().join("local"))}));
    assert_eq!(out["structured"]["warnings"], json!([]), "{out}");
}

#[test] // spec: CORE-041, ARCH-089
fn create_on_a_fat_or_usb_volume_warns_and_succeeds() {
    let env = TestEnv::with_label("Ünïcödé space");
    let volume = FakeVolume::new();
    let _core = core_with(&env, volume.clone(), FailPoints::none());
    let mut pipe = none(&env);

    for (name, fake) in [
        ("fat", facts("FAT32", false, false)),
        ("usb", Volume::Unreadable),
    ] {
        volume.set(fake);
        let ws = env.root().join(format!("Ünïcödé {name}"));
        let out = create(&mut pipe, json!({"path": arg(&ws), "default": true}));
        assert_eq!(out["status"], "ok", "{name}: {out}");
        let s = &out["structured"];
        assert_eq!(s["created"], true, "{name}: {out}");
        assert_eq!(s["default"], true, "{name}: {out}");
        assert_eq!(s["warnings"], json!([NO_FILE_IDS_WARNING]), "{name}: {out}");

        // It opens and serves as a workspace without stable IDs.
        let mut a = session(&env, &ws);
        let wrote = a.tool_ok(
            "test.store_write",
            json!({"path": "notes/a.txt", "text": "Ünïcödé", "expect": "new"}),
            Some(&key()),
        );
        assert_eq!(wrote["sha256"].as_str().map(str::len), Some(64), "{wrote}");
        assert!(ws.join(r"notes\a.txt").is_file(), "{name}");
    }
}

#[test] // spec: CORE-040
fn create_writes_gitattributes_and_appends_to_an_existing_one() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = none(&env);

    // A folder with its own .gitattributes (CRLF) and .gitignore.
    let ws = env.root().join("has files");
    write_file(&ws.join(".gitattributes"), b"*.png binary\r\n*.psd -text");
    write_file(&ws.join(".gitignore"), b"/build\n.trash/\n");
    let out = create(&mut pipe, json!({"path": arg(&ws)}));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join(".gitattributes")).unwrap(),
        "*.png binary\r\n*.psd -text\r\nprojects/** -text\r\n"
    );
    let ignore = lines(&ws.join(".gitignore"));
    assert_eq!(&ignore[..2], ["/build", ".trash/"], "{ignore:?}");
    for entry in GITIGNORE {
        assert_eq!(
            ignore.iter().filter(|l| *l == entry).count(),
            1,
            "{entry}: {ignore:?}"
        );
    }

    // One that has the entry already keeps a single copy, and a new folder gets one.
    let ws = env.root().join("has entry");
    write_file(&ws.join(".gitattributes"), b"projects/** -text\n");
    let out = create(&mut pipe, json!({"path": arg(&ws)}));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join(".gitattributes")).unwrap(),
        "projects/** -text\n"
    );
    let ws = env.root().join("new");
    create(&mut pipe, json!({"path": arg(&ws)}));
    assert_eq!(lines(&ws.join(".gitattributes")), ["projects/** -text"]);
}

// ---------------------------------------------------------------------------------------------
// workspace.set_default
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-090, CORE-007
fn set_default_registers_and_switches() {
    let env = TestEnv::with_label("Ünïcödé space");
    let a = env.make_workspace("a");
    let b = env.make_workspace("Ünïcödé b");
    env.register_workspace(&a, true);
    let _core = core(&env);
    let mut s_a = session(&env, &a);
    let mut admin = none(&env);
    let sub = admin.call("lil/subscribe", json!({"topics": ["workspace"]}));
    assert_eq!(sub["result"]["status"], "ok", "{sub}");

    // A folder without lilith.yaml is refused.
    let plain = env.root().join("plain");
    write_file(&plain.join("notes.txt"), b"x");
    let refused = admin.tool(
        "workspace.set_default",
        json!({"path": arg(&plain)}),
        Some(&key()),
    );
    assert_eq!(
        refused["error"]["code"], "workspace.path_invalid",
        "{refused}"
    );

    let out = admin.tool_ok(
        "workspace.set_default",
        json!({"path": arg(&b)}),
        Some(&key()),
    );
    assert_eq!(out["id"], json!(env.workspace_id(&b)), "{out}");
    assert_eq!(out["path"], json!(canonical(&b).display().to_string()));
    assert_eq!(registration(&env, &b), (true, true));
    assert_eq!(registration(&env, &a), (true, false));
    let event = admin
        .next_event(DEADLINE)
        .expect("a workspace event within the deadline");
    assert_eq!(event["topic"], "workspace", "{event}");
    assert_eq!(event["data"]["kind"], "default_changed", "{event}");
    assert_eq!(event["data"]["id"], json!(env.workspace_id(&b)), "{event}");

    // The old default's sessions are refused; the new default is served.
    let after = s_a.tool("test.echo", json!({"text": "x"}), None);
    assert_eq!(after["error"]["code"], "workspace.unsupported", "{after}");
    let mut s_b = session(&env, &b);
    s_b.tool_ok(
        "test.store_write",
        json!({"path": "notes/b.txt", "text": "b", "expect": "new"}),
        Some(&key()),
    );

    // Setting the default it already is changes nothing.
    let before = std::fs::read(env.env.suite_toml()).unwrap();
    admin.tool_ok(
        "workspace.set_default",
        json!({"path": arg(&b)}),
        Some(&key()),
    );
    assert_eq!(std::fs::read(env.env.suite_toml()).unwrap(), before);
}

#[test] // spec: ARCH-028, ARCH-090
fn set_default_closes_the_old_workspace() {
    let env = TestEnv::new();
    let a = env.make_workspace("a");
    let b = env.make_workspace("b");
    env.register_workspace(&a, true);
    env.register_workspace(&b, false);
    let core = core(&env);
    let db = env.workspace_local_dir(&a).join("state.db");

    // A call in `a` holds it across the switch.
    let mut s_a = session(&env, &a);
    let k = key();
    send_call(&mut s_a, 900, "test.block", json!({"gate": "held"}), &k);
    wait_for_running_row(&db, &k);
    let mut admin = none(&env);
    admin.tool_ok(
        "workspace.set_default",
        json!({"path": arg(&b)}),
        Some(&key()),
    );
    assert!(!lock_is_free(&a), "a call still holds the old workspace");

    // Once that call ends, the old workspace's watcher, index and core.lock are let go of.
    core.core().test_gates().open("held");
    let done = response_for(&mut s_a, 900);
    assert_eq!(done["status"], "ok", "{done}");
    assert_eq!(row_state(&db, &k).as_deref(), Some("done"));

    // A second hub can take the old workspace's core.lock.
    let rt = runtime();
    let states = Arc::new(
        StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}")))
            .expect("a second state registry"),
    );
    let clock: Arc<dyn Clock> = FakeClock::new();
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        Arc::new(EventBus::new(clock.clone())),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        Arc::new(SystemProbe),
        lil_core::watch::WatchConfig::default(),
    );
    let open = rt.block_on(hub.open(&workspace_ref(&a))).expect("the open");
    assert_eq!(open.access, Access::ReadWrite);
    drop(open);
    rt.block_on(hub.close_all());
    states.close();
}

/// Carry (Task 7): a workspace the hub let go of while a call held it, opened again in the
/// same core, is taken back as it is: its running idempotency rows are never released, so a
/// retry of the in-flight call doesn't run it twice.
#[test] // spec: ARCH-028, CORE-016
fn reopening_a_workspace_still_in_use_keeps_its_running_rows() {
    let env = TestEnv::new();
    let a = env.make_workspace("a");
    let b = env.make_workspace("b");
    env.register_workspace(&a, true);
    env.register_workspace(&b, false);
    let core = core(&env);
    let db = env.workspace_local_dir(&a).join("state.db");

    let mut s_a = session(&env, &a);
    let k = key();
    let args = json!({"gate": "reopen"});
    send_call(&mut s_a, 901, "test.block", args.clone(), &k);
    wait_for_running_row(&db, &k);

    // Away and back while the call runs.
    let mut admin = none(&env);
    for ws in [&b, &a] {
        admin.tool_ok(
            "workspace.set_default",
            json!({"path": arg(ws)}),
            Some(&key()),
        );
    }
    // A new session's call opens `a` again in this core.
    let mut s_a2 = session(&env, &a);
    s_a2.tool_ok("test.echo", json!({"text": "reopened"}), None);
    assert_eq!(
        row_state(&db, &k).as_deref(),
        Some("running"),
        "the reopen released this core's own in-flight row"
    );
    let busy = s_a2.tool("test.block", args.clone(), Some(&k));
    assert_eq!(busy["error"]["code"], "idempotency.in_progress", "{busy}");

    core.core().test_gates().open("reopen");
    let done = response_for(&mut s_a, 901);
    assert_eq!(done["structured"]["executions"], 1, "{done}");
    let replay = s_a2.tool_ok("test.block", args, Some(&k));
    assert_eq!(replay["executions"], 1, "the retry ran the tool again");
}

// ---------------------------------------------------------------------------------------------
// suite.toml and the unsupported hint
// ---------------------------------------------------------------------------------------------

#[tokio::test] // spec: CORE-007, ARCH-076
async fn suite_toml_keeps_comments_and_unknown_keys_and_is_hash_checked() {
    let env = TestEnv::with_label("Ünïcödé space");
    let suite = env.env.suite_toml();
    write_file(&suite, b"# my note\n[caps]\nmonthly_usd = 50\n");
    let ws = env.make_workspace("Ünïcödé one");

    // Between the read and the replace another writer adds a table, once.
    let fp = FailPoints::none();
    let (changed, file) = (Arc::new(AtomicBool::new(false)), suite.clone());
    let once = changed.clone();
    fp.on(
        "store.suite_toml",
        Box::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                let mut text = std::fs::read_to_string(&file)?;
                text.push_str("\n[other]\nkey = \"outside\"\n");
                write_file(&file, text.as_bytes());
            }
            Ok(())
        }),
    );
    let written = register_workspace(&env.env, &canonical(&ws), true, &fp)
        .await
        .expect("register");
    assert!(written);
    assert!(changed.load(Ordering::SeqCst));
    let text = std::fs::read_to_string(&suite).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.contains(&"# my note"), "{text}");
    assert!(
        lines
            .windows(2)
            .any(|w| w == ["[caps]", "monthly_usd = 50"]),
        "{text}"
    );
    assert!(
        lines
            .windows(2)
            .any(|w| w == ["[other]", "key = \"outside\""]),
        "the outside edit was lost:\n{text}"
    );
    assert_eq!(registration(&env, &ws), (true, true));
    // No temporary or backup file is left beside it.
    let leftovers: Vec<String> = std::fs::read_dir(suite.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".lil-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Registering it again, spelled differently, changes nothing (ruling Q6).
    let upper = PathBuf::from(canonical(&ws).display().to_string().to_uppercase());
    let again = register_workspace(&env.env, &upper, true, &FailPoints::none())
        .await
        .expect("register again");
    assert!(!again);
    assert_eq!(std::fs::read_to_string(&suite).unwrap(), text);

    // A file that keeps changing under every attempt is not written.
    let restless = FailPoints::none();
    let (file, n) = (suite.clone(), Arc::new(AtomicU64::new(0)));
    let count = n.clone();
    restless.on(
        "store.suite_toml",
        Box::new(move || {
            let i = count.fetch_add(1, Ordering::SeqCst);
            let mut text = std::fs::read_to_string(&file)?;
            text.push_str(&format!("# outside {i}\n"));
            write_file(&file, text.as_bytes());
            Ok(())
        }),
    );
    let two = env.make_workspace("two");
    let refused = register_workspace(&env.env, &canonical(&two), false, &restless)
        .await
        .expect_err("a restless suite.toml was written");
    assert!(
        matches!(refused, StoreError::HashConflict { .. }),
        "{refused:?}"
    );
    assert_eq!(n.load(Ordering::SeqCst), 2, "one re-read, no more");
    assert_eq!(registration(&env, &two), (false, false));
    let text = std::fs::read_to_string(&suite).unwrap();
    assert!(text.ends_with("# outside 0\n# outside 1\n"), "{text}");
}

/// A fail point hook that writes `text` to `file` the first time it runs.
fn write_once(file: &Path, text: &'static str) -> Box<dyn Fn() -> io::Result<()> + Send + Sync> {
    let (file, done) = (file.to_path_buf(), AtomicBool::new(false));
    Box::new(move || {
        if !done.swap(true, Ordering::SeqCst) {
            write_file(&file, text.as_bytes());
        }
        Ok(())
    })
}

/// The names in `suite.toml`'s folder that contain `part`.
fn beside(suite: &Path, part: &str) -> Vec<String> {
    std::fs::read_dir(suite.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(part))
        .collect()
}

/// Two outside saves race one write: X lands after the hash check and before the replace
/// (so the backup holds X and is put back), and Y lands after the replace and before the
/// put-back (so the put-back's second backup holds Y, not this core's bytes). X ends up under
/// the name, and Y is kept beside it as a conflict copy, never deleted (Task 6's C1 rule).
#[tokio::test] // spec: CORE-007, ARCH-074
async fn suite_toml_put_back_keeps_a_second_outside_save() {
    let env = TestEnv::new();
    let suite = env.env.suite_toml();
    write_file(&suite, b"# base\n");
    let ws = env.make_workspace("raced");
    let fp = FailPoints::none();
    fp.on("store.replace", write_once(&suite, "# X\n"));
    fp.on("store.after_replace", write_once(&suite, "# Y\n"));

    let refused = register_workspace(&env.env, &canonical(&ws), true, &fp)
        .await
        .expect_err("the raced write succeeded");
    let StoreError::HashConflict { kept, .. } = &refused else {
        panic!("expected store.hash_conflict, got {refused:?}");
    };
    assert_eq!(std::fs::read_to_string(&suite).unwrap(), "# X\n");
    let copies = beside(&suite, ".lil-conflict-");
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert!(
        copies[0].starts_with("suite.toml.lil-conflict-"),
        "{copies:?}"
    );
    let copy = suite.parent().unwrap().join(&copies[0]);
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "# Y\n");
    assert_eq!(
        kept.iter()
            .map(|k| k.as_str().to_string())
            .collect::<Vec<_>>(),
        copies
    );
    let error = refused.to_error("t");
    assert_eq!(error.code, "store.hash_conflict");
    assert_eq!(error.data.unwrap()["kept"], json!(copies));
    assert!(beside(&suite, ".lil-tmp-").is_empty());
    assert!(beside(&suite, ".lil-bak-").is_empty());
}

/// A failure after the replace leaves the replaced version in the backup, and the error names
/// that backup (it is never deleted unchecked).
#[tokio::test] // spec: CORE-007, ARCH-074
async fn suite_toml_failure_after_the_replace_names_the_backup() {
    let env = TestEnv::new();
    let suite = env.env.suite_toml();
    write_file(&suite, b"# before\n");
    let ws = env.make_workspace("stopped");
    let fp = FailPoints::none();
    fp.on(
        "store.after_replace",
        Box::new(|| Err(io::Error::other("fail point store.after_replace: io"))),
    );
    let refused = register_workspace(&env.env, &canonical(&ws), true, &fp)
        .await
        .expect_err("the stopped write succeeded");
    let StoreError::Io { path, .. } = &refused else {
        panic!("expected store.io naming the backup, got {refused:?}");
    };
    let backups = beside(&suite, ".lil-bak-");
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(path.ends_with(&backups[0]), "{path} vs {backups:?}");
    let backup = suite.parent().unwrap().join(&backups[0]);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "# before\n");
    assert_eq!(
        registration(&env, &ws),
        (true, true),
        "the new version is in place"
    );
    let error = refused.to_error("t");
    assert_eq!(error.code, "store.io");
    assert!(error.message.contains(&backups[0]), "{}", error.message);
}

#[tokio::test] // spec: CORE-007
async fn first_suite_toml_is_created_new() {
    let env = TestEnv::new();
    let suite = env.env.suite_toml();
    assert!(!suite.exists());
    let ws = env.make_workspace("first");
    assert!(
        register_workspace(&env.env, &canonical(&ws), false, &FailPoints::none())
            .await
            .unwrap()
    );
    assert_eq!(registration(&env, &ws), (true, false));
}

#[test] // spec: QA-003, ARCH-090
fn unsupported_fix_has_the_agent_hint_and_a_quoted_path() {
    let env = TestEnv::with_label("Ünïcödé space");
    let a = env.make_workspace("Ünïcödé a");
    let b = env.make_workspace("Ünïcödé b");
    env.register_workspace(&a, true);
    env.register_workspace(&b, false);
    let _core = core(&env);

    // At hello: the agent hint names `b`, and the message quotes it.
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&b)}));
    let error = &hello["result"]["error"];
    assert_eq!(error["code"], "workspace.unsupported", "{hello}");
    let hint = &error["fix"]["agent_hint"];
    assert_eq!(hint["tool"], "workspace.set_default", "{hello}");
    let hinted = PathBuf::from(hint["arguments"]["path"].as_str().unwrap());
    assert!(same(&hinted, &b), "{hello}");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("`lil workspace use \"{}\"`", hinted.display())),
        "{message}"
    );
    // The default as suite.toml names it.
    assert!(
        message.contains(&format!("\"{}\"", a.display())),
        "{message}"
    );

    // Following the hint works: the call it names makes `b` the default.
    let mut admin = none(&env);
    let mut s_a = session(&env, &a);
    let followed = admin.tool(
        hint["tool"].as_str().unwrap(),
        hint["arguments"].clone(),
        Some(&key()),
    );
    assert_eq!(followed["status"], "ok", "{followed}");
    session(&env, &b);

    // In a call: the old default's session gets the hint back to `a`.
    let refused = s_a.tool("test.echo", json!({"text": "x"}), None);
    let error = &refused["error"];
    assert_eq!(error["code"], "workspace.unsupported", "{refused}");
    let path = error["fix"]["agent_hint"]["arguments"]["path"]
        .as_str()
        .unwrap();
    assert!(same(Path::new(path), &a), "{refused}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains(&format!("`lil workspace use \"{path}\"`")),
        "{refused}"
    );
}

// ---------------------------------------------------------------------------------------------
// A plugin-era workspace and `workspace.doctor`'s legacy-name offer (owner question 1).
// ---------------------------------------------------------------------------------------------

/// A plugin-era workspace: the legacy workspace file, the legacy hint naming the project
/// `kites`, an asset whose manifest's `inputs` hold a legacy ref (a note that only mentions
/// one stays as written), and a sealed, read-only snapshot holding one too.
struct PluginEra {
    ws: PathBuf,
    file: PathBuf,
    hint: PathBuf,
    manifest: PathBuf,
    snapshot: PathBuf,
}

fn plugin_era(env: &TestEnv) -> PluginEra {
    use lil_schema::LEGACY_SCHEME;
    let ws = env.root().join("plugin era");
    let file = ws.join("worldwright.yaml");
    write_file(&file, b"name: plugin era\n");
    let hint = ws.join(".worldwright\\state.json");
    write_file(&hint, br#"{"active_project":"kites","panel":"left"}"#);
    write_file(
        &ws.join("projects").join("kites").join("project.yaml"),
        b"name: Kites\nslug: kites\ntype: media\nstatus: active\nlinks: []\ncreated: 2026-09-01\n",
    );
    let id = env.workspace_id(&ws);
    let asset = ws
        .join("projects")
        .join("kites")
        .join("assets")
        .join("kite_red");
    let manifest = asset.join("kite_red.manifest.json");
    let text = format!(
        "{{\r\n  \"asset_id\": \"kite_red\",\r\n  \"project\": \"kites\",\r\n  \"type\": \
         \"prop_2d\",\r\n  \"version\": \"1.1.0\",\r\n  \"approved_version\": \"1.0.0\",\r\n  \
         \"notes\": \"copied by hand from {LEGACY_SCHEME}{id}/kites/kite_blue@1.0.0\",\r\n  \
         \"inputs\": [\r\n    {{\"ref\": \"ww://{id}/kites/kite_blue@1.0.0\", \"role\": \
         \"reference\"}}\r\n  ]\r\n}}\r\n"
    );
    write_file(&manifest, text.as_bytes());
    let snapshot = asset.join("v1.0.0").join("manifest.snapshot.json");
    write_file(
        &snapshot,
        format!("{{\"inputs\": [{{\"ref\": \"ww://{id}/kites/kite_blue@1.0.0\"}}]}}\n").as_bytes(),
    );
    lil_core::store::fsops::set_readonly(&snapshot, true).unwrap();
    PluginEra {
        ws,
        file,
        hint,
        manifest,
        snapshot,
    }
}

/// The check `id` of a `workspace.doctor` result: its state and detail.
fn doctor_check(out: &Value, id: &str) -> (String, String) {
    let check = out["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("no checks: {out}"))
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check: {out}"));
    (
        check["state"].as_str().unwrap().to_string(),
        check["detail"].as_str().unwrap().to_string(),
    )
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path).unwrap().permissions().readonly()
}

#[test] // spec: ARCH-002, CORE-004, CORE-026 (owner question 1)
fn legacy_names_are_renamed_only_when_offered_from_an_interactive_session() {
    use lil_platform::names::{LEGACY_WORKSPACE_DIR, WORKSPACE_FILE};
    let env = TestEnv::new();
    let era = plugin_era(&env);
    let _core = core(&env);
    let mut admin = none(&env);
    let out = create(&mut admin, json!({"path": arg(&era.ws)}));
    assert_eq!(
        out["structured"]["created"], false,
        "registered, not made: {out}"
    );
    let (file, hint, manifest, snapshot) = (
        read_bytes(&era.file),
        read_bytes(&era.hint),
        read_bytes(&era.manifest),
        read_bytes(&era.snapshot),
    );
    let new_hint = era.ws.join(".lilith").join("state.json");

    // The core serves it as it is: the legacy hint's project is the active one.
    let mut s = session(&env, &era.ws);
    let status = s.tool_ok("project.status", json!({}), None);
    assert_eq!(status["project"], "kites", "{status}");

    // Reported, never renamed without the offer.
    let out = s.tool_ok("workspace.doctor", json!({"fix": true}), Some(&key()));
    let (state, detail) = doctor_check(&out, "legacy.names");
    assert_eq!(state, "problem", "{out}");
    assert!(detail.starts_with("legacy names"), "{detail}");
    for named in ["worldwright.yaml", "state.json", "kite_red.manifest.json"] {
        assert!(detail.contains(named), "{named}: {detail}");
    }
    assert!(!detail.contains("manifest.snapshot.json"), "{detail}");
    assert_eq!(doctor_check(&out, "legacy.sealed_refs").0, "info", "{out}");
    assert!(!era.ws.join(WORKSPACE_FILE).exists());
    assert!(!new_hint.exists());
    assert_eq!(read_bytes(&era.manifest), manifest);

    // A non-interactive session can't run the offer, whatever it asks.
    let mut quiet = RawPipe::connect(&env);
    let hello =
        quiet.hello(json!({"workspace": env.workspace_id(&era.ws), "non_interactive": true}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let refused = quiet.tool(
        "workspace.doctor",
        json!({"fix": true, "offers": ["legacy_names"]}),
        Some(&key()),
    );
    assert_eq!(refused["error"]["code"], "caller.not_allowed", "{refused}");
    assert_eq!(read_bytes(&era.file), file);
    assert!(!era.ws.join(WORKSPACE_FILE).exists());

    // Offered (`lil doctor` after a `y`): renamed through the store, nothing deleted.
    let out = s.tool_ok(
        "workspace.doctor",
        json!({"fix": true, "offers": ["legacy_names"]}),
        Some(&key()),
    );
    let (state, detail) = doctor_check(&out, "legacy.names");
    assert_eq!(state, "fixed", "{out}");
    let (state, detail_sealed) = doctor_check(&out, "legacy.sealed_refs");
    assert_eq!(state, "skipped", "{out}");
    assert!(
        detail_sealed.contains("v1.0.0/manifest.snapshot.json"),
        "{detail_sealed}"
    );
    assert_eq!(read_bytes(&era.ws.join(WORKSPACE_FILE)), file, "{detail}");
    assert!(!era.file.exists(), "{detail}");
    assert_eq!(read_bytes(&new_hint), hint, "the hint is copied as it was");
    let rewritten = String::from_utf8(manifest.clone())
        .unwrap()
        .replace("\"ww://", "\"lil://");
    assert_eq!(
        String::from_utf8(read_bytes(&era.manifest)).unwrap(),
        rewritten,
        "only the ref changed"
    );
    assert_eq!(
        read_bytes(&era.snapshot),
        snapshot,
        "the sealed snapshot is never rewritten"
    );
    assert!(is_read_only(&era.snapshot));
    assert_eq!(
        read_bytes(&era.hint),
        hint,
        "the legacy folder stays as it is"
    );
    assert!(era.ws.join(LEGACY_WORKSPACE_DIR).is_dir());

    // Afterwards: nothing to rename, the sealed snapshot is information, and it still reads.
    let out = s.tool_ok("workspace.doctor", json!({"fix": true}), Some(&key()));
    assert_eq!(doctor_check(&out, "legacy.names").0, "ok", "{out}");
    assert_eq!(doctor_check(&out, "legacy.sealed_refs").0, "info", "{out}");
    let status = s.tool_ok("project.status", json!({}), None);
    assert_eq!(status["project"], "kites", "{status}");
}

/// The rename's order (the hint, then the refs, then the workspace file last) means an
/// interruption leaves a plugin-era workspace the core still serves; the next doctor reports
/// what is left and its offer finishes it.
#[test] // spec: ARCH-002, CORE-004 (owner question 1)
fn an_interrupted_legacy_rename_is_finished_by_the_next_offer() {
    use lil_platform::names::WORKSPACE_FILE;
    let env = TestEnv::new();
    let era = plugin_era(&env);
    // Interrupted before the workspace file's move: the hint is copied, the ref rewritten.
    let hint = read_bytes(&era.hint);
    write_file(&era.ws.join(".lilith").join("state.json"), &hint);
    let manifest = String::from_utf8(read_bytes(&era.manifest))
        .unwrap()
        .replace("\"ww://", "\"lil://");
    write_file(&era.manifest, manifest.as_bytes());
    env.register_workspace(&era.ws, true);
    let _core = core(&env);

    let mut s = session(&env, &era.ws);
    assert_eq!(
        s.tool_ok("project.status", json!({}), None)["project"],
        "kites"
    );
    let out = s.tool_ok("workspace.doctor", json!({"fix": false}), Some(&key()));
    let (state, detail) = doctor_check(&out, "legacy.names");
    assert_eq!(state, "problem", "{out}");
    assert!(detail.contains("worldwright.yaml"), "{detail}");
    assert!(
        !detail.contains("state.json") && !detail.contains("manifest"),
        "{detail}"
    );

    let out = s.tool_ok(
        "workspace.doctor",
        json!({"fix": false, "offers": ["legacy_names"]}),
        Some(&key()),
    );
    assert_eq!(doctor_check(&out, "legacy.names").0, "fixed", "{out}");
    assert!(era.ws.join(WORKSPACE_FILE).is_file() && !era.file.exists());
    assert_eq!(read_bytes(&era.manifest), manifest.as_bytes());
}

/// A manifest whose ref fields hold `scheme` refs, and whose free text (`notes`, a pin's
/// `reason`, a human edit's `note`) is exactly a legacy ref. One `"ref" : ` has a space before
/// its colon.
fn manifest_with_free_text(id: &str, scheme: &str) -> String {
    use lil_schema::LEGACY_SCHEME;
    format!(
        "{{\n  \"asset_id\": \"kite_red\",\n  \"project\": \"kites\",\n  \"notes\": \
         \"{LEGACY_SCHEME}{id}/kites/kite_blue@1.0.0\",\n  \"inputs\": [{{\"ref\" : \
         \"{scheme}{id}/kites/kite_blue@1.0.0\"}}],\n  \"pinned_inputs\": [{{\"ref\": \
         \"{scheme}{id}/kites/kite_green@2.0.0\", \"reason\": \
         \"{LEGACY_SCHEME}{id}/kites/kite_green@2.0.0\"}}],\n  \"human_edits\": [{{\"path\": \
         \"v1.0.0/a.png\", \"note\": \"{LEGACY_SCHEME}{id}/kites/kite_blue@1.0.0\"}}]\n}}\n"
    )
}

/// Only `inputs[].ref` and `pinned_inputs[].ref` are rewritten: free text that is exactly a
/// legacy ref stays byte for byte (the adversarial review's probe).
#[test] // spec: CORE-148 (owner question 1)
fn probe_free_text_equal_to_a_ref_is_left_alone() {
    use lil_schema::LEGACY_SCHEME;
    let env = TestEnv::new();
    let era = plugin_era(&env);
    let id = env.workspace_id(&era.ws);
    write_file(
        &era.manifest,
        manifest_with_free_text(&id, LEGACY_SCHEME).as_bytes(),
    );
    env.register_workspace(&era.ws, true);
    let _core = core(&env);
    let mut s = session(&env, &era.ws);

    let out = s.tool_ok(
        "workspace.doctor",
        json!({"fix": false, "offers": ["legacy_names"]}),
        Some(&key()),
    );
    assert_eq!(doctor_check(&out, "legacy.names").0, "fixed", "{out}");
    assert_eq!(
        String::from_utf8(read_bytes(&era.manifest)).unwrap(),
        manifest_with_free_text(&id, "lil://"),
        "only the ref fields changed"
    );
}

/// The real probe, with one file reported as a cloud-only placeholder (never read).
#[derive(Debug)]
struct CloudOnly(&'static str);

impl Probe for CloudOnly {
    fn identity(&self, path: &Path) -> io::Result<FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if path.file_name().is_some_and(|n| n == self.0) {
            id.attributes |= lil_platform::fs::PLACEHOLDER_ATTRIBUTES;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }
}

/// A cloud-only manifest is listed as skipped, never read or edited, and the rest of the
/// rename goes on.
#[test] // spec: ARCH-074, CORE-004 (owner question 1)
fn a_cloud_only_manifest_is_skipped_and_the_rest_is_renamed() {
    use lil_platform::names::WORKSPACE_FILE;
    let env = TestEnv::new();
    let era = plugin_era(&env);
    let id = env.workspace_id(&era.ws);
    let cloud = era
        .ws
        .join("projects")
        .join("kites")
        .join("assets")
        .join("kite_cloud")
        .join("kite_cloud.manifest.json");
    let cloud_text =
        format!("{{\"inputs\": [{{\"ref\": \"ww://{id}/kites/kite_blue@1.0.0\"}}]}}\n");
    write_file(&cloud, cloud_text.as_bytes());
    env.register_workspace(&era.ws, true);
    let _core = core_with(
        &env,
        Arc::new(CloudOnly("kite_cloud.manifest.json")),
        FailPoints::none(),
    );
    let mut s = session(&env, &era.ws);

    let out = s.tool_ok(
        "workspace.doctor",
        json!({"fix": false, "offers": ["legacy_names"]}),
        Some(&key()),
    );
    assert_eq!(doctor_check(&out, "legacy.names").0, "fixed", "{out}");
    let (state, detail) = doctor_check(&out, "legacy.unreadable");
    assert_eq!(state, "skipped", "{out}");
    assert!(detail.contains("kite_cloud.manifest.json"), "{detail}");
    assert!(detail.contains("make them available offline"), "{detail}");
    assert_eq!(read_bytes(&cloud), cloud_text.as_bytes(), "never edited");
    assert!(era.ws.join(WORKSPACE_FILE).is_file() && !era.file.exists());
    assert!(era.ws.join(".lilith").join("state.json").is_file());
    let manifest = String::from_utf8(read_bytes(&era.manifest)).unwrap();
    assert!(manifest.contains("\"ref\": \"lil://"), "{manifest}");
}

/// Arms `store.replace` to fail while `armed` holds and `when()` is true; the hook runs before
/// the file call it stands for, so the file it guards isn't written yet.
fn fail_replace_when(
    fp: &FailPoints,
    armed: &Arc<AtomicBool>,
    when: impl Fn() -> bool + Send + Sync + 'static,
) {
    let armed = armed.clone();
    fp.on(
        "store.replace",
        Box::new(move || {
            if armed.load(Ordering::SeqCst) && when() {
                Err(io::Error::other("fail point: store.replace"))
            } else {
                Ok(())
            }
        }),
    );
}

/// P2: the rename is cut after the hint, at the manifest's edit. Nothing is lost, the core
/// still serves the workspace, and the next offer finishes the rename.
#[test] // spec: ARCH-074, CORE-004 (owner question 1)
fn a_rename_cut_after_the_hint_is_finished_by_the_next_offer() {
    use lil_platform::names::WORKSPACE_FILE;
    let env = TestEnv::new();
    let era = plugin_era(&env);
    env.register_workspace(&era.ws, true);
    let (file, hint, manifest) = (
        read_bytes(&era.file),
        read_bytes(&era.hint),
        read_bytes(&era.manifest),
    );
    let new_hint = era.ws.join(".lilith").join("state.json");
    let fp = FailPoints::none();
    let armed = Arc::new(AtomicBool::new(true));
    let watched = new_hint.clone();
    fail_replace_when(&fp, &armed, move || watched.exists());
    let _core = core_with(&env, Arc::new(SystemProbe), fp);
    let mut s = session(&env, &era.ws);
    let offer = json!({"fix": false, "offers": ["legacy_names"]});

    let out = s.tool_ok("workspace.doctor", offer.clone(), Some(&key()));
    let (state, detail) = doctor_check(&out, "legacy.names");
    assert_eq!(state, "problem", "{out}");
    assert!(detail.contains("the rename stopped"), "{detail}");
    assert_eq!(read_bytes(&new_hint), hint, "the hint was copied");
    assert_eq!(
        read_bytes(&era.manifest),
        manifest,
        "the cut edit left the manifest"
    );
    assert_eq!(read_bytes(&era.file), file);
    assert!(!era.ws.join(WORKSPACE_FILE).exists());
    assert_eq!(
        s.tool_ok("project.status", json!({}), None)["project"],
        "kites"
    );

    armed.store(false, Ordering::SeqCst);
    let out = s.tool_ok("workspace.doctor", offer, Some(&key()));
    assert_eq!(doctor_check(&out, "legacy.names").0, "fixed", "{out}");
    assert_eq!(read_bytes(&era.ws.join(WORKSPACE_FILE)), file);
    let rewritten = String::from_utf8(manifest)
        .unwrap()
        .replace("\"ww://", "\"lil://");
    assert_eq!(
        String::from_utf8(read_bytes(&era.manifest)).unwrap(),
        rewritten
    );
    assert_eq!(read_bytes(&era.hint), hint, "the legacy hint stays");
    assert_eq!(
        s.tool_ok("project.status", json!({}), None)["project"],
        "kites"
    );
}

/// P3: the workspace file's move fails, and then a new workspace file appears outside the
/// suite. The legacy file is never touched: it is reported as information, nothing is lost,
/// the core still serves, and the next offer has nothing left to do.
#[test] // spec: ARCH-074, CORE-004 (owner question 1)
fn a_failed_move_then_a_new_workspace_file_leaves_the_legacy_one_alone() {
    use lil_platform::names::WORKSPACE_FILE;
    let env = TestEnv::new();
    let era = plugin_era(&env);
    env.register_workspace(&era.ws, true);
    let file = read_bytes(&era.file);
    let new_file = era.ws.join(WORKSPACE_FILE);
    let fp = FailPoints::none();
    let armed = Arc::new(AtomicBool::new(true));
    let (manifest, watched) = (era.manifest.clone(), new_file.clone());
    fail_replace_when(&fp, &armed, move || {
        !watched.exists()
            && std::fs::read_to_string(&manifest).is_ok_and(|t| t.contains("\"lil://"))
    });
    let _core = core_with(&env, Arc::new(SystemProbe), fp);
    let mut s = session(&env, &era.ws);
    let offer = json!({"fix": false, "offers": ["legacy_names"]});

    let out = s.tool_ok("workspace.doctor", offer.clone(), Some(&key()));
    let (state, detail) = doctor_check(&out, "legacy.names");
    assert_eq!(state, "problem", "{out}");
    assert!(detail.contains("the rename stopped"), "{detail}");
    assert_eq!(
        read_bytes(&era.file),
        file,
        "the failed move kept the legacy file"
    );
    assert!(!new_file.exists());

    // Someone writes a new workspace file outside the suite.
    write_file(&new_file, b"name: made by hand\n");
    assert_eq!(
        s.tool_ok("project.status", json!({}), None)["project"],
        "kites"
    );
    let out = s.tool_ok("workspace.doctor", offer, Some(&key()));
    assert_eq!(doctor_check(&out, "legacy.names").0, "ok", "{out}");
    let (state, detail) = doctor_check(&out, "legacy.stray_file");
    assert_eq!(state, "info", "{out}");
    assert!(detail.contains("worldwright.yaml"), "{detail}");
    assert_eq!(
        read_bytes(&era.file),
        file,
        "the legacy file is never touched"
    );
    assert_eq!(read_bytes(&new_file), b"name: made by hand\n");
    armed.store(false, Ordering::SeqCst);
}
