//! Caps and metering: one prompt covers a cap and a safety confirmation, allow-once, only the
//! app changes caps, atomic reservations, alerts, subscription quota outside money caps, the
//! write-ahead usage row, loose usage, summaries by basis and currency with fixed costs, and
//! local-work records with energy (spec ARCH-056, ARCH-081, ARCH-099, CORE-008, CORE-015,
//! CORE-064, CORE-065, CORE-069, CORE-071, CORE-081, CORE-082, CORE-083, CORE-087, CORE-088,
//! CORE-089, CORE-090, CORE-091, APP-045, QA-016, QA-047, QA-056; Appendix F "Usage records",
//! "Caps and money in `suite.toml`"). Caps are written to `suite.toml` before the core starts,
//! with display currency USD. Driven through `test.meter_external`, `test.destructive`, the meter
//! tools, the app methods, `lil` and the files on disk (QA-018).

mod support;

use std::path::Path;

use lil_testkit::{
    CoreFixture, FakeComfy, FakeHwScript, TestApp, TestEnv, failpoints, sha256_file,
};
use serde_json::{Value, json};
use support::*;

/// `[money]` in USD and `[caps] monthly`.
fn caps(env: &TestEnv, monthly: &str) {
    env.suite_toml_set(&format!(
        "[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"{monthly}\"\n"
    ));
}

/// An external call through the meter: `amount` USD from the fake provider.
fn external(amount: &str) -> Value {
    json!({"provider": "fakeprov", "amount": amount})
}

/// The `monthly` row of `caps.get`.
fn monthly(c: &mut impl Caller) -> Value {
    let view = ok(c, "caps.get", json!({}), None);
    assert_eq!(view["display_currency"], "USD", "{view}");
    view["caps"]
        .as_array()
        .and_then(|caps| caps.iter().find(|c| c["cap"] == "monthly"))
        .cloned()
        .unwrap_or_else(|| panic!("caps.get has no monthly cap: {view}"))
}

/// The usage lines (of `path`) written for `tool`.
fn usage_lines(path: &Path, tool: &str) -> Vec<Value> {
    jsonl(path)
        .into_iter()
        .filter(|l| l["tool"] == tool)
        .collect()
}

#[test] // spec: CORE-091, APP-045
fn cap_prompt_covers_the_cap_and_allow_once_runs_it() {
    let env = TestEnv::new();
    caps(&env, "1");
    let toml_before = sha256_file(&env.env.suite_toml());
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &[]);
    let mut pipe = session(&env, None);
    let k = key();

    let pending = pipe.tool("test.meter_external", external("2"), Some(&k));
    let confirmation = pending_id(&pending);
    assert_eq!(pending["covers"], json!(["cap"]), "{pending}");
    answer(
        &mut app,
        &confirmation,
        json!({"answer": "approve", "cap": {"action": "allow_once"}}),
    );
    ok(&mut pipe, "test.meter_external", external("2"), Some(&k));

    let row = monthly(&mut pipe);
    assert_eq!(row["settled"], "2", "{row}");
    assert_eq!(
        sha256_file(&env.env.suite_toml()),
        toml_before,
        "allow once never changes the cap"
    );
}

#[test] // spec: CORE-015, ARCH-056
fn destructive_and_over_cap_share_one_prompt() {
    let env = TestEnv::new();
    caps(&env, "1");
    let _core = CoreFixture::start(&env);
    let mut app = TestApp::start(&env, None, &[]);
    let mut pipe = session(&env, None);

    let pending = pipe.tool("test.destructive", json!({"cost": "2"}), Some(&key()));
    pending_id(&pending);
    assert_eq!(
        pending["covers"],
        json!(["confirm", "cap"]),
        "one prompt covers both: {pending}"
    );
    let open = confirmations(&mut app, "pending");
    assert_eq!(open.len(), 1, "one confirmation, not two: {open:?}");
}

