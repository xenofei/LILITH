//! Notifications: toasts by category with their defaults and per-origin coalescing, none while a
//! suite window is in front, quiet while fullscreen, no progress toasts, the notification path
//! that lets a confirmation wait with no app open, the toast's Deny with its one-use token, and
//! the agent-activity toasts (spec ARCH-005, ARCH-055, ARCH-103, CORE-113, APP-022, APP-051,
//! APP-065, APP-146, QA-016; Appendix G "Notifications", rulings P4a-R7, P4a-R8, P4a-R9 and
//! P4a-R36). Each core is on a test channel, whose `FakeDesktop` records every toast; driven
//! through `test.desktop`, `test.job`, `test.destructive`, `notify.claude_code`, `desktop.get`,
//! `app/*` from the `ui:test` app, `lil` and `lil-mcp` under the fake `claude` (QA-018).

mod p4a;
mod support;

use lil_testkit::{CoreFixture, TestEnv};
use p4a::*;
use serde_json::{Value, json};
use support::*;

/// The categories of Appendix G, each on by default; there is no `job_progress`.
const CATEGORIES: [&str; 6] = [
    "job_finished",
    "job_failed",
    "confirmation_waiting",
    "cap_alert",
    "director_decision",
    "agent_activity",
];

/// A core whose fake desktop polls every 50 ms, ready for toasts.
fn ready_core(env: &TestEnv) -> (CoreFixture, lil_testkit::RawPipe) {
    let core = CoreFixture::start_with_env(env, &[("LIL_TEST_DESKTOP_POLL_MS", "50")]);
    let mut pipe = session(env, None);
    ready_desktop(&mut pipe);
    (core, pipe)
}

/// A `cli` batch of `n` quick `cpu` jobs, waited for; returns their ids.
fn finished_batch(pipe: &mut lil_testkit::RawPipe, n: u64) -> Vec<String> {
    let batch = write_ok(pipe, "test.job", json!({"lane": "cpu", "batch": n}));
    let ids = job_ids_of(&batch);
    for id in &ids {
        assert_eq!(wait_done(pipe, id)["status"], "succeeded");
    }
    ids
}

/// Waits until the desktop has polled at least twice more, so a change has been seen.
fn two_more_polls(pipe: &mut lil_testkit::RawPipe) {
    let start = desktop(pipe, json!({}))["polls"].as_u64().unwrap_or(0);
    poll("two more desktop polls", || {
        (desktop(pipe, json!({}))["polls"].as_u64().unwrap_or(0) >= start + 2).then_some(())
    });
}

/// The confirmation toast for `id`: its `(open, deny)` action URLs, checked.
fn confirmation_toast(
    pipe: &mut lil_testkit::RawPipe,
    env: &TestEnv,
    id: &str,
) -> (String, String) {
    let scheme = scheme(env);
    let posts = until_posted(pipe, "confirmation_waiting");
    let toast = posts
        .iter()
        .find(|t| t["tag"] == id)
        .unwrap_or_else(|| panic!("no confirmation toast tagged {id}: {posts:?}"));
    assert_eq!(toast["launch"], format!("{scheme}:confirm/{id}"), "{toast}");
    let actions = toast_actions(toast);
    assert_eq!(actions.len(), 2, "Open and Deny: {toast}");
    assert_eq!(actions[0].0, "Open", "{toast}");
    assert_eq!(actions[0].1, format!("{scheme}:confirm/{id}"), "{toast}");
    assert_eq!(actions[1].0, "Deny", "{toast}");
    let deny = actions[1].1.clone();
    let token = deny
        .strip_prefix(&format!("{scheme}:deny/{id}/"))
        .unwrap_or_else(|| panic!("the Deny URL is not {scheme}:deny/{id}/<token>: {deny}"));
    assert!(is_hex(token, 32), "a 128-bit token as 32 hex: {deny}");
    (actions[0].1.clone(), deny)
}

/// The token at the end of a Deny URL.
fn token_of(deny_url: &str) -> String {
    deny_url.rsplit('/').next().unwrap_or_default().to_string()
}

