//! `lil doctor`'s plan-4a checks against the real `lil` and `lil-core` (APP-022, ARCH-065,
//! CORE-130, CORE-164, QA-060; Appendix G "Activation and registration" and "The `lil`
//! commands this plan adds"; P4a-R26):
//! - the AUMID shortcut, created or repaired only after `y`, and the channel's `lil:` and
//!   `.lilref` handlers, each on its own: only the missing ones are written, after `y`, never
//!   with `--non-interactive`; one another channel or another program holds is a warning and
//!   kept (plan 4a ruling, 2026-10-25);
//! - a `warning:` for each `engines.environment` change, the asset catalogue line and the
//!   `models.report` counts, from a core that ran before the doctor; one the doctor's
//!   workspace checks started isn't read yet;
//! - a second run changes nothing.
//!
//! A test channel writes its shortcut under `<test root>\profile\Programs\` and its handlers
//! under `HKCU\Software\LILITH-test-<id>\Classes`, which the shell never reads (QA-040); the
//! `TestEnv`'s drop removes that key. The prompts are answered through stdin.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lil_platform::desktop::{
    HandlerPart, HandlerState, ShortcutState, handler_parts, handler_state, read_shortcut,
    register_handlers, shortcut_state, write_shortcut,
};
use lil_protocol::{Outcome, ToolOutput};
use lil_testkit::{CoreFixture, FakeComfy, LilOutput, TestEnv, ensure_built};
use serde_json::{Value, json};

/// The rollback hint every engine-change warning ends with (Appendix G).
const HINT: &str = "Roll back with ComfyUI-Manager's snapshot restore or comfy-cli's version \
                    switch; the suite doesn't restore engines (CORE-166).";

/// The three asset-catalogue lines; a run prints exactly one of them.
const CATALOGUE: [&str; 3] = [
    "ok: ComfyUI's asset catalogue is off",
    "warning: ComfyUI's asset catalogue is on",
    "info: ComfyUI's asset catalogue is unknown",
];

/// `lil doctor` with `answer` typed at every question.
fn doctor(env: &TestEnv, answer: &str) -> LilOutput {
    let out = env.lil_with_stdin(&["doctor"], &format!("{answer}\n").repeat(20));
    assert!(out.code == 0 || out.code == 1, "{out:?}");
    out
}

/// The printed lines that start with `kind` (`ok:`, `fixed:`, `problem:`, `warning:`,
/// `info:`) and contain `what`, ignoring case.
fn lines<'a>(out: &'a LilOutput, kind: &str, what: &str) -> Vec<&'a str> {
    let what = what.to_ascii_lowercase();
    out.stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(kind) && l.to_ascii_lowercase().contains(&what))
        .collect()
}

/// The channel's `lil-image.exe`, which the doctor's `lil` (in the test binaries' folder)
/// names: the exe beside it.
fn app_exe() -> PathBuf {
    ensure_built().dir.join("lil-image.exe")
}

/// The channel's shortcut, `<test root>\profile\Programs\LILITH (Test-<id>).lnk`.
fn shortcut(env: &TestEnv) -> PathBuf {
    env.env.start_menu_shortcut()
}

/// Whether `reg query <key>` finds the key.
fn reg_has(key: &str) -> bool {
    Command::new("reg")
        .args(["query", key])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("cannot run reg: {e}"))
        .success()
}

