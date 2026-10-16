//! The `ww` user commands `status`, `call` and `core stop`, and their exit codes (spec
//! Appendix D, "The `ww` user commands"). Each test runs the real `ww` on a test channel
//! through `TestEnv::ww` (QA-018, QA-056); any core it starts is drained when the env drops.

use std::time::Duration;

use serde_json::Value;
use ww_protocol::{Outcome, ToolOutput};
use ww_testkit::{RawPipe, TestEnv, WwOutput};

/// The pipe name Appendix D fixes for a test channel: the fixed test SID, then the channel.
fn contract_pipe(env: &TestEnv) -> String {
    format!(
        r"\\.\pipe\worldwright-core-S-1-5-21-1000-2000-3000-1001-{}",
        env.env.channel()
    )
}

/// `ww`'s stdout as JSON.
fn stdout_json(out: &WwOutput) -> Value {
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out:?}"))
}

#[test] // spec: ARCH-016, ARCH-018
fn status_starts_core_and_reports_it() {
    let env = TestEnv::new();
    let out = env.ww(&["status", "--json"]);
    assert_eq!(out.code, 0, "{out:?}");
    let status = stdout_json(&out);
    assert_eq!(status["running"], true, "{out:?}");
    assert_eq!(
        status["hello"]["core"]["pipe"],
        contract_pipe(&env),
        "{out:?}"
    );
}

#[test] // spec: ARCH-018
fn status_no_start_reports_not_running() {
    let env = TestEnv::new();
    let out = env.ww(&["--no-start", "status"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        RawPipe::try_connect(&env, Duration::from_secs(1)).is_none(),
        "`ww --no-start status` started a core"
    );
}

#[test] // spec: ARCH-018
fn no_start_call_is_a_silent_noop() {
    let env = TestEnv::new();
    let out = env.ww(&["--no-start", "call", "test.echo", "{\"text\":\"x\"}"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert_eq!(out.stderr, "", "{out:?}");
}

#[test] // spec: ARCH-016
fn call_maps_outcomes_to_exit_codes() {
    let env = TestEnv::new();

    let ok = env.ww(&["call", "test.echo", "{\"text\":\"x\"}"]);
    assert_eq!(ok.code, 0, "{ok:?}");
    let outcome: Outcome<ToolOutput> = serde_json::from_value(stdout_json(&ok))
        .unwrap_or_else(|e| panic!("stdout is not a tool Outcome ({e}): {ok:?}"));
    assert!(matches!(outcome, Outcome::Ok(_)), "{ok:?}");

    let err = env.ww(&["call", "no.such"]);
    assert_eq!(err.code, 1, "{err:?}");
}

#[test] // spec: QA-016
fn non_interactive_destructive_is_confirm_unavailable() {
    let env = TestEnv::new();
    let out = env.ww(&["--non-interactive", "call", "test.destructive"]);
    assert_eq!(out.code, 1, "{out:?}");
    let outcome = stdout_json(&out);
    assert_eq!(outcome["error"]["code"], "confirm.unavailable", "{out:?}");
}

#[test] // spec: ARCH-023
fn core_stop_drains_and_exits() {
    let env = TestEnv::new();
    let status = env.ww(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");

    let stop = env.ww(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(stop.stdout.trim(), "core stopped", "{stop:?}");

    let after = env.ww(&["--no-start", "status"]);
    assert_eq!(after.code, 3, "{after:?}");
}
