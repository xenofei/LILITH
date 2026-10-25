//! `lil-mcp` starting a stopped suite app for an agent (ARCH-015, APP-065; Appendix G
//! "`lil-mcp` launches"): only a `launch_command` whose `exe` is registered beside the core it
//! reached, only for an `mcp` session, detached and without focus, with `--no-activate` before
//! the URL. Driven through the binary under the fake `claude`, against a copied install (core,
//! `lil-mcp` and `lil-fake-image` as `lil-image.exe` in one folder), with what
//! `lil-fake-image` records in `%LIL_TEST_ROOT%\fake-image-args.jsonl`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lil_testkit::{CoreFixture, McpStdio, TestEnv, ensure_built};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// An installed layout in the test's folder: `<root>\bin\` with copies of the core, `lil-mcp`
/// and the fake image app as `lil-image.exe`, which `bin\apps\image.toml` registers.
struct Install {
    bin: PathBuf,
    _core: CoreFixture,
}

fn copy(from: &Path, to: &Path) {
    std::fs::copy(from, to)
        .unwrap_or_else(|e| panic!("cannot copy {} to {}: {e}", from.display(), to.display()));
}

/// `apps\<name>.toml` in `dir`: `name` runs `exe` and opens PNG images.
fn register(dir: &Path, name: &str, exe: &Path) {
    lil_testkit::files::write_file(
        &dir.join("apps").join(format!("{name}.toml")),
        format!(
            "app = \"{name}\"\nexe = '{}'\n[[opens]]\ntypes = [\"image\"]\nformats = [\"png\"]\nintents = [\"open\"]\n",
            exe.display()
        )
        .as_bytes(),
    );
}

fn install(env: &TestEnv) -> Install {
    let bins = ensure_built();
    let bin = env.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    copy(&bins.lil_core, &bin.join("lil-core.exe"));
    copy(&bins.lil_mcp, &bin.join("lil-mcp.exe"));
    copy(&bins.fake_image, &bin.join("lil-image.exe"));
    register(&bin, "image", &bin.join("lil-image.exe"));
    let core = CoreFixture::start_exe(env, &bin.join("lil-core.exe"));
    Install { bin, _core: core }
}

impl Install {
    /// The copied `lil-mcp`, the direct child of the fake `claude` (an `mcp` session).
    fn mcp(&self, env: &TestEnv) -> McpStdio {
        let exe = self.bin.join("lil-mcp.exe").into_os_string();
        initialized(env.spawn_stdio(&[exe.as_os_str()]))
    }

    /// The copied `lil-mcp` started through a shell under the fake `claude`: a `cli` session.
    fn mcp_under_shell(&self, env: &TestEnv) -> McpStdio {
        let exe = self.bin.join("lil-mcp.exe").into_os_string();
        initialized(env.spawn_stdio(&[
            OsStr::new("cmd.exe"),
            OsStr::new("/d"),
            OsStr::new("/c"),
            exe.as_os_str(),
        ]))
    }
}

fn initialized(mut mcp: McpStdio) -> McpStdio {
    let response = mcp.request(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "lil-mcp-launch-tests", "version": "0.0.0"}}),
    );
    assert!(response.get("result").is_some(), "{response}");
    mcp.notify("notifications/initialized", json!({}));
    mcp
}

/// A `tools/call`: whether it is an error, its structured result and its text.
fn call(mcp: &mut McpStdio, name: &str, args: Value) -> (bool, Value, String) {
    let response = mcp.request("tools/call", json!({"name": name, "arguments": args}));
    let result = &response["result"];
    let text: Vec<&str> = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: {response}"))
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect();
    (
        result["isError"] == true,
        result["structuredContent"]["structured"].clone(),
        text.join("\n"),
    )
}

/// A PNG to show, its name with a space and a `#`.
fn png(env: &TestEnv) -> PathBuf {
    let path = env.root().join("a picture #1.png");
    lil_testkit::files::write_file(&path, b"\x89PNG\r\n\x1a\n");
    path
}

