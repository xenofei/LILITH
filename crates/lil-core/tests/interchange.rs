//! The app handler registry, `open_in` and `ui.show` (APP-061, APP-063–066): registrations
//! from both folders, handlers matched on type and format, a running app told without focus
//! for an agent, a launch command and no process for an app that isn't running, whose last
//! argument is the target as a base64url `open/` URL (APP-020, N6), and the closed set of
//! intents; and `lilref.write` (APP-021), which writes only in the `.lilref` cache. Driven
//! through the registered tools on a `Registry`, with the connections the production router
//! reports ([`ConnObserver`]) simulated.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::interchange::{
    AppRegistration, Intent, Interchange, OpensEntry, Target, launch_args, load_registrations,
    register_interchange_tools,
};
use lil_core::server::ConnObserver;
use lil_core::session::Session;
use lil_core::tools::{CallCtx, Registry};
use lil_parse::{LilUrl, parse_lil_url};
use lil_protocol::CallerKind;
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, TestEnv, sha256_bytes};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// Writes `<dir>\apps\<name>.toml`: the app `name`, run from `exe`, opening `formats` of
/// `types` for `intents`.
fn register(dir: &Path, name: &str, exe: &str, types: &str, formats: &str, intents: &str) {
    let text = format!(
        "app = \"{name}\"\nexe = '{exe}'\nargs = [\"--flag\"]\n[[opens]]\ntypes = [{types}]\nformats = [{formats}]\nintents = [{intents}]\n"
    );
    write_file(
        &dir.join("apps").join(format!("{name}.toml")),
        text.as_bytes(),
    );
}

struct Rig {
    env: TestEnv,
    core_dir: PathBuf,
    interchange: Arc<Interchange>,
    reg: Registry,
    events: Arc<EventBus>,
}

fn rig() -> Rig {
    let env = TestEnv::new();
    let core_dir = env.root().join("core");
    let interchange = Interchange::new(core_dir.clone(), env.env.clone());
    let mut reg = Registry::new();
    register_interchange_tools(&mut reg, &interchange);
    Rig {
        env,
        core_dir,
        interchange,
        reg,
        events: Arc::new(EventBus::new(FakeClock::new())),
    }
}

impl Rig {
    fn ctx(&self, caller: CallerKind) -> CallCtx {
        common::ctx(&self.env, caller, &self.events)
    }

    /// `ui:<app>` says hello on connection `conn`, from process `pid`.
    fn connect_app(&self, conn: u64, app: &str, pid: u32) {
        let session = Session {
            id: format!("s_{conn}"),
            caller: CallerKind::Ui(app.to_string()),
            non_interactive: false,
            workspace: None,
            capabilities: Vec::new(),
            client_pid: pid,
            profile_id: None,
        };
        self.interchange.ready(conn, &session);
    }

    /// Subscribes a client to `topic` and returns where its events arrive.
    async fn listen(&self, topic: &str) -> tokio::sync::mpsc::Receiver<String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        self.events.attach(1, tx);
        self.events
            .subscribe(1, &[topic.to_string()], &BTreeMap::new())
            .await;
        rx
    }
}

/// The `data` of the next event on the receiver.
async fn next_data(rx: &mut tokio::sync::mpsc::Receiver<String>) -> Value {
    let line = tokio::time::timeout(DEADLINE, rx.recv())
        .await
        .expect("an event within the deadline")
        .expect("the bus is open");
    let message: Value = serde_json::from_str(&line).unwrap();
    message["params"]["data"].clone()
}