/// Everything `reg query <key> /s` prints (empty when the key is missing).
fn reg_tree(key: &str) -> String {
    let out = Command::new("reg")
        .args(["query", key, "/s"])
        .output()
        .unwrap_or_else(|e| panic!("cannot run reg: {e}"));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Everything under the test channel's registry root.
fn reg_dump(env: &TestEnv) -> String {
    reg_tree(&env.env.registry_root())
}

#[test] // spec: APP-022, QA-060
fn shortcut_problem_then_fixed_after_yes() {
    let env = TestEnv::new();
    let lnk = shortcut(&env);

    let no = doctor(&env, "n");
    assert_eq!(no.code, 1, "{no:?}");
    assert_eq!(lines(&no, "problem:", "shortcut").len(), 1, "{no:?}");
    assert!(no.stderr.contains("[y/N]"), "the question is asked: {no:?}");
    assert!(!lnk.exists(), "`n` changes nothing");
    assert_eq!(shortcut_state(&env.env, &app_exe()), ShortcutState::Missing);

    let yes = doctor(&env, "y");
    assert_eq!(lines(&yes, "fixed:", "shortcut").len(), 1, "{yes:?}");
    assert!(lines(&yes, "problem:", "shortcut").is_empty(), "{yes:?}");
    let info = read_shortcut(&lnk).unwrap_or_else(|e| panic!("{}: {e}", lnk.display()));
    assert_eq!(info.target, app_exe(), "{info:?}");
    assert_eq!(info.aumid, env.env.notification_aumid(), "{info:?}");
    assert_eq!(shortcut_state(&env.env, &app_exe()), ShortcutState::Ok);

    let again = doctor(&env, "n");
    assert_eq!(lines(&again, "ok:", "shortcut").len(), 1, "{again:?}");
    assert!(
        lines(&again, "problem:", "shortcut").is_empty(),
        "{again:?}"
    );
}

#[test] // spec: APP-020, APP-021, QA-060
fn handlers_missing_then_registered_after_yes() {
    let env = TestEnv::new();
    let scheme = env.env.activation_scheme();

    let no = doctor(&env, "n");
    assert_eq!(lines(&no, "problem:", "handler").len(), 1, "{no:?}");
    assert_eq!(handler_state(&env.env, &app_exe()), HandlerState::Missing);

    let yes = doctor(&env, "y");
    assert_eq!(lines(&yes, "fixed:", "handler").len(), 1, "{yes:?}");
    assert_eq!(handler_state(&env.env, &app_exe()), HandlerState::Ok);
    assert!(
        reg_has(&format!(r"{}\{scheme}", env.env.classes_root())),
        "under the test classes root"
    );
    // A test channel never writes the shell's classes root (QA-040).
    assert!(!reg_has(&format!(r"HKCU\Software\Classes\{scheme}")));

    let again = doctor(&env, "n");
    assert_eq!(lines(&again, "ok:", "handler").len(), 1, "{again:?}");
    assert!(lines(&again, "problem:", "handler").is_empty(), "{again:?}");
}

#[test] // spec: APP-020, APP-021
fn handlers_held_by_another_channel_are_a_warning_and_kept() {
    let env = TestEnv::new();
    let other = env.root().join(r"other build\lil-image.exe");
    register_handlers(&env.env, &other).unwrap();
    let classes = env.env.classes_root();
    let keys = [
        format!(r"{classes}\{}", env.env.activation_scheme()),
        format!(r"{classes}\.lilref"),
        format!(r"{classes}\LILITH.lilref.{}", env.env.channel()),
    ];
    let before: Vec<String> = keys.iter().map(|k| reg_tree(k)).collect();
    assert!(before.iter().all(|t| !t.is_empty()), "{before:?}");

    let out = doctor(&env, "y");
    // One for the scheme, one for `.lilref`.
    let warned = lines(&out, "warning:", "handler");
    assert_eq!(warned.len(), 2, "{out:?}");
    for line in &warned {
        assert!(line.contains(&other.display().to_string()), "{out:?}");
        assert!(line.contains("another LILITH channel or build"), "{out:?}");
    }
    assert!(lines(&out, "problem:", "handler").is_empty(), "{out:?}");
    assert!(lines(&out, "fixed:", "handler").is_empty(), "{out:?}");
    assert_eq!(
        handler_state(&env.env, &app_exe()),
        HandlerState::OtherChannel(other)
    );
    // The handlers' keys are as they were.
    let after: Vec<String> = keys.iter().map(|k| reg_tree(k)).collect();
    assert_eq!(after, before);
}

#[test] // spec: APP-022, P4a-R26
fn non_interactive_never_writes() {
    let env = TestEnv::new();

    // A `y` waiting on stdin is never read.
    let out = env.lil_with_stdin(&["--non-interactive", "doctor"], &"y\n".repeat(20));
    assert_eq!(out.code, 1, "{out:?}");
    assert!(!out.stderr.contains("[y/N]"), "nothing is asked: {out:?}");
    let shortcut_line = lines(&out, "problem:", "shortcut");
    assert_eq!(shortcut_line.len(), 1, "{out:?}");
    assert!(shortcut_line[0].contains("in a terminal"), "{out:?}");
    assert_eq!(lines(&out, "problem:", "handler").len(), 1, "{out:?}");
    assert!(!shortcut(&env).exists());
    assert_eq!(handler_state(&env.env, &app_exe()), HandlerState::Missing);
    // The root itself names the test's owner; nothing is below it.
    assert!(
        !reg_has(&env.env.classes_root()),
        "nothing was written under the test classes root"
    );
}

/// `reg add <key> /ve /d <value> /f`: the key's default value.
fn reg_default(key: &str, value: &str) {
    let ok = Command::new("reg")
        .args(["add", key, "/ve", "/d", value, "/f"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("cannot run reg: {e}"))
        .success();
    assert!(ok, "reg add {key}");
}

#[test] // spec: APP-020, APP-021
fn a_missing_own_scheme_is_offered_while_another_channel_holds_lilref() {
    let env = TestEnv::new();
    let classes = env.env.classes_root();
    let other = env.root().join(r"stable\lil-image.exe");
    reg_default(&format!(r"{classes}\.lilref"), "LILITH.lilref.stable");
    reg_default(
        &format!(r"{classes}\LILITH.lilref.stable\shell\open\command"),
        &format!("\"{}\" \"%1\"", other.display()),
    );
    let lilref = reg_tree(&format!(r"{classes}\.lilref"));

    let no = doctor(&env, "n");
    let missing = lines(&no, "problem:", "link handler");
    assert_eq!(missing.len(), 1, "{no:?}");
    assert!(!missing[0].contains(".lilref"), "{no:?}");
    let kept = lines(&no, "warning:", ".lilref file handler");
    assert_eq!(kept.len(), 1, "{no:?}");
    assert!(kept[0].contains(&other.display().to_string()), "{no:?}");

    let yes = doctor(&env, "y");
    assert_eq!(lines(&yes, "fixed:", "link handler").len(), 1, "{yes:?}");
    let parts = handler_parts(&env.env, &app_exe());
    assert_eq!(parts.scheme, HandlerPart::Ok);
    assert_eq!(parts.lilref, HandlerPart::OtherBuild(other));
    assert_eq!(reg_tree(&format!(r"{classes}\.lilref")), lilref);
}

#[test] // spec: APP-021
fn a_lilref_another_program_holds_is_a_warning_naming_it() {
    let env = TestEnv::new();
    let classes = env.env.classes_root();
    reg_default(&format!(r"{classes}\.lilref"), "txtfile");

    let out = doctor(&env, "y");
    let kept = lines(&out, "warning:", ".lilref file handler");
    assert_eq!(kept.len(), 1, "{out:?}");
    assert!(kept[0].contains("another program (txtfile)"), "{out:?}");
    let parts = handler_parts(&env.env, &app_exe());
    assert_eq!(
        parts.scheme,
        HandlerPart::Ok,
        "the missing scheme was written"
    );
    assert_eq!(parts.lilref, HandlerPart::Foreign("txtfile".into()));
}

#[test] // spec: APP-022
fn a_shortcut_to_another_build_is_repaired_after_yes() {
    let env = TestEnv::new();
    let other = env.root().join(r"other build\lil-image.exe");
    write_shortcut(&env.env, &other).unwrap();
    assert_eq!(
        shortcut_state(&env.env, &app_exe()),
        ShortcutState::WrongTarget
    );

    let no = doctor(&env, "n");
    let wrong = lines(&no, "problem:", "shortcut");
    assert_eq!(wrong.len(), 1, "{no:?}");
    assert!(wrong[0].contains("doesn't open"), "{no:?}");
    assert!(
        no.stderr.contains("Repair the Start-menu shortcut? [y/N]"),
        "{no:?}"
    );
    assert_eq!(read_shortcut(&shortcut(&env)).unwrap().target, other);

    let yes = doctor(&env, "y");
    let fixed = lines(&yes, "fixed:", "shortcut");
    assert_eq!(fixed.len(), 1, "{yes:?}");
    assert!(fixed[0].contains("is repaired"), "{yes:?}");
    assert_eq!(read_shortcut(&shortcut(&env)).unwrap().target, app_exe());
    assert_eq!(shortcut_state(&env.env, &app_exe()), ShortcutState::Ok);
}

#[test] // spec: QA-060
fn a_second_run_changes_nothing() {
    let env = TestEnv::new();
    let first = doctor(&env, "y");
    assert_eq!(first.code, 0, "{first:?}");
    let lnk = shortcut(&env);
    let bytes = std::fs::read(&lnk).unwrap();
    let modified = std::fs::metadata(&lnk).unwrap().modified().unwrap();
    let keys = reg_dump(&env);

    let second = doctor(&env, "y");
    assert_eq!(second.code, 0, "{second:?}");
    assert!(
        !second.stderr.contains("[y/N]"),
        "nothing is asked: {second:?}"
    );
    for kind in ["fixed:", "problem:", "warning:"] {
        assert!(lines(&second, kind, "").is_empty(), "{second:?}");
    }
    assert_eq!(std::fs::read(&lnk).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&lnk).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(reg_dump(&env), keys);
}

// ---------------------------------------------------------------------------------------------
// ComfyUI: the environment changes, the asset catalogue and the model report.

/// `[engines.comfyui]` attached to `fake`.
fn attach(env: &TestEnv, fake: &FakeComfy) {
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\n",
        fake.port(),
        fake.root().display()
    ));
}

