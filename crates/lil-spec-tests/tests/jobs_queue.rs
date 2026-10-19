//! Jobs and the suite-held queue: job tools return a job id at once, `job.wait` is bounded,
//! idempotent submits, the jobs log, recovery after a killed core, the idle blocker, one heavy
//! GPU job at a time, priority classes and FIFO, retries, and the VRAM fit check (spec
//! ARCH-020, ARCH-022, ARCH-034, ARCH-035, ARCH-071, CORE-009, CORE-016, CORE-117, CORE-121,
//! CORE-124, CORE-126, CORE-128, AI-113; Appendix F "Jobs", "The suite-held queue", "Retries and
//! recovery", "Jobs log lines"). Driven through the `test.job` test-channel tool and the job
//! tools over `RawPipe`, the fake hardware and the files on disk (QA-018).

mod support;

use std::time::Duration;

use lil_testkit::{CoreFixture, FakeComfy, FakeHwScript, TestEnv};
use serde_json::{Value, json};
use support::*;

/// A fake hardware script written into the test's folder; returns the variable pair that hands
/// it to a core (`LIL_TEST_FAKE_HW`).
fn fake_hw(env: &TestEnv, script: Value) -> (String, String) {
    let path = env.root().join("fake-hw.json");
    FakeHwScript::new(script).write(&path);
    ("LIL_TEST_FAKE_HW".to_string(), path_str(&path))
}

#[test] // spec: ARCH-034
fn job_tool_returns_a_job_id_at_once() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let submitted = write_ok(&mut pipe, "test.job", json!({"lane": "cpu", "gated": true}));
    let id = job_id_of(&submitted);
    assert!(is_job_id(&id), "job ids are j_<16 hex>: {submitted}");
    let status = &submitted["job"]["status"];
    assert!(
        status == "queued" || status == "running",
        "a gated job is not finished when its tool returns: {submitted}"
    );

    let short = ok(
        &mut pipe,
        "job.wait",
        json!({"job_id": id, "timeout_s": 1}),
        None,
    );
    assert_eq!(short["done"], false, "{short}");
    assert_eq!(short["job"]["job_id"], id.as_str(), "{short}");

    release(&mut pipe, &id);
    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");
}

#[test] // spec: AI-113
fn job_wait_is_at_most_90_seconds() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let id = submit(&mut pipe, "test.job", json!({"lane": "cpu"}));

    let too_long = pipe.tool("job.wait", json!({"job_id": id, "timeout_s": 91}), None);
    assert_eq!(error_code(&too_long), "params.invalid", "{too_long}");

    let longest = ok(
        &mut pipe,
        "job.wait",
        json!({"job_id": id, "timeout_s": 90}),
        None,
    );
    assert_eq!(longest["done"], true, "{longest}");
    assert_eq!(longest["job"]["status"], "succeeded", "{longest}");
}

#[test] // spec: CORE-016, ARCH-035
fn same_key_returns_the_same_job() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let k = key();
    let args = json!({"lane": "cpu", "gated": true});

    let first = job_id_of(&ok(&mut pipe, "test.job", args.clone(), Some(&k)));
    let second = job_id_of(&ok(&mut pipe, "test.job", args, Some(&k)));
    assert_eq!(first, second, "one key, one job");
    let jobs = all_jobs(&mut pipe);
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0]["job_id"], first.as_str(), "{jobs:?}");
    release(&mut pipe, &first);
}

