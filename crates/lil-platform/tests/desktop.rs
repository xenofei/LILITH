//! The desktop layer (APP-013, APP-014, APP-022, ARCH-015, ARCH-127, CORE-160, QA-038, QA-042;
//! Appendix G "Notifications" and "Activation and registration").
//!
//! Most tests here touch nothing on the desktop: the command-line split, `FakeDesktop`, the
//! desktop gate (in a child process) and the notifier-setting map. The desktop group
//! (`desktop_*`, `#[ignore = "desktop"]`) writes a real shortcut, real registry keys under a
//! test channel's own names, posts real toasts or holds a real power request. Each starts with
//! `require_desktop_group`, so it runs only through `lil dev desktop-tests` (P4a-R29, P4a-R39),
//! and each removes what it wrote on every path, through a guard that cleans up on drop.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lil_platform::desktop::{
    Desktop, Fullscreen, HandlerState, NotifierSetting, ShortcutState, Toast, ToastsState,
    split_first_argument, toasts_state_for,
};
use lil_platform::desktop_fake::{FakeDesktop, ToastOp};
use lil_platform::guard::require_desktop_group;
use lil_platform::process::run_hidden_without;
use lil_platform::{Channel, Env, Roots};

// ---------------------------------------------------------------------------------------------
// The raw command line (APP-020's input, P4a-R3).
// ---------------------------------------------------------------------------------------------