/// `lil call <tool>`'s structured result.
fn call(env: &TestEnv, tool: &str) -> Value {
    let out = env.lil(&["call", tool]);
    match serde_json::from_str::<Outcome<ToolOutput>>(out.stdout.trim()) {
        Ok(Outcome::Ok(output)) => output.structured,
        other => panic!("{tool}: {other:?} from {out:?}"),
    }
}

/// Polls `f` every 100 ms for up to 60 s.
fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "{what}: not reached in time");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The `comfyui` entry of `engines.status`.
fn comfyui(env: &TestEnv) -> Value {
    let status = call(env, "engines.status");
    status["engines"]
        .as_array()
        .and_then(|e| e.iter().find(|e| e["engine"] == "comfyui"))
        .cloned()
        .unwrap_or_else(|| panic!("no comfyui in {status}"))
}

/// `custom_nodes\spec_node\pyproject.toml` at `version` under the fake's root.
fn node_version(fake: &FakeComfy, version: &str) {
    lil_testkit::files::write_file(
        &fake.root().join(r"custom_nodes\spec_node\pyproject.toml"),
        format!("[project]\nname = \"spec-node\"\nversion = \"{version}\"\n").as_bytes(),
    );
}

/// The number of stored captures `engines.environment` shows (at most 2).
fn captures(env: &TestEnv) -> usize {
    call(env, "engines.environment")["captures"]
        .as_array()
        .map_or(0, Vec::len)
}

