//! The app's notification setup (APP-022; P4a-R26; `lil_qt::setup`): default features, so the
//! Sandbox runner's archive holds this binary. The tests over `FakeRegistrar` write nothing real;
//! `setup_rereads_fresh_after_writing` asks a real test-channel core, whose desktop is
//! `FakeDesktop`; `desktop_set_up_writes_the_shortcut_and_handlers` is the desktop group's real
//! write, on a test channel of its own, and removes what it wrote.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use lil_client::blocking::BlockingClient;
use lil_client::{ClientConfig, StartPolicy};
use lil_platform::desktop::{DESKTOP_TEST_ID_PREFIX, HandlerPart, ShortcutState};
use lil_platform::guard::require_desktop_group;
use lil_platform::{Channel, Env, Roots};
use lil_protocol::Outcome;
use lil_protocol::app::{HandlersWire, NotificationPath, ShortcutWire, ToastsWire};
use lil_qt::setup::{
    RealRegistrar, Registrar, SetupReport, Step, app_exe, app_missing, ask_core, set_up_with,
};
use lil_testkit::{CoreFixture, TestEnv, ensure_built};
use serde_json::{Map, Value, json};

/// A desktop that records each write and, when told, fails it.
struct FakeRegistrar<'a> {
    shortcut: ShortcutState,
    scheme: HandlerPart,
    shortcut_fails: bool,
    scheme_fails: bool,
    log: &'a RefCell<Vec<String>>,
    wrote: Box<dyn Fn(&str) + 'a>,
}

impl<'a> FakeRegistrar<'a> {
    fn new(log: &'a RefCell<Vec<String>>, shortcut: ShortcutState, scheme: HandlerPart) -> Self {
        FakeRegistrar {
            shortcut,
            scheme,
            shortcut_fails: false,
            scheme_fails: false,
            log,
            wrote: Box::new(|_| {}),
        }
    }
}

impl Registrar for FakeRegistrar<'_> {
    fn shortcut_state(&self, _: &Env, _: &Path) -> ShortcutState {
        self.shortcut
    }

    fn scheme_part(&self, _: &Env, _: &Path) -> HandlerPart {
        self.scheme.clone()
    }

    fn write_shortcut(&self, _: &Env, _: &Path) -> std::io::Result<()> {
        self.log.borrow_mut().push("write_shortcut".to_string());
        if self.shortcut_fails {
            return Err(std::io::Error::other("the shortcut was refused"));
        }
        (self.wrote)("shortcut");
        Ok(())
    }

    fn register_scheme(&self, _: &Env, _: &Path) -> std::io::Result<()> {
        self.log.borrow_mut().push("register_scheme".to_string());
        if self.scheme_fails {
            return Err(std::io::Error::other("the scheme was refused"));
        }
        (self.wrote)("handlers");
        Ok(())
    }
}

fn channel_env(channel: Channel, dir: &Path) -> Env {
    Env::with_roots(
        channel,
        Roots {
            roaming: dir.join("roaming"),
            local: dir.join("local"),
            user_profile: dir.join("profile"),
        },
        "S-1".into(),
    )
}

fn path_answer(shortcut: &str, handlers: &str) -> Outcome<lil_protocol::ToolOutput> {
    let structured = json!({"notification_path": {
        "toasts": "ready", "shortcut": shortcut, "handlers": handlers}});
    serde_json::from_value(
        json!({"status": "ok", "structured": structured, "artifacts": [],
                                  "summary_text": ""}),
    )
    .expect("a tool's outcome")
}

fn ok_path() -> NotificationPath {
    NotificationPath {
        toasts: ToastsWire::Ready,
        toasts_reason: None,
        shortcut: ShortcutWire::Ok,
        handlers: HandlersWire::Ok,
    }
}

/// `set_up_with` on a test channel over `registrar`; the log holds the one call as it was made.
fn run(registrar: &FakeRegistrar<'_>, log: &RefCell<Vec<String>>) -> SetupReport {
    let dir = tempfile::tempdir().unwrap();
    let env = channel_env(Channel::Test("0123abcd".into()), dir.path());
    set_up_with(
        &env,
        Path::new(r"C:\none\lil-image.exe"),
        registrar,
        |tool, args| {
            log.borrow_mut()
                .push(format!("{tool} {}", Value::Object(args)));
            Ok(path_answer("ok", "ok"))
        },
    )
}

