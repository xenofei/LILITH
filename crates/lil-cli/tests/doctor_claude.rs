//! `lil doctor`'s Claude Code checks against the fake `claude` (spec AI-110, AI-114, AI-117,
//! ARCH-104, ARCH-108; Appendix F "The `lil` commands this plan adds"; owner question 9, ruling
//! P3-R6). `PATH` holds the test binaries, so the fake `claude.exe` is found; it reads
//! `CLAUDE_CONFIG_DIR`, a folder of the test, and logs the plugin commands it is given. No test
//! here reaches a real Claude Code profile.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use lil_testkit::{LilOutput, RawPipe, TestEnv, ensure_built, sha256_file};
use serde_json::{Value, json};

/// The plugin as Claude Code names it in an install.
const PLUGIN: &str = "worldwright@lilith";

/// The test's default Claude Code profile (`CLAUDE_CONFIG_DIR`).
fn profile(env: &TestEnv) -> PathBuf {
    let dir = env.env.claude_default_profile_dir();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `lil doctor` in `dir`, `answer` typed at every question, with the fake `claude` on `PATH`.
fn doctor(env: &TestEnv, dir: &Path, answer: &str) -> LilOutput {
    let stdin = format!("{answer}\n").repeat(20);
    let extra = [
        ("PATH", env.path_with_testbins()),
        ("CLAUDE_CONFIG_DIR", OsString::from(profile(env))),
    ];
    let out = env.lil_in_with(dir, &["doctor"], &extra, Some(&stdin));
    assert!(out.code == 0 || out.code == 1, "{out:?}");
    out
}

/// What the fake `claude` was asked to do, one argument list per line.
fn claude_log(env: &TestEnv) -> Vec<Vec<String>> {
    let Ok(text) = std::fs::read_to_string(profile(env).join("fake-claude.log")) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| serde_json::from_str::<Vec<String>>(line).unwrap())
        .collect()
}

/// `claude plugin list --json` reports the plugin at `version`.
fn installed(env: &TestEnv, version: &str) {
    let list = json!([{"id": PLUGIN, "version": version, "scope": "user", "enabled": true,
                       "installPath": "x"}]);
    std::fs::write(profile(env).join("fake-plugins.json"), list.to_string()).unwrap();
}

