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
fn legacy_data_dir_hook_trips() {
    let _ = stable_env().legacy_data_dir();
}
