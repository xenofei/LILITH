//! Golden round-trips of the synthetic fixture manifests (ARCH-076, ARCH-077, CORE-148, QA-048).
//!
//! The fixture workspace `crates/ww-spec-tests/fixtures/sample-workspace/` is made up (owner
//! decision 6): `prop_signal_lamp` has the lantern manifest's legacy structure, and
//! `chr_paper_crane` the modern I5 shape. Both manifests are stored in `Manifest::to_text`
//! format with `\n` line endings; the nested `fixtures/.gitattributes` (`* -text`) keeps git
//! from converting them.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use ww_schema::manifest::{Manifest, remove_key};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ww-spec-tests")
        .join("fixtures")
}

fn manifest_path(asset: &str) -> PathBuf {
    fixtures()
        .join("sample-workspace/projects/sketchbook/assets")
        .join(asset)
        .join(format!("{asset}.manifest.json"))
}

fn read(asset: &str) -> String {
    let bytes = std::fs::read(manifest_path(asset)).expect("read fixture manifest");
    String::from_utf8(bytes).expect("UTF-8 fixture")
}

const ASSETS: [&str; 2] = ["prop_signal_lamp", "chr_paper_crane"];

/// The lines that differ between two texts with the same number of lines.
fn changed_lines(a: &str, b: &str) -> Vec<(String, String)> {
    let la: Vec<&str> = a.split('\n').collect();
    let lb: Vec<&str> = b.split('\n').collect();
    assert_eq!(la.len(), lb.len(), "line count differs");
    la.iter()
        .zip(&lb)
        .filter(|(x, y)| x != y)
        .map(|(x, y)| (x.to_string(), y.to_string()))
        .collect()
}

fn keys(m: &Manifest) -> Vec<String> {
    m.as_map().keys().cloned().collect()
}

#[test] // spec: ARCH-076, ARCH-077, QA-048
fn golden_manifests_round_trip_byte_for_byte() {
    for asset in ASSETS {
        let text = read(asset);
        let (m, _) = Manifest::parse(&text).unwrap();
        assert_eq!(m.to_text(), text, "{asset}");
    }
    // The sealed snapshot is stored in the same format.
    let snap = std::fs::read_to_string(fixtures().join(
        "sample-workspace/projects/sketchbook/assets/chr_paper_crane/v1.0.0/manifest.snapshot.json",
    ))
    .unwrap();
    assert_eq!(Manifest::parse(&snap).unwrap().0.to_text(), snap);
}

#[test] // spec: ARCH-076, QA-048
fn one_field_change_is_a_one_line_diff() {
    for asset in ASSETS {
        let text = read(asset);
        let (mut m, _) = Manifest::parse(&text).unwrap();
        m.set("status", json!("in_review"));
        let out = m.to_text();
        assert_eq!(
            changed_lines(&text, &out),
            [(
                "  \"status\": \"candidate\",".to_string(),
                "  \"status\": \"in_review\",".to_string()
            )],
            "{asset}"
        );
    }
}

#[test] // spec: CORE-148, ARCH-076
fn unknown_and_retired_fields_survive_edits() {
    let text = read("prop_signal_lamp");
    let (mut m, _) = Manifest::parse(&text).unwrap();
    let before = keys(&m);
    let watched = [
        "license_scope",
        "commercial_use",
        "content_rating",
        "budget_usd",
        "legacy_tool",
        "status_reason",
        "files",
        "notes",
    ];
    let values: Vec<Value> = watched.iter().map(|k| m.get(k).unwrap().clone()).collect();
    assert_eq!(m.get("status_reason"), Some(&Value::Null));
    assert_eq!(
        m.get("legacy_tool").unwrap()["settings"]["extras"],
        Value::Null
    );

    m.set("status", json!("in_review"));
    m.push(
        "candidate_runs",
        json!({"run_id": "r_20261005_0800", "status": "open"}),
    )
    .unwrap();
    m.push("locks", json!("prompts/signal_lamp.txt")).unwrap();
    m.set("updated", json!("2026-10-05T08:00:00Z"));

    let after = keys(&m);
    // Existing keys keep their positions; new ones come at the end, in order.
    assert_eq!(&after[..before.len()], &before[..]);
    assert_eq!(
        &after[before.len()..],
        ["candidate_runs", "locks", "updated"]
    );
    for (k, v) in watched.iter().zip(&values) {
        assert_eq!(m.get(k), Some(v), "{k}");
    }
    // And the text after a round-trip still has them, byte for byte on their lines.
    let out = m.to_text();
    let (again, _) = Manifest::parse(&out).unwrap();
    assert_eq!(keys(&again), after);
    for line in text.lines() {
        if line.contains("\"status\": ") || line == "}" || line.starts_with("  \"created\"") {
            continue;
        }
        assert!(out.contains(line), "lost line {line:?}");
    }
}

