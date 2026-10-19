//! Crash and recovery, the fast subset (QA-055; ARCH-074, ARCH-075, CORE-004, CORE-005,
//! CORE-038): a real `lil-core` killed at every write-order step and between a seal and its
//! manifest edit, a full disk in the middle of a commit, a manifest held open during a commit,
//! a damaged or missing `index.db`, a damaged workspace `state.db`, and suite databases a core
//! must refuse without changing anything, and a job whose core died in `submitting`.
//!
//! Crashes use the fail point action `abort` (`TerminateProcess` on the core itself), and a
//! fresh core is started only after the dead one has been seen to exit. Every workspace and
//! database is under the test's `TestEnv` root. Durations are failure deadlines only; the held
//! manifest runs on tokio's paused clock (QA-031).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lil_core::state::{StateDb, StateScope};
use lil_core::store::{CommitOp, FailPoints, ManifestEdit, OutputMove, Store, StoreError, WsPath};
use lil_platform::fs::{Share, SystemProbe};
use lil_schema::Manifest;
use lil_schema::logs::read_lines;
use lil_testkit::files::write_file;
use lil_testkit::{
    CoreFixture, FakeClock, HandleHolder, RawPipe, SeqIds, TestEnv, failpoints, sha256_file,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
/// The exit code of the fail point action `abort`.
const ABORT_EXIT: u32 = 0xDEAD;
/// The fixture asset runs are ingested into.
const ASSET: &str = "chr_paper_crane";
/// Its folder.
const ASSET_DIR: &str = "projects/sketchbook/assets/chr_paper_crane";
/// The runs the fixture already has.
const FIXTURE_RUNS: [&str; 2] = ["r_20261002_0915", "r_20261003_1400"];

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "crash-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A copy of the fixture workspace, registered as the channel's default.
fn default_workspace(env: &TestEnv, name: &str) -> PathBuf {
    let ws = env.copy_fixture_workspace(name);
    env.register_workspace(&ws, true);
    ws
}

/// A connection whose hello names `ws`, checked to succeed.
fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

fn file(ws: &Path, rel: &str) -> PathBuf {
    ws.join(rel.replace('/', "\\"))
}

/// Every file under `dir`, recursively, as `/`-separated paths relative to `root`.
fn files_under(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy();
            out.push(rel.replace('\\', "/"));
        }
    }
}

fn all_files(ws: &Path) -> Vec<String> {
    let mut out = Vec::new();
    files_under(ws, ws, &mut out);
    out.sort();
    out
}

/// The run folders of the fixture asset that the fixture didn't have.
fn new_runs(ws: &Path) -> Vec<String> {
    let mut runs: Vec<String> = std::fs::read_dir(file(ws, &format!("{ASSET_DIR}/candidates")))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !FIXTURE_RUNS.contains(&n.as_str()))
        .collect();
    runs.sort();
    runs
}

/// `test.ingest_run`'s arguments for one file.
fn ingest_args(name: &str, text: &str) -> Value {
    json!({
        "project": "sketchbook",
        "asset_id": ASSET,
        "files": [{"name": name, "text": text}],
        "job_line": true,
    })
}

/// Sends `tools/call name args` with `key` and expects the core to die instead of answering.
fn call_and_die(core: &mut CoreFixture, pipe: &mut RawPipe, name: &str, args: Value, key: &str) {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9_000_001,
        "method": "tools/call",
        "params": {"name": name, "arguments": args, "idempotency_key": key},
    });
    pipe.send_line(&request.to_string());
    if let Some(line) = pipe.read_line(DEADLINE) {
        panic!("the core answered instead of stopping: {line}");
    }
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(ABORT_EXIT),
        "the core didn't end at the fail point"
    );
}