#[test] // spec: APP-061
fn registrations_load_from_both_folders() {
    let r = rig();
    let data_dir = r.env.env.data_dir();
    register(
        &r.core_dir,
        "test",
        "test.exe",
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    register(
        &data_dir,
        "viewer",
        r"C:\Tools\viewer.exe",
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    // A file that doesn't parse is skipped; so is a name that is not an app name.
    write_file(
        &data_dir.join("apps").join("broken.toml"),
        b"app = [not toml",
    );
    write_file(
        &data_dir.join("apps").join("odd.toml"),
        b"app = \"No Good\"\nexe = 'C:\\x.exe'\n",
    );
    // An external program can't replace an installed app.
    register(&data_dir, "test", r"C:\Tools\fake.exe", "", "", r#""open""#);

    let regs = load_registrations(&r.core_dir, &data_dir, false);
    let names: Vec<(&str, bool)> = regs.iter().map(|g| (g.app.as_str(), g.external)).collect();
    assert_eq!(names, [("test", false), ("viewer", true)]);
    // An installed app's relative exe is beside the core.
    assert_eq!(regs[0].exe, r.core_dir.join("test.exe"));
    assert_eq!(regs[1].exe, PathBuf::from(r"C:\Tools\viewer.exe"));
    assert_eq!(regs[1].args, ["--flag"]);
    assert!(
        load_registrations(&r.core_dir.join("nowhere"), &r.core_dir.join("nor"), false).is_empty()
    );
}

/// Ruling I1 of the Task 11 review: an external registration whose exe is in the core's folder
/// (or below it) is skipped, so it can't start a suite program with arguments of its own.
#[test] // spec: APP-061, ARCH-015
#[allow(clippy::disallowed_methods)] // makes the hard link and the folder it is in
fn an_external_registration_of_a_suite_exe_is_skipped() {
    let r = rig();
    let data_dir = r.env.env.data_dir();
    register(
        &r.core_dir,
        "image",
        "lil-image.exe",
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    let inside = r.core_dir.join("LIL-IMAGE.EXE");
    let below = r.core_dir.join("sub").join("x.exe");
    for (name, exe) in [("evil", &inside), ("evil-two", &below)] {
        register(
            &data_dir,
            name,
            &exe.display().to_string(),
            r#""image""#,
            r#""png""#,
            r#""open""#,
        );
    }
    register(
        &data_dir,
        "viewer",
        &r.env
            .root()
            .join("tools")
            .join("viewer.exe")
            .display()
            .to_string(),
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    let test_app = r.core_dir.join("lil-test-app.exe");
    register(
        &data_dir,
        "test",
        &test_app.display().to_string(),
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    let names = |test_app: bool| -> Vec<String> {
        load_registrations(&r.core_dir, &data_dir, test_app)
            .into_iter()
            .map(|g| g.app)
            .collect()
    };
    assert_eq!(names(false), ["image", "viewer"]);

    // Review N1: the same file by another path is caught by its identity, not its path.
    write_file(&r.core_dir.join("lil-image.exe"), b"MZ suite program");
    let tools = r.env.root().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let link = tools.join("innocent.exe");
    std::fs::hard_link(r.core_dir.join("lil-image.exe"), &link).unwrap();
    register(
        &data_dir,
        "linked",
        &link.display().to_string(),
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    assert_eq!(names(false), ["image", "viewer"]);
    // An unrelated program outside is still registered.
    write_file(&tools.join("viewer.exe"), b"MZ another program");
    assert_eq!(names(false), ["image", "viewer"]);
    // On a test channel the test app, beside the test core, may be registered from outside.
    assert_eq!(names(true), ["image", "test", "viewer"]);
}

#[tokio::test] // spec: APP-061
async fn handlers_for_matches_types_and_formats() {
    let r = rig();
    register(
        &r.env.env.data_dir(),
        "viewer",
        r"C:\Tools\viewer.exe",
        r#""image""#,
        r#""png""#,
        r#""open", "use_as_input""#,
    );
    let ctx = r.ctx(CallerKind::Cli);
    let for_target = |target: &str| {
        let (reg, ctx) = (&r.reg, &ctx);
        let target = target.to_string();
        async move { common::call(reg, ctx, "handlers.for", json!({ "target": target })).await }
    };

    let png = for_target(r"C:\pics\knight.PNG").await.unwrap();
    assert_eq!(
        png,
        json!({"handlers": [{"app": "viewer", "intents": ["open", "use_as_input"], "external": true}]})
    );
    // A webp is no error: nothing opens it.
    let webp = for_target(r"C:\pics\knight.webp").await.unwrap();
    assert_eq!(webp, json!({"handlers": []}));
    // A candidate is typed by its file; a document is not an image.
    let candidate = for_target("@candidate/r_20261018_0900/out.png")
        .await
        .unwrap();
    assert_eq!(candidate["handlers"][0]["app"], "viewer");
    let doc = for_target("doc:d_1/proposal/r_20261018_0900/1")
        .await
        .unwrap();
    assert_eq!(doc, json!({"handlers": []}));
    // The same through the module's own function.
    assert_eq!(r.interchange.handlers_for(r"C:\pics\a.png").len(), 1);
    assert!(r.interchange.handlers_for("").is_empty());

    for bad in ["", "bad\ntarget", "lil://nonsense@1.0.0"] {
        let refused = for_target(bad).await.unwrap_err();
        assert_eq!(refused.code, "params.invalid", "{bad:?}");
    }
}

#[tokio::test] // spec: APP-063, APP-065
async fn open_in_running_publishes_without_focus_for_agents() {
    let r = rig();
    register(
        &r.core_dir,
        "test",
        "test.exe",
        r#""image""#,
        r#""png""#,
        r#""open", "edit""#,
    );
    r.connect_app(5, "test", 4242);
    let mut rx = r.listen("open_requests:test").await;
    let args = json!({"app": "test", "target": r"C:\pics\a.png", "intent": "open"});

    let agent = r.ctx(CallerKind::Cli);
    let result = common::call(&r.reg, &agent, "open_in", args.clone())
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({"running": true, "pid": 4242, "launch_command": null})
    );
    let request = next_data(&mut rx).await;
    assert_eq!(
        request,
        json!({"target": r"C:\pics\a.png", "intent": "open", "focus": false})
    );

    // `ui.show` is the same without naming the app, and an app caller keeps focus.
    let ui = r.ctx(CallerKind::Ui("image".into()));
    let shown = common::call(&r.reg, &ui, "ui.show", json!({"target": r"C:\pics\a.png"}))
        .await
        .unwrap();
    assert_eq!(shown["running"], true, "{shown}");
    assert_eq!(next_data(&mut rx).await["focus"], true);
    let edit = json!({"app": "test", "target": r"C:\pics\a.png", "intent": "edit"});
    common::call(&r.reg, &ui, "open_in", edit).await.unwrap();
    let request = next_data(&mut rx).await;
    assert_eq!(
        (request["intent"].as_str(), request["focus"].as_bool()),
        (Some("edit"), Some(true))
    );

    // An unknown app, and an app that doesn't handle the target, are `handler.none`.
    let unknown = json!({"app": "nothing", "target": r"C:\pics\a.png", "intent": "open"});
    let refused = common::call(&r.reg, &agent, "open_in", unknown)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "handler.none");
    let wrong = json!({"app": "test", "target": r"C:\pics\a.webp", "intent": "open"});
    let refused = common::call(&r.reg, &agent, "open_in", wrong)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "handler.none");
    let intent = json!({"app": "test", "target": r"C:\pics\a.png", "intent": "use_as_input"});
    let refused = common::call(&r.reg, &agent, "open_in", intent)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "handler.none");

    // When its connection closes the app is no longer running.
    r.interchange.closed(5);
    let after = common::call(&r.reg, &agent, "open_in", args).await.unwrap();
    assert_eq!(after["running"], false, "{after}");
}

#[tokio::test] // spec: APP-064
async fn open_in_never_spawns_a_process() {
    let r = rig();
    // A real executable that would show up in the process list if it ran.
    let exe = r.env.root().join("viewer").join("spec-viewer.exe");
    write_file(&exe, b"not a program");
    register(
        &r.env.env.data_dir(),
        "viewer",
        &exe.to_string_lossy(),
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    let agent = r.ctx(CallerKind::Cli);
    let target = r.env.root().join("picture.png");
    let args = json!({"app": "viewer", "target": target.to_string_lossy(), "intent": "open"});

    let result = common::call(&r.reg, &agent, "open_in", args).await.unwrap();
    assert_eq!(result["running"], false);
    assert_eq!(result["pid"], Value::Null);
    assert_eq!(
        result["launch_command"]["exe"],
        exe.to_string_lossy().as_ref()
    );
    let scheme = r.env.env.activation_scheme();
    assert_eq!(
        result["launch_command"]["args"],
        json!([
            "--flag",
            "--intent",
            "open",
            lil_parse::open_url(&scheme, &target.to_string_lossy())
        ])
    );
    let tasks = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq spec-viewer.exe", "/NH"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&tasks.stdout).contains("spec-viewer.exe"),
        "the core launched the app"
    );
    // `ui.show` has the same answer for the one app that opens it.
    let shown = common::call(
        &r.reg,
        &agent,
        "ui.show",
        json!({"target": target.to_string_lossy()}),
    )
    .await
    .unwrap();
    assert_eq!(
        shown["launch_command"]["exe"],
        exe.to_string_lossy().as_ref()
    );
    let nothing = common::call(&r.reg, &agent, "ui.show", json!({"target": "a.webp"}))
        .await
        .unwrap_err();
    assert_eq!(nothing.code, "handler.none");
}

#[tokio::test] // spec: APP-066
async fn intents_are_closed() {
    let r = rig();
    register(
        &r.core_dir,
        "test",
        "test.exe",
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    let agent = r.ctx(CallerKind::Cli);
    for intent in ["print", "OPEN", ""] {
        let args = json!({"app": "test", "target": "a.png", "intent": intent});
        let refused = common::call(&r.reg, &agent, "open_in", args)
            .await
            .unwrap_err();
        assert_eq!(refused.code, "params.invalid", "{intent:?}");
    }
    let unknown_field = json!({"app": "test", "target": "a.png", "intent": "open", "focus": true});
    let refused = common::call(&r.reg, &agent, "open_in", unknown_field)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "params.invalid");
}

#[tokio::test] // spec: APP-061
async fn a_target_of_unknown_type_has_no_handler() {
    let r = rig();
    register(
        &r.core_dir,
        "test",
        "test.exe",
        r#""image""#,
        r#""png""#,
        r#""open""#,
    );
    // An app that opens anything still isn't offered a target nothing is known about.
    register(&r.core_dir, "any", "any.exe", "", "", r#""open""#);
    let agent = r.ctx(CallerKind::Cli);
    // A sealed ref outside any open workspace: no type, no format.
    let target = "lil://0123456789abcdef/sketchbook/a@1.0.0";
    let none = common::call(&r.reg, &agent, "handlers.for", json!({ "target": target }))
        .await
        .unwrap();
    assert_eq!(none, json!({"handlers": []}));
    let open = json!({"app": "any", "target": target, "intent": "open"});
    let refused = common::call(&r.reg, &agent, "open_in", open)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "handler.none");
    // A known format still reaches the app that opens anything.
    let png = common::call(&r.reg, &agent, "handlers.for", json!({"target": "a.png"}))
        .await
        .unwrap();
    assert_eq!(png["handlers"].as_array().unwrap().len(), 2, "{png}");
}

/// The target the launch argument `url` carries, read as the app's handler reads it.
fn opened_by(url: &str, scheme: &str) -> String {
    match parse_lil_url(url, scheme) {
        Ok(LilUrl::Open(target)) => target,
        other => panic!("{url:?} is not an open/ URL of {scheme}: {other:?}"),
    }
}

#[test] // spec: APP-020, APP-063
fn launch_args_are_intent_then_a_base64url_url() {
    let reg = AppRegistration {
        app: "image".into(),
        exe: PathBuf::from(r"C:\Suite\lil-image.exe"),
        args: vec!["--flag".into(), "two words".into()],
        opens: vec![OpensEntry {
            types: Vec::new(),
            formats: Vec::new(),
            intents: vec![Intent::Open, Intent::Edit, Intent::UseAsInput],
        }],
        external: false,
    };
    let scheme = "lil-test-0a1b";
    let sealed = format!(
        "lil://0123456789abcdef/sketchbook/kite@1.0.0#sha256={}",
        "ab".repeat(32)
    );
    for (text, intent, word) in [
        (sealed.as_str(), Intent::Open, "open"),
        ("doc:d_1/proposal/r_20261018_0900/1", Intent::Edit, "edit"),
        (
            r"C:\pics\a picture #1 ünï.png",
            Intent::UseAsInput,
            "use_as_input",
        ),
    ] {
        let target = Target::parse(text).unwrap();
        let args = launch_args(&reg, &target, intent, scheme);
        assert_eq!(args.len(), 5, "{args:?}");
        assert_eq!(
            args[..4],
            ["--flag", "two words", "--intent", word],
            "{args:?}"
        );
        let b64 = args[4]
            .strip_prefix(&format!("{scheme}:open/"))
            .unwrap_or_else(|| panic!("{args:?}"));
        assert!(
            b64.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "base64url without padding: {args:?}"
        );
        assert_eq!(opened_by(&args[4], scheme), text);
    }
}

#[tokio::test] // spec: APP-020
async fn a_target_is_at_most_4096_bytes() {
    let r = rig();
    register(&r.core_dir, "test", "test.exe", "", "", r#""open""#);
    let agent = r.ctx(CallerKind::Cli);
    // 4,096 bytes is the longest target an open/ URL carries.
    let longest = format!("{}.png", "a".repeat(4092));
    let args = json!({"app": "test", "target": longest, "intent": "open"});
    let opened = common::call(&r.reg, &agent, "open_in", args).await.unwrap();
    let url = opened["launch_command"]["args"][3].as_str().unwrap();
    assert_eq!(opened_by(url, &r.env.env.activation_scheme()), longest);
    // Fewer than 4,096 characters, but more bytes.
    let wide = format!("{}.png", "ü".repeat(2047));
    assert!(wide.chars().count() < 4096 && wide.len() > 4096);
    let args = json!({"app": "test", "target": wide, "intent": "open"});
    let refused = common::call(&r.reg, &agent, "open_in", args)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "params.invalid");
}

/// A sealed ref with a content hash, in a workspace that needn't be open.
fn sealed(asset: &str) -> String {
    format!(
        "lil://0123456789abcdef/sketchbook/{asset}@1.0.0#sha256={}",
        "ab".repeat(32)
    )
}

/// The file names in `dir`.
fn names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[tokio::test] // spec: APP-021
async fn lilref_write_path_and_contents() {
    let r = rig();
    let agent = r.ctx(CallerKind::Mcp);
    let kite = sealed("kite");
    let args = json!({"ref": kite, "title": "Kite, sealed"});
    let written = common::call(&r.reg, &agent, "lilref.write", args)
        .await
        .unwrap();
    let path = PathBuf::from(written["path"].as_str().unwrap());
    let name = format!("{}.lilref", &sha256_bytes(kite.as_bytes())[..16]);
    assert_eq!(path, r.env.env.lilref_dir().join(&name), "{written}");
    let parsed = lil_schema::parse_lilref(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.r#ref, kite);
    assert_eq!(parsed.title.as_deref(), Some("Kite, sealed"));

    // The same ref again replaces its file; a ref from before the rename is written in the
    // canonical form, so it is the same file.
    common::call(&r.reg, &agent, "lilref.write", json!({ "ref": kite }))
        .await
        .unwrap();
    let parsed = lil_schema::parse_lilref(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.title, None);
    let legacy = kite.replacen(lil_schema::SCHEME, lil_schema::LEGACY_SCHEME, 1);
    let again = common::call(&r.reg, &agent, "lilref.write", json!({ "ref": legacy }))
        .await
        .unwrap();
    assert_eq!(again["path"], written["path"], "{again}");
    assert_eq!(
        names_in(&r.env.env.lilref_dir()),
        [name],
        "one file, no temp left"
    );
}

#[tokio::test] // spec: APP-021
async fn lilref_write_refuses_unsealed_refs() {
    let r = rig();
    let agent = r.ctx(CallerKind::Mcp);
    for bad in [
        "lil://0123456789abcdef/sketchbook/kite@1.0.0",
        "lil://0123456789abcdef/sketchbook/kite@candidate/r_20261002_0915/a.png",
        "lil://nonsense",
        r"C:\pics\a.png",
    ] {
        let refused = common::call(&r.reg, &agent, "lilref.write", json!({ "ref": bad }))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "ref.invalid", "{bad}");
    }
    for title in [
        "x".repeat(201),
        "a\nb".to_string(),
        "\u{202E}gnp.exe".to_string(),
    ] {
        let args = json!({"ref": sealed("kite"), "title": title});
        let refused = common::call(&r.reg, &agent, "lilref.write", args)
            .await
            .unwrap_err();
        assert_eq!(refused.code, "params.invalid", "{title:?}");
    }
    assert!(
        !r.env.env.lilref_dir().exists(),
        "nothing was written for a refused call"
    );
}

#[test] // spec: APP-021, ARCH-112
fn lilref_write_stays_in_the_cache_folder() {
    use lil_core::store::fsops::write_suite_cache_file;
    let env = TestEnv::new();
    let cache = env.env.lilref_dir();
    let diagnostics = env.env.diagnostics_dir();
    write_suite_cache_file(&env.env, &cache.join("a.lilref"), b"one").unwrap();
    write_suite_cache_file(&env.env, &cache.join("a.lilref"), b"two").unwrap();
    assert_eq!(std::fs::read(cache.join("a.lilref")).unwrap(), b"two");
    assert_eq!(names_in(&cache), ["a.lilref"], "no temp or backup left");
    write_suite_cache_file(&env.env, &diagnostics.join("bundle.zip"), b"zip").unwrap();
    assert_eq!(
        std::fs::read(diagnostics.join("bundle.zip")).unwrap(),
        b"zip"
    );
    let local = env.env.local_dir();
    for outside in [
        local.join("cache").join("a.lilref"),
        local.join(r"cache\lilref\..\escaped.lilref"),
        local.join(r"cache\lilref\.\dot.lilref"),
        cache.join("sub").join("a.lilref"),
        env.root().join("elsewhere.lilref"),
        cache.clone(),
        PathBuf::from("a.lilref"),
        // Names Windows reads as something else (review M1).
        cache.join("a.lilref:evil"),
        cache.join("a*.lilref"),
        cache.join("a?.lilref"),
        cache.join("a\"b.lilref"),
        cache.join("a<b.lilref"),
        cache.join("a>b.lilref"),
        cache.join("a|b.lilref"),
        cache.join("a\u{1}b.lilref"),
        cache.join("a.lilref."),
        cache.join("a.lilref "),
        cache.join("NUL"),
        cache.join("nul.lilref"),
        cache.join("Con.txt"),
        cache.join("aux"),
        cache.join("PRN.zip"),
        cache.join("com1.lilref"),
        cache.join("LPT9"),
        cache.join("nul .lilref"),
        cache.join("COM\u{B9}.lilref"),
        cache.join("lpt\u{B3}"),
        cache.join("CONIN$"),
        cache.join("conout$.txt"),
    ] {
        let refused = write_suite_cache_file(&env.env, &outside, b"x").unwrap_err();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{}",
            outside.display()
        );
    }
    assert!(!local.join("cache").join("a.lilref").exists());
    assert!(!local.join("cache").join("escaped.lilref").exists());
    assert!(!cache.join("sub").exists());
    // The folder is compared as Windows compares names, ignoring case.
    let upper = PathBuf::from(cache.to_string_lossy().to_uppercase()).join("b.lilref");
    write_suite_cache_file(&env.env, &upper, b"b").unwrap();
    assert_eq!(std::fs::read(cache.join("b.lilref")).unwrap(), b"b");
    // Names that only look like device names are fine.
    for fine in ["console.lilref", "com10.lilref", "nul-x.lilref", "lpt.zip"] {
        write_suite_cache_file(&env.env, &cache.join(fine), b"f").unwrap();
    }
}

/// Review M2: a crash's temp files older than a day are swept when the folder is written
/// next; newer ones, and other files, stay. A backup file is never swept (Checkpoint A store
/// M1): one is left only when a failed replace couldn't put the old file back, the only copy.
#[test] // spec: APP-021, ARCH-119
#[allow(clippy::disallowed_methods)] // ages the leftovers it plants
fn suite_cache_leftovers_older_than_a_day_are_swept() {
    use lil_core::store::fsops::write_suite_cache_file;
    let env = TestEnv::new();
    let cache = env.env.lilref_dir();
    let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
    for name in [
        ".a.lilref.1-1.tmp",
        ".a.lilref.1-2.bak",
        ".b.lilref.1-3.tmp",
        ".someone-else.tmp",
        "old.lilref",
    ] {
        write_file(&cache.join(name), b"x");
        if name != ".b.lilref.1-3.tmp" {
            std::fs::File::options()
                .write(true)
                .open(cache.join(name))
                .unwrap()
                .set_modified(two_days_ago)
                .unwrap();
        }
    }
    write_suite_cache_file(&env.env, &cache.join("c.lilref"), b"c").unwrap();
    let mut left = names_in(&cache);
    left.sort();
    assert_eq!(
        left,
        [
            ".a.lilref.1-2.bak",
            ".b.lilref.1-3.tmp",
            ".someone-else.tmp",
            "c.lilref",
            "old.lilref"
        ]
    );
}

/// Checkpoint A store M3: a `.lilref` someone holds open without `FILE_SHARE_DELETE` (a reader
/// or a scanner) is replaced once they let go, within the store's backoff, instead of failing
/// at once. A smoke test of the real files: whether the writer meets the held file first rests
/// on timing, so the retry itself is tested deterministically by `fsops`'
/// `a_sharing_violation_is_retried_and_nothing_else_is` (review M-5).
#[test] // spec: APP-021, ARCH-074
#[allow(clippy::disallowed_methods)] // holds the file open without FILE_SHARE_DELETE
fn a_sharing_violation_on_the_cache_writer_is_retried() {
    use lil_core::store::fsops::write_suite_cache_file;
    use std::os::windows::fs::OpenOptionsExt as _;
    let env = TestEnv::new();
    let cache = env.env.lilref_dir();
    write_suite_cache_file(&env.env, &cache.join("a.lilref"), b"one").unwrap();
    // FILE_SHARE_READ only.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(cache.join("a.lilref"))
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let reader = std::thread::spawn(move || {
        // Held until the writer is under way (a failure bound, not a measurement).
        let _ = rx.recv_timeout(Duration::from_millis(300));
        drop(held);
    });
    let written = write_suite_cache_file(&env.env, &cache.join("a.lilref"), b"two");
    drop(tx);
    reader.join().unwrap();
    written.unwrap();
    assert_eq!(std::fs::read(cache.join("a.lilref")).unwrap(), b"two");
    assert_eq!(names_in(&cache), ["a.lilref"], "no temp or backup left");
}

/// Checkpoint A store M3: a temp file a crashed core left under this process's pid (Windows
/// reuses pids) doesn't block the next write: its name is skipped.
#[test] // spec: APP-021, ARCH-074
fn a_crashed_cores_temp_name_is_skipped() {
    use lil_core::store::fsops::write_suite_cache_file;
    let env = TestEnv::new();
    let cache = env.env.lilref_dir();
    for n in 1..=5 {
        write_file(
            &cache.join(format!(".a.lilref.{}-{n}.tmp", std::process::id())),
            b"left",
        );
    }
    write_suite_cache_file(&env.env, &cache.join("a.lilref"), b"new").unwrap();
    assert_eq!(std::fs::read(cache.join("a.lilref")).unwrap(), b"new");
}

/// The interchange rows (CORE-146; Appendix G): `handlers.for`, `open_in` and `ui.show` read,
/// `lilref.write` and `notify.claude_code` write, all in `interchange`.
#[test] // spec: CORE-146, CORE-058, APP-021
fn the_interchange_tools_are_pinned() {
    let r = rig();
    let mut tools: Vec<(String, String, String)> = r
        .reg
        .descriptors()
        .into_iter()
        .map(|d| (d.name, format!("{:?}", d.effect), d.module))
        .collect();
    tools.sort();
    let row = |name: &str, effect: &str| {
        (
            name.to_string(),
            effect.to_string(),
            "interchange".to_string(),
        )
    };
    assert_eq!(
        tools,
        [
            row("handlers.for", "Read"),
            row("lilref.write", "Write"),
            row("notify.claude_code", "Write"),
            row("open_in", "Read"),
            row("ui.show", "Read"),
        ]
    );
}
