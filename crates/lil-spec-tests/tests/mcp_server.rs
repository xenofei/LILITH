//! The MCP server `lil-mcp`: its name and instructions, lowered tools, confirmations for `mcp`
//! callers, the `mcp` identity rule, pending results and retry tokens, error results, an
//! incompatible core, starting the core, and a model's calls never running as `ui:*` (spec
//! ARCH-012, ARCH-014, ARCH-018, ARCH-039, ARCH-045, ARCH-047, ARCH-052, ARCH-054, AI-108,
//! AI-110, AI-111, APP-046, CORE-091, CORE-112, QA-002, QA-010, QA-062; Appendix F "MCP
//! lowering", "Confirmations"). `lil-mcp.exe` from the test binaries runs over stdio as the
//! direct child of the fake `claude.exe` (`TestEnv::mcp_under_claude`) unless a test says
//! otherwise (QA-018).
//!
//! Ruling P3-R10 (no double prompts): with `mcp_confirmation = "core"`, the value Appendix F
//! records, the core's confirmation is the only prompt, so `destructive` and `exec` tools carry
//! no `anthropic/requiresUserInteraction` marker for an `mcp` connection (Task 19 amends
//! Appendix F, ARCH-054, CORE-112 and AI-114 to say so).

mod support;

use std::ffi::OsStr;

use lil_testkit::{CoreFixture, McpStdio, TestApp, TestEnv, ensure_built};
use serde_json::{Value, json};
use support::*;

/// An initialized `lil-mcp` under the fake `claude`.
fn mcp(env: &TestEnv) -> McpStdio {
    let mut mcp = env.mcp_under_claude();
    mcp_initialize(&mut mcp);
    mcp
}

/// The `structured` part of an ok result's JSON, or the JSON itself.
fn payload(json: &Value) -> &Value {
    json.get("structured").unwrap_or(json)
}

/// How often `test.destructive` has run in this core, by running it once more from the app.
fn runs_so_far(app: &mut TestApp) -> u64 {
    let ran = write_ok(app, "test.destructive", json!({}));
    ran["count"]
        .as_u64()
        .unwrap_or_else(|| panic!("test.destructive returned no count: {ran}"))
        - 1
}

/// The tool called `name` in a `tools/list`.
fn tool<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("tools/list has no {name}"))
}

#[test] // spec: ARCH-012, AI-110, AI-111
fn initialize_names_the_server_lil() {
    let env = TestEnv::new();
    let mut mcp = env.mcp_under_claude();
    let result = mcp_initialize(&mut mcp);
    assert_eq!(result["serverInfo"]["name"], "lil", "{result}");
    let instructions = result["instructions"].as_str().unwrap_or_default();
    let chars = instructions.chars().count();
    assert!(
        (1..=1500).contains(&chars),
        "instructions of {chars} characters: {result}"
    );
}

#[test] // spec: AI-110, AI-111, ARCH-054, CORE-112
fn tools_are_lowered_with_markers_and_retry_token() {
    let env = TestEnv::new();
    let mut mcp = mcp(&env);
    let tools = mcp_tools(&mut mcp);

    for name in [
        "asset_get",
        "job_wait",
        "lil_status",
        "project_context",
        "test_destructive",
    ] {
        tool(&tools, name);
    }
    for t in &tools {
        let name = t["name"].as_str().unwrap_or_default();
        assert!(!name.contains('.'), "a tool name with a dot: {name}");
        let description = t["description"].as_str().unwrap_or_default();
        assert!(
            description.chars().count() <= 2000,
            "{name}: a description over 2,000 characters"
        );
        let schema = &t["inputSchema"];
        assert_eq!(
            schema["properties"]["retry_token"]["type"], "string",
            "{name}: no optional retry_token: {schema}"
        );
        let required = schema["required"].as_array().cloned().unwrap_or_default();
        assert!(
            !required.contains(&json!("retry_token")),
            "{name}: retry_token is required"
        );
        // P3-R10: the core's confirmation is the only prompt.
        assert!(
            t["_meta"]
                .get("anthropic/requiresUserInteraction")
                .is_none(),
            "{name} asks for a second prompt in Claude Code: {t}"
        );
    }
    for name in ["lil_status", "project_context", "job_wait"] {
        assert_eq!(
            tool(&tools, name)["_meta"]["anthropic/alwaysLoad"],
            true,
            "{name}"
        );
    }
    assert!(
        tool(&tools, "asset_get")["_meta"]
            .get("anthropic/alwaysLoad")
            .is_none()
    );
}

