//! The outside-writers reconciler on real NTFS (QA-046; ARCH-079, ARCH-083, ARCH-085,
//! CORE-034, CORE-038): the scenarios of `reconcile_sim.rs` acted out by real outside writers
//! and `GitOps` against a real `lil-core`, in temporary folders only.
//!
//! Every scenario ends with the watcher fence (`RawPipe::fence`, never a sleep); then the
//! core's view (`assets.query`, `asset.get`) is compared with the files on disk
//! ([`assert_view_matches_disk`]), and the scenario's own invariant is checked: no outside
//! edit overwritten, invalid manifests kept as they are, no human edit from a locked read or
//! the core's own bytes or twice for one SHA-256, expected files that vanish are `missing`.
//!
//! Two operations of the simulation can't be timed from outside a real core and are covered
//! there and by the store's own tests: lost watcher events (a buffer overflow) and an outside
//! edit landing between the store's hash check and its replace (`store_write.rs`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lil_platform::fs::Share;
use lil_testkit::files::write_file;
use lil_testkit::{
    CoreFixture, GitOps, HandleHolder, RawPipe, TestEnv, failpoints, sha256_bytes, sha256_file,
};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
const CRANE: &str = "chr_paper_crane";
const CRANE_DIR: &str = "projects/sketchbook/assets/chr_paper_crane";
/// The open run's candidate files, asset-relative.
const OPEN_C: &str = "candidates/r_20261003_1400/crane_c.png";
const OPEN_CUTOUT: &str = "candidates/r_20261003_1400/crane_c_cutout.png";
/// The sealed file, asset-relative.
const SEALED: &str = "v1.0.0/crane_b.png";

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "ntfs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A copy of the fixture workspace, registered as the default.
fn default_workspace(env: &TestEnv, name: &str) -> PathBuf {
    let ws = env.copy_fixture_workspace(name);
    env.register_workspace(&ws, true);
    ws
}

fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

/// The crane's file `rel` (asset-relative).
fn crane(ws: &Path, rel: &str) -> PathBuf {
    ws.join(format!("{CRANE_DIR}/{rel}").replace('/', "\\"))
}

/// Writes `bytes` as an outside program would, clearing a read-only bit first.
#[allow(clippy::disallowed_methods)]
fn outside_write(path: &Path, bytes: &[u8]) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut permissions = meta.permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as an editor would"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).unwrap();
    }
    write_file(path, bytes);
}

#[allow(clippy::disallowed_methods)]
fn outside_remove(path: &Path) {
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

#[allow(clippy::disallowed_methods)]
fn outside_move(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(from, to)
        .unwrap_or_else(|e| panic!("{} → {}: {e}", from.display(), to.display()));
}

/// The next `ww/event` on `topic` whose `data.kind` is `kind`; other events are skipped.
fn wait_for_kind(pipe: &mut RawPipe, topic: &str, kind: &str) -> Value {
    loop {
        let event = pipe
            .next_event(DEADLINE)
            .unwrap_or_else(|| panic!("no {topic} event of kind {kind} within {DEADLINE:?}"));
        if event["topic"] == topic && event["data"]["kind"] == kind {
            return event;
        }
    }
}

fn asset_get(pipe: &mut RawPipe, asset_id: &str) -> Value {
    pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": asset_id}),
        None,
    )
}

/// The state `asset.get` gives the asset-relative `rel`.
fn state_of(detail: &Value, rel: &str) -> Option<String> {
    detail["files"]
        .as_array()?
        .iter()
        .find(|f| f["path"] == rel)
        .and_then(|f| f["state"].as_str().map(str::to_string))
}

