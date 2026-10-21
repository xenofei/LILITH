//! `lil-mcp` over stdio, as Claude Code runs it: the test binary under the fake `claude.exe`
//! (`TestEnv::mcp_under_claude`), against a real test-channel core (ARCH-012, ARCH-014,
//! ARCH-018, AI-110–113, AI-115; Appendix F "MCP lowering").

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use lil_protocol::lowering::{assert_injective, mcp_name};
use lil_testkit::{CoreFixture, McpStdio, RawPipe, TestEnv, ensure_built};
use serde_json::{Value, json};

/// `initialize` and `notifications/initialized`; returns the result.
fn initialize(mcp: &mut McpStdio) -> Value {
    let response = mcp.request(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "lil-mcp-tests", "version": "0.0.0"}}),
    );
    let result = response
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("initialize failed: {response}"));
    mcp.notify("notifications/initialized", json!({}));
    result
}

/// An initialized `lil-mcp` under the fake `claude`.
fn mcp(env: &TestEnv) -> McpStdio {
    let mut mcp = env.mcp_under_claude();
    initialize(&mut mcp);
    mcp
}

/// `tools/list` (one page).
fn tools(mcp: &mut McpStdio) -> Vec<Value> {
    let response = mcp.request("tools/list", json!({}));
    response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list failed: {response}"))
        .clone()
}

/// What a `tools/call` returned.
struct Call {
    is_error: bool,
    /// The first text block that is JSON.
    json: Value,
    texts: Vec<String>,
}

fn call(mcp: &mut McpStdio, name: &str, args: Value) -> Call {
    let response = mcp.request("tools/call", json!({"name": name, "arguments": args}));
    let result = response
        .get("result")
        .unwrap_or_else(|| panic!("tools/call {name} failed at the protocol level: {response}"));
    let texts: Vec<String> = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/call {name} has no content: {response}"))
        .iter()
        .filter_map(|c| c["text"].as_str().map(str::to_string))
        .collect();
    let json = texts
        .iter()
        .find_map(|t| serde_json::from_str::<Value>(t).ok())
        .unwrap_or_else(|| panic!("tools/call {name} has no JSON text: {response}"));
    Call {
        is_error: result["isError"] == true,
        json,
        texts,
    }
}

/// The `structured` part of an `ok` result.
fn ok(mcp: &mut McpStdio, name: &str, args: Value) -> Value {
    let c = call(mcp, name, args);
    assert!(!c.is_error, "{name}: {:?}", c.texts);
    assert_eq!(c.json["status"], "ok", "{name}: {:?}", c.texts);
    c.json["structured"].clone()
}

/// The registered (dotted) names a test-channel core lists for a `cli` session.
fn core_tool_names(env: &TestEnv) -> Vec<String> {
    let mut pipe = RawPipe::connect(env);
    pipe.hello(json!({}));
    let listed = pipe.call("tools/list", json!({}));
    listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list failed: {listed}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// The sample workspace, registered as the default one.
fn default_workspace(env: &TestEnv, name: &str) -> std::path::PathBuf {
    let ws = env.copy_fixture_workspace(name);
    env.register_workspace(&ws, true);
    ws
}

#[test] // spec: AI-110
fn lowered_names_are_injective_over_the_registry() {
    assert_eq!(mcp_name("asset.get"), "asset_get");
    assert_eq!(
        mcp_name("engines.comfyui.install_node"),
        "engines_comfyui_install_node"
    );
    assert_eq!(mcp_name("lil.status"), "lil_status");
    let err = assert_injective(&["a.b".to_string(), "a_b".to_string()]).unwrap_err();
    assert!(err.contains("a.b") && err.contains("a_b"), "{err}");

    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut names = core_tool_names(&env);
    assert!(names.len() > 20, "{names:?}");
    names.push("lil.status".into());
    assert_injective(&names).unwrap();

    // And the server lists every one of them, lowered, once.
    let mut mcp = mcp(&env);
    let mut listed: Vec<String> = tools(&mut mcp)
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    listed.sort();
    let mut lowered: Vec<String> = names.iter().map(|n| mcp_name(n)).collect();
    lowered.sort();
    assert_eq!(listed, lowered);
}

#[test] // spec: AI-111
fn descriptions_fit_2000_characters() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut mcp = mcp(&env);
    for t in tools(&mut mcp) {
        let description = t["description"].as_str().unwrap_or_default();
        let chars = description.chars().count();
        assert!(
            (1..=2000).contains(&chars),
            "{}: {chars} characters",
            t["name"]
        );
    }
}

