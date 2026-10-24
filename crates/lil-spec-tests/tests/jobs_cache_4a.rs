//! A ComfyUI run served from its cache (spec CORE-117; Appendix G "Jobs": `cache_hit_nodes` are
//! the node ids of ComfyUI's `execution_cached` event, and when every node of the prompt other
//! than its output nodes was cached the job succeeds with `all_cached: true` and the summary
//! text "Nothing changed: ComfyUI served every node from its cache."; plan 4a ruling,
//! 2026-10-24: an output node, such as a Save node, always runs again, since the suite gives it
//! its own `filename_prefix`). Driven through `test.comfy_submit`, `job.wait` and the fake
//! ComfyUI, whose `cached_nodes` makes it report nodes as cached and runs the Save node left
//! out again, as ComfyUI does (QA-018).
//!
//! A spec test added by Task 6 under plan 4a's one exception for spec tests: it needs the
//! fake's `cached_nodes`, which Task 6 adds.

mod support;

use std::time::Instant;

use lil_testkit::{CoreFixture, FakeComfy, TestEnv};
use serde_json::{Value, json};
use support::*;

/// Appendix G's summary text of a run that changed nothing.
const NOTHING_CHANGED: &str = "Nothing changed: ComfyUI served every node from its cache.";

/// An `EmptyImage` node `1` and a `SaveImage` node `2` that saves it.
fn graph() -> Value {
    json!({
        "1": {"class_type": "EmptyImage",
              "inputs": {"width": 64, "height": 64, "batch_size": 1, "color": 0}},
        "2": {"class_type": "SaveImage",
              "inputs": {"images": ["1", 0], "filename_prefix": "ComfyUI"}},
    })
}

/// `job.wait` until the job is done: the whole outcome (its `summary_text` with it).
fn waited(c: &mut impl Caller, id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let outcome = c.tool("job.wait", json!({"job_id": id, "timeout_s": 30}), None);
        assert_eq!(outcome["status"], "ok", "{outcome}");
        if outcome["structured"]["done"] == true {
            return outcome;
        }
        assert!(Instant::now() < deadline, "job {id} is not done: {outcome}");
    }
}

/// The sorted strings of a JSON array.
fn sorted(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

#[test] // spec: CORE-117
fn all_cached_run_says_nothing_changed() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    until_engine(&mut pipe, "attached", |e| e["class"] == "attached");

    // Every node but the output node is served from the cache; the Save node runs again.
    fake.cached_nodes(&["1"]);
    let id = submit(&mut pipe, "test.comfy_submit", json!({ "graph": graph() }));
    let outcome = waited(&mut pipe, &id);
    let job = &outcome["structured"]["job"];
    assert_eq!(job["status"], "succeeded", "{outcome}");
    assert_eq!(job["all_cached"], true, "{outcome}");
    assert_eq!(sorted(&job["cache_hit_nodes"]), ["1"], "{outcome}");
    assert_eq!(outcome["summary_text"], NOTHING_CHANGED, "{outcome}");

    // The image node executes: the run changed something, so it is not all cached.
    fake.cached_nodes(&[]);
    let id = submit(&mut pipe, "test.comfy_submit", json!({ "graph": graph() }));
    let outcome = waited(&mut pipe, &id);
    let job = &outcome["structured"]["job"];
    assert_eq!(job["status"], "succeeded", "{outcome}");
    assert_ne!(job["all_cached"], true, "{outcome}");
    assert_ne!(outcome["summary_text"], NOTHING_CHANGED, "{outcome}");
}
