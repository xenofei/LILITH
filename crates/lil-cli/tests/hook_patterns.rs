//! QA-061: the plugin hook patterns, as a table (spec ARCH-047, ARCH-056, ARCH-104, ARCH-105,
//! ARCH-106, CORE-008, CORE-116, QA-061; Appendix F "Plugin 1.3").
//!
//! `src/user/hook_patterns.rs` is pure (std and `serde_json` only), so this file compiles it
//! in directly and checks every row against `classify_command` and `classify_tool`, both
//! shells, without starting a process per row. The spec tests (`plugin_hooks.rs`) drive the
//! same rules through `lil hook pre-tool-use`.
//!
//! The rows marked "limit" document what a pattern guard cannot see (QA-061): they are
//! asserted as they behave, so a change in a limit shows up here.

#[allow(dead_code, reason = "only the classifiers are used here")]
#[path = "../src/user/hook_patterns.rs"]
mod hook_patterns;

use std::ffi::OsString;
use std::path::Path;

use hook_patterns::{Decision, Shell, classify_command, classify_tool};
use lil_testkit::{CoreFixture, LilOutput, TestEnv};
use serde_json::{Value, json};

const BOTH: &[Shell] = &[Shell::Bash, Shell::PowerShell];
const BASH: &[Shell] = &[Shell::Bash];
const PWSH: &[Shell] = &[Shell::PowerShell];

fn asks(d: &Decision) -> bool {
    matches!(d, Decision::Ask(_))
}

/// Every row of `rows` must ask (`want_ask`) or get no decision.
fn check(rows: &[(&str, &[Shell])], want_ask: bool) {
    let mut wrong = Vec::new();
    for (command, shells) in rows {
        for shell in *shells {
            let got = classify_command(*shell, command);
            let ok = if want_ask {
                asks(&got)
            } else {
                got == Decision::None
            };
            if !ok {
                wrong.push(format!("{shell:?} `{command}`: {got:?}"));
            }
        }
    }
    let what = if want_ask {
        "must ask"
    } else {
        "must get no decision"
    };
    assert!(wrong.is_empty(), "these {what}:\n{}", wrong.join("\n"));
}

