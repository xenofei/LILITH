//! The notifier (ARCH-005, ARCH-055, CORE-113, APP-022, APP-051, APP-065, APP-146, QA-016;
//! Appendix G "Notifications"; rulings P4a-R7, P4a-R8, P4a-R9, P4a-R36, P4a-R41): the toast
//! XML, toasts by category with their defaults, one per batch, none while a suite window is
//! in front, quiet while fullscreen, no progress toasts, the restart hold's toast, per-origin
//! coalescing, the confirmation toast with Open and Deny and its one-use token, cap alerts,
//! Claude Code's notices and an agent's opens.
//!
//! The XML and the batch rule are checked as functions; everything else runs over the pipe of
//! an in-process core on a test channel, whose `FakeDesktop` records each toast
//! (`test.desktop`). No real toast is posted (QA-038). Wall-clock durations are failure
//! deadlines only.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lil_core::identity::TrustedDirs;
use lil_core::interchange::notify::batch_end;
use lil_core::interchange::{ToastActions, ToastText, toast_xml};
use lil_core::jobs::{BatchJob, JobStatus, Priority, StartedBy, Surface};
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_platform::IdSource;
use lil_testkit::{FakeClock, RawPipe, SeqIds, TestApp, TestEnv, ensure_built};
use serde_json::{Value, json};

/// Failure deadline for anything a test waits on.
const DEADLINE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------------------------
// The harness.
// ---------------------------------------------------------------------------------------------

fn key() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("k-notify-{}", N.fetch_add(1, Ordering::SeqCst))
}

/// An in-process core that trusts the test app, polls the fake desktop every 50 ms, and runs
/// on `clock` with `ids` (`None`: the OS random source, as a real core).
fn core_with(
    env: &TestEnv,
    clock: Arc<FakeClock>,
    ids: Option<Arc<dyn IdSource>>,
    lifetime: Option<Duration>,
) -> CoreThread {
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        clock,
        ids: ids.unwrap_or_else(|| base.ids.clone()),
        test_parent_pid: None,
        desktop_poll: Some(Duration::from_millis(50)),
        confirm_lifetime: lifetime.unwrap_or(base.confirm_lifetime),
        trusted: TrustedDirs {
            app_dirs: vec![ensure_built().dir.clone()],
            test_app: true,
            ..base.trusted.clone()
        },
        ..base
    })
}

fn core(env: &TestEnv) -> CoreThread {
    core_with(env, FakeClock::new(), Some(Arc::new(SeqIds::new())), None)
}

fn pipe(env: &TestEnv, hello: Value) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let response = pipe.hello(hello);
    assert_eq!(response["result"]["status"], "ok", "hello: {response}");
    pipe
}

/// The `ui:test` app with every capability of this protocol.
fn app(env: &TestEnv) -> TestApp {
    TestApp::start_with_capabilities(env, None, &[], lil_protocol::CAPABILITIES)
}

/// A write call that must succeed: its `structured`.
fn write(p: &mut RawPipe, name: &str, args: Value) -> Value {
    p.tool_ok(name, args, Some(&key()))
}

fn desktop(p: &mut RawPipe, args: Value) -> Value {
    write(p, "test.desktop", args)
}

/// The path ready: toasts ready, shortcut and handlers in place, another program in front,
/// no fullscreen; the recorded toasts cleared.
fn ready(p: &mut RawPipe) {
    desktop(
        p,
        json!({"toasts": "ready", "shortcut": true, "handlers": true, "foreground": "other",
               "fullscreen": "no", "clear": true}),
    );
}

fn ops(p: &mut RawPipe) -> Vec<Value> {
    desktop(p, json!({}))["toasts"].as_array().unwrap().clone()
}

fn posts(p: &mut RawPipe, group: &str) -> Vec<Value> {
    ops(p)
        .into_iter()
        .filter(|t| t["op"] == "post" && t["group"] == group)
        .collect()
}

fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "{what}: not within {DEADLINE:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Polls until `group` has at least `n` posts; returns them.
fn until_posts(p: &mut RawPipe, group: &str, n: usize) -> Vec<Value> {
    until(&format!("{n} {group} toast(s)"), || {
        let found = posts(p, group);
        (found.len() >= n).then_some(found)
    })
}

fn wait_done(p: &mut RawPipe, id: &str) -> Value {
    let waited = p.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 30}), None);
    assert_eq!(waited["done"], true, "{waited}");
    waited["job"].clone()
}

fn job_ids(structured: &Value) -> Vec<String> {
    match structured.get("jobs") {
        Some(jobs) => jobs
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["job_id"].as_str().unwrap().to_string())
            .collect(),
        None => vec![structured["job"]["job_id"].as_str().unwrap().to_string()],
    }
}

/// A finished `cli` batch of `n` quick `cpu` jobs; its ids.
fn batch(p: &mut RawPipe, n: u64) -> Vec<String> {
    let ids = job_ids(&write(p, "test.job", json!({"lane": "cpu", "batch": n})));
    for id in &ids {
        assert_eq!(wait_done(p, id)["status"], "succeeded");
    }
    ids
}

/// Everything the notifier had to handle before now has been handled: a `cli` job finishes
/// and its own toast is posted (the notifier handles events in order). Needs `job_finished`
/// on and the path ready.
fn fence(p: &mut RawPipe, env: &TestEnv) {
    let id = batch(p, 1).remove(0);
    let launch = format!("{}:task/{id}", env.env.activation_scheme());
    until("the fence's toast", || {
        posts(p, "job_finished")
            .iter()
            .any(|t| t["launch"] == launch.as_str())
            .then_some(())
    });
}

fn pending_id(outcome: &Value) -> String {
    assert_eq!(outcome["status"], "pending_confirmation", "{outcome}");
    outcome["confirmation_id"].as_str().unwrap().to_string()
}

fn error_code(outcome: &Value) -> String {
    outcome["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `(content, arguments, activationType)` of each `<action>` of a toast's XML.
fn actions(xml: &str) -> Vec<(String, String, String)> {
    let doc = roxmltree::Document::parse(xml).unwrap_or_else(|e| panic!("{e}: {xml}"));
    doc.descendants()
        .filter(|n| n.has_tag_name("action"))
        .map(|n| {
            let a = |k: &str| n.attribute(k).unwrap_or_default().to_string();
            (a("content"), a("arguments"), a("activationType"))
        })
        .collect()
}

/// The confirmation toast of `id`, posted: its Deny URL's token.
fn deny_token(p: &mut RawPipe, scheme: &str, id: &str) -> String {
    let toast = until(&format!("the toast of {id}"), || {
        posts(p, "confirmation_waiting")
            .into_iter()
            .find(|t| t["tag"] == id)
    });
    let xml = toast["xml"].as_str().unwrap();
    let deny = actions(xml)[1].1.clone();
    deny.strip_prefix(&format!("{scheme}:deny/{id}/"))
        .unwrap_or_else(|| panic!("not a deny URL of {id}: {deny}"))
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// The toast XML.
// ---------------------------------------------------------------------------------------------

#[test] // spec: APP-022, ARCH-060
fn toast_xml_escapes_and_normalises() {
    let xml = toast_xml(&ToastText {
        title: "a <b> & \"c\"".into(),
        body: "x\u{202E}y\u{0007}z 'q'".into(),
        launch: Some("lil-test-a:task/j_0123456789abcdef".into()),
        actions: ToastActions::None,
    });
    assert!(xml.contains("a &lt;b&gt; &amp; &quot;c&quot;"), "{xml}");
    assert!(
        xml.contains("x\u{27E8}U+202E\u{27E9}yz &apos;q&apos;"),
        "{xml}"
    );
    assert!(
        !xml.contains('\u{202E}') && !xml.contains('\u{0007}'),
        "{xml:?}"
    );
    let doc = roxmltree::Document::parse(&xml).unwrap_or_else(|e| panic!("{e}: {xml}"));
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "toast");
    assert_eq!(root.attribute("activationType"), Some("protocol"));
    assert_eq!(
        root.attribute("launch"),
        Some("lil-test-a:task/j_0123456789abcdef")
    );
    let texts: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("text"))
        .filter_map(|n| n.text())
        .collect();
    assert_eq!(texts, ["a <b> & \"c\"", "x\u{27E8}U+202E\u{27E9}yz 'q'"]);
    let binding = doc
        .descendants()
        .find(|n| n.has_tag_name("binding"))
        .unwrap();
    assert_eq!(binding.attribute("template"), Some("ToastGeneric"));
    assert!(
        !doc.descendants().any(|n| n.has_tag_name("progress")),
        "no progress binding (P4a-R36)"
    );

    // Non-characters XML forbids are removed, so the toast still parses.
    let odd = toast_xml(&ToastText {
        title: "a\u{FFFF}b".into(),
        body: "c\u{FFFE}d".into(),
        launch: None,
        actions: ToastActions::None,
    });
    let doc = roxmltree::Document::parse(&odd).unwrap_or_else(|e| panic!("{e}: {odd:?}"));
    let texts: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("text"))
        .filter_map(|n| n.text())
        .collect();
    assert_eq!(texts, ["ab", "cd"]);

    // At most 200 characters, ending … when cut.
    let long = toast_xml(&ToastText {
        title: "t".into(),
        body: "b".repeat(500),
        launch: None,
        actions: ToastActions::None,
    });
    let doc = roxmltree::Document::parse(&long).unwrap();
    let body = doc
        .descendants()
        .filter(|n| n.has_tag_name("text"))
        .nth(1)
        .and_then(|n| n.text())
        .unwrap();
    assert_eq!(body.chars().count(), 200);
    assert!(body.ends_with('…'));
    assert_eq!(doc.root_element().attribute("launch"), None, "no target");
}

