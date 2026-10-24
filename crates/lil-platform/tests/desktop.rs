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
    DESKTOP_TEST_ID_PREFIX, Desktop, Fullscreen, HandlerState, NotifierSetting, ShortcutState,
    Toast, ToastsState, split_first_argument, toasts_state_for,
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
// The names a killed desktop run can leave behind (P4a-R39, Task 30's sweep).
// ---------------------------------------------------------------------------------------------

#[test] // spec: QA-042
fn leftover_names_give_the_desktop_test_id() {
    use lil_platform::desktop::desktop_test_id_in;
    let id = Some("desktop-0123456789abcdef".to_string());
    // The host toast tests' fixed id is swept like any other.
    assert_eq!(
        desktop_test_id_in(&format!("lil-test-{HOST_TOAST_TEST_ID}")).as_deref(),
        Some(HOST_TOAST_TEST_ID)
    );
    for name in [
        "lil-test-desktop-0123456789abcdef",
        "LILITH-test-desktop-0123456789abcdef",
        "LILITH (Test-desktop-0123456789abcdef).lnk",
        "lilith (test-desktop-0123456789ABCDEF).LNK",
        "LILITH.Suite.Test-desktop-0123456789abcdef",
    ] {
        assert_eq!(desktop_test_id_in(name), id, "{name}");
    }
    // The env's own names give the same id back.
    let dir = tempfile::tempdir().unwrap();
    let env = desktop_env(dir.path());
    let Channel::Test(own) = env.channel().clone() else {
        unreachable!()
    };
    let shortcut = env.start_menu_shortcut();
    let shortcut = shortcut.file_name().unwrap().to_string_lossy();
    let root = env.registry_root();
    for name in [
        env.activation_scheme().as_str(),
        root.trim_start_matches(r"HKCU\Software\"),
        &shortcut,
        &env.notification_aumid(),
    ] {
        assert_eq!(
            desktop_test_id_in(name).as_deref(),
            Some(own.as_str()),
            "{name}"
        );
    }
    // Stable, dev, other test channels, other lengths and other characters: none.
    for name in [
        "lil",
        "lil-dev",
        "LILITH",
        "LILITH-dev",
        "LILITH.lnk",
        "LILITH (Dev).lnk",
        "LILITH.Suite.Stable",
        "LILITH.Suite.Dev",
        "LILITH.Suite.Test-0123456789abcdef",
        "lil-test-0123456789abcdef",
        "LILITH-test-a",
        "lil-test-desktop-0123456789abcde",
        "lil-test-desktop-0123456789abcdef0",
        "lil-test-desktop-0123456789abcdeg",
        "lil-test-desktop-0123456789abcdef.lnk",
        "LILITH (Test-desktop-0123456789abcdef)",
        "x-lil-test-desktop-0123456789abcdef",
    ] {
        assert_eq!(desktop_test_id_in(name), None, "{name}");
    }
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
    fake.set_toasts_state(ToastsState::NotYetShown);
    assert_eq!(fake.toasts_state(), ToastsState::NotYetShown);
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
        // Never shown a toast yet (plan 4a ruling 2026-10-24): postable, unless elevated.
        (Some(NotYetShown), false, ToastsState::NotYetShown),
        (
            Some(NotYetShown),
            true,
            ToastsState::Unavailable("elevated"),
        ),
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
    assert!(ToastsState::Ready.can_post() && ToastsState::NotYetShown.can_post());
    assert!(!ToastsState::Unavailable("disabled_for_user").can_post());
}

#[test] // spec: APP-022, ARCH-055
fn not_yet_shown_ends_when_an_ok_post_never_shows() {
    use lil_platform::desktop::{NOT_SHOWN_GRACE, toasts_state_after_post};
    let grace = NOT_SHOWN_GRACE;
    assert_eq!(grace, Duration::from_secs(5));
    let after = |state, since| toasts_state_after_post(state, since);
    // Before any post, and within the grace after one: still postable.
    assert_eq!(
        after(ToastsState::NotYetShown, None),
        ToastsState::NotYetShown
    );
    assert_eq!(
        after(
            ToastsState::NotYetShown,
            Some(grace - Duration::from_millis(1))
        ),
        ToastsState::NotYetShown
    );
    // Still never shown 5 s after an Ok post: no platform, so confirmations answer at once.
    for since in [grace, grace * 10] {
        assert_eq!(
            after(ToastsState::NotYetShown, Some(since)),
            ToastsState::Unavailable("no_platform")
        );
    }
    // Every other reading keeps its meaning.
    for state in [
        ToastsState::Ready,
        ToastsState::Unavailable("disabled_for_user"),
        ToastsState::Unavailable("elevated"),
    ] {
        assert_eq!(after(state, Some(grace * 2)), state);
        assert_eq!(after(state, None), state);
    }
}

// ---------------------------------------------------------------------------------------------
// The desktop group (QA-042; P4a-R29, P4a-R31, P4a-R39). Not run by `cargo nextest` without
// `--run-ignored`; `lil dev desktop-tests` runs them serialised, the toast tests with `--host`.
// ---------------------------------------------------------------------------------------------

/// A test channel with an id of its own, `test-desktop-<16 hex>` (the fixed prefix lets a killed
/// run's leftovers be swept by name), its roots in a temporary folder.
fn desktop_env(dir: &Path) -> Env {
    let mut id = [0u8; 8];
    getrandom::fill(&mut id).unwrap();
    let id: String = id.iter().map(|b| format!("{b:02x}")).collect();
    env_with_id(&format!("{DESKTOP_TEST_ID_PREFIX}{id}"), dir)
}

/// The test channel `test-<id>`, its roots in `dir`.
fn env_with_id(id: &str, dir: &Path) -> Env {
    Env::with_roots(
        Channel::Test(id.to_string()),
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
/// toasts. Then checks that each is gone (P4a-R39): the shortcut file and its state, the
/// handlers' state, every registry key (`leftover_registrations`) and the notification
/// centre's History, where a read error is a failure too.
struct Cleanup<'a> {
    env: &'a Env,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        use lil_platform::desktop::{
            clear_toast_history, leftover_registrations, real_desktop,
            remove_notification_settings, remove_shortcut, remove_test_registry,
            toast_history_tags, unregister_handlers,
        };
        let env = self.env;
        let mut removed = vec![
            ("the shortcut", remove_shortcut(env)),
            ("the handlers", unregister_handlers(env)),
            ("the registry root", remove_test_registry(env)),
            ("the toast history", clear_toast_history(env)),
        ];
        // Windows' notification settings entry goes last: the notification platform writes it
        // after a toast (and after the History is cleared), a moment later, so it is removed
        // until it stays gone, within the desktop group's deadline; the check below fails if it
        // never does.
        let deadline = Instant::now() + DESKTOP_DEADLINE;
        let settings_removed = loop {
            if let Err(e) = remove_notification_settings(env) {
                break Err(e);
            }
            std::thread::sleep(Duration::from_millis(500));
            let back = leftover_registrations(env)
                .is_ok_and(|keys| keys.iter().any(|k| k.contains(r"\Notifications\Settings\")));
            if !back || Instant::now() >= deadline {
                break Ok(());
            }
        };
        removed.push(("the notification settings", settings_removed));
        let desktop = real_desktop(env);
        let any_exe = Path::new(r"C:\none\lil-image.exe");
        let mut left: Vec<String> = removed
            .iter()
            .filter_map(|(what, r)| r.as_ref().err().map(|e| format!("removing {what}: {e}")))
            .collect();
        if env.start_menu_shortcut().exists() {
            left.push(format!("{}", env.start_menu_shortcut().display()));
        }
        let shortcut = desktop.shortcut_state(env, any_exe);
        if shortcut != ShortcutState::Missing {
            left.push(format!("shortcut state {shortcut:?}"));
        }
        let handlers = desktop.handler_state(env, any_exe);
        if handlers != HandlerState::Missing {
            left.push(format!("handler state {handlers:?}"));
        }
        match leftover_registrations(env) {
            Ok(keys) => left.extend(keys),
            Err(e) => left.push(format!("reading the registrations: {e}")),
        }
        match toast_history_tags(env) {
            Ok(tags) => left.extend(tags.into_iter().map(|t| format!("toast {t}"))),
            Err(e) => left.push(format!("reading the toast history: {e}")),
        }
        if !std::thread::panicking() {
            assert!(
                left.is_empty(),
                "the desktop test left something behind: {left:?}"
            );
        }
    }
}

/// The desktop group's failure deadline: a shortcut just written, and the History entry after
/// it, can take a while on a busy PC. Never a measurement (QA-031).
const DESKTOP_DEADLINE: Duration = Duration::from_secs(60);

/// Waits up to [`DESKTOP_DEADLINE`] for `f` to hold; a timeout says `context()`.
fn eventually(what: &str, mut f: impl FnMut() -> bool, context: impl Fn() -> String) {
    let deadline = Instant::now() + DESKTOP_DEADLINE;
    while !f() {
        assert!(
            Instant::now() < deadline,
            "{what} within {DESKTOP_DEADLINE:?}; {}",
            context()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The host run's test channel. Its shortcut goes in the real Start-menu Programs folder under
/// the test channel's name, so Windows knows its AUMID (Task 1's S1.2). Its CLSID is registered
/// under the shell's classes root, where COM reads it (Appendix G, P4a-R37), through the
/// desktop group's opt-in, which writes the scheme and the CLSID only, never `.lilref`.
///
/// The host toast tests share one fixed id per PC, [`HOST_TOAST_TEST_ID`] (plan 4a ruling
/// 2026-10-24): Windows keeps a sender record for every AUMID that ever showed a toast (in
/// Settings > System > Notifications), which nothing documented removes, so a random id per run
/// would grow that list by two every run. With the fixed id it holds at most one test sender.
/// Everything registered under it is still removed and checked gone after each test (`Cleanup`),
/// and the host run's lock keeps two runs off it at once. On a PC whose notification platform
/// never saw this AUMID, the first toast test starts from `NotYetShown`; on later runs Windows
/// knows the sender and `Setting()` may read `Ready` from the start: both tests accept either.
fn host_env(dir: &Path) -> Env {
    let programs = lil_platform::known_folders::programs().expect("FOLDERID_Programs");
    env_with_id(HOST_TOAST_TEST_ID, dir)
        .with_programs_dir(programs)
        .with_shell_classes_root()
}

/// The host toast tests' fixed test channel id, in the desktop group's `desktop-<16 hex>` shape,
/// so a killed run's leftovers are swept like any other.
const HOST_TOAST_TEST_ID: &str = "desktop-00000000000000f1";

#[test] // spec: APP-022, QA-042
#[ignore = "desktop"]
fn desktop_toast_posts_under_the_shortcut_aumid() {
    use lil_platform::desktop::{post_watched, toast_history_tags};
    require_desktop_group(true);
    let dir = tempfile::tempdir().unwrap();
    let env = host_env(dir.path());
    let _cleanup = Cleanup { env: &env };
    let exe = std::env::current_exe().unwrap();
    lil_platform::desktop::write_shortcut(&env, &exe).unwrap();
    let desktop = lil_platform::desktop::real_desktop(&env);
    // A fresh AUMID is `NotYetShown` until its first toast (plan 4a ruling 2026-10-24).
    eventually(
        "toasts postable",
        || desktop.toasts_state().can_post(),
        || format!("toasts_state {:?}", desktop.toasts_state()),
    );

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
    // The toast's own events say why a dropped toast never reached History (P4a-R39).
    let watched =
        post_watched(&env, &toast(tag, "job_finished", &xml, true)).expect("the toast posts");
    let context = || {
        format!(
            "toasts_state {:?}, the toast's events {:?}, History {:?}",
            desktop.toasts_state(),
            watched.events(),
            toast_history_tags(&env)
        )
    };
    eventually(
        "the toast in History",
        || {
            if let Some(failed) = watched.failure() {
                panic!("Windows dropped the toast ({failed}); {}", context());
            }
            toast_history_tags(&env).is_ok_and(|t| t.iter().any(|x| x == tag))
        },
        context,
    );
    // Once a toast has shown, Windows has the AUMID's settings: `Setting()` is `Enabled`.
    eventually(
        "toasts ready after the first toast",
        || desktop.toasts_state() == ToastsState::Ready,
        context,
    );
    desktop.remove(tag, "job_finished");
    eventually(
        "the toast gone from History",
        || toast_history_tags(&env).is_ok_and(|t| t.is_empty()),
        context,
    );
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
    let state = || format!("toasts_state {:?}", desktop.toasts_state());
    // Which one this run started from: NotYetShown on the fixed id's first run on a PC, Ready
    // once Windows knows the sender (shown with --success-output).
    eprintln!(
        "{HOST_TOAST_TEST_ID}: toasts_state at the start {:?}",
        desktop.toasts_state()
    );
    // Before its first toast a fresh AUMID is `NotYetShown`, which may post (plan 4a ruling
    // 2026-10-24); after it, Windows reports the user's real setting, which must be `Enabled`.
    eventually(
        "toasts postable",
        || desktop.toasts_state().can_post(),
        state,
    );
    if desktop.toasts_state() == ToastsState::NotYetShown {
        let tag = "j_00000000000000a6";
        let xml = concat!(
            r#"<toast><visual><binding template="ToastGeneric"><text>Desktop test</text>"#,
            r#"<text>toasts state</text></binding></visual></toast>"#
        );
        desktop
            .post(&toast(tag, "job_finished", xml, true))
            .expect("the first toast posts");
        eventually(
            "toasts ready after the first toast",
            || desktop.toasts_state() == ToastsState::Ready,
            state,
        );
        desktop.remove(tag, "job_finished");
    }
    assert_eq!(desktop.toasts_state(), ToastsState::Ready);
}

/// The shortcut and handlers round trip, in the Sandbox, under both classes roots a test
/// channel can have (Appendix G): first the test channel's own,
/// `HKCU\Software\LILITH-test-<id>\Classes`, with every handler (`.lilref` too); then the
/// desktop group's opt-in into the shell's `HKCU\Software\Classes`, where only the
/// `lil-test-<id>` scheme and the CLSID are written and the `.lilref` association never moves.
#[test] // spec: APP-022, QA-038, QA-042
#[ignore = "desktop"]
fn desktop_shortcut_and_handlers_round_trip_under_a_test_scheme() {
    use lil_platform::desktop::{
        leftover_registrations, read_shortcut, register_handlers, remove_aumid_registration,
        remove_clsid_registration, remove_shortcut, unregister_handlers, write_shortcut,
    };
    require_desktop_group(false);
    let dir = tempfile::tempdir().unwrap();
    let env = desktop_env(&dir.path().join("own"));
    let _cleanup = Cleanup { env: &env };
    assert!(
        env.start_menu_shortcut()
            .starts_with(dir.path().join("own").join("profile"))
    );
    assert!(env.classes_root().starts_with(&format!(
        r"HKCU\Software\LILITH-test-{DESKTOP_TEST_ID_PREFIX}"
    )));
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

    // A shortcut at this channel's path that carries another channel's AUMID.
    let foreign = desktop_env(&dir.path().join("foreign"));
    let _foreign_cleanup = Cleanup { env: &foreign };
    write_shortcut(&foreign, &exe).unwrap();
    std::fs::copy(foreign.start_menu_shortcut(), env.start_menu_shortcut()).unwrap();
    assert_eq!(
        desktop.shortcut_state(&env, &exe),
        ShortcutState::WrongAumid
    );
    write_shortcut(&env, &exe).unwrap();
    assert_eq!(desktop.shortcut_state(&env, &exe), ShortcutState::Ok);

    // The CLSID's registration alone gone: the shortcut is wrong (P4a-R37).
    remove_clsid_registration(&env).unwrap();
    assert_eq!(
        desktop.shortcut_state(&env, &exe),
        ShortcutState::WrongClsid
    );
    write_shortcut(&env, &exe).unwrap();
    assert_eq!(desktop.shortcut_state(&env, &exe), ShortcutState::Ok);
    // The AUMID's registration alone gone: wrong too (plan 4a ruling 2026-10-24).
    remove_aumid_registration(&env).unwrap();
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

    // Opted into the shell's classes root: the scheme and the CLSID only.
    let shell = desktop_env(&dir.path().join("shell")).with_shell_classes_root();
    let _shell_cleanup = Cleanup { env: &shell };
    assert_eq!(shell.classes_root(), r"HKCU\Software\Classes");
    let shell_desktop = lil_platform::desktop::real_desktop(&shell);
    write_shortcut(&shell, &exe).unwrap();
    register_handlers(&shell, &exe).unwrap();
    assert_eq!(
        shell_desktop.shortcut_state(&shell, &exe),
        ShortcutState::Ok
    );
    assert_eq!(shell_desktop.handler_state(&shell, &exe), HandlerState::Ok);
    let scheme = format!(r"HKCU\Software\Classes\{}", shell.activation_scheme());
    let clsid = format!(
        r"HKCU\Software\Classes\CLSID\{}",
        shell.toast_activator_clsid()
    );
    let aumid = format!(
        r"HKCU\Software\Classes\AppUserModelId\{}",
        shell.notification_aumid()
    );
    let mut written = leftover_registrations(&shell).unwrap();
    written.sort();
    let mut want = vec![scheme, clsid, aumid];
    want.sort();
    assert_eq!(
        written, want,
        "only the scheme, the CLSID and the AUMID, never .lilref"
    );
    remove_shortcut(&shell).unwrap();
    unregister_handlers(&shell).unwrap();
    assert_eq!(
        leftover_registrations(&shell).unwrap(),
        Vec::<String>::new()
    );
}

/// A killed run's orphans are found by their `LilTestChannel` marker, not only by name (Task 30's
/// sweep): here the CLSID key and the AUMID key are each left alone, with every other name gone,
/// and the sweep finds and removes them.
#[test] // spec: APP-022, QA-042
#[ignore = "desktop"]
fn desktop_sweep_finds_orphaned_keys_by_their_marker() {
    use lil_platform::desktop::{
        desktop_test_ids_left, leftover_registrations, remove_aumid_registration,
        remove_clsid_registration, sweep_desktop_tests, write_shortcut,
    };
    require_desktop_group(false);
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    for orphan in ["CLSID", "AppUserModelId"] {
        let env = desktop_env(&dir.path().join(orphan)).with_shell_classes_root();
        let _cleanup = Cleanup { env: &env };
        let Channel::Test(id) = env.channel().clone() else {
            unreachable!()
        };
        let lnk = env.start_menu_shortcut();
        let programs = lnk.parent().unwrap().to_path_buf();
        write_shortcut(&env, &exe).unwrap();
        // Every other name gone: the shortcut, and the other key.
        std::fs::remove_file(&lnk).unwrap();
        if orphan == "CLSID" {
            remove_aumid_registration(&env).unwrap();
        } else {
            remove_clsid_registration(&env).unwrap();
        }
        let left = leftover_registrations(&env).unwrap();
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(left[0].contains(&format!(r"\{orphan}\")), "{left:?}");
        assert!(
            desktop_test_ids_left(&programs).unwrap().contains(&id),
            "the orphaned {orphan} key isn't found by its marker"
        );
        let sweep = sweep_desktop_tests(&programs).unwrap();
        assert!(sweep.removed.contains(&left[0]), "{sweep:?}");
        assert!(sweep.remaining.is_empty(), "{sweep:?}");
        assert_eq!(leftover_registrations(&env).unwrap(), Vec::<String>::new());
        assert!(!desktop_test_ids_left(&programs).unwrap().contains(&id));
    }
}

/// The marker only nominates an id: the sweep removes that id's own names, never the key that
/// carries the marker (unless it is one of them).
#[test] // spec: QA-042
#[ignore = "desktop"]
fn desktop_sweep_leaves_a_foreign_marked_key() {
    use lil_platform::desktop::{TEST_MARKER, desktop_test_ids_left, sweep_desktop_tests};
    require_desktop_group(false);
    let dir = tempfile::tempdir().unwrap();
    let env = desktop_env(dir.path()).with_shell_classes_root();
    let _cleanup = Cleanup { env: &env };
    let Channel::Test(id) = env.channel().clone() else {
        unreachable!()
    };
    let programs = dir.path().join("programs");
    let foreign = r"HKCU\Software\Classes\LilForeignMarkedKey";
    let reg = |args: &[&str]| {
        let out = std::process::Command::new("reg")
            .args(args)
            .output()
            .unwrap();
        out.status.success()
    };
    assert!(reg(&["add", foreign, "/v", TEST_MARKER, "/d", &id, "/f"]));
    let found = desktop_test_ids_left(&programs).unwrap().contains(&id);
    let sweep = sweep_desktop_tests(&programs);
    let survived = reg(&["query", foreign]);
    assert!(reg(&["delete", foreign, "/f"]));
    assert!(found, "the marker nominates its id");
    sweep.unwrap();
    assert!(
        survived,
        "the sweep deleted a key that isn't one of the id's names"
    );
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
