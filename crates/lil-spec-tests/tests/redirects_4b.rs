//! Plan 4b's redirects and repairs: an asset folder moved outside the suite is `moved` until
//! `asset.accept_move` records a redirect, after which old refs resolve to the new name; a
//! redirect cycle or a dead end is `ref.redirect_broken` and needs repair; a copied folder keeps
//! its original's UUID until `asset.make_independent`; and no repair runs by itself (spec
//! ARCH-125, CORE-154, CORE-047; Appendix H "`registry/redirects.yaml`", "`ref.resolve`",
//! "Moved and copied folders"). Driven through the asset, project and `ref.resolve` tools from a
//! `cli` connection, folders moved and copied as Explorer would, and the workspace files
//! (QA-018).

mod p4b;
mod support;

use lil_testkit::{CoreFixture, TestEnv};
use p4b::*;
use serde_json::{Value, json};
use support::*;

/// The new asset `accept_move_…` imports, and the name its folder is moved to.
const KITE: &str = "prop_kite";
const GLIDER: &str = "prop_glider";
/// The crane's `creation_uuid` in the fixture.
const CRANE_UUID: &str = "6f1c2a3e-8b4d-4c5e-9a7b-1d2e3f405162";

/// `ref.resolve {ref}`: the `Outcome`.
fn resolve(c: &mut impl Caller, r: &str) -> Value {
    c.tool("ref.resolve", json!({ "ref": r }), None)
}

/// The `moved` entries of `project.status`.
fn moved(c: &mut impl Caller) -> Vec<Value> {
    let status = project_status(c);
    status["moved"]
        .as_array()
        .unwrap_or_else(|| panic!("project.status has no moved list: {status}"))
        .clone()
}

#[test] // spec: CORE-154, ARCH-125
fn accept_move_records_a_redirect_and_old_refs_resolve() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("p4b");
    env.register_workspace(&ws, true);
    let sealed_ref = {
        let _core = CoreFixture::start(&env);
        let mut pipe = session(&env, Some(&ws));
        let imported = import(&mut pipe, &env, KITE, &[("kite.png", b"kite one")]);
        let signed = signoff(&mut pipe, KITE, &run_id_of(&imported), &["kite.png"]);
        signed["ref"]
            .as_str()
            .unwrap_or_else(|| panic!("no ref: {signed}"))
            .to_string()
    };

    // The folder is renamed outside the suite while no core runs.
    std::fs::rename(asset_dir(&ws, KITE), asset_dir(&ws, GLIDER))
        .unwrap_or_else(|e| panic!("cannot rename the kite's folder: {e}"));
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));

    let listed = moved(&mut pipe);
    assert!(
        listed.iter().any(|m| {
            m["from"].as_str().is_some_and(|f| f.contains(KITE))
                && m["to"].as_str().is_some_and(|t| t.contains(GLIDER))
        }),
        "project.status lists the move: {listed:?}"
    );
    let detail = asset_get(&mut pipe, GLIDER);
    assert_eq!(detail["repair"]["kind"], "moved", "{detail}");
    assert_eq!(detail["needs_repair"], true, "{detail}");

    let accepted = write_ok(
        &mut pipe,
        "asset.accept_move",
        json!({"project": PROJECT, "asset_id": GLIDER}),
    );
    assert_eq!(accepted["project"], PROJECT, "{accepted}");
    assert_eq!(accepted["asset_id"], GLIDER, "{accepted}");
    assert_eq!(
        accepted["redirect"]["from"],
        format!("{PROJECT}/{KITE}"),
        "{accepted}"
    );
    assert_eq!(
        accepted["redirect"]["to"],
        format!("{PROJECT}/{GLIDER}"),
        "{accepted}"
    );
    let redirects = std::fs::read_to_string(ws_path(&ws, "registry/redirects.yaml"))
        .unwrap_or_else(|e| panic!("registry/redirects.yaml: {e}"));
    assert!(
        redirects.contains(&format!("{PROJECT}/{KITE}"))
            && redirects.contains(&format!("{PROJECT}/{GLIDER}")),
        "{redirects}"
    );
    let manifest = manifest_of(&ws, GLIDER);
    assert_eq!(manifest["asset_id"], GLIDER, "{manifest}");
    assert_eq!(manifest["project"], PROJECT, "{manifest}");
    assert!(moved(&mut pipe).is_empty(), "the move is accepted");
    assert_eq!(asset_get(&mut pipe, GLIDER)["needs_repair"], false);

    // The old sealed ref reaches the new name.
    let resolved = resolve(&mut pipe, &sealed_ref);
    assert_eq!(resolved["status"], "ok", "{resolved}");
    let r = &resolved["structured"];
    assert_eq!(r["kind"], "sealed", "{r}");
    assert_eq!(r["project"], PROJECT, "{r}");
    assert_eq!(r["asset_id"], GLIDER, "{r}");
    assert_eq!(r["version"], "1.0.0", "{r}");
    assert_eq!(r["read_only"], true, "{r}");
    assert_eq!(r["sha256_ok"], true, "{r}");
    assert!(
        r["ref"]
            .as_str()
            .is_some_and(|n| n.contains(&format!("/{PROJECT}/{GLIDER}@1.0.0"))),
        "the ref names the new place: {r}"
    );
    assert_eq!(
        r["redirected_from"],
        json!([format!("{PROJECT}/{KITE}")]),
        "{r}"
    );
}

