//! Desktop state: settings that only a verified app sets, fullscreen auto-pause (off by default)
//! that adds and removes only its own reason, and keep-awake held only while a GPU job runs
//! (spec ARCH-127, CORE-160, APP-039, APP-146; Appendix G "Fullscreen", "Keep-awake",
//! "Notifications", rulings P4a-R5 and P4a-R6). Each core polls its `FakeDesktop` every 50 ms
//! (`LIL_TEST_DESKTOP_POLL_MS`); driven through `test.desktop`, `test.job`, `desktop.get`,
//! `jobs.list` and `app/*` from the `ui:test` app (QA-018).

mod p4a;
mod support;

use lil_testkit::{CoreFixture, RawPipe, TestEnv};
use p4a::*;
use serde_json::json;
use support::*;

/// A core whose fake desktop polls every 50 ms.
fn core(env: &TestEnv) -> CoreFixture {
    CoreFixture::start_with_env(env, &[("LIL_TEST_DESKTOP_POLL_MS", "50")])
}

/// Waits until the desktop has polled at least twice more.
fn two_more_polls(pipe: &mut RawPipe) {
    let start = desktop(pipe, json!({}))["polls"].as_u64().unwrap_or(0);
    poll("two more desktop polls", || {
        (desktop(pipe, json!({}))["polls"].as_u64().unwrap_or(0) >= start + 2).then_some(())
    });
}

/// Polls until the `gpu-heavy` lane's reasons, sorted, are `reasons`.
fn until_reasons(pipe: &mut RawPipe, reasons: &[&str]) -> serde_json::Value {
    poll(&format!("gpu-heavy reasons {reasons:?}"), || {
        let lane = lane_state(pipe, "gpu-heavy");
        (sorted_strings(&lane["reasons"]) == reasons).then_some(lane)
    })
}

/// `power_held` as `test.desktop` reports it.
fn power_held(pipe: &mut RawPipe) -> bool {
    desktop(pipe, json!({}))["power_held"] == true
}

#[test] // spec: APP-039, APP-146
fn desktop_settings_are_set_only_by_a_verified_app() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = session(&env, None);
    let change = json!({"notifications": {"job_finished": false}, "keep_awake_gpu": false});

    assert_eq!(
        app_error(&mut pipe, "app/desktop.set", change.clone()),
        "caller.not_allowed"
    );
    let mut old_app = lil_testkit::TestApp::start(&env, None, &[]);
    assert_eq!(
        app_error(&mut old_app, "app/desktop.set", change.clone()),
        "caller.not_allowed",
        "an app without desktop.v1"
    );

    let mut app = app_4a(&env, None, &["desktop"]);
    let set = app_ok(&mut app, "app/desktop.set", change);
    assert_eq!(set["notifications"]["job_finished"], false, "{set}");
    assert_eq!(set["keep_awake_gpu"], false, "{set}");
    let event = wait_for_kind(&mut app, "desktop", "settings");
    assert_eq!(event["settings"]["keep_awake_gpu"], false, "{event}");

    let text = std::fs::read_to_string(env.env.suite_toml()).unwrap_or_default();
    assert_eq!(
        toml_value(&text, "notifications", "job_finished").as_deref(),
        Some("false"),
        "{text}"
    );
    assert_eq!(
        toml_value(&text, "power", "keep_awake_gpu").as_deref(),
        Some("false"),
        "{text}"
    );
    let got = desktop_get(&mut pipe);
    assert_eq!(got["notifications"]["job_finished"], false, "{got}");
    assert_eq!(got["keep_awake_gpu"], false, "{got}");
}

#[test] // spec: CORE-160
fn autopause_is_off_by_default() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = session(&env, None);

    let settings = desktop_get(&mut pipe);
    assert_eq!(settings["fullscreen_autopause"], false, "{settings}");
    assert_eq!(settings["keep_awake_gpu"], true, "{settings}");

    desktop(&mut pipe, json!({"fullscreen": "d3d"}));
    two_more_polls(&mut pipe);
    let lane = lane_state(&mut pipe, "gpu-heavy");
    assert_eq!(lane["paused"], false, "{lane}");
    let id = submit(&mut pipe, "test.job", json!({"lane": "gpu-heavy"}));
    assert_eq!(wait_done(&mut pipe, &id)["status"], "succeeded");
}

