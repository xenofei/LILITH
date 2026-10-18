//! The test kit's processes (QA-066): `lil-test-app`'s hello and relay, and the fake `claude`.
//! Deadlines only bound a failure (QA-031).

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use lil_platform::process::{ProcessInspector as _, SystemInspector};
use lil_testkit::{TEST_APP_CAPABILITIES, TestApp, TestEnv};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

/// Serves one connection on `env`'s pipe: answers the hello with `ok` and every later request
/// with its own params as the result, and hands the hello's params back.
fn fake_core(env: &TestEnv) -> std::thread::JoinHandle<Value> {
    let name = env.env.pipe_name();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = lil_platform::pipe::create_instance(&name, true).unwrap();
            ready_tx.send(()).unwrap();
            server.connect().await.unwrap();
            let (read, mut write) = tokio::io::split(server);
            let mut lines = tokio::io::BufReader::new(read).lines();
            let hello: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(hello["method"], "core/hello");
            let ok = json!({"jsonrpc": "2.0", "id": hello["id"],
                            "result": {"status": "ok", "caller_kind": "ui:test"}});
            write.write_all(format!("{ok}\n").as_bytes()).await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let event = json!({"jsonrpc": "2.0", "method": "lil/event",
                                   "params": {"topic": "test:relay", "seq": 1}});
                let answer = json!({"jsonrpc": "2.0", "id": request["id"],
                                    "result": request["params"]});
                write
                    .write_all(format!("{event}\n{answer}\n").as_bytes())
                    .await
                    .unwrap();
            }
            hello["params"].clone()
        })
    });
    ready_rx.recv().unwrap();
    server
}

#[test] // spec: QA-066, ARCH-045
fn test_app_offers_the_protocol_1_1_capabilities() {
    let env = TestEnv::new();
    let server = fake_core(&env);
    let mut app = TestApp::start(&env, None, &[]);
    assert_eq!(app.hello()["caller_kind"], "ui:test");
    // The relay carries a request both ways, and an event that came first is kept.
    let response = app.call("test/echo", json!({"n": 1}));
    assert_eq!(response["result"], json!({"n": 1}), "{response}");
    let event = app.next_event(Duration::from_secs(30)).expect("the event");
    assert_eq!(event["topic"], "test:relay");
    drop(app);

    let hello = server.join().expect("the fake core's checks");
    assert_eq!(hello["caller_kind"], "ui:test", "{hello}");
    assert_eq!(hello["workspace"], "none", "{hello}");
    assert_eq!(
        hello["capabilities"],
        json!(TEST_APP_CAPABILITIES),
        "{hello}"
    );
    for capability in ["confirmations.v1", "caps.v1", "jobs.v1"] {
        assert!(
            hello["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!(capability)),
            "{hello}"
        );
    }
}

/// The fake `claude` with `vars` added to an environment without `CLAUDE_CONFIG_DIR` or any
/// channel variable: `(exit code, stdout)`.
fn claude_with(vars: &[(OsString, OsString)], args: &[&str]) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_claude"));
    cmd.args(args);
    for var in ["CLAUDE_CONFIG_DIR", "LIL_CHANNEL", "LIL_TEST_ROOT"] {
        cmd.env_remove(var);
    }
    cmd.envs(vars.iter().cloned());
    let out = cmd.output().expect("run the fake claude");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The fake `claude` on `env`'s test channel, with `CLAUDE_CONFIG_DIR` set to `config`.
fn claude(env: &TestEnv, config: Option<&Path>, args: &[&str]) -> (i32, String) {
    let mut vars = env.child_env();
    if let Some(config) = config {
        vars.push(("CLAUDE_CONFIG_DIR".into(), config.as_os_str().to_owned()));
    }
    claude_with(&vars, args)
}

#[test] // spec: QA-066, QA-041
fn fake_claude_honours_claude_config_dir() {
    let env = TestEnv::new();
    let config = tempfile::Builder::new()
        .prefix("lil-fake-claude-")
        .tempdir()
        .unwrap();
    let dir = config.path();
    let (code, version) = claude(&env, None, &["--version"]);
    assert_eq!(code, 0);
    assert!(version.contains("(Claude Code)"), "{version}");

    assert_eq!(
        claude(&env, Some(dir), &["plugin", "list", "--json"]),
        (0, "[]\n".into())
    );
    let plugins = r#"[{"id":"demo@market","version":"1.3.0","enabled":true}]"#;
    std::fs::write(dir.join("fake-plugins.json"), plugins).unwrap();
    assert_eq!(
        claude(&env, Some(dir), &["plugin", "list", "--json"]),
        (0, format!("{plugins}\n"))
    );
    assert_eq!(
        claude(
            &env,
            Some(dir),
            &["plugin", "marketplace", "add", "./plugin"]
        )
        .0,
        0
    );
    assert_eq!(
        claude(&env, Some(dir), &["plugin", "install", "demo@market"]).0,
        0
    );
    let log = std::fs::read_to_string(dir.join("fake-claude.log")).unwrap();
    let lines: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines,
        [
            json!(["plugin", "marketplace", "add", "./plugin"]),
            json!(["plugin", "install", "demo@market"])
        ]
    );
    // Without CLAUDE_CONFIG_DIR the plugin commands refuse: no real profile is ever read.
    assert_eq!(claude(&env, None, &["plugin", "list", "--json"]).0, 2);
}