#[test] // spec: APP-146, APP-022
fn toasts_follow_categories_and_defaults() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);

    let settings = desktop_get(&mut pipe);
    let notifications = settings["notifications"]
        .as_object()
        .unwrap_or_else(|| panic!("no notifications: {settings}"));
    for category in CATEGORIES {
        assert_eq!(
            notifications.get(category),
            Some(&json!(true)),
            "{settings}"
        );
    }
    assert!(
        !notifications.contains_key("job_progress"),
        "the progress category is withdrawn: {settings}"
    );
    assert_eq!(settings["quiet_while_fullscreen"], true, "{settings}");

    let scheme = scheme(&env);
    let ids = finished_batch(&mut pipe, 3);
    let posts = until_posted(&mut pipe, "job_finished");
    assert_eq!(posts.len(), 1, "one toast per batch: {posts:?}");
    let toast = &posts[0];
    assert_eq!(toast["title"], "Batch finished (3 jobs)", "{toast}");
    let launch = toast["launch"].as_str().unwrap_or_default();
    assert!(
        ids.iter().any(|id| launch == format!("{scheme}:task/{id}")),
        "launches one of the batch's jobs: {toast}"
    );
    // A `cli` batch isn't from a panel: coalesced per origin under `o_<16 hex>`.
    let tag = toast["tag"].as_str().unwrap_or_default();
    assert!(tag.starts_with("o_") && is_hex(&tag[2..], 16), "{toast}");

    let mut app = app_4a(&env, None, &[]);
    let set = app_ok(
        &mut app,
        "app/desktop.set",
        json!({"notifications": {"job_finished": false}}),
    );
    assert_eq!(set["notifications"]["job_finished"], false, "{set}");
    finished_batch(&mut pipe, 2);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        posts_in(&mut pipe, "job_finished").len(),
        1,
        "nothing is posted for a category that is off"
    );
}

#[test] // spec: APP-146
fn no_toast_while_a_suite_window_is_in_front() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);
    let _app = app_4a(&env, None, &[]);

    desktop(&mut pipe, json!({"foreground": "suite"}));
    finished_batch(&mut pipe, 1);
    desktop(&mut pipe, json!({"foreground": "other"}));
    two_more_polls(&mut pipe);
    assert!(
        posts_in(&mut pipe, "job_finished").is_empty(),
        "nothing is posted or queued while a suite window is in front"
    );

    finished_batch(&mut pipe, 1);
    assert_eq!(until_posted(&mut pipe, "job_finished").len(), 1);
}

#[test] // spec: APP-146
fn quiet_while_fullscreen_posts_silently_and_never_drops() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);
    path_ready(&mut pipe);

    desktop(&mut pipe, json!({"fullscreen": "d3d"}));
    two_more_polls(&mut pipe);
    let posted = write_ok(
        &mut pipe,
        "notify.claude_code",
        json!({"message": "Spec render finished"}),
    );
    assert_eq!(posted["posted"], true, "{posted}");
    let posts = until_posted(&mut pipe, "agent_activity");
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0]["silent"], true, "{posts:?}");

    // A confirmation is made quiet like the rest, never dropped.
    let pending = pipe.tool("test.destructive", json!({}), Some(&key()));
    let id = pending_id(&pending);
    let posts = until_posted(&mut pipe, "confirmation_waiting");
    let toast = posts
        .iter()
        .find(|t| t["tag"] == id.as_str())
        .unwrap_or_else(|| panic!("no toast for {id}: {posts:?}"));
    assert_eq!(toast["silent"], true, "{toast}");
}

#[test] // spec: APP-146
fn no_progress_toast_for_a_long_job() {
    let env = TestEnv::new();
    // An old `job_progress` key is kept and ignored.
    env.suite_toml_set("[notifications]\njob_progress = true\n");
    let (_core, mut pipe) = ready_core(&env);
    assert!(
        desktop_get(&mut pipe)["notifications"]
            .get("job_progress")
            .is_none(),
        "job_progress is not a category"
    );

    let id = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "gated": true, "expected_secs": 60}),
    );
    until_job(&mut pipe, &id, "running", running);
    two_more_polls(&mut pipe);
    assert_eq!(
        toast_ops(&mut pipe),
        Vec::<Value>::new(),
        "no toast while it runs"
    );

    release(&mut pipe, &id);
    assert_eq!(wait_done(&mut pipe, &id)["status"], "succeeded");
    let posts = until_posted(&mut pipe, "job_finished");
    let ops = toast_ops(&mut pipe);
    assert_eq!(ops.len(), 1, "only the job_finished post: {ops:?}");
    assert_eq!(posts[0]["title"], "Job finished", "{posts:?}");
}