#[test] // spec: CORE-009, CORE-117
fn jobs_log_follows_state_changes() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "jobslog");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));
    let k = key();

    let id = job_id_of(&ok(&mut pipe, "test.job", json!({"lane": "cpu"}), Some(&k)));
    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");

    // queue/jobs.testmachine.<yyyy-mm>.jsonl (workspace format I8).
    let queue = ws.join("queue");
    let logs = files_named(&queue, "jobs.testmachine.", ".jsonl");
    assert!(!logs.is_empty(), "no jobs log in {}", queue.display());
    for log in &logs {
        let name = log.file_name().unwrap().to_string_lossy().into_owned();
        let month = &name["jobs.testmachine.".len()..name.len() - ".jsonl".len()];
        assert!(
            month.len() == 7
                && month.as_bytes()[4] == b'-'
                && month
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| i == 4 || b.is_ascii_digit()),
            "the jobs log is named by month: {name}"
        );
    }
    let mut lines: Vec<Value> = logs
        .iter()
        .flat_map(|log| jsonl(log))
        .filter(|l| l["job_id"] == id.as_str())
        .collect();
    lines.sort_by_key(|l| l["seq"].as_u64().unwrap_or(u64::MAX));
    let statuses: Vec<&str> = lines.iter().filter_map(|l| l["status"].as_str()).collect();
    let mut wanted = ["queued", "running", "succeeded"].iter().peekable();
    for status in &statuses {
        if wanted.peek() == Some(&status) {
            wanted.next();
        }
    }
    assert!(
        wanted.peek().is_none(),
        "the log lines for {id} must pass through queued, running, succeeded in seq order: {statuses:?}"
    );
    for line in &lines {
        assert_eq!(line["machine"], "testmachine", "{line}");
        assert!(line["seq"].is_u64(), "{line}");
        assert_eq!(line["kind"], done["kind"], "{line}");
        assert_eq!(line["idempotency_key"], k.as_str(), "{line}");
        assert!(
            line["trace_id"].as_str().is_some_and(|t| !t.is_empty()),
            "{line}"
        );
    }
}

#[test] // spec: ARCH-022, QA-055
fn core_restart_interrupts_unfinished_jobs_and_never_reruns() {
    let env = TestEnv::new();
    let core = start_abortable(&env, &[]);
    let id = {
        let mut pipe = session(&env, None);
        let id = submit(&mut pipe, "test.job", json!({"lane": "cpu", "gated": true}));
        until_job(&mut pipe, &id, "running", running);
        id
    };
    // Killed, not drained: nothing gets to mark the job on the way out.
    drop(core);

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let after = wait_done(&mut pipe, &id);
    assert_eq!(after["status"], "failed", "{after}");
    assert_eq!(after["substatus"], "interrupted", "{after}");
    assert_eq!(after["attempt"], 1, "the handler ran once: {after}");
    let jobs = all_jobs(&mut pipe);
    assert_eq!(jobs.len(), 1, "nothing re-ran as a new job: {jobs:?}");
    assert_eq!(jobs[0]["job_id"], id.as_str(), "{jobs:?}");
}

#[test] // spec: ARCH-020
fn core_stays_up_while_a_job_runs() {
    let env = TestEnv::new();
    let mut core = CoreFixture::start_with_env(&env, &[("LIL_TEST_IDLE_SECS", "2")]);
    let id = {
        let mut pipe = session(&env, None);
        let id = submit(&mut pipe, "test.job", json!({"lane": "cpu", "gated": true}));
        until_job(&mut pipe, &id, "running", running);
        id
    };
    // No client is connected now; only the running job holds the core up.
    assert_eq!(
        core.wait_exit(Duration::from_secs(6)),
        None,
        "the core went idle while a job ran"
    );
    {
        let mut pipe = session(&env, None);
        release(&mut pipe, &id);
        let done = wait_done(&mut pipe, &id);
        assert_eq!(done["status"], "succeeded", "{done}");
    }
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(0),
        "once the job ended and no client is connected the core exits as idle"
    );
}

#[test] // spec: ARCH-071, CORE-121
fn one_heavy_job_at_a_time() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let first = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "gated": true}),
    );
    until_job(&mut pipe, &first, "running", running);
    let second = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "gated": true}),
    );
    let waiting = job(&mut pipe, &second);
    assert_eq!(waiting["status"], "queued", "{waiting}");
    assert_eq!(waiting["substatus"], "waiting", "{waiting}");
    assert_eq!(waiting["lane"], "gpu-heavy", "{waiting}");

    // The cpu lane is not held up by the heavy one.
    let cpu = submit(&mut pipe, "test.job", json!({"lane": "cpu"}));
    let cpu_done = wait_done(&mut pipe, &cpu);
    assert_eq!(cpu_done["status"], "succeeded", "{cpu_done}");
    let still = job(&mut pipe, &second);
    assert_eq!(still["substatus"], "waiting", "{still}");

    release(&mut pipe, &first);
    assert_eq!(wait_done(&mut pipe, &first)["status"], "succeeded");
    until_job(&mut pipe, &second, "running", running);
    release(&mut pipe, &second);
    assert_eq!(wait_done(&mut pipe, &second)["status"], "succeeded");
}

