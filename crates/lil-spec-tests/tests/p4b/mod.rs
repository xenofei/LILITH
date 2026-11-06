//! Helpers shared by the plan-4b spec tests (QA-017, QA-018), next to [`crate::support`]. They
//! drive only the stable surfaces: tool calls from a `cli` connection (`RawPipe`), `app/*` calls
//! and tool calls from the `ui:test` app (`TestApp`), the real `lil`, and the workspace files an
//! outside program reads and writes (manifests, `registry/*.yaml`, the trash). Images are made in
//! the test with `lil_testkit::png`. Each test binary uses a part of them, so unused ones are
//! allowed here.

#![allow(
    dead_code,
    reason = "each spec-test binary uses a different part of these helpers"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lil_testkit::{CoreFixture, RawPipe, TestApp, TestEnv};
use serde_json::{Value, json};

use crate::support::*;

/// The fixture's project.
pub const PROJECT: &str = "sketchbook";
/// The fixture asset with a sealed `v1.0.0/crane_b.png`, a closed run and an open run.
pub const CRANE: &str = "chr_paper_crane";
/// Its closed (signed-off) run and its open run (`crane_c.png` with a derived cutout).
pub const CRANE_CLOSED_RUN: &str = "r_20261002_0915";
pub const CRANE_OPEN_RUN: &str = "r_20261003_1400";
/// The crane's sealed file, relative to the asset folder, and its recorded SHA-256.
pub const CRANE_SEALED: &str = "v1.0.0/crane_b.png";
pub const CRANE_SEALED_SHA256: &str =
    "aaae4dbdb0e697350dac691b26928d34a69061cf320b175c257c103fcaa32766";
/// The fixture's legacy asset (no `creation_uuid`, no `candidate_runs`).
pub const LAMP: &str = "prop_signal_lamp";

/// Protocol 1.4's capabilities, in order (Appendix D): what a plan-4b app offers in its hello.
pub const CAPS_4B: &[&str] = &[
    "events.v1",
    "tools.v1",
    "jobs.v1",
    "confirmations.v1",
    "caps.v1",
    "queue.v1",
    "desktop.v1",
    "context.v1",
    "proof.v1",
    "assets.v1",
];

/// Protocol 1.3's capabilities: the 1.4 list without `assets.v1` (Appendix D).
pub const CAPS_13: &[&str] = &[
    "events.v1",
    "tools.v1",
    "jobs.v1",
    "confirmations.v1",
    "caps.v1",
    "queue.v1",
    "desktop.v1",
    "context.v1",
    "proof.v1",
];

/// The `ui:test` app offering protocol 1.4's capabilities.
pub fn app_4b(env: &TestEnv, ws: Option<&Path>, topics: &[&str]) -> TestApp {
    TestApp::start_with_capabilities(env, ws, topics, CAPS_4B)
}

/// An `app/*` call that must succeed: its `result`.
pub fn app_ok(c: &mut impl Caller, method: &str, params: Value) -> Value {
    let response = c.call(method, params.clone());
    assert_eq!(
        response["result"]["status"], "ok",
        "{method} {params} did not succeed: {response}"
    );
    response["result"].clone()
}

