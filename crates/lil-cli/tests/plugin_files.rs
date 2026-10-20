//! Plugin 1.3's files as Claude Code reads them, the content carried from plugin 1.2.0, and the
//! hooks in a workspace from before the rename (spec ARCH-013, ARCH-101, ARCH-103, AI-107,
//! AI-109; Appendix F "Plugin 1.3"; ruling P3-R6).

use std::path::{Path, PathBuf};

use lil_platform::names::{LEGACY_WORKSPACE_FILE, WORKSPACE_FILE};
use lil_testkit::{CoreFixture, RawPipe, TestEnv, failpoints, sha256_bytes, sha256_file};
use serde_json::{Value, json};

/// The plugin's folder in the repository; the plugin id is its last segment.
const PLUGIN_DIR: &str = "plugin/worldwright/";

/// The plugin as Claude Code names it in an install.
const PLUGIN: &str = "worldwright@lilith";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn plugin_dir() -> PathBuf {
    repo_root().join(PLUGIN_DIR)
}

/// The plugin's name: the last segment of its folder.
fn plugin_name() -> &'static str {
    PLUGIN_DIR
        .trim_start_matches("plugin/")
        .trim_end_matches('/')
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// Every file under `dir`.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// A token's start, the shortest run after it that counts, and whether that run must hold a
/// digit: the list of the spec test `plugin_hooks.rs` (the privacy scan has no secret
/// patterns).
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

/// The first token-looking text in `line`.
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
            let run: String = line[at + prefix.len()..]
                .chars()
                .take_while(|c| is_token_char(*c))
                .collect();
            if word_start
                && run.len() >= *min
                && (!needs_digit || run.chars().any(|c| c.is_ascii_digit()))
            {
                return Some(format!("{prefix}… ({} characters)", run.len()));
            }
        }
    }
    None
}

