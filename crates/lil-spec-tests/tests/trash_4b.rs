//! Plan 4b's whole-asset trash: `asset.trash` asks every caller, the app included, with the
//! dependents, versions and the folder it deletes; the whole folder moves as one entry and comes
//! back read-only; refs to it answer `asset.trashed`; it is refused while the asset is in use; and
//! `trash.restore {to}` restores to a free name with a redirect (spec CORE-157, CORE-037,
//! APP-115, ARCH-125; Appendix H "Trash an asset", "Confirmations", "`registry/redirects.yaml`").
//! Driven through the asset, trash and `ref.resolve` tools from the `ui:test` app and a `cli`
//! connection, `test.job` and `test.hold`, and the workspace files (QA-018).

mod p4b;
mod support;

use std::path::{Path, PathBuf};

use lil_testkit::{TestApp, sha256_file};
use p4b::*;
use serde_json::{Value, json};
use support::*;

/// The crane's folder as a touch names it.
const CRANE_DIR: &str = "projects/sketchbook/assets/chr_paper_crane/";

/// `asset.trash`'s arguments for the crane.
fn trash_args() -> Value {
    json!({"project": PROJECT, "asset_id": CRANE})
}

/// The app trashes the crane, approving it when it asks. Returns the `entry_id`.
fn trash_crane(app: &mut TestApp) -> String {
    let ran = app_call_confirmed(app, "asset.trash", trash_args());
    assert_eq!(ran["status"], "ok", "{ran}");
    ran["structured"]["entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no entry_id: {ran}"))
        .to_string()
}

/// The sealed ref of the crane's first version, without the hash.
fn crane_ref(s: &Served) -> String {
    format!("lil://{}/{PROJECT}/{CRANE}@1.0.0", s.ws_id())
}

/// The first file under `dir` (recursively) whose path ends with `suffix`.
fn find_file(dir: &Path, suffix: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, suffix) {
                return Some(found);
            }
        } else if path.to_string_lossy().ends_with(suffix) {
            return Some(path);
        }
    }
    None
}

/// `trash.list`'s entries whose reason is `asset.trash`.
fn asset_trash_entries(c: &mut impl Caller) -> Vec<Value> {
    let list = ok(c, "trash.list", json!({}), None);
    list["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("no entries: {list}"))
        .iter()
        .filter(|e| e["reason"] == "asset.trash")
        .cloned()
        .collect()
}

#[test] // spec: CORE-157, APP-115
fn asset_trash_asks_even_the_app() {
    // The lamp uses the crane's first version: a dependent.
    let s = Served::start_with(
        |_, ws| {
            add_manifest_fields(
                ws,
                LAMP,
                r#""inputs": [{"ref": "chr_paper_crane@1.0.0", "role": "reference"}]"#,
            )
        },
        &[],
    );
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);

    let k = key();
    let first = app.tool("asset.trash", trash_args(), Some(&k));
    let confirmation = pending_id(&first);
    let view = pending_view(&mut app, "asset.trash");
    assert_eq!(view["id"], confirmation.as_str(), "{view}");
    assert_eq!(
        view["files"],
        json!([{"path": CRANE_DIR, "access": "delete"}]),
        "{view}"
    );
    let facts = &view["facts"];
    let dependents = strings(&facts["dependents"]);
    assert_eq!(dependents.len(), 1, "{view}");
    assert!(dependents[0].contains(LAMP), "{view}");
    assert_eq!(facts["dependents_total"], 1, "{view}");
    assert_eq!(facts["versions"], json!(["1.0.0"]), "{view}");
    assert!(
        s.asset_dir(CRANE).is_dir(),
        "nothing moves before the answer"
    );

    answer(&mut app, &confirmation, json!({"answer": "approve"}));
    let ran = ok(&mut app, "asset.trash", trash_args(), Some(&k));
    assert!(ran["entry_id"].is_string(), "{ran}");
    assert!(
        strings(&ran["dependents"]).iter().any(|d| d.contains(LAMP)),
        "{ran}"
    );
    assert!(
        !s.asset_dir(CRANE).exists(),
        "the crane's folder is still there"
    );
}

#[test] // spec: CORE-157, CORE-037
fn trash_moves_the_whole_asset_and_restores_it_read_only() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    let manifest_file = s.asset_dir(CRANE).join(format!("{CRANE}.manifest.json"));
    let manifest_before = std::fs::read(&manifest_file).unwrap_or_else(|e| panic!("{e}"));

    let entry_id = trash_crane(&mut app);
    assert!(
        !s.asset_dir(CRANE).exists(),
        "the crane's folder is still there"
    );
    let entries = asset_trash_entries(&mut s.pipe);
    assert_eq!(
        entries.len(),
        1,
        "one entry for the whole folder: {entries:?}"
    );
    assert_eq!(entries[0]["entry_id"], entry_id.as_str(), "{entries:?}");
    assert_eq!(
        entries[0]["original_path"]
            .as_str()
            .map(|p| p.trim_end_matches('/')),
        Some(CRANE_DIR.trim_end_matches('/')),
        "{entries:?}"
    );
    let in_trash = find_file(
        &s.ws.join(".trash"),
        &format!(r"payload\{CRANE}\v1.0.0\crane_b.png"),
    )
    .unwrap_or_else(|| panic!("the sealed file is not in the trash entry"));
    assert_eq!(
        sha256_file(&in_trash),
        CRANE_SEALED_SHA256,
        "sealed files are unmodified"
    );

    let restored = write_ok(
        &mut s.pipe,
        "trash.restore",
        json!({ "entry_id": entry_id }),
    );
    assert_eq!(
        restored["path"].as_str().map(|p| p.trim_end_matches('/')),
        Some(CRANE_DIR.trim_end_matches('/')),
        "{restored}"
    );
    let sealed = s.asset_dir(CRANE).join(CRANE_SEALED.replace('/', "\\"));
    assert_eq!(sha256_file(&sealed), CRANE_SEALED_SHA256);
    assert!(
        is_read_only(&sealed),
        "read-only is re-applied to {}",
        sealed.display()
    );
    assert_eq!(
        std::fs::read(&manifest_file).unwrap_or_else(|e| panic!("{e}")),
        manifest_before,
        "the manifest comes back as it was"
    );
    let detail = asset_get(&mut s.pipe, CRANE);
    assert_eq!(detail["approved_version"], "1.0.0", "{detail}");
}

