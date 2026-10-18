//! The `lil` user commands `status`, `call` and `core stop`, and their exit codes (spec
//! Appendix D, "The `lil` user commands"). Each test runs the real `lil` on a test channel
//! through `TestEnv::lil` (QA-018, QA-056); any core it starts is drained when the env drops.

use std::time::Duration;

use lil_protocol::{Outcome, ToolOutput};
use lil_testkit::{LilOutput, RawPipe, TestEnv};
use serde_json::Value;

/// The pipe name Appendix D fixes for a test channel: the fixed test SID, then the channel.
fn contract_pipe(env: &TestEnv) -> String {
    format!(
        r"\\.\pipe\lilith-core-S-1-5-21-1000-2000-3000-1001-{}",
        env.env.channel()
    )
}

/// `lil`'s stdout as JSON.
fn stdout_json(out: &LilOutput) -> Value {
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out:?}"))
}

#[test] // spec: ARCH-016, ARCH-018
fn status_starts_core_and_reports_it() {
    let env = TestEnv::new();
    let out = env.lil(&["status", "--json"]);
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
    let out = env.lil(&["--no-start", "status"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        RawPipe::try_connect(&env, Duration::from_secs(1)).is_none(),
        "`lil --no-start status` started a core"
    );
    // A started core always creates the suite state.db, so its absence shows none ran.
    let db = env.env.suite_state_db();
    assert!(
        !db.exists(),
        "`lil --no-start status` left {}: a core ran",
        db.display()
    );
}

#[test] // spec: ARCH-018
fn no_start_call_is_a_silent_noop() {
    let env = TestEnv::new();
    let out = env.lil(&["--no-start", "call", "test.echo", "{\"text\":\"x\"}"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert_eq!(out.stderr, "", "{out:?}");
}

#[test] // spec: ARCH-016
fn call_maps_outcomes_to_exit_codes() {
    let env = TestEnv::new();

    let ok = env.lil(&["call", "test.echo", "{\"text\":\"x\"}"]);
    assert_eq!(ok.code, 0, "{ok:?}");
    let outcome: Outcome<ToolOutput> = serde_json::from_value(stdout_json(&ok))
        .unwrap_or_else(|e| panic!("stdout is not a tool Outcome ({e}): {ok:?}"));
    assert!(matches!(outcome, Outcome::Ok(_)), "{ok:?}");

    let err = env.lil(&["call", "no.such"]);
    assert_eq!(err.code, 1, "{err:?}");
}

#[test] // spec: QA-016
fn non_interactive_destructive_is_confirm_unavailable() {
    let env = TestEnv::new();
    let out = env.lil(&["--non-interactive", "call", "test.destructive"]);
    assert_eq!(out.code, 1, "{out:?}");
    let outcome = stdout_json(&out);
    assert_eq!(outcome["error"]["code"], "confirm.unavailable", "{out:?}");
}

#[test] // spec: ARCH-023
fn core_stop_drains_and_exits() {
    let env = TestEnv::new();
    let status = env.lil(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");

    let stop = env.lil(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(stop.stdout.trim(), "core stopped", "{stop:?}");

    let after = env.lil(&["--no-start", "status"]);
    assert_eq!(after.code, 3, "{after:?}");
}