#[test] // spec: ARCH-055, CORE-113, APP-051, QA-016
fn with_the_notification_path_a_call_waits_without_an_app() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);
    path_ready(&mut pipe);
    let k = key();

    let pending = pipe.tool("test.destructive", json!({}), Some(&k));
    let id = pending_id(&pending);
    let (open, _deny) = confirmation_toast(&mut pipe, &env, &id);
    assert_eq!(open, format!("{}:confirm/{id}", scheme(&env)));
    let toast = posts_in(&mut pipe, "confirmation_waiting")
        .into_iter()
        .find(|t| t["tag"] == id.as_str())
        .unwrap_or_else(|| panic!("no toast for {id}"));
    assert_eq!(toast["title"], "Confirmation waiting", "{toast}");
    assert_eq!(
        toast["body"], "A script (lil) wants to run test.destructive",
        "{toast}"
    );

    // An app connected later answers it; the retry runs once, and the toast goes.
    let mut app = app_4a(&env, None, &[]);
    answer(&mut app, &id, json!({"answer": "approve"}));
    let ran = ok(&mut pipe, "test.destructive", json!({}), Some(&k));
    assert_eq!(ran["count"], 1, "{ran}");
    poll("the confirmation toast removed", || {
        toast_ops(&mut pipe)
            .iter()
            .any(|t| t["op"] == "remove" && t["tag"] == id.as_str())
            .then_some(())
    });
}

#[test] // spec: APP-022, ARCH-055
fn a_toast_deny_answers_once_with_its_token() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);
    path_ready(&mut pipe);
    let mut app = app_4a(&env, None, &[]);
    let deny_with = |app: &mut lil_testkit::TestApp, id: &str, token: &str| -> Value {
        app.call(
            "app/confirmation.answer",
            json!({"confirmation_id": id, "answer": "deny", "toast_token": token}),
        )
    };
    let code = |response: &Value| -> String {
        response["result"]["error"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("not an error: {response}"))
            .to_string()
    };

    // A wrong token, then the right one: denied once.
    let k = key();
    let first = pending_id(&pipe.tool("test.destructive", json!({}), Some(&k)));
    let (_, deny) = confirmation_toast(&mut pipe, &env, &first);
    let token = token_of(&deny);
    let wrong = "0123456789abcdef0123456789abcdef";
    assert_ne!(token, wrong);
    assert_eq!(code(&deny_with(&mut app, &first, wrong)), "confirm.unknown");
    let denied = deny_with(&mut app, &first, &token);
    assert_eq!(denied["result"]["status"], "ok", "{denied}");
    let retried = pipe.tool("test.destructive", json!({}), Some(&k));
    assert_eq!(error_code(&retried), "confirm.denied", "{retried}");
    // The token died when it was used.
    assert_eq!(
        code(&deny_with(&mut app, &first, &token)),
        "confirm.unknown"
    );

    // Answered in the window: its toast's token is dead at once.
    let second = pending_id(&pipe.tool("test.destructive", json!({}), Some(&key())));
    let (_, deny) = confirmation_toast(&mut pipe, &env, &second);
    answer(&mut app, &second, json!({"answer": "deny"}));
    assert_eq!(
        code(&deny_with(&mut app, &second, &token_of(&deny))),
        "confirm.unknown"
    );

    // A token only denies, and never with a cap.
    let third = pending_id(&pipe.tool("test.destructive", json!({}), Some(&key())));
    let (_, deny) = confirmation_toast(&mut pipe, &env, &third);
    let token = token_of(&deny);
    let approve = app.call(
        "app/confirmation.answer",
        json!({"confirmation_id": third, "answer": "approve", "toast_token": token}),
    );
    assert_eq!(code(&approve), "params.invalid", "{approve}");
    let with_cap = app.call(
        "app/confirmation.answer",
        json!({"confirmation_id": third, "answer": "deny", "toast_token": token,
               "cap": {"action": "allow_once"}}),
    );
    assert_eq!(code(&with_cap), "params.invalid", "{with_cap}");
}

#[test] // spec: APP-146, ARCH-055
fn turning_off_confirmation_toasts_makes_agent_calls_unavailable() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);
    path_ready(&mut pipe);
    pending_id(&pipe.tool("test.destructive", json!({}), Some(&key())));

    {
        let mut app = app_4a(&env, None, &[]);
        app_ok(
            &mut app,
            "app/desktop.set",
            json!({"notifications": {"confirmation_waiting": false}}),
        );
    }
    let settings = desktop_get(&mut pipe);
    assert_eq!(
        settings["notifications"]["confirmation_waiting"], false,
        "{settings}"
    );
    let refused = pipe.tool("test.destructive", json!({}), Some(&key()));
    assert_eq!(error_code(&refused), "confirm.unavailable", "{refused}");
}