/// A client of `env`'s test core, whichever folder the test kit built it in.
fn test_config(env: &TestEnv) -> ClientConfig {
    let mut cfg = ClientConfig::for_env(env.env.clone(), "cli".parse().unwrap()).unwrap();
    cfg.server_dirs = vec![ensure_built().dir.clone()];
    cfg.core_exe = ensure_built().lil_core.clone();
    cfg.start = StartPolicy::NoStart;
    cfg
}

/// Real writes are stood in for: each tells the test core's `FakeDesktop` what it wrote, and the
/// one `desktop.get {fresh: true}` that follows reads that from the real core.
#[test] // spec: APP-022
fn setup_rereads_fresh_after_writing() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let client = BlockingClient::connect(&test_config(&env)).expect("the test core");
    let tell = |args: Value| {
        let Value::Object(args) = args else {
            unreachable!()
        };
        let answer = client
            .call_tool("test.desktop", args, None)
            .expect("test.desktop");
        assert!(matches!(answer, Outcome::Ok(_)), "{answer:?}");
    };
    tell(json!({"shortcut": false, "handlers": false}));

    let log = RefCell::new(Vec::new());
    let mut registrar = FakeRegistrar::new(&log, ShortcutState::Missing, HandlerPart::Missing);
    registrar.wrote = Box::new(|what| tell(json!({ what: true })));
    let report = set_up_with(
        &env.env,
        Path::new(r"C:\none\lil-image.exe"),
        &registrar,
        |tool, args: Map<String, Value>| {
            log.borrow_mut()
                .push(format!("{tool} {}", Value::Object(args.clone())));
            client
                .call_tool(tool, args, None)
                .map_err(|e| e.to_string())
        },
    );

    assert_eq!(
        *log.borrow(),
        [
            "write_shortcut",
            "register_scheme",
            r#"desktop.get {"fresh":true}"#
        ],
        "both writes, then one fresh read"
    );
    assert_eq!(report.shortcut, Step::Written);
    assert_eq!(report.scheme, Step::Written);
    assert_eq!(report.reread_error, None);
    assert_eq!(
        report.notification_path,
        Some(ok_path()),
        "the core's own answer"
    );
}

/// The bridge's own way to the core: a connection that names no workspace and starts no core.
#[test] // spec: APP-022
fn the_read_starts_no_core_and_names_no_workspace() {
    let env = TestEnv::new();
    let mut cfg = test_config(&env);
    // `ask_core` itself makes it `NoStart`.
    cfg.start = StartPolicy::StartIfNeeded;
    // A default that went away must not stand between the app and its notification path.
    cfg.workspace = "ws_gone".to_string();
    let fresh = || {
        let mut args = Map::new();
        args.insert("fresh".to_string(), Value::Bool(true));
        args
    };
    assert!(
        ask_core(&cfg, "desktop.get", fresh()).is_err(),
        "no core runs"
    );
    assert!(
        !lil_client::doctor::core_running(&env.env),
        "the read never starts a core"
    );
    let _core = CoreFixture::start(&env);
    let answer = ask_core(&cfg, "desktop.get", fresh()).expect("the test core answers");
    let Outcome::Ok(output) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(
        output.structured["notification_path"]["handlers"],
        "missing"
    );
}

#[test] // spec: APP-022
fn setup_says_which_write_failed_and_still_reads() {
    let log = RefCell::new(Vec::new());
    let mut registrar = FakeRegistrar::new(&log, ShortcutState::Missing, HandlerPart::Missing);
    registrar.scheme_fails = true;
    let report = run(&registrar, &log);
    assert_eq!(report.shortcut, Step::Written);
    assert_eq!(
        report.scheme,
        Step::Failed {
            why: "the scheme was refused".to_string()
        }
    );
    assert_eq!(report.notification_path, Some(ok_path()));

    // The other way round, the scheme is still tried.
    let log = RefCell::new(Vec::new());
    let mut registrar = FakeRegistrar::new(&log, ShortcutState::Missing, HandlerPart::Missing);
    registrar.shortcut_fails = true;
    let report = run(&registrar, &log);
    assert!(matches!(report.shortcut, Step::Failed { .. }), "{report:?}");
    assert_eq!(report.scheme, Step::Written);
}

