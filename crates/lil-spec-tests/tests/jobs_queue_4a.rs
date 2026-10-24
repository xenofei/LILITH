//! Plan 4a's queue controls and job records: class order and `job.move`, batch cancels, Pause GPU
//! work with its reasons and the safe withdrawal of a pending prompt, the restart hold and
//! `app/queue.resume {held_only}`, the new `JobSummary` fields with the typed `origin`, and the
//! stall flag (spec CORE-117, CORE-120, CORE-160, ARCH-022, ARCH-072, APP-040; Appendix G
//! "Jobs", rulings P4a-R5, P4a-R12, P4a-R13 and P4a-R41). Driven through `test.job`,
//! `test.comfy_submit`, the job tools, `app/queue.*` from the `ui:test` app and the fake ComfyUI
//! (QA-018).
//!
//! Not here: the all-cached run (CORE-117's `all_cached`), which needs a fake ComfyUI that
//! reports cached nodes. Task 6 adds that fake (`FakeComfy::cached_nodes`) together with the
//! spec test `all_cached_run_says_nothing_changed` in a spec-test file of its own.

mod p4a;
mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use lil_testkit::{CoreFixture, FakeComfy, TestApp, TestEnv};
use p4a::*;
use serde_json::json;
use support::*;

/// A gated `gpu-heavy` job, running: everything submitted after it waits.
fn running_gate(c: &mut impl Caller) -> String {
    let id = submit(c, "test.job", json!({"lane": "gpu-heavy", "gated": true}));
    until_job(c, &id, "running", running);
    id
}

/// A queued `gpu-heavy` job (behind the gate), checked to be waiting.
fn queued(c: &mut impl Caller) -> String {
    let id = submit(c, "test.job", json!({"lane": "gpu-heavy", "gated": true}));
    let waiting = until_job(c, &id, "queued", |j| j["status"] == "queued");
    assert_eq!(waiting["status"], "queued", "{waiting}");
    id
}

/// Releases `ran` and checks that `next` is the job that runs next.
fn release_then_runs(c: &mut impl Caller, ran: &str, next: &str) {
    release(c, ran);
    assert_eq!(wait_done(c, ran)["status"], "succeeded");
    until_job(c, next, "running next", running);
}

#[test] // spec: CORE-160
fn job_move_next_and_last_stay_within_the_class() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let mut app = TestApp::start(&env, None, &[]);

    let gate = running_gate(&mut pipe);
    let a1 = queued(&mut pipe);
    let a2 = queued(&mut pipe);
    let a3 = queued(&mut pipe);
    let user = queued(&mut app);
    assert_eq!(job(&mut pipe, &a3)["priority"], "agent");
    assert_eq!(job(&mut pipe, &user)["priority"], "user");

    let moved = write_ok(&mut pipe, "job.move", json!({"job_id": a3, "to": "next"}));
    assert_eq!(moved["job"]["job_id"], a3.as_str(), "{moved}");
    assert_eq!(
        moved["job"]["priority"], "agent",
        "a move never changes the class: {moved}"
    );
    let moved = write_ok(&mut pipe, "job.move", json!({"job_id": a1, "to": "last"}));
    assert_eq!(moved["job"]["job_id"], a1.as_str(), "{moved}");

    // The user job still goes first; then the agent class in its new order: a3, a2, a1.
    release_then_runs(&mut pipe, &gate, &user);
    for waiting in [&a1, &a2, &a3] {
        assert_eq!(job(&mut pipe, waiting)["status"], "queued");
    }
    release_then_runs(&mut pipe, &user, &a3);
    release_then_runs(&mut pipe, &a3, &a2);
    assert_eq!(job(&mut pipe, &a1)["status"], "queued");
    release_then_runs(&mut pipe, &a2, &a1);
    release(&mut pipe, &a1);
    assert_eq!(wait_done(&mut pipe, &a1)["status"], "succeeded");
}

#[test] // spec: CORE-160
fn job_move_of_a_running_job_is_not_queued() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let gate = running_gate(&mut pipe);
    let waiting = queued(&mut pipe);
    write_ok(
        &mut pipe,
        "job.move",
        json!({"job_id": waiting, "to": "next"}),
    );

    let refused = pipe.tool(
        "job.move",
        json!({"job_id": gate, "to": "next"}),
        Some(&key()),
    );
    assert_eq!(error_code(&refused), "job.not_queued", "{refused}");
    assert_eq!(job(&mut pipe, &gate)["status"], "running");

    release(&mut pipe, &gate);
    wait_done(&mut pipe, &gate);
    let finished = pipe.tool(
        "job.move",
        json!({"job_id": gate, "to": "last"}),
        Some(&key()),
    );
    assert_eq!(error_code(&finished), "job.not_queued", "{finished}");
    release(&mut pipe, &waiting);
}

