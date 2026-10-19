//! The app handler registry, `open_in` and `ui.show` (APP-061, APP-063–066): registrations
//! from both folders, handlers matched on type and format, a running app told without focus
//! for an agent, a launch command and no process for an app that isn't running, and the closed
//! set of intents. Driven through the registered tools on a `Registry`, with the connections
//! the production router reports ([`ConnObserver`]) simulated.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::interchange::{Interchange, load_registrations, register_interchange_tools};
use lil_core::server::ConnObserver;
use lil_core::session::Session;
use lil_core::tools::{CallCtx, Registry};
use lil_protocol::CallerKind;
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, TestEnv};
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
    let interchange = Interchange::new(core_dir.clone(), env.env.data_dir());
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

    let regs = load_registrations(&r.core_dir, &data_dir);
    let names: Vec<(&str, bool)> = regs.iter().map(|g| (g.app.as_str(), g.external)).collect();
    assert_eq!(names, [("test", false), ("viewer", true)]);
    // An installed app's relative exe is beside the core.
    assert_eq!(regs[0].exe, r.core_dir.join("test.exe"));
    assert_eq!(regs[1].exe, PathBuf::from(r"C:\Tools\viewer.exe"));
    assert_eq!(regs[1].args, ["--flag"]);
    assert!(load_registrations(&r.core_dir.join("nowhere"), &r.core_dir.join("nor")).is_empty());
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
    assert_eq!(
        result["launch_command"]["args"],
        json!(["--flag", format!("lil:open/{}", target.display())])
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
