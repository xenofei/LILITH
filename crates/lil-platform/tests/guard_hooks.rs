//! The guard hooks in `Env` itself (QA-038, QA-040): each name method must call the guard.
//!
//! A Stable `Env` is built before the guard is armed (it is impossible afterwards), then the
//! guard is armed with matching fake real locations. Each test asserts one method panics, so
//! deleting a single `guard::check_*` call in `env.rs` fails exactly one test here. This is its
//! own test binary, so arming cannot leak into other suites under plain `cargo test`.

use std::path::PathBuf;
use std::sync::OnceLock;

use lil_platform::{Channel, Env, RealLocations, Roots, arm_test_guard};

/// A Stable `Env` built before arming, then the guard armed. Built once, under the
/// `OnceLock`, so test threads never see "armed but not built".
fn stable_env() -> &'static Env {
    static ENV: OnceLock<Env> = OnceLock::new();
    ENV.get_or_init(|| {
        let roots = Roots {
            roaming: PathBuf::from(r"C:\Users\Fake\AppData\Roaming"),
            local: PathBuf::from(r"C:\Users\Fake\AppData\Local"),
            user_profile: PathBuf::from(r"C:\Users\Fake"),
        };
        let env = Env::with_roots(Channel::Stable, roots.clone(), "S-1-5-21-1-2-3-1001".into());
        arm_test_guard(RealLocations {
            roaming: roots.roaming,
            local: roots.local,
            programs: roots.user_profile.join("Programs"),
            user_profile: roots.user_profile,
            registry_root: r"HKCU\Software\LILITH".to_string(),
            credential_prefix: "LILITH/".to_string(),
            workspaces: Vec::new(),
        });
        env
    })
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn registry_root_hook_trips() {
    let _ = stable_env().registry_root();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn credential_prefix_hook_trips() {
    let _ = stable_env().credential_prefix();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn pipe_name_hook_trips() {
    let _ = stable_env().pipe_name();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn notification_aumid_hook_trips() {
    let _ = stable_env().notification_aumid();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn claude_default_profile_dir_hook_trips() {
    let _ = stable_env().claude_default_profile_dir();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn activation_scheme_hook_trips() {
    let _ = stable_env().activation_scheme();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn classes_root_hook_trips() {
    let _ = stable_env().classes_root();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn start_menu_shortcut_hook_trips() {
    let _ = stable_env().start_menu_shortcut();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn toast_activator_clsid_hook_trips() {
    let _ = stable_env().toast_activator_clsid();
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn app_pipe_name_hook_trips() {
    let _ = stable_env().app_pipe_name("image", false);
}

#[test] // spec: QA-040
#[should_panic(expected = "real LILITH location")]
fn legacy_data_dir_hook_trips() {
    let _ = stable_env().legacy_data_dir();
}

/// The desktop group's remove and watch helpers refuse a stable or dev channel before they
/// resolve a single name (I2 of the plan-4a Task 5 review): with the guard armed, any name
/// this stable `Env` resolved would panic, so an `InvalidInput` error, and no panic, shows the
/// refusal came first and nothing was read, written or deleted.
#[test] // spec: QA-038, QA-040
fn remove_helpers_refuse_a_real_channel_before_touching_anything() {
    use lil_platform::desktop::{
        Toast, clear_toast_history, leftover_registrations, post_watched,
        remove_aumid_registration, remove_clsid_registration, remove_notification_settings,
        remove_shortcut, remove_test_registry, unregister_handlers,
    };
    let env = stable_env();
    let refused = |what: &str, got: std::io::Result<()>| {
        let err = got.expect_err(what);
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidInput,
            "{what}: {err}"
        );
        assert!(
            err.to_string().contains("only on a test channel"),
            "{what}: {err}"
        );
    };
    refused("unregister_handlers", unregister_handlers(env));
    refused("remove_shortcut", remove_shortcut(env));
    refused("remove_clsid_registration", remove_clsid_registration(env));
    refused("clear_toast_history", clear_toast_history(env));
    refused("remove_test_registry", remove_test_registry(env));
    refused("remove_aumid_registration", remove_aumid_registration(env));
    refused(
        "remove_notification_settings",
        remove_notification_settings(env),
    );
    refused(
        "leftover_registrations",
        leftover_registrations(env).map(drop),
    );
    let toast = Toast {
        tag: "t".into(),
        group: "g".into(),
        xml: "<toast/>".into(),
        silent: true,
    };
    let err = post_watched(env, &toast).err().expect("post_watched");
    assert!(err.contains("only on a test channel"), "{err}");
}