#[test] // spec: CORE-130
fn engine_changes_warn_with_the_rollback_hint() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    node_version(&fake, "1.0.0");
    {
        let _core = CoreFixture::start(&env);
        poll("the first capture", || (captures(&env) == 1).then_some(()));
    }
    node_version(&fake, "1.0.1");
    let _core = CoreFixture::start(&env);
    poll("two captures", || (captures(&env) == 2).then_some(()));

    let out = doctor(&env, "n");
    let changed = lines(&out, "warning:", "spec_node");
    assert_eq!(changed.len(), 1, "{out:?}");
    assert!(
        changed[0].contains("1.0.0") && changed[0].contains("1.0.1"),
        "{out:?}"
    );
    assert!(changed[0].ends_with(HINT), "{out:?}");
    // Dated by the newer capture, so the repeat on the next run reads as the same change.
    assert!(
        changed[0].starts_with("warning: ComfyUI's environment changed on 20"),
        "{out:?}"
    );
}

#[test] // spec: ARCH-065
fn asset_catalogue_lines() {
    for (status, body, want) in [
        (200, json!({"assets": true}), "on"),
        (200, json!({"assets": false}), "off"),
        (404, json!({"error": "not found"}), "unknown"),
    ] {
        let env = TestEnv::new();
        let fake = FakeComfy::start();
        fake.set_features(status, body);
        attach(&env, &fake);
        let _core = CoreFixture::start(&env);
        poll("the catalogue probe", || {
            let probed = fake
                .requests()
                .iter()
                .any(|r| r.path.ends_with("/features"));
            (probed && comfyui(&env)["asset_catalogue"] == want).then_some(())
        });

        let out = doctor(&env, "n");
        let shown: Vec<&str> = out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|l| CATALOGUE.contains(l))
            .collect();
        let expected = CATALOGUE
            .iter()
            .find(|l| l.ends_with(&format!("is {want}")))
            .unwrap();
        assert_eq!(shown, [*expected], "{want}: {out:?}");
    }
}

