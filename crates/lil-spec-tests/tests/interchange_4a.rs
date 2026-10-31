//! Interchange and sessions from plan 4a: launch arguments that carry the target as base64url,
//! `.lilref` files from `lilref.write`, the session's project from `app/set_context`, the project
//! and workspace lists, and `lil-mcp` starting a stopped suite app without focus (spec ARCH-015,
//! ARCH-088, APP-006, APP-020, APP-021, APP-063, APP-065; Appendix G "`lil:` URLs", "`.lilref`
//! files", "Session and listing tools", "`lil-mcp` launches"). Driven through the interchange,
//! project and workspace tools, `app/set_context` from the `ui:test` app, `lil-mcp` under the fake
//! `claude`, `lil-fake-image` and the files on disk (QA-018).

mod p4a;
mod support;

use std::path::{Path, PathBuf};

use lil_testkit::{CoreFixture, TestEnv, ensure_built, sha256_bytes, version_content_hash_of};
use p4a::*;
use serde_json::{Value, json};
use support::*;

/// Writes an app registration `<dir>\<name>.toml` for PNG images, with `args`.
fn register(dir: &Path, name: &str, exe: &Path, args: &[&str]) {
    let args: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
    lil_testkit::files::write_file(
        &dir.join(format!("{name}.toml")),
        format!(
            "app = \"{name}\"\nexe = '{}'\nargs = [{}]\n[[opens]]\ntypes = [\"image\"]\nformats = [\"png\"]\nintents = [\"open\", \"edit\"]\n",
            path_str(exe),
            args.join(", ")
        )
        .as_bytes(),
    );
}

/// A PNG file to open, in the test's folder.
fn png(env: &TestEnv) -> PathBuf {
    let path = env.root().join("spec picture #1.png");
    lil_testkit::files::write_file(&path, b"\x89PNG\r\n\x1a\n");
    path
}

/// Whether two path texts name the same file, ignoring case and separators.
fn same_path(a: &str, b: &Path) -> bool {
    let norm = |s: &str| {
        s.replace('/', "\\")
            .trim_start_matches(r"\\?\")
            .to_ascii_lowercase()
    };
    norm(a) == norm(&path_str(b))
}

#[test] // spec: APP-020, APP-063
fn open_in_launch_args_carry_base64url_targets() {
    let env = TestEnv::new();
    let viewer = env.root().join("viewer").join("spec-viewer.exe");
    register(
        &env.env.data_dir().join("apps"),
        "viewer",
        &viewer,
        &["--spec-flag"],
    );
    let target = png(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let result = ok(
        &mut pipe,
        "open_in",
        json!({"app": "viewer", "target": path_str(&target), "intent": "open"}),
        None,
    );
    let args: Vec<&str> = result["launch_command"]["args"]
        .as_array()
        .unwrap_or_else(|| panic!("no launch_command.args: {result}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(args.len(), 4, "{result}");
    assert_eq!(args[..3], ["--spec-flag", "--intent", "open"], "{result}");
    let opened = open_target(&scheme(&env), args[3]);
    assert!(same_path(&opened, &target), "{opened:?}");
}

/// A sealed ref of the fixture's `chr_paper_crane@1.0.0`, with its content hash.
fn sealed_ref(env: &TestEnv, ws: &Path) -> String {
    let hash =
        version_content_hash_of(&ws.join(r"projects\sketchbook\assets\chr_paper_crane\v1.0.0"));
    format!(
        "lil://{}/sketchbook/chr_paper_crane@1.0.0#sha256={hash}",
        env.workspace_id(ws)
    )
}

#[test] // spec: APP-021
fn lilref_write_makes_a_file_that_parses_back() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "lilref");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));
    let r = sealed_ref(&env, &ws);

    let written = write_ok(
        &mut pipe,
        "lilref.write",
        json!({"ref": r, "title": "Spec crane"}),
    );
    let path = PathBuf::from(
        written["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no path: {written}")),
    );
    let expected = env
        .env
        .local_dir()
        .join(r"cache\lilref")
        .join(format!("{}.lilref", &sha256_bytes(r.as_bytes())[..16]));
    assert!(same_path(&path_str(&path), &expected), "{written}");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_default())
        .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()));
    assert_eq!(file["lilref"], 1, "{file}");
    assert_eq!(file["ref"], r.as_str(), "{file}");
    assert_eq!(file["title"], "Spec crane", "{file}");
}

#[test] // spec: APP-021
fn lilref_write_refuses_a_candidate_ref() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "lilref-candidate");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));

    let candidate = format!(
        "lil://{}/sketchbook/chr_paper_crane@candidate/r_20261002_0915",
        env.workspace_id(&ws)
    );
    let refused = pipe.tool("lilref.write", json!({ "ref": candidate }), Some(&key()));
    assert_eq!(error_code(&refused), "ref.invalid", "{refused}");
    let unsealed = format!(
        "lil://{}/sketchbook/chr_paper_crane@1.0.0",
        env.workspace_id(&ws)
    );
    let refused = pipe.tool("lilref.write", json!({ "ref": unsealed }), Some(&key()));
    assert_eq!(
        error_code(&refused),
        "ref.invalid",
        "without #sha256=: {refused}"
    );
    assert!(
        !env.env.local_dir().join(r"cache\lilref").exists()
            || files_named(&env.env.local_dir().join(r"cache\lilref"), "", ".lilref").is_empty(),
        "nothing was written"
    );
}