/// The argument lists `lil-fake-image` recorded.
fn recorded(env: &TestEnv) -> Vec<Vec<String>> {
    std::fs::read_to_string(env.root().join("fake-image-args.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
            v["args"]
                .as_array()
                .unwrap_or_else(|| panic!("no args: {l}"))
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

/// Waits until `n` runs are recorded; returns them.
fn until_recorded(env: &TestEnv, n: usize) -> Vec<Vec<String>> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let runs = recorded(env);
        if runs.len() >= n {
            return runs;
        }
        assert!(
            Instant::now() < deadline,
            "{n} run(s) of lil-fake-image not recorded within {DEADLINE:?}: {runs:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The `open/` URL of `target` on the test channel.
fn open_url(env: &TestEnv, target: &Path) -> String {
    lil_parse::open_url(&env.env.activation_scheme(), &target.to_string_lossy())
}

#[test] // spec: ARCH-015, APP-065, APP-020
fn launches_a_stopped_registered_app_without_focus() {
    let env = TestEnv::new();
    let install = install(&env);
    let target = png(&env);
    let mut mcp = install.mcp(&env);

    let (is_error, structured, text) = call(
        &mut mcp,
        "ui_show",
        json!({"target": target.to_string_lossy()}),
    );
    assert!(!is_error, "{text}");
    assert_eq!(structured["running"], false, "{text}");
    assert!(text.contains("Started image in the background."), "{text}");
    let runs = until_recorded(&env, 1);
    assert_eq!(
        runs,
        [vec![
            "--intent".to_string(),
            "open".to_string(),
            "--no-activate".to_string(),
            open_url(&env, &target),
        ]]
    );
    // The core's answer is not changed: the launch command still holds no `--no-activate`.
    let args = structured["launch_command"]["args"].as_array().unwrap();
    assert!(!args.iter().any(|a| a == "--no-activate"), "{structured}");

    // `open_in` is launched the same way.
    let (is_error, _, text) = call(
        &mut mcp,
        "open_in",
        json!({"app": "image", "target": target.to_string_lossy(), "intent": "open"}),
    );
    assert!(!is_error, "{text}");
    assert!(text.contains("Started image in the background."), "{text}");
    assert_eq!(until_recorded(&env, 2).len(), 2);
}

#[test] // spec: ARCH-015
fn never_launches_an_unregistered_exe() {
    let env = TestEnv::new();
    let install = install(&env);
    // An external program registered in `%APPDATA%\LILITH\apps\`: the core offers its
    // launch command, but it is not a suite app beside the core.
    let elsewhere = env.root().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    copy(&ensure_built().fake_image, &elsewhere.join("viewer.exe"));
    register(&env.env.data_dir(), "viewer", &elsewhere.join("viewer.exe"));
    let target = png(&env);
    let mut mcp = install.mcp(&env);

    let (is_error, structured, text) = call(
        &mut mcp,
        "open_in",
        json!({"app": "viewer", "target": target.to_string_lossy(), "intent": "open"}),
    );
    assert!(!is_error, "{text}");
    assert!(
        structured["launch_command"]["exe"]
            .as_str()
            .is_some_and(|e| e.ends_with("viewer.exe")),
        "{structured}"
    );
    assert!(!text.contains("in the background"), "{text}");

    // A launch that does happen afterwards is the only run recorded.
    let (_, _, text) = call(
        &mut mcp,
        "open_in",
        json!({"app": "image", "target": target.to_string_lossy(), "intent": "open"}),
    );
    assert!(text.contains("Started image in the background."), "{text}");
    assert_eq!(until_recorded(&env, 1).len(), 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(recorded(&env).len(), 1, "viewer.exe was started");
}

#[test] // spec: ARCH-015, ARCH-047
fn never_launches_for_a_cli_caller() {
    let env = TestEnv::new();
    let install = install(&env);
    let target = png(&env);

    let mut shell = install.mcp_under_shell(&env);
    let (_, status, _) = call(&mut shell, "lil_status", json!({}));
    assert_eq!(status["caller"], "cli", "{status}");
    let (is_error, structured, text) = call(
        &mut shell,
        "ui_show",
        json!({"target": target.to_string_lossy()}),
    );
    assert!(!is_error, "{text}");
    assert_eq!(structured["running"], false, "{structured}");
    assert!(!text.contains("in the background"), "{text}");

    // An `mcp` session's launch afterwards is the only run recorded.
    let mut mcp = install.mcp(&env);
    let (_, _, text) = call(
        &mut mcp,
        "ui_show",
        json!({"target": target.to_string_lossy()}),
    );
    assert!(text.contains("Started image in the background."), "{text}");
    assert_eq!(until_recorded(&env, 1).len(), 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(recorded(&env).len(), 1, "the cli session started the app");
}
