//! Plan 4b's saved searches and collections: `collection.save`, `collections.list`,
//! `assets.query {collection}`, `collection.delete` to the trash and `trash.restore` back by id,
//! and the quick collection that is always listed and never deleted (spec CORE-155; Appendix H
//! "Organising tools", "`registry/collections.yaml`"). Driven through the collection, query and
//! trash tools from a `cli` connection and the `ui:test` app, and the workspace files (QA-018).

mod p4b;
mod support;

use p4b::*;
use serde_json::{Value, json};
use support::*;

/// The quick collection's id: the nil UUID.
const QUICK: &str = "00000000-0000-0000-0000-000000000000";
const CRANE_REF: &str = "sketchbook/chr_paper_crane";
const LAMP_REF: &str = "sketchbook/prop_signal_lamp";

/// The entries of `collections.list`.
fn collections(c: &mut impl Caller) -> Vec<Value> {
    let list = ok(c, "collections.list", json!({}), None);
    list["collections"]
        .as_array()
        .unwrap_or_else(|| panic!("no collections list: {list}"))
        .clone()
}

/// The `collections.list` entry with `id`, if any.
fn listed(c: &mut impl Caller, id: &str) -> Option<Value> {
    collections(c).into_iter().find(|e| e["id"] == id)
}

/// The `(ref, state)` of each of an entry's `refs`.
fn ref_states(entry: &Value) -> Vec<(String, String)> {
    entry["refs"]
        .as_array()
        .unwrap_or_else(|| panic!("no refs: {entry}"))
        .iter()
        .map(|r| {
            (
                r["ref"].as_str().unwrap_or_default().to_string(),
                r["state"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// `[(CRANE_REF, "ok")]`.
fn crane_ok() -> Vec<(String, String)> {
    vec![(CRANE_REF.to_string(), "ok".to_string())]
}

/// The `id` of a `collection.save` result.
fn id_of(saved: &Value) -> String {
    saved["collection"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no collection.id: {saved}"))
        .to_string()
}

/// The `entry_id` of a `collection.delete` `Outcome`, checked to succeed.
fn entry_of(outcome: &Value) -> String {
    assert_eq!(outcome["status"], "ok", "{outcome}");
    outcome["structured"]["entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no entry_id: {outcome}"))
        .to_string()
}

#[test] // spec: CORE-155
fn save_list_query_delete_restore() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);

    let saved = write_ok(
        &mut s.pipe,
        "collection.save",
        json!({"name": "Cranes", "refs": [CRANE_REF]}),
    );
    let coll = id_of(&saved);
    assert!(
        is_canonical_uuid(&coll) && coll.as_bytes()[14] == b'4',
        "a fresh UUID v4: {saved}"
    );
    assert_eq!(saved["collection"]["name"], "Cranes", "{saved}");
    let search = id_of(&write_ok(
        &mut s.pipe,
        "collection.save",
        json!({"name": "Props", "rule": {"type": "prop_2d"}}),
    ));

    let entry = listed(&mut s.pipe, &coll).unwrap_or_else(|| panic!("{coll} is not listed"));
    assert_eq!(entry["kind"], "collection", "{entry}");
    assert_eq!(ref_states(&entry), crane_ok(), "{entry}");
    let entry = listed(&mut s.pipe, &search).unwrap_or_else(|| panic!("{search} is not listed"));
    assert_eq!(entry["kind"], "search", "{entry}");
    assert_eq!(entry["rule"], json!({"type": "prop_2d"}), "{entry}");
    let file = std::fs::read_to_string(s.path("registry/collections.yaml"))
        .unwrap_or_else(|e| panic!("registry/collections.yaml: {e}"));
    assert!(file.contains(&coll) && file.contains(&search), "{file}");

    let by_refs = query(&mut s.pipe, json!({"collection": coll}));
    assert_eq!(ids_of(&by_refs), [CRANE], "{by_refs}");
    let by_rule = query(&mut s.pipe, json!({"collection": search}));
    assert_eq!(ids_of(&by_rule), [LAMP], "{by_rule}");

    // Names are unique whatever their case.
    let taken = s.pipe.tool(
        "collection.save",
        json!({"name": "CRANES", "refs": [LAMP_REF]}),
        Some(&key()),
    );
    assert_eq!(error_code(&taken), "collection.name_taken", "{taken}");

    // A cli caller's delete waits for a confirmation, then goes to the trash.
    let k = key();
    let first = s
        .pipe
        .tool("collection.delete", json!({ "id": coll }), Some(&k));
    let confirmation = pending_id(&first);
    answer(&mut app, &confirmation, json!({"answer": "approve"}));
    let entry_id = entry_of(
        &s.pipe
            .tool("collection.delete", json!({ "id": coll }), Some(&k)),
    );
    assert!(
        listed(&mut s.pipe, &coll).is_none(),
        "{coll} is still listed"
    );
    let gone = s
        .pipe
        .tool("assets.query", json!({ "collection": coll }), None);
    assert_eq!(error_code(&gone), "collection.unknown", "{gone}");
    let trash = ok(&mut s.pipe, "trash.list", json!({}), None);
    let record = trash["entries"]
        .as_array()
        .and_then(|e| e.iter().find(|e| e["entry_id"] == entry_id.as_str()))
        .unwrap_or_else(|| panic!("trash.list has no {entry_id}: {trash}"));
    assert_eq!(record["reason"], "collection.delete", "{record}");
    assert_eq!(
        record["original_path"], "registry/collections.yaml",
        "{record}"
    );

    // Restored by id.
    write_ok(
        &mut s.pipe,
        "trash.restore",
        json!({ "entry_id": entry_id }),
    );
    let back = listed(&mut s.pipe, &coll).unwrap_or_else(|| panic!("{coll} is not back"));
    assert_eq!(back["name"], "Cranes", "{back}");
    assert_eq!(ref_states(&back), crane_ok(), "{back}");

    // Deleted again; its name is taken meanwhile, so the restore is a conflict.
    let again = entry_of(&call_confirmed(
        &mut s.pipe,
        &mut app,
        "collection.delete",
        json!({ "id": coll }),
    ));
    write_ok(
        &mut s.pipe,
        "collection.save",
        json!({"name": "Cranes", "refs": [LAMP_REF]}),
    );
    let conflict = s
        .pipe
        .tool("trash.restore", json!({ "entry_id": again }), Some(&key()));
    assert_eq!(
        error_code(&conflict),
        "trash.restore_conflict",
        "{conflict}"
    );
}

#[test] // spec: CORE-155
fn quick_collection_is_always_listed_and_never_deleted() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);

    let quick = listed(&mut s.pipe, QUICK).unwrap_or_else(|| panic!("no quick collection"));
    assert_eq!(quick["name"], "Quick collection", "{quick}");
    assert_eq!(quick["builtin"], "quick", "{quick}");
    assert_eq!(quick["kind"], "collection", "{quick}");

    let refused = app.tool("collection.delete", json!({ "id": QUICK }), Some(&key()));
    assert_eq!(error_code(&refused), "params.invalid", "{refused}");

    let saved = write_ok(
        &mut app,
        "collection.save",
        json!({"id": QUICK, "add": [CRANE_REF]}),
    );
    assert_eq!(id_of(&saved), QUICK, "{saved}");
    let quick = listed(&mut s.pipe, QUICK).unwrap_or_else(|| panic!("the quick collection went"));
    assert_eq!(ref_states(&quick), crane_ok(), "{quick}");

    let refused = call_confirmed(
        &mut s.pipe,
        &mut app,
        "collection.delete",
        json!({ "id": QUICK }),
    );
    assert_eq!(error_code(&refused), "params.invalid", "{refused}");
    assert!(
        listed(&mut s.pipe, QUICK).is_some(),
        "the quick collection is never deleted"
    );
}