#[test] // spec: APP-022
fn only_what_isnt_right_is_written() {
    // A wrong shortcut is rewritten, each kind of wrong; one that is right is left alone.
    for wrong in [
        ShortcutState::Missing,
        ShortcutState::WrongTarget,
        ShortcutState::WrongAumid,
        ShortcutState::WrongClsid,
    ] {
        let log = RefCell::new(Vec::new());
        let report = run(&FakeRegistrar::new(&log, wrong, HandlerPart::Ok), &log);
        assert_eq!(report.shortcut, Step::Written, "{wrong:?}");
        assert_eq!(report.scheme, Step::Ok);
        assert_eq!(log.borrow()[0], "write_shortcut");
        assert_eq!(log.borrow().len(), 2, "{:?}", log.borrow());
    }
    let log = RefCell::new(Vec::new());
    let report = run(
        &FakeRegistrar::new(&log, ShortcutState::Ok, HandlerPart::Ok),
        &log,
    );
    assert_eq!((report.shortcut, report.scheme), (Step::Ok, Step::Ok));
    assert_eq!(*log.borrow(), [r#"desktop.get {"fresh":true}"#], "no write");

    // A LILITH registration whose exe is gone is this channel's to rewrite.
    let log = RefCell::new(Vec::new());
    let stale = HandlerPart::Stale(PathBuf::from(r"C:\gone\lil-image.exe"));
    let report = run(&FakeRegistrar::new(&log, ShortcutState::Ok, stale), &log);
    assert_eq!(report.scheme, Step::Written);
}

/// `handlers: other_channel` has no fix in the app: the scheme stays with whoever holds it.
#[test] // spec: APP-022
fn a_scheme_another_exe_holds_is_left_alone() {
    for held in [
        HandlerPart::OtherBuild(PathBuf::from(r"C:\stable\lil-image.exe")),
        HandlerPart::Foreign("URL:Another app".to_string()),
    ] {
        let log = RefCell::new(Vec::new());
        let report = run(
            &FakeRegistrar::new(&log, ShortcutState::Missing, held),
            &log,
        );
        assert_eq!(report.shortcut, Step::Written);
        assert_eq!(report.scheme, Step::Held);
        assert!(
            !log.borrow().iter().any(|l| l == "register_scheme"),
            "{:?}",
            log.borrow()
        );
    }
}

#[test] // spec: APP-022
fn a_failed_read_keeps_what_was_written() {
    let log = RefCell::new(Vec::new());
    let registrar = FakeRegistrar::new(&log, ShortcutState::Missing, HandlerPart::Ok);
    let dir = tempfile::tempdir().unwrap();
    let env = channel_env(Channel::Test("0123abcd".into()), dir.path());
    let report = set_up_with(
        &env,
        Path::new(r"C:\none\lil-image.exe"),
        &registrar,
        |_, _| Err("no core runs".to_string()),
    );
    assert_eq!(report.shortcut, Step::Written);
    assert_eq!(report.notification_path, None);
    assert_eq!(report.reread_error.as_deref(), Some("no core runs"));
}

/// `lil doctor`'s rule (plan 4a ruling, 2026-10-25): outside a test channel, nothing while the app
/// isn't there, so a working registration is never pointed at a missing exe.
#[test] // spec: APP-022
fn a_missing_app_is_set_up_never() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("lil-image.exe");
    for channel in [Channel::Stable, Channel::Dev] {
        let env = channel_env(channel, dir.path());
        let log = RefCell::new(Vec::new());
        let registrar = FakeRegistrar::new(&log, ShortcutState::Missing, HandlerPart::Missing);
        let report = set_up_with(&env, &gone, &registrar, |tool, _| {
            panic!("no call while the app is missing: {tool}")
        });
        assert_eq!(
            report.app_missing.as_deref(),
            Some(&*gone.to_string_lossy())
        );
        assert_eq!(
            (report.shortcut, report.scheme),
            (Step::NotTried, Step::NotTried)
        );
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
        assert!(app_missing(&env, &gone));
    }
    let test = channel_env(Channel::Test("0123abcd".into()), dir.path());
    assert!(
        !app_missing(&test, &gone),
        "a test channel's shell never reads it"
    );
    std::fs::write(&gone, b"").unwrap();
    assert!(!app_missing(&channel_env(Channel::Dev, dir.path()), &gone));
}

/// The path the core checks (`lil_core::server`): else `shortcut` reads `wrong`.
#[test] // spec: APP-022
fn the_app_path_is_the_one_the_core_checks() {
    let dir = tempfile::tempdir().unwrap();
    let config = |channel| {
        let env = channel_env(channel, dir.path());
        let mut cfg = ClientConfig::for_env(env, "cli".parse().unwrap()).unwrap();
        cfg.core_exe = PathBuf::from(r"C:\deploy\lil-core.exe");
        cfg
    };
    assert_eq!(
        app_exe(&config(Channel::Dev)),
        Some(PathBuf::from(r"C:\deploy\lil-image.exe")),
        "beside the core"
    );
    let stable = config(Channel::Stable);
    assert_eq!(
        app_exe(&stable),
        Some(stable.env.install_dir().unwrap().join("lil-image.exe")),
        "the install folder"
    );
}

/// What QML reads (`NotificationText.setupOutcome`, `tst_settings.qml`'s fixtures).
#[test] // spec: APP-022
fn the_report_reads_as_qml_expects() {
    let report = SetupReport {
        app_missing: None,
        shortcut: Step::Written,
        scheme: Step::Failed {
            why: "denied".to_string(),
        },
        notification_path: Some(NotificationPath {
            toasts: ToastsWire::NotYetShown,
            toasts_reason: None,
            shortcut: ShortcutWire::Ok,
            handlers: HandlersWire::Missing,
        }),
        reread_error: None,
    };
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        json!({"app_missing": null, "shortcut": {"state": "written"},
               "scheme": {"state": "failed", "why": "denied"},
               "notification_path": {"toasts": "not_yet_shown", "shortcut": "ok", "handlers": "missing"},
               "reread_error": null})
    );
}