#[test] // spec: CORE-124
fn interactive_goes_after_the_running_job_and_before_agent_batches() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let heavy = |priority: &str| json!({"lane": "gpu-heavy", "gated": true, "priority": priority});

    let a1 = submit(&mut pipe, "test.job", heavy("agent"));
    until_job(&mut pipe, &a1, "running", running);
    let a2 = submit(&mut pipe, "test.job", heavy("agent"));
    let i = submit(&mut pipe, "test.job", heavy("interactive"));
    assert_eq!(job(&mut pipe, &i)["priority"], "interactive");
    assert_eq!(job(&mut pipe, &a2)["priority"], "agent");

    // The running agent job is never overtaken; the interactive one goes next.
    let i_waiting = job(&mut pipe, &i);
    assert_eq!(i_waiting["status"], "queued", "{i_waiting}");
    release(&mut pipe, &a1);
    until_job(&mut pipe, &i, "running", running);
    let a2_waiting = job(&mut pipe, &a2);
    assert_eq!(a2_waiting["status"], "queued", "{a2_waiting}");

    release(&mut pipe, &i);
    until_job(&mut pipe, &a2, "running", running);
    release(&mut pipe, &a2);
    assert_eq!(wait_done(&mut pipe, &a2)["status"], "succeeded");
}

#[test] // spec: CORE-124
fn fifo_within_a_class() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let heavy = json!({"lane": "gpu-heavy", "gated": true, "priority": "agent"});

    let ids: Vec<String> = (0..3)
        .map(|_| submit(&mut pipe, "test.job", heavy.clone()))
        .collect();
    for (n, id) in ids.iter().enumerate() {
        until_job(&mut pipe, id, "running", running);
        for later in &ids[n + 1..] {
            let waiting = job(&mut pipe, later);
            assert_eq!(
                waiting["status"], "queued",
                "job {n} runs, so the later one waits: {waiting}"
            );
        }
        release(&mut pipe, id);
        assert_eq!(wait_done(&mut pipe, id)["status"], "succeeded");
    }
}

#[test] // spec: CORE-128, QA-004
fn transient_failures_retry_with_events_and_disk_full_does_not() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let id = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "cpu", "fail": "transient_2"}),
    );
    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["attempt"], 3, "{done}");

    let topic = format!("job:{id}");
    let mut events = session(&env, None);
    let seq = subscribe_replay(&mut events, &topic);
    let retrying: Vec<(u64, u64)> = replayed(&mut events, &topic, seq)
        .iter()
        .filter(|d| d["kind"] == "retrying")
        .map(|d| {
            (
                d["attempt"].as_u64().unwrap_or(0),
                d["of"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        retrying,
        vec![(1, 5), (2, 5)],
        "the retrying events on {topic}"
    );

    let full = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "cpu", "fail": "disk_full"}),
    );
    let failed = wait_done(&mut pipe, &full);
    assert_eq!(failed["status"], "failed", "{failed}");
    assert_eq!(failed["error"]["code"], "store.disk_full", "{failed}");
    assert_eq!(failed["attempt"], 1, "disk full is final: {failed}");
}

/// One GPU and per-process sample: the GPU's total and used memory, and another process's
/// local usage. `n` repeats it.
fn vram_samples(total: u64, used: u64, procs: &[(u32, u64)], n: usize) -> (Vec<Value>, Vec<Value>) {
    let gpu = json!({"name": "Fake GPU", "vram_total_mib": total, "vram_used_mib": used,
                     "util_pct": 0, "power_mw": 20000});
    let mem: Vec<Value> = procs
        .iter()
        .map(|(pid, mib)| json!({"pid": pid, "local_mib": mib}))
        .collect();
    (vec![gpu; n], vec![Value::Array(mem); n])
}