/// What a fresh core must find after any crash: no store temp or backup file, every manifest
/// parses, every log line parses, and `assets.query` agrees with the manifests on disk.
fn assert_consistent(ws: &Path, pipe: &mut RawPipe, what: &str) {
    let files = all_files(ws);
    let leftovers: Vec<&String> = files
        .iter()
        .filter(|f| f.contains(".lil-tmp-") || f.contains(".lil-bak-"))
        .collect();
    assert!(leftovers.is_empty(), "{what}: {leftovers:?}");

    let mut manifests = std::collections::BTreeMap::new();
    for rel in files.iter().filter(|f| f.ends_with(".manifest.json")) {
        let text = std::fs::read_to_string(file(ws, rel)).unwrap();
        let (m, _) =
            Manifest::parse(&text).unwrap_or_else(|e| panic!("{what}: {rel} doesn't parse: {e:?}"));
        manifests.insert(rel.clone(), m);
    }
    for rel in files
        .iter()
        .filter(|f| f.starts_with("queue/") && f.ends_with(".jsonl"))
    {
        let text = std::fs::read_to_string(file(ws, rel)).unwrap();
        let written = text.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(read_lines(&text).len(), written, "{what}: {rel}:\n{text}");
    }

    let query = pipe.tool_ok("assets.query", json!({"limit": 1000}), None);
    let assets = query["assets"].as_array().expect("assets");
    assert_eq!(assets.len(), manifests.len(), "{what}: {query}");
    for asset in assets {
        let rel = format!(
            "projects/{}/assets/{}/{}.manifest.json",
            asset["project"].as_str().unwrap(),
            asset["asset_id"].as_str().unwrap(),
            asset["asset_id"].as_str().unwrap()
        );
        let m = manifests
            .get(&rel)
            .unwrap_or_else(|| panic!("{what}: no manifest {rel}"));
        for field in ["status", "version", "approved_version"] {
            let on_disk = m.get(field).cloned().unwrap_or(Value::Null);
            assert_eq!(asset[field], on_disk, "{what}: {rel} {field}");
        }
    }
}

/// The orphans `project.status` lists, without trailing `/`.
fn orphans(pipe: &mut RawPipe) -> Vec<String> {
    let status = pipe.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    status["orphans"]
        .as_array()
        .unwrap_or_else(|| panic!("{status}"))
        .iter()
        .map(|o| o.as_str().unwrap().trim_end_matches('/').to_string())
        .collect()
}

#[test] // spec: QA-055, ARCH-075, CORE-038
fn crash_at_each_write_order_step_recovers_cleanly() {
    for point in [
        "store.after_output",
        "store.after_manifest",
        "store.after_jobs_log",
        "store.after_progress",
        "store.after_index",
    ] {
        let env = TestEnv::with_label("Ünïcödé space");
        let ws = default_workspace(&env, "steps");
        let k = key();
        let text = format!("written before {point}");
        {
            let spec = failpoints::spec(&[(point, "abort")]);
            let mut core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &spec)]);
            let mut pipe = session(&env, &ws);
            call_and_die(
                &mut core,
                &mut pipe,
                "test.ingest_run",
                ingest_args("a.png", &text),
                &k,
            );
        }
        let runs = new_runs(&ws);
        assert_eq!(runs.len(), 1, "{point}: {runs:?}");
        let first = runs[0].clone();
        let first_file = file(&ws, &format!("{ASSET_DIR}/candidates/{first}/a.png"));
        assert_eq!(
            std::fs::read_to_string(&first_file).unwrap(),
            text,
            "{point}"
        );

        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, &ws);
        let listed = orphans(&mut pipe);
        let folder = format!("{ASSET_DIR}/candidates/{first}");
        assert_eq!(
            listed.contains(&folder),
            point == "store.after_output",
            "{point}: the run is an orphan only when its manifest entry never landed: {listed:?}"
        );
        assert_consistent(&ws, &mut pipe, point);

        // The dead core's `running` row was released under `core.lock`: the same key runs
        // again, and both runs' files exist.
        let retry = pipe.tool_ok("test.ingest_run", ingest_args("a.png", &text), Some(&k));
        let second = retry["run_id"].as_str().expect("a run id").to_string();
        assert_ne!(second, first, "{point}");
        assert!(
            first_file.is_file(),
            "{point}: the first run's file is gone"
        );
        assert!(
            file(&ws, &format!("{ASSET_DIR}/candidates/{second}/a.png")).is_file(),
            "{point}: the retry's file is missing"
        );
        assert_consistent(&ws, &mut pipe, &format!("{point}, after the retry"));
    }
}