#[test] // spec: APP-022
fn confirmation_toast_xml_has_open_and_deny() {
    let (open, deny) = (
        "lil-test-a:confirm/cf_0123456789abcdef",
        "lil-test-a:deny/cf_0123456789abcdef/0123456789abcdef0123456789abcdef",
    );
    let xml = toast_xml(&ToastText {
        title: "Confirmation waiting".into(),
        body: "A script (lil) wants to run test.destructive".into(),
        launch: Some(open.into()),
        actions: ToastActions::OpenAndDeny {
            open_url: open.into(),
            deny_url: deny.into(),
        },
    });
    assert_eq!(
        actions(&xml),
        [
            ("Open".into(), open.into(), "protocol".into()),
            ("Deny".into(), deny.into(), "protocol".into())
        ]
    );
    let plain = toast_xml(&ToastText {
        title: "Job finished".into(),
        body: "b".into(),
        launch: Some("lil-test-a:task/j_0123456789abcdef".into()),
        actions: ToastActions::None,
    });
    assert!(actions(&plain).is_empty());
    assert!(!plain.contains("<actions"), "{plain}");
}

// ---------------------------------------------------------------------------------------------
// The batch rule.
// ---------------------------------------------------------------------------------------------

fn summary(id: &str, index: u32, status: &str, surface: &str, retry_of: Option<&str>) -> BatchJob {
    BatchJob {
        job_id: id.into(),
        kind: "test.job".into(),
        priority: Priority::Agent,
        status: JobStatus::parse(status).unwrap(),
        substatus: None,
        project: None,
        created_utc: 100,
        started_utc: Some(101),
        finished_utc: JobStatus::parse(status)
            .unwrap()
            .is_final()
            .then_some(105 + i64::from(index)),
        error_message: None,
        error_code: None,
        retry_of: retry_of.map(str::to_string),
        index,
        origin: Some(StartedBy {
            surface: Surface::parse(surface),
            id: None,
            step: None,
        }),
        batch_id: "b_0123456789abcdef".into(),
    }
}

#[test] // spec: APP-146, ARCH-022
fn a_batch_ended_by_a_core_start_or_stop_never_toasts() {
    let mut interrupted = summary("j_2", 1, "failed", "cli", None);
    interrupted.substatus = Some("interrupted".into());
    // An engine's interruption (ComfyUI restarted, QA-009) still ends the batch's toast.
    interrupted.error_code = Some("comfyui.restarted".into());
    let end = batch_end(&[
        summary("j_1", 0, "succeeded", "cli", None),
        interrupted.clone(),
    ])
    .unwrap();
    assert_eq!(end.failed.unwrap().job_id, "j_2");
    // A core's start or stop: never.
    interrupted.error_code = Some("core.draining".into());
    assert!(
        batch_end(&[
            summary("j_1", 0, "succeeded", "cli", None),
            interrupted.clone()
        ])
        .is_none(),
        "the job that ended last was interrupted"
    );
    // Ended later by a job of its own: it toasts, the interrupted one counted as failed.
    interrupted.finished_utc = Some(100);
    let end = batch_end(&[summary("j_1", 0, "succeeded", "cli", None), interrupted]).unwrap();
    assert_eq!(end.failed.unwrap().job_id, "j_2");
}

#[test] // spec: APP-146
fn cancelled_and_core_jobs_never_toast() {
    // A batch whose every job was cancelled, or whose jobs the core submitted itself.
    assert!(batch_end(&[summary("j_1", 0, "cancelled", "cli", None)]).is_none());
    assert!(batch_end(&[summary("j_1", 0, "succeeded", "core", None)]).is_none());
    // A cancelled job is left out of the count.
    let end = batch_end(&[
        summary("j_1", 0, "succeeded", "cli", None),
        summary("j_2", 1, "cancelled", "cli", None),
    ])
    .unwrap();
    assert_eq!(end.counted.len(), 1);
    // Not yet: a newest attempt is still running. A retried attempt doesn't count.
    assert!(
        batch_end(&[
            summary("j_1", 0, "failed", "cli", None),
            summary("j_2", 0, "running", "cli", Some("j_1")),
        ])
        .is_none()
    );
    let end = batch_end(&[
        summary("j_1", 0, "failed", "cli", None),
        summary("j_2", 0, "succeeded", "cli", Some("j_1")),
    ])
    .unwrap();
    assert_eq!(end.newest, ["j_2"]);
    assert!(end.failed.is_none());

    // Over the pipe: a cancelled job posts nothing.
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let id = job_ids(&write(
        &mut p,
        "test.job",
        json!({"lane": "cpu", "gated": true}),
    ))
    .remove(0);
    write(&mut p, "job.cancel", json!({"job_id": id}));
    assert_eq!(wait_done(&mut p, &id)["status"], "cancelled");
    fence(&mut p, &env);
    let launch = format!("{}:task/{id}", env.env.activation_scheme());
    assert!(
        !ops(&mut p).iter().any(|t| t["launch"] == launch.as_str()),
        "{:?}",
        ops(&mut p)
    );
}

// ---------------------------------------------------------------------------------------------
// Over the pipe.
// ---------------------------------------------------------------------------------------------

#[test] // spec: APP-146
fn categories_and_their_defaults() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    let got = p.tool_ok("desktop.get", json!({}), None);
    let categories: Vec<&String> = got["notifications"].as_object().unwrap().keys().collect();
    assert_eq!(
        categories,
        [
            "agent_activity",
            "cap_alert",
            "confirmation_waiting",
            "director_decision",
            "job_failed",
            "job_finished"
        ]
    );
    assert!(
        got["notifications"]
            .as_object()
            .unwrap()
            .values()
            .all(|on| *on == json!(true)),
        "{got}"
    );
    assert_eq!(got["quiet_while_fullscreen"], true, "{got}");

    // A category that is off posts nothing.
    ready(&mut p);
    let mut a = app(&env);
    let set = a.call(
        "app/desktop.set",
        json!({"notifications": {"agent_activity": false}}),
    );
    assert_eq!(set["result"]["status"], "ok", "{set}");
    let off = write(&mut p, "notify.claude_code", json!({"message": "off"}));
    assert_eq!(off, json!({"posted": false, "reason": "category_off"}));
}