#[test] // spec: ARCH-054, CORE-112, APP-046
fn mcp_destructive_call_follows_mcp_confirmation() {
    // Appendix F records `mcp_confirmation = "core"`: an `mcp` call is treated like any agent
    // call.
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut mcp = mcp(&env);

    let no_app = mcp_call(&mut mcp, "test_destructive", json!({}));
    assert!(no_app.is_error, "{}", no_app.text);
    assert_eq!(
        no_app.json["error"]["code"], "confirm.unavailable",
        "{}",
        no_app.text
    );

    let mut app = TestApp::start(&env, None, &[]);
    let pending = mcp_call(&mut mcp, "test_destructive", json!({}));
    assert!(!pending.is_error, "{}", pending.text);
    let confirmation = pending_id(&pending.json);
    let token = pending.json["retry_token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    answer(&mut app, &confirmation, json!({"answer": "approve"}));
    let ran = mcp_call(
        &mut mcp,
        "test_destructive",
        json!({ "retry_token": token }),
    );
    assert!(!ran.is_error, "{}", ran.text);
    assert_eq!(payload(&ran.json)["count"], 1, "{}", ran.text);

    let history = confirmations(&mut app, "history");
    let entry = history
        .iter()
        .find(|c| c["id"] == confirmation.as_str())
        .unwrap_or_else(|| panic!("no {confirmation} in the history: {history:?}"));
    assert_eq!(entry["confirmed_via"], "app", "{entry}");
    assert_eq!(entry["caller_kind"], "mcp", "{entry}");
    assert_eq!(runs_so_far(&mut app), 1, "the refused call never ran");
}

#[test] // spec: ARCH-047, ARCH-052
fn mcp_spawned_from_a_shell_is_cli() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut mcp = env.mcp_under_shell();
    mcp_initialize(&mut mcp);

    let status = mcp_call(&mut mcp, "lil_status", json!({}));
    assert!(!status.is_error, "{}", status.text);
    assert_eq!(payload(&status.json)["caller"], "cli", "{}", status.text);
    let refused = mcp_call(&mut mcp, "test_destructive", json!({}));
    assert!(refused.is_error, "{}", refused.text);
    assert_eq!(
        refused.json["error"]["code"], "confirm.unavailable",
        "{}",
        refused.text
    );
    let mut app = TestApp::start(&env, None, &[]);
    assert_eq!(runs_so_far(&mut app), 0, "the refused call never ran");
}

#[test] // spec: QA-002, CORE-091
fn pending_is_a_normal_result_and_retry_token_continues() {
    let env = TestEnv::new();
    env.suite_toml_set("[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n");
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &[]);
    let mut mcp = mcp(&env);
    let args = json!({"provider": "fakeprov", "amount": "2"});

    let pending = mcp_call(&mut mcp, "test_meter_external", args.clone());
    assert!(
        !pending.is_error,
        "pending is not an error: {}",
        pending.text
    );
    let confirmation = pending_id(&pending.json);
    let token = pending.json["retry_token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!token.is_empty(), "{}", pending.text);
    assert!(
        pending.text.contains("the same arguments"),
        "{}",
        pending.text
    );
    assert!(pending.text.contains(&token), "{}", pending.text);

    answer(
        &mut app,
        &confirmation,
        json!({"answer": "approve", "cap": {"action": "allow_once"}}),
    );
    let mut again = args;
    again["retry_token"] = json!(token);
    let done = mcp_call(&mut mcp, "test_meter_external", again);
    assert!(!done.is_error, "{}", done.text);
    assert_eq!(done.json["status"], "ok", "{}", done.text);
}

