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

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use ww_core::events::EventBus;
use ww_core::state::StateRegistry;
use ww_core::store::{FailPoints, StoreError, register_workspace};
use ww_core::tools::workspace_tools::{NETWORK_WARNING, NO_FILE_IDS_WARNING};
use ww_core::workspace::{Access, WorkspaceHub, WorkspaceRef};
use ww_core::{CoreConfig, CoreThread, start_in_thread};
use ww_platform::fs::{FileIdentity, Probe, SystemProbe, VolumeInfo};
use ww_platform::{Clock, Env};
use ww_testkit::files::{sha256_file, write_file};
use ww_testkit::{FakeClock, RawPipe, SeqIds, TestEnv};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// The five `.gitignore` entries (Global Constraints, CORE-040).
const GITIGNORE: [&str; 5] = [
    ".worldwright/",
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
    ww_core::store::fsops::lock_exclusive(&ws.join(".worldwright").join("core.lock"))
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
    // The folder has its files, worldwright.yaml included, and isn't registered.
    for rel in [
        r"projects\sketchbook\project.yaml",
        r"projects\sketchbook\progress.md",
        ".gitignore",
        ".gitattributes",
        "worldwright.yaml",
    ] {
        assert!(ws.join(rel).is_file(), "{rel} is missing");
    }
    assert_eq!(registration(&env, &ws), (false, false));
    let progress = std::fs::read_to_string(ws.join(r"projects\sketchbook\progress.md")).unwrap();
    assert!(progress.contains("## Done"), "{progress}");
    let project = lines(&ws.join(r"projects\sketchbook\project.yaml"));
    assert!(
        project.iter().any(|l| l == "slug: sketchbook"),
        "{project:?}"
    );
    assert!(project.iter().any(|l| l == "status: active"), "{project:?}");
    let yaml = lines(&ws.join("worldwright.yaml"));
    assert!(
        yaml.iter().any(|l| l == "name: Ünïcödé space ws"),
        "{yaml:?}"
    );

    // A retry adopts the folder and registers it, as the default (none is set).
    let yaml_hash = sha256_file(&ws.join("worldwright.yaml"));
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
    assert_eq!(sha256_file(&ws.join("worldwright.yaml")), yaml_hash);
    assert_eq!(registration(&env, &ws), (true, true));
}

#[test] // spec: ARCH-003, CORE-007
fn create_adopts_an_existing_workspace_without_writing() {
    let env = TestEnv::new();
    let ws = env.make_workspace("Ünïcödé adopted");
    let before = sha256_file(&ws.join("worldwright.yaml"));
    let _core = core(&env);
    let mut pipe = none(&env);

    let out = create(&mut pipe, json!({"path": arg(&ws), "default": false}));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["structured"]["created"], false, "{out}");
    assert_eq!(out["structured"]["default"], false, "{out}");
    assert_eq!(sha256_file(&ws.join("worldwright.yaml")), before);
    for absent in [".gitignore", ".gitattributes", "projects", ".worldwright"] {
        assert!(!ws.join(absent).exists(), "{absent} was written");
    }
    assert_eq!(registration(&env, &ws), (true, false));
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
        assert!(ws.join("worldwright.yaml").is_file(), "{name}");
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
    let sub = admin.call("ww/subscribe", json!({"topics": ["workspace"]}));
    assert_eq!(sub["result"]["status"], "ok", "{sub}");

    // A folder without worldwright.yaml is refused.
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
        .filter(|n| n.contains(".ww-"))
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
        message.contains(&format!("`ww workspace use \"{}\"`", hinted.display())),
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
            .contains(&format!("`ww workspace use \"{path}\"`")),
        "{refused}"
    );
}
