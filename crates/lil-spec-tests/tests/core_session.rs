//! Core sessions: one core per channel, the `core/hello` handshake, caller identity and the
//! pipe contract (spec Appendix D, "Core protocol"). Each test drives a real `lil-core` over its
//! real pipe through the test kit's `RawPipe` (QA-018, QA-056).

use std::time::Duration;

use lil_testkit::{CoreFixture, RawPipe, TestEnv};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// The pipe name Appendix D fixes for a test channel: the fixed test SID, then the channel.
fn contract_pipe(env: &TestEnv) -> String {
    format!(
        r"\\.\pipe\worldwright-core-S-1-5-21-1000-2000-3000-1001-{}",
        env.env.channel()
    )
}

#[test] // spec: ARCH-026, ARCH-024
fn single_core_per_channel() {
    let env = TestEnv::new();
    let _first = CoreFixture::start(&env);
    let second = CoreFixture::spawn_raw(&env);
    assert_eq!(
        second.wait_exit(DEADLINE),
        Some(3),
        "a second core on the same channel must exit with code 3 (another core owns the pipe)"
    );
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
}

#[test] // spec: ARCH-029
fn hello_is_mandatory_first_call() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.call("tools/list", json!({}));
    assert_eq!(response["error"]["code"], -32001, "{response}");
}

#[test] // spec: ARCH-029, QA-005
fn hello_major_mismatch_is_refused() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({"protocol": "2.0"}));
    let result = &response["result"];
    assert_eq!(result["status"], "error", "{response}");
    assert_eq!(
        result["error"]["code"], "protocol.incompatible",
        "{response}"
    );
    assert_eq!(
        result["error"]["data"]["core_protocol"], "1.0",
        "{response}"
    );
}

#[test] // spec: ARCH-029
fn hello_minor_mismatch_uses_shared_capabilities() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({
        "protocol": "1.7",
        "capabilities": ["events.v1", "future.x"],
    }));
    let result = &response["result"];
    assert_eq!(result["status"], "ok", "{response}");
    assert_eq!(result["capabilities"], json!(["events.v1"]), "{response}");
}

#[test] // spec: ARCH-029
fn hello_rejects_unknown_parameters() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({"protocol": "1.0", "color": "red"}));
    assert_eq!(response["error"]["code"], -32602, "{response}");
}

#[test] // spec: ARCH-023, ARCH-031
fn drain_is_the_only_call_after_incompatible_hello() {
    let env = TestEnv::new();
    let mut core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);

    let hello = pipe.hello(json!({"protocol": "2.0"}));
    assert_eq!(hello["result"]["status"], "error", "{hello}");
    assert_eq!(
        hello["result"]["error"]["code"], "protocol.incompatible",
        "{hello}"
    );

    let list = pipe.call("tools/list", json!({}));
    assert_eq!(list["error"]["code"], -32001, "{list}");

    let drain = pipe.call("core/drain", json!({}));
    assert_eq!(drain["result"]["status"], "ok", "{drain}");

    drop(pipe);
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(0),
        "an accepted core/drain ends the core with exit code 0 within {DEADLINE:?}"
    );
}

#[test] // spec: ARCH-023, ARCH-014
fn older_client_cannot_drain_newer_core() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);

    let hello = pipe.hello(json!({"protocol": "0.9"}));
    assert_eq!(hello["result"]["status"], "error", "{hello}");
    assert_eq!(
        hello["result"]["error"]["code"], "protocol.incompatible",
        "{hello}"
    );

    let drain = pipe.call("core/drain", json!({}));
    assert_eq!(
        drain["result"]["error"]["code"], "protocol.incompatible",
        "{drain}"
    );
    drop(pipe);

    let mut fresh = RawPipe::connect(&env);
    let again = fresh.hello(json!({}));
    assert_eq!(again["result"]["status"], "ok", "{again}");
}

#[test] // spec: ARCH-048, ARCH-050
fn forged_engine_token_is_refused_not_downgraded() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({"caller_kind": "assistant", "engine_token": "forged"}));
    let result = &response["result"];
    assert_eq!(result["status"], "error", "{response}");
    assert_eq!(
        result["error"]["code"], "caller.engine_token_invalid",
        "{response}"
    );
}

#[test] // spec: ARCH-045, ARCH-046
fn declared_ui_from_unverified_process_is_cli() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({"caller_kind": "ui:image"}));
    let result = &response["result"];
    assert_eq!(result["status"], "ok", "{response}");
    assert_eq!(result["caller_kind"], "cli", "{response}");
}

#[test] // spec: ARCH-024
fn hello_reports_pipe_contract() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({}));
    let result: &Value = &response["result"];
    assert_eq!(result["status"], "ok", "{response}");
    assert_eq!(result["core"]["pipe"], contract_pipe(&env), "{response}");
}
