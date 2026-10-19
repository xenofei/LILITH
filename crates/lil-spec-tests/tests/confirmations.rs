//! Agent safety confirmations: a destructive agent call waits for the app's answer and then
//! runs once; denied and expired answers; non-interactive callers never get a pending result;
//! a retry with other arguments is refused; and a pending confirmation keeps the core up (spec
//! ARCH-020, ARCH-052, ARCH-053, ARCH-055, CORE-016, CORE-017, CORE-109, CORE-111, CORE-115,
//! APP-044, APP-046, QA-016; Appendix F "Confirmations", "`app/*` methods"). Driven through the
//! `test.destructive` test-channel tool from a `cli` connection, `lil` and the `ui:test` app
//! (QA-018).

mod support;

use std::time::Duration;

use lil_testkit::{CoreFixture, TestApp, TestEnv};
use serde_json::json;
use support::*;

/// How often `test.destructive` has run in this core, by running it once more from the app
/// (a `ui:*` caller's destructive call runs at once). Returns the count before that run.
fn runs_so_far(app: &mut TestApp) -> u64 {
    let ran = write_ok(app, "test.destructive", json!({}));
    ran["count"]
        .as_u64()
        .unwrap_or_else(|| panic!("test.destructive returned no count: {ran}"))
        - 1
}

#[test] // spec: ARCH-052, ARCH-053, ARCH-055, CORE-017, CORE-109, CORE-111, APP-044, APP-046
fn agent_destructive_call_waits_for_the_app_and_runs_once() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &["confirmations"]);
    let mut pipe = session(&env, None);
    let k = key();

    let pending = pipe.tool("test.destructive", json!({}), Some(&k));
    let confirmation = pending_id(&pending);
    assert_eq!(pending["retry_token"], k.as_str(), "{pending}");
    assert_eq!(pending["covers"], json!(["confirm"]), "{pending}");

    let again = pipe.tool("test.destructive", json!({}), Some(&k));
    assert_eq!(
        pending_id(&again),
        confirmation,
        "a retry before the answer: {again}"
    );

    // Agents never answer.
    let by_agent = pipe.call(
        "app/confirmation.answer",
        json!({"confirmation_id": confirmation, "answer": "approve"}),
    );
    assert_eq!(
        by_agent["result"]["error"]["code"], "caller.not_allowed",
        "{by_agent}"
    );

    answer(&mut app, &confirmation, json!({"answer": "approve"}));
    let ran = ok(&mut pipe, "test.destructive", json!({}), Some(&k));
    assert_eq!(ran["count"], 1, "{ran}");
    let replayed = ok(&mut pipe, "test.destructive", json!({}), Some(&k));
    assert_eq!(
        replayed["count"], 1,
        "the approved call runs once: {replayed}"
    );

    let history = confirmations(&mut app, "history");
    let entry = history
        .iter()
        .find(|c| c["id"] == confirmation.as_str())
        .unwrap_or_else(|| panic!("no {confirmation} in the history: {history:?}"));
    assert_eq!(entry["state"], "approved", "{entry}");
    assert_eq!(entry["confirmed_via"], "app", "{entry}");
    assert_eq!(entry["tool"], "test.destructive", "{entry}");
    assert_eq!(entry["caller_kind"], "cli", "{entry}");

    // The app's own destructive call needs no confirmation.
    assert_eq!(runs_so_far(&mut app), 1);
}

#[test] // spec: CORE-115
fn denied_confirmation_is_confirm_denied() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &[]);
    let mut pipe = session(&env, None);
    let k = key();

    let confirmation = pending_id(&pipe.tool("test.destructive", json!({}), Some(&k)));
    answer(&mut app, &confirmation, json!({"answer": "deny"}));
    let denied = pipe.tool("test.destructive", json!({}), Some(&k));
    assert_eq!(error_code(&denied), "confirm.denied", "{denied}");
    assert_eq!(runs_so_far(&mut app), 0, "a denied call never runs");
}

#[test] // spec: CORE-115
fn expired_confirmation_is_confirm_expired() {
    let env = TestEnv::new();
    let _core = CoreFixture::start_with_env(&env, &[("LIL_TEST_CONFIRM_LIFETIME_SECS", "1")]);
    let _app = TestApp::start(&env, None, &[]);
    let mut pipe = session(&env, None);
    let k = key();

    pending_id(&pipe.tool("test.destructive", json!({}), Some(&k)));
    std::thread::sleep(Duration::from_secs(2));
    let expired = pipe.tool("test.destructive", json!({}), Some(&k));
    assert_eq!(error_code(&expired), "confirm.expired", "{expired}");
}

#[test] // spec: ARCH-055, QA-016
fn non_interactive_agents_never_get_pending() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &[]);

    let k = key();
    let out = env.lil(&[
        "--non-interactive",
        "call",
        "test.destructive",
        "{}",
        "--idempotency-key",
        &k,
    ]);
    assert_eq!(out.code, 1, "{out:?}");
    let outcome = stdout_json(&out);
    assert_eq!(error_code(&outcome), "confirm.unavailable", "{out:?}");
    assert!(
        confirmations(&mut app, "pending").is_empty(),
        "a non-interactive call must leave no confirmation"
    );
    assert!(confirmations(&mut app, "history").is_empty());
}

#[test] // spec: CORE-016
fn retry_with_other_arguments_is_refused() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &[]);
    let mut pipe = session(&env, None);
    let k = key();

    pending_id(&pipe.tool("test.destructive", json!({}), Some(&k)));
    let other = pipe.tool("test.destructive", json!({"cost": "1"}), Some(&k));
    assert_eq!(error_code(&other), "idempotency.conflict", "{other}");
    assert_eq!(runs_so_far(&mut app), 0, "nothing ran");
}

#[test] // spec: ARCH-020
fn pending_confirmation_keeps_the_core_up() {
    let env = TestEnv::new();
    let mut core = CoreFixture::start_with_env(
        &env,
        &[
            ("LIL_TEST_IDLE_SECS", "2"),
            ("LIL_TEST_CONFIRM_LIFETIME_SECS", "8"),
        ],
    );
    {
        let _app = TestApp::start(&env, None, &[]);
        let mut pipe = session(&env, None);
        pending_id(&pipe.tool("test.destructive", json!({}), Some(&key())));
    }
    // No client is connected now; only the pending confirmation holds the core up.
    assert_eq!(
        core.wait_exit(Duration::from_secs(5)),
        None,
        "the core went idle with a confirmation pending"
    );
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(0),
        "once the confirmation expired the core exits as idle"
    );
}