/// The `human_edits` SHA-256 values recorded for `rel`, in order.
fn edits_of(detail: &Value, rel: &str) -> Vec<String> {
    detail["human_edits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["path"] == rel)
        .filter_map(|e| e["sha256"].as_str().map(str::to_string))
        .collect()
}

/// The SHA-256 a file stands for: an LFS pointer's `oid`, else its bytes' (ARCH-085).
fn content_hash(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let text = String::from_utf8_lossy(&bytes);
    if text.starts_with("version https://git-lfs")
        && let Some(oid) = text.lines().find_map(|l| l.strip_prefix("oid sha256:"))
    {
        return oid.trim().to_string();
    }
    sha256_bytes(&bytes)
}

/// The core's view agrees with the disk: every asset `assets.query` lists answers `asset.get`
/// (or is marked as needing repair when its manifest doesn't parse), and every file it lists is
/// on disk unless it is `missing`; a `not_fetched` file is an LFS pointer; a `human_edit` file
/// holds the last recorded edit's bytes; no SHA-256 is recorded twice in a row for one file.
fn assert_view_matches_disk(ws: &Path, pipe: &mut RawPipe, what: &str) {
    let query = pipe.tool_ok("assets.query", json!({"limit": 1000}), None);
    for asset in query["assets"].as_array().expect("assets") {
        let (project, id) = (
            asset["project"].as_str().unwrap(),
            asset["asset_id"].as_str().unwrap(),
        );
        let dir = ws.join(format!("projects/{project}/assets/{id}"));
        let manifest = dir.join(format!("{id}.manifest.json"));
        let parses = std::fs::read_to_string(&manifest)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some();
        if !parses {
            assert_eq!(asset["needs_repair"], true, "{what}: {id}: {asset}");
            continue;
        }
        let detail = asset_get(pipe, id);
        for entry in detail["files"].as_array().expect("files") {
            let rel = entry["path"].as_str().unwrap();
            let path = dir.join(rel.replace('/', "\\"));
            let state = entry["state"].as_str().unwrap();
            match state {
                "missing" => assert!(!path.exists(), "{what}: {id}/{rel} is missing yet on disk"),
                "not_fetched" => {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    assert!(
                        text.starts_with("version https://git-lfs"),
                        "{what}: {id}/{rel} isn't a pointer"
                    );
                }
                "human_edit" => {
                    let edits = edits_of(&detail, rel);
                    let last = edits
                        .last()
                        .unwrap_or_else(|| panic!("{what}: {rel}: no edit"));
                    assert_eq!(&content_hash(&path), last, "{what}: {id}/{rel}");
                }
                _ => assert!(path.exists(), "{what}: {id}/{rel} ({state}) is not on disk"),
            }
            let edits = edits_of(&detail, rel);
            for pair in edits.windows(2) {
                assert_ne!(
                    pair[0], pair[1],
                    "{what}: {id}/{rel} recorded twice: {detail}"
                );
            }
        }
    }
}

#[test] // spec: QA-046, CORE-034, ARCH-079
fn outside_edits_slow_writes_and_touches_record_each_edit_once() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "edits");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    // An outside edit of an open run's file.
    let first = "Ünïcödé outside edit".as_bytes();
    outside_write(&crane(&ws, OPEN_C), first);
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(edits_of(&detail, OPEN_C), [sha256_bytes(first)], "{detail}");
    assert_eq!(state_of(&detail, OPEN_C).as_deref(), Some("human_edit"));

    // The same bytes again: no second record.
    outside_write(&crane(&ws, OPEN_C), first);
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(edits_of(&detail, OPEN_C), [sha256_bytes(first)], "{detail}");

    // A slow write: half of it lands while the writer still holds the file; that read is
    // parked, never a human edit (read from the manifest on disk: `asset.get` would read the
    // held file too). Then the write ends.
    let sealed = crane(&ws, SEALED);
    outside_write(&sealed, b"half");
    let held = HandleHolder::hold(&sealed, Share::None);
    pipe.fence(&ws);
    let on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(crane(&ws, &format!("{CRANE}.manifest.json"))).unwrap(),
    )
    .unwrap();
    assert!(
        !edits_of(&on_disk, SEALED).contains(&sha256_bytes(b"half")),
        "a locked read became a human edit: {on_disk}"
    );
    held.release();
    let whole = b"the whole slow write";
    outside_write(&sealed, whole);
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(
        edits_of(&detail, SEALED).last(),
        Some(&sha256_bytes(whole)),
        "{detail}"
    );
    assert_eq!(
        std::fs::read(&sealed).unwrap(),
        whole,
        "the edit was overwritten"
    );
    assert_view_matches_disk(&ws, &mut pipe, "edits");
}

#[test] // spec: QA-046, ARCH-079, CORE-038
fn deletes_renames_and_folder_moves_leave_missing_files() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "removed");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    outside_remove(&crane(&ws, SEALED));
    outside_move(
        &crane(&ws, OPEN_C),
        &crane(&ws, "candidates/r_20261003_1400/crane_c_renamed.png"),
    );
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    for rel in [SEALED, OPEN_C] {
        assert_eq!(
            state_of(&detail, rel).as_deref(),
            Some("missing"),
            "{rel}: {detail}"
        );
    }
    assert_view_matches_disk(&ws, &mut pipe, "delete and rename");

    // A folder move: the open run's whole folder goes elsewhere.
    outside_move(
        &crane(&ws, "candidates/r_20261003_1400"),
        &ws.join("inbox").join("r_20261003_1400"),
    );
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(
        state_of(&detail, OPEN_CUTOUT).as_deref(),
        Some("missing"),
        "{detail}"
    );
    let status = pipe.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    let missing: Vec<&str> = status["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        missing.contains(&format!("{CRANE_DIR}/{OPEN_CUTOUT}").as_str()),
        "{status}"
    );
    assert_view_matches_disk(&ws, &mut pipe, "folder move");

    // The same bytes coming back make the file ok again.
    outside_move(
        &ws.join("inbox").join("r_20261003_1400"),
        &crane(&ws, "candidates/r_20261003_1400"),
    );
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(
        state_of(&detail, OPEN_CUTOUT).as_deref(),
        Some("ok"),
        "{detail}"
    );
    assert_view_matches_disk(&ws, &mut pipe, "moved back");
}