/// The GPU memory in use a snapshot reports: it tells which scripted reading a tick took.
fn vram_used(snapshot: &Value) -> Option<u64> {
    snapshot["gpu"]["vram_used_mib"].as_u64()
}

#[test] // spec: ARCH-071, CORE-126
fn waiting_for_vram_until_memory_frees() {
    let env = TestEnv::new();
    const OTHER: u32 = 4242;
    // Two readings with the other process at 15000 MiB, then it is at 2000 MiB (the GPU's used
    // memory moves with it, so a snapshot shows which reading a tick took).
    let (mut gpu, mut mem) = vram_samples(16384, 15000, &[(OTHER, 15000)], 2);
    let (gpu_after, mem_after) = vram_samples(16384, 2000, &[(OTHER, 2000)], 1);
    gpu.extend(gpu_after);
    mem.extend(mem_after);
    let hw = fake_hw(
        &env,
        json!({"gpu": gpu, "gpu_mem": mem, "cpu_energy": null,
               "sys": [{"cpu_permille": 100, "ram_total_mib": 32768, "ram_used_mib": 8000}]}),
    );
    let _core = CoreFixture::start_with_env(&env, &[(&hw.0, &hw.1)]);
    let mut pipe = session(&env, None);

    let first = write_ok(&mut pipe, "test.hw_tick", json!({"samples": 1}));
    assert_eq!(vram_used(&first), Some(15000), "{first}");
    let id = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "vram_need_mib": 4000}),
    );
    // free = min(16384 − 15000, 1384 + 0) − 512 = 872 MiB < 4000 MiB.
    let blocked = until_job(&mut pipe, &id, "waiting_for_vram", |j| {
        j["substatus"] == "waiting_for_vram"
    });
    assert_eq!(blocked["status"], "queued", "{blocked}");

    // Re-checked on every sample: once the other process is down to 2000 MiB the job fits.
    let mut freed = false;
    for _ in 0..4 {
        let snapshot = write_ok(&mut pipe, "test.hw_tick", json!({"samples": 1}));
        if vram_used(&snapshot) == Some(2000) {
            freed = true;
            break;
        }
        let still = job(&mut pipe, &id);
        assert_eq!(still["substatus"], "waiting_for_vram", "{still}");
    }
    assert!(freed, "the fake hardware never reported the freed memory");
    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(done.get("vram_unchecked").is_none(), "{done}");
}

#[test] // spec: ARCH-071
fn an_engines_own_vram_counts_as_available() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    // The in-process fake's pid is this test's: the engine itself holds 12000 MiB; 2000 MiB
    // more are used outside any process. Counting the engine's own memory as free gives
    // min(16384 − 0, 2384 + 12000) − 512 = 13872 MiB; counting it as another process's would
    // give min(4384, 2384) − 512 = 1872 MiB, too little for 3000.
    let (gpu, mem) = vram_samples(16384, 14000, &[(std::process::id(), 12000)], 1);
    let hw = fake_hw(
        &env,
        json!({"gpu": gpu, "gpu_mem": mem, "cpu_energy": null,
               "sys": [{"cpu_permille": 100, "ram_total_mib": 32768, "ram_used_mib": 8000}]}),
    );
    let _core = CoreFixture::start_with_env(&env, &[(&hw.0, &hw.1)]);
    let mut pipe = session(&env, None);
    until_engine(&mut pipe, "attached", |e| e["class"] == "attached");
    write_ok(&mut pipe, "test.hw_tick", json!({"samples": 1}));

    let id = submit(
        &mut pipe,
        "test.job",
        json!({"lane": "gpu-heavy", "engine": "comfyui", "vram_need_mib": 3000}),
    );
    let done = wait_done(&mut pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(done.get("vram_unchecked").is_none(), "{done}");
}
