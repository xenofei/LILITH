//! The process environment on a test channel (ARCH-094, ARCH-095, QA-038, QA-040). Own test
//! binary: building a test-channel `Env` from the process variables arms the process-global
//! guard.

use std::path::PathBuf;

use lil_platform::process_env::{ENV_CHANNEL, ENV_TEST_ROOT, TEST_PIPE_SID};
use lil_platform::{Channel, Env, RealLocations, Roots};

fn test_env(id: &str, root: &std::path::Path) -> Env {
    let channel = format!("test-{id}");
    let root = root.to_string_lossy().into_owned();
    Env::from_vars("dev", &|name| match name {
        ENV_CHANNEL => Some(channel.clone()),
        ENV_TEST_ROOT => Some(root.clone()),
        _ => None,
    })
    .expect("test channel env")
}

#[test] // spec: ARCH-095
fn test_channel_env_uses_root_and_fixed_sid() {
    let dir = tempfile::tempdir().unwrap();
    let env = test_env("ab12", dir.path());
    assert_eq!(env.channel(), &Channel::Test("ab12".into()));
    assert!(
        env.pipe_name()
            .ends_with(&format!("{TEST_PIPE_SID}-test-ab12")),
        "{}",
        env.pipe_name()
    );
    assert!(env.data_dir().starts_with(dir.path().join("roaming")));
    assert!(env.local_dir().starts_with(dir.path().join("local")));
    assert_eq!(env.suite_toml(), env.data_dir().join("suite.toml"));
    assert_eq!(env.suite_state_db(), env.local_dir().join("state.db"));
    assert_eq!(env.install_dir(), None);
}

#[test] // spec: QA-040
#[should_panic(expected = "real Worldwright location")]
fn test_channel_env_arms_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let _env = test_env("cd34", dir.path());
    let real = RealLocations::from_system().expect("known folders");
    let roots = Roots {
        roaming: real.roaming,
        local: real.local,
        user_profile: PathBuf::from(r"D:\Scratch\Profile"),
    };
    let other = Env::with_roots(Channel::Test("ef56".into()), roots, "S-1".into());
    let _ = other.data_dir();
}