#[test] // spec: ARCH-088, APP-043
fn app_set_context_sets_the_session_project() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "context");
    let _core = CoreFixture::start(&env);

    let mut old_app = lil_testkit::TestApp::start(&env, Some(&ws), &[]);
    assert_eq!(
        app_error(
            &mut old_app,
            "app/set_context",
            json!({"project": "sketchbook"})
        ),
        "caller.not_allowed",
        "an app without context.v1"
    );

    let mut app = app_4a(&env, Some(&ws), &[]);
    let set = app_ok(
        &mut app,
        "app/set_context",
        json!({"project": "sketchbook"}),
    );
    assert_eq!(set["project"], "sketchbook", "{set}");
    assert_eq!(
        set["workspace"]["id"],
        env.workspace_id(&ws).as_str(),
        "{set}"
    );
    let context = ok(&mut app, "project.context", json!({}), None);
    assert_eq!(context["project"], "sketchbook", "{context}");

    assert_eq!(
        app_error(
            &mut app,
            "app/set_context",
            json!({"project": "spec-no-such-project"})
        ),
        "project.unknown"
    );
}

#[test] // spec: ARCH-088, ARCH-098, APP-006
fn app_set_context_answers_the_effective_project() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "effective");
    let _core = CoreFixture::start(&env);

    let mut app = app_4a(&env, Some(&ws), &[]);
    let read = app_ok(&mut app, "app/set_context", json!({}));
    assert_eq!(read["project"], Value::Null, "{read}");
    assert_eq!(read["effective_project"], "sketchbook", "{read}");

    let mut bare = app_4a(&env, None, &[]);
    let read = app_ok(&mut bare, "app/set_context", json!({}));
    assert_eq!(read["effective_project"], Value::Null, "{read}");
}

#[test] // spec: APP-006
fn projects_and_workspaces_are_listed() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "listed");
    let gone = env.root().join("ws-gone");
    env.register_workspace(&gone, false);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));

    let projects = ok(&mut pipe, "projects.list", json!({}), None);
    let sketchbook = projects["projects"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["project"] == "sketchbook"))
        .unwrap_or_else(|| panic!("no sketchbook: {projects}"));
    assert_eq!(sketchbook["assets"], 2, "{projects}");
    assert!(sketchbook["name"].is_string(), "{projects}");
    assert!(sketchbook["status"].is_string(), "{projects}");

    let workspaces = ok(&mut pipe, "workspaces.list", json!({}), None);
    let list = workspaces["workspaces"]
        .as_array()
        .unwrap_or_else(|| panic!("no workspaces: {workspaces}"));
    let served = list
        .iter()
        .find(|w| w["id"] == env.workspace_id(&ws).as_str())
        .unwrap_or_else(|| panic!("the served workspace is not listed: {workspaces}"));
    assert_eq!(served["default"], true, "{served}");
    assert_eq!(served["served"], true, "{served}");
    assert_eq!(served["missing"], false, "{served}");
    let missing = list
        .iter()
        .find(|w| w["path"].as_str().is_some_and(|p| same_path(p, &gone)))
        .unwrap_or_else(|| panic!("the missing workspace is not listed: {workspaces}"));
    assert_eq!(missing["missing"], true, "{missing}");
    assert_eq!(missing["default"], false, "{missing}");
}

#[test] // spec: ARCH-015, APP-065
fn mcp_launches_a_stopped_app_without_focus() {
    let env = TestEnv::new();
    // An installed layout in the test's folder: the core, `lil-mcp` and the image app side by
    // side, with the app registered beside the core.
    let bin = env.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let bins = ensure_built();
    let copy = |from: &Path, to: &str| {
        std::fs::copy(from, bin.join(to))
            .unwrap_or_else(|e| panic!("cannot copy {} as {to}: {e}", from.display()));
    };
    copy(&bins.lil_core, "lil-core.exe");
    copy(&bins.lil_mcp, "lil-mcp.exe");
    copy(&bins.dir.join("lil-fake-image.exe"), "lil-image.exe");
    register(&bin.join("apps"), "image", &bin.join("lil-image.exe"), &[]);
    let target = png(&env);
    let _core = CoreFixture::start_exe(&env, &bin.join("lil-core.exe"));

    let mcp_exe = bin.join("lil-mcp.exe").into_os_string();
    let mut mcp = env.spawn_stdio(&[mcp_exe.as_os_str()]);
    mcp_initialize(&mut mcp);
    let shown = mcp_call(&mut mcp, "ui_show", json!({"target": path_str(&target)}));
    assert!(!shown.is_error, "{}", shown.text);
    assert!(
        shown.text.contains("Started image in the background."),
        "{}",
        shown.text
    );

    let recorded = env.root().join("fake-image-args.jsonl");
    let line = poll("lil-fake-image's recorded arguments", || {
        jsonl(&recorded).into_iter().next_back()
    });
    let args: Vec<String> = line
        .get("args")
        .unwrap_or(&line)
        .as_array()
        .unwrap_or_else(|| panic!("no argument list: {line}"))
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let url = args.last().cloned().unwrap_or_default();
    let opened = open_target(&scheme(&env), &url);
    assert!(same_path(&opened, &target), "{args:?}");
    let no_activate = args
        .iter()
        .position(|a| a == "--no-activate")
        .unwrap_or_else(|| panic!("no --no-activate: {args:?}"));
    assert!(
        no_activate < args.len() - 1,
        "--no-activate before the URL: {args:?}"
    );
    assert!(
        args.windows(2).any(|w| w == ["--intent", "open"]),
        "{args:?}"
    );
}