#[test] // spec: ARCH-125
fn a_redirect_cycle_is_broken_and_needs_repair() {
    // ghost_a and ghost_b lead to each other; ghost_c leads to a name with no folder.
    let yaml = concat!(
        "redirects:\n",
        "  - from: sketchbook/ghost_a\n",
        "    to: sketchbook/ghost_b\n",
        "    moved_utc: 2026-11-01T10:00:00Z\n",
        "    by: cli\n",
        "  - from: sketchbook/ghost_b\n",
        "    to: sketchbook/ghost_a\n",
        "    moved_utc: 2026-11-01T10:01:00Z\n",
        "    by: cli\n",
        "  - from: sketchbook/ghost_c\n",
        "    to: sketchbook/ghost_d\n",
        "    moved_utc: 2026-11-01T10:02:00Z\n",
        "    by: cli\n",
    );
    let mut s = Served::start_with(
        |_, ws| {
            lil_testkit::files::write_file(&ws_path(ws, "registry/redirects.yaml"), yaml.as_bytes())
        },
        &[],
    );
    let ws_id = s.ws_id();

    let cycle = resolve(
        &mut s.pipe,
        &format!("lil://{ws_id}/{PROJECT}/ghost_a@1.0.0"),
    );
    assert_eq!(error_code(&cycle), "ref.redirect_broken", "{cycle}");
    let data = &cycle["error"]["data"];
    assert_eq!(data["reason"], "cycle", "{cycle}");
    let chain = strings(&data["chain"]);
    for name in ["sketchbook/ghost_a", "sketchbook/ghost_b"] {
        assert!(
            chain.iter().any(|c| c == name),
            "{name} in the chain: {cycle}"
        );
    }

    let dead_end = resolve(
        &mut s.pipe,
        &format!("lil://{ws_id}/{PROJECT}/ghost_c@1.0.0"),
    );
    assert_eq!(error_code(&dead_end), "ref.redirect_broken", "{dead_end}");
    assert_eq!(
        dead_end["error"]["data"]["reason"], "target_missing",
        "{dead_end}"
    );
    assert_eq!(
        dead_end["error"]["data"]["to"], "sketchbook/ghost_d",
        "{dead_end}"
    );

    let status = project_status(&mut s.pipe);
    let needs_repair = strings(&status["needs_repair"]);
    assert!(
        needs_repair
            .iter()
            .any(|p| p.starts_with("registry/redirects.yaml#sketchbook/ghost_")),
        "the redirect file needs repair: {status}"
    );
}