#[test] // spec: QA-055, CORE-038, ARCH-075
fn crash_between_seal_and_manifest_lists_the_version_and_loses_nothing() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "seal");
    let sources = env.root().join("Ünïcödé sources");
    let (a, b) = (sources.join("kite_a.png"), sources.join("kite_b.png"));
    write_file(&a, b"kite a");
    write_file(&b, b"kite b");
    let asset_dir = "projects/sketchbook/assets/prop_kite";
    let run = {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, &ws);
        let imported = pipe.tool_ok(
            "asset.import",
            json!({"project": "sketchbook", "asset_id": "prop_kite", "type": "image",
                   "files": [a.to_string_lossy(), b.to_string_lossy()], "mode": "copy"}),
            Some(&key()),
        );
        imported["run_id"].as_str().expect("a run id").to_string()
    };
    let before = all_files(&ws);
    {
        let spec = failpoints::spec(&[("store.after_seal", "abort")]);
        let mut core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &spec)]);
        let mut pipe = session(&env, &ws);
        call_and_die(
            &mut core,
            &mut pipe,
            "asset.signoff",
            json!({"project": "sketchbook", "asset_id": "prop_kite", "run_id": run,
                   "files": ["kite_a.png"]}),
            &key(),
        );
    }

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    let listed = orphans(&mut pipe);
    let version = format!("{asset_dir}/v1.0.0");
    assert!(
        listed.contains(&version),
        "{version} isn't listed: {listed:?}"
    );
    for name in ["kite_a.png", "kite_b.png"] {
        let places = [
            file(&ws, &format!("{version}/{name}")),
            file(&ws, &format!("{asset_dir}/candidates/{run}/{name}")),
        ];
        let found = places.iter().filter(|p| p.is_file()).count();
        assert_eq!(found, 1, "{name} exists {found} times: {places:?}");
    }
    let manifest =
        std::fs::read_to_string(file(&ws, &format!("{asset_dir}/prop_kite.manifest.json")))
            .unwrap();
    let (m, _) = Manifest::parse(&manifest).expect("the manifest parses");
    assert!(
        m.get("approved_version").is_none_or(Value::is_null),
        "{manifest}"
    );
    // Nothing was deleted: every file is still there, possibly moved into the version folder.
    let after = all_files(&ws);
    for rel in &before {
        let name = rel.rsplit('/').next().unwrap();
        let kept = after.contains(rel)
            || after
                .iter()
                .any(|f| f.starts_with(&format!("{version}/")) && f.ends_with(&format!("/{name}")));
        assert!(kept, "{rel} was lost");
    }
    assert!(a.is_file() && b.is_file(), "the sources were touched");
    assert_consistent(&ws, &mut pipe, "after the seal crash");
}