#[test] // spec: ARCH-039, QA-010
fn tool_errors_are_is_error_results() {
    let env = TestEnv::new();
    default_workspace(&env, "mcp");
    let mut mcp = mcp(&env);
    let missing = mcp_call(
        &mut mcp,
        "asset_get",
        json!({"project": "sketchbook", "asset_id": "chr_no_such_asset"}),
    );
    assert!(missing.is_error, "{}", missing.text);
    assert_eq!(
        missing.json["error"]["code"], "asset.unknown",
        "{}",
        missing.text
    );
}

#[test] // spec: ARCH-014, AI-108, QA-062
fn incompatible_core_exposes_only_lil_status() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    // `cmd` sets the test-channel override, then the fake `claude` (found on PATH) starts
    // `lil-mcp` as its direct child, so only the protocol differs from `mcp_under_claude`.
    let mcp_exe = path_str(&ensure_built().lil_mcp);
    let mut mcp = env.spawn_stdio(&[
        OsStr::new("cmd.exe"),
        OsStr::new("/d"),
        OsStr::new("/c"),
        OsStr::new("set"),
        OsStr::new("LIL_TEST_CLIENT_PROTOCOL=0.9&&"),
        OsStr::new("claude.exe"),
        OsStr::new("--spawn-stdio"),
        OsStr::new(&mcp_exe),
    ]);
    mcp_initialize(&mut mcp);
    let tools = mcp_tools(&mut mcp);
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(
        names,
        ["lil_status"],
        "only lil_status on an incompatible core"
    );
    let status = mcp_call(&mut mcp, "lil_status", json!({}));
    let reported = payload(&status.json);
    assert_eq!(reported["status"], "incompatible", "{}", status.text);
    assert_eq!(
        reported["error"]["code"], "protocol.incompatible",
        "{}",
        status.text
    );
}

#[test] // spec: ARCH-018
fn mcp_server_starts_the_core() {
    let env = TestEnv::new();
    let before = env.lil(&["--no-start", "status"]);
    assert_eq!(before.code, 3, "no core yet: {before:?}");
    let mut mcp = mcp(&env);
    mcp_tools(&mut mcp);
    let after = env.lil(&["--no-start", "status"]);
    assert_eq!(after.code, 0, "lil-mcp started the core: {after:?}");
}

#[test] // spec: ARCH-045, ARCH-047, ARCH-052
fn model_tool_calls_never_run_as_ui() {
    // Ruling P3-R9: a model's tool calls never run over a `ui:*` connection. Under Claude Code
    // they are `mcp` calls: agent jobs, and destructive calls wait for the person.
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &[]);
    let mut mcp = mcp(&env);

    let status = mcp_call(&mut mcp, "lil_status", json!({}));
    assert_eq!(payload(&status.json)["caller"], "mcp", "{}", status.text);
    let job = mcp_call(&mut mcp, "test_job", json!({"lane": "cpu"}));
    assert!(!job.is_error, "{}", job.text);
    assert_eq!(
        payload(&job.json)["job"]["priority"],
        "agent",
        "{}",
        job.text
    );

    let pending = mcp_call(&mut mcp, "test_destructive", json!({}));
    let confirmation = pending_id(&pending.json);
    let open = confirmations(&mut app, "pending");
    let entry = open
        .iter()
        .find(|c| c["id"] == confirmation.as_str())
        .unwrap_or_else(|| panic!("no pending {confirmation}: {open:?}"));
    assert_eq!(entry["caller_kind"], "mcp", "{entry}");
    assert_eq!(
        runs_so_far(&mut app),
        0,
        "the model's destructive call did not run"
    );
}