#[test] // spec: APP-146, APP-022
fn one_toast_per_batch_when_its_last_job_ends() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);

    let ids = batch(&mut p, 3);
    until_posts(&mut p, "job_finished", 1);
    fence(&mut p, &env);
    // Counted by launch: one toast, for the batch's first job, and none for the others.
    let all = posts(&mut p, "job_finished");
    let launching = |id: &str| -> Vec<Value> {
        let url = format!("{scheme}:task/{id}");
        all.iter()
            .filter(|t| t["launch"] == url.as_str())
            .cloned()
            .collect()
    };
    let finished = launching(&ids[0]);
    assert_eq!(finished.len(), 1, "one toast for the batch: {all:?}");
    for id in &ids[1..] {
        assert!(
            launching(id).is_empty(),
            "no toast of its own for {id}: {all:?}"
        );
    }
    let toast = &finished[0];
    assert_eq!(toast["title"], "Batch finished (3 jobs)", "{toast}");
    assert_eq!(
        toast["launch"],
        format!("{scheme}:task/{}", ids[0]),
        "{toast}"
    );
    let body = toast["body"].as_str().unwrap();
    assert!(body.starts_with("test.job · no project · "), "{body}");
    assert!(body.ends_with(" s"), "{body}");

    // A failed job: "Job failed" with its error, launching it.
    let failed = job_ids(&write(
        &mut p,
        "test.job",
        json!({"lane": "cpu", "fail": "final"}),
    ))
    .remove(0);
    assert_eq!(wait_done(&mut p, &failed)["status"], "failed");
    let toast = until_posts(&mut p, "job_failed", 1).remove(0);
    assert_eq!(toast["title"], "Job failed", "{toast}");
    assert_eq!(
        toast["launch"],
        format!("{scheme}:task/{failed}"),
        "{toast}"
    );
    assert!(
        toast["body"]
            .as_str()
            .is_some_and(|b| b.starts_with("test.job: ")),
        "{toast}"
    );
    // No job toast has actions.
    for t in ops(&mut p).iter().filter(|t| t["op"] == "post") {
        assert!(actions(t["xml"].as_str().unwrap()).is_empty(), "{t}");
    }
}

#[test] // spec: APP-146
fn a_suite_window_in_front_suppresses_and_nothing_is_queued() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let _app = app(&env);

    desktop(&mut p, json!({"foreground": "suite"}));
    let ids = batch(&mut p, 1);
    let held = write(&mut p, "notify.claude_code", json!({"message": "in front"}));
    assert_eq!(held, json!({"posted": false, "reason": "foreground"}));
    // At once, whether or not the notifier got to the batch yet: it is judged as it ended.
    desktop(&mut p, json!({"foreground": "other"}));
    // Later toasts go out; nothing of the time in front comes with them.
    fence(&mut p, &env);
    let launch = format!("{}:task/{}", env.env.activation_scheme(), ids[0]);
    let all = ops(&mut p);
    assert!(
        !all.iter().any(|t| t["launch"] == launch.as_str()),
        "nothing queued: {all:?}"
    );
    assert!(posts(&mut p, "agent_activity").is_empty(), "{all:?}");
}

#[test] // spec: APP-146
fn quiet_while_fullscreen_sets_silent_and_drops_nothing() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let start = desktop(&mut p, json!({"fullscreen": "d3d"}))["polls"]
        .as_u64()
        .unwrap();
    until("two more polls", || {
        (desktop(&mut p, json!({}))["polls"].as_u64().unwrap() >= start + 2).then_some(())
    });
    let posted = write(
        &mut p,
        "notify.claude_code",
        json!({"message": "quiet one"}),
    );
    assert_eq!(posted["posted"], true, "{posted}");
    let k = key();
    let id = pending_id(&p.tool("test.destructive", json!({}), Some(&k)));
    let toast = until(&format!("the toast of {id}"), || {
        posts(&mut p, "confirmation_waiting")
            .into_iter()
            .find(|t| t["tag"] == id.as_str())
    });
    assert_eq!(
        toast["silent"], true,
        "confirmations are quiet too: {toast}"
    );
    assert_eq!(posts(&mut p, "agent_activity")[0]["silent"], true);

    // With quiet mode off, the same desktop pops.
    let mut a = app(&env);
    let set = a.call("app/desktop.set", json!({"quiet_while_fullscreen": false}));
    assert_eq!(set["result"]["status"], "ok", "{set}");
    drop(a);
    write(&mut p, "notify.claude_code", json!({"message": "loud one"}));
    let all = posts(&mut p, "agent_activity");
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[1]["silent"], false, "{all:?}");
}

#[test] // spec: APP-146
fn no_progress_toast_is_ever_posted() {
    let env = TestEnv::new();
    env.suite_toml_set("[notifications]\njob_progress = true\n");
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let got = p.tool_ok("desktop.get", json!({}), None);
    assert!(got["notifications"].get("job_progress").is_none(), "{got}");

    let id = job_ids(&write(
        &mut p,
        "test.job",
        json!({"lane": "cpu", "gated": true, "expected_secs": 60}),
    ))
    .remove(0);
    until("the job runs", || {
        let listed = p.tool_ok("jobs.list", json!({"ids": [id]}), None);
        (listed["jobs"][0]["status"] == "running").then_some(())
    });
    let start = desktop(&mut p, json!({}))["polls"].as_u64().unwrap();
    until("two more polls", || {
        (desktop(&mut p, json!({}))["polls"].as_u64().unwrap() >= start + 2).then_some(())
    });
    assert_eq!(ops(&mut p), Vec::<Value>::new(), "no toast while it runs");
    write(&mut p, "test.job_release", json!({"job_id": id}));
    assert_eq!(wait_done(&mut p, &id)["status"], "succeeded");
    let finished = until_posts(&mut p, "job_finished", 1);
    assert_eq!(finished[0]["title"], "Job finished");
    assert_eq!(ops(&mut p).len(), 1, "only the job_finished post");
}

#[test] // spec: APP-146, ARCH-022, CORE-160
fn a_restart_hold_posts_one_toast() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let ids = {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut a = app(&env);
        let paused = a.call("app/queue.pause", json!({}));
        assert_eq!(paused["result"]["status"], "ok", "{paused}");
        let mut p = pipe(&env, json!({}));
        job_ids(&write(
            &mut p,
            "test.job",
            json!({"lane": "gpu-heavy", "batch": 3}),
        ))
    };
    {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut p = pipe(&env, json!({}));
        let held = until_posts(&mut p, "job_failed", 1);
        assert_eq!(held.len(), 1, "{held:?}");
        let toast = &held[0];
        assert_eq!(toast["title"], "Jobs held after a restart", "{toast}");
        // A test core starts with no handlers: the toast has no target and says so
        // (Checkpoint A walkthrough, ODD-6).
        assert_eq!(
            toast["body"], "3 jobs wait until you resume them in Jobs · Open LILITH to see it",
            "{toast}"
        );
        assert_eq!(toast["tag"], format!("restart_{}", ids[0]), "{toast}");
        assert!(toast.get("launch").is_none(), "{toast}");
        assert!(!toast["xml"].as_str().unwrap().contains(&scheme), "{toast}");
        write(&mut p, "job.cancel", json!({"all_mine": true}));
    }
    // A start that held none posts nothing.
    let _core = core_with(&env, FakeClock::new(), None, None);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    fence(&mut p, &env);
    assert!(posts(&mut p, "job_failed").is_empty());
}

/// Owner decision 2026-10-26 (Checkpoint A walkthrough): the restart hold's toast comes once
/// per held batch, across restarts; a later start that holds the same jobs again posts
/// nothing, and a hold of new jobs posts again.
#[test] // spec: APP-146, ARCH-022, CORE-160
fn a_restart_hold_toasts_once_per_held_batch() {
    let env = TestEnv::new();
    // Jobs held in a paused lane when `start` drops its core.
    let held_in = |n: u64| {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut a = app(&env);
        let paused = a.call("app/queue.pause", json!({}));
        assert_eq!(paused["result"]["status"], "ok", "{paused}");
        let mut p = pipe(&env, json!({}));
        job_ids(&write(
            &mut p,
            "test.job",
            json!({"lane": "gpu-heavy", "batch": n}),
        ))
    };
    let first = held_in(2);
    {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut p = pipe(&env, json!({}));
        let toast = until_posts(&mut p, "job_failed", 1).remove(0);
        assert!(
            toast["body"]
                .as_str()
                .is_some_and(|b| b.starts_with("2 jobs wait until you resume them in Jobs")),
            "{toast}"
        );
    }
    // The same two held again: no new toast.
    {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut p = pipe(&env, json!({}));
        let listed = p.tool_ok("jobs.list", json!({}), None);
        let held = listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["pause_reasons"] == json!(["restart"]))
            .count();
        assert_eq!(held, 2, "held again: {listed}");
        desktop(
            &mut p,
            json!({"toasts": "ready", "shortcut": true, "handlers": true,
                   "foreground": "other", "fullscreen": "no"}),
        );
        fence(&mut p, &env);
        assert!(
            posts(&mut p, "job_failed").is_empty(),
            "{:?}",
            posts(&mut p, "job_failed")
        );
    }
    // A new job held with them: one toast for the three.
    let more = held_in(1);
    let _core = core_with(&env, FakeClock::new(), None, None);
    let mut p = pipe(&env, json!({}));
    let toast = until_posts(&mut p, "job_failed", 1).remove(0);
    assert!(
        toast["body"]
            .as_str()
            .is_some_and(|b| b.starts_with("3 jobs wait until you resume them in Jobs")),
        "{toast}"
    );
    assert_eq!(toast["tag"], format!("restart_{}", first[0]), "{toast}");
    assert_ne!(first[0], more[0]);
}

