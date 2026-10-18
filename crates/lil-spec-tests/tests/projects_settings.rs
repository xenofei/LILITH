//! Projects and settings: the project tools, the settings cascade, `progress.md` entries,
//! the staleness check when an archived project is restored, and the per-session active
//! project (spec ARCH-088, CORE-039, CORE-048, CORE-055, CORE-057; Appendix E "Settings
//! cascade", "`progress.md` entries"). Driven through the project tools and `test.settings`
//! on a real core, and the workspace files written and read as an outside program would
//! (QA-018).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lil_testkit::{CoreFixture, RawPipe, TestEnv, sha256_bytes, utc, version_content_hash_of};
use serde_json::{Value, json};

/// A fresh idempotency key for every write call (ARCH-035).
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-ps-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A copy of the fixture workspace, registered as this channel's default.
fn default_workspace(env: &TestEnv) -> PathBuf {
    let ws = env.copy_fixture_workspace("projects");
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

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Writes `bytes` to `path` as an outside writer would, creating its folders.
fn write_outside(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// `project.create {project:"harbor", name:"Harbor"}`; returns the `Outcome`.
fn create_harbor(pipe: &mut RawPipe) -> Value {
    pipe.tool(
        "project.create",
        json!({"project": "harbor", "name": "Harbor"}),
        Some(&key()),
    )
}

fn error_code(outcome: &Value) -> String {
    assert_eq!(outcome["status"], "error", "expected an error: {outcome}");
    outcome["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a code: {outcome}"))
        .to_string()
}

#[test] // spec: CORE-055
fn project_create_status_and_context() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let created = create_harbor(&mut pipe);
    assert_eq!(created["status"], "ok", "{created}");
    assert_eq!(created["structured"]["project"], "harbor", "{created}");
    assert!(file(&ws, "projects/harbor/project.yaml").is_file());
    assert!(file(&ws, "projects/harbor/progress.md").is_file());

    let status = pipe.tool_ok("project.status", json!({"project": "harbor"}), None);
    assert_eq!(status["project"], "harbor", "{status}");
    assert_eq!(status["status"], "active", "{status}");

    let context = pipe.tool_ok("project.context", json!({"project": "harbor"}), None);
    assert_eq!(context["project"], "harbor", "{context}");
    assert!(context["settings"].is_object(), "{context}");
    let suggested: Vec<&str> = context["suggested_tools"]
        .as_array()
        .unwrap_or_else(|| panic!("project.context has no suggested_tools: {context}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for tool in ["asset.get", "project.status"] {
        assert!(
            suggested.contains(&tool),
            "{tool} (module `workspace`) is not suggested: {context}"
        );
    }

    let again = create_harbor(&mut pipe);
    assert_eq!(error_code(&again), "project.exists", "{again}");
}

#[test] // spec: CORE-057
fn settings_cascade_overrides_workspace_then_project_then_request() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let workspace_yaml = ws.join("lilith.yaml");
    let text = read_text(&workspace_yaml);
    let models = text
        .lines()
        .find(|l| l.trim_start().starts_with("models:"))
        .unwrap_or_else(|| panic!("the fixture's defaults have no models line:\n{text}"));
    let text = text.replacen(models, "  models: {image_generate: a}", 1);
    std::fs::write(&workspace_yaml, text).unwrap();
    let project_yaml = file(&ws, "projects/sketchbook/project.yaml");
    let mut project = read_text(&project_yaml);
    project.push_str("models: {image_generate: b}\n");
    std::fs::write(&project_yaml, project).unwrap();

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let layered = pipe.tool_ok("test.settings", json!({"project": "sketchbook"}), None);
    assert_eq!(layered["values"]["models.image_generate"], "b", "{layered}");
    assert_eq!(
        layered["sources"]["models.image_generate"], "project",
        "{layered}"
    );

    let requested = pipe.tool_ok(
        "test.settings",
        json!({"project": "sketchbook", "settings": {"models.image_generate": "c"}}),
        None,
    );
    assert_eq!(
        requested["values"]["models.image_generate"], "c",
        "{requested}"
    );
    assert_eq!(
        requested["sources"]["models.image_generate"], "request",
        "{requested}"
    );

    let suite_level = pipe.tool(
        "test.settings",
        json!({"project": "sketchbook", "settings": {"caps.monthly_usd": 1}}),
        None,
    );
    assert_eq!(error_code(&suite_level), "params.invalid", "{suite_level}");
}

