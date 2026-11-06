//! Plan 4b's status log and deprecation: every status change appends one `status_log` entry in
//! the same manifest edit, and `asset.deprecate` and back returns to the status before (spec
//! CORE-156, CORE-170; Appendix H "Manifest fields", "Status log", "Deprecation"). Driven
//! through the asset tools from a `cli` connection and the manifests on disk (QA-018).

mod p4b;
mod support;

use lil_testkit::sha256_file;
use p4b::*;
use serde_json::{Value, json};
use support::*;

/// The new asset these tests import.
const KITE: &str = "prop_kite";

/// The manifest's `status_log`.
fn status_log(manifest: &Value) -> Vec<Value> {
    manifest["status_log"]
        .as_array()
        .unwrap_or_else(|| panic!("the manifest has no status_log: {manifest}"))
        .clone()
}

#[test] // spec: CORE-156
fn every_status_change_appends_one_entry() {
    let mut s = Served::start();
    let imported = import(&mut s.pipe, &s.env, KITE, &[("kite.png", b"kite one")]);
    let run_id = run_id_of(&imported);
    let after_import = s.manifest(KITE)["status"].clone();

    write_ok(
        &mut s.pipe,
        "asset.request_signoff",
        json!({"project": PROJECT, "asset_id": KITE}),
    );
    signoff(&mut s.pipe, KITE, &run_id, &["kite.png"]);
    let deprecated = write_ok(
        &mut s.pipe,
        "asset.deprecate",
        json!({"project": PROJECT, "asset_id": KITE, "deprecated": true}),
    );
    assert_eq!(deprecated["status"], "deprecated", "{deprecated}");
    let back = write_ok(
        &mut s.pipe,
        "asset.deprecate",
        json!({"project": PROJECT, "asset_id": KITE, "deprecated": false}),
    );
    assert_eq!(back["status"], "approved", "{back}");

    let manifest = s.manifest(KITE);
    let log = status_log(&manifest);
    let to: Vec<&Value> = log.iter().map(|e| &e["to"]).collect();
    assert_eq!(
        to,
        [
            &after_import,
            &json!("in_review"),
            &json!("approved"),
            &json!("deprecated"),
            &json!("approved"),
        ],
        "five entries, newest last: {manifest}"
    );
    for (i, entry) in log.iter().enumerate() {
        assert_eq!(entry["by"], "cli", "entry {i}: {entry}");
        let utc = entry["utc"].as_str().unwrap_or_default();
        assert!(
            utc.ends_with('Z') && utc.contains('T'),
            "entry {i}'s utc is RFC 3339 UTC: {entry}"
        );
        if i > 0 {
            assert_eq!(entry["from"], log[i - 1]["to"], "entry {i}: {manifest}");
        }
    }
    assert_eq!(manifest["status"], "approved", "{manifest}");

    // asset.get shows the same log.
    let detail = asset_get(&mut s.pipe, KITE);
    assert_eq!(detail["status_log"], manifest["status_log"], "{detail}");
}

#[test] // spec: CORE-170
fn deprecate_and_back_restores_the_previous_status() {
    // The crane uses the kite's first version: a dependent.
    let mut s = Served::start_with(
        |_, ws| {
            edit_manifest_text(
                ws,
                CRANE,
                r#""inputs": ["#,
                r#""inputs": [{"ref": "prop_kite@1.0.0", "role": "reference"}, "#,
            )
        },
        &[],
    );
    let imported = import(&mut s.pipe, &s.env, KITE, &[("kite.png", b"kite one")]);
    signoff(&mut s.pipe, KITE, &run_id_of(&imported), &["kite.png"]);
    assert_eq!(s.manifest(KITE)["status"], "approved");

    let sealed = s.asset_dir(KITE).join(r"v1.0.0\kite.png");
    let sealed_sha = sha256_file(&sealed);
    let hashes = s.manifest(KITE)["hashes"].clone();
    let dependent_inputs = s.manifest(CRANE)["inputs"].clone();
    let dependent_uses = asset_get(&mut s.pipe, CRANE)["uses"].clone();

    let deprecated = write_ok(
        &mut s.pipe,
        "asset.deprecate",
        json!({"project": PROJECT, "asset_id": KITE, "deprecated": true}),
    );
    assert_eq!(deprecated["status"], "deprecated", "{deprecated}");
    assert_eq!(s.manifest(KITE)["status"], "deprecated");

    let back = write_ok(
        &mut s.pipe,
        "asset.deprecate",
        json!({"project": PROJECT, "asset_id": KITE, "deprecated": false}),
    );
    assert_eq!(back["status"], "approved", "{back}");
    let manifest = s.manifest(KITE);
    assert_eq!(manifest["status"], "approved", "{manifest}");
    assert_eq!(manifest["approved_version"], "1.0.0", "{manifest}");

    // The asset's files and its dependent are untouched.
    assert_eq!(sha256_file(&sealed), sealed_sha);
    assert!(
        is_read_only(&sealed),
        "{} lost its read-only bit",
        sealed.display()
    );
    assert_eq!(manifest["hashes"], hashes, "{manifest}");
    assert_eq!(s.manifest(CRANE)["inputs"], dependent_inputs);
    assert_eq!(asset_get(&mut s.pipe, CRANE)["uses"], dependent_uses);
}