/// Checkpoint A walkthrough (ODD-6): while the handlers aren't this channel's, a toast with
/// a target is posted without it, and its text says to open LILITH, the ending always shown.
#[test] // spec: APP-146, APP-022
fn without_the_handlers_a_toast_has_no_target_and_says_to_open_lilith() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    desktop(&mut p, json!({"handlers": false}));
    batch(&mut p, 1);
    let finished = until_posts(&mut p, "job_finished", 1).remove(0);
    assert!(finished.get("launch").is_none(), "{finished}");
    assert_eq!(
        finished["body"], "test.job · no project · 0 s · Open LILITH to see it",
        "{finished}"
    );
    let failing = write(
        &mut p,
        "test.job",
        json!({"lane": "cpu", "fail": "final", "fail_message": "x".repeat(500)}),
    );
    wait_done(&mut p, &job_ids(&failing)[0]);
    let failed = until_posts(&mut p, "job_failed", 1).remove(0);
    assert!(failed.get("launch").is_none(), "{failed}");
    let body = failed["body"].as_str().unwrap();
    assert!(body.ends_with("x… · Open LILITH to see it"), "{failed}");
    assert!(body.chars().count() <= 200, "{failed}");
    // With the handlers back, the target is back.
    desktop(&mut p, json!({"handlers": true, "clear": true}));
    let id = batch(&mut p, 1).remove(0);
    let scheme = env.env.activation_scheme();
    let back = until_posts(&mut p, "job_finished", 1).remove(0);
    assert_eq!(back["launch"], format!("{scheme}:task/{id}"), "{back}");
    assert_eq!(back["body"], "test.job · no project · 0 s", "{back}");
}

/// Checkpoint A walkthrough (BUG-1): every toast's text goes through the diagnostics
/// redaction before it is posted: a failed job's error message, an agent's notice.
#[test] // spec: APP-146, ARCH-121
fn every_toasts_text_is_redacted() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let key = format!("sk-ant-api03-{}", "FAKEtoastKEY".repeat(3));
    let (secret, mail) = ("hunter2toastsecret", "toast.owner@example.com");
    let message =
        format!("upload failed: api_key={key} Authorization: Bearer {secret} contact {mail}");
    let failing = write(
        &mut p,
        "test.job",
        json!({"lane": "cpu", "fail": "final", "fail_message": message}),
    );
    wait_done(&mut p, &job_ids(&failing)[0]);
    let failed = until_posts(&mut p, "job_failed", 1).remove(0);
    assert_eq!(
        failed["body"],
        "test.job: upload failed: api_key=<redacted> Authorization: <redacted> contact <redacted>",
        "{failed}"
    );
    write(
        &mut p,
        "notify.claude_code",
        json!({"message": format!("done with {key}"), "title": format!("for {mail}")}),
    );
    let notice = until_posts(&mut p, "agent_activity", 1).remove(0);
    assert_eq!(notice["title"], "Claude Code: for <redacted>", "{notice}");
    assert_eq!(notice["body"], "done with <redacted>", "{notice}");
    for toast in [&failed, &notice] {
        let xml = toast["xml"].as_str().unwrap();
        for s in [key.as_str(), secret, mail] {
            assert!(!xml.contains(s), "{s} in {xml}");
        }
    }
}

/// Review I-1: a key that a control character splits is redacted as the toast shows it. The
/// text is normalised first (the control goes, joining the key) and the result redacted, so
/// the join never puts the key back in clear: BEL, NUL, NEL and DEL.
#[test] // spec: APP-146, ARCH-121, APP-022
fn a_key_split_by_a_control_character_is_redacted_as_shown() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let tail = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdef";
    let cases = [
        (format!("sk-ant-api03-ABCD\u{7}{tail}"), "BEL"),
        (format!("sk-ant-\u{0}api03-{tail}"), "NUL"),
        (format!("ghp_\u{85}{}", "A1".repeat(18)), "NEL"),
        (
            format!("hf_{}\u{7f}{}", "d4".repeat(8), "d4".repeat(9)),
            "DEL",
        ),
    ];
    for (i, (split, name)) in cases.iter().enumerate() {
        write(
            &mut p,
            "notify.claude_code",
            json!({"message": format!("{i}: upload failed: {split}")}),
        );
        let joined: String = split.chars().filter(|c| !c.is_control()).collect();
        let toast = until(&format!("the {name} notice"), || {
            posts(&mut p, "agent_activity").into_iter().find(|t| {
                t["body"]
                    .as_str()
                    .is_some_and(|b| b.starts_with(&format!("{i}: ")))
            })
        });
        assert_eq!(
            toast["body"],
            format!("{i}: upload failed: <redacted>"),
            "{name}: {toast}"
        );
        assert!(
            !toast["xml"].as_str().unwrap().contains(&joined[4..]),
            "{name}: {toast}"
        );
    }
    // A failed job's message too.
    let failing = write(
        &mut p,
        "test.job",
        json!({"lane": "cpu", "fail": "final", "fail_message": format!("upload failed: {}", cases[0].0)}),
    );
    wait_done(&mut p, &job_ids(&failing)[0]);
    let failed = until_posts(&mut p, "job_failed", 1).remove(0);
    assert_eq!(
        failed["body"], "test.job: upload failed: <redacted>",
        "{failed}"
    );
}

/// Checkpoint A walkthrough (ODD-5): a call that needs a confirmation and passes a cap names
/// the cap in its toast too.
#[test] // spec: APP-146, APP-022, APP-054
fn a_confirm_and_cap_toast_names_the_cap() {
    let env = TestEnv::new();
    env.suite_toml_set("[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n");
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let id = pending_id(&p.tool("test.destructive", json!({"cost": "2"}), Some(&key())));
    let toast = until_posts(&mut p, "confirmation_waiting", 1).remove(0);
    assert_eq!(toast["tag"], id.as_str(), "{toast}");
    assert_eq!(
        toast["body"], "A script (lil) wants to run test.destructive, over your monthly cap",
        "{toast}"
    );
}

#[test] // spec: APP-146
fn fyi_toasts_coalesce_per_origin() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);

    // Two `cli` batches: the same Tag, each posted once when it ends.
    batch(&mut p, 2);
    batch(&mut p, 2);
    let both = until_posts(&mut p, "job_finished", 2);
    assert_eq!(both.len(), 2, "{both:?}");
    let tag = both[0]["tag"].as_str().unwrap();
    assert!(
        tag.starts_with("o_") && tag.len() == 18 && tag[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "{tag}"
    );
    assert_eq!(both[1]["tag"], tag, "the newer replaces the older");

    // A panel batch keeps its own Tag: its batch id.
    let mut a = app(&env);
    let made = a.tool("test.job", json!({"lane": "cpu", "batch": 2}), Some(&key()));
    assert_eq!(made["status"], "ok", "{made}");
    let batch_id = made["structured"]["jobs"][0]["batch_id"]
        .as_str()
        .unwrap()
        .to_string();
    let toast = until("the panel batch's toast", || {
        posts(&mut p, "job_finished")
            .into_iter()
            .find(|t| t["tag"] == batch_id.as_str())
    });
    assert_eq!(toast["title"], "Batch finished (2 jobs)", "{toast}");
    drop(a);

    // A confirmation's toast is never replaced by another's: each has its own Tag.
    let first = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    let second = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    let asks = until_posts(&mut p, "confirmation_waiting", 2);
    let tags: Vec<&str> = asks.iter().map(|t| t["tag"].as_str().unwrap()).collect();
    assert_eq!(tags, [first.as_str(), second.as_str()]);
}