#[test] // spec: CORE-160
fn job_cancel_by_ids_batch_and_all_mine() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let mut app = TestApp::start(&env, None, &[]);
    let gated = json!({"lane": "cpu", "gated": true});

    // {ids}: exactly those.
    let ids: Vec<String> = (0..3)
        .map(|_| submit(&mut pipe, "test.job", gated.clone()))
        .collect();
    let cancelled = write_ok(&mut pipe, "job.cancel", json!({"ids": [ids[0], ids[1]]}));
    let mut named = job_ids_of(&cancelled);
    named.sort();
    let mut wanted = vec![ids[0].clone(), ids[1].clone()];
    wanted.sort();
    assert_eq!(named, wanted, "{cancelled}");
    for id in &ids[..2] {
        assert_eq!(wait_done(&mut pipe, id)["status"], "cancelled");
    }
    assert_ne!(job(&mut pipe, &ids[2])["status"], "cancelled");

    // {batch_id}: the batch of `test.job {batch: 3}`.
    let batch = write_ok(
        &mut pipe,
        "test.job",
        json!({"lane": "cpu", "gated": true, "batch": 3}),
    );
    let members = job_ids_of(&batch);
    assert_eq!(members.len(), 3, "{batch}");
    let batch_id = batch["jobs"][0]["batch_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no batch_id: {batch}"))
        .to_string();
    for j in batch["jobs"].as_array().into_iter().flatten() {
        assert_eq!(j["batch_id"], batch_id.as_str(), "one batch: {batch}");
    }
    let cancelled = write_ok(&mut pipe, "job.cancel", json!({ "batch_id": batch_id }));
    assert_eq!(job_ids_of(&cancelled).len(), 3, "{cancelled}");
    for id in &members {
        assert_eq!(wait_done(&mut pipe, id)["status"], "cancelled");
    }

    // {all_mine: true} from `cli` leaves the `ui:test` app's job alone.
    let theirs = submit(&mut app, "test.job", gated.clone());
    let mine = submit(&mut pipe, "test.job", gated.clone());
    write_ok(&mut pipe, "job.cancel", json!({"all_mine": true}));
    for id in [&ids[2], &mine] {
        assert_eq!(wait_done(&mut pipe, id)["status"], "cancelled");
    }
    let kept = job(&mut pipe, &theirs);
    assert!(
        kept["status"] == "queued" || kept["status"] == "running",
        "another caller's job is not mine: {kept}"
    );

    // Exactly one selector.
    for args in [
        json!({}),
        json!({"job_id": theirs, "ids": [theirs]}),
        json!({"batch_id": batch_id, "all_mine": true}),
    ] {
        let refused = pipe.tool("job.cancel", args.clone(), Some(&key()));
        assert_eq!(error_code(&refused), "params.invalid", "{args}: {refused}");
    }
    release(&mut app, &theirs);
}

#[test] // spec: CORE-117, CORE-160
fn queue_pause_holds_every_queued_job_and_never_interrupts_the_running_prompt() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let mut app = app_4a(&env, None, &[]);

    let gate = running_gate(&mut pipe);
    let agent = queued(&mut pipe);
    let user = queued(&mut app);

    let state = app_ok(&mut app, "app/queue.pause", json!({}));
    assert_eq!(state["lane"], "gpu-heavy", "{state}");
    assert_eq!(state["paused"], true, "{state}");
    assert_eq!(state["reasons"], json!(["manual"]), "{state}");

    assert_eq!(
        job(&mut pipe, &gate)["status"],
        "running",
        "the running job is never interrupted"
    );
    for id in [&agent, &user] {
        let held = until_job(&mut pipe, id, "queue_paused", |j| {
            j["substatus"] == "queue_paused"
        });
        assert_eq!(held["status"], "queued", "{held}");
        assert_eq!(held["pause_reasons"], json!(["manual"]), "{held}");
    }
    // The counts are read once the jobs show the pause: the call need not wait for it.
    let lane = poll("the lane counting its 2 held jobs", || {
        let lane = lane_state(&mut pipe, "gpu-heavy");
        (lane["held"] == 2).then_some(lane)
    });
    assert_eq!(lane["paused"], true, "{lane}");
    assert_eq!(lane["reasons"], json!(["manual"]), "{lane}");
    assert_eq!(lane["restart_held"], 0, "{lane}");

    // A paused lane dispatches nothing, even once the running job ends.
    release(&mut pipe, &gate);
    assert_eq!(wait_done(&mut pipe, &gate)["status"], "succeeded");
    std::thread::sleep(Duration::from_millis(500));
    for id in [&agent, &user] {
        assert_eq!(job(&mut pipe, id)["substatus"], "queue_paused");
    }

    let resumed = app_ok(&mut app, "app/queue.resume", json!({}));
    assert_eq!(resumed["paused"], false, "{resumed}");
    assert_eq!(resumed["reasons"], json!([]), "{resumed}");
    // In order: the user class first.
    until_job(&mut pipe, &user, "running", running);
    assert_eq!(job(&mut pipe, &agent)["status"], "queued");
    release_then_runs(&mut pipe, &user, &agent);
    release(&mut pipe, &agent);
    assert_eq!(wait_done(&mut pipe, &agent)["status"], "succeeded");
}

