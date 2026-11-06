//! Plan 4b's marks and the filters over them: `asset.mark`'s tags, rating and label and their
//! `assets.query` filters, take flags only on open runs, disjunctive facets counted with the page,
//! duplicates by bytes with the import warning, the deprecated filter and cursors that stay valid
//! (spec CORE-152, CORE-168, CORE-170, CORE-023, APP-025, APP-147; Appendix H "Manifest fields",
//! "Takes", "`assets.query`", "Organising tools", "Duplicates"). Driven through the asset tools
//! from a `cli` connection and the manifests on disk (QA-018).

mod p4b;
mod support;

use p4b::*;
use serde_json::{Value, json};
use support::*;

/// The new asset some tests import.
const KITE: &str = "prop_kite";

/// `asset.mark` with `args` on top of the project, checked to succeed.
fn mark(c: &mut impl Caller, asset_id: &str, args: Value) -> Value {
    let mut all = json!({"project": PROJECT, "asset_id": asset_id});
    for (k, v) in args.as_object().into_iter().flatten() {
        all[k] = v.clone();
    }
    write_ok(c, "asset.mark", all)
}

/// The tags of a mark result, manifest or summary, sorted: tag order isn't a contract.
fn tag_set(v: &Value) -> Vec<String> {
    let mut tags = strings(&v["tags"]);
    tags.sort();
    tags
}

/// The count of `value` in the facet `name` (`None` when the facet doesn't list it).
fn facet(page: &Value, name: &str, value: &Value) -> Option<u64> {
    page["facets"][name]
        .as_array()
        .unwrap_or_else(|| panic!("no facet {name}: {page}"))
        .iter()
        .find(|f| &f["value"] == value)
        .map(|f| {
            f["count"]
                .as_u64()
                .unwrap_or_else(|| panic!("a facet count is not a number: {f}"))
        })
}

#[test] // spec: CORE-152
fn marks_round_trip_and_filter() {
    let mut s = Served::start();
    let marked = mark(
        &mut s.pipe,
        CRANE,
        json!({"tags": ["Tree", "oak"], "rating": 4, "label": "red"}),
    );
    assert_eq!(tag_set(&marked), ["Tree", "oak"], "{marked}");
    assert_eq!(marked["rating"], 4, "{marked}");
    assert_eq!(marked["label"], "red", "{marked}");
    let manifest = s.manifest(CRANE);
    assert_eq!(tag_set(&manifest), ["Tree", "oak"], "{manifest}");
    assert_eq!(manifest["rating"], 4, "{manifest}");
    assert_eq!(manifest["label"], "red", "{manifest}");

    let changed = mark(
        &mut s.pipe,
        CRANE,
        json!({"add_tags": ["birch"], "remove_tags": ["OAK"]}),
    );
    assert_eq!(tag_set(&changed), ["Tree", "birch"], "{changed}");

    // A lamp in red rated 2, and a kite in blue rated 5.
    mark(&mut s.pipe, LAMP, json!({"rating": 2, "label": "red"}));
    import(&mut s.pipe, &s.env, KITE, &[("kite.png", b"kite one")]);
    mark(&mut s.pipe, KITE, json!({"rating": 5, "label": "blue"}));

    // Tags match case-insensitively.
    let trees = query(&mut s.pipe, json!({"tags": ["TREE"]}));
    assert_eq!(ids_of(&trees), [CRANE], "{trees}");
    let crane = summary_of(&trees, CRANE);
    assert_eq!(tag_set(crane), ["Tree", "birch"], "{crane}");
    assert_eq!(crane["rating"], 4, "{crane}");
    assert_eq!(crane["label"], "red", "{crane}");

    let rated = query(&mut s.pipe, json!({"rating_min": 4}));
    assert_eq!(ids_of(&rated), [CRANE, KITE], "{rated}");
    let red = query(&mut s.pipe, json!({"label": ["red"]}));
    assert_eq!(ids_of(&red), [CRANE, LAMP], "{red}");
    let both = query(&mut s.pipe, json!({"rating_min": 4, "label": ["red"]}));
    assert_eq!(
        ids_of(&both),
        [CRANE],
        "different filters AND together: {both}"
    );
    assert_eq!(both["total"], 1, "{both}");

    // `label: null` clears.
    let cleared = mark(&mut s.pipe, LAMP, json!({"label": null}));
    assert!(cleared["label"].is_null(), "{cleared}");
    let unlabelled = query(&mut s.pipe, json!({"label": ["none"]}));
    assert!(
        ids_of(&unlabelled).contains(&LAMP.to_string()),
        "{unlabelled}"
    );
}

