//! Plan 4b's restore as a new version and version detail: `asset.restore_version` seals a copy
//! of an old version as the next version with `restored_from`, leaving the old one as it was, and
//! `asset.get_version` reads a version's snapshot (spec CORE-177, APP-141; Appendix H "Restore
//! as new version and version detail", "Manifest fields"). Images are made with
//! `lil_testkit::png`. Driven through the asset tools from a `cli` connection and the files on
//! disk (QA-018).

mod p4b;
mod support;

use lil_testkit::{png, sha256_bytes, sha256_file};
use p4b::*;
use serde_json::{Value, json};
use support::*;

const KITE: &str = "prop_kite";

/// A small PNG whose pixels are all `shade`.
fn picture(shade: u8) -> Vec<u8> {
    png::rgba(8, 8, &[shade; 8 * 8 * 4])
}

/// The kite signed off twice: `v1.0.0/kite.png` holds `picture(10)`, `v1.1.0/kite.png`
/// `picture(20)`. Returns the first version's bytes.
fn two_versions(s: &mut Served) -> Vec<u8> {
    let first = picture(10);
    let run = run_id_of(&import(&mut s.pipe, &s.env, KITE, &[("kite.png", &first)]));
    assert_eq!(
        signoff(&mut s.pipe, KITE, &run, &["kite.png"])["version"],
        "1.0.0"
    );
    let run = run_id_of(&import(
        &mut s.pipe,
        &s.env,
        KITE,
        &[("kite.png", &picture(20))],
    ));
    assert_eq!(
        signoff(&mut s.pipe, KITE, &run, &["kite.png"])["version"],
        "1.1.0"
    );
    first
}

#[test] // spec: CORE-177
fn restore_version_seals_a_copy_as_the_next_version() {
    let mut s = Served::start();
    let first = two_versions(&mut s);
    let old = s.asset_dir(KITE).join(r"v1.0.0\kite.png");
    let old_sha = sha256_file(&old);

    let restored = write_ok(
        &mut s.pipe,
        "asset.restore_version",
        json!({"project": PROJECT, "asset_id": KITE, "version": "1.0.0"}),
    );
    assert_eq!(
        restored["version"], "1.2.0",
        "a minor bump by default: {restored}"
    );
    assert_eq!(restored["restored_from"], "1.0.0", "{restored}");
    assert!(
        restored["ref"]
            .as_str()
            .is_some_and(|r| r.contains(&format!("/{PROJECT}/{KITE}@1.2.0#sha256="))),
        "{restored}"
    );
    assert!(restored["handoff"].is_object(), "{restored}");

    let copy = s.asset_dir(KITE).join(r"v1.2.0\kite.png");
    assert_eq!(
        std::fs::read(&copy).unwrap_or_else(|e| panic!("{}: {e}", copy.display())),
        first
    );
    assert!(is_read_only(&copy), "{}", copy.display());
    assert_eq!(sha256_file(&old), old_sha, "the old version is unchanged");
    assert!(is_read_only(&old), "{}", old.display());

    let manifest = s.manifest(KITE);
    assert_eq!(manifest["approved_version"], "1.2.0", "{manifest}");
    let runs = manifest["candidate_runs"]
        .as_array()
        .unwrap_or_else(|| panic!("no candidate_runs: {manifest}"));
    let run = runs
        .iter()
        .find(|r| r["signed_off"]["version"] == "1.2.0")
        .unwrap_or_else(|| panic!("no run signed off as 1.2.0: {manifest}"));
    assert_eq!(run["restored_from"], "1.0.0", "{run}");
    let log = manifest["status_log"]
        .as_array()
        .unwrap_or_else(|| panic!("no status_log: {manifest}"));
    let newest = log
        .last()
        .unwrap_or_else(|| panic!("an empty status_log: {manifest}"));
    assert_eq!(newest["reason"]["code"], "restored", "{newest}");
    assert_eq!(newest["reason"]["ref"], format!("{KITE}@1.0.0"), "{newest}");
}

#[test] // spec: CORE-177, APP-141
fn get_version_reads_the_snapshot() {
    let mut s = Served::start();
    let first = two_versions(&mut s);

    let version = ok(
        &mut s.pipe,
        "asset.get_version",
        json!({"project": PROJECT, "asset_id": KITE, "version": "1.0.0"}),
        None,
    );
    assert_eq!(version["version"], "1.0.0", "{version}");
    assert!(
        version["ref"]
            .as_str()
            .is_some_and(|r| r.contains(&format!("/{PROJECT}/{KITE}@1.0.0#sha256="))),
        "{version}"
    );
    let files = version["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files: {version}"));
    assert_eq!(files.len(), 1, "{version}");
    let file = &files[0];
    assert!(
        file["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("kite.png")),
        "{file}"
    );
    assert_eq!(file["sha256"], sha256_bytes(&first).as_str(), "{file}");
    assert_eq!(file["media_kind"], "image", "{file}");
    assert_eq!(file["primary"], true, "{file}");
    assert_eq!(version["signed_off"]["version"], "1.0.0", "{version}");
    assert_eq!(version["signed_off"]["by"], "cli", "{version}");
    assert!(
        version.get("restored_from").is_none_or(Value::is_null),
        "{version}"
    );

    // A legacy version: its files and hashes only.
    let legacy = ok(
        &mut s.pipe,
        "asset.get_version",
        json!({"project": PROJECT, "asset_id": LAMP, "version": "1.0.0"}),
        None,
    );
    let lamp = legacy["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files: {legacy}"))
        .iter()
        .find(|f| {
            f["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("prop_signal_lamp.png"))
        })
        .unwrap_or_else(|| panic!("no prop_signal_lamp.png: {legacy}"));
    assert_eq!(
        lamp["sha256"], "4564ff7231849624a5b632fff8c1e3e0219d4fb8612ef1bb83263001079c2772",
        "{legacy}"
    );
}