#[test] // spec: CORE-160, CORE-120
fn queue_pause_withdraws_a_pending_prompt_and_reposts_it_once() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let mut app = app_4a(&env, None, &[]);
    until_engine(&mut pipe, "attached", |e| e["class"] == "attached");

    // When the suite's prompt arrives, an outside prompt starts first, so the suite's waits in
    // ComfyUI's queue behind it (`queued_in_engine`).
    let (arrived_tx, arrived_rx) = mpsc::sync_channel::<()>(1);
    let (go_tx, go_rx) = mpsc::sync_channel::<()>(1);
    let (arrived_tx, go_rx) = (Mutex::new(arrived_tx), Mutex::new(go_rx));
    let first = AtomicBool::new(true);
    fake.on_prompt(move |_| {
        if first.swap(false, Ordering::SeqCst) {
            let _ = arrived_tx.lock().map(|tx| tx.send(()));
            let _ = go_rx
                .lock()
                .map(|rx| rx.recv_timeout(Duration::from_secs(10)));
        }
    });
    let id = submit(&mut pipe, "test.comfy_submit", json!({"graph": "tiny"}));
    arrived_rx
        .recv_timeout(DEADLINE)
        .expect("the suite's prompt never reached ComfyUI");
    let outside = fake.add_outside_prompt(true, None);
    go_tx.send(()).expect("the fake's hook is gone");
    let pending = until_job(&mut pipe, &id, "queued_in_engine", |j| {
        j["substatus"] == "queued_in_engine"
    });
    assert_eq!(pending["status"], "queued", "{pending}");
    let old = posted_prompts(&fake)[0]["prompt_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let state = app_ok(&mut app, "app/queue.pause", json!({}));
    assert_eq!(state["paused"], true, "{state}");
    let held = until_job(&mut pipe, &id, "queue_paused", |j| {
        j["substatus"] == "queue_paused"
    });
    assert_eq!(held["status"], "queued", "{held}");
    let lane = poll("the lane counting one withdrawn prompt", || {
        let lane = lane_state(&mut pipe, "gpu-heavy");
        (lane["withdrawn"] == 1).then_some(lane)
    });
    assert_eq!(lane["held"], 1, "{lane}");
    let cancels: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path.ends_with("/cancel"))
        .collect();
    assert_eq!(cancels.len(), 1, "one cancel: {cancels:?}");
    assert_eq!(cancels[0].path, format!("/api/jobs/{old}/cancel"));
    assert!(
        !fake
            .requests()
            .iter()
            .any(|r| r.path.ends_with("/interrupt")),
        "the outside prompt was interrupted"
    );

    app_ok(&mut app, "app/queue.resume", json!({}));
    fake.release(&outside);
    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");
    let bodies = posted_prompts(&fake);
    assert_eq!(bodies.len(), 2, "exactly one new POST /prompt: {bodies:?}");
    let new = bodies[1]["prompt_id"].as_str().unwrap_or_default();
    assert!(is_canonical_uuid(new), "{new:?}");
    assert_ne!(new, old, "the repost carries a new prompt id");
}

#[test] // spec: APP-040, CORE-160
fn queue_pause_is_only_for_verified_apps() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    assert_eq!(
        app_error(&mut pipe, "app/queue.pause", json!({})),
        "caller.not_allowed"
    );
    let mut old_app = TestApp::start(&env, None, &[]);
    assert_eq!(
        app_error(&mut old_app, "app/queue.pause", json!({})),
        "caller.not_allowed",
        "an app without queue.v1"
    );
    let mut app = app_4a(&env, None, &[]);
    let state = app_ok(&mut app, "app/queue.pause", json!({"lane": "cpu"}));
    assert_eq!(state["lane"], "cpu", "{state}");
    assert_eq!(state["paused"], true, "{state}");
    let state = app_ok(&mut app, "app/queue.resume", json!({"lane": "cpu"}));
    assert_eq!(state["paused"], false, "{state}");
}

