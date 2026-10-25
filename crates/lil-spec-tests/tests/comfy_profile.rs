//! ComfyUI from plan 4a: the owned engine's launch profile and the refused asset-catalogue flag,
//! the out-of-memory advice after two retries, the model inventory report's states, and the
//! stored environment captures with their changes (spec ARCH-126, CORE-130, CORE-163, CORE-164;
//! Appendix G "ComfyUI launch profile", "Out of memory", "Model inventory report", "Engine
//! environments", rulings P4a-R15 and P4a-R28). Driven through `test.comfy_submit`, the engine
//! tools, the in-process `FakeComfy` (attached) or `lil-fake-comfyui` (owned), what the fakes
//! recorded and the files on disk (QA-018).
//!
//! The owned engine's command line is read from what `lil-fake-comfyui` records
//! (`fake-comfyui.json`'s `argv`), the fake's own record of how ComfyUI was started.

mod support;

use std::path::{Path, PathBuf};

use lil_testkit::{CoreFixture, FakeComfy, FakeFailure, TestEnv, ensure_built, sha256_file};
use serde_json::{Value, json};
use support::*;

/// The `[engines.comfyui]` of an owned engine: `root` a new folder holding an empty `main.py`,
/// `python` the fake ComfyUI, a free loopback port and `extra_args` when given. Returns the
/// root and the port.
fn own(env: &TestEnv, extra_args: Option<&str>) -> (PathBuf, u16) {
    let root = env.root().join("comfy-owned");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.py"), b"").unwrap();
    let port = lil_testkit::free_loopback_port();
    let extra = extra_args.map_or_else(String::new, |a| format!("extra_args = {a}\n"));
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nroot = '{}'\npython = '{}'\nport = {port}\nautostart = false\n{extra}",
        path_str(&root),
        path_str(&ensure_built().fake_comfyui),
    ));
    (root, port)
}