#[test] // spec: QA-002, CORE-091
fn retry_token_becomes_the_idempotency_key() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "retry");
    let _core = CoreFixture::start(&env);
    let mut mcp = mcp(&env);

    // test.job refuses unknown arguments, so these run only if retry_token was taken out.
    let job = |mcp: &mut McpStdio, args: Value| {
        ok(mcp, "test_job", args)["job"]["job_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let first = job(&mut mcp, json!({"lane": "cpu", "retry_token": "k1"}));
    let again = job(&mut mcp, json!({"lane": "cpu", "retry_token": "k1"}));
    assert_eq!(first, again, "the same token is the same call");
    let a = job(&mut mcp, json!({"lane": "cpu"}));
    let b = job(&mut mcp, json!({"lane": "cpu"}));
    assert_ne!(a, b, "without a token every call gets its own key");
    assert_ne!(a, first);

    // The core recorded k1 itself as the job's idempotency key (jobs log, workspace format I8).
    let queue = ws.join("queue");
    let lines: Vec<Value> = std::fs::read_dir(&queue)
        .unwrap_or_else(|e| panic!("{}: {e}", queue.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("jobs.")
        })
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    let keys = |id: &str| -> Vec<Value> {
        lines
            .iter()
            .filter(|l| l["job_id"] == id)
            .map(|l| l["idempotency_key"].clone())
            .collect()
    };
    let first_keys = keys(&first);
    assert!(!first_keys.is_empty(), "no log line for {first}");
    assert!(first_keys.iter().all(|k| k == "k1"), "{first_keys:?}");
    assert_ne!(keys(&a).first(), keys(&b).first());
}

#[test] // spec: ARCH-014, AI-108, QA-062
fn incompatible_core_lists_only_lil_status() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    // `cmd` sets the test-channel override, then the fake `claude` (on PATH) starts lil-mcp as
    // its direct child.
    let exe = ensure_built().lil_mcp.clone();
    let mut mcp = env.spawn_stdio(&[
        OsStr::new("cmd.exe"),
        OsStr::new("/d"),
        OsStr::new("/c"),
        OsStr::new("set"),
        OsStr::new("LIL_TEST_CLIENT_PROTOCOL=0.9&&"),
        OsStr::new("claude.exe"),
        OsStr::new("--spawn-stdio"),
        exe.as_os_str(),
    ]);
    initialize(&mut mcp);
    let names: Vec<String> = tools(&mut mcp)
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["lil_status"]);

    let status = ok(&mut mcp, "lil_status", json!({}));
    assert_eq!(status["status"], "incompatible", "{status}");
    assert_eq!(status["error"]["code"], "protocol.incompatible", "{status}");
    assert_eq!(status["caller"], Value::Null, "{status}");
    // Any other tool is an error result naming the same error, not a hang.
    let other = call(&mut mcp, "test_echo", json!({"text": "x"}));
    assert!(other.is_error, "{:?}", other.texts);
    assert_eq!(other.json["error"]["code"], "protocol.incompatible");
    // No resources either (review M3).
    let resources = mcp.request("resources/list", json!({}));
    assert_eq!(
        resources["result"]["resources"],
        json!([]),
        "only lil_status is exposed: {resources}"
    );
}

