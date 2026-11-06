//! Plan 4b's workspace protections: imported files are untrusted until your own Mark as trusted
//! from the app, every run in a project with a style bible gets the style input, agents can't
//! write instruction files, and a new workspace gets the LFS baseline in `.gitattributes` (spec
//! ARCH-060, CORE-025, CORE-040, ARCH-086; Appendix H "Trust", "Style input", "Instruction-file
//! names", "LFS baseline and size guard"). Driven through `asset.import` from a `cli` connection
//! and the `ui:test` app, `app/asset.trust`, the real `lil`, and the workspace files (QA-018).

mod p4b;
mod support;

use lil_testkit::TestEnv;
use p4b::*;
use serde_json::{Value, json};
use support::*;

const KITE: &str = "prop_kite";

/// The `(path, record)` pairs of a manifest's `trust` map.
fn trust_of(manifest: &Value) -> Vec<(String, Value)> {
    manifest["trust"]
        .as_object()
        .unwrap_or_else(|| panic!("the manifest has no trust map: {manifest}"))
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[test] // spec: ARCH-060
fn import_marks_files_untrusted_and_only_the_app_clears_it() {
    let mut s = Served::start();
    let run_id = run_id_of(&import(
        &mut s.pipe,
        &s.env,
        KITE,
        &[("kite.png", b"kite one")],
    ));
    let path = format!("candidates/{run_id}/kite.png");

    let trust = trust_of(&s.manifest(KITE));
    assert_eq!(trust.len(), 1, "{trust:?}");
    let (key, record) = &trust[0];
    assert_eq!(key, &path, "keys are asset-relative: {trust:?}");
    assert_eq!(record["state"], "untrusted", "{record}");
    assert_eq!(record["sources"][0]["kind"], "import", "{record}");
    assert_eq!(
        asset_get(&mut s.pipe, KITE)["trust"][&path]["state"],
        "untrusted"
    );

    let params = json!({"project": PROJECT, "asset_id": KITE});
    assert_eq!(
        app_error(&mut s.pipe, "app/asset.trust", params.clone()),
        "caller.not_allowed",
        "only your own Mark as trusted clears it"
    );
    assert_eq!(s.manifest(KITE)["trust"][&path]["state"], "untrusted");

    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    let result = app_ok(&mut app, "app/asset.trust", params);
    assert_eq!(result["trusted"], json!([path]), "{result}");
    let record = &s.manifest(KITE)["trust"][&path];
    assert_eq!(record["state"], "trusted", "{record}");
    assert!(record["marked_trusted"]["utc"].is_string(), "{record}");
}

#[test] // spec: CORE-025
fn style_input_is_added_to_every_run() {
    let mut s = Served::start_with(
        |_, ws| {
            let project = ws_path(ws, "projects/sketchbook/project.yaml");
            let mut text = std::fs::read_to_string(&project).unwrap_or_else(|e| panic!("{e}"));
            text.push_str("style_bible: style/bible.md\nstyle_version: 2\n");
            std::fs::write(&project, text).unwrap_or_else(|e| panic!("{e}"));
            lil_testkit::files::write_file(
                &ws_path(ws, "projects/sketchbook/style/bible.md"),
                b"# Style bible\n\nPaper and ink.\n",
            );
        },
        &[],
    );
    let first = run_id_of(&import(
        &mut s.pipe,
        &s.env,
        KITE,
        &[("kite.png", b"kite one")],
    ));
    let second = run_id_of(&import(
        &mut s.pipe,
        &s.env,
        KITE,
        &[("kite_2.png", b"kite two")],
    ));

    let manifest = s.manifest(KITE);
    for run_id in [&first, &second] {
        let run = run_of(&manifest, run_id);
        let styles: Vec<&Value> = run["inputs"]
            .as_array()
            .unwrap_or_else(|| panic!("run {run_id} has no inputs: {run}"))
            .iter()
            .filter(|i| i["role"] == "style")
            .collect();
        assert_eq!(styles.len(), 1, "one style input: {run}");
        assert_eq!(styles[0]["ref"], "style@2", "{run}");
    }
}

#[test] // spec: CORE-040
fn agents_cannot_write_instruction_files() {
    let mut s = Served::start();
    // A cli caller's import of an instruction file is refused.
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let refused = s.pipe.tool(
            "asset.import",
            import_args(&s.env, "prop_rules", &[(name, b"# instructions\n")]),
            Some(&key()),
        );
        assert_eq!(
            error_code(&refused),
            "store.path_invalid",
            "{name}: {refused}"
        );
        assert_eq!(
            refused["error"]["data"]["reason"], "instruction_surface",
            "{name}: {refused}"
        );
    }
    let rules = s.asset_dir("prop_rules");
    assert!(
        !rules.exists() || find_named(&rules, &["CLAUDE.md", "AGENTS.md"]).is_empty(),
        "an instruction file was written"
    );

    // Your own import from the app leaves it out and says so.
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    let imported = write_ok(
        &mut app,
        "asset.import",
        import_args(
            &s.env,
            KITE,
            &[("kite.png", b"kite one"), ("CLAUDE.md", b"# notes\n")],
        ),
    );
    assert_eq!(
        imported["left_out"],
        json!([{"name": "CLAUDE.md", "reason": "instruction_surface"}]),
        "{imported}"
    );
    let names: Vec<&str> = imported["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files: {imported}"))
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert_eq!(names, ["kite.png"], "{imported}");
    assert!(find_named(&s.asset_dir(KITE), &["CLAUDE.md"]).is_empty());
}

/// Every file under `dir` (recursively) named one of `names`, any case.
fn find_named(dir: &std::path::Path, names: &[&str]) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_dir() {
            found.extend(find_named(&path, names));
        } else if names
            .iter()
            .any(|n| entry.file_name().to_string_lossy().eq_ignore_ascii_case(n))
        {
            found.push(path);
        }
    }
    found
}

#[test] // spec: ARCH-086
fn workspace_create_writes_the_lfs_baseline() {
    let env = TestEnv::new();
    let ws = env.root().join("lfs ws");
    let out = env.lil(&["workspace", "init", &path_str(&ws)]);
    assert_eq!(out.code, 0, "{out:?}");

    let lines = lines_of(&ws.join(".gitattributes"));
    let first = lines
        .iter()
        .position(|l| l == "# >>> LILITH LFS baseline v1")
        .unwrap_or_else(|| panic!("no baseline block start: {lines:?}"));
    let last = lines
        .iter()
        .position(|l| l == "# <<< LILITH LFS baseline v1")
        .unwrap_or_else(|| panic!("no baseline block end: {lines:?}"));
    assert!(first < last, "{lines:?}");
    let block = &lines[first..=last];
    for line in [
        "projects/**/*.png filter=lfs diff=lfs merge=lfs -text",
        "projects/**/*.PNG filter=lfs diff=lfs merge=lfs -text",
    ] {
        assert!(block.iter().any(|l| l == line), "{line:?} in {block:?}");
    }
    assert!(
        lines.iter().any(|l| l == "projects/** -text"),
        "the plan-2b entry stays: {lines:?}"
    );
}
