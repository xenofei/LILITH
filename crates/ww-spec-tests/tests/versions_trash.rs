//! Sealed versions and trash: sign-off seals a read-only version with its snapshot, the
//! confirmed bump, refs that carry the content hash, link-external assets that must be
//! collected first, and trash entries with restore records (spec ARCH-042, ARCH-058, CORE-026,
//! CORE-027, CORE-029, CORE-031, CORE-037, CORE-047, CORE-051, CORE-110; Appendix E "Refs",
//! "Workspace areas"). Driven through the asset and trash tools on a real core and the files
//! on disk (QA-018).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use ww_testkit::{CoreFixture, RawPipe, TestEnv, utc, version_content_hash_of};

/// The new asset these tests import; the fixture has no asset of that id.
const ASSET: &str = "prop_kite";
/// Its folder, workspace-relative.
const ASSET_DIR: &str = "projects/sketchbook/assets/prop_kite";

/// A fresh idempotency key for every write call (ARCH-035).
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-vt-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A served fixture workspace: a session, its core, the workspace and the env. Fields drop in
/// this order, so the connection closes before the core is drained and the env goes last.
struct Served {
    pipe: RawPipe,
    _core: CoreFixture,
    ws: PathBuf,
    env: TestEnv,
}

impl Served {
    fn start() -> Served {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("versions");
        env.register_workspace(&ws, true);
        let core = CoreFixture::start(&env);
        let mut pipe = RawPipe::connect(&env);
        let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
        assert_eq!(hello["result"]["status"], "ok", "{hello}");
        Served {
            pipe,
            _core: core,
            ws,
            env,
        }
    }

    /// A write tool call with a fresh key; returns the `Outcome`.
    fn write(&mut self, tool: &str, args: Value) -> Value {
        self.pipe.tool(tool, args, Some(&key()))
    }

    /// A write tool call that must succeed; returns its `structured` result.
    fn write_ok(&mut self, tool: &str, args: Value) -> Value {
        self.pipe.tool_ok(tool, args, Some(&key()))
    }