/// Review M5: a workspace found from Claude Code's folder that `suite.toml` does not list is
/// left out, and `lil_status` says so instead of a bare `workspace: null`.
#[test] // spec: ARCH-030
fn an_unregistered_workspace_is_reported() {
    let env = TestEnv::new();
    let ws = env.make_workspace("loose");
    let _core = CoreFixture::start(&env);
    // `cmd` changes to the workspace folder, then the fake `claude` starts lil-mcp there.
    let exe = ensure_built().lil_mcp.clone();
    let mut mcp = env.spawn_stdio(&[
        OsStr::new("cmd.exe"),
        OsStr::new("/d"),
        OsStr::new("/c"),
        OsStr::new("cd"),
        OsStr::new("/d"),
        ws.as_os_str(),
        OsStr::new("&&"),
        OsStr::new("claude.exe"),
        OsStr::new("--spawn-stdio"),
        exe.as_os_str(),
    ]);
    initialize(&mut mcp);
    let status = call(&mut mcp, "lil_status", json!({}));
    assert_eq!(
        status.json["structured"]["status"], "connected",
        "{:?}",
        status.texts
    );
    assert_eq!(status.json["structured"]["workspace"], Value::Null);
    let summary = status.json["summary_text"].as_str().unwrap();
    assert!(
        summary.contains("not registered") && summary.contains("ws-loose"),
        "{summary}"
    );
}

/// Ruling P3-R25: when no core can be started (here a suite `state.db` from a newer version,
/// so the core refuses to start), `lil_status` says `unreachable` with `core.unreachable`,
/// only `lil_status` is listed, and another tool's result carries the same error.
#[test] // spec: ARCH-014, ARCH-018, QA-003
fn an_unreachable_core_reports_core_unreachable() {
    let env = TestEnv::new();
    env.write_state_db_version(&env.env.local_dir().join("state.db"), 999, true);
    let mut mcp = mcp(&env);

    let status = ok(&mut mcp, "lil_status", json!({}));
    assert_eq!(status["status"], "unreachable", "{status}");
    assert_eq!(status["error"]["code"], "core.unreachable", "{status}");
    assert_eq!(status["error"]["retry"], "same_idempotency_key", "{status}");
    let names: Vec<String> = tools(&mut mcp)
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["lil_status"]);
    let other = call(&mut mcp, "test_echo", json!({"text": "x"}));
    assert!(other.is_error, "{:?}", other.texts);
    assert_eq!(other.json["error"]["code"], "core.unreachable");
}

#[test] // spec: ARCH-018
fn server_starts_the_core_and_reconnects() {
    let env = TestEnv::new();
    assert_eq!(env.lil(&["--no-start", "status"]).code, 3, "no core yet");
    let mut mcp = mcp(&env);
    let first = ok(&mut mcp, "lil_status", json!({}));
    assert_eq!(first["status"], "connected", "{first}");
    assert_eq!(first["caller"], "mcp", "{first}");
    let first_pid = first["core"]["pid"].as_u64().unwrap();

    let stop = env.lil(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(
        env.lil(&["--no-start", "status"]).code,
        3,
        "the core stopped"
    );

    // The next call starts a new core and succeeds.
    let echoed = ok(&mut mcp, "test_echo", json!({"text": "again"}));
    assert_eq!(echoed["text"], "again");
    let second = ok(&mut mcp, "lil_status", json!({}));
    assert_eq!(second["status"], "connected", "{second}");
    assert_ne!(second["core"]["pid"].as_u64().unwrap(), first_pid);
}

#[test] // spec: AI-113
fn job_wait_never_waits_past_90_seconds() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut mcp = mcp(&env);
    let job = ok(&mut mcp, "test_job", json!({"lane": "cpu", "gated": true}));
    let job_id = job["job"]["job_id"].as_str().unwrap().to_string();

    let started = Instant::now();
    let refused = call(
        &mut mcp,
        "job_wait",
        json!({"job_id": job_id, "timeout_s": 91}),
    );
    assert!(refused.is_error, "{:?}", refused.texts);
    assert_eq!(refused.json["error"]["code"], "params.invalid");
    assert!(started.elapsed() < Duration::from_secs(30), "it waited");
    ok(&mut mcp, "test_job_release", json!({"job_id": job_id}));
}

/// A manifest's last-write time, set explicitly.
fn set_mtime(manifest: &Path, at: SystemTime) {
    let file = std::fs::File::options().write(true).open(manifest).unwrap();
    file.set_modified(at).unwrap();
}