#[test] // spec: QA-055, ARCH-074, ARCH-075
fn disk_full_mid_commit_keeps_files_consistent() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "full");
    let manifest = file(&ws, &format!("{ASSET_DIR}/{ASSET}.manifest.json"));
    let before = sha256_file(&manifest);
    {
        // The outputs land, then the manifest write meets a full disk.
        let spec = failpoints::spec(&[("store.after_output", "disk_full")]);
        let _core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &spec)]);
        let mut pipe = session(&env, &ws);
        let result = pipe.tool(
            "test.ingest_run",
            ingest_args("full.png", "full"),
            Some(&key()),
        );
        assert_eq!(result["status"], "error", "{result}");
        assert_eq!(result["error"]["code"], "store.disk_full", "{result}");
        assert_eq!(sha256_file(&manifest), before, "the manifest changed");
        assert!(
            !all_files(&ws).iter().any(|f| f.starts_with("queue/")),
            "a jobs line was written"
        );
    }
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    let runs = new_runs(&ws);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let folder = format!("{ASSET_DIR}/candidates/{}", runs[0]);
    assert!(
        orphans(&mut pipe).contains(&folder),
        "the landed output is listed"
    );
    assert_consistent(&ws, &mut pipe, "after the full disk");
    // With room again, the same work goes through.
    pipe.tool_ok(
        "test.ingest_run",
        ingest_args("full.png", "full"),
        Some(&key()),
    );
    assert_consistent(&ws, &mut pipe, "after the retry");
}

// ---------------------------------------------------------------------------------------------
// A held manifest (in-process, paused clock).
// ---------------------------------------------------------------------------------------------

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[tokio::test(start_paused = true)] // spec: QA-055, ARCH-074
async fn held_manifest_during_commit_is_retried() {
    let env = TestEnv::with_label("Ünïcödé space");
    let root = std::fs::canonicalize(env.make_workspace("held")).unwrap();
    let text = root.to_string_lossy().into_owned();
    let root = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
    let db = env.root().join("local-ws").join("state.db");
    let state = Arc::new(
        StateDb::open(&db, StateScope::Workspace, Arc::new(|_: &str| {})).expect("state.db"),
    );
    let manifest_rel = "projects/sketchbook/assets/crane/crane.manifest.json";
    let m = Manifest::new(
        "sketchbook",
        "crane",
        "image",
        "00000000-0000-4000-8000-0000000000aa",
    );
    write_file(
        &root.join(manifest_rel.replace('/', "\\")),
        m.to_text().as_bytes(),
    );

    let (attempts_tx, mut attempts) = tokio::sync::mpsc::unbounded_channel();
    let fp = FailPoints::none();
    fp.on(
        "store.replace",
        Box::new(move || {
            let _ = attempts_tx.send(());
            Ok(())
        }),
    );
    let store = Arc::new(
        Store::new(
            root.clone(),
            state,
            fp,
            FakeClock::new(),
            Arc::new(SeqIds::new()),
            Arc::new(SystemProbe),
        )
        .with_machine("testmachine"),
    );
    let handle = store.staging_handle().await.expect("a staging handle");
    let staged = handle.dir.join("a.png");
    write_file(&staged, b"a");
    let edit: ManifestEdit = Arc::new(|m: &mut Manifest| {
        m.push(
            "candidate_runs",
            json!({"run_id": "r_1", "path": "candidates/r_1/", "status": "open",
                   "files": [{"path": "candidates/r_1/a.png", "role": "candidate"}]}),
        )
        .map_err(|e| StoreError::manifest_edit(&p(manifest_rel), &e))
    });
    let op = CommitOp {
        outputs: vec![OutputMove {
            from: staged,
            to: p("projects/sketchbook/assets/crane/candidates/r_1/a.png"),
        }],
        moved: Vec::new(),
        removed: Vec::new(),
        manifest: Some((p(manifest_rel), edit)),
        job_line: None,
        progress: None,
    };

    // A preview pane holds the manifest while the commit runs.
    let holder = HandleHolder::hold(&root.join(manifest_rel.replace('/', "\\")), Share::Read);
    let start = tokio::time::Instant::now();
    let mut commit = {
        let store = store.clone();
        tokio::spawn(async move { store.commit(op).await })
    };
    let mut tries = 0;
    while start.elapsed() < Duration::from_secs(3) {
        tokio::select! {
            got = attempts.recv() => {
                got.expect("the commit keeps trying");
                tries += 1;
            }
            done = &mut commit => panic!("the commit ended while the manifest was held: {done:?}"),
        }
    }
    assert!(tries >= 5, "{tries} attempts in 3 s");
    holder.release();
    let report = commit.await.unwrap().expect("the commit after the release");
    assert!(start.elapsed() < Duration::from_secs(10));
    let manifest = report.manifest.expect("the edited manifest");
    assert_eq!(manifest.view().candidate_runs[0].run_id, "r_1");
    let written = std::fs::read_to_string(root.join(manifest_rel.replace('/', "\\"))).unwrap();
    assert!(written.contains("\"r_1\""), "{written}");
    let mut files = Vec::new();
    files_under(&root, &root, &mut files);
    assert!(
        !files
            .iter()
            .any(|f| f.contains(".lil-tmp-") || f.contains(".lil-bak-")),
        "{files:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Local databases.
// ---------------------------------------------------------------------------------------------

/// Removes a file as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_remove(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("cannot remove {}: {e}", path.display()),
    }
}