#[test] // spec: APP-022, ARCH-055, QA-016
fn toasts_off_in_windows_make_the_path_unreachable() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);
    path_ready(&mut pipe);

    desktop(
        &mut pipe,
        json!({"toasts": "disabled_for_app", "clear": true}),
    );
    let settings = desktop_get(&mut pipe);
    let path = &settings["notification_path"];
    assert_eq!(path["toasts"], "unavailable", "{settings}");
    assert_eq!(path["toasts_reason"], "disabled_for_app", "{settings}");

    let refused = pipe.tool("test.destructive", json!({}), Some(&key()));
    assert_eq!(error_code(&refused), "confirm.unavailable", "{refused}");
    assert_eq!(toast_ops(&mut pipe), Vec::<Value>::new(), "no toast");
}

#[test] // spec: QA-016, ARCH-055
fn non_interactive_callers_never_wait() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);
    path_ready(&mut pipe);

    let out = env.lil(&[
        "--non-interactive",
        "call",
        "test.destructive",
        "{}",
        "--idempotency-key",
        &key(),
    ]);
    assert_eq!(out.code, 1, "{out:?}");
    assert_eq!(
        error_code(&stdout_json(&out)),
        "confirm.unavailable",
        "{out:?}"
    );
    assert!(
        posts_in(&mut pipe, "confirmation_waiting").is_empty(),
        "no confirmation toast for a non-interactive caller"
    );
}

#[test] // spec: APP-022
fn toast_text_is_escaped_and_normalised() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);

    let message = "a<b & c\u{202E}d\u{0007}e";
    write_ok(
        &mut pipe,
        "notify.claude_code",
        json!({ "message": message }),
    );
    let posts = until_posted(&mut pipe, "agent_activity");
    let toast = &posts[0];
    let body = toast["body"].as_str().unwrap_or_default();
    assert!(body.contains("\u{27E8}U+202E\u{27E9}"), "{toast}");
    assert!(!body.chars().any(char::is_control), "{toast}");
    let xml = toast_xml(toast);
    assert!(
        xml.contains("a&lt;b &amp; c\u{27E8}U+202E\u{27E9}de"),
        "{xml}"
    );
    assert!(
        !xml.contains('\u{202E}') && !xml.contains('\u{0007}'),
        "{xml:?}"
    );
}

#[test] // spec: ARCH-103, APP-146
fn notify_claude_code_posts_agent_activity_and_drops_duplicates() {
    let env = TestEnv::new();
    let (_core, mut pipe) = ready_core(&env);

    let message = "Spec task needs your review";
    let first = write_ok(
        &mut pipe,
        "notify.claude_code",
        json!({ "message": message }),
    );
    assert_eq!(first["posted"], true, "{first}");
    let again = write_ok(
        &mut pipe,
        "notify.claude_code",
        json!({ "message": message }),
    );
    assert_eq!(again["posted"], false, "{again}");
    assert_eq!(again["reason"], "duplicate", "{again}");

    let posts = until_posted(&mut pipe, "agent_activity");
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0]["title"], "Claude Code", "{posts:?}");
    assert_eq!(posts[0]["body"], message, "{posts:?}");

    let long = "x".repeat(2001);
    let refused = pipe.tool(
        "notify.claude_code",
        json!({ "message": long }),
        Some(&key()),
    );
    assert_eq!(error_code(&refused), "params.invalid", "{refused}");
}

#[test] // spec: APP-065, ARCH-005
fn agent_open_posts_agent_activity() {
    let env = TestEnv::new();
    let viewer = env.root().join("viewer").join("spec-viewer.exe");
    lil_testkit::files::write_file(
        &env.env.data_dir().join(r"apps\viewer.toml"),
        format!(
            "app = \"viewer\"\nexe = '{}'\n[[opens]]\ntypes = [\"image\"]\nformats = [\"png\"]\nintents = [\"open\", \"edit\"]\n",
            path_str(&viewer)
        )
        .as_bytes(),
    );
    let target = env.root().join("picture.png");
    lil_testkit::files::write_file(&target, b"\x89PNG\r\n\x1a\n");
    let (_core, mut pipe) = ready_core(&env);

    let mut mcp = env.mcp_under_claude();
    mcp_initialize(&mut mcp);
    let shown = mcp_call(&mut mcp, "ui_show", json!({"target": path_str(&target)}));
    assert!(!shown.is_error, "{}", shown.text);

    let posts = until_posted(&mut pipe, "agent_activity");
    let toast = &posts[0];
    assert_eq!(toast["title"], "Claude Code", "{toast}");
    assert!(
        toast["body"]
            .as_str()
            .is_some_and(|b| b.starts_with("Claude Code opened ")),
        "{toast}"
    );
    let launch = toast["launch"].as_str().unwrap_or_default();
    let opened = open_target(&scheme(&env), launch);
    assert!(opened.contains("picture.png"), "{opened:?}");
}