/// The profile's `settings.json` with the plugin's options set to `options`.
fn plugin_options(env: &TestEnv, options: Value) -> PathBuf {
    let file = profile(env).join("settings.json");
    let settings = json!({"theme": "dark", "pluginConfigs": {PLUGIN: {"options": options}}});
    std::fs::write(&file, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    file
}

fn lines(out: &LilOutput) -> Vec<String> {
    out.stdout
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn ran(log: &[Vec<String>], words: &[&str]) -> bool {
    log.iter()
        .any(|a| a.windows(words.len()).any(|w| w == words))
}

#[test] // spec: AI-117, ARCH-108
fn doctor_plugin_install_only_after_yes_in_the_default_profile() {
    let env = TestEnv::new();
    let settings = plugin_options(&env, json!({}));
    let untouched = sha256_file(&settings);

    // Not installed: `n` installs nothing.
    let declined = doctor(&env, env.root(), "n");
    assert!(
        lines(&declined)
            .iter()
            .any(|l| l.starts_with("problem:") && l.contains("plugin")),
        "{declined:?}"
    );
    assert!(claude_log(&env).is_empty(), "{:?}", claude_log(&env));

    // Plugin 1.2.0: `y` updates it in place, never removes it.
    installed(&env, "1.2.0");
    let yes = doctor(&env, env.root(), "y");
    let log = claude_log(&env);
    assert!(ran(&log, &["plugin", "marketplace", "add"]), "{log:?}");
    assert!(ran(&log, &["plugin", "install", PLUGIN]), "{log:?}");
    assert!(ran(&log, &["plugin", "update", PLUGIN]), "{log:?}");
    assert!(
        !log.iter()
            .flatten()
            .any(|w| w == "uninstall" || w == "remove" || w == "disable"),
        "{log:?}"
    );
    assert!(
        lines(&yes)
            .iter()
            .any(|l| l.starts_with("fixed:") && l.contains("1.3.0")),
        "{yes:?}"
    );

    // The options are unset: each is a problem naming the path to enter, and the doctor never
    // writes the profile's plugin settings.
    let folder = ensure_built().dir.display().to_string();
    for exe in ["lil-mcp.exe", "lil.exe"] {
        let path = format!("{folder}\\{exe}");
        assert!(
            lines(&yes)
                .iter()
                .any(|l| l.starts_with("problem:") && l.contains(&path)),
            "no problem naming {path}: {yes:?}"
        );
    }
    assert_eq!(
        sha256_file(&settings),
        untouched,
        "the doctor wrote settings.json"
    );

    // A path outside the channel's trusted folder is a problem too; the trusted one is ok.
    installed(&env, "1.3.0");
    plugin_options(
        &env,
        json!({"lil_mcp_path": r"C:\elsewhere\lil-mcp.exe", "lil_path": format!("{folder}\\lil.exe")}),
    );
    let set = doctor(&env, env.root(), "y");
    let problems: Vec<String> = lines(&set)
        .into_iter()
        .filter(|l| l.starts_with("problem:") && l.contains("lil_"))
        .collect();
    assert_eq!(problems.len(), 1, "{set:?}");
    assert!(
        problems[0].contains("lil_mcp_path")
            && problems[0].contains(&format!("{folder}\\lil-mcp.exe")),
        "{problems:?}"
    );
}

#[test] // spec: AI-110, AI-114
fn doctor_claude_rules_name_both_tool_name_forms() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("rules");
    env.register_workspace(&ws, true);
    installed(&env, "1.3.0");
    let settings = ws.join(".claude").join("settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    // The person's own settings stay.
    std::fs::write(
        &settings,
        r#"{"permissions": {"allow": ["Bash(git status)"], "deny": ["mcp__lil__resource_delete"]}, "model": "x"}"#,
    )
    .unwrap();

    doctor(&env, &ws, "y");
    let rules: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let list = |name: &str| -> Vec<String> {
        rules["permissions"][name]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let (allow, ask, deny) = (list("allow"), list("ask"), list("deny"));
    assert_eq!(rules["model"], "x", "{rules}");
    assert!(allow.contains(&"Bash(git status)".to_string()), "{rules}");
    assert_eq!(
        deny,
        ["mcp__lil__resource_delete"],
        "the person's deny rule stays: {rules}"
    );

    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let listed = pipe.call("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools").clone();
    assert!(tools.len() > 10, "{listed}");
    let mut wrong = Vec::new();
    for tool in &tools {
        let name = tool["name"].as_str().unwrap().replace('.', "_");
        let risky = matches!(tool["effect"].as_str(), Some("destructive" | "exec"));
        let (here, other) = if risky {
            (&ask, &allow)
        } else {
            (&allow, &ask)
        };
        for form in [
            format!("mcp__plugin_worldwright_lil__{name}"),
            format!("mcp__lil__{name}"),
        ] {
            let placed = here.contains(&form) || deny.contains(&form);
            if !placed || other.contains(&form) {
                wrong.push(form);
            }
        }
    }
    assert!(wrong.is_empty(), "misplaced or missing: {wrong:?}\n{rules}");
    assert!(allow.contains(&"mcp__plugin_worldwright_lil__lil_status".to_string()));
}

#[test] // spec: ARCH-104
fn doctor_lists_skills_writing_directly() {
    let env = TestEnv::new();
    installed(&env, "1.3.0");
    let inventory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugin/worldwright/skills-inventory.toml");
    let table: toml::Table = std::fs::read_to_string(inventory).unwrap().parse().unwrap();
    let direct: Vec<String> = ["skill", "agent"]
        .iter()
        .flat_map(|kind| table[*kind].as_array().cloned().unwrap_or_default())
        .filter(|s| s["writes_directly"].as_bool() == Some(true))
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    let out = doctor(&env, env.root(), "n");
    let line = lines(&out)
        .into_iter()
        .find(|l| l.contains("writ") && l.contains("directly"))
        .unwrap_or_else(|| panic!("no skills line: {out:?}"));
    if direct.is_empty() {
        assert!(line.starts_with("ok:"), "{line}");
    } else {
        assert!(line.starts_with("info:"), "{line}");
        for name in &direct {
            assert!(line.contains(name.as_str()), "{name} missing: {line}");
        }
    }
    assert!(
        !line.contains("lilith,") && !line.ends_with("lilith"),
        "{line}"
    );
}

#[test] // spec: AI-117, ARCH-108
fn another_copy_of_the_plugin_is_named_with_its_removal_steps_and_never_removed() {
    let env = TestEnv::new();
    // The same plugin name from another marketplace.
    let other = format!("{}@old-market", PLUGIN.split('@').next().unwrap());
    let list = json!([
        {"id": PLUGIN, "version": "1.3.0", "scope": "user", "enabled": true, "installPath": "x"},
        {"id": other, "version": "1.2.0", "scope": "local",
         "projectPath": "D:/projects/ws", "enabled": true, "installPath": "y"}
    ]);
    std::fs::write(profile(&env).join("fake-plugins.json"), list.to_string()).unwrap();
    let out = doctor(&env, env.root(), "y");
    let line = lines(&out)
        .into_iter()
        .find(|l| l.contains(&other))
        .unwrap_or_else(|| panic!("the other copy isn't named: {out:?}"));
    assert!(line.starts_with("problem:"), "{line}");
    assert!(
        line.contains(&format!("claude plugin uninstall {other} --scope local")),
        "{line}"
    );
    let log = claude_log(&env);
    assert!(
        !log.iter()
            .flatten()
            .any(|w| w == "uninstall" || w == "remove" || w == "disable"),
        "{log:?}"
    );
}

#[test] // spec: ARCH-108; ruling P3-R28 (I7)
fn piped_answers_are_refused_outside_tests() {
    let env = TestEnv::new();
    // `LIL_TEST_DOCTOR_STDIN` other than `1`: the doctor reads answers only from a terminal.
    let extra = [
        ("PATH", env.path_with_testbins()),
        ("CLAUDE_CONFIG_DIR", OsString::from(profile(&env))),
        ("LIL_TEST_DOCTOR_STDIN", OsString::from("0")),
    ];
    let out = env.lil_in_with(env.root(), &["doctor"], &extra, Some(&"y\n".repeat(20)));
    assert!(
        claude_log(&env).is_empty(),
        "a piped y installed: {:?}",
        claude_log(&env)
    );
    assert!(
        out.stderr.contains("only from a person at a terminal"),
        "{out:?}"
    );
    assert!(
        lines(&out)
            .iter()
            .any(|l| l.starts_with("problem:") && l.contains("isn't installed")),
        "{out:?}"
    );
}

#[test] // spec: AI-117; review M3
fn a_lilith_marketplace_from_another_folder_is_a_problem_and_left_alone() {
    let env = TestEnv::new();
    let markets = json!([{"name": "lilith", "source": "directory",
                          "path": "D:/old-checkout/plugin", "installLocation": "x"}]);
    std::fs::write(
        profile(&env).join("fake-marketplaces.json"),
        markets.to_string(),
    )
    .unwrap();
    let out = doctor(&env, env.root(), "y");
    let line = lines(&out)
        .into_iter()
        .find(|l| l.contains("D:/old-checkout/plugin"))
        .unwrap_or_else(|| panic!("the other marketplace isn't named: {out:?}"));
    assert!(line.starts_with("problem:"), "{line}");
    assert!(
        line.contains("claude plugin marketplace remove lilith"),
        "{line}"
    );
    assert!(claude_log(&env).is_empty(), "{:?}", claude_log(&env));
}

#[test] // spec: AI-117; ruling P3-R28 (I6)
fn the_hooks_are_checked_to_run() {
    let env = TestEnv::new();
    let folder = ensure_built().dir.display().to_string();
    plugin_options(
        &env,
        json!({"lil_mcp_path": format!("{folder}\\lil-mcp.exe"),
               "lil_path": format!("{folder}\\lil.exe")}),
    );
    // Claude Code loads the plugin from a folder without hooks/hooks.json.
    let install = env.root().join("installed-plugin");
    std::fs::create_dir_all(&install).unwrap();
    let list = json!([{"id": PLUGIN, "version": "1.3.0", "scope": "user", "enabled": true,
                       "installPath": install.to_str().unwrap()}]);
    std::fs::write(profile(&env).join("fake-plugins.json"), list.to_string()).unwrap();
    let out = doctor(&env, env.root(), "n");
    let printed = lines(&out);
    assert!(
        printed
            .iter()
            .any(|l| l.starts_with("problem:") && l.contains("hooks.json")),
        "{out:?}"
    );
    // The trusted lil.exe answers `ask` for an `rm`, so its option is ok.
    assert!(
        printed
            .iter()
            .any(|l| l.starts_with("ok:") && l.contains("option lil_path")),
        "{out:?}"
    );
    std::fs::create_dir_all(install.join("hooks")).unwrap();
    std::fs::write(install.join("hooks").join("hooks.json"), "{}").unwrap();
    let again = doctor(&env, env.root(), "n");
    assert!(
        !lines(&again)
            .iter()
            .any(|l| l.contains("hooks.json") && l.starts_with("problem:")),
        "{again:?}"
    );
}

/// Ruling P3-R30: the plugin's hooks carry `"onFailure": "block"`, which Claude Code honours
/// from 2.1.295. An older Claude Code is a problem naming the update step; 2.1.295 and later
/// are fine.
#[test] // spec: AI-117, ARCH-108; ruling P3-R30
fn a_claude_code_older_than_2_1_295_is_a_problem_with_the_update_step() {
    let env = TestEnv::new();
    installed(&env, "1.3.0");
    for (version, problem) in [
        ("2.1.294", true),
        ("2.0.999", true),
        ("2.1.295", false),
        ("2.1.296", false),
    ] {
        std::fs::write(profile(&env).join("fake-version"), version).unwrap();
        let out = doctor(&env, env.root(), "n");
        let flagged = lines(&out).iter().any(|l| {
            l.starts_with("problem:")
                && l.contains(&format!("Claude Code {version} is older than 2.1.295"))
                && l.contains("claude update")
        });
        assert_eq!(flagged, problem, "{version}: {out:?}");
        if !problem {
            assert!(
                lines(&out)
                    .iter()
                    .any(|l| l.starts_with("ok:") && l.contains(&format!("Claude Code {version}"))),
                "{version}: {out:?}"
            );
        }
    }
}
