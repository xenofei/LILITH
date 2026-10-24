//! The harness guard (QA-038, QA-040). Integration-test binary so arming the process-global
//! guard cannot affect the unit tests; every test here runs under the same armed guard.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use lil_platform::names::{LEGACY_APP_NAME, LEGACY_LOCAL_DIR};
use lil_platform::{Channel, Env, RealLocations, Roots, arm_test_guard};

/// The fake real Start-menu Programs folder (`FOLDERID_Programs`).
const REAL_PROGRAMS: &str = r"C:\Users\Fake\AppData\Roaming\Microsoft\Windows\Start Menu\Programs";

fn fake_real() -> RealLocations {
    RealLocations {
        roaming: PathBuf::from(r"C:\Users\Fake\AppData\Roaming"),
        local: PathBuf::from(r"C:\Users\Fake\AppData\Local"),
        user_profile: PathBuf::from(r"C:\Users\Fake"),
        programs: PathBuf::from(REAL_PROGRAMS),
        registry_root: r"HKCU\Software\LILITH".to_string(),
        credential_prefix: "LILITH/".to_string(),
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
fn real_roaming_and_local_lilith_folders_trip() {
    arm();
    let real = roots(r"C:\Users\Fake\AppData");
    // Test channel under the real roots: names start with `LILITH`, and the legacy readers
    // with the legacy name.
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
        |e| drop(e.legacy_data_dir()),
    ] {
        let msg = panics(|| f(&env)).expect("must panic");
        assert!(
            msg.contains("test resolved a real LILITH location"),
            "{msg}"
        );
    }
}

#[test] // spec: QA-040
fn temp_style_paths_under_the_real_local_root_do_not_trip() {
    arm();
    // %TEMP% lives under %LOCALAPPDATA%\Temp, which is not a `LILITH*` folder
    // (or a legacy-named one).
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        Roots {
            roaming: PathBuf::from(r"C:\Users\Fake\AppData\Local\Temp\x\roaming"),
            local: PathBuf::from(r"C:\Users\Fake\AppData\Local\Temp\x\local"),
            user_profile: PathBuf::from(r"C:\Users\Fake\AppData\Local\Temp\x\profile"),
        },
        "S-1".into(),
    );
    assert!(env.data_dir().ends_with("LILITH-test-abc"));
    assert!(
        env.legacy_data_dir()
            .ends_with(format!("{LEGACY_APP_NAME}-test-abc"))
    );
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
    assert!(msg.contains("real LILITH location"), "{msg}");
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
fn lilith_and_legacy_local_components_trip() {
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
    assert!(msg.contains("real LILITH location"), "{msg}");
    assert!(msg.contains(".lilith-local"), "{msg}");
}

/// Puts every path on its own drive so the workspace lands in `<volume>\.lilith-local`.
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
        assert!(msg.contains("real LILITH location"), "{msg}");
    }
    let msg = panics(|| {
        let _ = Env::from_system(Channel::Stable);
    })
    .expect("from_system(Stable) must panic");
    assert!(msg.contains("real LILITH location"), "{msg}");
}

#[test] // spec: QA-040
fn test_scoped_names_do_not_trip_and_real_ones_do() {
    arm();
    let env = Env::with_roots(
        Channel::Test("abc".into()),
        roots(r"D:\Scratch"),
        "S-1".into(),
    );
    assert_eq!(env.registry_root(), r"HKCU\Software\LILITH-test-abc");
    assert_eq!(env.credential_prefix(), "LILITH-test-abc/");

    let real = fake_real();
    assert!(real.is_real_registry(r"HKCU\Software\LILITH"));
    assert!(real.is_real_registry(r"hkcu\software\lilith-DEV"));
    assert!(real.is_real_registry(r"HKCU\Software\LILITH\Sub"));
    assert!(!real.is_real_registry(r"HKCU\Software\LILITH-test-abc"));
    assert!(!real.is_real_registry(r"HKCU\Software\Other"));
    assert!(real.is_real_credential("LILITH/"));
    assert!(real.is_real_credential("LILITH-dev/"));
    assert!(real.is_real_credential("lilith/token"));
    assert!(!real.is_real_credential("LILITH-test-abc/"));
    assert!(!real.is_real_credential("Other/"));
    // The legacy name is real too (R4), with the same test-channel exception.
    assert!(real.is_real_registry(&format!(r"HKCU\Software\{LEGACY_APP_NAME}-dev")));
    assert!(!real.is_real_registry(&format!(r"HKCU\Software\{LEGACY_APP_NAME}-test-abc")));
    assert!(real.is_real_credential(&format!("{LEGACY_APP_NAME}/")));
    assert!(!real.is_real_credential(&format!("{LEGACY_APP_NAME}-test-abc/")));
}

