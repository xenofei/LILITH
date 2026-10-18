//! Hardware telemetry: measured and estimated parts of the whole-system figure, energy from
//! counters across resets, the CPU estimate without the Energy Meter, throttle alerts, the
//! hardware profile, a damaged `telemetry.db`, and per-job energy by SM share (spec CORE-004,
//! CORE-059, CORE-060, CORE-062, CORE-063, CORE-064, CORE-065, CORE-066, CORE-067, QA-055;
//! Appendix F "Telemetry"). Each core runs on fake hardware (`LIL_TEST_FAKE_HW`) and is advanced
//! with `test.hw_tick`, never by waiting for the sampler. Driven through the telemetry tools
//! and the files on disk (QA-018).

mod support;

use lil_testkit::{CoreFixture, FakeComfy, FakeHwScript, RawPipe, TestEnv, sha256_file, utc};
use serde_json::{Value, json};
use support::*;

/// Writes the fake hardware script and starts a core on it.
fn core_on(env: &TestEnv, script: Value) -> CoreFixture {
    let path = env.root().join("fake-hw.json");
    FakeHwScript::new(script).write(&path);
    CoreFixture::start_with_env(env, &[("LIL_TEST_FAKE_HW", &path_str(&path))])
}

/// A GPU reading.
fn gpu(power_mw: u64, energy_mj: u64, throttle: u64) -> Value {
    json!({"name": "Fake GPU", "driver": "999.99", "util_pct": 50, "power_mw": power_mw,
           "energy_mj": energy_mj, "temp_c": 60, "sm_clock_mhz": 1800, "mem_clock_mhz": 9000,
           "throttle": throttle, "vram_total_mib": 16384, "vram_used_mib": 2048})
}

/// A system reading at `cpu_permille`.
fn sys(cpu_permille: u64) -> Value {
    json!({"cpu_permille": cpu_permille, "per_core_permille": [cpu_permille, cpu_permille],
           "ram_total_mib": 32768, "ram_used_mib": 8192})
}

/// `test.hw_tick {samples}`: the snapshot after it.
fn tick(pipe: &mut RawPipe, samples: u32) -> Value {
    write_ok(pipe, "test.hw_tick", json!({ "samples": samples }))
}

/// A part of `system_parts`.
fn part<'a>(snapshot: &'a Value, name: &str) -> &'a Value {
    snapshot["system_parts"]
        .as_array()
        .and_then(|parts| parts.iter().find(|p| p["part"] == name))
        .unwrap_or_else(|| panic!("no {name} part in {snapshot}"))
}

/// Whether a `measured` label says measured (a `true` flag or the word).
fn is_measured(v: &Value) -> bool {
    v == true || v == "measured"
}

