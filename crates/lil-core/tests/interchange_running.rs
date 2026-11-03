//! When an app counts as running for `open_in` and `ui.show` (APP-063, APP-064): only while
//! one of its sessions is subscribed to `open_requests:<app>`. An app's window that is open
//! but reads no such topic (the confirm window, which connects as `ui:<app>` and loads no
//! shell) must not make the core answer `running: true`, because the request would then be
//! published to nobody and `lil-mcp` would start nothing. Driven over the pipe of an in-process
//! core, with `lil-test-app` as the `ui:test` app.

use std::sync::Arc;
use std::time::Duration;

use lil_core::identity::TrustedDirs;
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_protocol::method;
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, RawPipe, SeqIds, TestApp, TestEnv, ensure_built};
use serde_json::{Value, json};

/// Failure deadline for anything the test waits on.
const DEADLINE: Duration = Duration::from_secs(30);

/// An in-process core that trusts the test binaries' folder for apps (`lil-test-app.exe`).
fn core(env: &TestEnv) -> CoreThread {
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        trusted: TrustedDirs {
            app_dirs: vec![ensure_built().dir.clone()],
            test_app: true,
            ..base.trusted.clone()
        },
        ..base
    })
}

/// Registers the `test` app (the test binary `lil-test-app.exe`) for PNG files.
fn register_test_app(env: &TestEnv) {
    let exe = &ensure_built().test_app;
    let text = format!(
        "app = \"test\"\nexe = '{}'\n[[opens]]\ntypes = [\"image\"]\nformats = [\"png\"]\nintents = [\"open\"]\n",
        exe.display()
    );
    write_file(
        &env.env.data_dir().join("apps").join("test.toml"),
        text.as_bytes(),
    );
}

/// An agent's connection (`cli`).
fn agent(env: &TestEnv) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let response = pipe.hello(json!({}));
    assert_eq!(response["result"]["status"], "ok", "hello: {response}");
    pipe
}

/// The `structured` answer of the agent's `open_in` of a PNG in the `test` app.
fn open_in(agent: &mut RawPipe, target: &str) -> Value {
    agent.tool_ok(
        "open_in",
        json!({"app": "test", "target": target, "intent": "open"}),
        None,
    )
}

/// Subscribes `app` to `topic`.
fn subscribe(app: &mut TestApp, topic: &str) {
    let answer = app.call(
        method::SUBSCRIBE,
        json!({"topics": [topic], "since_seq": {}}),
    );
    assert_eq!(answer["result"]["status"], "ok", "{answer}");
}

#[test] // spec: APP-063, APP-064
fn an_app_is_running_only_while_a_session_reads_its_open_requests() {
    let env = TestEnv::new();
    register_test_app(&env);
    let target = env.root().join("picture.png");
    write_file(&target, b"\x89PNG\r\n\x1a\n");
    let target = target.to_string_lossy().into_owned();
    let _core = core(&env);
    let mut agent = agent(&env);

    // The confirm window's shape: connected as `ui:test`, subscribed to nothing. The core
    // must tell the requester to start the app, not claim a window that reads no request.
    let mut window = TestApp::start(&env, None, &[]);
    let answer = open_in(&mut agent, &target);
    assert_eq!(answer["running"], false, "{answer}");
    assert_eq!(answer["pid"], Value::Null, "{answer}");
    assert_eq!(
        answer["launch_command"]["exe"]
            .as_str()
            .map(str::to_lowercase),
        Some(ensure_built().test_app.to_string_lossy().to_lowercase()),
        "{answer}"
    );
    // `ui.show` picks the same: nothing is running, so the first registered opener.
    let shown = agent.tool_ok("ui.show", json!({"target": target}), None);
    assert_eq!(shown["running"], false, "{shown}");
    assert!(shown["launch_command"].is_object(), "{shown}");

    // Another topic doesn't make it running either.
    subscribe(&mut window, "confirmations");
    assert_eq!(open_in(&mut agent, &target)["running"], false);

    // Once the session reads `open_requests:test`, it is told.
    subscribe(&mut window, "open_requests:test");
    let answer = open_in(&mut agent, &target);
    assert_eq!(answer["running"], true, "{answer}");
    assert_eq!(answer["pid"], window.pid(), "{answer}");
    assert_eq!(answer["launch_command"], Value::Null, "{answer}");
    let shown = agent.tool_ok("ui.show", json!({"target": target}), None);
    assert_eq!(shown["running"], true, "{shown}");
    let event = loop {
        let event = window
            .next_event(DEADLINE)
            .expect("an open request within the deadline");
        if event["topic"] == "open_requests:test" && event["kind"] == "event" {
            break event;
        }
    };
    assert_eq!(event["data"]["intent"], "open", "{event}");
    assert_eq!(event["data"]["focus"], false, "{event}");

    // Reading it no more is not running again.
    let left = window.call(
        method::UNSUBSCRIBE,
        json!({"topics": ["open_requests:test"]}),
    );
    assert_eq!(left["result"]["status"], "ok", "{left}");
    assert_eq!(open_in(&mut agent, &target)["running"], false);
}
