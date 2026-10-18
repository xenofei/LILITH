//! Tool dispatch: the result envelope, error fields, argument checks, idempotency, fail-closed
//! confirmations, `app/*` gating and panics (spec Appendix D, "Core protocol"). Each test talks
//! to a real `lil-core` on a test channel through `RawPipe` and calls the test-channel tools
//! (QA-018, QA-056).

use lil_protocol::{ErrorCategory, Outcome, RetryPolicy, ToolOutput};
use lil_testkit::{CoreFixture, RawPipe, TestEnv};
use serde_json::{Value, json};

/// The seven error categories (QA-003).
const CATEGORIES: [&str; 7] = [
    "user_fixable",
    "transient",
    "external_down",
    "declined",
    "data_conflict",
    "cancelled",
    "bug",
];

/// The four retry policies (QA-004).
const RETRIES: [&str; 4] = ["auto", "same_idempotency_key", "after_resolve", "never"];

/// A running core and a connection that has said hello with the test defaults merged with
/// `hello`. The core is drained when the fixture drops.
fn session(env: &TestEnv, hello: Value) -> (CoreFixture, RawPipe) {
    let core = CoreFixture::start(env);
    let mut pipe = RawPipe::connect(env);
    let response = pipe.hello(hello);
    assert_eq!(response["result"]["status"], "ok", "hello: {response}");
    (core, pipe)
}

/// The error code of an error `Outcome`, after checking it is one.
fn error_code(outcome: &Value) -> &str {
    assert_eq!(outcome["status"], "error", "{outcome}");
    outcome["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no error code: {outcome}"))
}

#[test] // spec: QA-001, ARCH-036, CORE-017
fn results_are_exactly_ok_pending_or_error() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));

    let ok = pipe.tool("test.echo", json!({"text": "x"}), None);
    assert_eq!(ok["status"], "ok", "{ok}");
    assert_eq!(ok["structured"]["text"], "x", "{ok}");
    assert!(ok["artifacts"].is_array(), "{ok}");
    assert!(ok["summary_text"].is_string(), "{ok}");

    let err = pipe.tool("no.such", json!({}), None);
    assert_eq!(err["status"], "error", "{err}");

    for outcome in [&ok, &err] {
        let parsed: Outcome<ToolOutput> =
            serde_json::from_value(outcome.clone()).unwrap_or_else(|e| {
                panic!("not exactly ok, pending_confirmation or error ({e}): {outcome}")
            });
        match (&parsed, outcome["status"].as_str()) {
            (Outcome::Ok(_), Some("ok")) | (Outcome::Error { .. }, Some("error")) => {}
            _ => panic!("{outcome} parsed as {parsed:?}"),
        }
    }
}

#[test] // spec: QA-003, QA-004
fn errors_carry_code_category_trace_and_retry() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));

    let outcome = pipe.tool("no.such", json!({}), None);
    assert_eq!(outcome["status"], "error", "{outcome}");
    let error = &outcome["error"];
    for field in ["code", "message", "trace_id"] {
        let text = error[field].as_str().unwrap_or_default();
        assert!(
            !text.is_empty(),
            "error.{field} is missing or empty: {outcome}"
        );
    }
    let category = error["category"].as_str().unwrap_or_default();
    assert!(
        CATEGORIES.contains(&category),
        "category {category:?} is not one of {CATEGORIES:?}: {outcome}"
    );
    let retry = error["retry"].as_str().unwrap_or_default();
    assert!(
        RETRIES.contains(&retry),
        "retry {retry:?} is not one of {RETRIES:?}: {outcome}"
    );
    // The protocol types agree with the closed sets.
    serde_json::from_value::<ErrorCategory>(error["category"].clone()).expect("category");
    serde_json::from_value::<RetryPolicy>(error["retry"].clone()).expect("retry");
}

#[test] // spec: CORE-015
fn unknown_tool_arguments_are_rejected() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));
    let outcome = pipe.tool("test.echo", json!({"text": "x", "bogus": 1}), None);
    assert_eq!(error_code(&outcome), "params.invalid", "{outcome}");
}

#[test] // spec: ARCH-035
fn write_without_idempotency_key_is_refused() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));
    let outcome = pipe.tool("test.write", json!({"label": "a"}), None);
    assert_eq!(
        error_code(&outcome),
        "idempotency.key_required",
        "{outcome}"
    );
}

#[test] // spec: CORE-016
fn same_key_same_args_returns_same_result() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));
    let first = pipe.tool("test.write", json!({"label": "a"}), Some("k"));
    let second = pipe.tool("test.write", json!({"label": "a"}), Some("k"));
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(first, second);
    assert_eq!(first["structured"]["executions"], 1, "{first}");
}

#[test] // spec: CORE-016
fn same_key_different_args_is_rejected() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));
    let first = pipe.tool("test.write", json!({"label": "a"}), Some("k"));
    assert_eq!(first["status"], "ok", "{first}");
    let second = pipe.tool("test.write", json!({"label": "b"}), Some("k"));
    assert_eq!(error_code(&second), "idempotency.conflict", "{second}");
}

#[test] // spec: QA-016, ARCH-052, ARCH-055
fn agent_destructive_call_without_answer_path_is_unavailable() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({"caller_kind": "cli", "non_interactive": true}));
    let outcome = pipe.tool("test.destructive", json!({}), Some("k1"));
    assert_eq!(error_code(&outcome), "confirm.unavailable", "{outcome}");
    assert_eq!(outcome["error"]["category"], "user_fixable", "{outcome}");
    assert_eq!(outcome["error"]["retry"], "after_resolve", "{outcome}");
}

#[test] // spec: ARCH-031, APP-040
fn app_methods_refuse_cli_callers() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));
    let response = pipe.call("app/set_context", json!({}));
    assert_eq!(
        response["result"]["error"]["code"], "caller.not_allowed",
        "{response}"
    );
}

#[test] // spec: QA-012
fn tool_panic_fails_with_bug_and_core_survives() {
    let env = TestEnv::new();
    let (_core, mut pipe) = session(&env, json!({}));
    let outcome = pipe.tool("test.panic", json!({}), None);
    assert_eq!(error_code(&outcome), "core.bug", "{outcome}");
    assert_eq!(outcome["error"]["category"], "bug", "{outcome}");

    let echo = pipe.tool("test.echo", json!({"text": "still here"}), None);
    assert_eq!(echo["status"], "ok", "{echo}");
}
