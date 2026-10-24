//! Estimates and telemetry from plan 4a: `job.estimate` in parts from measured history (no part
//! below three similar runs), argument checks like a call's, `telemetry.energy` over a range, and
//! the app's process tracked while it is connected (spec CORE-161, APP-127, APP-140; Appendix G
//! "Estimates", rulings P4a-R14 and P4a-R27). Each core runs on fake hardware
//! (`LIL_TEST_FAKE_HW`) advanced with `test.hw_tick`. Driven through `test.job`, `job.estimate`,
//! the telemetry tools and the `ui:test` app (QA-018).

mod support;

use lil_testkit::{CoreFixture, FakeHwScript, RawPipe, TestApp, TestEnv, utc};
use serde_json::{Value, json};
use support::*;

/// Writes the fake hardware script and starts a core on it.
fn core_on(env: &TestEnv, script: Value) -> CoreFixture {
    let path = env.root().join("fake-hw.json");
    FakeHwScript::new(script).write(&path);
    CoreFixture::start_with_env(env, &[("LIL_TEST_FAKE_HW", &path_str(&path))])
}

/// A GPU reading whose energy counter reads `energy_mj`.
fn gpu(energy_mj: u64) -> Value {
    json!({"name": "Fake GPU", "driver": "999.99", "util_pct": 50, "power_mw": 100_000,
           "energy_mj": energy_mj, "temp_c": 60, "sm_clock_mhz": 1800, "mem_clock_mhz": 9000,
           "throttle": 0, "vram_total_mib": 16384, "vram_used_mib": 2048})
}

/// Fake hardware whose GPU energy counter rises by 1000 mJ a sample.
fn hardware() -> Value {
    let readings: Vec<Value> = (1..=40).map(|n| gpu(n * 1000)).collect();
    json!({"gpu": readings, "gpu_mem": [[]], "cpu_energy": null,
           "sys": [{"cpu_permille": 100, "per_core_permille": [100, 100],
                    "ram_total_mib": 32768, "ram_used_mib": 8192}]})
}

/// `test.hw_tick {samples}`: the snapshot after it.
fn tick(pipe: &mut RawPipe, samples: u32) -> Value {
    write_ok(pipe, "test.hw_tick", json!({ "samples": samples }))
}

/// `job.estimate` of `test.job` with `args`.
fn estimate(pipe: &mut RawPipe, args: Value) -> Value {
    ok(
        pipe,
        "job.estimate",
        json!({"tool": "test.job", "args": args}),
        None,
    )
}

