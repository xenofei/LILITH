//! Plan 4b's ComfyUI inbox: a new file in an owned engine's `output` folder is copied to the
//! default workspace's `inbox/`, untrusted, with the metadata read from its PNG text chunk;
//! `inbox.keep` imports it and `inbox.dismiss` only marks it; the suite's own job outputs are not
//! inboxed (spec CORE-179, ARCH-060, CORE-130; Appendix H "ComfyUI inbox", "Trust"). Each test
//! first waits until `inbox.list`'s `watch.state` is `on`, then writes straight into the owned
//! `lil-fake-comfyui` engine's `<root>\output`. Images are made with `lil_testkit::png`. Driven
//! through the engine and inbox tools from a `cli` connection and the files on disk (QA-018).

mod p4b;
mod support;

use std::path::{Path, PathBuf};

use lil_testkit::{CoreFixture, RawPipe, TestEnv, ensure_built, png, sha256_bytes};
use p4b::*;
use serde_json::{Value, json};
use support::*;

/// A ComfyUI API-format `prompt`: a sampler with seed 4242, a checkpoint loader and the two
/// text encoders its `positive` and `negative` links reach, decoded and saved. Appendix H takes
/// `model` from "the first `CheckpointLoader`- or `UNETLoader`-class node"; the loader here is
/// `CheckpointLoaderSimple`, the class real ComfyUI graphs use, read as a `class_type` that
/// starts with `CheckpointLoader`.
const PROMPT: &str = r#"{
  "3": {"class_type": "KSampler", "inputs": {"seed": 4242, "steps": 20, "model": ["4", 0],
        "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0]}},
  "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sample-model.safetensors"}},
  "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 8, "height": 8, "batch_size": 1}},
  "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a paper crane at dawn", "clip": ["4", 1]}},
  "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["4", 1]}},
  "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
  "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "ComfyUI"}}
}"#;

/// A served default workspace with an owned, ready `lil-fake-comfyui` engine.
struct Inboxed {
    pipe: RawPipe,
    _core: CoreFixture,
    ws: PathBuf,
    output: PathBuf,
    _env: TestEnv,
}

impl Inboxed {
    /// Starts the core and the engine, then waits until the inbox watches the output folder.
    fn start() -> Inboxed {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("p4b");
        env.register_workspace(&ws, true);
        let root = env.root().join("comfy-owned");
        let output = root.join("output");
        std::fs::create_dir_all(&output).unwrap_or_else(|e| panic!("{}: {e}", output.display()));
        std::fs::write(root.join("main.py"), b"").unwrap_or_else(|e| panic!("main.py: {e}"));
        env.suite_toml_set(&format!(
            "[engines.comfyui]\nroot = '{}'\npython = '{}'\nport = {}\nautostart = false\n",
            path_str(&root),
            path_str(&ensure_built().fake_comfyui),
            lil_testkit::free_loopback_port()
        ));
        let core = CoreFixture::start_with_env(&env, &[("LIL_TEST_INBOX_STABLE_MS", "300")]);
        let mut pipe = session(&env, Some(&ws));
        write_ok(&mut pipe, "engines.start", json!({"engine": "comfyui"}));
        until_engine(&mut pipe, "ready", |e| e["state"] == "ready");
        poll("inbox.list's watch.state on", || {
            let list = ok(&mut pipe, "inbox.list", json!({}), None);
            (list["watch"]["state"] == "on").then_some(())
        });
        Inboxed {
            pipe,
            _core: core,
            ws,
            output,
            _env: env,
        }
    }

    /// Writes `bytes` as `<output>\<name>`, as ComfyUI saves an image.
    fn write_output(&self, name: &str, bytes: &[u8]) {
        lil_testkit::files::write_file(&self.output.join(name), bytes);
    }

    /// The `inbox.list` entry named `name`, once there is one.
    fn until_listed(&mut self, name: &str) -> Value {
        poll(&format!("{name} in the inbox"), || {
            let list = ok(&mut self.pipe, "inbox.list", json!({}), None);
            list["entries"]
                .as_array()
                .and_then(|e| e.iter().find(|e| e["name"] == name))
                .cloned()
        })
    }

    /// Every entry of `inbox.list`.
    fn entries(&mut self) -> Vec<Value> {
        let list = ok(&mut self.pipe, "inbox.list", json!({"limit": 500}), None);
        list["entries"]
            .as_array()
            .unwrap_or_else(|| panic!("no entries: {list}"))
            .clone()
    }

    /// `inbox/<import_id>/<file>` of the workspace.
    fn inbox_file(&self, import_id: &str, file: &str) -> PathBuf {
        ws_path(&self.ws, &format!("inbox/{import_id}/{file}"))
    }
}

/// A small PNG holding the ComfyUI `prompt` text chunk.
fn comfy_png(shade: u8) -> Vec<u8> {
    png::with_text(&png::rgba(8, 8, &[shade; 8 * 8 * 4]), "prompt", PROMPT)
}

/// The `import_id` of an inbox entry, checked to be `in_<16 hex>`.
fn import_id_of(entry: &Value) -> String {
    let id = entry["import_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no import_id: {entry}"))
        .to_string();
    assert!(
        id.strip_prefix("in_")
            .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit())),
        "{entry}"
    );
    id
}

