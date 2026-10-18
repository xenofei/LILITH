//! Libraries and interchange: resources in workspace and personal scopes, saves that keep the
//! replaced version in the trash, soft deletes that ask agents, unknown kinds, published
//! changes, registered external handlers, and `open_in`, which returns a launch command and
//! never launches (spec APP-061, APP-063, APP-064, APP-065, APP-078, APP-079, APP-082,
//! CORE-110; Appendix F "Resources", "Interchange"). Driven through the resource, trash and
//! interchange tools from a `cli` connection and the `ui:test` app, and the files on disk
//! (QA-018).

mod support;

use std::path::{Path, PathBuf};

use lil_testkit::{CoreFixture, RawPipe, TestApp, TestEnv, ensure_built};
use serde_json::{Value, json};
use support::*;

/// `library/resources/<kind>/<id>.json` in the workspace.
fn workspace_file(ws: &Path, kind: &str, id: &str) -> PathBuf {
    ws.join("library")
        .join("resources")
        .join(kind)
        .join(format!("{id}.json"))
}

/// `%APPDATA%\LILITH\resources\<kind>\<id>.json` (this channel's data folder).
fn personal_file(env: &TestEnv, kind: &str, id: &str) -> PathBuf {
    env.env
        .data_dir()
        .join("resources")
        .join(kind)
        .join(format!("{id}.json"))
}

/// Whether the path `a` (absolute, or relative to the workspace `ws`) and `b` name the same
/// existing file, ignoring case and separators.
fn same_path(a: &Value, b: &Path, ws: &Path) -> bool {
    let a = PathBuf::from(a.as_str().unwrap_or_default().replace('/', "\\"));
    let a = if a.is_absolute() { a } else { ws.join(a) };
    let canon = |p: &Path| {
        std::fs::canonicalize(p)
            .map(|c| {
                c.to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .to_ascii_lowercase()
            })
            .unwrap_or_default()
    };
    !canon(b).is_empty() && canon(&a) == canon(b)
}

/// A resource file as an outside program reads it.
fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// A served workspace with a `cli` session in it.
fn served(env: &TestEnv) -> (PathBuf, CoreFixture, RawPipe) {
    let ws = default_workspace(env, "library");
    let core = CoreFixture::start(env);
    let pipe = session(env, Some(&ws));
    (ws, core, pipe)
}

#[test] // spec: APP-078, APP-079
fn resources_round_trip_in_workspace_and_personal_scopes() {
    let env = TestEnv::new();
    let (ws, _core, mut pipe) = served(&env);

    let palette = json!({"colors": ["#ff7e5f", "#feb47b"]});
    let saved = write_ok(
        &mut pipe,
        "resource.save",
        json!({"kind": "palette", "id": "sunset", "name": "Sunset", "data": palette}),
    );
    let file = workspace_file(&ws, "palette", "sunset");
    assert!(same_path(&saved["path"], &file, &ws), "{saved}");
    let on_disk = read_json(&file);
    assert_eq!(on_disk["schema_version"], 1, "{on_disk}");
    assert_eq!(on_disk["kind"], "palette", "{on_disk}");
    assert_eq!(on_disk["id"], "sunset", "{on_disk}");
    assert_eq!(on_disk["name"], "Sunset", "{on_disk}");
    assert_eq!(on_disk["data"], palette, "{on_disk}");
    for time in ["created", "updated"] {
        assert!(on_disk[time].is_string(), "{on_disk}");
    }

    // A model profile defaults to the personal scope.
    let profile = json!({"model": "fake-model", "steps": 20});
    let saved = write_ok(
        &mut pipe,
        "resource.save",
        json!({"kind": "model_profile", "id": "fast", "name": "Fast", "data": profile}),
    );
    let file = personal_file(&env, "model_profile", "fast");
    assert!(same_path(&saved["path"], &file, &ws), "{saved}");
    assert_eq!(read_json(&file)["data"], profile);

    let list = ok(&mut pipe, "resource.list", json!({}), None);
    let listed = list["resources"].as_array().cloned().unwrap_or_default();
    let scope_of = |kind: &str, id: &str| {
        listed
            .iter()
            .find(|r| r["kind"] == kind && r["id"] == id)
            .map(|r| r["scope"].clone())
            .unwrap_or_else(|| panic!("resource.list has no {kind}/{id}: {list}"))
    };
    assert_eq!(scope_of("palette", "sunset"), "workspace");
    assert_eq!(scope_of("model_profile", "fast"), "personal");

    let got = ok(
        &mut pipe,
        "resource.get",
        json!({"kind": "palette", "id": "sunset"}),
        None,
    );
    assert_eq!(got["resource"]["data"], palette, "{got}");
    let got = ok(
        &mut pipe,
        "resource.get",
        json!({"kind": "model_profile", "id": "fast"}),
        None,
    );
    assert_eq!(got["resource"]["data"], profile, "{got}");
}

