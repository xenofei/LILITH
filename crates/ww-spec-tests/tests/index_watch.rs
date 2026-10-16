//! The index and the watcher: a damaged `index.db` is rebuilt, outside edits to sealed files
//! are human edits, invalid manifests are marked and kept, writes wait while git holds
//! `index.lock`, LFS pointers are `not_fetched`, and `ww notify` (spec ARCH-016, ARCH-073,
//! ARCH-076, ARCH-079, ARCH-083, ARCH-085, ARCH-089, CORE-034; Appendix E). Every watcher
//! assertion follows a `RawPipe::fence`, never a sleep (Global Constraints, Tests). Driven
//! through the read tools, `ww notify` and the workspace files (QA-018).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use ww_testkit::{CoreFixture, RawPipe, TestEnv, failpoints, sha256_bytes};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
/// The fixture asset with a sealed `v1.0.0/`.
const CRANE: &str = "chr_paper_crane";
/// Its sealed file and the SHA-256 the manifest records for it.
const SEALED: &str = "projects/sketchbook/assets/chr_paper_crane/v1.0.0/crane_b.png";
const SEALED_SHA256: &str = "aaae4dbdb0e697350dac691b26928d34a69061cf320b175c257c103fcaa32766";

/// A fresh idempotency key for every write call (ARCH-035).
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-iw-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A copy of the fixture workspace, registered as this channel's default.
fn default_workspace(env: &TestEnv) -> PathBuf {
    let ws = env.copy_fixture_workspace("watch");
    env.register_workspace(&ws, true);
    ws
}

/// A connection whose hello names the workspace `ws`, checked to succeed.
fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(
        hello["result"]["status"],
        "ok",
        "hello naming {}: {hello}",
        ws.display()
    );
    pipe
}

/// The file at the workspace-relative `rel` (with `/` separators).
fn file(ws: &Path, rel: &str) -> PathBuf {
    ws.join(rel.replace('/', "\\"))
}

/// Replaces a sealed file's bytes as an outside writer would: clears its read-only bit first.
fn edit_outside(path: &Path, bytes: &[u8]) {
    let mut permissions = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as Explorer or an editor would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|e| panic!("cannot clear the read-only bit of {}: {e}", path.display()));
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// `asset.get` of a sketchbook asset.
fn asset_get(pipe: &mut RawPipe, asset_id: &str) -> Value {
    pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": asset_id}),
        None,
    )
}

/// The sealed crane file as `human_edits` names it: relative to the asset folder (I5,
/// `docs/reference/workspace-format.md`).
const SEALED_IN_ASSET: &str = "v1.0.0/crane_b.png";

/// The `human_edits` entries of an `asset.get` result whose path is the sealed crane file.
/// Any entry for another spelling of that file fails the test.
fn crane_edits(detail: &Value) -> Vec<Value> {
    let edits = detail["human_edits"]
        .as_array()
        .unwrap_or_else(|| panic!("asset.get has no human_edits list: {detail}"));
    for edit in edits {
        let path = edit["path"].as_str().unwrap_or_default();
        assert!(
            path == SEALED_IN_ASSET || !path.ends_with("crane_b.png"),
            "human_edits names {path:?}, not the asset-relative {SEALED_IN_ASSET:?}: {detail}"
        );
    }
    edits
        .iter()
        .filter(|e| e["path"] == SEALED_IN_ASSET)
        .cloned()
        .collect()
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

#[test] // spec: ARCH-089
fn damaged_index_is_rebuilt() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let index_db = env.workspace_local_dir(&ws).join("index.db");

    {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, &ws);
        let first = pipe.tool_ok("assets.query", json!({}), None);
        assert_eq!(first["total"], 2, "{first}");
    }
    assert!(
        index_db.is_file(),
        "the core keeps its index at {}",
        index_db.display()
    );
    std::fs::write(&index_db, vec![0xAB_u8; 4096])
        .unwrap_or_else(|e| panic!("cannot damage {}: {e}", index_db.display()));

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    let page = pipe.tool_ok("assets.query", json!({}), None);
    let mut ids: Vec<&str> = page["assets"]
        .as_array()
        .unwrap_or_else(|| panic!("assets.query has no assets list: {page}"))
        .iter()
        .filter_map(|a| a["asset_id"].as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [CRANE, "prop_signal_lamp"], "{page}");
    assert_eq!(page["total"], 2, "{page}");
    let bytes = std::fs::read(&index_db).unwrap_or_default();
    assert!(
        bytes.iter().any(|&b| b != 0xAB),
        "{} still holds the damaged bytes",
        index_db.display()
    );
}

#[test] // spec: CORE-034, ARCH-079
fn outside_edit_to_sealed_file_is_a_human_edit() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    let sealed = file(&ws, SEALED);
    let edited = b"an outside edit of the sealed crane";
    edit_outside(&sealed, edited);
    pipe.fence(&ws);

    let detail = asset_get(&mut pipe, CRANE);
    let edits = crane_edits(&detail);
    assert_eq!(edits.len(), 1, "one human edit of crane_b.png: {detail}");
    assert_eq!(edits[0]["sha256"], json!(sha256_bytes(edited)), "{detail}");
    assert_eq!(edits[0]["previous_sha256"], SEALED_SHA256, "{detail}");
    assert_eq!(
        std::fs::read(&sealed).unwrap(),
        edited,
        "a human edit is never overwritten"
    );
}

