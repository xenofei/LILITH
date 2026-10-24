//! The plan-4a names the environment provider adds (ARCH-095, QA-038; Appendix G
//! "Activation and registration"): the activation scheme, the classes root, the AUMID shortcut,
//! the toast activator CLSID, the app folders and the app pipes. Nothing here arms the guard
//! or touches the system: every `Env` is built from explicit roots.

use std::path::{Path, PathBuf};

use lil_platform::{Channel, Env, RealLocations, Roots};

const SID: &str = "S-1-5-21-1-2-3-1001";

fn roots() -> Roots {
    Roots {
        roaming: PathBuf::from(r"C:\T\roaming"),
        local: PathBuf::from(r"C:\T\local"),
        user_profile: PathBuf::from(r"C:\T\profile"),
    }
}

fn env(channel: Channel) -> Env {
    Env::with_roots(channel, roots(), SID.to_string())
}

#[test] // spec: QA-038, ARCH-095, APP-020, APP-022
fn names_per_channel() {
    let stable = env(Channel::Stable);
    assert_eq!(stable.activation_scheme(), "lil");
    assert_eq!(stable.classes_root(), r"HKCU\Software\Classes");
    assert_eq!(
        stable.start_menu_shortcut(),
        PathBuf::from(r"C:\T\profile\Programs\LILITH.lnk")
    );

    let dev = env(Channel::Dev);
    assert_eq!(dev.activation_scheme(), "lil-dev");
    assert_eq!(dev.classes_root(), r"HKCU\Software\Classes");
    assert_eq!(
        dev.start_menu_shortcut(),
        PathBuf::from(r"C:\T\profile\Programs\LILITH (Dev).lnk")
    );

    let test = env(Channel::Test("abc".into()));
    assert_eq!(test.activation_scheme(), "lil-test-abc");
    assert_eq!(
        test.classes_root(),
        r"HKCU\Software\LILITH-test-abc\Classes"
    );
    // On a test channel `<Programs>` is `<test root>\profile\Programs\`.
    assert_eq!(
        test.start_menu_shortcut(),
        PathBuf::from(r"C:\T\profile\Programs\LILITH (Test-abc).lnk")
    );
    assert!(test.start_menu_shortcut().starts_with(r"C:\T"));

    // App folders and pipes (APP-013, P4a-R11, P4a-R16, P4a-R19).
    assert_eq!(
        test.app_state_dir("image"),
        PathBuf::from(r"C:\T\local\LILITH-test-abc\apps\image")
    );
    assert_eq!(
        test.diagnostics_dir(),
        PathBuf::from(r"C:\T\local\LILITH-test-abc\diagnostics")
    );
    assert_eq!(
        test.lilref_dir(),
        PathBuf::from(r"C:\T\local\LILITH-test-abc\cache\lilref")
    );
    assert_eq!(
        test.app_pipe_name("image", false),
        format!(r"\\.\pipe\lilith-app-image-{SID}-test-abc")
    );
    assert_eq!(
        test.app_pipe_name("image", true),
        format!(r"\\.\pipe\lilith-app-image-confirm-{SID}-test-abc")
    );
    assert_eq!(
        dev.app_pipe_name("image", true),
        format!(r"\\.\pipe\lilith-app-image-confirm-{SID}-dev")
    );
}

/// The CLSID is fixed per channel and differs between channels (P4a-R37).
#[test] // spec: APP-022, QA-038
fn toast_activator_clsid_is_fixed_per_channel() {
    let channels = [
        Channel::Stable,
        Channel::Dev,
        Channel::Test("abc".into()),
        Channel::Test("abd".into()),
    ];
    let clsids: Vec<String> = channels
        .iter()
        .map(|c| env(c.clone()).toast_activator_clsid())
        .collect();
    for (channel, clsid) in channels.iter().zip(&clsids) {
        // Fixed: the same channel gives the same CLSID, whatever the roots and SID.
        let other = Env::with_roots(
            channel.clone(),
            Roots {
                roaming: PathBuf::from(r"D:\x"),
                local: PathBuf::from(r"D:\y"),
                user_profile: PathBuf::from(r"D:\z"),
            },
            "S-1-5-21-9".into(),
        );
        assert_eq!(&other.toast_activator_clsid(), clsid, "{channel}");
        // The registry form: `{8-4-4-4-12}` upper-case hex.
        let b = clsid.as_bytes();
        assert_eq!(b.len(), 38, "{clsid}");
        assert_eq!((b[0], b[37]), (b'{', b'}'), "{clsid}");
        for (i, c) in clsid[1..37].char_indices() {
            if [8, 13, 18, 23].contains(&i) {
                assert_eq!(c, '-', "{clsid}");
            } else {
                assert!(c.is_ascii_digit() || ('A'..='F').contains(&c), "{clsid}");
            }
        }
    }
    let mut unique = clsids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), clsids.len(), "{clsids:?}");
}