/// The argv `lil-fake-comfyui` recorded once it listened.
fn launched_argv(root: &Path) -> Vec<String> {
    let file = root.join("fake-comfyui.json");
    let recorded = poll(&format!("{} with listen", file.display()), || {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).ok()?).ok()?;
        v.get("listen").is_some().then_some(v)
    });
    recorded["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("no argv: {recorded}"))
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

#[test] // spec: ARCH-126
fn owned_comfyui_starts_with_the_launch_profile() {
    let env = TestEnv::new();
    let (root, port) = own(&env, None);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let started = pipe.tool("engines.start", json!({"engine": "comfyui"}), Some(&key()));
    assert_eq!(started["status"], "ok", "{started}");
    let argv = launched_argv(&root);
    let flags = ["--reserve-vram", "1.5", "--preview-method", "taesd"];
    let port = port.to_string();
    let head = ["main.py", "--listen", "127.0.0.1", "--port", port.as_str()];
    let at = argv
        .iter()
        .position(|a| a.ends_with("main.py"))
        .unwrap_or_else(|| panic!("no main.py in {argv:?}"));
    let after: Vec<&str> = argv[at + 1..].iter().map(String::as_str).collect();
    assert_eq!(after[..4], head[1..], "{argv:?}");
    // Plan 4a ruling, 2026-10-24: an owned start always ends with --disable-assets.
    assert_eq!(
        after[4..after.len() - 1],
        flags,
        "the profile flags, then no extra_args: {argv:?}"
    );
    assert_eq!(after.last(), Some(&"--disable-assets"), "{argv:?}");
    assert!(
        !argv
            .iter()
            .any(|a| a == "--enable-manager" || a == "--disable-dynamic-vram"),
        "neither the Manager flag nor the dynamic-VRAM opt-out by default: {argv:?}"
    );

    let engine = until_engine(&mut pipe, "ready", |e| e["state"] == "ready");
    let profile = &engine["launch_profile"];
    assert_eq!(profile["reserve_vram_gb"], "1.5", "{engine}");
    assert_eq!(profile["preview_method"], "taesd", "{engine}");
    assert_eq!(profile["manager"], false, "{engine}");
    assert_eq!(profile["dynamic_vram"], true, "{engine}");
    assert_eq!(profile["flags"], json!(flags), "{engine}");
    let catalogue = engine["asset_catalogue"].as_str().unwrap_or_default();
    assert!(
        ["on", "off", "unknown"].contains(&catalogue),
        "asset_catalogue: {engine}"
    );
}

#[test] // spec: ARCH-126
fn enable_assets_in_extra_args_stops_the_start() {
    for flag in ["--enable-assets", "--enable-as=1"] {
        let env = TestEnv::new();
        let (root, _) = own(&env, Some(&format!("[\"{flag}\"]")));
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, None);

        let refused = pipe.tool("engines.start", json!({"engine": "comfyui"}), Some(&key()));
        assert_eq!(
            error_code(&refused),
            "engine.not_configured",
            "{flag}: {refused}"
        );
        assert!(
            refused["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains(flag)),
            "the message names {flag}: {refused}"
        );
        assert!(
            !root.join("fake-comfyui.json").exists(),
            "a refused start starts no process"
        );
    }
}

#[test] // spec: CORE-163
fn out_of_memory_retries_twice_then_fails_with_advice() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    for _ in 0..3 {
        fake.fail_next(FakeFailure::Oom);
    }
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let id = submit(&mut pipe, "test.comfy_submit", json!({"graph": "tiny"}));
    let failed = wait_done(&mut pipe, &id);
    assert_eq!(failed["status"], "failed", "{failed}");
    assert_eq!(
        posted_prompts(&fake).len(),
        3,
        "the first try and two retries"
    );
    let error = &failed["error"];
    assert_eq!(error["code"], "comfyui.out_of_memory", "{failed}");
    assert_eq!(
        error["message"], "ComfyUI ran out of GPU memory, also on 2 retries.",
        "{failed}"
    );
    assert_eq!(
        error["fix"]["user_hint"],
        "Close other programs that use the GPU, or choose the model profile's lighter variant, then retry.",
        "{failed}"
    );
    assert_eq!(error["retry"], "after_resolve", "{failed}");
}