#[test] // spec: QA-061, ARCH-105, CORE-116, ARCH-047
fn hook_patterns_table() {
    let must_ask: &[(&str, &[Shell])] = &[
        // Deletes.
        ("rm -rf build", BOTH),
        ("rm x", BOTH),
        ("del x.txt", BOTH),
        ("erase x.txt", BOTH),
        ("rd /s /q build", BOTH),
        ("rmdir build", BOTH),
        ("Remove-Item -Recurse a", BOTH),
        ("remove-item a", PWSH),
        ("ri a", PWSH),
        ("git clean -fdx", BOTH),
        ("git -C repo clean -n", BOTH),
        ("git reset --hard", BOTH),
        ("git reset --hard HEAD~1", BOTH),
        // Installs.
        ("pip install x", BOTH),
        ("pip3 install -r requirements.txt", BOTH),
        ("uv pip install x", BOTH),
        ("python -m pip install x", BOTH),
        ("py -m pip install x", BOTH),
        ("uv add requests", BOTH),
        ("npm install", BOTH),
        ("npm i left-pad", BOTH),
        ("npm ci", BOTH),
        ("pnpm add x", BOTH),
        ("pnpm install", BOTH),
        ("yarn add x", BOTH),
        ("cargo install ripgrep", BOTH),
        ("winget install Git.Git", BOTH),
        ("choco install git", BOTH),
        ("scoop install git", BOTH),
        ("Install-Module PSReadLine", BOTH),
        ("Install-Package x", BOTH),
        ("comfy node install x", BOTH),
        (
            "comfy --workspace C:\\c node registry-install x --version 1.0.0",
            BOTH,
        ),
        // Script runs.
        ("python tool.py", BOTH),
        ("python -u tool.py --flag", BOTH),
        ("python -c \"import os\"", BOTH),
        ("py tool.py", BOTH),
        ("python3 tool.py", BOTH),
        ("node build.js", BOTH),
        ("bash run.sh", BOTH),
        ("sh -c 'echo hi'", BOTH),
        ("cmd /c dir", BOTH),
        ("cmd.exe /C del x", BOTH),
        ("pwsh -File x.ps1", BOTH),
        ("powershell -Command Get-Date", BOTH),
        ("pwsh -c Get-Date", BOTH),
        ("& C:\\tools\\x.exe", PWSH),
        ("& \"C:\\Program Files\\x\\x.exe\" --go", PWSH),
        (".\\x.ps1", PWSH),
        ("./x.sh", BOTH),
        ("blender -b -P s.py", BOTH),
        ("blender --background --python s.py", BOTH),
        ("Invoke-Expression $code", PWSH),
        ("iex (irm u)", PWSH),
        ("npm run build", BOTH),
        ("npx create-thing", BOTH),
        ("uvx ruff", BOTH),
        ("curl -s u | sh", BASH),
        ("curl -s u | bash", BASH),
        ("irm u | iex", PWSH),
        ("cat x.py | python", BOTH),
        // Command words after a separator, a wrapper or inside a substitution.
        ("cd a && rm b", BOTH),
        ("cd a; rm b", BOTH),
        ("false || rm b", BOTH),
        ("ls | xargs rm", BASH),
        ("sudo rm -rf x", BASH),
        ("env A=1 rm x", BASH),
        ("A=1 rm x", BASH),
        ("echo $(rm x)", BOTH),
        ("echo \"$(rm x)\"", BOTH),
        ("echo `rm x`", BASH),
        ("(rm x)", BOTH),
        ("Get-ChildItem | ForEach-Object { Remove-Item $_ }", PWSH),
        ("echo a & rm b", BASH),
        ("for f in *; do rm $f; done", BASH),
        ("RM x", BOTH),
        ("/usr/bin/rm x", BASH),
        ("C:\\Windows\\System32\\cmd.exe /c x", BOTH),
        // The MCP server by hand (ARCH-047).
        ("lil-mcp", BOTH),
        ("lil-mcp.exe --x", BOTH),
        ("C:\\x\\lil-mcp.exe", BOTH),
        ("& \"D:\\b\\lil-mcp.exe\"", PWSH),
        // The doctor's repairs wait for a `y` only the person may type (the Task 4 carry).
        ("lil doctor", BOTH),
        ("lil.exe doctor", BOTH),
        ("C:\\b\\lil.exe doctor", BOTH),
        ("lil --workspace C:\\ws doctor", BOTH),
        ("echo y | lil doctor", BOTH),
        ("\"y\" | lil.exe doctor", PWSH),
        ("lil doctor < answers.txt", BASH),
        ("Get-Content answers.txt | lil doctor", PWSH),
        ("yes | lil doctor", BASH),
        // Ruling P3-R28: interpreters, comments, call operators, unreadable text.
        ("python", BOTH),
        ("python -m http.server", BOTH),
        ("# rm x", BOTH),
        ("ls # comment", BOTH),
        ("$x = \"rm\"; & $x f", PWSH),
        ("echo 'unclosed", BOTH),
        ("echo $(rm x", BOTH),
        // I1: comments, typographic quotes, here-strings, ANSI-C quoting.
        ("<#c#> Remove-Item -Recurse X", PWSH),
        ("<#x#> echo y | lil doctor", PWSH),
        ("<#x#> pip install evil", PWSH),
        ("Write-Output \u{201c}it's\u{201d}; Remove-Item X", PWSH),
        ("Write-Output \u{2018}a\u{2019}; Remove-Item X", PWSH),
        ("$s = @'\nit's\n'@; Remove-Item X", PWSH),
        ("$s = @\"\nx\n\"@; Remove-Item X", PWSH),
        ("echo $'a\\'b'; rm -rf X", BASH),
        // I2: assignments, hashtables and script blocks.
        ("$x=Remove-Item X", PWSH),
        ("$x= Remove-Item X", PWSH),
        ("$x = Remove-Item X", PWSH),
        ("[string]$x = Remove-Item X", PWSH),
        ("$a, $b = Remove-Item X", PWSH),
        ("$x += Remove-Item X", PWSH),
        ("@{a = Remove-Item X}", PWSH),
        (".{Remove-Item X}", PWSH),
        ("1|%{Remove-Item X}", PWSH),
        ("gci | ForEach-Object{Remove-Item $_}", PWSH),
        ("Invoke-Command -ScriptBlock{Remove-Item X}", PWSH),
        // I3: cmd, stdin and eval.
        ("cmd /c\"del x\"", BOTH),
        ("cmd /cdel x", BOTH),
        ("cmd /r del x", BOTH),
        ("cmd /s/c del x", BOTH),
        ("echo y | cmd /c\"lil doctor\"", BOTH),
        ("bash < evil.sh", BASH),
        ("sh <<< 'rm -rf x'", BASH),
        ("python < evil.py", BASH),
        ("python - < evil.py", BASH),
        ("echo x | cmd", BOTH),
        ("echo x | pwsh -", BOTH),
        ("echo x | powershell -NoProfile -", BOTH),
        ("cat x.js | node", BOTH),
        ("eval rm -rf x", BASH),
        // I4: a flag before the install subcommand.
        (
            "pip --index-url https://example.invalid/simple install evil",
            BOTH,
        ),
        ("pip --proxy h install evil", BOTH),
        ("npm --prefix d install evil", BOTH),
        ("pnpm -C d add evil", BOTH),
        ("cargo +nightly install x", BOTH),
        // M7: more script runs, deletes and installs.
        ("Start-Process notepad", PWSH),
        ("saps x.exe", PWSH),
        ("start x.bat", PWSH),
        ("Invoke-Item x.lnk", PWSH),
        ("ii x.lnk", PWSH),
        ("cscript x.vbs", BOTH),
        ("wscript x.js", BOTH),
        ("perl -e 'unlink q(x)'", BOTH),
        ("wsl rm -rf x", BOTH),
        ("C:\\tools\\x.bat", BOTH),
        ("/c/tools/x.sh", BASH),
        ("& (Get-Command Remove-Item) x", PWSH),
        ("find . -name x -delete", BASH),
        ("find . -exec rm {} ;", BASH),
        ("git -c alias.x='!rm -rf y' x", BASH),
        ("[IO.File]::Delete('x')", PWSH),
        ("uv tool install ruff", BOTH),
        ("uv run x.py", BOTH),
        ("pipx install x", BOTH),
        ("pipx run x", BOTH),
        ("conda install x", BOTH),
        ("winget add Git.Git", BOTH),
        ("Install-PSResource x", PWSH),
        ("source x.sh", BASH),
        (". ./x.sh", BASH),
        // Documented limit: `bash -c` is asked about, its script isn't read.
        ("bash -c \"ls\"", BOTH),
    ];
    let must_not_ask: &[(&str, &[Shell])] = &[
        ("git status", BOTH),
        ("git log --oneline", BOTH),
        ("git reset HEAD x", BOTH),
        ("ls -la", BASH),
        ("Get-ChildItem", PWSH),
        ("cat notes.md", BOTH),
        ("type notes.md", PWSH),
        ("python --version", BOTH),
        ("python -V", BOTH),
        ("node --version", BOTH),
        ("bash --version", BOTH),
        ("pip list", BOTH),
        ("pip show x", BOTH),
        ("npm --version", BOTH),
        ("npm ls", BOTH),
        ("cargo build", BOTH),
        ("cargo nextest run -p lil-cli", BOTH),
        ("winget list", BOTH),
        ("comfy node list", BOTH),
        ("echo rm", BOTH),
        ("echo 'rm -rf x'", BOTH),
        ("echo \"pip install x\"", BOTH),
        ("grep -r \"pip install\" .", BASH),
        ("Select-String -Pattern 'Remove-Item' -Path x.ps1", PWSH),
        ("git commit -m \"rm the old file\"", BOTH),
        ("lil doctor --non-interactive", BOTH),
        ("lil --non-interactive doctor", BOTH),
        ("lil status", BOTH),
        ("lil status --no-start", BOTH),
        ("lil call asset.get", BOTH),
        ("lilith doctor", BOTH),
        ("lil-mcpx", BOTH),
        ("echo lil-mcp", BOTH),
        ("blender --version", BOTH),
        ("cmd /?", BOTH),
        ("pwsh -Version", BOTH),
        ("2>&1 echo ok", BASH),
        ("echo ok 2>&1", BOTH),
        // Ordinary commands still pass (ruling P3-R28 asks only for what it can't read or
        // what is on its lists).
        ("git diff --stat", BOTH),
        ("git add -A && git commit -m \"fix: hooks\"", BOTH),
        ("cargo test -p lil-cli -- --nocapture", BOTH),
        (
            "cargo clippy --workspace --all-targets -- -D warnings",
            BOTH,
        ),
        ("ls | grep x", BASH),
        ("echo a && echo b", BOTH),
        ("wc -l < notes.txt", BASH),
        ("Get-ChildItem | Select-Object Name", PWSH),
        ("Get-Content x | Select-String y", PWSH),
        ("gci | ForEach-Object { $_.Name }", PWSH),
        ("$d = Get-Date", PWSH),
        ("$x = \"rm\"", PWSH),
        ("cd crates; ls", BASH),
        ("Set-Location crates; Get-ChildItem", PWSH),
        ("mkdir -p build/out", BASH),
        ("echo $HOME", BASH),
        ("git log --format=%H -1", BOTH),
    ];
    check(must_ask, true);
    check(must_not_ask, false);
}