/// Run by `fake_claude_spawn_stdio_is_the_direct_parent` as `claude`'s child: reads one stdin
/// line and prints `parent=<its parent pid> got=<the line>`.
#[test]
#[ignore = "a helper the spawn-stdio test runs"]
fn helper_report_parent() {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    let me = SystemInspector.inspect(std::process::id()).unwrap();
    println!("parent={} got={}", me.parent_pid.unwrap_or(0), line.trim());
}

#[test] // spec: QA-066, ARCH-047
fn fake_claude_spawn_stdio_is_the_direct_parent() {
    let env = TestEnv::new();
    let exe = std::env::current_exe().unwrap();
    let mut server = env.spawn_stdio(&[
        exe.as_os_str(),
        OsStr::new("helper_report_parent"),
        OsStr::new("--exact"),
        OsStr::new("--ignored"),
        OsStr::new("--nocapture"),
    ]);
    server.send_line("ping");
    let line = loop {
        let line = server
            .read_line(Duration::from_secs(30))
            .unwrap_or_else(|| panic!("no report; stderr: {}", server.stderr_so_far()));
        if line.starts_with("parent=") {
            break line;
        }
    };
    assert_eq!(line, format!("parent={} got=ping", server.claude_pid()));
    assert_eq!(server.wait(Duration::from_secs(30)), Some(0));

    // The fake passes the child's exit code through.
    let spawn = ["--spawn-stdio", "cmd.exe", "/d", "/c", "exit 7"];
    assert_eq!(claude(&env, None, &spawn).0, 7);
}

/// Off a test channel the fake does nothing at all, so a `claude.exe` left in a build folder can
/// never start a process under a `claude.exe` parent on a dev channel.
#[test] // spec: QA-066, ARCH-047
fn fake_claude_refuses_outside_a_test_channel() {
    let env = TestEnv::new();
    let root: OsString = env.root().as_os_str().to_owned();
    let pair = |k: &str, v: &OsStr| (OsString::from(k), v.to_owned());
    let spawn = ["--spawn-stdio", "cmd.exe", "/d", "/c", "exit 7"];
    for vars in [
        vec![],
        vec![
            pair("LIL_CHANNEL", OsStr::new("dev")),
            pair("LIL_TEST_ROOT", &root),
        ],
        vec![pair("LIL_CHANNEL", OsStr::new("stable"))],
        vec![pair("LIL_CHANNEL", OsStr::new("test-ab12cd34"))],
    ] {
        assert_eq!(
            claude_with(&vars, &["--version"]),
            (2, String::new()),
            "{vars:?}"
        );
        assert_eq!(claude_with(&vars, &spawn).0, 2, "{vars:?}");
    }
    assert_eq!(claude(&env, None, &["--version"]).0, 0);
}