#[test] // spec: APP-146
fn an_unknown_category_key_is_kept_and_ignored() {
    let env = TestEnv::new();
    env.suite_toml_set("[notifications]\nlater_category = false\n");
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let got = p.tool_ok("desktop.get", json!({}), None);
    assert!(
        got["notifications"].get("later_category").is_none(),
        "{got}"
    );
    let posted = write(&mut p, "notify.claude_code", json!({"message": "still on"}));
    assert_eq!(posted["posted"], true, "{posted}");
    let mut a = app(&env);
    let set = a.call(
        "app/desktop.set",
        json!({"notifications": {"cap_alert": true}}),
    );
    assert_eq!(set["result"]["status"], "ok", "{set}");
    let text = std::fs::read_to_string(env.env.suite_toml()).unwrap();
    assert!(text.contains("later_category = false"), "{text}");
}

#[test] // spec: APP-146, APP-022, CORE-115
fn confirmation_toast_is_removed_when_answered_or_expired() {
    let env = TestEnv::new();
    let clock = FakeClock::new();
    let _core = core_with(&env, clock.clone(), Some(Arc::new(SeqIds::new())), None);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let removed = |p: &mut RawPipe, id: &str| {
        until(&format!("the toast of {id} removed"), || {
            ops(p)
                .iter()
                .any(|t| {
                    t["op"] == "remove" && t["tag"] == id && t["group"] == "confirmation_waiting"
                })
                .then_some(())
        });
    };

    // Answered in the app.
    let answered = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    until_posts(&mut p, "confirmation_waiting", 1);
    let mut a = app(&env);
    let r = a.call(
        "app/confirmation.answer",
        json!({"confirmation_id": answered, "answer": "deny"}),
    );
    assert_eq!(r["result"]["status"], "ok", "{r}");
    removed(&mut p, &answered);
    drop(a);

    // Expired unanswered.
    let expired = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    until_posts(&mut p, "confirmation_waiting", 2);
    clock.advance(Duration::from_secs(31 * 60));
    removed(&mut p, &expired);
}

#[test] // spec: APP-146, APP-054, CORE-114
fn a_cancelled_cap_confirmation_loses_its_toast() {
    let env = TestEnv::new();
    env.suite_toml_set("[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n");
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    // Your own call that would pass a cap asks about the cap; the app isn't in front.
    let mut a = app(&env);
    let asked = a.tool("test.destructive", json!({"cost": "5"}), Some(&key()));
    let id = pending_id(&asked);
    let toast = until_posts(&mut p, "confirmation_waiting", 1).remove(0);
    assert_eq!(toast["tag"], id.as_str(), "{toast}");
    assert_eq!(
        toast["body"], "A call would go over your monthly cap",
        "{toast}"
    );
    // The app closes: its cap confirmation is cancelled, and its toast goes.
    drop(a);
    until(&format!("the toast of {id} removed"), || {
        ops(&mut p)
            .iter()
            .any(|t| t["op"] == "remove" && t["tag"] == id.as_str())
            .then_some(())
    });
}

#[test] // spec: APP-146, APP-022, ARCH-055
fn a_restart_reposts_the_waiting_toasts_and_removes_them_when_answered() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let ids: Arc<dyn IdSource> = Arc::new(lil_platform::RandomIds);
    let (id, old_token) = {
        let _core = core_with(&env, FakeClock::new(), Some(ids.clone()), None);
        let mut p = pipe(&env, json!({}));
        ready(&mut p);
        let id = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
        let token = deny_token(&mut p, &scheme, &id);
        (id, token)
    };
    // The next core posts it again, Tag and all (replacing the old toast and its dead Deny).
    // A test core starts with no handlers, so it comes without its target and its Open and
    // Deny, and says to open LILITH (Checkpoint A walkthrough, ODD-6).
    let _core = core_with(&env, FakeClock::new(), Some(ids), None);
    let mut p = pipe(&env, json!({}));
    let toast = until(&format!("the toast of {id} again"), || {
        posts(&mut p, "confirmation_waiting")
            .into_iter()
            .find(|t| t["tag"] == id.as_str())
    });
    assert!(toast.get("launch").is_none(), "{toast}");
    assert!(
        actions(toast["xml"].as_str().unwrap()).is_empty(),
        "{toast}"
    );
    assert!(!toast["xml"].as_str().unwrap().contains(&scheme), "{toast}");
    assert_eq!(
        toast["body"], "A script (lil) wants to run test.destructive · Open LILITH to see it",
        "{toast}"
    );
    let mut a = app(&env);
    let old = a.call(
        "app/confirmation.answer",
        json!({"confirmation_id": id, "answer": "deny", "toast_token": old_token}),
    );
    assert_eq!(error_code(&old["result"]), "confirm.unknown", "{old}");
    let r = a.call(
        "app/confirmation.answer",
        json!({"confirmation_id": id, "answer": "deny"}),
    );
    assert_eq!(r["result"]["status"], "ok", "the app denies: {r}");
    until(&format!("the toast of {id} removed"), || {
        ops(&mut p)
            .iter()
            .any(|t| t["op"] == "remove" && t["tag"] == id.as_str())
            .then_some(())
    });
}

/// Checkpoint A walkthrough (ODD-6, review): a confirmation's toast reposted at a restart
/// without its target (a test core starts with no handlers) is posted again by the next
/// reconcile once the handlers are this channel's, with Open, Deny and a fresh token: the
/// fresh token denies, and the earlier run's is dead.
#[test] // spec: APP-146, APP-022, ARCH-055
fn a_reposted_toast_gets_its_buttons_and_a_fresh_token_once_the_handlers_are_back() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let ids: Arc<dyn IdSource> = Arc::new(lil_platform::RandomIds);
    let (id, old_token) = {
        let _core = core_with(&env, FakeClock::new(), Some(ids.clone()), None);
        let mut p = pipe(&env, json!({}));
        ready(&mut p);
        let id = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
        let token = deny_token(&mut p, &scheme, &id);
        (id, token)
    };
    let _core = core_with(&env, FakeClock::new(), Some(ids), None);
    let mut p = pipe(&env, json!({}));
    let of_id = |p: &mut RawPipe| -> Vec<Value> {
        posts(p, "confirmation_waiting")
            .into_iter()
            .filter(|t| t["tag"] == id.as_str())
            .collect()
    };
    let bare = until("the bare repost", || of_id(&mut p).into_iter().next());
    assert!(actions(bare["xml"].as_str().unwrap()).is_empty(), "{bare}");
    // The handlers are back; the last app closing reconciles.
    desktop(&mut p, json!({"handlers": true}));
    drop(app(&env));
    let full = until("the repost with its buttons", || {
        of_id(&mut p)
            .into_iter()
            .find(|t| !actions(t["xml"].as_str().unwrap()).is_empty())
    });
    assert_eq!(full["launch"], format!("{scheme}:confirm/{id}"), "{full}");
    assert_eq!(
        full["body"], "A script (lil) wants to run test.destructive",
        "{full}"
    );
    let xml = full["xml"].as_str().unwrap();
    let token = actions(xml)[1]
        .1
        .strip_prefix(&format!("{scheme}:deny/{id}/"))
        .unwrap_or_else(|| panic!("not a deny URL of {id}: {xml}"))
        .to_string();
    assert_ne!(token, old_token);
    let mut a = app(&env);
    let deny = |a: &mut TestApp, token: &str| -> Value {
        a.call(
            "app/confirmation.answer",
            json!({"confirmation_id": id, "answer": "deny", "toast_token": token}),
        )["result"]
            .clone()
    };
    assert_eq!(error_code(&deny(&mut a, &old_token)), "confirm.unknown");
    let denied = deny(&mut a, &token);
    assert_eq!(denied["status"], "ok", "the fresh token denies: {denied}");
    until(&format!("the toast of {id} removed"), || {
        ops(&mut p)
            .iter()
            .any(|t| t["op"] == "remove" && t["tag"] == id.as_str())
            .then_some(())
    });
}