#[test] // spec: APP-078, CORE-110
fn resource_save_over_an_existing_one_keeps_the_old_in_the_trash() {
    let env = TestEnv::new();
    let (ws, _core, mut pipe) = served(&env);
    let save = |pipe: &mut RawPipe, a: u64| {
        write_ok(
            pipe,
            "resource.save",
            json!({"kind": "palette", "id": "sunset", "name": "Sunset", "data": {"a": a}}),
        )
    };

    save(&mut pipe, 1);
    let second = save(&mut pipe, 2);
    let entry = second["replaced_entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no replaced_entry_id: {second}"))
        .to_string();
    let trash = ok(&mut pipe, "trash.list", json!({}), None);
    let listed = trash["entries"]
        .as_array()
        .and_then(|e| e.iter().find(|e| e["entry_id"] == entry.as_str()))
        .unwrap_or_else(|| panic!("trash.list has no {entry}: {trash}"));
    assert_eq!(listed["reason"], "resource.save", "{listed}");

    let file = workspace_file(&ws, "palette", "sunset");
    let blocked = pipe.tool("trash.restore", json!({"entry_id": entry}), Some(&key()));
    assert_eq!(error_code(&blocked), "trash.restore_conflict", "{blocked}");
    assert_eq!(read_json(&file)["data"], json!({"a": 2}), "nothing moved");

    let mut app = TestApp::start(&env, Some(&ws), &[]);
    write_ok(
        &mut app,
        "resource.delete",
        json!({"kind": "palette", "id": "sunset"}),
    );
    write_ok(&mut pipe, "trash.restore", json!({"entry_id": entry}));
    let got = ok(
        &mut pipe,
        "resource.get",
        json!({"kind": "palette", "id": "sunset"}),
        None,
    );
    assert_eq!(got["resource"]["data"], json!({"a": 1}), "{got}");
}

#[test] // spec: APP-082, CORE-110
fn resource_delete_is_soft_and_asks_agents() {
    let env = TestEnv::new();
    let (ws, _core, mut pipe) = served(&env);
    let target = json!({"kind": "palette", "id": "sunset"});
    write_ok(
        &mut pipe,
        "resource.save",
        json!({"kind": "palette", "id": "sunset", "name": "Sunset", "data": {"a": 1}}),
    );
    let file = workspace_file(&ws, "palette", "sunset");

    let by_agent = pipe.tool("resource.delete", target.clone(), Some(&key()));
    assert_eq!(error_code(&by_agent), "confirm.unavailable", "{by_agent}");
    assert!(file.is_file(), "an unconfirmed delete removed the file");

    let mut app = TestApp::start(&env, Some(&ws), &[]);
    let deleted = write_ok(&mut app, "resource.delete", target);
    let entry = deleted["entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no entry_id: {deleted}"))
        .to_string();
    assert!(!file.exists(), "the deleted resource is still there");
    let trash = ok(&mut pipe, "trash.list", json!({}), None);
    assert!(
        trash["entries"]
            .as_array()
            .is_some_and(|e| e.iter().any(|e| e["entry_id"] == entry.as_str())),
        "{trash}"
    );
    write_ok(&mut pipe, "trash.restore", json!({ "entry_id": entry }));
    assert_eq!(read_json(&file)["data"], json!({"a": 1}));
}

#[test] // spec: APP-079
fn unknown_resource_kind_is_refused() {
    let env = TestEnv::new();
    let (_ws, _core, mut pipe) = served(&env);
    let refused = pipe.tool(
        "resource.save",
        json!({"kind": "brush", "id": "round", "name": "Round", "data": {}}),
        Some(&key()),
    );
    assert_eq!(error_code(&refused), "resource.kind_unknown", "{refused}");
}