#[test] // spec: CORE-154, CORE-047
fn make_independent_gives_a_copy_a_new_uuid() {
    const COPY: &str = "chr_paper_crane_copy";
    let mut s = Served::start_with(
        |_, ws| copy_dir(&asset_dir(ws, CRANE), &asset_dir(ws, COPY)),
        &[],
    );

    let all = query(&mut s.pipe, json!({}));
    assert_eq!(
        ids_of(&all),
        [CRANE, COPY, LAMP],
        "both folders are listed: {all}"
    );
    let copy = asset_get(&mut s.pipe, COPY);
    assert_eq!(copy["repair"]["kind"], "copy", "{copy}");
    assert!(
        copy["repair"]["other"]
            .as_str()
            .is_some_and(|o| o.contains(CRANE) && !o.contains(COPY)),
        "`other` names the original: {copy}"
    );
    assert_eq!(copy["needs_repair"], true, "{copy}");
    assert_eq!(asset_get(&mut s.pipe, CRANE)["needs_repair"], true);
    let status = project_status(&mut s.pipe);
    let groups = status["duplicates"]
        .as_array()
        .unwrap_or_else(|| panic!("project.status has no duplicates: {status}"));
    assert!(
        groups.iter().any(|g| {
            let paths = strings(g);
            paths.len() == 2 && paths.iter().any(|p| p.contains(COPY))
        }),
        "{status}"
    );

    let made = write_ok(
        &mut s.pipe,
        "asset.make_independent",
        json!({"project": PROJECT, "asset_id": COPY}),
    );
    let uuid = made["creation_uuid"]
        .as_str()
        .unwrap_or_else(|| panic!("no creation_uuid: {made}"))
        .to_string();
    assert!(is_canonical_uuid(&uuid) && uuid != CRANE_UUID, "{made}");
    let copy_manifest = s.manifest(COPY);
    assert_eq!(
        copy_manifest["creation_uuid"],
        uuid.as_str(),
        "{copy_manifest}"
    );
    assert_eq!(copy_manifest["asset_id"], COPY, "{copy_manifest}");
    assert_eq!(copy_manifest["project"], PROJECT, "{copy_manifest}");
    assert_eq!(
        s.manifest(CRANE)["creation_uuid"],
        CRANE_UUID,
        "only the original keeps it"
    );

    let copy = asset_get(&mut s.pipe, COPY);
    assert_eq!(copy["needs_repair"], false, "{copy}");
    assert!(copy["repair"].is_null(), "{copy}");
    assert_eq!(asset_get(&mut s.pipe, CRANE)["needs_repair"], false);
    let status = project_status(&mut s.pipe);
    assert_eq!(status["duplicates"], json!([]), "{status}");
}

#[test] // spec: CORE-154
fn repairs_never_run_by_themselves() {
    const ORIGAMI: &str = "chr_origami";
    let mut s = Served::start_with(
        |_, ws| {
            std::fs::rename(asset_dir(ws, CRANE), asset_dir(ws, ORIGAMI))
                .unwrap_or_else(|e| panic!("cannot rename the crane's folder: {e}"));
        },
        &[],
    );
    let manifest_file = s.asset_dir(ORIGAMI).join(format!("{CRANE}.manifest.json"));
    let before = std::fs::read(&manifest_file).unwrap_or_else(|e| panic!("{e}"));

    let listed = moved(&mut s.pipe);
    assert!(
        listed
            .iter()
            .any(|m| m["to"].as_str().is_some_and(|t| t.contains(ORIGAMI))),
        "the move is detected: {listed:?}"
    );
    s.pipe.fence(&s.ws);
    assert_eq!(moved(&mut s.pipe).len(), listed.len(), "still moved");

    // Neither repair applies where it isn't needed.
    for (tool, asset) in [
        ("asset.accept_move", LAMP),
        ("asset.make_independent", LAMP),
        ("asset.make_independent", ORIGAMI),
    ] {
        let refused = s.pipe.tool(
            tool,
            json!({"project": PROJECT, "asset_id": asset}),
            Some(&key()),
        );
        assert_eq!(
            error_code(&refused),
            "repair.not_needed",
            "{tool} {asset}: {refused}"
        );
    }

    assert_eq!(
        std::fs::read(&manifest_file).unwrap_or_else(|e| panic!("{e}")),
        before,
        "the moved asset's manifest is never rewritten by itself"
    );
    assert!(
        !s.path("registry/redirects.yaml").exists(),
        "no redirect is written by itself"
    );
}
