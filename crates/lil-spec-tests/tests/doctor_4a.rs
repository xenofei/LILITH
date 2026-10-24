//! `lil doctor`'s plan-4a checks and `lil confirm-window`: the AUMID shortcut and the channel's
//! `lil:` and `.lilref` handlers, each written only after you type `y` and `ok:` on the next
//! run; the engine-environment warnings and the asset catalogue line; and the confirm window
//! started from `lil` (spec APP-022, APP-049, ARCH-065, CORE-130, CORE-164, QA-060; Appendix G
//! "Activation and registration", "The `lil` commands this plan adds", rulings P4a-R4 and
//! P4a-R26). A test channel writes its shortcut under `<test root>\profile\Programs\` and its
//! handlers under `HKCU\Software\LILITH-test-<id>\Classes`, which the shell never reads. Driven
//! through the real `lil` with the fake `claude` first on `PATH`, `reg query`, `lil-fake-image`
//! and the files on disk (QA-018).

mod p4a;
mod support;

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use lil_testkit::{CoreFixture, FakeComfy, LilOutput, TestEnv, ensure_built};
use p4a::*;
use serde_json::json;
use support::*;

/// `lil doctor` with `answer` typed at every question, the fake `claude` first on `PATH` and the
/// test's Claude Code profile.
fn doctor(env: &TestEnv, answer: &str) -> LilOutput {
    let stdin = format!("{answer}\n").repeat(20);
    let profile = env.env.claude_default_profile_dir();
    std::fs::create_dir_all(&profile).unwrap();
    let extra = [
        ("PATH", env.path_with_testbins()),
        ("CLAUDE_CONFIG_DIR", OsString::from(profile)),
    ];
    let out = lil_with(env, env.root(), &["doctor"], &extra, Some(&stdin));
    assert!(out.code == 0 || out.code == 1, "{out:?}");
    out
}

/// The printed lines that start with `kind` (`ok:`, `problem:`, `warning:`, `info:`) and contain
/// `what`, ignoring case.
fn lines<'a>(out: &'a LilOutput, kind: &str, what: &str) -> Vec<&'a str> {
    let what = what.to_ascii_lowercase();
    out.stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(kind) && l.to_ascii_lowercase().contains(&what))
        .collect()
}

/// The test channel's classes root, removed again when the test ends.
struct TestClasses(String);

impl TestClasses {
    fn new(env: &TestEnv) -> TestClasses {
        TestClasses(format!(r"HKCU\Software\LILITH-test-{}", channel_id(env)))
    }

    /// Whether `reg query` finds `<root>\Classes\<sub>`, and what it printed.
    fn query(&self, sub: &str) -> (bool, String) {
        let key = format!(r"{}\Classes\{sub}", self.0);
        let out = Command::new("reg")
            .args(["query", &key])
            .output()
            .unwrap_or_else(|e| panic!("cannot run reg: {e}"));
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }
}