/// `path` with `suffix` added to its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn total(pipe: &mut RawPipe) -> u64 {
    let query = pipe.tool_ok("assets.query", json!({}), None);
    query["total"].as_u64().unwrap_or_else(|| panic!("{query}"))
}

#[test] // spec: CORE-005, ARCH-089, QA-055
fn corrupt_or_missing_index_db_is_rebuilt_at_open() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "index");
    let index = env.workspace_local_dir(&ws).join("index.db");
    let expected = {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, &ws);
        total(&mut pipe)
    };
    assert!(expected > 0);
    assert!(index.is_file(), "{} was never created", index.display());

    for damage in ["corrupt", "missing"] {
        for suffix in ["-wal", "-shm"] {
            outside_remove(&beside(&index, suffix));
        }
        if damage == "corrupt" {
            write_file(&index, &[0x5A_u8; 8192]);
        } else {
            outside_remove(&index);
        }
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, &ws);
        assert_eq!(total(&mut pipe), expected, "{damage}");
        let conn = Connection::open_with_flags(&index, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|e| panic!("{damage}: index.db doesn't open: {e}"));
        let check: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(check, "ok", "{damage}");
    }
}

#[test] // spec: CORE-005, QA-055
fn damaged_workspace_state_db_fails_that_workspace_not_the_core() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "damaged");
    let db = env.workspace_local_dir(&ws).join("state.db");
    write_file(&db, &[0xAB_u8; 4096]);
    let before = sha256_file(&db);

    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    let refused = if hello["result"]["status"] == "ok" {
        pipe.tool("assets.query", json!({}), None)
    } else {
        hello["result"].clone()
    };
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(refused["error"]["code"], "state.unavailable", "{refused}");

    // The core still serves: a session without a workspace works.
    let mut other = RawPipe::connect(&env);
    let hello = other.hello(json!({}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let echo = other.tool_ok("test.echo", json!({"text": "Ünïcödé space"}), None);
    assert!(echo.to_string().contains("Ünïcödé space"), "{echo}");
    assert_eq!(sha256_file(&db), before, "the damaged state.db changed");
}

#[test] // spec: CORE-004, QA-062
fn refused_wal_db_gets_no_new_side_files() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    env.write_state_db_version(&db, 999, true);
    for suffix in ["-wal", "-shm"] {
        assert!(!beside(&db, suffix).exists(), "the fixture left {suffix}");
    }
    let before = sha256_file(&db);

    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(core.wait_exit(DEADLINE), Some(6), "a newer state.db");
    for suffix in ["-wal", "-shm"] {
        let side = beside(&db, suffix);
        assert!(
            !side.exists(),
            "the refused start created {}",
            side.display()
        );
    }
    assert_eq!(sha256_file(&db), before);
}