/// Removes what the test wrote, on every path, each through the removers that refuse anything but
/// a test channel, and fails when something is left.
struct Cleanup<'a>(&'a Env);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        use lil_platform::desktop::{
            leftover_registrations, remove_shortcut, remove_test_registry, unregister_handlers,
        };
        let env = self.0;
        let mut left: Vec<String> = [
            remove_shortcut(env),
            unregister_handlers(env),
            remove_test_registry(env),
        ]
        .into_iter()
        .filter_map(|r| r.err().map(|e| e.to_string()))
        .collect();
        if env.start_menu_shortcut().exists() {
            left.push(env.start_menu_shortcut().display().to_string());
        }
        match leftover_registrations(env) {
            Ok(keys) => left.extend(keys),
            Err(e) => left.push(e.to_string()),
        }
        if !std::thread::panicking() {
            assert!(left.is_empty(), "left behind: {left:?}");
        }
    }
}

/// The real write (QA-042): the Sandbox runner only, on a desktop test channel of its own. The
/// shortcut and the own scheme read `ok` for the deployed `lil-image.exe`, `.lilref` stays unwritten,
/// and a second run touches nothing. The channel's own classes root, which the shell never reads,
/// is the one that has a `.lilref` to leave alone.
#[test] // spec: APP-022, QA-038, QA-042
#[ignore = "desktop"]
fn desktop_set_up_writes_the_shortcut_and_handlers() {
    require_desktop_group(false);
    let deploy =
        std::env::var_os("LIL_DEPLOY_DIR").expect("LIL_DEPLOY_DIR names the deploy folder");
    let exe = PathBuf::from(deploy).join("lil-image.exe");
    let dir = tempfile::tempdir().unwrap();
    // A fresh id per run, so a killed run's leftovers are swept by name.
    let id = {
        use std::hash::{BuildHasher, RandomState};
        RandomState::new().hash_one(std::process::id())
    };
    let env = channel_env(
        Channel::Test(format!("{DESKTOP_TEST_ID_PREFIX}{id:016x}")),
        dir.path(),
    );
    let _cleanup = Cleanup(&env);
    assert_eq!(
        lil_platform::desktop::shortcut_state(&env, &exe),
        ShortcutState::Missing
    );

    let asked = RefCell::new(0);
    let reread = |_: &str, _: Map<String, Value>| {
        *asked.borrow_mut() += 1;
        Err("no core in this test".to_string())
    };
    let report = set_up_with(&env, &exe, &RealRegistrar, reread);
    assert_eq!(
        (&report.shortcut, &report.scheme),
        (&Step::Written, &Step::Written)
    );
    assert_eq!(*asked.borrow(), 1);

    assert_eq!(
        lil_platform::desktop::shortcut_state(&env, &exe),
        ShortcutState::Ok,
        "the core's own check of the same path"
    );
    let parts = lil_platform::desktop::handler_parts(&env, &exe);
    assert_eq!(parts.scheme, HandlerPart::Ok);
    assert_eq!(
        parts.lilref,
        HandlerPart::Missing,
        "Set up never writes .lilref"
    );

    let again = set_up_with(
        &env,
        &exe,
        &RealRegistrar,
        |_, _| Err("no core".to_string()),
    );
    assert_eq!((again.shortcut, again.scheme), (Step::Ok, Step::Ok));
}