/// Review M-3: a repost of a bare confirmation toast that fails (here Windows' toasts are
/// turned off for the app as the handlers come back) leaves the bare toast where it is. Each
/// reconcile is logged once it is done, so the check waits for it, not for time.
#[test] // spec: APP-146, APP-022, ARCH-055
fn a_failed_repost_keeps_the_bare_toast() {
    let logs = capture_logs();
    let reconciled = || {
        logs.text()
            .lines()
            .filter(|l| l.contains("the confirmation toasts were reconciled"))
            .count()
    };
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let id = {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut p = pipe(&env, json!({}));
        ready(&mut p);
        let id = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
        deny_token(&mut p, &scheme, &id);
        id
    };
    let _core = core_with(&env, FakeClock::new(), None, None);
    let mut p = pipe(&env, json!({}));
    let bare = until("the bare repost", || {
        posts(&mut p, "confirmation_waiting")
            .into_iter()
            .find(|t| t["tag"] == id.as_str())
    });
    assert!(actions(bare["xml"].as_str().unwrap()).is_empty(), "{bare}");
    // Both starts' reconciles are done.
    until("two reconciles", || (reconciled() >= 2).then_some(()));
    let before = reconciled();
    let a = app(&env);
    desktop(
        &mut p,
        json!({"handlers": true, "toasts": "disabled_for_app"}),
    );
    drop(a);
    until("the reconcile after the app closed", || {
        (reconciled() > before).then_some(())
    });
    assert!(
        !ops(&mut p)
            .iter()
            .any(|t| t["op"] == "remove" && t["tag"] == id.as_str()),
        "the bare toast stays: {:?}",
        ops(&mut p)
    );
}

#[test] // spec: APP-146, ARCH-055, APP-022
fn a_toast_held_back_while_the_app_was_in_front_goes_out_when_it_closes() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let a = app(&env);
    desktop(&mut p, json!({"foreground": "suite"}));
    let id = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    // The app shows it; no toast while it is in front.
    std::thread::sleep(Duration::from_millis(300));
    assert!(posts(&mut p, "confirmation_waiting").is_empty());
    // The app closes unanswered: the confirmation still waits, now with its toast.
    drop(a);
    let toast = until_posts(&mut p, "confirmation_waiting", 1).remove(0);
    assert_eq!(toast["tag"], id.as_str(), "{toast}");
}

#[test] // spec: APP-146, APP-022, ARCH-055, QA-016
fn a_bus_overflow_mid_batch_still_ends_in_one_toast() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let core = core(&env);
    let (notifier, events) = (core.core().notifier().clone(), core.core().events().clone());
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let mut a = app(&env);
    let noise = |n: usize| {
        for i in 0..n {
            events.publish("jobs", json!({"kind": "noise", "n": i}));
        }
    };

    // The notifier falls behind: its queue fills with other events first.
    assert!(notifier.pause_job_events(true), "a test channel");
    noise(1500);
    let ids = batch(&mut p, 2);
    // A confirmation waits meanwhile, and one is answered: its own path, never dropped.
    let waiting = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    let answered = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    until_posts(&mut p, "confirmation_waiting", 2);
    let r = a.call(
        "app/confirmation.answer",
        json!({"confirmation_id": answered, "answer": "deny"}),
    );
    assert_eq!(r["result"]["status"], "ok", "{r}");
    // More than the bus holds: the batch's end events are cleared, and a resync is queued.
    noise(1500);
    assert!(notifier.pause_job_events(false), "a test channel");

    let launch = format!("{scheme}:task/{}", ids[0]);
    until("the batch's toast after the resync", || {
        posts(&mut p, "job_finished")
            .iter()
            .any(|t| t["launch"] == launch.as_str())
            .then_some(())
    });
    fence(&mut p, &env);
    let all = posts(&mut p, "job_finished");
    assert_eq!(
        all.iter()
            .filter(|t| t["launch"] == launch.as_str())
            .count(),
        1,
        "once: {all:?}"
    );
    assert!(
        ops(&mut p)
            .iter()
            .any(|t| t["op"] == "remove" && t["tag"] == answered.as_str())
    );
    assert!(
        !ops(&mut p)
            .iter()
            .any(|t| t["op"] == "remove" && t["tag"] == waiting.as_str())
    );
}

#[test] // spec: APP-022, ARCH-055, QA-016
fn posts_that_keep_failing_while_not_yet_shown_make_calls_unavailable() {
    let env = TestEnv::new();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    desktop(
        &mut p,
        json!({"toasts": "not_yet_shown", "post_fails": true}),
    );
    // Not yet shown counts as ready: the call waits, and its toast fails.
    pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    let first = Instant::now();
    // The failed post started the 5 s grace; once it passes, the path is unavailable.
    let refused = until("confirm.unavailable", || {
        std::thread::sleep(Duration::from_millis(200));
        let out = p.tool("test.destructive", json!({}), Some(&key()));
        (out["status"] == "error").then_some(out)
    });
    assert_eq!(error_code(&refused), "confirm.unavailable", "{refused}");
    assert!(
        first.elapsed() >= Duration::from_secs(4),
        "{:?}",
        first.elapsed()
    );
    let got = p.tool_ok("desktop.get", json!({"fresh": true}), None);
    assert_eq!(got["notification_path"]["toasts"], "unavailable", "{got}");
    assert_eq!(
        got["notification_path"]["toasts_reason"], "no_platform",
        "{got}"
    );
    assert!(posts(&mut p, "confirmation_waiting").is_empty());
}

#[test] // spec: QA-056, ARCH-055, CORE-113
fn lil_waits_through_the_toast_with_no_app_open() {
    let env = TestEnv::new();
    // The real core binary: `lil` trusts only a core beside it.
    let _core =
        lil_testkit::CoreFixture::start_with_env(&env, &[("LIL_TEST_DESKTOP_POLL_MS", "50")]);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let k = key();
    let call =
        |env: &TestEnv| env.lil(&["call", "test.destructive", "{}", "--idempotency-key", &k]);
    let out = call(&env);
    assert_eq!(out.code, 4, "pending_confirmation: {out:?}");
    let outcome: Value = serde_json::from_str(&out.stdout).unwrap();
    let id = pending_id(&outcome);
    let toast = until_posts(&mut p, "confirmation_waiting", 1).remove(0);
    assert_eq!(toast["tag"], id.as_str(), "{toast}");
    assert_eq!(
        toast["body"], "A script (lil) wants to run test.destructive",
        "{toast}"
    );
    // An app opened later answers it; the retry runs it.
    let mut a = app(&env);
    let r = a.call(
        "app/confirmation.answer",
        json!({"confirmation_id": id, "answer": "approve"}),
    );
    assert_eq!(r["result"]["status"], "ok", "{r}");
    let out = call(&env);
    assert_eq!(out.code, 0, "{out:?}");
    let ran: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(ran["structured"]["count"], 1, "{ran}");
}