#[test] // spec: ARCH-076, QA-044
fn hand_formatted_manifest_round_trips_by_value_and_order() {
    for asset in ASSETS {
        let text = read(asset);
        let value: Value = serde_json::from_str(&text).unwrap();
        let compact = serde_json::to_string(&value).unwrap();
        assert!(!compact.contains('\n'));
        let (m, _) = Manifest::parse(&compact).unwrap();
        let (golden, _) = Manifest::parse(&text).unwrap();
        assert_eq!(keys(&m), keys(&golden), "{asset}");
        assert_eq!(serde_json::to_string(m.as_map()).unwrap(), compact);
        // Written out, it is the golden text again.
        assert_eq!(m.to_text(), text, "{asset}");
    }
    // Odd spacing too.
    let odd = "{ \"b\" :1 ,\n\t\"a\": [ 1,2 ] , \"c\":{\"z\":null,\"y\":true} }";
    let (m, _) = Manifest::parse(odd).unwrap();
    assert_eq!(keys(&m), ["b", "a", "c"]);
    assert_eq!(
        serde_json::to_string(m.as_map()).unwrap(),
        "{\"b\":1,\"a\":[1,2],\"c\":{\"z\":null,\"y\":true}}"
    );
}

#[test] // spec: CORE-148, CORE-033, ARCH-076
fn legacy_manifest_reads_with_warnings_not_errors() {
    let (m, warnings) = Manifest::parse(&read("prop_signal_lamp")).unwrap();
    assert!(
        warnings.iter().any(|w| w.pointer == "/schema_version"),
        "{warnings:?}"
    );
    let v = m.view();
    assert_eq!(v.schema_version, None);
    assert_eq!(v.asset_id, "prop_signal_lamp");
    assert_eq!(v.project, "sketchbook");
    assert_eq!(v.status, Some(ww_schema::Status::Candidate));
    assert!(v.candidate_runs.is_empty());
    assert_eq!(v.creation_uuid, None);
    assert_eq!(v.hashes.len(), 5);

    let (_, modern) = Manifest::parse(&read("chr_paper_crane")).unwrap();
    assert!(modern.is_empty(), "{modern:?}");
}

#[test] // spec: CORE-148, ARCH-076
fn edits_never_add_schema_version_to_a_legacy_manifest() {
    let (mut m, _) = Manifest::parse(&read("prop_signal_lamp")).unwrap();
    m.set("status", json!("in_review"));
    m.push("locks", json!("v1.0.0/prop_signal_lamp.png"))
        .unwrap();
    m.remove("status_reason");
    let out = m.to_text();
    assert!(!out.contains("schema_version"), "{out}");
    assert!(m.get("schema_version").is_none());
}