#[test] // spec: QA-016, CORE-091
fn non_interactive_over_cap_is_cap_exceeded() {
    let env = TestEnv::new();
    caps(&env, "1");
    let _core = CoreFixture::start(&env);

    let args = external("2").to_string();
    let k = key();
    let out = env.lil(&[
        "--non-interactive",
        "call",
        "test.meter_external",
        &args,
        "--idempotency-key",
        &k,
    ]);
    assert_eq!(out.code, 1, "{out:?}");
    assert_eq!(error_code(&stdout_json(&out)), "cap.exceeded", "{out:?}");
    let lines = usage_lines(&loose_usage(&env), "test.meter_external");
    assert!(
        lines.is_empty(),
        "a refused call writes no usage line: {lines:?}"
    );
}

#[test] // spec: CORE-008, ARCH-056
fn only_the_app_changes_caps() {
    let env = TestEnv::new();
    let file = env.env.suite_toml();
    lil_testkit::files::write_file(
        &file,
        b"# my caps, set by hand\n[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n",
    );
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let list = pipe.call("tools/list", json!({}));
    let tools = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list: {list}"))
        .clone();
    let cap_tools: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .filter(|n| n.starts_with("caps."))
        .collect();
    assert_eq!(cap_tools, ["caps.get"], "no tool but caps.get touches caps");

    let by_cli = pipe.call("app/caps.set", json!({"monthly": "5"}));
    assert_eq!(
        by_cli["result"]["error"]["code"], "caller.not_allowed",
        "{by_cli}"
    );

    let mut app = TestApp::start(&env, None, &[]);
    let set = app.call("app/caps.set", json!({"monthly": "5"}));
    assert_eq!(set["result"]["status"], "ok", "{set}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.lines().any(|l| l.trim() == "# my caps, set by hand"),
        "the comment line was lost: {text}"
    );
    let limit = toml_key(&text, "caps", "monthly");
    assert_eq!(limit.as_deref(), Some("5"), "{text}");
    assert_eq!(monthly(&mut pipe)["limit"], "5");
}

/// The value of `key` in the `[table]` of a TOML text, without quotes: enough to read one cap
/// back as an outside program would (the spec tests have no TOML parser of their own).
fn toml_key(text: &str, table: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim_matches(['[', ']']).trim().to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if current == table && k.trim() == key {
            let v = v.split('#').next().unwrap_or_default().trim();
            return Some(v.trim_matches(['"', '\'']).to_string());
        }
    }
    None
}

#[test] // spec: CORE-087, QA-047, QA-056
fn concurrent_calls_never_overshoot_a_cap() {
    let env = TestEnv::new();
    caps(&env, "1");
    let _core = CoreFixture::start(&env);
    let args = external("0.30").to_string();

    let outcomes: Vec<(i32, Value)> = std::thread::scope(|s| {
        let calls: Vec<_> = (0..10)
            .map(|_| {
                s.spawn(|| {
                    let k = key();
                    let out = env.lil(&[
                        "--non-interactive",
                        "call",
                        "test.meter_external",
                        &args,
                        "--idempotency-key",
                        &k,
                    ]);
                    (out.code, stdout_json(&out))
                })
            })
            .collect();
        calls.into_iter().map(|c| c.join().unwrap()).collect()
    });
    let succeeded = outcomes.iter().filter(|(_, o)| o["status"] == "ok").count();
    let exceeded = outcomes
        .iter()
        .filter(|(_, o)| o["error"]["code"] == "cap.exceeded")
        .count();
    assert_eq!((succeeded, exceeded), (3, 7), "{outcomes:?}");
    let mut pipe = session(&env, None);
    assert_eq!(monthly(&mut pipe)["settled"], "0.9");
}