#[test] // spec: ARCH-079, ARCH-076
fn invalid_manifest_is_marked_needs_repair_and_kept() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    let manifest = file(
        &ws,
        "projects/sketchbook/assets/prop_signal_lamp/prop_signal_lamp.manifest.json",
    );
    std::fs::write(&manifest, b"{").unwrap();
    pipe.fence(&ws);

    let detail = asset_get(&mut pipe, "prop_signal_lamp");
    assert_eq!(detail["needs_repair"], true, "{detail}");
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        b"{",
        "an invalid manifest is never rewritten"
    );
}

#[test] // spec: ARCH-083
fn writes_wait_while_git_holds_index_lock() {
    /// The JSON-RPC id of the write that waits; far above `RawPipe`'s own ids.
    const WRITE_ID: u64 = 9_000_001;

    let env = TestEnv::new();
    let ws = default_workspace(&env);
    env.git_init(&ws);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    let subscribed = pipe.call(
        "ww/subscribe",
        json!({"topics": ["workspace"], "since_seq": {}}),
    );
    assert_eq!(subscribed["result"]["status"], "ok", "{subscribed}");

    let lock = ws.join(".git").join("index.lock");
    std::fs::write(&lock, b"").unwrap_or_else(|e| panic!("{}: {e}", lock.display()));
    pipe.fence(&ws);
    wait_for_kind(&mut pipe, "workspace", "git_paused");

    let request = json!({
        "jsonrpc": "2.0",
        "id": WRITE_ID,
        "method": "tools/call",
        "params": {
            "name": "test.store_write",
            "arguments": {"path": "notes/after_git.txt", "text": "after git", "expect": "new"},
            "idempotency_key": key(),
        },
    });
    pipe.send_line(&request.to_string());
    std::fs::remove_file(&lock).unwrap_or_else(|e| panic!("{}: {e}", lock.display()));

    let mut resumed = false;
    loop {
        let line = pipe
            .read_line(DEADLINE)
            .unwrap_or_else(|| panic!("no response to the waiting write within {DEADLINE:?}"));
        let message: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("the core sent a line that is not JSON ({e}): {line}"));
        if message["method"] == "ww/event" {
            let params = &message["params"];
            if params["topic"] == "workspace" && params["data"]["kind"] == "git_resumed" {
                resumed = true;
            }
            continue;
        }
        if message["id"] == json!(WRITE_ID) {
            assert!(
                resumed,
                "the write was answered before git_resumed was published: {message}"
            );
            assert_eq!(message["result"]["status"], "ok", "{message}");
            break;
        }
    }
    assert_eq!(
        std::fs::read(file(&ws, "notes/after_git.txt")).unwrap(),
        b"after git"
    );
}

#[test] // spec: ARCH-085
fn lfs_pointer_is_not_fetched_not_corrupt() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    let sealed = file(&ws, SEALED);
    let size = std::fs::metadata(&sealed).unwrap().len();
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{SEALED_SHA256}\nsize {size}\n"
    );
    edit_outside(&sealed, pointer.as_bytes());
    pipe.fence(&ws);

    let detail = asset_get(&mut pipe, CRANE);
    let entry = detail["files"]
        .as_array()
        .unwrap_or_else(|| panic!("asset.get has no files list: {detail}"))
        .iter()
        .find(|f| {
            f["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("v1.0.0/crane_b.png"))
        })
        .unwrap_or_else(|| panic!("asset.get lists no v1.0.0/crane_b.png: {detail}"));
    assert_eq!(entry["state"], "not_fetched", "{detail}");
    assert!(
        crane_edits(&detail).is_empty(),
        "an unfetched LFS pointer is not a human edit: {detail}"
    );
}

#[test] // spec: ARCH-016, ARCH-073
fn ww_notify_reconciles_and_is_silent_without_core() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let sealed = file(&ws, SEALED);
    let sealed_arg = sealed.to_str().expect("a UTF-8 path").to_string();

    let quiet = env.ww(&["notify", "--no-start", &sealed_arg]);
    assert_eq!(quiet.code, 0, "{quiet:?}");
    assert_eq!(quiet.stdout, "", "{quiet:?}");
    assert_eq!(quiet.stderr, "", "{quiet:?}");

    // Without a watcher only `ww notify` can find the edit. `asset.get` answers only after the
    // first reconcile at open, so that reconcile ran before the edit and cannot record it.
    let points = failpoints::spec(&[("watch.start", "unsupported")]);
    let _core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &points)]);
    let mut pipe = session(&env, &ws);
    let before = asset_get(&mut pipe, CRANE);
    assert!(crane_edits(&before).is_empty(), "{before}");

    let edited = b"edited, then announced with ww notify";
    edit_outside(&sealed, edited);
    let notified = env.ww(&["notify", &sealed_arg]);
    assert_eq!(notified.code, 0, "{notified:?}");
    assert_eq!(
        notified.stdout, "",
        "`ww notify` prints nothing on success: {notified:?}"
    );

    let detail = asset_get(&mut pipe, CRANE);
    let edits = crane_edits(&detail);
    assert_eq!(edits.len(), 1, "{detail}");
    assert_eq!(edits[0]["sha256"], json!(sha256_bytes(edited)), "{detail}");
}