/// Every key path in `v` whose last key mentions a temperature.
fn temperature_keys(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let here = format!("{path}.{k}");
                if k.to_ascii_lowercase().contains("temp") {
                    out.push(here.clone());
                }
                temperature_keys(child, &here, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                temperature_keys(child, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// `from` and `to` around the snapshots' times, a minute either side.
fn range_around(snapshots: &[Value]) -> (String, String) {
    let secs: Vec<i64> = snapshots
        .iter()
        .map(|s| utc::parse_rfc3339(s["ts"].as_str().unwrap_or_else(|| panic!("no ts: {s}"))))
        .collect();
    let from = secs.iter().min().copied().unwrap_or(0) - 60;
    let to = secs.iter().max().copied().unwrap_or(0) + 60;
    (rfc3339(from), rfc3339(to))
}

/// The sum of the average column of `metric`'s points between `from` and `to`, at 1 s.
fn series_sum(pipe: &mut RawPipe, metric: &str, from: &str, to: &str) -> f64 {
    let series = ok(
        pipe,
        "telemetry.series",
        json!({"metrics": [metric], "from": from, "to": to, "resolution": "1s"}),
        None,
    );
    let points = series["series"]
        .as_array()
        .and_then(|s| s.iter().find(|m| m["metric"] == metric))
        .and_then(|m| m["points"].as_array())
        .unwrap_or_else(|| panic!("no {metric} series: {series}"))
        .clone();
    points.iter().map(|p| number(&p[2])).sum()
}

#[test] // spec: CORE-059, CORE-060, CORE-064
fn snapshot_labels_measured_and_estimated_parts() {
    let env = TestEnv::new();
    let _core = core_on(
        &env,
        json!({"gpu": [gpu(200_000, 1000, 0), gpu(200_000, 2000, 0)],
               "gpu_mem": [[]],
               "cpu_energy": [{"package_energy_pwh": 1_000_000_000, "package_power_mw": 30_000},
                              {"package_energy_pwh": 9_333_333_333_u64, "package_power_mw": 30_000}],
               "sys": [sys(250)]}),
    );
    let mut pipe = session(&env, None);
    tick(&mut pipe, 2);
    let snapshot = ok(&mut pipe, "telemetry.snapshot", json!({}), None);

    assert!(is_measured(&snapshot["gpu"]["measured"]), "{snapshot}");
    assert!(is_measured(&snapshot["cpu"]["measured"]), "{snapshot}");
    assert!(
        is_measured(&part(&snapshot, "gpu")["measured"]),
        "{snapshot}"
    );
    assert!(
        is_measured(&part(&snapshot, "cpu")["measured"]),
        "{snapshot}"
    );
    let gpu_mw = number(&snapshot["gpu"]["power_mw"]);
    let cpu_mw = number(&snapshot["cpu"]["package_power_mw"]);
    assert_eq!((gpu_mw, cpu_mw), (200_000.0, 30_000.0), "{snapshot}");
    // (GPU + CPU package + rest of system 0) / PSU efficiency 0.9.
    let expected = (gpu_mw + cpu_mw + 0.0) * 1000.0 / 900.0;
    let system = number(&snapshot["system_power_mw"]);
    assert!(
        (system - expected).abs() <= 1.0,
        "system {system} mW, expected {expected}: {snapshot}"
    );

    let mut temps = Vec::new();
    temperature_keys(&snapshot, "", &mut temps);
    let not_gpu: Vec<&String> = temps.iter().filter(|k| !k.starts_with(".gpu.")).collect();
    assert!(
        not_gpu.is_empty(),
        "CPU temperature is never sampled: {not_gpu:?}"
    );
}

#[test] // spec: CORE-062, CORE-063
fn energy_totals_come_from_counters_across_resets() {
    let env = TestEnv::new();
    let _core = core_on(
        &env,
        json!({"gpu": [gpu(100_000, 1000, 0), gpu(100_000, 2000, 0),
                       gpu(100_000, 100, 0), gpu(100_000, 300, 0)],
               "gpu_mem": [[]], "cpu_energy": null, "sys": [sys(100)]}),
    );
    let mut pipe = session(&env, None);
    let snapshots: Vec<Value> = (0..4).map(|_| tick(&mut pipe, 1)).collect();
    let (from, to) = range_around(&snapshots);

    // 1000 → 2000 adds 1000; the drop to 100 is a reset, so that interval adds 100; then 200.
    let total = series_sum(&mut pipe, "gpu.energy", &from, &to);
    assert!(
        (total - 1300.0).abs() < 0.5,
        "gpu.energy over the ticks: {total} mJ"
    );
}

#[test] // spec: CORE-059
fn cpu_energy_estimate_when_the_meter_is_missing() {
    let script = json!({"gpu": [gpu(120_000, 1000, 0)], "gpu_mem": [[]], "cpu_energy": null,
                        "sys": [sys(400)]});

    // With a TDP: utilisation × TDP, estimated.
    let env = TestEnv::new();
    env.suite_toml_set("[power]\ncpu_tdp_w = \"65\"\n");
    let _core = core_on(&env, script.clone());
    let mut pipe = session(&env, None);
    tick(&mut pipe, 2);
    let snapshot = ok(&mut pipe, "telemetry.snapshot", json!({}), None);
    let cpu = part(&snapshot, "cpu");
    assert_eq!(cpu["measured"], "estimated", "{snapshot}");
    assert!(
        (number(&cpu["mw"]) - 26_000.0).abs() <= 1.0,
        "40 % of 65 W: {snapshot}"
    );

    // Without one: unknown, and left out of the whole-system figure.
    let env = TestEnv::new();
    let _core = core_on(&env, script);
    let mut pipe = session(&env, None);
    tick(&mut pipe, 2);
    let snapshot = ok(&mut pipe, "telemetry.snapshot", json!({}), None);
    assert_eq!(part(&snapshot, "cpu")["measured"], "unknown", "{snapshot}");
    let gpu_mw = number(&snapshot["gpu"]["power_mw"]);
    let expected = (gpu_mw + 0.0) * 1000.0 / 900.0;
    let system = number(&snapshot["system_power_mw"]);
    assert!(
        (system - expected).abs() <= 1.0,
        "system {system} mW, expected {expected}: {snapshot}"
    );
}

#[test] // spec: CORE-066
fn thermal_throttle_raises_and_clears_an_alert() {
    let env = TestEnv::new();
    // NVML's hardware thermal slowdown reason bit.
    const THERMAL: u64 = 0x40;
    let readings: Vec<Value> = [0, THERMAL, THERMAL, THERMAL, 0, 0]
        .iter()
        .map(|t| gpu(100_000, 1000, *t))
        .collect();
    let _core = core_on(
        &env,
        json!({"gpu": readings, "gpu_mem": [[]], "cpu_energy": null, "sys": [sys(100)]}),
    );
    let mut pipe = session(&env, None);
    for _ in 0..6 {
        tick(&mut pipe, 1);
    }
    let mut events = session(&env, None);
    let seq = subscribe_replay(&mut events, "telemetry");
    let kinds: Vec<String> = replayed(&mut events, "telemetry", seq)
        .iter()
        .filter_map(|d| d["kind"].as_str().map(str::to_string))
        .filter(|k| k.starts_with("alert"))
        .collect();
    assert_eq!(
        kinds,
        ["alert", "alert_cleared"],
        "raised once and cleared once"
    );
}

/// A line `key: value` (the value plain or quoted) in a YAML text.
fn yaml_has(text: &str, key: &str, value: &str) -> bool {
    text.lines().any(|l| {
        l.trim()
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|v| v.trim().trim_matches(['"', '\'']) == value)
    })
}

#[test] // spec: CORE-067
fn hardware_profile_is_written_only_when_it_changes() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "hardware");
    let file = ws.join("lilith.yaml");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str(
        "hardware:\n  gpu: Old GPU\n  vram_gb: 8\n  ram_gb: 16\n  detected: 2026-01-01\n",
    );
    std::fs::write(&file, &text).unwrap();
    let script = json!({"gpu": [{"name": "Fake RTX 9090", "vram_total_mib": 16384,
                                  "vram_used_mib": 1024, "power_mw": 50_000, "energy_mj": 10}],
                        "gpu_mem": [[]], "cpu_energy": null,
                        "sys": [{"cpu_permille": 100, "ram_total_mib": 32768, "ram_used_mib": 4096}]});

    let core = core_on(&env, script.clone());
    {
        let mut pipe = session(&env, Some(&ws));
        tick(&mut pipe, 1);
        let updated = poll("the hardware profile edit", || {
            let now = std::fs::read_to_string(&file).ok()?;
            now.contains("Fake RTX 9090").then_some(now)
        });
        assert!(yaml_has(&updated, "gpu", "Fake RTX 9090"), "{updated}");
        assert!(yaml_has(&updated, "vram_gb", "16"), "{updated}");
        assert!(yaml_has(&updated, "ram_gb", "32"), "{updated}");
        assert!(
            updated.starts_with("# Synthetic sample workspace"),
            "a targeted edit: {updated}"
        );
    }
    drop(core);
    let before = sha256_file(&file);

    let _core = core_on(&env, script);
    let mut pipe = session(&env, Some(&ws));
    tick(&mut pipe, 1);
    ok(&mut pipe, "telemetry.snapshot", json!({}), None);
    assert_eq!(
        sha256_file(&file),
        before,
        "an unchanged profile is not rewritten"
    );
}