#[test] // spec: APP-022, ARCH-053, CORE-111
fn a_toast_deny_needs_its_token_and_works_once() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let ids: Arc<dyn IdSource> = Arc::new(lil_platform::RandomIds);
    let clock = FakeClock::new();
    let k = key();
    let (survivor, old_token) = {
        let _core = core_with(&env, clock.clone(), Some(ids.clone()), None);
        let mut p = pipe(&env, json!({}));
        ready(&mut p);
        let mut a = app(&env);
        let deny = |a: &mut TestApp, id: &str, token: &str| -> Value {
            a.call(
                "app/confirmation.answer",
                json!({"confirmation_id": id, "answer": "deny", "toast_token": token}),
            )["result"]
                .clone()
        };

        let first = pending_id(&p.tool("test.destructive", json!({}), Some(&k)));
        let token = deny_token(&mut p, &scheme, &first);
        let second = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
        let other = deny_token(&mut p, &scheme, &second);
        assert_ne!(token, other, "each toast its own token");
        // A wrong token, and another confirmation's: unknown, and the confirmation still waits.
        assert_eq!(
            error_code(&deny(&mut a, &first, "0123456789abcdef0123456789abcdef")),
            "confirm.unknown"
        );
        assert_eq!(error_code(&deny(&mut a, &first, &other)), "confirm.unknown");
        // A token only denies, and never with a cap.
        let approve = a.call(
            "app/confirmation.answer",
            json!({"confirmation_id": first, "answer": "approve", "toast_token": token}),
        );
        assert_eq!(
            error_code(&approve["result"]),
            "params.invalid",
            "{approve}"
        );
        let with_cap = a.call(
            "app/confirmation.answer",
            json!({"confirmation_id": first, "answer": "deny", "toast_token": token,
                   "cap": {"action": "allow_once"}}),
        );
        assert_eq!(
            error_code(&with_cap["result"]),
            "params.invalid",
            "{with_cap}"
        );
        // The right one denies once, recorded as the app's answer.
        let denied = deny(&mut a, &first, &token);
        assert_eq!(denied["status"], "ok", "{denied}");
        assert_eq!(denied["confirmation"]["state"], "denied");
        assert_eq!(denied["confirmation"]["confirmed_via"], "app");
        assert_eq!(
            error_code(&p.tool("test.destructive", json!({}), Some(&k))),
            "confirm.denied"
        );
        assert_eq!(error_code(&deny(&mut a, &first, &token)), "confirm.unknown");
        // The window's path keeps its own codes.
        let again = a.call(
            "app/confirmation.answer",
            json!({"confirmation_id": first, "answer": "deny"}),
        );
        assert_eq!(error_code(&again["result"]), "confirm.already_answered");

        // Answered in the window: its toast's token is dead at once.
        let r = a.call(
            "app/confirmation.answer",
            json!({"confirmation_id": second, "answer": "deny"}),
        );
        assert_eq!(r["result"]["status"], "ok");
        assert_eq!(
            error_code(&deny(&mut a, &second, &other)),
            "confirm.unknown"
        );

        // Expired: dead at once.
        let third = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
        let t3 = deny_token(&mut p, &scheme, &third);
        clock.advance(Duration::from_secs(31 * 60));
        assert_eq!(error_code(&deny(&mut a, &third, &t3)), "confirm.unknown");

        // One that survives the restart.
        let fourth = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
        let t4 = deny_token(&mut p, &scheme, &fourth);
        (fourth, t4)
    };
    // After a core restart an old token is unknown; the confirmation still waits.
    let _core = core_with(&env, clock, Some(ids), None);
    let mut a = app(&env);
    let r = a.call(
        "app/confirmation.answer",
        json!({"confirmation_id": survivor, "answer": "deny", "toast_token": old_token}),
    );
    assert_eq!(error_code(&r["result"]), "confirm.unknown", "{r}");
    let listed = a.call("app/confirmations.list", json!({"state": "pending"}));
    assert_eq!(
        listed["result"]["confirmations"][0]["id"],
        survivor.as_str(),
        "{listed}"
    );
}

/// What a test's core logs, all of it.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Captures every log line of this process from now on (nextest runs each test in its own
/// process, so the test owns the global subscriber).
fn capture_logs() -> Captured {
    let logs = Captured::default();
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish(),
    );
    logs
}

#[test] // spec: APP-022, ARCH-117
fn a_toast_token_never_reaches_a_log_line() {
    let logs = capture_logs();
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let _core = core_with(&env, FakeClock::new(), None, None);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let mut a = app(&env);
    let id = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    let token = deny_token(&mut p, &scheme, &id);
    let wrong = "fedcba9876543210fedcba9876543210";
    for t in [wrong, token.as_str()] {
        a.call(
            "app/confirmation.answer",
            json!({"confirmation_id": id, "answer": "deny", "toast_token": t}),
        );
    }
    let text = logs.text();
    assert!(
        !text.contains(&token),
        "a log line holds the token:\n{text}"
    );
    assert!(
        !text.contains(wrong),
        "a log line holds a refused token:\n{text}"
    );
    assert!(
        text.contains(&format!("{scheme}:deny/{id}/…")),
        "the posted Deny URL is logged cut:\n{text}"
    );
}

#[test] // spec: APP-022, QA-031
fn toast_tokens_come_from_the_os_random_source() {
    // In production the core's ids are the OS random source.
    let env = TestEnv::new();
    let base = CoreConfig::for_env(env.env.clone()).unwrap();
    assert_eq!(format!("{:?}", base.ids), "RandomIds");

    // The notifier takes its tokens from the injected source: here the test's counter.
    let scheme = env.env.activation_scheme();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let id = pending_id(&p.tool("test.destructive", json!({}), Some(&key())));
    let token = deny_token(&mut p, &scheme, &id);
    assert_eq!(token.len(), 32, "{token}");
    let (a, b) = token.split_at(16);
    let (a, b): (u64, u64) = (a.parse().unwrap(), b.parse().unwrap());
    assert_eq!(b, a + 1, "two ids of the injected source in a row: {token}");
}

#[test] // spec: APP-146, CORE-089
fn cap_alert_posts_once_per_threshold() {
    let env = TestEnv::new();
    env.suite_toml_set("[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n");
    let _core = core(&env);
    let mut p = pipe(&env, json!({"non_interactive": true}));
    ready(&mut p);
    // 30 %, then 95 % (past 50 % and 80 % at once), then 100 %.
    for amount in ["0.3", "0.65", "0.05"] {
        write(
            &mut p,
            "test.meter_external",
            json!({"provider": "fakeprov", "amount": amount}),
        );
    }
    let alerts = until_posts(&mut p, "cap_alert", 3);
    // Owner decision 2026-10-26 (Checkpoint A walkthrough): one toast per threshold, each
    // with the spend as it was then.
    let text = |t: &Value, k: &str| t[k].as_str().unwrap().to_string();
    let period = text(&alerts[0], "title")
        .rsplit_once('(')
        .and_then(|(_, p)| p.strip_suffix(')'))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{alerts:?}"));
    assert!(
        period.len() == 7 && period.as_bytes()[4] == b'-',
        "{period}"
    );
    let shown: Vec<(String, String)> = alerts
        .iter()
        .map(|t| (text(t, "title"), text(t, "body")))
        .collect();
    let spending = |pct: u32| format!("Spending at {pct} % of your monthly cap ({period})");
    assert_eq!(
        shown,
        [
            (spending(95), "Alert at 50 %".to_string()),
            (spending(95), "Alert at 80 %".to_string()),
            (spending(100), "Alert at 100 %".to_string()),
        ]
    );
    for t in &alerts {
        assert!(t.get("launch").is_none(), "no target: {t}");
    }
    let tags: std::collections::HashSet<&str> =
        alerts.iter().map(|t| t["tag"].as_str().unwrap()).collect();
    assert_eq!(tags.len(), 3, "{alerts:?}");
    fence(&mut p, &env);
    assert_eq!(posts(&mut p, "cap_alert").len(), 3, "once each");
}

#[test] // spec: ARCH-103, APP-146
fn claude_code_duplicates_within_10_s_are_dropped() {
    let env = TestEnv::new();
    let clock = FakeClock::new();
    let _core = core_with(&env, clock.clone(), Some(Arc::new(SeqIds::new())), None);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let say = |p: &mut RawPipe, args: Value| write(p, "notify.claude_code", args);
    assert_eq!(
        say(&mut p, json!({"message": "m"})),
        json!({"posted": true})
    );
    assert_eq!(
        say(&mut p, json!({"message": "m"})),
        json!({"posted": false, "reason": "duplicate"})
    );
    assert_eq!(
        say(&mut p, json!({"message": "other", "title": "Waiting"})),
        json!({"posted": true})
    );
    clock.advance(Duration::from_secs(9));
    assert_eq!(say(&mut p, json!({"message": "m"}))["reason"], "duplicate");
    clock.advance(Duration::from_secs(2));
    assert_eq!(
        say(&mut p, json!({"message": "m"})),
        json!({"posted": true})
    );

    let all = posts(&mut p, "agent_activity");
    let shown: Vec<(&str, &str)> = all
        .iter()
        .map(|t| (t["title"].as_str().unwrap(), t["body"].as_str().unwrap()))
        .collect();
    assert_eq!(
        shown,
        [
            ("Claude Code", "m"),
            ("Claude Code: Waiting", "other"),
            ("Claude Code", "m")
        ]
    );
    assert!(all.iter().all(|t| t.get("launch").is_none()), "{all:?}");
    // Each its own Tag.
    assert_ne!(all[0]["tag"], all[2]["tag"]);

    for bad in [
        json!({"message": ""}),
        json!({"message": "x".repeat(2001)}),
        json!({"message": "x", "title": "t".repeat(101)}),
    ] {
        let refused = p.tool("notify.claude_code", bad, Some(&key()));
        assert_eq!(error_code(&refused), "params.invalid", "{refused}");
    }
}