/// The error code of an `app/*` call that must fail with one.
pub fn app_error(c: &mut impl Caller, method: &str, params: Value) -> String {
    let response = c.call(method, params.clone());
    response["result"]["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("{method} {params} did not fail with a code: {response}"))
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// A served copy of the fixture workspace

/// A copy of the fixture workspace, registered as the channel's default, its core and a `cli`
/// session naming it. Fields drop in this order: the connection closes before the core is
/// drained, and the env (with its folder) goes last.
pub struct Served {
    pub pipe: RawPipe,
    pub core: CoreFixture,
    pub ws: PathBuf,
    pub env: TestEnv,
}

impl Served {
    /// The fixture as it is, served.
    pub fn start() -> Served {
        Served::start_with(|_, _| {}, &[])
    }

    /// The fixture, changed by `prepare(env, ws)` before the core starts, served by a core
    /// started with the extra environment `extra`.
    pub fn start_with(prepare: impl FnOnce(&TestEnv, &Path), extra: &[(&str, &str)]) -> Served {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("p4b");
        env.register_workspace(&ws, true);
        prepare(&env, &ws);
        let core = CoreFixture::start_with_env(&env, extra);
        let pipe = session(&env, Some(&ws));
        Served {
            pipe,
            core,
            ws,
            env,
        }
    }

    /// The file at the workspace-relative `rel` (`/` separators).
    pub fn path(&self, rel: &str) -> PathBuf {
        ws_path(&self.ws, rel)
    }

    /// The workspace's id.
    pub fn ws_id(&self) -> String {
        self.env.workspace_id(&self.ws)
    }

    /// The manifest of the sketchbook asset `asset_id`, as JSON.
    pub fn manifest(&self, asset_id: &str) -> Value {
        manifest_of(&self.ws, asset_id)
    }

    /// The sketchbook asset folder of `asset_id`.
    pub fn asset_dir(&self, asset_id: &str) -> PathBuf {
        asset_dir(&self.ws, asset_id)
    }
}

/// The file at the workspace-relative `rel` (`/` separators) of `ws`.
pub fn ws_path(ws: &Path, rel: &str) -> PathBuf {
    ws.join(rel.replace('/', "\\"))
}

/// `projects/sketchbook/assets/<asset_id>` of `ws`.
pub fn asset_dir(ws: &Path, asset_id: &str) -> PathBuf {
    ws_path(ws, &format!("projects/{PROJECT}/assets/{asset_id}"))
}

/// The manifest of the sketchbook asset `asset_id` in `ws`, as JSON.
pub fn manifest_of(ws: &Path, asset_id: &str) -> Value {
    let path = asset_dir(ws, asset_id).join(format!("{asset_id}.manifest.json"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// Writes `fields` (JSON members, `"key": value, …` without the braces) at the start of the
/// manifest of `asset_id` in `ws`, as a person editing the file would; the rest of the file is
/// kept byte for byte.
pub fn add_manifest_fields(ws: &Path, asset_id: &str, fields: &str) {
    let path = asset_dir(ws, asset_id).join(format!("{asset_id}.manifest.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let at = text
        .find('{')
        .unwrap_or_else(|| panic!("{} holds no object", path.display()))
        + 1;
    let edited = format!("{}\n  {fields},{}", &text[..at], &text[at..]);
    serde_json::from_str::<Value>(&edited)
        .unwrap_or_else(|e| panic!("the edited {} is not JSON ({e}): {edited}", path.display()));
    std::fs::write(&path, edited).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// Replaces the one occurrence of `from` with `to` in the manifest of `asset_id` in `ws`, as a
/// person editing the file would.
pub fn edit_manifest_text(ws: &Path, asset_id: &str, from: &str, to: &str) {
    let path = asset_dir(ws, asset_id).join(format!("{asset_id}.manifest.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        text.matches(from).count(),
        1,
        "{} holds {from:?} not exactly once",
        path.display()
    );
    let edited = text.replacen(from, to, 1);
    serde_json::from_str::<Value>(&edited)
        .unwrap_or_else(|e| panic!("the edited {} is not JSON ({e}): {edited}", path.display()));
    std::fs::write(&path, edited).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// The run `run_id` of a manifest or `AssetDetail`.
pub fn run_of<'a>(manifest: &'a Value, run_id: &str) -> &'a Value {
    manifest["candidate_runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|r| r["run_id"] == run_id))
        .unwrap_or_else(|| panic!("no run {run_id}: {manifest}"))
}

/// The entry of `run`'s `files` whose path ends with `/<name>`.
pub fn run_file<'a>(run: &'a Value, name: &str) -> &'a Value {
    run["files"]
        .as_array()
        .and_then(|files| {
            files.iter().find(|f| {
                f["path"]
                    .as_str()
                    .is_some_and(|p| p == name || p.ends_with(&format!("/{name}")))
            })
        })
        .unwrap_or_else(|| panic!("the run has no file {name}: {run}"))
}

/// Whether the file's read-only attribute is set.
pub fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .readonly()
}

/// Copies the folder `from` to the new folder `to`, file by file (attributes kept), as Explorer
/// would.
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|e| panic!("{}: {e}", to.display()));
    for entry in std::fs::read_dir(from).unwrap_or_else(|e| panic!("{}: {e}", from.display())) {
        let entry = entry.unwrap_or_else(|e| panic!("{}: {e}", from.display()));
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap_or_else(|e| {
                panic!("{} -> {}: {e}", entry.path().display(), target.display())
            });
        }
    }
}

/// The text lines of a file, without line endings.
pub fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Asset tools

/// A new file outside the workspace holding `bytes`, named `name` (each call gets its own
/// folder, so one name can be written twice).
pub fn outside_file(env: &TestEnv, name: &str, bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = env
        .root()
        .join("import-src")
        .join(NEXT.fetch_add(1, Ordering::Relaxed).to_string())
        .join(name);
    lil_testkit::files::write_file(&path, bytes);
    path
}

/// The arguments of `asset.import` (copy) of `files` (name and bytes, written outside the
/// workspace first) into the sketchbook asset `asset_id`, of type `image`.
pub fn import_args(env: &TestEnv, asset_id: &str, files: &[(&str, &[u8])]) -> Value {
    let paths: Vec<String> = files
        .iter()
        .map(|(name, bytes)| path_str(&outside_file(env, name, bytes)))
        .collect();
    json!({"project": PROJECT, "asset_id": asset_id, "type": "image", "files": paths,
           "mode": "copy"})
}

/// `asset.import` (copy) of `files` into `asset_id`, checked to succeed; its `structured`.
pub fn import(
    c: &mut impl Caller,
    env: &TestEnv,
    asset_id: &str,
    files: &[(&str, &[u8])],
) -> Value {
    write_ok(c, "asset.import", import_args(env, asset_id, files))
}

/// The `run_id` of an `asset.import` or `asset.collect` result.
pub fn run_id_of(result: &Value) -> String {
    result["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no run_id in {result}"))
        .to_string()
}

/// `asset.signoff` of the files `files` of run `run_id`, checked to succeed.
pub fn signoff(c: &mut impl Caller, asset_id: &str, run_id: &str, files: &[&str]) -> Value {
    write_ok(
        c,
        "asset.signoff",
        json!({"project": PROJECT, "asset_id": asset_id, "run_id": run_id, "files": files}),
    )
}

/// `asset.get` of a sketchbook asset, checked to succeed.
pub fn asset_get(c: &mut impl Caller, asset_id: &str) -> Value {
    ok(
        c,
        "asset.get",
        json!({"project": PROJECT, "asset_id": asset_id}),
        None,
    )
}

/// `assets.query` with `args`, checked to succeed.
pub fn query(c: &mut impl Caller, args: Value) -> Value {
    ok(c, "assets.query", args, None)
}

/// The asset ids of an `assets.query` page, sorted.
pub fn ids_of(page: &Value) -> Vec<String> {
    let mut ids: Vec<String> = page["assets"]
        .as_array()
        .unwrap_or_else(|| panic!("no assets list: {page}"))
        .iter()
        .map(|a| {
            a["asset_id"]
                .as_str()
                .unwrap_or_else(|| panic!("an asset without an asset_id: {a}"))
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

/// The `AssetSummary` of `asset_id` in an `assets.query` page.
pub fn summary_of<'a>(page: &'a Value, asset_id: &str) -> &'a Value {
    page["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["asset_id"] == asset_id))
        .unwrap_or_else(|| panic!("the page has no {asset_id}: {page}"))
}

/// `project.status` of the sketchbook, checked to succeed.
pub fn project_status(c: &mut impl Caller) -> Value {
    ok(c, "project.status", json!({"project": PROJECT}), None)
}

/// The strings of a JSON array (panics on anything else).
pub fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .map(|s| {
            s.as_str()
                .unwrap_or_else(|| panic!("not a string: {s}"))
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Confirmations

/// The pending `ConfirmationView` of `tool`, from `app/confirmations.list`.
pub fn pending_view(app: &mut TestApp, tool: &str) -> Value {
    confirmations(app, "pending")
        .into_iter()
        .find(|c| c["tool"] == tool)
        .unwrap_or_else(|| panic!("no pending confirmation of {tool}"))
}

/// The app's own call of `tool`, which asks first (an "asks every caller" tool): the call gets
/// `pending_confirmation`, the app approves it, and the retry with the same key runs. Returns
/// the retry's `Outcome`.
pub fn app_call_confirmed(app: &mut TestApp, tool: &str, args: Value) -> Value {
    let k = key();
    let first = app.tool(tool, args.clone(), Some(&k));
    let id = pending_id(&first);
    answer(app, &id, json!({"answer": "approve"}));
    app.tool(tool, args, Some(&k))
}

/// `c`'s call of `tool`: when it gets `pending_confirmation`, the app approves it and the call
/// is retried with the same key. Returns the last `Outcome`.
pub fn call_confirmed(c: &mut impl Caller, app: &mut TestApp, tool: &str, args: Value) -> Value {
    let k = key();
    let first = c.tool(tool, args.clone(), Some(&k));
    if first["status"] != "pending_confirmation" {
        return first;
    }
    let id = pending_id(&first);
    answer(app, &id, json!({"answer": "approve"}));
    c.tool(tool, args, Some(&k))
}