/// A test channel whose AUMID shortcut resolves to the real Start-menu Programs folder trips
/// the guard, by its roots or by an explicit Programs folder; its own shortcut and classes root
/// don't (APP-022's names, QA-038).
#[test] // spec: QA-040, QA-038
fn test_channel_shortcut_in_the_real_programs_folder_trips() {
    arm();
    let start_menu = PathBuf::from(REAL_PROGRAMS).parent().unwrap().to_path_buf();
    let by_roots = Env::with_roots(
        Channel::Test("abc".into()),
        Roots {
            roaming: PathBuf::from(r"D:\Scratch\Roaming"),
            local: PathBuf::from(r"D:\Scratch\Local"),
            user_profile: start_menu,
        },
        "S-1".into(),
    );
    let by_dir = Env::with_roots(
        Channel::Test("abc".into()),
        roots(r"D:\Scratch"),
        "S-1".into(),
    )
    .with_programs_dir(PathBuf::from(
        r"c:\users\fake\appdata\roaming\microsoft\windows\start menu\PROGRAMS",
    ));
    for env in [&by_roots, &by_dir] {
        let msg = panics(|| drop(env.start_menu_shortcut())).expect("must panic");
        assert!(msg.contains("real LILITH location"), "{msg}");
        assert!(msg.contains("LILITH (Test-abc).lnk"), "{msg}");
    }

    let own = Env::with_roots(
        Channel::Test("abc".into()),
        roots(r"D:\Scratch"),
        "S-1".into(),
    );
    assert_eq!(
        own.start_menu_shortcut(),
        PathBuf::from(r"D:\Scratch\Profile\Programs\LILITH (Test-abc).lnk")
    );
    assert_eq!(own.classes_root(), r"HKCU\Software\LILITH-test-abc\Classes");
    assert!(fake_real().is_real_classes_root(r"HKCU\Software\Classes"));
    // The desktop group's opt-in into the shell's classes root still trips once armed.
    let shell = own.clone().with_shell_classes_root();
    let msg = panics(|| drop(shell.classes_root())).expect("must panic");
    assert!(msg.contains("real LILITH location"), "{msg}");
    assert!(msg.contains(r"HKCU\Software\Classes"), "{msg}");
}

#[test] // spec: QA-040
fn real_path_matching_is_by_prefix_not_substring_and_case_insensitive() {
    let real = fake_real();
    assert!(real.is_real_path(std::path::Path::new(
        r"c:\users\fake\appdata\roaming\lilith"
    )));
    assert!(real.is_real_path(std::path::Path::new(
        r"\\?\C:\Users\Fake\AppData\Local\LILITH-dev\logs"
    )));
    assert!(real.is_real_path(std::path::Path::new(r"D:\.LiLiTh-Local\abc")));
    assert!(!real.is_real_path(std::path::Path::new(
        r"C:\Users\Fake\AppData\Local\Temp\LILITH\x"
    )));
    assert!(!real.is_real_path(std::path::Path::new(r"C:\Elsewhere\LILITH-test-abc")));
    assert!(!real.is_real_path(std::path::Path::new(
        r"C:\Users\Fake\AppData\Roaming2\LILITH"
    )));
    let legacy_local = format!(r"D:\{}\abc", LEGACY_LOCAL_DIR.to_ascii_uppercase());
    assert!(real.is_real_path(std::path::Path::new(&legacy_local)));
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
        assert!(msg.contains("real LILITH location"), "{msg}");
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