/// A Write or Edit of `path`.
fn write_of(tool: &str, path: &str) -> Decision {
    classify_tool(tool, &json!({"file_path": path, "content": "x"}), false)
}

#[test] // spec: ARCH-056, CORE-008
fn suite_toml_guard() {
    let stable = r"D:\data\Roaming\LILITH\suite.toml";
    let dev = r"D:\data\Roaming\LILITH-dev\suite.toml";
    let test = "C:/t/roaming/LILITH-test-0a1b2c3d/suite.toml";
    // I5: the forms Windows reads as the same file (`.`, `..`, trailing dots and spaces, an
    // alternate data stream, 8.3 short names, case).
    let variants = [
        r"D:\data\Roaming\LILITH\.\suite.toml",
        r"D:\data\Roaming\LILITH\x\..\suite.toml",
        r"D:\data\Roaming\LILITH\suite.toml.",
        r"D:\data\Roaming\LILITH\suite.toml . ",
        r"D:\data\Roaming\LILITH\suite.toml::$DATA",
        r"D:\data\Roaming\LILITH\suite.toml:x",
        r"D:\data\Roaming\LILITH\SUITE~1.TOM",
        r"D:\data\Roaming\LILITH~1\suite.toml",
        r"D:\data\Roaming\LILITH.\suite.toml",
        r"D:\data\Roaming\lilith\Suite.TOML",
        r"D:\data\Roaming\LILITH\suit*.toml",
    ];
    for path in [stable, dev, test].into_iter().chain(variants) {
        for tool in ["Write", "Edit", "MultiEdit"] {
            assert!(asks(&write_of(tool, path)), "{tool} {path}");
        }
    }
    for path in [
        r"C:\notes\suite.toml",
        r"C:\x\LILITHS\suite.toml",
        r"C:\x\LILITH\other.toml",
        r"C:\x\LILITH\sub\suite.toml",
        r"C:\x\LILITH\..\suite.toml",
        r"C:\x\LILITH~1\notes.txt",
        r"C:\x\PROGRA~1\x\suite.toml",
    ] {
        assert_eq!(write_of("Write", path), Decision::None, "{path}");
    }

    let ask: &[(&str, &[Shell])] = &[
        ("Set-Content $env:APPDATA\\LILITH\\suite.toml x", BOTH),
        (
            "Add-Content -Path $env:APPDATA\\LILITH\\suite.toml -Value x",
            BOTH,
        ),
        ("'x' | Out-File $env:APPDATA\\LILITH-dev\\suite.toml", PWSH),
        ("Clear-Content $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("New-Item -Force $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("Move-Item x $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("Rename-Item $env:APPDATA\\LILITH\\suite.toml y", PWSH),
        ("Copy-Item x $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("echo x > \"$APPDATA/LILITH/suite.toml\"", BOTH),
        ("echo x >> \"$APPDATA/LILITH/suite.toml\"", BOTH),
        ("echo x>>$APPDATA/LILITH/suite.toml", BASH),
        ("'x' > $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("cp x \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("mv x \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("echo x | tee -a \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("sed -i 's/a/b/' \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("sed -i.bak 's/a/b/' \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("rm \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("Remove-Item $env:APPDATA\\LILITH\\suite.toml", PWSH),
        // I5: path variants and write-like verbs.
        ("Set-Content $env:APPDATA\\LILITH\\suit*.toml x", PWSH),
        ("sed -i 's/a/b/' \"$APPDATA/LILITH/suit?.toml\"", BASH),
        ("sc $env:APPDATA\\LILITH\\suite.toml x", PWSH),
        ("'x' | Tee-Object $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("cd $env:APPDATA\\LILITH; Set-Content suite.toml x", PWSH),
        ("cd \"$APPDATA/LILITH\" && echo x > suite.toml", BASH),
        ("robocopy C:\\x $env:APPDATA\\LILITH suite.toml", PWSH),
        (
            "Invoke-WebRequest u -OutFile $env:APPDATA\\LILITH\\suite.toml",
            PWSH,
        ),
        ("dd if=x of=\"$APPDATA/LILITH/suite.toml\"", BASH),
        ("truncate -s 0 \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("ln -sf x \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("Set-Content -Path:$env:APPDATA\\LILITH\\suite.toml x", PWSH),
    ];
    let pass: &[(&str, &[Shell])] = &[
        ("Get-Content $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("cat \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("type $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("grep caps \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("Select-String caps $env:APPDATA\\LILITH\\suite.toml", PWSH),
        ("sed -n 1,5p \"$APPDATA/LILITH/suite.toml\"", BASH),
        ("echo x > notes/suite.toml", BOTH),
        ("Set-Content C:\\notes\\suite.toml x", PWSH),
        ("cat \"$APPDATA/LILITH/suite.toml\" > copy.toml", BASH),
    ];
    check(ask, true);
    check(pass, false);
}

/// The comfy MCP's tool `name` with `input`, with and without a core.
fn comfy(name: &str, input: Value, core_running: bool) -> Decision {
    classify_tool(&format!("mcp__comfyui__{name}"), &input, core_running)
}

#[test] // spec: ARCH-106
fn comfy_mcp_denies_while_a_core_runs() {
    for name in [
        "launch_comfyui",
        "stop_comfyui",
        "restart_comfyui",
        "partner_generate",
        "auth_login",
    ] {
        assert!(
            matches!(comfy(name, json!({}), true), Decision::Deny(_)),
            "{name} while a core runs"
        );
    }
    for name in [
        "launch_comfyui",
        "stop_comfyui",
        "partner_generate",
        "auth_login",
    ] {
        assert_eq!(comfy(name, json!({}), false), Decision::None, "{name}");
    }
    assert!(asks(&comfy("restart_comfyui", json!({}), false)));
}

#[test] // spec: ARCH-105, CORE-116
fn comfy_mcp_asks_by_tool_and_action() {
    for running in [false, true] {
        for name in [
            "install_node",
            "update_comfyui",
            "switch_comfyui_version",
            "download_model",
            "free_memory",
        ] {
            assert!(asks(&comfy(name, json!({}), running)), "{name} ({running})");
        }
        for name in ["job", "download"] {
            for input in [
                json!({"action": "cancel"}),
                json!({}),
                json!({"action": null}),
            ] {
                assert!(
                    asks(&comfy(name, input.clone(), running)),
                    "{name} {input} ({running})"
                );
            }
        }
        for action in ["status", "wait", "watch", "error", "queue"] {
            assert_eq!(
                comfy("job", json!({ "action": action }), running),
                Decision::None,
                "job {action}"
            );
        }
        for action in ["status", "wait"] {
            assert_eq!(
                comfy("download", json!({ "action": action }), running),
                Decision::None,
                "download {action}"
            );
        }
        for name in [
            "server_info",
            "generate_image",
            "run_workflow",
            "system_stats",
        ] {
            assert_eq!(comfy(name, json!({}), running), Decision::None, "{name}");
        }
    }
    // Another server's tool of the same name is not the comfy MCP's.
    assert_eq!(
        classify_tool("mcp__other__install_node", &json!({}), true),
        Decision::None
    );
}

#[test] // spec: ARCH-104
fn warn_only_guard_for_logs_and_manifests() {
    for path in [
        r"C:\ws\queue\jobs.pc1.2026-10.jsonl",
        r"C:\ws\registry\usage.pc1.2026-10.jsonl",
        r"C:\ws\registry\usage-loose.jsonl",
        "C:/ws/projects/sketchbook/assets/kite/kite.manifest.json",
    ] {
        for tool in ["Write", "Edit", "MultiEdit"] {
            assert!(
                matches!(write_of(tool, path), Decision::Warn(_)),
                "{tool} {path}"
            );
        }
    }
    for path in [
        r"C:\ws\queue\notes.jsonl",
        r"C:\ws\registry\models.yaml",
        r"C:\ws\projects\sketchbook\assets\kite\manifest.json.bak",
    ] {
        assert_eq!(write_of("Write", path), Decision::None, "{path}");
    }
}

// ---------------------------------------------------------------------------------------------
// `lil hook` itself.
// ---------------------------------------------------------------------------------------------

/// `lil hook <event>` in `dir` with `input` on stdin and `extra` variables; it always exits 0.
fn hook(
    env: &TestEnv,
    dir: &Path,
    event: &str,
    input: &Value,
    extra: &[(&str, OsString)],
) -> LilOutput {
    let out = env.lil_in_with(dir, &["hook", event], extra, Some(&input.to_string()));
    assert_eq!(out.code, 0, "`lil hook {event}` exits 0: {out:?}");
    out
}

fn path_str(path: &Path) -> String {
    path.to_str().expect("test paths are UTF-8").to_string()
}

/// The three hook inputs for a session in `ws`: SessionStart, a PreToolUse that asks (`rm`),
/// and a PostToolUse for a file in `ws`.
fn inputs(ws: &Path) -> [(&'static str, Value); 3] {
    let cwd = path_str(ws);
    [
        (
            "session-start",
            json!({"session_id": "s", "hook_event_name": "SessionStart", "source": "startup", "cwd": cwd}),
        ),
        (
            "pre-tool-use",
            json!({"session_id": "s", "hook_event_name": "PreToolUse", "cwd": cwd,
                   "tool_name": "Bash", "tool_input": {"command": "rm -rf build"}}),
        ),
        (
            "post-tool-use",
            json!({"session_id": "s", "hook_event_name": "PostToolUse", "cwd": cwd, "tool_name": "Write",
                   "tool_input": {"file_path": path_str(&ws.join("notes.txt"))}, "tool_response": {}}),
        ),
    ]
}

/// The default workspace `ws-<name>`.
fn default_workspace(env: &TestEnv, name: &str) -> std::path::PathBuf {
    let ws = env.make_workspace(name);
    env.register_workspace(&ws, true);
    ws
}

#[test] // spec: ARCH-107
fn engine_a_marker_silences_every_hook() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "engine");
    std::fs::write(ws.join("notes.txt"), b"x").unwrap();
    let _core = CoreFixture::start(&env);
    let engine = [("LIL_ENGINE", OsString::from("1"))];
    for (event, input) in inputs(&ws) {
        let out = hook(&env, &ws, event, &input, &engine);
        assert_eq!(out.stdout, "", "{event} printed in Engine A: {out:?}");
    }
    // The same inputs outside Engine A do speak, so the silence above is the marker's.
    let [(start, start_input), (pre, pre_input), _] = inputs(&ws);
    assert!(
        !hook(&env, &ws, start, &start_input, &[])
            .stdout
            .trim()
            .is_empty()
    );
    assert!(
        hook(&env, &ws, pre, &pre_input, &[])
            .stdout
            .contains("\"ask\"")
    );
    // A malformed input prints nothing, and still exits 0.
    let out = env.lil_in_with(&ws, &["hook", "pre-tool-use"], &[], Some("{not json"));
    assert_eq!((out.code, out.stdout.as_str()), (0, ""), "{out:?}");
}

#[test] // spec: ARCH-018, ARCH-103
fn session_start_and_post_tool_use_use_no_start() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "nostart");
    std::fs::write(ws.join("notes.txt"), b"x").unwrap();
    let [(start, start_input), _, (post, post_input)] = inputs(&ws);
    for (event, input) in [(start, start_input), (post, post_input)] {
        let out = hook(&env, &ws, event, &input, &[]);
        assert_eq!(out.stdout, "", "{event} without a core: {out:?}");
    }
    let status = env.lil(&["--no-start", "status"]);
    assert_eq!(status.code, 3, "a hook started a core: {status:?}");
}

#[test] // spec: ARCH-106; ruling P3-R28 (I6)
fn the_hook_fails_closed_when_it_cant_tell_whether_a_core_runs() {
    let env = TestEnv::new();
    let input = json!({"session_id": "s", "hook_event_name": "PreToolUse",
                       "cwd": path_str(env.root()), "tool_name": "mcp__comfyui__launch_comfyui",
                       "tool_input": {}});
    // A channel this build refuses: `lil` can't find its folders, so it can't probe a core.
    let broken = [("LIL_CHANNEL", OsString::from("not a channel"))];
    let out = hook(&env, env.root(), "pre-tool-use", &input, &broken);
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("an answer");
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "ask",
        "{out:?}"
    );
    // With its folders found and no core running, the same tool gets no decision.
    assert_eq!(
        hook(&env, env.root(), "pre-tool-use", &input, &[]).stdout,
        ""
    );
}
