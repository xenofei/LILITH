//! The harness guard (QA-038, QA-040). Integration-test binary so arming the process-global
//! guard cannot affect the unit tests; every test here runs under the same armed guard.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use lil_platform::{Channel, Env, RealLocations, Roots, arm_test_guard};

fn fake_real() -> RealLocations {
    RealLocations {
        roaming: PathBuf::from(r"C:\Users\Fake\AppData\Roaming"),
        local: PathBuf::from(r"C:\Users\Fake\AppData\Local"),
        user_profile: PathBuf::from(r"C:\Users\Fake"),
        registry_root: r"HKCU\Software\Worldwright".to_string(),
        credential_prefix: "Worldwright/".to_string(),
        workspaces: vec![PathBuf::from(r"C:\Users\Fake\Ws")],
    }
}

fn arm() {
    arm_test_guard(fake_real());
    arm_test_guard(fake_real()); // idempotent
}

fn roots(base: &str) -> Roots {
    Roots {
        roaming: PathBuf::from(format!(r"{base}\Roaming")),
        local: PathBuf::from(format!(r"{base}\Local")),
        user_profile: PathBuf::from(format!(r"{base}\Profile")),
    }
}

fn panics(f: impl FnOnce()) -> Option<String> {
    let err = catch_unwind(AssertUnwindSafe(f)).err()?;
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    Some(msg)
}

#[test] // spec: QA-040
fn real_roaming_and_local_worldwright_folders_trip() {
    arm();
    let real = roots(r"C:\Users\Fake\AppData");
    // Test channel under the real roots: names start with `Worldwright`.
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        Roots {
            roaming: fake_real().roaming,
            local: fake_real().local,
            user_profile: real.user_profile,
        },
        "S-1".into(),
    );
    for f in [
        (|e: &Env| drop(e.data_dir())) as fn(&Env),
        |e| drop(e.local_dir()),
        |e| drop(e.logs_dir()),
        |e| drop(e.claude_profiles_dir()),
    ] {
        let msg = panics(|| f(&env)).expect("must panic");
        assert!(
            msg.contains("test resolved a real Worldwright location"),
            "{msg}"
        );
    }
}

#[test] // spec: QA-040
fn temp_style_paths_under_the_real_local_root_do_not_trip() {
    arm();
    // %TEMP% lives under %LOCALAPPDATA%\Temp, which is not a Worldwright* folder.
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        Roots {
            roaming: PathBuf::from(r"C:\Users\Fake\AppData\Local\Temp\x\roaming"),
            local: PathBuf::from(r"C:\Users\Fake\AppData\Local\Temp\x\local"),
            user_profile: PathBuf::from(r"C:\Users\Fake\AppData\Local\Temp\x\profile"),
        },
        "S-1".into(),
    );
    assert!(env.data_dir().ends_with("Worldwright-test-abc"));
    assert!(env.logs_dir().ends_with("logs"));
}

#[test] // spec: QA-040
fn real_claude_default_profile_trips() {
    arm();
    // `user_profile` equal to the real profile: `<profile>\.claude` is the real Claude profile.
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        Roots {
            roaming: PathBuf::from(r"D:\Scratch\Roaming"),
            local: PathBuf::from(r"D:\Scratch\Local"),
            user_profile: fake_real().user_profile,
        },
        "S-1".into(),
    );
    let msg = panics(|| drop(env.claude_default_profile_dir())).expect("must panic");
    assert!(msg.contains("real Worldwright location"), "{msg}");
    assert!(msg.contains(".claude"), "{msg}");
}

#[test] // spec: QA-040
fn temp_style_profile_roots_under_the_real_profile_do_not_trip() {
    arm();
    // %TEMP% is under the real profile (`...\AppData\Local\Temp`): only `<profile>\.claude`
    // is real, not the rest of the profile.
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        Roots {
            roaming: PathBuf::from(r"D:\Scratch\Roaming"),
            local: PathBuf::from(r"D:\Scratch\Local"),
            user_profile: PathBuf::from(r"C:\Users\Fake\AppData\Local\Temp\x\profile"),
        },
        "S-1".into(),
    );
    assert!(env.claude_default_profile_dir().ends_with(".claude"));
    let real = fake_real();
    assert!(real.is_real_path(std::path::Path::new(r"C:\Users\Fake\.claude")));
    assert!(real.is_real_path(std::path::Path::new(r"c:\users\fake\.CLAUDE\projects\x")));
    assert!(!real.is_real_path(std::path::Path::new(r"C:\Users\Fake")));
    assert!(!real.is_real_path(std::path::Path::new(r"C:\Users\Fake\Documents\.claude")));
    assert!(!real.is_real_path(std::path::Path::new(r"C:\Users\Fake\.claude-other")));
}

