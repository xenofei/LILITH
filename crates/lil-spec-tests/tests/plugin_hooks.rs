//! `lil hook` and plugin 1.3: deletes, installs and script runs ask; writes to a `suite.toml`
//! ask and reads don't; harmless commands pass; the comfy MCP's risky tools ask, and its launch
//! and stop are denied while a core runs; direct log and manifest writes only warn; hooks do
//! nothing in Engine A; SessionStart reports jobs and confirmations; PostToolUse tells the
//! core; and the plugin's files use the `userConfig` paths (spec ARCH-013, ARCH-056, ARCH-101,
//! ARCH-103, ARCH-104, ARCH-105, ARCH-106, ARCH-107, AI-107, AI-116, CORE-008, CORE-116,
//! QA-061; Appendix F "Plugin 1.3", "The `lil` commands this plan adds"). Driven through
//! `lil hook …` with the hook JSON on stdin, and the plugin files as Claude Code reads them
//! (QA-018).

mod support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use lil_testkit::{CoreFixture, LilOutput, TestApp, TestEnv, failpoints, sha256_bytes};
use serde_json::{Value, json};
use support::*;

/// The plugin's folder in the repository; the plugin id is its last segment.
const PLUGIN_DIR: &str = "plugin/worldwright/";

/// Runs `lil hook <event>` in `dir` with `input` on stdin and `extra` variables.
fn hook_in(
    env: &TestEnv,
    dir: &Path,
    event: &str,
    input: &Value,
    extra: &[(&str, OsString)],
) -> LilOutput {
    let out = lil_with(env, dir, &["hook", event], extra, Some(&input.to_string()));
    assert_eq!(out.code, 0, "`lil hook {event}` always exits 0: {out:?}");
    out
}

/// The hook output JSON, or `None` when the hook printed nothing.
fn output(out: &LilOutput) -> Option<Value> {
    let text = out.stdout.trim();
    if text.is_empty() {
        return None;
    }
    Some(
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("hook output is not JSON ({e}): {out:?}")),
    )
}