/// The guard's predicates for the two new real locations (QA-040): the real Start-menu
/// Programs folder and the real classes root. A test channel's own names are not real.
#[test] // spec: QA-040
fn real_programs_and_classes_root_are_real_and_test_names_are_not() {
    let real = RealLocations {
        roaming: PathBuf::from(r"C:\Users\Fake\AppData\Roaming"),
        local: PathBuf::from(r"C:\Users\Fake\AppData\Local"),
        user_profile: PathBuf::from(r"C:\Users\Fake"),
        programs: PathBuf::from(
            r"C:\Users\Fake\AppData\Roaming\Microsoft\Windows\Start Menu\Programs",
        ),
        registry_root: r"HKCU\Software\LILITH".to_string(),
        credential_prefix: "LILITH/".to_string(),
        workspaces: Vec::new(),
    };
    for p in [
        r"C:\Users\Fake\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\LILITH.lnk",
        r"c:\users\fake\appdata\roaming\microsoft\windows\start menu\programs\LILITH (Test-abc).lnk",
    ] {
        assert!(real.is_real_shortcut(Path::new(p)), "{p}");
    }
    let test = env(Channel::Test("abc".into()));
    assert!(!real.is_real_shortcut(&test.start_menu_shortcut()));

    for v in [
        r"HKCU\Software\Classes",
        r"hkcu\software\CLASSES\lil",
        r"HKCU\Software\Classes\CLSID\{0}",
    ] {
        assert!(real.is_real_classes_root(v), "{v}");
    }
    for v in [
        test.classes_root(),
        r"HKCU\Software\ClassesX".to_string(),
        r"HKCU\Software\Other\Classes".to_string(),
    ] {
        assert!(!real.is_real_classes_root(&v), "{v}");
    }
}

/// The desktop group's opt-in into the shell's classes root (Appendix G: the test classes root
/// is for test channels outside the desktop group): test channels only.
#[test] // spec: APP-022, QA-038
fn shell_classes_root_is_a_test_channel_opt_in() {
    let test = env(Channel::Test("desktop-abc".into())).with_shell_classes_root();
    assert_eq!(test.classes_root(), r"HKCU\Software\Classes");
    // Nothing else moves: the scheme, the CLSID and the registry root stay the channel's own.
    assert_eq!(test.activation_scheme(), "lil-test-desktop-abc");
    assert_eq!(
        test.toast_activator_clsid(),
        env(Channel::Test("desktop-abc".into())).toast_activator_clsid()
    );
    assert_eq!(
        test.registry_root(),
        r"HKCU\Software\LILITH-test-desktop-abc"
    );
    // Only a desktop-group id, so Task 30's sweep by prefix finds whatever it wrote.
    for channel in [Channel::Stable, Channel::Dev, Channel::Test("abc".into())] {
        let got = std::panic::catch_unwind(|| env(channel.clone()).with_shell_classes_root());
        assert!(got.is_err(), "{channel} must refuse the opt-in");
    }
}

/// A URL scheme is letters, digits, `+`, `-` and `.` (RFC 3986 §3.1): a test id with `_` makes
/// none, so `activation_scheme` refuses it rather than give a scheme the shell won't resolve.
#[test] // spec: APP-020, QA-038
fn a_test_id_with_an_underscore_makes_no_scheme() {
    assert_eq!(
        env(Channel::Test("desktop-0a1B".into())).activation_scheme(),
        "lil-test-desktop-0a1B"
    );
    let got = std::panic::catch_unwind(|| env(Channel::Test("a_b".into())).activation_scheme());
    let msg = got.expect_err("must panic");
    let msg = msg.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("can't make a URL scheme"), "{msg}");
}