#[test] // spec: ARCH-022, CORE-160
fn a_restart_holds_never_started_jobs_until_resumed() {
    let env = TestEnv::new();
    let core = start_abortable(&env, &[]);
    let (gate, held) = {
        let mut pipe = session(&env, None);
        let gate = running_gate(&mut pipe);
        let held: Vec<String> = (0..2)
            .map(|_| {
                let id = submit(&mut pipe, "test.job", json!({"lane": "gpu-heavy"}));
                until_job(&mut pipe, &id, "waiting", |j| j["substatus"] == "waiting");
                id
            })
            .collect();
        (gate, held)
    };
    // Killed, not drained.
    drop(core);

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let interrupted = wait_done(&mut pipe, &gate);
    assert_eq!(interrupted["substatus"], "interrupted", "{interrupted}");
    for id in &held {
        let j = job(&mut pipe, id);
        assert_eq!(j["status"], "queued", "{j}");
        assert_eq!(j["substatus"], "queue_paused", "{j}");
        assert_eq!(j["pause_reasons"], json!(["restart"]), "{j}");
    }
    let lane = lane_state(&mut pipe, "gpu-heavy");
    assert_eq!(
        lane["paused"], false,
        "a restart never pauses the lane: {lane}"
    );
    assert_eq!(lane["restart_held"], 2, "{lane}");
    assert_eq!(lane["held"], 2, "{lane}");

    // New jobs run.
    let fresh = submit(&mut pipe, "test.job", json!({"lane": "gpu-heavy"}));
    assert_eq!(wait_done(&mut pipe, &fresh)["status"], "succeeded");

    // "Resume held jobs" keeps your own pause.
    let mut app = app_4a(&env, None, &[]);
    app_ok(&mut app, "app/queue.pause", json!({}));
    let state = app_ok(&mut app, "app/queue.resume", json!({"held_only": true}));
    assert_eq!(state["paused"], true, "{state}");
    assert_eq!(state["reasons"], json!(["manual"]), "{state}");
    assert_eq!(state["restart_held"], 0, "{state}");
    for id in &held {
        let j = until_job(&mut pipe, id, "held by the manual pause only", |j| {
            j["pause_reasons"] == json!(["manual"])
        });
        assert_eq!(j["substatus"], "queue_paused", "{j}");
    }

    let state = app_ok(&mut app, "app/queue.resume", json!({}));
    assert_eq!(state["paused"], false, "{state}");
    for id in &held {
        assert_eq!(wait_done(&mut pipe, id)["status"], "succeeded");
    }
    let jobs = all_jobs(&mut pipe);
    assert_eq!(jobs.len(), 4, "nothing re-ran as a new job: {jobs:?}");
}

#[test] // spec: CORE-117
fn job_records_carry_batch_origin_phase_and_status_seq() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let id = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "cpu", "phases": ["loading_models", "sampling"]}),
    );
    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");
    let batch_id = done["batch_id"].as_str().unwrap_or_default();
    assert!(
        batch_id.starts_with("b_") && is_hex(&batch_id[2..], 16),
        "batch_id is b_<16 hex>: {done}"
    );
    assert_eq!(done["index"], 0, "{done}");
    assert_eq!(done["origin"], json!({"surface": "cli"}), "{done}");
    assert_eq!(done["phase"], "sampling", "{done}");
    let names: Vec<&str> = done["subtasks"]
        .as_array()
        .unwrap_or_else(|| panic!("no subtasks: {done}"))
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(names, ["loading_models", "sampling"], "{done}");

    let seq = done["status_seq"]
        .as_array()
        .unwrap_or_else(|| panic!("no status_seq: {done}"));
    assert_eq!(
        seq.first().map(|s| &s["status"]),
        Some(&json!("queued")),
        "{done}"
    );
    assert_eq!(
        seq.last().map(|s| &s["status"]),
        Some(&json!("succeeded")),
        "{done}"
    );
    for entry in seq {
        assert!(entry["at"].is_string(), "{entry}");
    }

    let listed = ok(
        &mut pipe,
        "jobs.list",
        json!({ "batch_id": batch_id }),
        None,
    );
    assert_eq!(job_ids_of(&listed), vec![id.clone()], "{listed}");
}

#[test] // spec: ARCH-072
fn a_silent_job_is_unresponsive_and_nothing_is_cancelled() {
    let env = TestEnv::new();
    let _core = CoreFixture::start_with_env(&env, &[("LIL_TEST_STALL_SECS", "2")]);
    let mut pipe = session(&env, None);

    // Silent for longer than the stall time plus one 5 s check (P4a-R33).
    let id = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "cpu", "silent_secs": 12}),
    );
    let topic = format!("job:{id}");
    let mut events = session(&env, None);
    subscribe_live(&mut events, &[&topic]);
    let flagged = until_job(&mut pipe, &id, "unresponsive", |j| {
        j["unresponsive"] == true
    });
    assert_eq!(flagged["status"], "running", "{flagged}");
    let flipped = wait_for_kind(&mut events, &topic, "unresponsive");
    assert_eq!(flipped["on"], true, "{flipped}");

    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "nothing is cancelled: {done}");
}