#[test] // spec: CORE-152, CORE-023
fn flags_are_per_take_and_only_on_open_runs() {
    let mut s = Served::start();
    let imported = import(
        &mut s.pipe,
        &s.env,
        KITE,
        &[("kite_a.png", b"kite a"), ("kite_b.png", b"kite b")],
    );
    let run_id = run_id_of(&imported);

    let picked = mark(
        &mut s.pipe,
        KITE,
        json!({"run_id": run_id, "take": 1, "flag": "pick"}),
    );
    assert_eq!(picked["take"]["run_id"], run_id.as_str(), "{picked}");
    assert_eq!(picked["take"]["take"], 1, "{picked}");
    assert_eq!(picked["take"]["flag"], "pick", "{picked}");
    // `file` addresses the take too.
    let rejected = mark(
        &mut s.pipe,
        KITE,
        json!({"run_id": run_id, "file": "kite_b.png", "flag": "reject", "rating": 2}),
    );
    assert_eq!(rejected["take"]["take"], 2, "{rejected}");
    assert_eq!(rejected["take"]["flag"], "reject", "{rejected}");
    assert_eq!(rejected["take"]["rating"], 2, "{rejected}");

    let manifest = s.manifest(KITE);
    let run = run_of(&manifest, &run_id);
    assert_eq!(run_file(run, "kite_a.png")["flag"], "pick", "{run}");
    assert_eq!(run_file(run, "kite_b.png")["flag"], "reject", "{run}");
    assert_eq!(run_file(run, "kite_b.png")["rating"], 2, "{run}");
    // Take marks are not asset marks.
    assert!(manifest.get("rating").is_none_or(|r| r == 0), "{manifest}");

    // `flag: null` clears.
    let cleared = mark(
        &mut s.pipe,
        KITE,
        json!({"run_id": run_id, "take": 2, "flag": null}),
    );
    assert!(cleared["take"]["flag"].is_null(), "{cleared}");
    let manifest = s.manifest(KITE);
    assert!(
        run_file(run_of(&manifest, &run_id), "kite_b.png")
            .get("flag")
            .is_none_or(Value::is_null),
        "{manifest}"
    );

    // After sign-off the run is closed: its marks don't change.
    signoff(&mut s.pipe, KITE, &run_id, &["kite_a.png"]);
    for (asset, run) in [(KITE, run_id.as_str()), (CRANE, CRANE_CLOSED_RUN)] {
        let refused = s.pipe.tool(
            "asset.mark",
            json!({"project": PROJECT, "asset_id": asset, "run_id": run, "take": 1,
                   "flag": "reject"}),
            Some(&key()),
        );
        assert_eq!(
            error_code(&refused),
            "status.invalid_transition",
            "{asset} {run}: {refused}"
        );
    }
}

#[test] // spec: CORE-152, APP-147
fn facets_are_counted_with_the_page() {
    // Marks written in the manifests by hand (Appendix H "Manifest fields").
    let mut s = Served::start_with(
        |_, ws| {
            add_manifest_fields(
                ws,
                CRANE,
                r#""tags": ["tree"], "rating": 4, "label": "red""#,
            );
            add_manifest_fields(
                ws,
                LAMP,
                r#""tags": ["tree", "lamp"], "rating": 2, "label": "blue""#,
            );
        },
        &[],
    );
    // A third asset with no marks.
    import(&mut s.pipe, &s.env, KITE, &[("kite.png", b"kite one")]);

    let all = query(&mut s.pipe, json!({"facets": true}));
    assert_eq!(all["total"], 3, "{all}");
    assert_eq!(facet(&all, "label", &json!("red")), Some(1), "{all}");
    assert_eq!(facet(&all, "label", &json!("blue")), Some(1), "{all}");
    assert_eq!(
        facet(&all, "label", &Value::Null),
        Some(1),
        "none counts as null: {all}"
    );
    assert_eq!(facet(&all, "tags", &json!("tree")), Some(2), "{all}");
    assert_eq!(facet(&all, "tags", &json!("lamp")), Some(1), "{all}");
    assert_eq!(facet(&all, "rating", &json!(4)), Some(1), "{all}");
    assert_eq!(facet(&all, "rating", &json!(2)), Some(1), "{all}");
    assert_eq!(all["tags_more"], false, "{all}");
    for name in ["status", "type", "flag"] {
        assert!(all["facets"][name].is_array(), "facet {name}: {all}");
    }

    // Disjunctive: the label facet ignores the label filter; every other facet, the page and
    // `total` use it.
    let red = query(&mut s.pipe, json!({"label": ["red"], "facets": true}));
    assert_eq!(ids_of(&red), [CRANE], "{red}");
    assert_eq!(red["total"], 1, "{red}");
    assert_eq!(facet(&red, "label", &json!("red")), Some(1), "{red}");
    assert_eq!(facet(&red, "label", &json!("blue")), Some(1), "{red}");
    assert_eq!(facet(&red, "tags", &json!("tree")), Some(1), "{red}");
    // A value the other filters leave out counts 0, listed or not.
    assert_eq!(facet(&red, "tags", &json!("lamp")).unwrap_or(0), 0, "{red}");
    assert_eq!(facet(&red, "rating", &json!(2)).unwrap_or(0), 0, "{red}");
}