#[test] // spec: APP-020, APP-014
fn split_first_argument_keeps_the_rest_untouched() {
    let line = r#""C:\x y\lil-image.exe" "lil-dev:confirm/cf_0123456789abcdef""#;
    assert_eq!(
        split_first_argument(line),
        (
            r"C:\x y\lil-image.exe".to_string(),
            r#""lil-dev:confirm/cf_0123456789abcdef""#.to_string()
        )
    );
    // An unquoted exe ends at the first space or tab; quotes, percent signs, backslashes and
    // inner spaces of the rest are left as they are.
    assert_eq!(
        split_first_argument(r#"lil-image.exe  "a \"b\" %41"  "#),
        (
            "lil-image.exe".to_string(),
            r#""a \"b\" %41"  "#.to_string()
        )
    );
    assert_eq!(
        split_first_argument("lil-image.exe\t\tlil:task/j_0123456789abcdef"),
        (
            "lil-image.exe".to_string(),
            "lil:task/j_0123456789abcdef".to_string()
        )
    );
    assert_eq!(
        split_first_argument("\"C:\\a\\b.exe\"\t\"x\ty\""),
        ("C:\\a\\b.exe".to_string(), "\"x\ty\"".to_string())
    );
    // No argument, and a quoted exe that never closes.
    assert_eq!(
        split_first_argument(r#""C:\a b\c.exe""#),
        (r"C:\a b\c.exe".to_string(), String::new())
    );
    assert_eq!(
        split_first_argument(r#""C:\a b\c.exe x"#),
        (r"C:\a b\c.exe x".to_string(), String::new())
    );
    assert_eq!(split_first_argument(""), (String::new(), String::new()));
}

// ---------------------------------------------------------------------------------------------
// FakeDesktop (QA-066's test fakes).
// ---------------------------------------------------------------------------------------------

fn toast(tag: &str, group: &str, xml: &str, silent: bool) -> Toast {
    Toast {
        tag: tag.to_string(),
        group: group.to_string(),
        xml: xml.to_string(),
        silent,
    }
}

#[test] // spec: APP-022, APP-146, QA-066
fn fake_desktop_records_posts_and_removes() {
    let fake = FakeDesktop::new();
    let xml = concat!(
        r#"<toast launch="lil-test-a:task/j_0123456789abcdef" activationType="protocol">"#,
        r#"<visual><binding template="ToastGeneric"><text>Job &amp; &lt;done&gt;</text>"#,
        r#"<text>test.job · no project · 1 s</text></binding></visual></toast>"#
    );
    fake.post(&toast("j_0123456789abcdef", "job_finished", xml, true))
        .unwrap();
    fake.remove("cf_0123456789abcdef", "confirmation_waiting");

    let records = fake.records();
    assert_eq!(records.len(), 2, "{records:?}");
    let post = &records[0];
    assert_eq!(post.op, ToastOp::Post);
    assert_eq!(post.tag, "j_0123456789abcdef");
    assert_eq!(post.group, "job_finished");
    assert_eq!(post.title.as_deref(), Some("Job & <done>"));
    assert_eq!(post.body.as_deref(), Some("test.job · no project · 1 s"));
    assert_eq!(
        post.launch.as_deref(),
        Some("lil-test-a:task/j_0123456789abcdef")
    );
    assert_eq!(post.silent, Some(true));
    assert_eq!(post.xml.as_deref(), Some(xml));
    let remove = &records[1];
    assert_eq!(remove.op, ToastOp::Remove);
    assert_eq!(
        (remove.tag.as_str(), remove.group.as_str()),
        ("cf_0123456789abcdef", "confirmation_waiting")
    );
    assert_eq!(
        (&remove.title, &remove.silent, &remove.xml),
        (&None, &None, &None)
    );
    fake.clear();
    assert!(fake.records().is_empty());

    // The state it reports is what the test set; each fullscreen read counts as a poll.
    assert_eq!(fake.polls(), 0);
    assert_eq!(fake.fullscreen(), Fullscreen::No);
    fake.set_fullscreen(Fullscreen::D3d);
    assert_eq!(fake.fullscreen(), Fullscreen::D3d);
    assert_eq!(fake.polls(), 2);
    assert_eq!(fake.foreground_pid(), None);
    fake.set_foreground(Some(4242));
    assert_eq!(fake.foreground_pid(), Some(4242));
    assert_eq!(fake.toasts_state(), ToastsState::Ready);
    fake.set_toasts_state(ToastsState::Unavailable("disabled_for_user"));
    assert_eq!(
        fake.toasts_state(),
        ToastsState::Unavailable("disabled_for_user")
    );

    let env = Env::with_roots(
        Channel::Test("fake".into()),
        Roots {
            roaming: PathBuf::from(r"C:\T\roaming"),
            local: PathBuf::from(r"C:\T\local"),
            user_profile: PathBuf::from(r"C:\T\profile"),
        },
        "S-1".into(),
    );
    let exe = Path::new(r"C:\T\lil-image.exe");
    assert_eq!(fake.shortcut_state(&env, exe), ShortcutState::Ok);
    assert_eq!(fake.handler_state(&env, exe), HandlerState::Ok);
    fake.set_shortcut(ShortcutState::WrongClsid);
    fake.set_handlers(HandlerState::OtherChannel(PathBuf::from(r"C:\other.exe")));
    assert_eq!(fake.shortcut_state(&env, exe), ShortcutState::WrongClsid);
    assert_eq!(
        fake.handler_state(&env, exe),
        HandlerState::OtherChannel(PathBuf::from(r"C:\other.exe"))
    );

    assert!(fake.power_request_available());
    assert!(!fake.power_held());
    fake.set_power_request(true).unwrap();
    assert!(fake.power_held());
    fake.set_power_request(false).unwrap();
    assert!(!fake.power_held());
}

// ---------------------------------------------------------------------------------------------
// The desktop gate (P4a-R29, QA-042).
// ---------------------------------------------------------------------------------------------

/// Set by `require_desktop_group_panics_outside_the_sandbox` for its child runs only:
/// `sandbox_only` or `host_ok`, the gate's `host_ok` argument.
const GATE_HELPER_VAR: &str = "LIL_PLATFORM_TEST_DESKTOP_GATE";

/// The child half of `require_desktop_group_panics_outside_the_sandbox`. In a normal test run
/// the variable is unset and this passes without doing anything.
#[test]
fn desktop_gate_helper() {
    match std::env::var(GATE_HELPER_VAR).as_deref() {
        Ok("sandbox_only") => require_desktop_group(false),
        Ok("host_ok") => require_desktop_group(true),
        _ => {}
    }
}

/// Runs `desktop_gate_helper` in a child with only `vars` of the gate's variables set;
/// `Ok(())` when it passed, `Err(stderr)` when it failed.
fn gate(mode: &str, vars: &[(&str, &str)]) -> Result<(), String> {
    let exe = std::env::current_exe().unwrap();
    let args: Vec<&OsStr> = ["desktop_gate_helper", "--exact", "--nocapture"]
        .iter()
        .map(OsStr::new)
        .collect();
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        vec![(GATE_HELPER_VAR.into(), mode.into())];
    env.extend(vars.iter().map(|(k, v)| ((*k).into(), (*v).into())));
    let out = run_hidden_without(
        &exe,
        &args,
        exe.parent().unwrap(),
        &env,
        &["LIL_SANDBOX", "LIL_DESKTOP_HOST", "CI"],
        Duration::from_secs(120),
    )
    .unwrap();
    match out.code {
        Some(0) => Ok(()),
        _ => Err(format!("{}{}", out.stdout, out.stderr)),
    }
}

#[test] // spec: QA-042
fn require_desktop_group_panics_outside_the_sandbox() {
    let refused = |mode: &str, vars: &[(&str, &str)]| {
        let out = gate(mode, vars).expect_err(&format!("{mode} {vars:?} must panic"));
        assert!(
            out.contains("desktop test outside the desktop gate"),
            "{out}"
        );
    };
    // A Sandbox-only test: outside the Sandbox, in CI, and on the host run.
    refused("sandbox_only", &[]);
    refused("sandbox_only", &[("CI", "true")]);
    refused("sandbox_only", &[("LIL_DESKTOP_HOST", "1")]);
    refused("sandbox_only", &[("LIL_SANDBOX", "0")]);
    // A host-allowed test (the toast and foreground tests): not outside both gates, not in CI.
    refused("host_ok", &[]);
    refused("host_ok", &[("CI", "true")]);
    refused("host_ok", &[("LIL_DESKTOP_HOST", "true")]);
    // Where each may run.
    gate("sandbox_only", &[("LIL_SANDBOX", "1")]).unwrap();
    gate("host_ok", &[("LIL_SANDBOX", "1")]).unwrap();
    gate("host_ok", &[("LIL_DESKTOP_HOST", "1")]).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Whether toasts can be posted (Appendix G "Ready").
// ---------------------------------------------------------------------------------------------

#[test] // spec: APP-022, ARCH-055
fn toasts_state_maps_each_notifier_setting() {
    use NotifierSetting::*;
    let cases = [
        (Some(Enabled), false, ToastsState::Ready),
        (
            Some(DisabledForApplication),
            false,
            ToastsState::Unavailable("disabled_for_app"),
        ),
        (
            Some(DisabledByManifest),
            false,
            ToastsState::Unavailable("disabled_for_app"),
        ),
        (
            Some(DisabledForUser),
            false,
            ToastsState::Unavailable("disabled_for_user"),
        ),
        (
            Some(DisabledByGroupPolicy),
            false,
            ToastsState::Unavailable("disabled_by_policy"),
        ),
        (None, false, ToastsState::Unavailable("no_platform")),
        // Elevated: never ready, whatever the setting.
        (Some(Enabled), true, ToastsState::Unavailable("elevated")),
        (
            Some(DisabledForUser),
            true,
            ToastsState::Unavailable("elevated"),
        ),
        (None, true, ToastsState::Unavailable("no_platform")),
    ];
    for (setting, elevated, want) in cases {
        assert_eq!(
            toasts_state_for(setting, elevated),
            want,
            "{setting:?} elevated={elevated}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The desktop group (QA-042; P4a-R29, P4a-R31, P4a-R39). Not run by `cargo nextest` without
// `--run-ignored`; `lil dev desktop-tests` runs them serialised, the toast tests with `--host`.
// ---------------------------------------------------------------------------------------------

/// A test channel with an id of its own (`p4a5<16 hex>`), its roots in a temporary folder.
fn desktop_env(dir: &Path) -> Env {
    let mut id = [0u8; 8];
    getrandom::fill(&mut id).unwrap();
    let id: String = id.iter().map(|b| format!("{b:02x}")).collect();
    Env::with_roots(
        Channel::Test(format!("p4a5{id}")),
        Roots {
            roaming: dir.join("roaming"),
            local: dir.join("local"),
            user_profile: dir.join("profile"),
        },
        "S-1".into(),
    )
}

/// Removes, on drop, everything a desktop test may have written under its test channel: the
/// shortcut, the CLSID registration, the handlers, the test channel's registry root and its
/// toasts. Then checks each is gone (P4a-R39).
struct Cleanup<'a> {
    env: &'a Env,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        use lil_platform::desktop::{
            clear_toast_history, remove_shortcut, remove_test_registry, toast_history_tags,
            unregister_handlers,
        };
        let env = self.env;
        let _ = remove_shortcut(env);
        let _ = unregister_handlers(env);
        let _ = remove_test_registry(env);
        clear_toast_history(env);
        let left = (
            env.start_menu_shortcut().exists(),
            toast_history_tags(env).map(|t| t.len()).unwrap_or(0),
        );
        if !std::thread::panicking() {
            assert_eq!(left, (false, 0), "the desktop test left something behind");
        }
    }
}

/// Waits up to 10 s for `f` to hold.
fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "{what} within 10 s");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The host run's test channel: its shortcut in the real Start-menu Programs folder under the
/// test channel's name, so Windows knows its AUMID (Task 1's S1.2); its registry keys under
/// the test channel's own root.
fn host_env(dir: &Path) -> Env {
    let programs = lil_platform::known_folders::programs().expect("FOLDERID_Programs");
    desktop_env(dir).with_programs_dir(programs)
}

#[test] // spec: APP-022, QA-042
#[ignore = "desktop"]
fn desktop_toast_posts_under_the_shortcut_aumid() {
    require_desktop_group(true);
    let dir = tempfile::tempdir().unwrap();
    let env = host_env(dir.path());
    let _cleanup = Cleanup { env: &env };
    let exe = std::env::current_exe().unwrap();
    lil_platform::desktop::write_shortcut(&env, &exe).unwrap();
    let desktop = lil_platform::desktop::real_desktop(&env);
    eventually("toasts ready", || {
        desktop.toasts_state() == ToastsState::Ready
    });

    let tag = "j_00000000000000a5";
    let xml = format!(
        concat!(
            r#"<toast launch="{}:task/{}" activationType="protocol"><visual>"#,
            r#"<binding template="ToastGeneric"><text>Job finished</text>"#,
            r#"<text>desktop test · no project · 0 s</text></binding></visual></toast>"#
        ),
        env.activation_scheme(),
        tag
    );
    desktop
        .post(&toast(tag, "job_finished", &xml, true))
        .expect("the toast posts");
    eventually("the toast in History", || {
        lil_platform::desktop::toast_history_tags(&env).is_ok_and(|t| t.iter().any(|x| x == tag))
    });
    desktop.remove(tag, "job_finished");
    eventually("the toast gone from History", || {
        lil_platform::desktop::toast_history_tags(&env).is_ok_and(|t| t.is_empty())
    });
}

#[test] // spec: APP-022, ARCH-055, QA-042
#[ignore = "desktop"]
fn desktop_toasts_state_is_ready_on_this_pc() {
    require_desktop_group(true);
    let dir = tempfile::tempdir().unwrap();
    let env = host_env(dir.path());
    let _cleanup = Cleanup { env: &env };
    let exe = std::env::current_exe().unwrap();
    lil_platform::desktop::write_shortcut(&env, &exe).unwrap();
    let desktop = lil_platform::desktop::real_desktop(&env);
    eventually("toasts ready", || {
        desktop.toasts_state() == ToastsState::Ready
    });
}

#[test] // spec: APP-022, QA-038, QA-042
#[ignore = "desktop"]
fn desktop_shortcut_and_handlers_round_trip_under_a_test_scheme() {
    use lil_platform::desktop::{
        read_shortcut, register_handlers, remove_clsid_registration, remove_shortcut,
        unregister_handlers, write_shortcut,
    };
    require_desktop_group(false);
    let dir = tempfile::tempdir().unwrap();
    let env = desktop_env(dir.path());
    let _cleanup = Cleanup { env: &env };
    assert!(
        env.start_menu_shortcut()
            .starts_with(dir.path().join("profile"))
    );
    assert!(
        env.classes_root()
            .starts_with(r"HKCU\Software\LILITH-test-p4a5")
    );
    let desktop = lil_platform::desktop::real_desktop(&env);
    // Long, non-verbatim paths to files that exist, as a deployed exe's would be.
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let base = PathBuf::from(base.to_string_lossy().trim_start_matches(r"\\?\"));
    let exe = base.join("app").join("lil-image.exe");
    let other = base.join("other").join("lil-image.exe");
    for path in [&exe, &other] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    assert_eq!(desktop.shortcut_state(&env, &exe), ShortcutState::Missing);
    assert_eq!(desktop.handler_state(&env, &exe), HandlerState::Missing);

    write_shortcut(&env, &exe).unwrap();
    register_handlers(&env, &exe).unwrap();
    let info = read_shortcut(&env.start_menu_shortcut()).unwrap();
    assert_eq!(info.target, exe);
    assert_eq!(info.aumid, env.notification_aumid());
    assert_eq!(
        info.toast_activator_clsid.as_deref(),
        Some(env.toast_activator_clsid().as_str())
    );
    assert_eq!(desktop.shortcut_state(&env, &exe), ShortcutState::Ok);
    assert_eq!(desktop.handler_state(&env, &exe), HandlerState::Ok);
    assert_eq!(
        desktop.shortcut_state(&env, &other),
        ShortcutState::WrongTarget
    );

    // The CLSID's registration alone gone: the shortcut is wrong (P4a-R37).
    remove_clsid_registration(&env).unwrap();
    assert_eq!(
        desktop.shortcut_state(&env, &exe),
        ShortcutState::WrongClsid
    );
    write_shortcut(&env, &exe).unwrap();
    assert_eq!(desktop.shortcut_state(&env, &exe), ShortcutState::Ok);

    // Another build of the channel holds the handlers.
    register_handlers(&env, &other).unwrap();
    assert_eq!(
        desktop.handler_state(&env, &exe),
        HandlerState::OtherChannel(other.clone())
    );
    register_handlers(&env, &exe).unwrap();

    remove_shortcut(&env).unwrap();
    unregister_handlers(&env).unwrap();
    assert_eq!(desktop.shortcut_state(&env, &exe), ShortcutState::Missing);
    assert_eq!(desktop.handler_state(&env, &exe), HandlerState::Missing);
}

#[test] // spec: ARCH-127, QA-042
#[ignore = "desktop"]
fn desktop_power_request_sets_and_clears() {
    require_desktop_group(false);
    let dir = tempfile::tempdir().unwrap();
    let env = desktop_env(dir.path());
    let desktop = lil_platform::desktop::real_desktop(&env);
    assert!(desktop.power_request_available());
    // Windows' own report (Task 1's S1.7): `powercfg /requests` lists the request under SYSTEM
    // while it is held. It needs an elevated token, which the Sandbox's logon command has.
    let listed = || {
        let powercfg = lil_platform::process::find_on_path("powercfg").expect("powercfg");
        let out = lil_platform::process::run_hidden(
            &powercfg,
            &[OsStr::new("/requests")],
            dir.path(),
            &[],
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(out.code, Some(0), "{out:?}");
        out.stdout.contains(lil_platform::desktop::POWER_REASON)
    };
    desktop.set_power_request(true).unwrap();
    desktop.set_power_request(true).unwrap();
    assert!(listed(), "the request isn't listed while held");
    desktop.set_power_request(false).unwrap();
    assert!(
        !listed(),
        "the request is still listed after it was cleared"
    );
    desktop.set_power_request(false).unwrap();
}
