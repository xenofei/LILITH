//! Plan 4b's `jobs.list` filters: `status` as a list or `"finished"`, `origin` as a surface or a
//! list, and `dispatched` (spec APP-114, CORE-160; Appendix H "Jobs"). Driven through `test.job`
//! and the job tools from a `cli` connection and the `ui:test` app (QA-018).

mod support;

use lil_testkit::{CoreFixture, TestApp, TestEnv};
use serde_json::{Value, json};
use support::*;

/// The sorted job ids `jobs.list` returns for `args`.
fn listed(c: &mut impl Caller, args: Value) -> Vec<String> {
    let mut all = args.clone();
    all["limit"] = json!(500);
    let list = ok(c, "jobs.list", all, None);
    let mut ids: Vec<String> = list["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("jobs.list {args} has no jobs list: {list}"))
        .iter()
        .map(|j| j["job_id"].as_str().unwrap_or_default().to_string())
        .collect();
    ids.sort();
    ids
}

/// `ids`, sorted.
fn sorted(ids: &[&String]) -> Vec<String> {
    let mut out: Vec<String> = ids.iter().map(|s| (*s).clone()).collect();
    out.sort();
    out
}

#[test] // spec: APP-114, CORE-160
fn jobs_list_filters_by_status_list_origin_and_dispatch() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let mut app = TestApp::start(&env, None, &[]);

    // Finished from cli: one succeeded, one cancelled.
    let done = submit(&mut pipe, "test.job", json!({"lane": "cpu"}));
    assert_eq!(wait_done(&mut pipe, &done)["status"], "succeeded");
    let cancelled = submit(&mut pipe, "test.job", json!({"lane": "cpu", "gated": true}));
    write_ok(&mut pipe, "job.cancel", json!({ "job_id": cancelled }));
    assert_eq!(wait_done(&mut pipe, &cancelled)["status"], "cancelled");
    // Finished from the app.
    let theirs = submit(&mut app, "test.job", json!({"lane": "cpu"}));
    assert_eq!(wait_done(&mut app, &theirs)["status"], "succeeded");
    let surface = job(&mut pipe, &theirs)["origin"]["surface"]
        .as_str()
        .unwrap_or_else(|| panic!("the app's job has no origin surface"))
        .to_string();
    assert_ne!(surface, "cli");
    // Running, and queued behind it.
    let running = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "gated": true}),
    );
    until_job(&mut pipe, &running, "running", |j| j["status"] == "running");
    let queued = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "gated": true}),
    );
    until_job(&mut pipe, &queued, "queued", |j| j["status"] == "queued");

    // A single status works as before.
    assert_eq!(
        listed(&mut pipe, json!({"status": "running"})),
        sorted(&[&running])
    );
    assert_eq!(
        listed(&mut pipe, json!({"status": ["running", "queued"]})),
        sorted(&[&running, &queued])
    );
    assert_eq!(
        listed(&mut pipe, json!({"status": "finished"})),
        sorted(&[&done, &cancelled, &theirs])
    );
    assert_eq!(
        listed(&mut pipe, json!({"status": "finished", "origin": "cli"})),
        sorted(&[&done, &cancelled])
    );
    assert_eq!(
        listed(&mut pipe, json!({ "origin": [surface] })),
        sorted(&[&theirs])
    );
    assert_eq!(
        listed(&mut pipe, json!({"origin": ["cli", surface]})),
        sorted(&[&done, &cancelled, &theirs, &running, &queued])
    );
    assert_eq!(
        listed(&mut pipe, json!({"dispatched": true})),
        sorted(&[&running])
    );
    assert_eq!(
        listed(
            &mut pipe,
            json!({"dispatched": false, "status": ["queued", "running"]})
        ),
        sorted(&[&queued])
    );

    write_ok(&mut pipe, "job.cancel", json!({ "job_id": queued }));
    release(&mut pipe, &running);
    wait_done(&mut pipe, &running);
}