impl Drop for TestClasses {
    fn drop(&mut self) {
        let _ = Command::new("reg")
            .args(["delete", &self.0, "/f"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[test] // spec: APP-022, QA-060
fn doctor_checks_the_aumid_shortcut_and_repairs_after_yes() {
    let env = TestEnv::new();
    let _classes = TestClasses::new(&env);
    let shortcut = env
        .root()
        .join(r"profile\Programs")
        .join(format!("LILITH (Test-{}).lnk", channel_id(&env)));

    let no = doctor(&env, "n");
    assert!(!lines(&no, "problem:", "shortcut").is_empty(), "{no:?}");
    assert!(!shortcut.exists(), "`n` changes nothing");

    let yes = doctor(&env, "y");
    assert!(
        shortcut.is_file(),
        "{} after y: {yes:?}",
        shortcut.display()
    );

    let again = doctor(&env, "n");
    assert!(!lines(&again, "ok:", "shortcut").is_empty(), "{again:?}");
    assert!(
        lines(&again, "problem:", "shortcut").is_empty(),
        "{again:?}"
    );
}

#[test] // spec: APP-022, QA-060
fn doctor_registers_the_handlers_after_yes() {
    let env = TestEnv::new();
    let classes = TestClasses::new(&env);
    let scheme = scheme(&env);

    let no = doctor(&env, "n");
    assert!(!lines(&no, "problem:", "handler").is_empty(), "{no:?}");
    assert!(!classes.query(&scheme).0, "`n` registers nothing");

    doctor(&env, "y");
    let (found, printed) = classes.query(&format!(r"{scheme}\shell\open\command"));
    assert!(found, "the {scheme} handler after y");
    assert!(printed.contains("\"%1\""), "{printed}");
    let (found, printed) = classes.query(&scheme);
    assert!(found && printed.contains("URL Protocol"), "{printed}");
    let (found, printed) = classes.query(".lilref");
    assert!(found, ".lilref after y");
    assert!(
        printed.contains(&format!("LILITH.lilref.{}", env.env.channel())),
        "{printed}"
    );
    // A test channel never writes the real classes root (QA-040).
    let real = Command::new("reg")
        .args(["query", &format!(r"HKCU\Software\Classes\{scheme}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("cannot run reg: {e}"));
    assert!(!real.success(), "{scheme} reached HKCU\\Software\\Classes");

    let again = doctor(&env, "n");
    assert!(!lines(&again, "ok:", "handler").is_empty(), "{again:?}");
    assert!(lines(&again, "problem:", "handler").is_empty(), "{again:?}");
}

/// `custom_nodes\spec_node\pyproject.toml` at `version` under the fake's root.
fn node_version(fake: &FakeComfy, version: &str) {
    lil_testkit::files::write_file(
        &fake.root().join(r"custom_nodes\spec_node\pyproject.toml"),
        format!("[project]\nname = \"spec-node\"\nversion = \"{version}\"\n").as_bytes(),
    );
}

#[test] // spec: CORE-130, ARCH-065, CORE-164
fn doctor_reports_engine_changes_and_the_asset_catalogue() {
    let env = TestEnv::new();
    let _classes = TestClasses::new(&env);
    let fake = FakeComfy::start();
    attach(&env, &fake);
    node_version(&fake, "1.0.0");
    {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, None);
        poll("the first capture", || {
            let view = ok(&mut pipe, "engines.environment", json!({}), None);
            (view["captures"].as_array().is_some_and(|c| !c.is_empty())).then_some(())
        });
    }
    node_version(&fake, "1.0.1");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    poll("two captures", || {
        let view = ok(&mut pipe, "engines.environment", json!({}), None);
        (view["captures"].as_array().is_some_and(|c| c.len() == 2)).then_some(())
    });

    let out = doctor(&env, "n");
    let hint = "Roll back with ComfyUI-Manager's snapshot restore or comfy-cli's version switch; the suite doesn't restore engines (CORE-166).";
    let changed = lines(&out, "warning:", "spec_node");
    assert_eq!(changed.len(), 1, "{out:?}");
    assert!(changed[0].ends_with(hint), "{out:?}");
    let catalogue: Vec<&str> = out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| {
            [
                "ok: ComfyUI's asset catalogue is off",
                "warning: ComfyUI's asset catalogue is on",
                "info: ComfyUI's asset catalogue is unknown",
            ]
            .contains(l)
        })
        .collect();
    assert_eq!(catalogue.len(), 1, "one asset-catalogue line: {out:?}");
    assert!(
        !lines(&out, "info:", "model").is_empty(),
        "the model report counts: {out:?}"
    );
}

/// `lil.exe` and `lil-fake-image.exe` (as `lil-image.exe`) copied side by side into
/// `<test root>\bin`; returns the copied `lil.exe`.
fn lil_beside_the_app(env: &TestEnv) -> std::path::PathBuf {
    let bin = env.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let bins = ensure_built();
    for (from, to) in [
        (bins.lil.clone(), "lil.exe"),
        (bins.dir.join("lil-fake-image.exe"), "lil-image.exe"),
    ] {
        std::fs::copy(&from, bin.join(to))
            .unwrap_or_else(|e| panic!("cannot copy {} as {to}: {e}", from.display()));
    }
    bin.join("lil.exe")
}

/// Runs `lil` at `exe` with `args` on the test channel; waits for it.
fn run(env: &TestEnv, exe: &Path, args: &[&str]) -> LilOutput {
    let start = std::time::Instant::now();
    let out = Command::new(exe)
        .args(args)
        .current_dir(env.root())
        .envs(env.child_env())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", exe.display()));
    LilOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        elapsed: start.elapsed(),
    }
}

#[test] // spec: APP-049
fn confirm_window_starts_the_app_in_confirm_mode() {
    let env = TestEnv::new();
    let lil = lil_beside_the_app(&env);
    let id = "cf_0123456789abcdef";

    let out = run(&env, &lil, &["confirm-window", id]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout
            .trim()
            .starts_with("confirm window started (pid "),
        "{out:?}"
    );
    let recorded = env.root().join("fake-image-args.jsonl");
    let line = poll("lil-fake-image's recorded arguments", || {
        jsonl(&recorded).into_iter().next_back()
    });
    let args = line.get("args").unwrap_or(&line).clone();
    assert_eq!(args, json!(["--confirm", id]), "{line}");

    let refused = run(&env, &lil, &["--non-interactive", "confirm-window", id]);
    assert_eq!(refused.code, 2, "{refused:?}");
    // Appendix G doesn't name the stream the refusal goes to.
    assert!(
        format!("{}{}", refused.stdout, refused.stderr)
            .contains("the confirm window is for interactive use"),
        "{refused:?}"
    );
    assert_eq!(jsonl(&recorded).len(), 1, "nothing more was started");
}