#[test] // spec: CORE-160
fn fullscreen_autopause_pauses_and_resumes() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = session(&env, None);
    let mut app = app_4a(&env, None, &[]);
    app_ok(
        &mut app,
        "app/desktop.set",
        json!({"fullscreen_autopause": true}),
    );

    desktop(&mut pipe, json!({"fullscreen": "d3d"}));
    let lane = until_reasons(&mut pipe, &["fullscreen"]);
    assert_eq!(lane["paused"], true, "{lane}");
    let id = submit(&mut pipe, "test.job", json!({"lane": "gpu-heavy"}));
    let held = until_job(&mut pipe, &id, "queue_paused", |j| {
        j["substatus"] == "queue_paused"
    });
    assert_eq!(held["pause_reasons"], json!(["fullscreen"]), "{held}");

    desktop(&mut pipe, json!({"fullscreen": "no"}));
    until_reasons(&mut pipe, &[]);
    assert_eq!(wait_done(&mut pipe, &id)["status"], "succeeded");
}

#[test] // spec: CORE-160
fn fullscreen_reason_never_clears_a_manual_pause() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = session(&env, None);
    let mut app = app_4a(&env, None, &[]);
    app_ok(
        &mut app,
        "app/desktop.set",
        json!({"fullscreen_autopause": true}),
    );

    app_ok(&mut app, "app/queue.pause", json!({}));
    desktop(&mut pipe, json!({"fullscreen": "d3d"}));
    until_reasons(&mut pipe, &["fullscreen", "manual"]);
    let id = submit(&mut pipe, "test.job", json!({"lane": "gpu-heavy"}));

    desktop(&mut pipe, json!({"fullscreen": "no"}));
    let lane = until_reasons(&mut pipe, &["manual"]);
    assert_eq!(lane["paused"], true, "your own pause stays: {lane}");
    two_more_polls(&mut pipe);
    let still = job(&mut pipe, &id);
    assert_eq!(still["substatus"], "queue_paused", "{still}");
    assert_eq!(still["pause_reasons"], json!(["manual"]), "{still}");

    app_ok(&mut app, "app/queue.resume", json!({}));
    assert_eq!(wait_done(&mut pipe, &id)["status"], "succeeded");
}

#[test] // spec: ARCH-127
fn keep_awake_is_held_only_while_a_gpu_job_runs() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut pipe = session(&env, None);
    assert!(!power_held(&mut pipe), "nothing runs yet");

    let gpu = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "gated": true}),
    );
    until_job(&mut pipe, &gpu, "running", running);
    poll("the power request held", || {
        power_held(&mut pipe).then_some(())
    });
    release(&mut pipe, &gpu);
    wait_done(&mut pipe, &gpu);
    poll("the power request released", || {
        (!power_held(&mut pipe)).then_some(())
    });

    let cpu = submit(&mut pipe, "test.job", json!({"lane": "cpu", "gated": true}));
    until_job(&mut pipe, &cpu, "running", running);
    two_more_polls(&mut pipe);
    assert!(!power_held(&mut pipe), "never for a cpu job");
    release(&mut pipe, &cpu);
    wait_done(&mut pipe, &cpu);

    let mut app = app_4a(&env, None, &[]);
    app_ok(
        &mut app,
        "app/desktop.set",
        json!({"keep_awake_gpu": false}),
    );
    let gpu = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "gated": true}),
    );
    until_job(&mut pipe, &gpu, "running", running);
    two_more_polls(&mut pipe);
    assert!(!power_held(&mut pipe), "never with keep_awake_gpu off");
    release(&mut pipe, &gpu);
    wait_done(&mut pipe, &gpu);
}