#[test] // spec: CORE-157, ARCH-125
fn refs_to_a_trashed_asset_are_asset_trashed() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    let entry_id = trash_crane(&mut app);

    let sealed = crane_ref(&s);
    let candidate = format!(
        "lil://{}/{PROJECT}/{CRANE}@candidate/{CRANE_OPEN_RUN}/crane_c.png",
        s.ws_id()
    );
    for r in [sealed, candidate] {
        let resolved = s.pipe.tool("ref.resolve", json!({ "ref": r }), None);
        assert_eq!(error_code(&resolved), "asset.trashed", "{r}: {resolved}");
        assert_eq!(
            resolved["error"]["data"]["entry_id"],
            entry_id.as_str(),
            "{resolved}"
        );
    }
}

#[test] // spec: CORE-157
fn trash_refused_while_in_use() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);

    // An unfinished job of the asset.
    let job = submit(
        &mut s.pipe,
        "test.job",
        json!({"lane": "cpu", "gated": true, "asset": {"project": PROJECT, "asset_id": CRANE}}),
    );
    let refused = app_call_confirmed(&mut app, "asset.trash", trash_args());
    assert_eq!(error_code(&refused), "asset.in_use", "{refused}");
    assert!(s.asset_dir(CRANE).is_dir(), "nothing moved");
    assert!(asset_trash_entries(&mut s.pipe).is_empty());
    release(&mut s.pipe, &job);
    wait_done(&mut s.pipe, &job);

    // A document hold inside the asset folder.
    let mut holder = TestApp::start(&s.env, Some(&s.ws), &[]);
    write_ok(
        &mut holder,
        "test.hold",
        json!({"doc_id": "doc_crane", "path": format!("{CRANE_DIR}work/sketch.lil")}),
    );
    let refused = app_call_confirmed(&mut app, "asset.trash", trash_args());
    assert_eq!(error_code(&refused), "asset.in_use", "{refused}");
    assert!(s.asset_dir(CRANE).is_dir(), "nothing moved");
    assert!(asset_trash_entries(&mut s.pipe).is_empty());
}

#[test] // spec: CORE-037, ARCH-125
fn restore_to_a_free_name() {
    const ORIGAMI: &str = "chr_origami";
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    let lamp_manifest = s.asset_dir(LAMP).join(format!("{LAMP}.manifest.json"));
    let lamp_before = std::fs::read(&lamp_manifest).unwrap_or_else(|e| panic!("{e}"));
    let entry_id = trash_crane(&mut app);

    // A used name is a conflict, and nothing is replaced.
    let conflict = s.pipe.tool(
        "trash.restore",
        json!({"entry_id": entry_id, "to": LAMP}),
        Some(&key()),
    );
    assert_eq!(
        error_code(&conflict),
        "trash.restore_conflict",
        "{conflict}"
    );
    assert_eq!(
        std::fs::read(&lamp_manifest).unwrap_or_default(),
        lamp_before
    );

    let restored = write_ok(
        &mut s.pipe,
        "trash.restore",
        json!({"entry_id": entry_id, "to": ORIGAMI}),
    );
    assert!(
        restored["path"].as_str().is_some_and(
            |p| p.trim_end_matches('/') == format!("projects/{PROJECT}/assets/{ORIGAMI}")
        ),
        "{restored}"
    );
    assert!(!s.asset_dir(CRANE).exists());
    let sealed = s.asset_dir(ORIGAMI).join(CRANE_SEALED.replace('/', "\\"));
    assert_eq!(sha256_file(&sealed), CRANE_SEALED_SHA256);
    assert!(is_read_only(&sealed), "{}", sealed.display());
    assert_eq!(s.manifest(ORIGAMI)["asset_id"], ORIGAMI);

    // Refs to the old name keep working through the redirect.
    let redirects = std::fs::read_to_string(s.path("registry/redirects.yaml"))
        .unwrap_or_else(|e| panic!("registry/redirects.yaml: {e}"));
    assert!(
        redirects.contains(&format!("{PROJECT}/{CRANE}"))
            && redirects.contains(&format!("{PROJECT}/{ORIGAMI}")),
        "{redirects}"
    );
    let old_ref = crane_ref(&s);
    let resolved = ok(&mut s.pipe, "ref.resolve", json!({ "ref": old_ref }), None);
    assert_eq!(resolved["asset_id"], ORIGAMI, "{resolved}");
    assert_eq!(
        resolved["redirected_from"],
        json!([format!("{PROJECT}/{CRANE}")]),
        "{resolved}"
    );
}