#[test] // spec: QA-046, ARCH-076, ARCH-079
fn partial_and_invalid_manifest_edits_are_kept() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "manifests");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    let manifest = crane(&ws, &format!("{CRANE}.manifest.json"));
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["note"] = json!("Ünïcödé space, added by hand");
    let partial = format!("{}\n", serde_json::to_string_pretty(&m).unwrap());
    outside_write(&manifest, partial.as_bytes());
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(detail["needs_repair"], false, "{detail}");
    assert_eq!(
        std::fs::read_to_string(&manifest).unwrap(),
        partial,
        "the edit was rewritten"
    );
    assert_view_matches_disk(&ws, &mut pipe, "partial manifest edit");

    let lamp = ws.join(
        "projects/sketchbook/assets/prop_signal_lamp/prop_signal_lamp.manifest.json"
            .replace('/', "\\"),
    );
    outside_write(&lamp, b"{ \"asset_id\": ");
    pipe.fence(&ws);
    let query = pipe.tool_ok("assets.query", json!({}), None);
    let lamp_row = query["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["asset_id"] == "prop_signal_lamp")
        .cloned()
        .unwrap_or_else(|| panic!("{query}"));
    assert_eq!(lamp_row["needs_repair"], true, "{lamp_row}");
    // A core write that would touch it is refused, so it is never rewritten.
    let refused = pipe.tool(
        "test.ingest_run",
        json!({"project": "sketchbook", "asset_id": "prop_signal_lamp",
               "files": [{"name": "x.png", "text": "x"}], "job_line": false}),
        Some(&key()),
    );
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(std::fs::read(&lamp).unwrap(), b"{ \"asset_id\": ");
    assert_view_matches_disk(&ws, &mut pipe, "invalid manifest");
}

#[test] // spec: QA-046, ARCH-083, CORE-034
fn a_checkout_is_reconciled_after_git_and_records_only_real_edits() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "checkout");
    env.git_init(&ws);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);
    let subscribed = pipe.call(
        "ww/subscribe",
        json!({"topics": ["workspace"], "since_seq": {}}),
    );
    assert_eq!(subscribed["result"]["status"], "ok", "{subscribed}");

    // A checkout of three files: the sealed file (its manifest hash stays, so a human edit),
    // and an open run's candidate whose run record changes in the same checkout (not an edit).
    let ops = GitOps::begin(&ws);
    let sealed_bytes = b"sealed bytes from another branch";
    let candidate_bytes = "candidate from another branch, Ünïcödé".as_bytes();
    ops.write(&format!("{CRANE_DIR}/{SEALED}"), sealed_bytes);
    ops.write(&format!("{CRANE_DIR}/{OPEN_C}"), candidate_bytes);
    let manifest_rel = format!("{CRANE_DIR}/{CRANE}.manifest.json");
    let mut m: Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join(manifest_rel.replace('/', "\\"))).unwrap(),
    )
    .unwrap();
    m["candidate_runs"][1]["files"][0]["sha256"] = json!(sha256_bytes(candidate_bytes));
    ops.write(
        &manifest_rel,
        format!("{}\n", serde_json::to_string_pretty(&m).unwrap()).as_bytes(),
    );
    // While git works, the watcher only defers (the fence still returns).
    pipe.fence(&ws);
    ops.finish();
    // The rescan after git, then everything after it.
    wait_for_kind(&mut pipe, "workspace", "git_resumed");
    pipe.fence(&ws);

    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(
        edits_of(&detail, SEALED),
        [sha256_bytes(sealed_bytes)],
        "{detail}"
    );
    assert!(
        edits_of(&detail, OPEN_C).is_empty(),
        "the checkout's own record: {detail}"
    );
    assert_eq!(state_of(&detail, OPEN_C).as_deref(), Some("ok"), "{detail}");
    assert_eq!(std::fs::read(crane(&ws, SEALED)).unwrap(), sealed_bytes);
    assert_view_matches_disk(&ws, &mut pipe, "checkout");
}