/// `count` more copies of the fixture's `prop_signal_lamp`, as `prop_lamp_<nn>`. Every
/// manifest's time is set explicitly (review I3: a copy keeps the checkout's times): the
/// fixture's own on 2026-01-01, the copies one second apart from 2026-01-02, oldest first.
fn add_assets(ws: &Path, count: usize) -> Vec<String> {
    let assets = ws.join("projects").join("sketchbook").join("assets");
    let source = assets.join("prop_signal_lamp");
    let day = Duration::from_secs(86_400);
    // 2026-01-01T00:00:00Z.
    let fixture_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
    for entry in std::fs::read_dir(&assets).unwrap() {
        let dir = entry.unwrap().path();
        let id = dir.file_name().unwrap().to_string_lossy().into_owned();
        set_mtime(&dir.join(format!("{id}.manifest.json")), fixture_time);
    }
    let base = fixture_time + day;
    (0..count)
        .map(|i| {
            let id = format!("prop_lamp_{i:02}");
            let dir = assets.join(&id);
            copy_dir(&source, &dir);
            std::fs::remove_file(dir.join("prop_signal_lamp.manifest.json")).unwrap();
            let text = std::fs::read_to_string(source.join("prop_signal_lamp.manifest.json"))
                .unwrap()
                .replace("\"prop_signal_lamp\"", &format!("\"{id}\""));
            let manifest = dir.join(format!("{id}.manifest.json"));
            std::fs::write(&manifest, text).unwrap();
            set_mtime(&manifest, base + Duration::from_secs(i as u64));
            id
        })
        .collect()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// `resources/read` of `uri`: its one content's text, parsed.
fn read_resource(mcp: &mut McpStdio, uri: &str) -> Value {
    let response = mcp.request("resources/read", json!({ "uri": uri }));
    let contents = response["result"]["contents"]
        .as_array()
        .unwrap_or_else(|| panic!("resources/read {uri} failed: {response}"));
    assert_eq!(contents.len(), 1, "{response}");
    assert_eq!(contents[0]["uri"], uri, "{response}");
    serde_json::from_str(contents[0]["text"].as_str().unwrap())
        .unwrap_or_else(|e| panic!("{uri} is not JSON ({e}): {response}"))
}

#[test] // spec: AI-115
fn resources_list_project_jobs_and_recent_assets() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "resources");
    let added = add_assets(&ws, 22);
    let _core = CoreFixture::start(&env);
    // Wait for the workspace's first reconcile, so every manifest is indexed.
    let mut pipe = RawPipe::connect(&env);
    pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    pipe.fence(&ws);
    let mut mcp = mcp(&env);

    let response = mcp.request("resources/list", json!({}));
    let mut uris: Vec<&str> = response["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("resources/list failed: {response}"))
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    uris.sort_unstable();
    assert_eq!(
        uris,
        [
            "lil://assets/recent",
            "lil://context/project",
            "lil://jobs/open"
        ]
    );

    let recent = read_resource(&mut mcp, "lil://assets/recent");
    let ids: Vec<&str> = recent["assets"]
        .as_array()
        .unwrap_or_else(|| panic!("{recent}"))
        .iter()
        .map(|a| a["asset_id"].as_str().unwrap())
        .collect();
    // The 20 newest of the added ones (the fixture's own two are older), newest first.
    let newest: Vec<&str> = added.iter().rev().take(20).map(String::as_str).collect();
    assert_eq!(ids, newest);

    // Open jobs: one running, one queued behind it in the same lane; newest first.
    let gated = |mcp: &mut McpStdio| {
        ok(mcp, "test_job", json!({"lane": "cpu", "gated": true}))["job"]["job_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let running = gated(&mut mcp);
    let queued = gated(&mut mcp);
    let open = read_resource(&mut mcp, "lil://jobs/open");
    let ids: Vec<&str> = open["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("{open}"))
        .iter()
        .map(|j| j["job_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [queued.as_str(), running.as_str()], "{open}");
    for id in [&running, &queued] {
        ok(&mut mcp, "test_job_release", json!({ "job_id": id }));
    }

    let context = read_resource(&mut mcp, "lil://context/project");
    assert!(context.is_object(), "{context}");
}