#[test] // spec: CORE-168
fn duplicates_finds_assets_sharing_bytes() {
    let mut s = Served::start();
    // The kite holds the bytes of the crane's sealed file.
    let crane_b = std::fs::read(s.asset_dir(CRANE).join(r"v1.0.0\crane_b.png"))
        .unwrap_or_else(|e| panic!("the crane's sealed file: {e}"));
    let imported = import(&mut s.pipe, &s.env, KITE, &[("kite.png", &crane_b)]);
    let warnings = strings(&imported["warnings"]);
    let same = warnings
        .iter()
        .find(|w| w.starts_with("same bytes as "))
        .unwrap_or_else(|| panic!("no duplicate warning: {imported}"));
    assert!(
        same.contains(&format!("{CRANE}@1.0.0")),
        "the warning names the crane's sealed ref first: {same}"
    );

    let duplicates = query(&mut s.pipe, json!({"duplicates": true}));
    assert_eq!(ids_of(&duplicates), [CRANE, KITE], "{duplicates}");
}

#[test] // spec: CORE-170
fn deprecated_filter_hides_and_shows() {
    let mut s = Served::start();
    write_ok(
        &mut s.pipe,
        "asset.deprecate",
        json!({"project": PROJECT, "asset_id": CRANE, "deprecated": true}),
    );

    let shown = query(&mut s.pipe, json!({}));
    assert_eq!(ids_of(&shown), [CRANE, LAMP], "the default shows: {shown}");
    let show = query(&mut s.pipe, json!({"deprecated": "show"}));
    assert_eq!(ids_of(&show), [CRANE, LAMP], "{show}");
    let hidden = query(&mut s.pipe, json!({"deprecated": "hide"}));
    assert_eq!(ids_of(&hidden), [LAMP], "{hidden}");
    let only = query(&mut s.pipe, json!({"deprecated": "only"}));
    assert_eq!(ids_of(&only), [CRANE], "{only}");
}

/// Every asset id of a query, page by page with `limit: 1`; each page continues with the
/// previous page's cursor and the same arguments.
fn page_through(c: &mut impl Caller, args: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut page_args = args.clone();
        page_args["limit"] = json!(1);
        if let Some(cursor) = &cursor {
            page_args["cursor"] = json!(cursor);
        }
        let page = query(c, page_args);
        ids.extend(ids_of(&page));
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => return ids,
        }
        assert!(ids.len() <= 10, "paging does not end: {ids:?}");
    }
}

#[test] // spec: CORE-152, APP-025
fn old_cursors_stay_valid() {
    let mut s = Served::start();
    let both = vec![CRANE.to_string(), LAMP.to_string()];

    // A query without the new keys pages on as before.
    assert_eq!(
        page_through(&mut s.pipe, &json!({"status": "candidate"})),
        both
    );
    // So does one with a list and new keys.
    assert_eq!(
        page_through(
            &mut s.pipe,
            &json!({"status": ["candidate"], "deprecated": "show", "rating_min": 0}),
        ),
        both
    );

    // A string and a list hash differently, and a new key changes the query: a cursor is bound
    // to its own query.
    let first = query(&mut s.pipe, json!({"status": "candidate", "limit": 1}));
    let cursor = first["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("no next_cursor: {first}"))
        .to_string();
    for other in [
        json!({"status": ["candidate"], "limit": 1, "cursor": cursor}),
        json!({"status": "candidate", "tags": ["tree"], "limit": 1, "cursor": cursor}),
    ] {
        let refused = s.pipe.tool("assets.query", other.clone(), None);
        assert_eq!(error_code(&refused), "params.invalid", "{other}: {refused}");
    }
    let next = query(
        &mut s.pipe,
        json!({"status": "candidate", "limit": 1, "cursor": cursor}),
    );
    assert_eq!(ids_of(&next), [LAMP], "{next}");
}