#[test] // spec: QA-046, ARCH-085
fn lfs_pointers_are_not_fetched_or_edits_by_their_oid() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "lfs");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    let pointer = |oid: &str| {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 42\n")
    };
    let recorded = sha256_file(&crane(&ws, OPEN_CUTOUT));
    outside_write(&crane(&ws, OPEN_CUTOUT), pointer(&recorded).as_bytes());
    let other = sha256_bytes(b"other bytes behind the pointer");
    outside_write(&crane(&ws, OPEN_C), pointer(&other).as_bytes());
    pipe.fence(&ws);

    let detail = asset_get(&mut pipe, CRANE);
    assert_eq!(
        state_of(&detail, OPEN_CUTOUT).as_deref(),
        Some("not_fetched"),
        "{detail}"
    );
    assert!(edits_of(&detail, OPEN_CUTOUT).is_empty(), "{detail}");
    assert_eq!(
        edits_of(&detail, OPEN_C),
        std::slice::from_ref(&other),
        "the pointer's oid: {detail}"
    );
    assert_view_matches_disk(&ws, &mut pipe, "lfs");
}

#[test] // spec: QA-046, ARCH-074, ARCH-078
fn core_writes_and_a_crash_after_replace_are_never_human_edits() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "core writes");
    let rel = format!("{CRANE_DIR}/{OPEN_C}");
    {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, &ws);
        pipe.fence(&ws);
        // A core write over the recorded bytes, then an outside edit the next core write must
        // not overwrite.
        let current = sha256_file(&crane(&ws, OPEN_C));
        pipe.tool_ok(
            "test.store_write",
            json!({"path": rel, "text": "the core's bytes", "expect": current}),
            Some(&key()),
        );
        pipe.fence(&ws);
        let detail = asset_get(&mut pipe, CRANE);
        assert!(
            edits_of(&detail, OPEN_C).is_empty(),
            "the core's own bytes: {detail}"
        );

        outside_write(&crane(&ws, OPEN_C), b"edited after the core wrote");
        let refused = pipe.tool(
            "test.store_write",
            json!({"path": rel, "text": "the core again", "expect": "generated"}),
            Some(&key()),
        );
        assert_eq!(refused["error"]["code"], "store.hash_conflict", "{refused}");
        assert_eq!(
            std::fs::read(crane(&ws, OPEN_C)).unwrap(),
            b"edited after the core wrote"
        );
        pipe.fence(&ws);
        assert_view_matches_disk(&ws, &mut pipe, "core writes");
    }

    // A crash right after the replace of the cutout: the next core accepts its own bytes.
    let cutout = format!("{CRANE_DIR}/{OPEN_CUTOUT}");
    let current = sha256_file(&crane(&ws, OPEN_CUTOUT));
    {
        let spec = failpoints::spec(&[("store.after_replace", "abort")]);
        let mut core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &spec)]);
        let mut pipe = session(&env, &ws);
        let request = json!({
            "jsonrpc": "2.0", "id": 9_000_002, "method": "tools/call",
            "params": {"name": "test.store_write",
                       "arguments": {"path": cutout, "text": "crashed core bytes", "expect": current},
                       "idempotency_key": key()},
        });
        pipe.send_line(&request.to_string());
        assert!(pipe.read_line(DEADLINE).is_none(), "the core answered");
        assert!(core.wait_exit(DEADLINE).is_some(), "the core didn't stop");
    }
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    assert!(
        edits_of(&detail, OPEN_CUTOUT).is_empty(),
        "the crashed core's own bytes became a human edit: {detail}"
    );
    assert_eq!(
        std::fs::read(crane(&ws, OPEN_CUTOUT)).unwrap(),
        b"crashed core bytes"
    );
    assert_view_matches_disk(&ws, &mut pipe, "crash after replace");
}

#[test] // spec: QA-046, ARCH-075, CORE-038
fn a_crash_at_a_write_order_step_keeps_orphans_and_the_view() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = default_workspace(&env, "crash step");
    {
        let spec = failpoints::spec(&[("store.after_manifest", "abort")]);
        let mut core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &spec)]);
        let mut pipe = session(&env, &ws);
        let request = json!({
            "jsonrpc": "2.0", "id": 9_000_003, "method": "tools/call",
            "params": {"name": "test.ingest_run",
                       "arguments": {"project": "sketchbook", "asset_id": CRANE,
                                     "files": [{"name": "n.png", "text": "new run"}],
                                     "job_line": true},
                       "idempotency_key": key()},
        });
        pipe.send_line(&request.to_string());
        assert!(pipe.read_line(DEADLINE).is_none(), "the core answered");
        assert!(core.wait_exit(DEADLINE).is_some(), "the core didn't stop");
    }
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);
    let detail = asset_get(&mut pipe, CRANE);
    let runs: Vec<&str> = detail["candidate_runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["run_id"].as_str())
        .collect();
    assert_eq!(
        runs.len(),
        3,
        "the manifest edit landed before the crash: {detail}"
    );
    let new_run = runs[2];
    let rel = format!("candidates/{new_run}/n.png");
    assert_eq!(state_of(&detail, &rel).as_deref(), Some("ok"), "{detail}");
    assert!(edits_of(&detail, &rel).is_empty(), "{detail}");
    assert_view_matches_disk(&ws, &mut pipe, "crash after the manifest");
}