#[test] // spec: CORE-039
fn progress_entries_go_under_done() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let day_before = utc::today();
    let created = create_harbor(&mut pipe);
    let day_after = utc::today();
    assert_eq!(created["status"], "ok", "{created}");

    let text = read_text(&file(&ws, "projects/harbor/progress.md"));
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let done = lines
        .iter()
        .position(|l| l.trim_end() == "## Done")
        .unwrap_or_else(|| panic!("no `## Done` heading:\n{text}"));
    let entry = lines
        .get(done + 1)
        .unwrap_or_else(|| panic!("nothing after `## Done`:\n{text}"));
    let dated = [&day_before, &day_after]
        .iter()
        .any(|d| entry.starts_with(&format!("- {d}: ")) && entry.trim_end().len() > 14);
    assert!(
        dated,
        "the line after `## Done` is not `- YYYY-MM-DD: …`: {entry:?}"
    );

    // Without the entry, the file is the new-project template byte for byte: the fixture's
    // sketchbook progress.md is that template, named "Sketchbook".
    let without: String = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != done + 1)
        .map(|(_, l)| *l)
        .collect();
    let template = read_text(&file(&ws, "projects/sketchbook/progress.md")).replacen(
        "# Sketchbook",
        "# Harbor",
        1,
    );
    assert_eq!(without, template, "progress.md:\n{text}");
}

/// A sealed version folder of an asset written by an outside writer: `files` plus a
/// `manifest.snapshot.json` holding `manifest`. Returns the version's content hash.
fn write_sealed(
    asset_dir: &Path,
    version: &str,
    files: &[(&str, &[u8])],
    manifest: &Value,
) -> String {
    let dir = asset_dir.join(format!("v{version}"));
    for (name, bytes) in files {
        write_outside(&dir.join(name), bytes);
    }
    let snapshot = serde_json::to_vec_pretty(manifest).unwrap();
    write_outside(&dir.join("manifest.snapshot.json"), &snapshot);
    version_content_hash_of(&dir)
}

/// A signed-off run record of I5 for the one file `name` with `bytes`.
fn signed_off_run(run_id: &str, version: &str, name: &str, bytes: &[u8]) -> Value {
    json!({
        "run_id": run_id,
        "path": format!("candidates/{run_id}/"),
        "status": "selected",
        "created": "2026-10-01T10:00:00Z",
        "target_version": version,
        "files": [{
            "path": format!("candidates/{run_id}/{name}"),
            "role": "candidate",
            "sha256": sha256_bytes(bytes),
        }],
        "selected": [name],
        "signed_off": {"by": "user", "date": "2026-10-01", "version": version},
        "pinned": false,
    })
}

/// The I5 manifest of a sketchbook asset approved at `version`, with its runs, hashes and inputs.
fn approved_manifest(
    asset_id: &str,
    uuid: &str,
    version: &str,
    runs: Vec<Value>,
    hashes: Value,
    inputs: Value,
) -> Value {
    json!({
        "schema_version": 1,
        "asset_id": asset_id,
        "project": "sketchbook",
        "creation_uuid": uuid,
        "type": "image",
        "version": version,
        "approved_version": version,
        "status": "approved",
        "status_reason": null,
        "inputs": inputs,
        "pinned_inputs": [],
        "candidate_runs": runs,
        "human_edits": [],
        "hashes": hashes,
        "created": "2026-10-01T10:00:00Z",
    })
}

fn write_manifest(asset_dir: &Path, asset_id: &str, manifest: &Value) {
    let path = asset_dir.join(format!("{asset_id}.manifest.json"));
    write_outside(&path, &serde_json::to_vec_pretty(manifest).unwrap());
}