/// A safetensors file: the 8-byte little-endian header length, then the JSON header.
fn safetensors(header: &Value) -> Vec<u8> {
    let json = header.to_string();
    let mut bytes = (json.len() as u64).to_le_bytes().to_vec();
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

#[test] // spec: CORE-164
fn models_report_names_orphan_duplicate_and_unknown_family() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "models");
    let fake = FakeComfy::start();
    attach(&env, &fake);
    let models = fake.root().join("models");
    std::fs::create_dir_all(models.join("checkpoints")).unwrap();
    std::fs::create_dir_all(models.join("loras")).unwrap();
    let with_family =
        safetensors(&json!({"__metadata__": {"modelspec.architecture": "spec-arch-xl"}}));
    std::fs::write(models.join(r"checkpoints\spec_a.safetensors"), &with_family).unwrap();
    std::fs::write(
        models.join(r"checkpoints\spec_a_copy.safetensors"),
        &with_family,
    )
    .unwrap();
    std::fs::write(
        models.join(r"loras\spec_plain.safetensors"),
        safetensors(&json!({"__metadata__": {}})),
    )
    .unwrap();
    let absent = "ab".repeat(32);
    lil_testkit::files::write_file(
        &ws.join(r"registry\models.yaml"),
        format!("- id: spec_absent\n  name: Spec absent model\n  files:\n    - sha256: {absent}\n")
            .as_bytes(),
    );
    let before: Vec<(PathBuf, String)> = [
        "checkpoints\\spec_a.safetensors",
        "checkpoints\\spec_a_copy.safetensors",
        "loras\\spec_plain.safetensors",
    ]
    .iter()
    .map(|p| (models.join(p), sha256_file(&models.join(p))))
    .collect();

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));
    let report = poll("a refreshed models report", || {
        // An error outcome fails at once; only the refresh is waited for.
        let report = ok(&mut pipe, "models.report", json!({}), None);
        (report["models"].as_array().is_some_and(|m| m.len() == 3)).then_some(report)
    });

    let entry = |path: &str| -> Value {
        report["models"]
            .as_array()
            .and_then(|m| m.iter().find(|e| e["path"] == path))
            .cloned()
            .unwrap_or_else(|| panic!("no {path} in {report}"))
    };
    let states = |e: &Value| -> Vec<String> {
        let mut s: Vec<String> = e["states"]
            .as_array()
            .unwrap_or_else(|| panic!("no states: {e}"))
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        s.sort();
        s
    };
    for path in [
        r"checkpoints\spec_a.safetensors",
        r"checkpoints\spec_a_copy.safetensors",
    ] {
        let e = entry(path);
        assert_eq!(e["family"], "spec-arch-xl", "{e}");
        assert_eq!(states(&e), ["duplicate", "orphan"], "{e}");
    }
    let plain = entry(r"loras\spec_plain.safetensors");
    assert!(plain["family"].is_null(), "{plain}");
    assert_eq!(states(&plain), ["orphan", "unknown_family"], "{plain}");
    let missing = report["missing"]
        .as_array()
        .unwrap_or_else(|| panic!("no missing list: {report}"));
    assert_eq!(missing.len(), 1, "{report}");
    assert_eq!(missing[0]["model_id"], "spec_absent", "{report}");
    assert_eq!(missing[0]["sha256"], absent.as_str(), "{report}");

    for (path, sha) in before {
        assert_eq!(sha256_file(&path), sha, "{} changed", path.display());
    }
}

/// `custom_nodes\spec_node\pyproject.toml` at `version` under the fake's root.
fn node_version(fake: &FakeComfy, version: &str) {
    lil_testkit::files::write_file(
        &fake.root().join(r"custom_nodes\spec_node\pyproject.toml"),
        format!("[project]\nname = \"spec-node\"\nversion = \"{version}\"\n").as_bytes(),
    );
}

/// `engines.environment` once it holds two captures.
fn two_captures(pipe: &mut impl Caller) -> Value {
    poll("two engine captures", || {
        let env = ok(pipe, "engines.environment", json!({}), None);
        (env["captures"].as_array().is_some_and(|c| c.len() == 2)).then_some(env)
    })
}

#[test] // spec: CORE-130
fn engine_environment_reports_a_changed_node() {
    let env = TestEnv::new();
    let fake = FakeComfy::start();
    attach(&env, &fake);
    node_version(&fake, "1.0.0");
    {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, None);
        poll("the first capture", || {
            let view = ok(&mut pipe, "engines.environment", json!({}), None);
            (view["captures"].as_array().is_some_and(|c| !c.is_empty())).then_some(())
        });
    }
    node_version(&fake, "1.0.1");
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);

    let view = two_captures(&mut pipe);
    let changes = view["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("no changes: {view}"));
    assert_eq!(changes.len(), 1, "one node changed, nothing else: {view}");
    assert_eq!(changes[0]["what"], "node", "{view}");
    assert_eq!(changes[0]["change"], "changed", "{view}");
    // Appendix G doesn't say whether a node's `from` and `to` are its version, its commit or
    // both: each holds the version at least.
    let holds = |v: &Value, version: &str| v.as_str().is_some_and(|s| s.contains(version));
    assert!(holds(&changes[0]["from"], "1.0.0"), "{view}");
    assert!(holds(&changes[0]["to"], "1.0.1"), "{view}");
    assert!(
        changes[0]["name"]
            .as_str()
            .is_some_and(|n| n.contains("spec_node")),
        "{view}"
    );
}