#[test] // spec: QA-040
fn worldwright_local_component_trips() {
    arm();
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        roots(r"D:\Scratch"),
        "S-1".into(),
    )
    .with_volume_resolver(std::sync::Arc::new(OtherVolume));
    let msg = panics(|| {
        let _ = env.workspace_local_dir_for_canonical(std::path::Path::new(r"E:\ws"));
    })
    .expect("must panic");
    assert!(msg.contains("real Worldwright location"), "{msg}");
    assert!(msg.contains(".worldwright-local"), "{msg}");
}

/// Puts every path on its own drive so the workspace lands in `<volume>\.worldwright-local`.
#[derive(Debug)]
struct OtherVolume;
impl lil_platform::VolumeResolver for OtherVolume {
    fn volume_root(&self, path: &std::path::Path) -> std::io::Result<PathBuf> {
        let s = path.to_string_lossy();
        Ok(PathBuf::from(format!("{}:\\", s[..1].to_ascii_uppercase())))
    }
}

#[test] // spec: QA-040
fn constructing_stable_or_dev_after_arming_panics() {
    arm();
    for ch in [Channel::Stable, Channel::Dev] {
        let msg = panics(|| {
            let _ = Env::with_roots(ch.clone(), roots(r"D:\Scratch"), "S-1".into());
        })
        .unwrap_or_else(|| panic!("{ch} must panic"));
        assert!(msg.contains("real Worldwright location"), "{msg}");
    }
    let msg = panics(|| {
        let _ = Env::from_system(Channel::Stable);
    })
    .expect("from_system(Stable) must panic");
    assert!(msg.contains("real Worldwright location"), "{msg}");
}

#[test] // spec: QA-040
fn test_scoped_names_do_not_trip_and_real_ones_do() {
    arm();
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        roots(r"D:\Scratch"),
        "S-1".into(),
    );
    assert_eq!(env.registry_root(), r"HKCU\Software\Worldwright-test-abc");
    assert_eq!(env.credential_prefix(), "Worldwright-test-abc/");

    let real = fake_real();
    assert!(real.is_real_registry(r"HKCU\Software\Worldwright"));
    assert!(real.is_real_registry(r"hkcu\software\WORLDWRIGHT-dev"));
    assert!(real.is_real_registry(r"HKCU\Software\Worldwright\Sub"));
    assert!(!real.is_real_registry(r"HKCU\Software\Worldwright-test-abc"));
    assert!(!real.is_real_registry(r"HKCU\Software\Other"));
    assert!(real.is_real_credential("Worldwright/"));
    assert!(real.is_real_credential("Worldwright-dev/"));
    assert!(real.is_real_credential("worldwright/token"));
    assert!(!real.is_real_credential("Worldwright-test-abc/"));
    assert!(!real.is_real_credential("Other/"));
}

#[test] // spec: QA-040
fn real_path_matching_is_by_prefix_not_substring_and_case_insensitive() {
    let real = fake_real();
    assert!(real.is_real_path(std::path::Path::new(
        r"c:\users\fake\appdata\roaming\worldwright"
    )));
    assert!(real.is_real_path(std::path::Path::new(
        r"\\?\C:\Users\Fake\AppData\Local\Worldwright-dev\logs"
    )));
    assert!(real.is_real_path(std::path::Path::new(r"D:\.WorldWright-Local\abc")));
    assert!(!real.is_real_path(std::path::Path::new(
        r"C:\Users\Fake\AppData\Local\Temp\Worldwright\x"
    )));
    assert!(!real.is_real_path(std::path::Path::new(r"C:\Elsewhere\Worldwright-test-abc")));
    assert!(!real.is_real_path(std::path::Path::new(
        r"C:\Users\Fake\AppData\Roaming2\Worldwright"
    )));
}

#[test] // spec: QA-040
fn registered_real_workspace_trips_guard() {
    arm();
    let ws = PathBuf::from(r"C:\Users\Fake\Ws");
    for inside in [
        ws.clone(),
        ws.join("projects"),
        PathBuf::from(r"c:\users\fake\WS\Projects"),
        PathBuf::from(r"\\?\C:\Users\Fake\Ws\projects"),
    ] {
        let msg = panics(|| {
            let _ = lil_platform::guard::check_workspace(&inside);
        })
        .unwrap_or_else(|| panic!("{} must panic", inside.display()));
        assert!(msg.contains("real Worldwright location"), "{msg}");
    }
    for outside in [
        PathBuf::from(r"C:\Users\Fake\Other"),
        PathBuf::from(r"C:\Users\Fake\Ws2"),
        PathBuf::from(r"C:\Users\Fake"),
    ] {
        let got = lil_platform::guard::check_workspace(&outside);
        assert_eq!(got, outside);
    }
}