#[test] // spec: CORE-004, QA-055
fn damaged_telemetry_db_is_rebuilt() {
    let env = TestEnv::new();
    let db = env.env.local_dir().join("telemetry.db");
    lil_testkit::files::write_file(&db, &[0xAB_u8; 4096]);
    let damaged_sha = sha256_file(&db);

    let _core = core_on(
        &env,
        json!({"gpu": [gpu(100_000, 1000, 0)], "gpu_mem": [[]], "cpu_energy": null,
               "sys": [sys(100)]}),
    );
    let mut pipe = session(&env, None);
    let snapshot = tick(&mut pipe, 1);
    let (from, to) = range_around(&[snapshot]);
    ok(
        &mut pipe,
        "telemetry.series",
        json!({"metrics": ["gpu.power"], "from": from, "to": to}),
        None,
    );

    let compact = utc::compact_now_pattern();
    let aside = files_named(db.parent().unwrap(), "telemetry.db.damaged-", "");
    let kept: Vec<_> = aside
        .iter()
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            compact(&name["telemetry.db.damaged-".len()..])
        })
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the damaged file is set aside once: {aside:?}"
    );
    assert_eq!(sha256_file(kept[0]), damaged_sha, "the old bytes are kept");
}

#[test] // spec: CORE-065
fn job_energy_is_split_by_sm_share() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    let engine = std::process::id();
    const OTHER: u32 = 4242;
    let readings: Vec<Value> = (1..=40)
        .map(|n| {
            let mut g = gpu(150_000, n * 1000, 0);
            g["sm_by_pid"] = json!([[engine, 750], [OTHER, 250]]);
            g
        })
        .collect();
    let _core = core_on(
        &env,
        json!({"gpu": readings, "gpu_mem": [[{"pid": engine, "local_mib": 3000}]],
               "cpu_energy": null, "sys": [sys(100)]}),
    );
    let mut pipe = session(&env, None);

    fake.hold_prompts(true);
    let id = submit(&mut pipe, "test.comfy_submit", json!({"graph": "tiny"}));
    let body = poll("the prompt in ComfyUI", || {
        posted_prompts(&fake).into_iter().next()
    });
    until_job(&mut pipe, &id, "running", running);
    tick(&mut pipe, 4);
    fake.release(body["prompt_id"].as_str().unwrap_or_default());
    assert_eq!(wait_done(&mut pipe, &id)["status"], "succeeded");
    tick(&mut pipe, 1);

    let energy = poll("the job's energy", || {
        let jobs = ok(&mut pipe, "telemetry.jobs", json!({"job_ids": [id]}), None);
        jobs["jobs"]
            .as_array()?
            .iter()
            .find(|j| j["job_id"] == id.as_str())
            .cloned()
    });
    let from = energy["from"].as_str().unwrap_or_default().to_string();
    let to = energy["to"].as_str().unwrap_or_default().to_string();
    let window = series_sum(&mut pipe, "gpu.energy", &from, &to);
    assert!(window > 0.0, "no GPU energy in the job's window: {energy}");
    let gross = number(&energy["gpu_mj_gross"]);
    assert!(
        (gross - 0.75 * window).abs() <= 1.0,
        "75 % of the window's {window} mJ, got {gross}: {energy}"
    );
}