#[test] // spec: CORE-089
fn cap_alerts_fire_once_per_level() {
    let env = TestEnv::new();
    caps(&env, "1");
    let _core = CoreFixture::start(&env);
    let mut pipe = non_interactive_session(&env, None);

    // 60 %, 70 %, 85 %, 100 %.
    for amount in ["0.6", "0.1", "0.15", "0.15"] {
        ok(
            &mut pipe,
            "test.meter_external",
            external(amount),
            Some(&key()),
        );
    }
    let mut events = session(&env, None);
    let seq = subscribe_replay(&mut events, "caps");
    let levels: Vec<Value> = replayed(&mut events, "caps", seq)
        .iter()
        .filter(|d| d["kind"] == "alert")
        .map(|d| d["threshold_pct"].clone())
        .collect();
    assert_eq!(
        levels,
        [json!(50), json!(80), json!(100)],
        "one alert per level, in order, none repeated"
    );
}

#[test] // spec: CORE-090
fn subscription_quota_never_counts_against_money_caps() {
    let env = TestEnv::new();
    caps(&env, "1");
    let _core = CoreFixture::start(&env);
    let mut pipe = non_interactive_session(&env, None);

    let quota = json!({"provider": "fakeprov", "amount": "100", "basis": "subscription_quota"});
    let outcome = pipe.tool("test.meter_external", quota, Some(&key()));
    assert_eq!(
        outcome["status"], "ok",
        "no prompt and no cap error: {outcome}"
    );
    let row = monthly(&mut pipe);
    assert_eq!(row["settled"], "0", "{row}");
    assert_eq!(row["held"], "0", "{row}");
}

#[test] // spec: CORE-071, CORE-088
fn usage_is_written_ahead_and_survives_a_crash() {
    let env = TestEnv::new();
    caps(&env, "10");
    let spec = failpoints::spec(&[("meter.after_pending", "abort")]);
    let core = start_abortable(&env, &[(failpoints::ENV, &spec)]);
    {
        let mut pipe = session(&env, None);
        call_into_abort(&mut pipe, "test.meter_external", external("0.5"));
    }
    assert!(core.wait_exit(DEADLINE).is_some(), "the core did not abort");
    drop(core);

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let row = monthly(&mut pipe);
    assert_eq!(
        row["held"], "0.5",
        "the dead call's reservation is kept: {row}"
    );
    let lines = poll("the pending usage line", || {
        let lines = usage_lines(&loose_usage(&env), "test.meter_external");
        (!lines.is_empty()).then_some(lines)
    });
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["status"], "pending", "{lines:?}");
}

#[test] // spec: ARCH-081, ARCH-099
fn loose_calls_log_to_usage_loose_and_count_against_caps() {
    let env = TestEnv::new();
    caps(&env, "10");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    ok(
        &mut pipe,
        "test.meter_external",
        external("0.25"),
        Some(&key()),
    );
    let lines = usage_lines(&loose_usage(&env), "test.meter_external");
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["workspace"], "loose", "{line}");
    assert_eq!(line["machine"], "testmachine", "{line}");
    assert_eq!(line["status"], "succeeded", "{line}");
    assert_eq!(line["provider"], "fakeprov", "{line}");
    assert_eq!(line["caller_kind"], "cli", "{line}");
    assert_eq!(monthly(&mut pipe)["settled"], "0.25");
}

/// Fake hardware whose GPU counter rises 1000 mJ a sample, with `pid` (the in-process engine)
/// using 900 ‰ of the SMs.
fn busy_gpu(env: &TestEnv, pid: u32) -> String {
    let samples: Vec<Value> = (1..=40)
        .map(|n| {
            json!({"name": "Fake GPU", "util_pct": 90, "power_mw": 150_000, "energy_mj": n * 1000,
                   "temp_c": 60, "vram_total_mib": 16384, "vram_used_mib": 4000,
                   "sm_by_pid": [[pid, 900]]})
        })
        .collect();
    let sys: Vec<Value> = (1..=40)
        .map(|n| {
            json!({"cpu_permille": 200, "ram_total_mib": 32768, "ram_used_mib": 8000,
                         "procs": [[pid, n * 100, 500]]})
        })
        .collect();
    let path = env.root().join("fake-hw.json");
    FakeHwScript::new(
        json!({"gpu": samples, "gpu_mem": [[{"pid": pid, "local_mib": 3000}]],
                             "cpu_energy": null, "sys": sys}),
    )
    .write(&path);
    path_str(&path)
}