#[test] // spec: CORE-179, ARCH-060
fn a_new_comfyui_output_lands_in_the_inbox_untrusted() {
    let mut inbox = Inboxed::start();
    let bytes = comfy_png(40);
    inbox.write_output("ComfyUI_00001_.png", &bytes);

    let entry = inbox.until_listed("ComfyUI_00001_.png");
    let id = import_id_of(&entry);
    assert_eq!(entry["untrusted"], true, "{entry}");
    assert_eq!(entry["source"], "comfyui_output", "{entry}");
    assert_eq!(entry["state"], "new", "{entry}");
    assert_eq!(entry["media_kind"], "image", "{entry}");
    assert_eq!(entry["sha256"], sha256_bytes(&bytes).as_str(), "{entry}");
    assert_eq!(entry["metadata_state"], "read", "{entry}");
    let meta = &entry["metadata"];
    assert_eq!(meta["untrusted"], true, "{entry}");
    assert_eq!(meta["prompt"], "a paper crane at dawn", "{entry}");
    assert_eq!(meta["negative"], "blurry", "{entry}");
    assert_eq!(meta["model"], "sample-model.safetensors", "{entry}");
    assert_eq!(meta["seed"], 4242, "{entry}");

    // Copied, never moved.
    assert_eq!(
        std::fs::read(inbox.inbox_file(&id, "ComfyUI_00001_.png")).unwrap_or_default(),
        bytes
    );
    assert!(
        inbox.output.join("ComfyUI_00001_.png").is_file(),
        "the output stays"
    );
    let record: Value = serde_json::from_slice(
        &std::fs::read(inbox.inbox_file(&id, "inbox.json")).unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("inbox.json is not JSON: {e}"));
    assert_eq!(record["import_id"], id.as_str(), "{record}");
    assert_eq!(record["untrusted"], true, "{record}");
}

#[test] // spec: CORE-179, ARCH-060
fn keep_imports_and_dismiss_only_marks() {
    let mut inbox = Inboxed::start();
    inbox.write_output("ComfyUI_00001_.png", &comfy_png(40));
    inbox.write_output("ComfyUI_00002_.png", &comfy_png(80));
    let kept = import_id_of(&inbox.until_listed("ComfyUI_00001_.png"));
    let dismissed = import_id_of(&inbox.until_listed("ComfyUI_00002_.png"));

    let imported = write_ok(
        &mut inbox.pipe,
        "inbox.keep",
        json!({"import_id": kept, "project": PROJECT, "asset_id": "prop_inboxed",
               "type": "image"}),
    );
    let run_id = run_id_of(&imported);
    let manifest = manifest_of(&inbox.ws, "prop_inboxed");
    let run = run_of(&manifest, &run_id);
    assert_eq!(run["recipe"]["tool"], "comfyui", "{run}");
    assert_eq!(run["recipe"]["source"], "inbox", "{run}");
    assert_eq!(run["recipe"]["untrusted"], true, "{run}");
    assert_eq!(run["recipe"]["seed"], 4242, "{run}");
    let trust = manifest["trust"]
        .as_object()
        .unwrap_or_else(|| panic!("no trust map: {manifest}"));
    assert!(
        !trust.is_empty()
            && trust
                .values()
                .all(|r| r["state"] == "untrusted" && r["sources"][0]["kind"] == "inbox"),
        "{manifest}"
    );

    let out = ok(
        &mut inbox.pipe,
        "inbox.dismiss",
        json!({ "import_ids": [dismissed] }),
        Some(&key()),
    );
    assert!(!out["dismissed"].is_null(), "{out}");

    let entries = inbox.entries();
    let state = |id: &str| {
        entries
            .iter()
            .find(|e| e["import_id"] == id)
            .unwrap_or_else(|| panic!("no {id}: {entries:?}"))
            .clone()
    };
    assert_eq!(state(&kept)["state"], "kept", "{entries:?}");
    assert!(!state(&kept)["kept_as"].is_null(), "{entries:?}");
    assert_eq!(state(&dismissed)["state"], "dismissed", "{entries:?}");
    assert!(
        inbox.inbox_file(&dismissed, "ComfyUI_00002_.png").is_file(),
        "a dismissed entry's files stay"
    );

    let unknown = inbox.pipe.tool(
        "inbox.keep",
        json!({"import_id": "in_0000000000000000", "project": PROJECT,
               "asset_id": "prop_nothing", "type": "image"}),
        Some(&key()),
    );
    assert_eq!(error_code(&unknown), "inbox.unknown", "{unknown}");
}

#[test] // spec: CORE-179, CORE-130
fn suite_job_outputs_are_not_inboxed() {
    let mut inbox = Inboxed::start();
    // The suite's own output first, then a ComfyUI file, inboxed; then a second ComfyUI file
    // written after that, inboxed too: by then the first file has had more than one chance.
    inbox.write_output("lil_0123456789abcdef_00001_.png", &comfy_png(40));
    inbox.write_output("ComfyUI_00003_.png", &comfy_png(80));
    inbox.until_listed("ComfyUI_00003_.png");
    inbox.write_output("ComfyUI_00004_.png", &comfy_png(120));
    inbox.until_listed("ComfyUI_00004_.png");

    let names: Vec<Value> = inbox.entries().iter().map(|e| e["name"].clone()).collect();
    assert!(
        !names.contains(&json!("lil_0123456789abcdef_00001_.png")),
        "a suite job's output is not inboxed: {names:?}"
    );
    assert!(no_inbox_copy(&inbox.ws, "lil_0123456789abcdef_00001_.png"));
}

/// No file named `name` under the workspace's `inbox/`.
fn no_inbox_copy(ws: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(ws.join("inbox")) else {
        return true;
    };
    entries
        .filter_map(Result::ok)
        .all(|e| !e.path().join(name).exists())
}
