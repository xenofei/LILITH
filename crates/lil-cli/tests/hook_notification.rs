//! `lil hook notification` (ARCH-103, AI-116; Appendix G "Plugin 1.3.1"): Claude Code's
//! Notification hook. It reads the hook JSON (`message`, `title?`), calls `notify.claude_code`
//! on a core that already runs (never starting one), prints nothing and exits 0. Driven through
//! the real `lil` against a test-channel core, whose fake desktop records the toasts
//! (`test.desktop`); no real toast is posted (QA-038).

use std::ffi::OsString;
use std::time::Duration;

use lil_testkit::{CoreFixture, RawPipe, TestEnv};
use serde_json::{Value, json};

fn key() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("hook-notification-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// `lil hook notification` with `input` on stdin and `extra` variables; checks it printed
/// nothing and exited 0.
fn hook(env: &TestEnv, input: &str, extra: &[(&str, OsString)]) {
    let out = env.lil_in_with(env.root(), &["hook", "notification"], extra, Some(input));
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert_eq!(out.stderr, "", "{out:?}");
}

/// A Notification hook input as Claude Code sends it.
fn notice(message: &str, title: Option<&str>) -> String {
    let mut input = json!({"session_id": "s", "transcript_path": "t.jsonl", "cwd": ".",
                           "hook_event_name": "Notification", "message": message,
                           "notification_type": "permission_prompt"});
    if let Some(title) = title {
        input["title"] = json!(title);
    }
    input.to_string()
}

/// A connection to the running core, its fake desktop ready to post toasts.
fn ready_desktop(env: &TestEnv) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe.tool_ok(
        "test.desktop",
        json!({"toasts": "ready", "shortcut": true, "handlers": true, "foreground": "other",
               "fullscreen": "no", "clear": true}),
        Some(&key()),
    );
    pipe
}

/// The `agent_activity` toasts the fake desktop posted.
fn posted(pipe: &mut RawPipe) -> Vec<Value> {
    let recorded = pipe.tool_ok("test.desktop", json!({}), Some(&key()));
    recorded["toasts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["op"] == "post" && t["group"] == "agent_activity")
        .cloned()
        .collect()
}

#[test] // spec: ARCH-103, AI-116, ARCH-018
fn notification_hook_calls_notify_without_starting_a_core() {
    let env = TestEnv::new();
    hook(&env, &notice("Claude needs your permission", None), &[]);
    assert!(
        RawPipe::try_connect(&env, Duration::ZERO).is_none(),
        "the hook started a core"
    );

    let _core = CoreFixture::start(&env);
    let mut pipe = ready_desktop(&env);
    hook(
        &env,
        &notice(
            "Claude needs your permission to use Bash",
            Some("Permission"),
        ),
        &[],
    );
    let toasts = posted(&mut pipe);
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0]["title"], "Claude Code: Permission", "{toasts:?}");
    assert_eq!(
        toasts[0]["body"], "Claude needs your permission to use Bash",
        "{toasts:?}"
    );

    // A notice longer than notify.claude_code takes is cut, not dropped.
    hook(
        &env,
        &notice(&"x".repeat(2500), Some(&"t".repeat(150))),
        &[],
    );
    assert_eq!(posted(&mut pipe).len(), 2);
}

#[test] // spec: ARCH-103, ARCH-107
fn malformed_notification_input_prints_nothing() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = ready_desktop(&env);
    for input in [
        "not json".to_string(),
        "[]".to_string(),
        json!({"hook_event_name": "Notification"}).to_string(),
        json!({"message": ""}).to_string(),
        json!({"message": 5}).to_string(),
        json!({"message": "hi", "title": 5}).to_string(),
    ] {
        hook(&env, &input, &[]);
    }
    // In Engine A the hook does nothing (ARCH-107).
    hook(
        &env,
        &notice("from Engine A", None),
        &[("LIL_ENGINE", OsString::from("1"))],
    );
    assert_eq!(posted(&mut pipe), Vec::<Value>::new());
}