fn read_manifest(asset_dir: &Path, asset_id: &str) -> Value {
    let path = asset_dir.join(format!("{asset_id}.manifest.json"));
    serde_json::from_slice(&std::fs::read(&path).unwrap())
        .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

#[test] // spec: CORE-055, CORE-048
fn archived_project_restore_runs_staleness_check() {
    const INPUT: &str = "env_stone_arch";
    const DEPENDENT: &str = "chr_stone_golem";
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let assets = file(&ws, "projects/sketchbook/assets");
    let input_dir = assets.join(INPUT);
    let dependent_dir = assets.join(DEPENDENT);

    // The input asset, approved at 1.0.0.
    let stone_1: &[u8] = b"stone arch 1.0.0";
    let input_1 = approved_manifest(
        INPUT,
        "1b0e6c52-4f7d-4a2b-9c3e-5d6f7a8b9c01",
        "1.0.0",
        vec![signed_off_run(
            "r_20261001_1000",
            "1.0.0",
            "stone.png",
            stone_1,
        )],
        json!({"v1.0.0/stone.png": format!("sha256:{}", sha256_bytes(stone_1))}),
        json!([]),
    );
    let input_hash = write_sealed(&input_dir, "1.0.0", &[("stone.png", stone_1)], &input_1);
    write_manifest(&input_dir, INPUT, &input_1);

    // Its dependent, approved, whose input is the input asset at 1.0.0.
    let golem: &[u8] = b"stone golem 1.0.0";
    let dependent = approved_manifest(
        DEPENDENT,
        "2c1f7d63-5a8e-4b3c-8d4f-6e7a8b9c0d12",
        "1.0.0",
        vec![signed_off_run(
            "r_20261001_1100",
            "1.0.0",
            "golem.png",
            golem,
        )],
        json!({"v1.0.0/golem.png": format!("sha256:{}", sha256_bytes(golem))}),
        json!([{"ref": format!("{INPUT}@1.0.0"), "role": "reference", "sha256": input_hash}]),
    );
    write_sealed(&dependent_dir, "1.0.0", &[("golem.png", golem)], &dependent);
    write_manifest(&dependent_dir, DEPENDENT, &dependent);

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    pipe.fence(&ws);

    let archived = pipe.tool_ok(
        "project.set_status",
        json!({"project": "sketchbook", "status": "archived"}),
        Some(&key()),
    );
    assert_eq!(archived["status"], "archived", "{archived}");

    // While the project is archived, the input gets a newer sealed version on disk.
    let stone_2: &[u8] = b"stone arch 1.1.0";
    let input_2 = approved_manifest(
        INPUT,
        "1b0e6c52-4f7d-4a2b-9c3e-5d6f7a8b9c01",
        "1.1.0",
        vec![
            signed_off_run("r_20261001_1000", "1.0.0", "stone.png", stone_1),
            signed_off_run("r_20261002_1000", "1.1.0", "stone.png", stone_2),
        ],
        json!({
            "v1.0.0/stone.png": format!("sha256:{}", sha256_bytes(stone_1)),
            "v1.1.0/stone.png": format!("sha256:{}", sha256_bytes(stone_2)),
        }),
        json!([]),
    );
    write_sealed(&input_dir, "1.1.0", &[("stone.png", stone_2)], &input_2);
    write_manifest(&input_dir, INPUT, &input_2);
    pipe.fence(&ws);

    let restored = pipe.tool_ok(
        "project.set_status",
        json!({"project": "sketchbook", "status": "active"}),
        Some(&key()),
    );
    let prefix = format!("lil://{}/sketchbook/{DEPENDENT}", env.workspace_id(&ws));
    let stale: Vec<&str> = restored["stale"]
        .as_array()
        .unwrap_or_else(|| panic!("project.set_status has no stale list: {restored}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        stale.iter().any(|r| r
            .strip_prefix(&prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('@'))),
        "the dependent {prefix} is not listed as stale: {restored}"
    );
    assert!(
        !stale.iter().any(|r| r.contains(&format!("/{INPUT}"))),
        "the input itself is not stale: {restored}"
    );

    let after = read_manifest(&dependent_dir, DEPENDENT);
    assert_eq!(after["status"], "stale", "{after}");
    assert_eq!(after["status_before_stale"], "approved", "{after}");
}

#[test] // spec: ARCH-088
fn active_project_is_per_session_with_state_json_hint() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    {
        let mut setup = session(&env, &ws);
        let created = create_harbor(&mut setup);
        assert_eq!(created["status"], "ok", "{created}");
    }

    let mut a = session(&env, &ws);
    let chosen = a.tool_ok("project.status", json!({"project": "harbor"}), None);
    assert_eq!(chosen["project"], "harbor", "{chosen}");

    // B has chosen nothing, so it gets the last-used hint.
    let mut b = session(&env, &ws);
    let hinted = b.tool_ok("project.status", json!({}), None);
    assert_eq!(hinted["project"], "harbor", "{hinted}");

    // B's own choice is B's; A keeps its own.
    let own = b.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    assert_eq!(own["project"], "sketchbook", "{own}");
    let kept = a.tool_ok("project.status", json!({}), None);
    assert_eq!(
        kept["project"], "harbor",
        "A's active project changed: {kept}"
    );
    let b_again = b.tool_ok("project.status", json!({}), None);
    assert_eq!(b_again["project"], "sketchbook", "{b_again}");

    let state = file(&ws, ".lilith/state.json");
    let hint: Value = serde_json::from_slice(
        &std::fs::read(&state).unwrap_or_else(|e| panic!("{}: {e}", state.display())),
    )
    .unwrap_or_else(|e| panic!("{} is not JSON: {e}", state.display()));
    assert_eq!(hint["active_project"], "sketchbook", "{hint}");
}