/// A safetensors file: the 8-byte little-endian header length, then the JSON header.
fn safetensors(header: &Value) -> Vec<u8> {
    let json = header.to_string();
    let mut bytes = (json.len() as u64).to_le_bytes().to_vec();
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

#[test] // spec: CORE-164
fn models_report_counts_are_info() {
    let env = TestEnv::new();
    let ws = env.make_workspace("models");
    env.register_workspace(&ws, true);
    let absent = "ab".repeat(32);
    lil_testkit::files::write_file(
        &ws.join(r"registry\models.yaml"),
        format!("- id: absent\n  name: Absent model\n  files:\n    - sha256: {absent}\n")
            .as_bytes(),
    );
    let fake = FakeComfy::start();
    attach(&env, &fake);
    let models = fake.root().join("models");
    let family = safetensors(&json!({"__metadata__": {"modelspec.architecture": "spec-xl"}}));
    for (path, bytes) in [
        (r"checkpoints\a.safetensors", family.clone()),
        (r"checkpoints\a_copy.safetensors", family),
        (
            r"loras\plain.safetensors",
            safetensors(&json!({"__metadata__": {}})),
        ),
    ] {
        lil_testkit::files::write_file(&models.join(path), &bytes);
    }
    let _core = CoreFixture::start(&env);
    poll("the inventory", || {
        let report = call(&env, "models.report");
        (report["models"].as_array().is_some_and(|m| m.len() == 3)).then_some(())
    });

    let out = doctor(&env, "n");
    let counts = lines(&out, "info:", "models:");
    assert_eq!(counts.len(), 1, "{out:?}");
    let line = counts[0];
    for part in [
        "3 files",
        "3 orphan",
        "2 duplicate",
        "1 unknown family",
        "1 missing",
    ] {
        assert!(line.contains(part), "{part}: {out:?}");
    }
    assert!(
        lines(&out, "problem:", "model").is_empty(),
        "counts are never a problem: {out:?}"
    );
}

#[test] // spec: ARCH-065, CORE-130
fn without_a_core_the_comfyui_lines_say_so_and_start_none() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);

    let out = doctor(&env, "n");
    assert_eq!(lines(&out, "info:", "no core runs").len(), 1, "{out:?}");
    for line in CATALOGUE {
        assert!(!out.stdout.contains(line), "{out:?}");
    }
    let probe = env.lil(&["--no-start", "status"]);
    assert_eq!(probe.code, 3, "no core was started: {probe:?}");
}

#[test] // spec: ARCH-065, CORE-130
fn a_core_the_workspace_checks_started_is_not_read_yet() {
    let env = TestEnv::new();
    let ws = env.make_workspace("just started");
    env.register_workspace(&ws, true);
    let fake = FakeComfy::start();
    attach(&env, &fake);

    let out = doctor(&env, "n");
    assert_eq!(
        lines(&out, "info:", "the core has just started").len(),
        1,
        "{out:?}"
    );
    for line in CATALOGUE {
        assert!(!out.stdout.contains(line), "{out:?}");
    }
    assert!(lines(&out, "info:", "models:").is_empty(), "{out:?}");
    let probe = env.lil(&["--no-start", "status"]);
    assert_eq!(probe.code, 0, "the workspace checks started it: {probe:?}");
}