#[test] // spec: APP-078
fn resource_changes_are_published() {
    let env = TestEnv::new();
    let (ws, _core, mut pipe) = served(&env);
    let mut watcher = session(&env, Some(&ws));
    subscribe_live(&mut watcher, &["resources"]);

    write_ok(
        &mut pipe,
        "resource.save",
        json!({"kind": "palette", "id": "sunset", "name": "Sunset", "data": {}}),
    );
    wait_for_kind(&mut watcher, "resources", "saved");
    let mut app = TestApp::start(&env, Some(&ws), &[]);
    write_ok(
        &mut app,
        "resource.delete",
        json!({"kind": "palette", "id": "sunset"}),
    );
    wait_for_kind(&mut watcher, "resources", "deleted");
}

/// Writes `%APPDATA%\LILITH\apps\<name>.toml` registering `exe` for PNG images.
fn register_app(env: &TestEnv, name: &str, exe: &Path) {
    let file = env.env.data_dir().join("apps").join(format!("{name}.toml"));
    let text = format!(
        "app = \"{name}\"\nexe = '{}'\n[[opens]]\ntypes = [\"image\"]\nformats = [\"png\"]\nintents = [\"open\", \"edit\"]\n",
        path_str(exe)
    );
    lil_testkit::files::write_file(&file, text.as_bytes());
}

/// A PNG file to open, in the test's folder.
fn png(env: &TestEnv) -> PathBuf {
    let path = env.root().join("picture.png");
    lil_testkit::files::write_file(&path, b"\x89PNG\r\n\x1a\n");
    path
}

#[test] // spec: APP-061
fn handlers_for_lists_registered_apps() {
    let env = TestEnv::new();
    register_app(
        &env,
        "viewer",
        &env.root().join("viewer").join("viewer.exe"),
    );
    let target = png(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let handlers = ok(
        &mut pipe,
        "handlers.for",
        json!({"target": path_str(&target)}),
        None,
    );
    let viewer = handlers["handlers"]
        .as_array()
        .and_then(|h| h.iter().find(|h| h["app"] == "viewer"))
        .unwrap_or_else(|| panic!("viewer is not offered: {handlers}"));
    assert_eq!(viewer["external"], true, "{viewer}");
}

#[test] // spec: APP-063, APP-064, APP-065
fn open_in_returns_a_launch_command_and_never_launches() {
    let env = TestEnv::new();
    let viewer = env.root().join("viewer").join("spec-viewer.exe");
    register_app(&env, "viewer", &viewer);
    register_app(&env, "test", &ensure_built().test_app);
    let target = path_str(&png(&env));
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let open = json!({"app": "viewer", "target": target, "intent": "open"});
    let result = ok(&mut pipe, "open_in", open, None);
    assert_eq!(result["running"], false, "{result}");
    assert!(result["pid"].is_null(), "{result}");
    assert!(
        same_path_text(&result["launch_command"]["exe"], &viewer),
        "{result}"
    );
    let tasks = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq spec-viewer.exe", "/NH"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&tasks.stdout).contains("spec-viewer.exe"),
        "the core launched the app"
    );

    // The registered `test` app is running: it is told, without focus for an agent caller.
    let mut app = TestApp::start(&env, None, &["open_requests:test"]);
    let open = json!({"app": "test", "target": target, "intent": "open"});
    let result = ok(&mut pipe, "open_in", open, None);
    assert_eq!(result["running"], true, "{result}");
    assert_eq!(result["pid"], app.pid(), "{result}");
    let request = wait_for_event(&mut app, "open_requests:test");
    assert_eq!(request["intent"], "open", "{request}");
    assert_eq!(request["focus"], false, "{request}");
    assert!(
        request["target"].to_string().contains("picture.png"),
        "{request}"
    );
}

/// The `data` of the next event on `topic`.
fn wait_for_event(c: &mut impl Caller, topic: &str) -> Value {
    loop {
        let event = c
            .next_event(DEADLINE)
            .unwrap_or_else(|| panic!("no event on {topic} within {DEADLINE:?}"));
        if event["topic"] == topic && event["kind"] == "event" {
            return event["data"].clone();
        }
    }
}

/// Whether the text path `a` names `b` (which need not exist), ignoring case and separators.
fn same_path_text(a: &Value, b: &Path) -> bool {
    let norm = |s: &str| {
        s.replace('/', "\\")
            .trim_start_matches(r"\\?\")
            .to_ascii_lowercase()
    };
    a.as_str().is_some_and(|a| norm(a) == norm(&path_str(b)))
}