#[test] // spec: APP-065, ARCH-005
fn agent_open_posts_agent_activity_with_an_open_url() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let viewer = env.root().join("viewer").join("spec-viewer.exe");
    lil_testkit::files::write_file(
        &env.env.data_dir().join(r"apps\viewer.toml"),
        format!(
            "app = \"viewer\"\nexe = '{}'\n[[opens]]\ntypes = [\"image\"]\nformats = [\"png\"]\nintents = [\"open\", \"edit\"]\n",
            viewer.display()
        )
        .as_bytes(),
    );
    let target = env.root().join("picture.png");
    lil_testkit::files::write_file(&target, b"\x89PNG\r\n\x1a\n");
    let target = target.to_string_lossy().into_owned();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);

    let shown = p.tool_ok("ui.show", json!({"target": target}), None);
    assert_eq!(shown["running"], false, "{shown}");
    let toast = until_posts(&mut p, "agent_activity", 1).remove(0);
    // A script (this `cli` pipe) is named as the confirmation toast names it (ruling).
    assert_eq!(toast["title"], "A script (lil)", "{toast}");
    assert_eq!(
        toast["body"], "A script (lil) opened picture.png",
        "{toast}"
    );
    assert_eq!(
        toast["launch"],
        lil_parse::open_url(&scheme, &target),
        "{toast}"
    );
    p.tool_ok(
        "open_in",
        json!({"app": "viewer", "target": target, "intent": "edit"}),
        None,
    );
    until_posts(&mut p, "agent_activity", 2);

    // The longest target, 4,096 bytes of UTF-8, still has its open URL (well within 8,192
    // characters); a longer one is refused before anything is posted (Appendix G "`lil:`
    // URLs", plan 4a clarification 2026-10-25).
    let long = format!(r"C:\{}.png", "\u{1F600}".repeat(1022));
    assert_eq!(long.len(), 4095);
    p.tool_ok("ui.show", json!({"target": long}), None);
    let toast = until_posts(&mut p, "agent_activity", 3).remove(2);
    assert_eq!(
        toast["launch"],
        lil_parse::open_url(&scheme, &long),
        "{toast}"
    );
    let longer = format!(r"C:\{}.png", "\u{1F600}".repeat(1023));
    let refused = p.tool("ui.show", json!({ "target": longer }), None);
    assert_eq!(refused["error"]["code"], "params.invalid", "{refused}");
    fence(&mut p, &env);
    assert_eq!(
        posts(&mut p, "agent_activity").len(),
        3,
        "nothing posted for it"
    );

    // Your own app's request posts nothing.
    let mut a = app(&env);
    let mine = a.tool("ui.show", json!({"target": target}), None);
    assert_eq!(mine["status"], "ok", "{mine}");
    drop(a);
    fence(&mut p, &env);
    assert_eq!(posts(&mut p, "agent_activity").len(), 3);
}

#[test] // spec: APP-146, ARCH-022, CORE-160
fn a_workspace_recovered_later_posts_its_own_restart_toast() {
    let env = TestEnv::new();
    let (first, later) = (env.make_workspace("first"), env.make_workspace("later"));
    env.register_workspace(&first, false);
    env.register_workspace(&later, true);
    let ids = {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut a = app(&env);
        let paused = a.call("app/queue.pause", json!({}));
        assert_eq!(paused["result"]["status"], "ok", "{paused}");
        let mut p = pipe(&env, json!({"workspace": env.workspace_id(&later)}));
        job_ids(&write(
            &mut p,
            "test.job",
            json!({"lane": "gpu-heavy", "batch": 2}),
        ))
    };
    // The next core starts in another workspace: the held jobs' one isn't recovered yet.
    env.register_workspace(&first, true);
    let _core = core_with(&env, FakeClock::new(), None, None);
    let mut p = pipe(&env, json!({}));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        posts(&mut p, "job_failed").is_empty(),
        "the workspace isn't recovered until it is used: {:?}",
        posts(&mut p, "job_failed")
    );
    // Switched to, and first used: that recovery's held jobs get their own toast.
    write(
        &mut p,
        "workspace.set_default",
        json!({"path": later.to_string_lossy()}),
    );
    let mut in_ws = pipe(&env, json!({"workspace": env.workspace_id(&later)}));
    in_ws.tool_ok("jobs.list", json!({}), None);
    let toast = until_posts(&mut p, "job_failed", 1).remove(0);
    assert_eq!(toast["title"], "Jobs held after a restart", "{toast}");
    assert_eq!(
        // No handlers on a test core until a test sets them (ODD-6).
        toast["body"],
        "2 jobs wait until you resume them in Jobs · Open LILITH to see it",
        "{toast}"
    );
    assert_eq!(toast["tag"], format!("restart_{}", ids[0]), "{toast}");
}

#[test] // spec: APP-146, ARCH-055
fn a_resync_looks_back_from_the_events_own_time() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let clock = FakeClock::new();
    let core = core_with(&env, clock.clone(), Some(Arc::new(SeqIds::new())), None);
    let (notifier, events) = (core.core().notifier().clone(), core.core().events().clone());
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    // One real job event handled first.
    fence(&mut p, &env);
    // The notifier stalls; the events it will handle late carry the time they happened.
    assert!(notifier.pause_job_events(true), "a test channel");
    let happened = lil_schema::time::rfc3339(
        i64::try_from(
            lil_platform::Clock::utc_now(clock.as_ref())
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap(),
    );
    let stale = |n: usize| {
        for i in 0..n {
            events.publish(
                "jobs",
                json!({"kind": "job", "job": {"job_id": format!("j_stale{i}"), "batch_id": "b_stale",
                       "status": "queued", "updated": happened}}),
            );
        }
    };
    stale(1100);
    let ids = batch(&mut p, 2);
    stale(1500);
    // Handled more than 5 s after the batch ended.
    clock.advance(Duration::from_secs(60));
    assert!(notifier.pause_job_events(false), "a test channel");
    let launch = format!("{scheme}:task/{}", ids[0]);
    until("the batch's toast after the late resync", || {
        posts(&mut p, "job_finished")
            .iter()
            .any(|t| t["launch"] == launch.as_str())
            .then_some(())
    });
}

#[test] // spec: APP-146, ARCH-055, APP-022
fn a_carried_over_confirmation_of_a_later_workspace_gets_its_toast_when_its_retry_comes() {
    let env = TestEnv::new();
    let (first, later) = (env.make_workspace("first"), env.make_workspace("later"));
    env.register_workspace(&first, false);
    env.register_workspace(&later, true);
    let k = key();
    let id = {
        let _core = core_with(&env, FakeClock::new(), None, None);
        let mut p = pipe(&env, json!({"workspace": env.workspace_id(&later)}));
        ready(&mut p);
        pending_id(&p.tool("test.destructive", json!({}), Some(&k)))
    };
    env.register_workspace(&first, true);
    let _core = core_with(&env, FakeClock::new(), None, None);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    write(
        &mut p,
        "workspace.set_default",
        json!({"path": later.to_string_lossy()}),
    );
    // The agent's retry opens the workspace: the confirmation still waits, with its toast.
    let mut agent = pipe(&env, json!({"workspace": env.workspace_id(&later)}));
    assert_eq!(
        pending_id(&agent.tool("test.destructive", json!({}), Some(&k))),
        id
    );
    let toast = until_posts(&mut p, "confirmation_waiting", 1).remove(0);
    assert_eq!(toast["tag"], id.as_str(), "{toast}");
}

/// m5: a settled batch whose job is retried ends again, and toasts again (the retry is a new
/// job id, so the settled batch's memory doesn't hold it back).
#[test] // spec: APP-146
fn a_retried_job_of_a_settled_batch_toasts_again() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();
    let _core = core(&env);
    let mut p = pipe(&env, json!({}));
    ready(&mut p);
    let failed = job_ids(&write(
        &mut p,
        "test.job",
        json!({"lane": "cpu", "fail": "final"}),
    ))
    .remove(0);
    assert_eq!(wait_done(&mut p, &failed)["status"], "failed");
    until_posts(&mut p, "job_failed", 1);
    let retried = write(&mut p, "job.retry", json!({"job_id": failed}));
    let again = retried["job"]["job_id"].as_str().unwrap().to_string();
    assert_ne!(again, failed);
    assert_eq!(wait_done(&mut p, &again)["status"], "failed");
    let toasts = until_posts(&mut p, "job_failed", 2);
    assert_eq!(
        toasts[1]["launch"],
        format!("{scheme}:task/{again}"),
        "{toasts:?}"
    );
}