    /// A file outside the workspace, for `asset.import`.
    fn outside_file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.env.root().join("import-src").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        path
    }

    /// `asset.import` (copy) of `name` into the new asset; returns the run id.
    fn import(&mut self, name: &str, bytes: &[u8]) -> String {
        let source = self.outside_file(name, bytes);
        let result = self.write_ok(
            "asset.import",
            json!({
                "project": "sketchbook",
                "asset_id": ASSET,
                "type": "image",
                "files": [source.to_str().unwrap()],
                "mode": "copy",
            }),
        );
        result["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("asset.import returned no run_id: {result}"))
            .to_string()
    }

    /// `asset.signoff` of the file `name` of run `run_id`, with `bump` when given.
    fn signoff(&mut self, run_id: &str, name: &str, bump: Option<&str>) -> Value {
        let mut args = json!({
            "project": "sketchbook",
            "asset_id": ASSET,
            "run_id": run_id,
            "files": [name],
        });
        if let Some(bump) = bump {
            args["bump"] = json!(bump);
        }
        self.write_ok("asset.signoff", args)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.ws.join(rel.replace('/', "\\"))
    }

    fn manifest(&self) -> Value {
        let path = self.path(&format!("{ASSET_DIR}/{ASSET}.manifest.json"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
    }
}

fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .readonly()
}

#[test] // spec: CORE-026, CORE-051
fn signoff_seals_read_only_version_with_snapshot() {
    let mut s = Served::start();
    let run_id = s.import("kite.png", b"kite one");
    let result = s.signoff(&run_id, "kite.png", None);
    assert_eq!(result["version"], "1.0.0", "{result}");

    let version = s.path(&format!("{ASSET_DIR}/v1.0.0"));
    for name in ["kite.png", "manifest.snapshot.json"] {
        let path = version.join(name);
        assert!(path.is_file(), "{} is missing", path.display());
        assert!(is_read_only(&path), "{} is not read-only", path.display());
    }
    assert_eq!(
        std::fs::read(version.join("kite.png")).unwrap(),
        b"kite one"
    );
    let snapshot = std::fs::read(version.join("manifest.snapshot.json")).unwrap();
    let snapshot: Value = serde_json::from_slice(&snapshot)
        .unwrap_or_else(|e| panic!("manifest.snapshot.json is not JSON: {e}"));
    assert_eq!(snapshot["asset_id"], ASSET, "{snapshot}");

    let manifest = s.manifest();
    assert_eq!(manifest["approved_version"], "1.0.0", "{manifest}");
    assert_eq!(manifest["status"], "approved", "{manifest}");
    let run = manifest["candidate_runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|r| r["run_id"] == run_id.as_str()))
        .unwrap_or_else(|| panic!("no run {run_id}: {manifest}"));
    assert_eq!(run["status"], "selected", "{run}");
    assert_eq!(run["signed_off"]["by"], "cli", "{run}");
    assert_eq!(run["signed_off"]["version"], "1.0.0", "{run}");
}

#[test] // spec: CORE-027
fn signoff_uses_the_confirmed_bump() {
    let mut s = Served::start();
    let first = s.import("kite.png", b"kite one");
    s.signoff(&first, "kite.png", None);

    let detail = s.pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": ASSET}),
        None,
    );
    assert_eq!(detail["next_versions"]["minor"], "1.1.0", "{detail}");
    assert_eq!(detail["next_versions"]["major"], "2.0.0", "{detail}");
    assert_eq!(detail["next_versions"]["patch"], "1.0.1", "{detail}");

    let second = s.import("kite_two.png", b"kite two");
    let result = s.signoff(&second, "kite_two.png", Some("minor"));
    assert_eq!(result["version"], "1.1.0", "{result}");
    let sealed = s.path(&format!("{ASSET_DIR}/v1.1.0/kite_two.png"));
    assert_eq!(
        std::fs::read(&sealed).unwrap_or_else(|e| panic!("{}: {e}", sealed.display())),
        b"kite two"
    );
    assert_eq!(s.manifest()["approved_version"], "1.1.0");
}

#[test] // spec: ARCH-042, CORE-031, CORE-047
fn sealed_ref_carries_the_content_hash() {
    let mut s = Served::start();
    let run_id = s.import("kite.png", b"kite one");
    let result = s.signoff(&run_id, "kite.png", None);

    let hash = version_content_hash_of(&s.path(&format!("{ASSET_DIR}/v1.0.0")));
    let expected = format!(
        "ww://{}/sketchbook/{ASSET}@1.0.0#sha256={hash}",
        s.env.workspace_id(&s.ws)
    );
    assert_eq!(result["ref"], json!(expected), "{result}");
}

#[test] // spec: CORE-029
fn link_external_asset_cannot_be_signed_off_until_collected() {
    let mut s = Served::start();
    let source = s.outside_file("kite_ext.png", b"external kite");
    s.write_ok(
        "asset.import",
        json!({
            "project": "sketchbook",
            "asset_id": ASSET,
            "type": "image",
            "files": [source.to_str().unwrap()],
            "mode": "link_external",
        }),
    );

    let refused = s.write(
        "asset.signoff",
        json!({
            "project": "sketchbook",
            "asset_id": ASSET,
            "run_id": "r_20261016_1200",
            "files": ["kite_ext.png"],
        }),
    );
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(refused["error"]["code"], "asset.link_external", "{refused}");
    assert_eq!(
        refused["error"]["fix"]["agent_hint"]["tool"], "asset.collect",
        "{refused}"
    );
    assert!(!s.path(&format!("{ASSET_DIR}/v1.0.0")).exists());

    let collected = s.write_ok(
        "asset.collect",
        json!({"project": "sketchbook", "asset_id": ASSET}),
    );
    let run_id = collected["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("asset.collect returned no run_id: {collected}"))
        .to_string();
    assert!(
        s.manifest().get("external").is_none(),
        "`external` stays after the collect: {}",
        s.manifest()
    );

    let result = s.signoff(&run_id, "kite_ext.png", None);
    assert_eq!(result["version"], "1.0.0", "{result}");
    let sealed = s.path(&format!("{ASSET_DIR}/v1.0.0/kite_ext.png"));
    assert_eq!(
        std::fs::read(&sealed).unwrap_or_else(|e| panic!("{}: {e}", sealed.display())),
        b"external kite",
        "the sealed version must hold the collected file inside the workspace"
    );
}

#[test] // spec: ARCH-058, CORE-037, CORE-110
fn trash_keeps_restore_record_and_restores() {
    let mut s = Served::start();
    let rel = "notes/trash_me.txt";
    let target = s.path(rel);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"keep me").unwrap();

    let day_before = utc::today();
    let trashed = s.write_ok("test.trash", json!({"path": rel}));
    let day_after = utc::today();
    let entry_id = trashed["entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("test.trash returned no entry_id: {trashed}"))
        .to_string();
    assert!(!target.exists(), "the trashed file is still there");

    let list = s.pipe.tool_ok("trash.list", json!({}), None);
    let entry = list["entries"]
        .as_array()
        .and_then(|entries| entries.iter().find(|e| e["entry_id"] == entry_id.as_str()))
        .unwrap_or_else(|| panic!("trash.list has no entry {entry_id}: {list}"))
        .clone();
    assert_eq!(entry["original_path"], rel, "{entry}");
    let moved = utc::parse_rfc3339(entry["moved_utc"].as_str().unwrap_or_default());
    let purge = utc::parse_rfc3339(entry["purge_after"].as_str().unwrap_or_default());
    assert_eq!(
        purge - moved,
        30 * 86_400,
        "purge_after is 30 days after moved_utc: {entry}"
    );

    // <ws>\.trash\<YYYY-MM-DD>\<te_16hex>\payload\<name>, with restore.json beside payload.
    let day_dir = [&day_before, &day_after]
        .iter()
        .map(|d| s.ws.join(".trash").join(d.as_str()))
        .find(|p| p.is_dir())
        .unwrap_or_else(|| panic!("no .trash\\{day_before} folder"));
    let entries: Vec<PathBuf> = std::fs::read_dir(&day_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let folder = entries[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let hex = folder
        .strip_prefix("te_")
        .unwrap_or_else(|| panic!("{folder} is not te_<16 hex>"));
    assert!(
        hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{folder} is not te_<16 hex>"
    );
    let payload = entries[0].join("payload").join("trash_me.txt");
    assert_eq!(
        std::fs::read(&payload).unwrap_or_else(|e| panic!("{}: {e}", payload.display())),
        b"keep me"
    );
    let record = std::fs::read(entries[0].join("restore.json")).unwrap();
    let record: Value =
        serde_json::from_slice(&record).unwrap_or_else(|e| panic!("restore.json is not JSON: {e}"));
    assert_eq!(record["entry_id"], entry_id.as_str(), "{record}");
    assert_eq!(record["original_path"], rel, "{record}");

    let restored = s.write_ok("trash.restore", json!({"entry_id": entry_id}));
    assert_eq!(restored["path"], rel, "{restored}");
    assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
}
