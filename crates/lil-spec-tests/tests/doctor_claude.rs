//! `lil doctor`'s Claude Code checks: the plugin is installed or updated in place only after
//! you type `y`, never removed, and nothing outside the Claude Code profile changes; the
//! workspace's `.claude/settings.json` allow and ask rules are written only after `y`, and a
//! second run changes nothing (spec AI-114, AI-117, ARCH-108; Appendix F "The `lil` commands
//! this plan adds"). `PATH` holds the fake `claude`, which reads `CLAUDE_CONFIG_DIR` and logs the
//! plugin commands it is given. Driven through the real `lil` and the files on disk (QA-018).

mod support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use lil_testkit::{LilOutput, TestEnv, sha256_file};
use serde_json::Value;
use support::*;

/// The plugin as Claude Code names it in an install.
const PLUGIN: &str = "worldwright@lilith";

/// The test's default Claude Code profile (`CLAUDE_CONFIG_DIR`).
fn profile(env: &TestEnv) -> PathBuf {
    let dir = env.env.claude_default_profile_dir();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `lil doctor` in `ws` with `answer` typed at every question, the fake `claude` on `PATH`.
fn doctor(env: &TestEnv, ws: &Path, answer: &str) -> LilOutput {
    let stdin = format!("{answer}\n").repeat(20);
    let extra = [
        ("PATH", env.path_with_testbins()),
        ("CLAUDE_CONFIG_DIR", OsString::from(profile(env))),
    ];
    let out = lil_with(env, ws, &["doctor"], &extra, Some(&stdin));
    assert!(out.code == 0 || out.code == 1, "{out:?}");
    out
}

/// What the fake `claude` was asked to do, one argument list per line.
fn claude_log(env: &TestEnv) -> Vec<Vec<String>> {
    jsonl(&profile(env).join("fake-claude.log"))
        .into_iter()
        .map(|line| {
            line.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// `claude plugin list --json` reports the plugin at `version`.
fn installed(env: &TestEnv, version: &str) {
    let list = format!(
        r#"[{{"id": "worldwright@lilith", "version": "{version}", "scope": "user", "enabled": true}}]"#
    );
    std::fs::write(profile(env).join("fake-plugins.json"), list).unwrap();
}

/// Every file under `dir` except the folder `skip`, with its SHA-256.
fn tree(dir: &Path, skip: &Path, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.map(Result::unwrap) {
        let path = entry.path();
        if path == skip {
            continue;
        }
        if path.is_dir() {
            tree(&path, skip, out);
        } else {
            out.push((path.clone(), sha256_file(&path)));
        }
    }
}

/// The Claude Code side outside `CLAUDE_CONFIG_DIR`: the profile folder and the suite's other
/// Claude Code profiles.
fn outside_profile(env: &TestEnv) -> Vec<(PathBuf, String)> {
    let skip = profile(env);
    let mut out = Vec::new();
    tree(&env.root().join("profile"), &skip, &mut out);
    tree(&env.env.claude_profiles_dir(), &skip, &mut out);
    out.sort();
    out
}

/// The non-empty stdout lines.
fn lines(out: &LilOutput) -> Vec<String> {
    out.stdout
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.trim().is_empty())
        .collect()
}

#[test] // spec: AI-117, ARCH-108
fn doctor_installs_the_plugin_only_after_yes() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "doctorcc");
    profile(&env);
    let untouched = outside_profile(&env);

    let declined = doctor(&env, &ws, "n");
    assert!(
        lines(&declined)
            .iter()
            .any(|l| l.starts_with("problem:") && l.contains("plugin")),
        "a missing plugin is a problem: {declined:?}"
    );
    assert!(
        !claude_log(&env)
            .iter()
            .any(|a| a.iter().any(|w| w == "install")),
        "installed without a yes: {:?}",
        claude_log(&env)
    );

    doctor(&env, &ws, "y");
    let log = claude_log(&env);
    assert!(
        log.iter()
            .any(|a| a.windows(2).any(|w| w == ["install", PLUGIN])),
        "no `plugin install {PLUGIN}` after a yes: {log:?}"
    );
    assert_eq!(
        outside_profile(&env),
        untouched,
        "nothing outside CLAUDE_CONFIG_DIR changed"
    );

    // An installed older plugin is installed over in place, never removed.
    installed(&env, "1.2.0");
    std::fs::remove_file(profile(&env).join("fake-claude.log")).unwrap();
    doctor(&env, &ws, "y");
    let log = claude_log(&env);
    assert!(
        log.iter()
            .any(|a| a.windows(2).any(|w| w == ["install", PLUGIN])),
        "1.2.0 was not installed over: {log:?}"
    );
    assert!(
        !log.iter()
            .any(|a| a.iter().any(|w| w == "uninstall" || w == "remove")),
        "the plugin is never removed: {log:?}"
    );
}

#[test] // spec: AI-114
fn doctor_writes_claude_rules_only_after_yes() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "doctorrules");
    installed(&env, "1.3.0");
    let settings = ws.join(".claude").join("settings.json");

    doctor(&env, &ws, "n");
    assert!(!settings.exists(), "written without a yes");

    doctor(&env, &ws, "y");
    let text = std::fs::read_to_string(&settings)
        .unwrap_or_else(|e| panic!("no {} after a yes: {e}", settings.display()));
    let rules: Value = serde_json::from_str(&text).unwrap();
    let has = |list: &str, rule: &str| {
        rules["permissions"][list]
            .as_array()
            .is_some_and(|l| l.iter().any(|r| r == rule))
    };
    for rule in [
        "mcp__plugin_worldwright_lil__asset_get",
        "mcp__lil__asset_get",
    ] {
        assert!(has("allow", rule), "permissions.allow lacks {rule}: {text}");
    }
    for rule in [
        "mcp__plugin_worldwright_lil__resource_delete",
        "mcp__lil__resource_delete",
    ] {
        assert!(has("ask", rule), "permissions.ask lacks {rule}: {text}");
    }
    let written = sha256_file(&settings);

    let again = doctor(&env, &ws, "y");
    let about: Vec<String> = lines(&again)
        .into_iter()
        .filter(|l| l.contains("settings.json"))
        .collect();
    assert!(
        !about.is_empty(),
        "the second run reports the rules: {again:?}"
    );
    assert!(
        about.iter().all(|l| l.starts_with("ok:")),
        "the second run only confirms the rules: {about:?}"
    );
    assert_eq!(
        sha256_file(&settings),
        written,
        "the second run changed the file"
    );
}