/// Runs one held `comfy.prompt` job with hardware samples taken while it executes; returns
/// its id once it succeeded.
fn local_job(pipe: &mut lil_testkit::RawPipe, fake: &FakeComfy) -> String {
    fake.hold_prompts(true);
    let id = submit(pipe, "test.comfy_submit", json!({"graph": "tiny"}));
    let body = poll("the prompt in ComfyUI", || {
        posted_prompts(fake).into_iter().next()
    });
    until_job(pipe, &id, "running", running);
    write_ok(pipe, "test.hw_tick", json!({"samples": 3}));
    fake.release(body["prompt_id"].as_str().unwrap_or_default());
    let done = wait_done(pipe, &id);
    assert_eq!(done["status"], "succeeded", "{done}");
    write_ok(pipe, "test.hw_tick", json!({"samples": 1}));
    id
}

#[test] // spec: CORE-081, CORE-082, CORE-083
fn summary_keeps_bases_and_currencies_apart_and_adds_fixed_costs() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    env.suite_toml_set(
        "[money]\ndisplay_currency = \"USD\"\n\
         [[money.rates]]\ncurrency = \"EUR\"\nin_display = \"1.1\"\ndate = \"2026-01-01\"\n\
         [[fixed_costs]]\nname = \"storage\"\namount = \"5\"\ncurrency = \"USD\"\nperiod = \"monthly\"\n",
    );
    let hw = busy_gpu(&env, std::process::id());
    let _core = CoreFixture::start_with_env(&env, &[("LIL_TEST_FAKE_HW", &hw)]);
    let mut pipe = session(&env, None);

    ok(
        &mut pipe,
        "test.meter_external",
        external("1"),
        Some(&key()),
    );
    let eur = json!({"provider": "fakeprov", "amount": "2", "currency": "EUR"});
    ok(&mut pipe, "test.meter_external", eur, Some(&key()));
    local_job(&mut pipe, &fake);

    let summary = ok(&mut pipe, "usage.summary", json!({"period": "month"}), None);
    assert_eq!(
        summary["by_currency"],
        json!({"USD": "1", "EUR": "2"}),
        "money only, per currency: {summary}"
    );
    // 1 USD + 2 EUR × 1.1 + the 5 USD fixed cost of the month (CORE-083).
    assert_eq!(summary["display_total"], "8.2", "{summary}");
    assert_eq!(
        summary["fixed_costs"],
        json!([{"name": "storage", "amount": "5", "currency": "USD", "display_amount": "5"}]),
        "{summary}"
    );
    let bases = &summary["bases"];
    assert_eq!(bases["money"]["records"], 2, "{summary}");
    assert_eq!(bases["local"]["records"], 1, "{summary}");
}

#[test] // spec: CORE-064, CORE-065, CORE-069
fn local_job_usage_has_energy_and_no_money() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    let hw = busy_gpu(&env, std::process::id());
    let _core = CoreFixture::start_with_env(&env, &[("LIL_TEST_FAKE_HW", &hw)]);
    let mut pipe = session(&env, None);

    let id = local_job(&mut pipe, &fake);
    let line = poll("the job's usage line", || {
        jsonl(&loose_usage(&env))
            .into_iter()
            .find(|l| l["job_id"] == id.as_str())
    });
    assert_eq!(line["billing_basis"], "local", "{line}");
    assert_eq!(line["cost"], "0", "{line}");
    assert!(number(&line["gpu_seconds"]) > 0.0, "{line}");
    assert!(number(&line["energy_wh_gross"]) > 0.0, "{line}");
}