#[test] // spec: AI-107, ARCH-013, ARCH-101
fn plugin_files_are_valid_and_use_the_user_config_paths() {
    let plugin = plugin_dir();
    let market = read_json(&repo_root().join("plugin/.claude-plugin/marketplace.json"));
    assert_eq!(market["name"], "lilith", "{market}");
    assert!(market["owner"]["name"].is_string(), "{market}");
    let entries = market["plugins"].as_array().expect("plugins is a list");
    assert_eq!(entries.len(), 1, "{market}");
    assert_eq!(entries[0]["name"], plugin_name(), "{market}");
    assert_eq!(
        entries[0]["source"],
        format!("./{}", plugin_name()),
        "{market}"
    );
    assert_eq!(
        format!(
            "{}@{}",
            entries[0]["name"].as_str().unwrap(),
            market["name"].as_str().unwrap()
        ),
        PLUGIN
    );

    let manifest = read_json(&plugin.join(".claude-plugin/plugin.json"));
    assert_eq!(manifest["name"], plugin_name());
    assert_eq!(manifest["version"], "1.3.0");
    let config = manifest["userConfig"].as_object().expect("userConfig");
    for field in ["lil_mcp_path", "lil_path"] {
        let entry = config[field]
            .as_object()
            .unwrap_or_else(|| panic!("{field}"));
        for text in ["title", "description"] {
            assert!(
                entry[text].as_str().is_some_and(|t| !t.trim().is_empty()),
                "{field}.{text}"
            );
        }
        assert_eq!(entry["type"], "file", "{field}");
        assert_eq!(entry["required"], true, "{field}");
        assert!(!entry.contains_key("default"), "{field} has no default");
    }

    let mcp = read_json(&plugin.join(".mcp.json"));
    assert_eq!(
        mcp["mcpServers"]["lil"]["command"],
        "${user_config.lil_mcp_path}"
    );

    let hooks = read_json(&plugin.join("hooks/hooks.json"));
    let events = hooks["hooks"].as_object().expect("hooks object");
    for (event, groups) in events {
        let arg = match event.as_str() {
            "SessionStart" => "session-start",
            "PreToolUse" => "pre-tool-use",
            "PostToolUse" => "post-tool-use",
            other => panic!("unexpected event {other}"),
        };
        for group in groups.as_array().expect("a list of groups") {
            for h in group["hooks"].as_array().expect("a list of hooks") {
                assert_eq!(
                    h,
                    &json!({"type": "command", "command": "${user_config.lil_path}",
                            "args": ["hook", arg], "onFailure": "block"}),
                    "{event}: exec form, the event as `lil hook` takes it, failing closed"
                );
            }
        }
    }

    // A shell-form hook (no `args`) that names a `userConfig` value fails in Claude Code.
    let mut files = Vec::new();
    files_under(&repo_root().join("plugin"), &mut files);
    let mut found = Vec::new();
    for file in &files {
        if file.file_name().is_some_and(|n| n == "hooks.json") {
            let text = std::fs::read_to_string(file).unwrap();
            let v: Value = serde_json::from_str(&text).unwrap();
            for groups in v["hooks"].as_object().into_iter().flatten().map(|(_, g)| g) {
                for group in groups.as_array().into_iter().flatten() {
                    for h in group["hooks"].as_array().into_iter().flatten() {
                        let shell_form = h.get("args").is_none();
                        if shell_form && h.to_string().contains("${user_config.") {
                            found.push(format!(
                                "{}: shell form with userConfig: {h}",
                                file.display()
                            ));
                        }
                    }
                }
            }
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if let Some(what) = token_in(line) {
                found.push(format!("{}:{}: {what}", file.display(), n + 1));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// `carried-1.2.0.toml`: one `[[file]] path, sha256` per file copied from plugin 1.2.0.
fn carried() -> Vec<(String, String)> {
    let path = plugin_dir().join("carried-1.2.0.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let table: toml::Table = text.parse().expect("carried-1.2.0.toml is TOML");
    table["file"]
        .as_array()
        .expect("[[file]] entries")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().expect("path").to_string(),
                f["sha256"].as_str().expect("sha256").to_string(),
            )
        })
        .collect()
}

#[test] // spec: ARCH-101
fn plugin_carries_1_2_0_content() {
    let listed = carried();
    assert!(
        listed.iter().any(|(p, _)| p.starts_with("skills/")),
        "{listed:?}"
    );
    assert!(
        listed.iter().any(|(p, _)| p.starts_with("agents/")),
        "{listed:?}"
    );
    for (rel, sha) in &listed {
        let file = plugin_dir().join(rel);
        assert!(file.is_file(), "{rel} is listed but missing");
        assert_eq!(
            &sha256_file(&file),
            sha,
            "{rel} changed since it was carried"
        );
    }
    let mut files = Vec::new();
    for sub in ["skills", "agents"] {
        files_under(&plugin_dir().join(sub), &mut files);
    }
    for file in files {
        let rel = file
            .strip_prefix(plugin_dir())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel.starts_with("skills/lilith/") {
            continue;
        }
        assert!(
            listed.iter().any(|(p, _)| *p == rel),
            "{rel} is not in carried-1.2.0.toml"
        );
    }
    let mcp = read_json(&plugin_dir().join(".mcp.json"));
    for server in ["lil", "civitai", "huggingface"] {
        assert!(mcp["mcpServers"].get(server).is_some(), "{server}: {mcp}");
    }
}

#[test] // spec: AI-109, ARCH-101
fn every_skill_is_in_the_inventory() {
    let text = std::fs::read_to_string(plugin_dir().join("skills-inventory.toml")).unwrap();
    let table: toml::Table = text.parse().expect("skills-inventory.toml is TOML");
    let listed: Vec<(String, bool)> = table["skill"]
        .as_array()
        .expect("[[skill]] entries")
        .iter()
        .map(|s| {
            (
                s["name"].as_str().expect("name").to_string(),
                s["writes_directly"].as_bool().expect("writes_directly"),
            )
        })
        .collect();
    assert!(
        listed.contains(&("lilith".to_string(), false)),
        "{listed:?}"
    );
    let skills = std::fs::read_dir(plugin_dir().join("skills")).unwrap();
    for skill in skills.map(Result::unwrap) {
        let name = skill.file_name().to_string_lossy().into_owned();
        assert!(
            listed.iter().any(|(n, _)| *n == name),
            "skills/{name} is not in skills-inventory.toml"
        );
    }
    let agents: Vec<String> = table["agent"]
        .as_array()
        .expect("[[agent]] entries")
        .iter()
        .map(|a| a["name"].as_str().expect("name").to_string())
        .collect();
    for agent in std::fs::read_dir(plugin_dir().join("agents")).unwrap() {
        let name = agent.unwrap().path();
        let name = name.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(
            agents.contains(&name),
            "agents/{name}.md is not in skills-inventory.toml"
        );
    }
}

/// The fixture workspace, as a workspace from before the rename: its workspace file has the
/// legacy name. Registered as the default.
fn plugin_era_workspace(env: &TestEnv) -> PathBuf {
    let ws = env.copy_fixture_workspace("plugin-era");
    std::fs::rename(ws.join(WORKSPACE_FILE), ws.join(LEGACY_WORKSPACE_FILE)).unwrap();
    env.register_workspace(&ws, true);
    ws
}

fn path_str(path: &Path) -> String {
    path.to_str().expect("test paths are UTF-8").to_string()
}

fn hook_out(env: &TestEnv, ws: &Path, event: &str, input: &Value) -> String {
    let out = env.lil_in_with(ws, &["hook", event], &[], Some(&input.to_string()));
    assert_eq!(out.code, 0, "{out:?}");
    out.stdout
}

#[test] // spec: ARCH-103, AI-116
fn hooks_work_in_a_plugin_era_workspace() {
    let env = TestEnv::new();
    let ws = plugin_era_workspace(&env);
    assert!(!ws.join(WORKSPACE_FILE).exists());
    let points = failpoints::spec(&[("watch.start", "unsupported")]);
    let _core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &points)]);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");

    // SessionStart reports the open job.
    let job = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "gated": true}),
        Some("plugin-era-job"),
    );
    let job_id = job["job"]["job_id"].as_str().expect("a job id").to_string();
    let start = json!({"session_id": "s", "hook_event_name": "SessionStart", "source": "startup",
                       "cwd": path_str(&ws)});
    let context = hook_out(&env, &ws, "session-start", &start);
    assert!(context.contains(&job_id), "{context}");

    // PostToolUse tells the core about an edited sealed file.
    let asset = json!({"project": "sketchbook", "asset_id": "chr_paper_crane"});
    let edits = |pipe: &mut RawPipe| {
        pipe.tool_ok("asset.get", asset.clone(), None)["human_edits"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let before = edits(&mut pipe).len();
    let sealed = ws.join(r"projects\sketchbook\assets\chr_paper_crane\v1.0.0\crane_b.png");
    let mut permissions = std::fs::metadata(&sealed).unwrap().permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as an editor would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&sealed, permissions).unwrap();
    let edited = b"edited in a plugin-era workspace";
    std::fs::write(&sealed, edited).unwrap();
    let post = json!({"session_id": "s", "hook_event_name": "PostToolUse", "tool_name": "Edit",
                      "cwd": path_str(&ws), "tool_input": {"file_path": path_str(&sealed)},
                      "tool_response": {}});
    assert_eq!(hook_out(&env, &ws, "post-tool-use", &post), "");
    let after = edits(&mut pipe);
    assert_eq!(after.len(), before + 1, "{after:?}");
    assert!(
        after
            .iter()
            .any(|e| e["sha256"] == sha256_bytes(edited).as_str())
    );

    pipe.tool_ok(
        "test.job_release",
        json!({ "job_id": job_id }),
        Some("plugin-era-release"),
    );
}