/// The PreToolUse `permissionDecision`, if any.
fn decision(out: &LilOutput) -> Option<String> {
    output(out).and_then(|v| {
        v["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .map(str::to_string)
    })
}

/// A PreToolUse input for `tool_name` with `tool_input`.
fn pre_tool_use(env: &TestEnv, tool_name: &str, tool_input: Value) -> Value {
    json!({"session_id": "spec-session", "transcript_path": path_str(&env.root().join("t.jsonl")),
           "cwd": path_str(env.root()), "permission_mode": "default",
           "hook_event_name": "PreToolUse", "tool_name": tool_name, "tool_input": tool_input})
}

/// The decision for a shell command.
fn shell_decision(env: &TestEnv, shell: &str, command: &str) -> Option<String> {
    let input = pre_tool_use(env, shell, json!({ "command": command }));
    decision(&hook_in(env, env.root(), "pre-tool-use", &input, &[]))
}

/// The decision for a tool call with `tool_input`.
fn tool_decision(env: &TestEnv, tool_name: &str, tool_input: Value) -> Option<String> {
    let input = pre_tool_use(env, tool_name, tool_input);
    decision(&hook_in(env, env.root(), "pre-tool-use", &input, &[]))
}

const BOTH: &[&str] = &["Bash", "PowerShell"];
const BASH: &[&str] = &["Bash"];
const PWSH: &[&str] = &["PowerShell"];

#[test] // spec: ARCH-105, CORE-116, QA-061
fn delete_install_and_script_commands_ask() {
    let env = TestEnv::new();
    let cases: &[(&str, &[&str])] = &[
        ("rm -rf build", BOTH),
        ("del x.txt", PWSH),
        ("Remove-Item -Recurse a", PWSH),
        ("git clean -fdx", BOTH),
        ("git reset --hard", BOTH),
        ("pip install x", BOTH),
        ("uv pip install x", BOTH),
        ("npm install", BOTH),
        ("comfy node install x", BOTH),
        ("python tool.py", BOTH),
        ("blender -b -P s.py", BOTH),
        ("iex (irm u)", PWSH),
        ("cd a && rm b", BOTH),
        ("lil-mcp", BOTH),
        (r#"& "C:\x\lil-mcp.exe""#, PWSH),
    ];
    let mut wrong = Vec::new();
    for (command, shells) in cases {
        for shell in *shells {
            let got = shell_decision(&env, shell, command);
            if got.as_deref() != Some("ask") {
                wrong.push(format!("{shell} `{command}`: {got:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "these must ask:\n{}", wrong.join("\n"));
}

#[test] // spec: ARCH-056, CORE-008
fn suite_toml_writes_ask() {
    let env = TestEnv::new();
    // `<APPDATA>` is the test environment's own roaming folder.
    let appdata = env.root().join("roaming");
    let suite = appdata.join("LILITH").join("suite.toml");
    let channel_suite = env.env.suite_toml();
    let mut wrong = Vec::new();
    let mut expect = |what: String, got: Option<String>, want: Option<&str>| {
        if got.as_deref() != want {
            wrong.push(format!("{what}: {got:?}, expected {want:?}"));
        }
    };
    for file in [&suite, &channel_suite] {
        let path = path_str(file);
        expect(
            format!("Write {path}"),
            tool_decision(&env, "Write", json!({"file_path": path, "content": "x"})),
            Some("ask"),
        );
        expect(
            format!("Edit {path}"),
            tool_decision(
                &env,
                "Edit",
                json!({"file_path": path, "old_string": "a", "new_string": "b"}),
            ),
            Some("ask"),
        );
    }
    for (shell, command, want) in [
        (
            "Bash",
            r#"echo x >> "$APPDATA/LILITH/suite.toml""#,
            Some("ask"),
        ),
        (
            "PowerShell",
            r"Set-Content $env:APPDATA\LILITH\suite.toml x",
            Some("ask"),
        ),
        (
            "PowerShell",
            r"Get-Content $env:APPDATA\LILITH\suite.toml",
            None,
        ),
        ("Bash", r#"grep caps "$APPDATA/LILITH/suite.toml""#, None),
    ] {
        expect(
            format!("{shell} `{command}`"),
            shell_decision(&env, shell, command),
            want,
        );
    }
    let notes = path_str(&env.root().join("notes").join("suite.toml"));
    expect(
        format!("Write {notes}"),
        tool_decision(&env, "Write", json!({"file_path": notes, "content": "x"})),
        None,
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test] // spec: QA-061
fn harmless_commands_pass() {
    let env = TestEnv::new();
    let cases: &[(&str, &[&str])] = &[
        ("git status", BOTH),
        ("ls -la", BASH),
        ("Get-ChildItem", PWSH),
        ("cat notes.md", BOTH),
        ("python --version", BOTH),
        ("pip list", BOTH),
        ("echo rm", BOTH),
        (r#"grep -r "pip install" ."#, BASH),
    ];
    let mut wrong = Vec::new();
    for (command, shells) in cases {
        for shell in *shells {
            if let Some(got) = shell_decision(&env, shell, command) {
                wrong.push(format!("{shell} `{command}`: {got}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "these must pass without a decision:\n{}",
        wrong.join("\n")
    );
}

#[test] // spec: ARCH-105, CORE-116
fn comfy_mcp_install_update_download_restart_ask() {
    let env = TestEnv::new();
    for tool in [
        "install_node",
        "update_comfyui",
        "switch_comfyui_version",
        "download_model",
        "download",
        "restart_comfyui",
    ] {
        let name = format!("mcp__comfyui__{tool}");
        assert_eq!(
            tool_decision(&env, &name, json!({})).as_deref(),
            Some("ask"),
            "{name}"
        );
    }
}

#[test] // spec: ARCH-106
fn comfy_mcp_launch_and_stop_are_denied_while_a_core_runs() {
    let env = TestEnv::new();
    let tools = ["mcp__comfyui__launch_comfyui", "mcp__comfyui__stop_comfyui"];
    for name in tools {
        assert_eq!(
            tool_decision(&env, name, json!({})),
            None,
            "{name} without a core"
        );
    }
    let _core = CoreFixture::start(&env);
    for name in tools {
        assert_eq!(
            tool_decision(&env, name, json!({})).as_deref(),
            Some("deny"),
            "{name} while a core runs"
        );
    }
}

#[test] // spec: ARCH-104, AI-116
fn direct_log_and_manifest_writes_only_warn() {
    let env = TestEnv::new();
    let ws = env.root().join("ws-direct");
    for rel in [
        r"queue\jobs.x.jsonl",
        r"registry\usage.x.jsonl",
        r"projects\sketchbook\assets\kite\kite.manifest.json",
    ] {
        let path = path_str(&ws.join(rel));
        let input = pre_tool_use(&env, "Write", json!({"file_path": path, "content": "{}"}));
        let out = hook_in(&env, env.root(), "pre-tool-use", &input, &[]);
        let printed = output(&out).unwrap_or_else(|| panic!("no warning for {path}: {out:?}"));
        assert!(
            printed["systemMessage"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "{path}: {printed}"
        );
        assert!(
            printed["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none(),
            "{path}: a warning, not a decision: {printed}"
        );
    }
}

#[test] // spec: ARCH-107
fn hooks_do_nothing_in_engine_a() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let engine = [("LIL_ENGINE", OsString::from("1"))];
    let cwd = path_str(env.root());
    for (event, input) in [
        (
            "session-start",
            json!({"hook_event_name": "SessionStart", "source": "startup", "cwd": cwd}),
        ),
        (
            "pre-tool-use",
            pre_tool_use(&env, "Bash", json!({"command": "rm -rf build"})),
        ),
        (
            "post-tool-use",
            json!({"hook_event_name": "PostToolUse", "tool_name": "Write", "cwd": cwd,
                   "tool_input": {"file_path": path_str(&env.root().join("x.txt"))},
                   "tool_response": {}}),
        ),
    ] {
        let out = hook_in(&env, env.root(), event, &input, &engine);
        assert_eq!(
            out.stdout, "",
            "`lil hook {event}` printed something in Engine A: {out:?}"
        );
    }
}

/// The SessionStart `additionalContext` for a session in `ws`, if any.
fn session_start(env: &TestEnv, ws: &Path) -> Option<String> {
    let input = json!({"session_id": "spec-session", "hook_event_name": "SessionStart",
                       "source": "startup", "cwd": path_str(ws)});
    let out = hook_in(env, ws, "session-start", &input, &[]);
    output(&out).map(|v| {
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| panic!("no additionalContext: {v}"))
            .to_string()
    })
}

#[test] // spec: ARCH-103, AI-116
fn session_start_reports_jobs_and_confirmations() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "hooks");
    assert_eq!(session_start(&env, &ws), None, "nothing without a core");

    let _core = CoreFixture::start(&env);
    let _app = TestApp::start(&env, Some(&ws), &[]);
    let mut pipe = session(&env, Some(&ws));
    ok(
        &mut pipe,
        "project.status",
        json!({"project": "sketchbook"}),
        None,
    );
    let job = submit(&mut pipe, "test.job", json!({"lane": "cpu", "gated": true}));
    let confirmation = pending_id(&pipe.tool("test.destructive", json!({}), Some(&key())));

    let context = session_start(&env, &ws).expect("context while a core runs");
    assert!(
        context.contains("sketchbook"),
        "the active project: {context}"
    );
    assert!(context.contains(&job), "the open job's id: {context}");
    assert!(
        context.contains(&confirmation),
        "the pending confirmation's id: {context}"
    );
    release(&mut pipe, &job);
}

#[test] // spec: ARCH-103
fn post_tool_use_tells_the_core() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "posttool");
    let points = failpoints::spec(&[("watch.start", "unsupported")]);
    let _core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &points)]);
    let mut pipe = session(&env, Some(&ws));
    let asset = json!({"project": "sketchbook", "asset_id": "chr_paper_crane"});
    let before = ok(&mut pipe, "asset.get", asset.clone(), None);
    let edits_before = before["human_edits"].as_array().map_or(0, Vec::len);

    let sealed = ws.join(r"projects\sketchbook\assets\chr_paper_crane\v1.0.0\crane_b.png");
    let mut permissions = std::fs::metadata(&sealed).unwrap().permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as an editor would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&sealed, permissions).unwrap();
    let edited = b"edited by an agent's Edit tool";
    std::fs::write(&sealed, edited).unwrap();

    let input = json!({"session_id": "spec-session", "hook_event_name": "PostToolUse",
                       "tool_name": "Edit", "cwd": path_str(&ws),
                       "tool_input": {"file_path": path_str(&sealed)}, "tool_response": {}});
    hook_in(&env, &ws, "post-tool-use", &input, &[]);

    let after = ok(&mut pipe, "asset.get", asset, None);
    let edits = after["human_edits"].as_array().cloned().unwrap_or_default();
    assert_eq!(edits.len(), edits_before + 1, "{after}");
    assert!(
        edits
            .iter()
            .any(|e| e["sha256"] == sha256_bytes(edited).as_str()),
        "{after}"
    );
}

/// The repository root: two folders above this crate.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/lil-spec-tests lives two folders below the repository root")
        .to_path_buf()
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// A token's start (a well-known secret prefix), the shortest run after it that counts, and
/// whether that run must hold a digit (`sk-` and `hf_` also start ordinary words and names).
/// The privacy scan has no secret patterns, so the list is the test's own.
const TOKEN_PREFIXES: &[(&str, usize, bool)] = &[
    ("ghp_", 30, false),
    ("gho_", 30, false),
    ("ghs_", 30, false),
    ("github_pat_", 30, false),
    ("hf_", 30, true),
    ("sk-ant-", 20, false),
    ("sk-", 32, true),
    ("AKIA", 16, false),
    ("AIza", 30, false),
    ("xoxb-", 20, false),
    ("xoxp-", 20, false),
];

/// The first token-looking text in `line`: a prefix at a word start (no letter, digit or `_`
/// before it), then a long enough run of token characters.
fn token_in(line: &str) -> Option<String> {
    if line.contains("PRIVATE KEY-----") {
        return Some("a private key block".to_string());
    }
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    for (prefix, min, needs_digit) in TOKEN_PREFIXES {
        for (at, _) in line.match_indices(prefix) {
            let word_start = line[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
            if !word_start {
                continue;
            }
            let run: String = line[at + prefix.len()..]
                .chars()
                .take_while(|c| is_token_char(*c))
                .collect();
            if run.len() >= *min && (!needs_digit || run.chars().any(|c| c.is_ascii_digit())) {
                return Some(format!("{prefix}… ({} characters)", run.len()));
            }
        }
    }
    None
}

/// Every file under `dir`.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
    for entry in entries.map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// The `args` a lil hook for `event` (a `hooks.json` key) must carry.
fn hook_args(event: &str) -> Value {
    let arg = match event {
        "SessionStart" => "session-start",
        "PreToolUse" => "pre-tool-use",
        "PostToolUse" => "post-tool-use",
        other => panic!("a lil hook on an unexpected event {other}"),
    };
    json!(["hook", arg])
}

/// `plugin.json` has `name: "worldwright"`, `version: "1.3.0"`, and the two `userConfig`
/// paths; `.mcp.json` and `hooks/hooks.json` name them (ruling P3-R6).
#[test] // spec: ARCH-013, AI-107, ARCH-101
fn plugin_uses_the_user_config_paths() {
    let plugin = repo_root().join(PLUGIN_DIR);
    let id = PLUGIN_DIR
        .trim_start_matches("plugin/")
        .trim_end_matches('/');

    let manifest = read_json(&plugin.join(".claude-plugin").join("plugin.json"));
    assert_eq!(manifest["name"], id, "{manifest}");
    assert_eq!(manifest["version"], "1.3.0", "{manifest}");
    for field in ["lil_mcp_path", "lil_path"] {
        let entry = &manifest["userConfig"][field];
        for text in ["title", "description"] {
            assert!(
                entry[text].as_str().is_some_and(|t| !t.trim().is_empty()),
                "userConfig.{field}.{text}: {entry}"
            );
        }
        assert_eq!(entry["type"], "file", "userConfig.{field}: {entry}");
        assert_eq!(entry["required"], true, "userConfig.{field}: {entry}");
        assert!(
            entry.get("default").is_none(),
            "userConfig.{field} has no default: {entry}"
        );
    }

    let mcp = read_json(&plugin.join(".mcp.json"));
    let servers = &mcp["mcpServers"];
    assert_eq!(
        servers["lil"]["command"], "${user_config.lil_mcp_path}",
        "{servers}"
    );
    for kept in ["civitai", "huggingface"] {
        assert!(
            servers.get(kept).is_some(),
            "plugin 1.2.0's {kept} server is gone: {servers}"
        );
    }

    let hooks = read_json(&plugin.join("hooks").join("hooks.json"));
    let events = hooks["hooks"]
        .as_object()
        .unwrap_or_else(|| panic!("hooks.json has no hooks object: {hooks}"));
    let mut lil_hooks: Vec<(String, String)> = Vec::new();
    for (event, groups) in events {
        for group in groups.as_array().into_iter().flatten() {
            let matcher = group["matcher"].as_str().unwrap_or_default().to_string();
            // Every hook is the exec-form lil hook (plugin 1.2.0 had no `hooks.json`), and a
            // hook that fails blocks its action (ruling P3-R30).
            for h in group["hooks"].as_array().into_iter().flatten() {
                let exec = json!({"type": "command", "command": "${user_config.lil_path}",
                                  "args": hook_args(event), "onFailure": "block"});
                assert_eq!(h, &exec, "{event}: every hook is exec form");
                lil_hooks.push((event.clone(), matcher.clone()));
            }
        }
    }
    lil_hooks.sort();
    let events: Vec<&str> = lil_hooks.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(
        events,
        ["PostToolUse", "PreToolUse", "SessionStart"],
        "one lil hook per event: {lil_hooks:?}"
    );
    let matcher = |event: &str| {
        lil_hooks
            .iter()
            .find(|(e, _)| e == event)
            .map(|(_, m)| m.as_str())
    };
    assert_eq!(
        matcher("PreToolUse"),
        Some("Bash|PowerShell|Write|Edit|MultiEdit|mcp__comfyui__.*")
    );
    assert_eq!(matcher("PostToolUse"), Some("Write|Edit|MultiEdit"));

    let mut files = Vec::new();
    files_under(&repo_root().join("plugin"), &mut files);
    let mut found = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if let Some(what) = token_in(line) {
                found.push(format!("{}:{}: {what}", file.display(), n + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "no secret is ever copied into the plugin:\n{}",
        found.join("\n")
    );
}