#[test] // spec: CORE-004
fn hot_journal_exits_7_with_nothing_changed() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    env.write_hot_journal_db(&db, lil_schema::SUITE_STATE_SCHEMA);
    let journal = beside(&db, "-journal");
    let (db_before, journal_before) = (sha256_file(&db), sha256_file(&journal));

    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(core.wait_exit(DEADLINE), Some(7), "a hot journal");
    assert_eq!(sha256_file(&db), db_before, "the database changed");
    assert_eq!(sha256_file(&journal), journal_before, "the journal changed");
    for suffix in ["-wal", "-shm"] {
        assert!(!beside(&db, suffix).exists(), "{suffix} appeared");
    }
}

/// `project.status`'s `missing` list.
fn missing(pipe: &mut RawPipe) -> Vec<String> {
    let status = pipe.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    status["missing"]
        .as_array()
        .unwrap_or_else(|| panic!("{status}"))
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect()
}

#[test] // spec: ARCH-079, CORE-038, CORE-005 (ruling Q25)
fn files_gone_while_no_core_ran_are_missing_after_a_cold_rebuild() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "cold missing");
    let sealed = format!("{ASSET_DIR}/v1.0.0/crane_b.png");
    let open_run = format!("{ASSET_DIR}/candidates/r_20261003_1400/crane_c.png");
    let sealed_bytes = std::fs::read(file(&ws, &sealed)).unwrap();
    {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, &ws);
        assert!(missing(&mut pipe).is_empty());
    }
    // Both vanish while no core runs, and so does the index.
    outside_remove(&file(&ws, &sealed));
    outside_remove(&file(&ws, &open_run));
    let index = env.workspace_local_dir(&ws).join("index.db");
    for suffix in ["", "-wal", "-shm"] {
        outside_remove(&beside(&index, suffix));
    }

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    let listed = missing(&mut pipe);
    // The open run's folder is still there (its cutout is), so its gone file counts.
    assert!(listed.contains(&sealed), "{listed:?}");
    assert!(listed.contains(&open_run), "{listed:?}");
    let doctor = workspace_missing(&mut pipe);
    assert!(
        doctor.contains(&sealed) && doctor.contains(&open_run),
        "workspace.missing: {doctor}"
    );
    let detail = pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": ASSET}),
        None,
    );
    let state = |rel: &str| {
        detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == rel)
            .map(|f| f["state"].clone())
    };
    assert_eq!(
        state("v1.0.0/crane_b.png"),
        Some(json!("missing")),
        "{detail}"
    );

    // A read-only core (another channel; an in-memory index, so every open is cold) sees it too.
    let other = TestEnv::with_label("Ünïcödé other");
    other.register_workspace(&ws, true);
    {
        let _reader = CoreFixture::start(&other);
        let mut pipe = session(&other, &ws);
        let listed = missing(&mut pipe);
        assert!(listed.contains(&sealed), "read-only: {listed:?}");
    }

    // The same bytes coming back make it ok again.
    pipe.fence(&ws);
    write_file(&file(&ws, &sealed), &sealed_bytes);
    pipe.fence(&ws);
    let listed = missing(&mut pipe);
    assert!(!listed.contains(&sealed), "{listed:?}");
    assert!(listed.contains(&open_run), "{listed:?}");
}

/// The detail of `workspace.doctor {fix: false}`'s `workspace.missing` check.
fn workspace_missing(pipe: &mut RawPipe) -> String {
    let report = pipe.tool_ok("workspace.doctor", json!({"fix": false}), Some(&key()));
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "workspace.missing")
        .map(|c| {
            format!(
                "{}: {}",
                c["state"].as_str().unwrap(),
                c["detail"].as_str().unwrap()
            )
        })
        .unwrap_or_else(|| panic!("{report}"))
}