/// Every file under `dir`, recursively.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test] // spec: QA-048, CORE-040
fn fixture_files_have_no_crlf() {
    let mut files = Vec::new();
    walk(&fixtures(), &mut files);
    let text_ext = ["json", "yaml", "yml", "toml", "md", "txt"];
    let mut checked = 0;
    for file in &files {
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        // Binary fixtures are skipped: every PNG signature contains 0D 0A.
        if !ext.is_some_and(|e| text_ext.contains(&e.as_str())) {
            continue;
        }
        let bytes = std::fs::read(file).unwrap();
        assert!(
            !bytes.windows(2).any(|w| w == b"\r\n"),
            "{} has CRLF line endings",
            file.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "found only {checked} text fixtures under {}",
        fixtures().display()
    );
}

#[test] // spec: ARCH-076, CORE-040
fn crlf_manifest_stays_crlf_and_a_one_field_edit_is_one_line() {
    let crlf = read("chr_paper_crane").replace('\n', "\r\n");
    let (mut m, _) = Manifest::parse(&crlf).unwrap();
    assert_eq!(m.eol(), "\r\n");
    assert_eq!(m.to_text(), crlf);
    m.set("status", json!("in_review"));
    let out = m.to_text();
    assert_eq!(out.matches('\n').count(), out.matches("\r\n").count());
    assert_eq!(
        changed_lines(&crlf, &out),
        [(
            "  \"status\": \"candidate\",\r".to_string(),
            "  \"status\": \"in_review\",\r".to_string()
        )]
    );
}

#[test] // spec: ARCH-076, CORE-148
fn removing_a_key_keeps_the_order_of_the_rest() {
    let (mut m, _) = Manifest::parse(&read("chr_paper_crane")).unwrap();
    let mut want = keys(&m);
    let middle = want.iter().position(|k| k == "status").unwrap();
    assert!(middle > 0 && middle < want.len() - 1);
    assert_eq!(m.remove("status"), Some(json!("candidate")));
    want.remove(middle);
    assert_eq!(keys(&m), want);
    assert_eq!(m.remove("status"), None);

    // A nested map: remove a middle key of a run.
    let mut runs = m.runs_mut();
    let run = &mut runs[0];
    let mut run_keys: Vec<String> = run.keys().cloned().collect();
    let at = run_keys.iter().position(|k| k == "created").unwrap();
    assert!(at > 0 && at < run_keys.len() - 1);
    assert_eq!(
        remove_key(run, "created"),
        Some(json!("2026-10-02T09:15:00Z"))
    );
    run_keys.remove(at);
    assert_eq!(run.keys().cloned().collect::<Vec<_>>(), run_keys);
    assert_eq!(remove_key(run, "created"), None);
}

#[test] // spec: ARCH-076, QA-048
fn seventeen_digit_floats_round_trip_byte_for_byte() {
    // Shortest-form f64 texts that need 17 significant digits, and the extremes; without
    // serde_json's `float_roundtrip` some of them came back one digit shorter.
    for f in [
        "0.47960756426982587",
        "-2.323068369499641e-11",
        "1.7976931348623157e+308",
        "5e-324",
        "2.2250738585072014e-308",
        "0.30000000000000004",
        "123456.78901234567",
    ] {
        let text = format!("{{\n  \"budget_usd\": {f},\n  \"n\": [\n    {f}\n  ]\n}}\n");
        let (m, _) = Manifest::parse(&text).unwrap();
        assert_eq!(m.to_text(), text, "{f}");
    }
}

/// Lowercase hex SHA-256 of a fixture file.
fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    ww_schema::sha256_hex(&bytes)
}

#[test] // spec: ARCH-077, ARCH-078, QA-048
fn fixture_hashes_match_the_bytes_on_disk() {
    let assets = fixtures().join("sample-workspace/projects/sketchbook/assets");
    let mut checked = 0;
    for asset in ASSETS {
        let folder = assets.join(asset);
        let (m, _) = Manifest::parse(&read(asset)).unwrap();
        let view = m.view();
        assert!(!view.hashes.is_empty(), "{asset}");
        for (rel, hex) in &view.hashes {
            assert_eq!(&sha256_of(&folder.join(rel)), hex, "{asset}: hashes {rel}");
            checked += 1;
        }
        for run in &view.candidate_runs {
            for file in &run.files {
                let Some(hex) = &file.sha256 else { continue };
                let on_disk = folder.join(&file.path);
                let name = file.path.rsplit('/').next().unwrap();
                if run.status.as_deref() == Some("open") {
                    // An open run's files are all expected on disk.
                    assert_eq!(&sha256_of(&on_disk), hex, "{asset}: {}", file.path);
                } else if run.selected.iter().any(|s| s == name) {
                    // A chosen file was moved into the sealed version.
                    let version = run.signed_off.as_ref().unwrap().version;
                    let sealed = folder.join(format!("v{version}")).join(name);
                    assert!(!on_disk.exists(), "{}", file.path);
                    assert_eq!(&sha256_of(&sealed), hex, "{asset}: {}", file.path);
                } else {
                    assert_eq!(&sha256_of(&on_disk), hex, "{asset}: {}", file.path);
                }
                checked += 1;
            }
        }
    }
    // The sealed snapshot records the same hashes.
    let (snap, _) = Manifest::parse(
        &std::fs::read_to_string(assets.join("chr_paper_crane/v1.0.0/manifest.snapshot.json"))
            .unwrap(),
    )
    .unwrap();
    for (rel, hex) in &snap.view().hashes {
        assert_eq!(&sha256_of(&assets.join("chr_paper_crane").join(rel)), hex);
    }
    // 5 legacy hashes; 1 sealed hash and 4 run files of chr_paper_crane.
    assert_eq!(checked, 10, "every recorded hash was checked");
}