/// The `(part, reason)` pairs of an estimate's `unknown` list, sorted.
fn unknown(estimate: &Value) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = estimate["unknown"]
        .as_array()
        .unwrap_or_else(|| panic!("no unknown list: {estimate}"))
        .iter()
        .map(|u| {
            (
                u["part"].as_str().unwrap_or_default().to_string(),
                u["reason"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// A decimal string with exactly `decimals` digits after the point.
fn decimal(v: &Value, decimals: usize) -> bool {
    v.as_str().is_some_and(|s| {
        s.split_once('.').is_some_and(|(int, frac)| {
            !int.is_empty()
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.len() == decimals
                && frac.bytes().all(|b| b.is_ascii_digit())
        })
    })
}

#[test] // spec: CORE-161
fn job_estimate_is_none_without_history() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    // Appendix G: an estimate in parts; with no history it has none, and `unknown` names the
    // parts a local job needs. A local job spends no money, so `money` is neither a part nor
    // unknown.
    let none = estimate(&mut pipe, json!({"lane": "cpu", "estimate_key": "spec"}));
    for part in ["local", "wall_s", "money"] {
        assert!(none.get(part).is_none(), "no {part} part yet: {none}");
    }
    assert_eq!(
        unknown(&none),
        [
            ("local".to_string(), "no_history".to_string()),
            ("wall_s".to_string(), "no_history".to_string())
        ],
        "{none}"
    );
}

#[test] // spec: CORE-161, APP-140
fn job_estimate_uses_measured_history_after_three_runs() {
    let env = TestEnv::new();
    let _core = core_on(&env, hardware());
    let mut pipe = session(&env, None);
    let args = json!({"lane": "gpu-heavy", "estimate_key": "k"});

    tick(&mut pipe, 1);
    for run in 0..3 {
        let below = estimate(&mut pipe, args.clone());
        assert!(
            below.get("wall_s").is_none(),
            "{run} runs are not history: {below}"
        );
        let id = submit(&mut pipe, "test.job", args.clone());
        until_job(&mut pipe, &id, "running", |j| j["status"] != "queued");
        tick(&mut pipe, 2);
        assert_eq!(wait_done(&mut pipe, &id)["status"], "succeeded");
        tick(&mut pipe, 1);
    }

    // `meter` may write the third run's history just after `job.wait` reports it done.
    let measured = poll("an estimate from 3 runs", || {
        let e = estimate(&mut pipe, args.clone());
        (e["wall_s"]["samples"] == 3).then_some(e)
    });
    let wall = &measured["wall_s"];
    assert_eq!(wall["source"], "measured_history", "{measured}");
    assert_eq!(wall["samples"], 3, "{measured}");
    assert!(
        decimal(&wall["p50"], 1),
        "seconds with one decimal: {measured}"
    );
    assert!(
        decimal(&wall["p90"], 1),
        "seconds with one decimal: {measured}"
    );
    let local = &measured["local"];
    assert_eq!(local["source"], "measured_history", "{measured}");
    assert_eq!(local["samples"], 3, "{measured}");
    assert!(
        decimal(&local["wh_p50"], 3),
        "Wh with three decimals: {measured}"
    );
    assert!(
        measured.get("money").is_none(),
        "a local job spends no money: {measured}"
    );
    assert_eq!(unknown(&measured), [], "{measured}");

    // Another key has no history of its own.
    let other = estimate(
        &mut pipe,
        json!({"lane": "gpu-heavy", "estimate_key": "other"}),
    );
    assert!(other.get("wall_s").is_none(), "{other}");
}

#[test] // spec: CORE-161
fn job_estimate_validates_args_like_a_call() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    estimate(&mut pipe, json!({"lane": "cpu"}));
    let unknown_arg = pipe.tool(
        "job.estimate",
        json!({"tool": "test.job", "args": {"lane": "cpu", "spec_no_such_argument": 1}}),
        None,
    );
    assert_eq!(error_code(&unknown_arg), "params.invalid", "{unknown_arg}");
    let unknown_tool = pipe.tool(
        "job.estimate",
        json!({"tool": "spec.no_such_tool", "args": {}}),
        None,
    );
    assert_eq!(error_code(&unknown_tool), "tool.unknown", "{unknown_tool}");
}

#[test] // spec: CORE-161
fn telemetry_energy_sums_a_range() {
    let env = TestEnv::new();
    let _core = core_on(&env, hardware());
    let mut pipe = session(&env, None);

    let snapshots: Vec<Value> = (0..5).map(|_| tick(&mut pipe, 1)).collect();
    let secs: Vec<i64> = snapshots
        .iter()
        .map(|s| utc::parse_rfc3339(s["ts"].as_str().unwrap_or_else(|| panic!("no ts: {s}"))))
        .collect();
    let from = rfc3339(secs.iter().min().copied().unwrap_or(0) - 60);
    let to = rfc3339(secs.iter().max().copied().unwrap_or(0) + 60);

    let series = ok(
        &mut pipe,
        "telemetry.series",
        json!({"metrics": ["gpu.energy"], "from": from, "to": to, "resolution": "1s"}),
        None,
    );
    let points: f64 = series["series"]
        .as_array()
        .and_then(|s| s.iter().find(|m| m["metric"] == "gpu.energy"))
        .and_then(|m| m["points"].as_array())
        .unwrap_or_else(|| panic!("no gpu.energy series: {series}"))
        .iter()
        .map(|p| number(&p[2]))
        .sum();
    assert!(points > 0.0, "the ticks measured energy: {series}");

    let energy = ok(
        &mut pipe,
        "telemetry.energy",
        json!({"from": from, "to": to}),
        None,
    );
    let instant = |v: &Value| {
        utc::parse_rfc3339(v.as_str().unwrap_or_else(|| panic!("not a time: {energy}")))
    };
    assert_eq!(instant(&energy["from"]), instant(&json!(from)), "{energy}");
    assert_eq!(instant(&energy["to"]), instant(&json!(to)), "{energy}");
    let gpu_mj = number(&energy["gpu_mj"]);
    assert!(
        (gpu_mj - points).abs() < 0.5,
        "telemetry.energy {gpu_mj} mJ, the series {points} mJ: {energy}"
    );
    assert!(
        energy["system_mj"].is_string() || energy["system_mj"].is_number(),
        "{energy}"
    );
    assert_eq!(energy["measured"]["gpu"], true, "{energy}");
}

/// Whether a snapshot lists `pid` among its processes with role `app`.
fn tracked_as_app(snapshot: &Value, pid: u32) -> bool {
    snapshot["processes"]
        .as_array()
        .is_some_and(|ps| ps.iter().any(|p| p["pid"] == pid && p["role"] == "app"))
}

#[test] // spec: APP-127
fn ui_connections_are_tracked_in_telemetry() {
    let env = TestEnv::new();
    let _core = core_on(&env, hardware());
    let mut pipe = session(&env, None);

    let app = TestApp::start(&env, None, &[]);
    let pid = app.pid();
    assert!(
        within_ticks(&mut pipe, |s| tracked_as_app(s, pid)),
        "the connected app is tracked with role app"
    );

    drop(app);
    assert!(
        within_ticks(&mut pipe, |s| !tracked_as_app(s, pid)),
        "the app is no longer tracked once it closed"
    );
}

/// Whether `pred` holds for a snapshot, ticking, within 10 s (a deadline, never a
/// measurement; a tool error fails at once).
fn within_ticks(pipe: &mut RawPipe, pred: impl Fn(&Value) -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        tick(pipe, 1);
        let snapshot = ok(pipe, "telemetry.snapshot", json!({}), None);
        if pred(&snapshot) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