/// Removes a folder tree as an outside program (or a fresh clone) would.
#[allow(clippy::disallowed_methods)]
fn outside_remove_tree(path: &Path) {
    std::fs::remove_dir_all(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

#[test] // spec: CORE-032, ARCH-079, CORE-038 (ruling Q28)
fn a_fresh_clone_without_candidates_has_nothing_missing() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "fresh clone");
    // `candidates/` is gitignored: a clone has the manifest's open run but none of its files,
    // and no index yet.
    outside_remove_tree(&file(&ws, &format!("{ASSET_DIR}/candidates")));

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    let listed = missing(&mut pipe);
    assert!(listed.is_empty(), "{listed:?}");
    let doctor = workspace_missing(&mut pipe);
    assert!(doctor.starts_with("ok: "), "workspace.missing: {doctor}");
    let detail = pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": ASSET}),
        None,
    );
    assert!(
        !detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["state"] == "missing"),
        "{detail}"
    );
}

// ---------------------------------------------------------------------------------------------
// Leftovers from before the rename (map row 22, Review Focus 3).
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

/// The names in `dir`, sorted, with each conflict copy's stamp cut off and its marker read as
/// `<conflict>`, so two folders whose leftovers used different markers compare equal.
fn shape_of(dir: &Path) -> Vec<String> {
    use lil_platform::names::{CONFLICT_MARKER, LEGACY_CONFLICT_MARKER};
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .map(|name| {
            for marker in [CONFLICT_MARKER, LEGACY_CONFLICT_MARKER] {
                if let Some(i) = name.find(marker) {
                    let tag = if marker == CONFLICT_MARKER {
                        "new"
                    } else {
                        "legacy"
                    };
                    return format!("{}<conflict {tag}>", &name[..i]);
                }
            }
            name
        })
        .collect();
    names.sort();
    names
}

/// The ARCH-074 crash shapes a dead core leaves in `dir`, with `tmp` and `bak` as markers:
/// a backup whose target is gone (restored), a backup equal to its target (removed), a backup
/// of an outside save (moved aside to a visible conflict copy) and a temp file of a dead
/// process (removed).
fn crash_shapes(dir: &Path, tmp: &str, bak: &str, dead: u32) {
    write_file(
        &dir.join(format!(".a.txt{bak}{dead}-1")),
        b"a, the only copy",
    );
    write_file(&dir.join("b.txt"), b"b");
    write_file(&dir.join(format!(".b.txt{bak}{dead}-1")), b"b");
    write_file(&dir.join("c.txt"), b"c, the core's");
    write_file(
        &dir.join(format!(".c.txt{bak}{dead}-1")),
        b"c, an outside save",
    );
    write_file(&dir.join(format!(".d.txt{tmp}{dead}-1")), b"half a write");
}

#[test] // spec: QA-055, ARCH-074, CORE-041
fn recovery_restores_legacy_leftovers_and_the_index_skips_them() {
    use lil_platform::names::{BAK_MARKER, LEGACY_BAK_MARKER, LEGACY_TMP_MARKER, TMP_MARKER};
    let env = TestEnv::new();
    let ws = default_workspace(&env, "legacy-leftovers");
    let (dead, _child) = exited_pid();
    let work = file(&ws, &format!("{ASSET_DIR}/work"));
    let (new, old) = (work.join("new"), work.join("old"));
    crash_shapes(&new, TMP_MARKER, BAK_MARKER, dead);
    crash_shapes(&old, LEGACY_TMP_MARKER, LEGACY_BAK_MARKER, dead);

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    // A call waits for the workspace's open (begun in the background at start), which recovers.
    pipe.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    // The open recovered both folders; the legacy one exactly as its `lil` twin.
    let want = ["a.txt", "b.txt", "c.txt", "c.txt<conflict new>"];
    assert_eq!(shape_of(&new), want);
    assert_eq!(
        shape_of(&old),
        want,
        "the legacy leftovers were not resolved"
    );
    for dir in [&new, &old] {
        assert_eq!(
            std::fs::read(dir.join("a.txt")).unwrap(),
            b"a, the only copy"
        );
        assert_eq!(std::fs::read(dir.join("c.txt")).unwrap(), b"c, the core's");
    }

    // The index never lists a leftover or a conflict copy.
    let detail = pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": ASSET}),
        None,
    );
    let files: Vec<&str> = detail["files"]
        .as_array()
        .unwrap_or_else(|| panic!("{detail}"))
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"work/old/a.txt"), "{files:?}");
    for path in &files {
        assert!(
            !path.contains("-tmp-") && !path.contains("-bak-") && !path.contains("-conflict-"),
            "{path} is indexed: {files:?}"
        );
    }
    assert_consistent(&ws, &mut pipe, "after recovering legacy leftovers");
}

#[test] // spec: CORE-038, CORE-041
fn legacy_conflict_copies_are_left_alone_and_not_indexed() {
    use lil_platform::names::LEGACY_CONFLICT_MARKER;
    let env = TestEnv::new();
    let ws = default_workspace(&env, "legacy-conflicts");
    let name = format!("notes.txt{LEGACY_CONFLICT_MARKER}20261001T100000Z");
    let copy = file(&ws, &format!("{ASSET_DIR}/work/{name}"));
    write_file(&copy, b"an outside save, set aside before the rename");
    let hash = sha256_file(&copy);

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    // Listed as an orphan, like a new conflict copy, and never moved or changed.
    assert!(
        orphans(&mut pipe).contains(&format!("{ASSET_DIR}/work/{name}")),
        "{:?}",
        orphans(&mut pipe)
    );
    assert_eq!(sha256_file(&copy), hash);
    let detail = pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": ASSET}),
        None,
    );
    let files = detail["files"]
        .as_array()
        .unwrap_or_else(|| panic!("{detail}"));
    assert!(
        files
            .iter()
            .all(|f| !f["path"].as_str().unwrap().contains(&name)),
        "{detail}"
    );
    drop(pipe);
    drop(_core);
    assert_eq!(
        sha256_file(&copy),
        hash,
        "the copy changed after the core stopped"
    );
}

/// A real core killed right after an engine job's `submitting` is persisted (fail point
/// `jobs.after_submitting`, CORE-120): the next core marks the job `interrupted`, or
/// `interrupted_unknown` when it has external side effects, and its handler never runs again
/// (ARCH-022, QA-007, M8 of the Task 8 review).
#[test] // spec: ARCH-022, QA-007, QA-055, CORE-120
fn crash_after_submitting_interrupts_the_job_and_never_reruns_it() {
    for (external, status, substatus) in [
        (false, "failed", "interrupted"),
        (true, "running", "interrupted_unknown"),
    ] {
        let env = TestEnv::new();
        let k = key();
        {
            let spec = failpoints::spec(&[("jobs.after_submitting", "abort")]);
            let mut core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &spec)]);
            let mut pipe = RawPipe::connect(&env);
            assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
            let request = json!({
                "jsonrpc": "2.0",
                "id": 9_000_002,
                "method": "tools/call",
                "params": {
                    "name": "test.job",
                    "arguments": {
                        "lane": "gpu-heavy",
                        "engine": "comfyui",
                        "external_side_effects": external,
                    },
                    "idempotency_key": k,
                },
            });
            pipe.send_line(&request.to_string());
            // The call may be answered before the job reaches the fail point; the core dies
            // either way.
            let _ = pipe.read_line(DEADLINE);
            assert_eq!(
                core.wait_exit(DEADLINE),
                Some(ABORT_EXIT),
                "the core didn't end at jobs.after_submitting"
            );
        }
        let _core = CoreFixture::start(&env);
        let mut pipe = RawPipe::connect(&env);
        assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
        let listed = pipe.tool_ok("jobs.list", json!({}), None);
        let jobs = listed["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 1, "nothing ran again as a new job: {listed}");
        let job = &jobs[0];
        assert_eq!(
            (job["status"].as_str(), job["substatus"].as_str()),
            (Some(status), Some(substatus)),
            "external {external}: {job}"
        );
        assert_eq!(job["attempt"], 1, "{job}");
        assert!(
            job.get("result").is_none(),
            "the handler never finished: {job}"
        );
    }
}
