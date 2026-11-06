//! Plan 4b's attribution of outside edits: `lil notify --by <kind>` records an agent edit with
//! that actor, without `--by` the edit stays yours, and `app/asset.ai_edit` relabels one entry,
//! only from the app (spec CORE-034, ARCH-103, APP-039; Appendix H "Attribution", owner decision
//! 2026-11-04). The watcher is off (a fail point), so only `lil notify` finds an edit. Driven
//! through the real `lil`, `asset.get` from a `cli` connection, `app/asset.ai_edit` from the
//! `ui:test` app, and the sealed file edited as an outside program would (QA-018).

mod p4b;
mod support;

use std::path::{Path, PathBuf};

use lil_testkit::{failpoints, sha256_bytes};
use p4b::*;
use serde_json::{Value, json};
use support::*;

/// The fixture served by a core whose watcher can't start, once its first reconcile has
/// finished: `asset.get` answers only after the first reconcile at open (Appendix E), so that
/// reconcile ran before any edit the test makes and cannot record it. Without a watcher, only
/// `lil notify` finds an edit after that.
fn served_without_watcher() -> Served {
    let points = failpoints::spec(&[("watch.start", "unsupported")]);
    let mut s = Served::start_with(|_, _| {}, &[(failpoints::ENV, &points)]);
    asset_get(&mut s.pipe, CRANE);
    s
}

/// The crane's sealed file.
fn sealed(s: &Served) -> PathBuf {
    s.asset_dir(CRANE).join(CRANE_SEALED.replace('/', "\\"))
}

/// Replaces a sealed file's bytes as an outside writer would: clears its read-only bit first.
fn edit_outside(path: &Path, bytes: &[u8]) {
    let mut permissions = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as Explorer or an editor would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|e| panic!("cannot clear the read-only bit of {}: {e}", path.display()));
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// `lil notify` with `flags` for `path`, checked to exit 0 silently.
fn notify(s: &Served, flags: &[&str], path: &Path) {
    let path = path_str(path);
    let mut args = vec!["notify"];
    args.extend_from_slice(flags);
    args.push(&path);
    let out = s.env.lil(&args);
    assert_eq!(out.code, 0, "lil {args:?}: {out:?}");
    assert_eq!(out.stdout, "", "lil {args:?}: {out:?}");
}

/// The crane's `human_edits` entry for its sealed file with `sha256`.
fn edit_with(c: &mut impl Caller, sha256: &str) -> Value {
    let detail = asset_get(c, CRANE);
    detail["human_edits"]
        .as_array()
        .and_then(|edits| {
            edits
                .iter()
                .find(|e| e["path"] == CRANE_SEALED && e["sha256"] == sha256)
        })
        .cloned()
        .unwrap_or_else(|| panic!("no human edit of {CRANE_SEALED} with {sha256}: {detail}"))
}

/// The agent kinds of `Actor` (Appendix H "Manifest fields").
const AGENT_KINDS: [&str; 3] = ["claude_code", "tether", "director"];

#[test] // spec: CORE-034, ARCH-103
fn notify_with_by_is_an_agent_edit() {
    let mut s = served_without_watcher();
    let edited = b"edited by an agent";
    edit_outside(&sealed(&s), edited);
    notify(&s, &["--by", "claude_code", "--session", "s1"], &sealed(&s));

    let entry = edit_with(&mut s.pipe, &sha256_bytes(edited));
    assert_eq!(entry["actor"]["kind"], "claude_code", "{entry}");
    assert_eq!(entry["actor"]["session_id"], "s1", "{entry}");
    // The manifest records it too.
    let manifest = s.manifest(CRANE);
    let recorded = manifest["human_edits"]
        .as_array()
        .and_then(|e| {
            e.iter()
                .find(|e| e["sha256"] == sha256_bytes(edited).as_str())
        })
        .unwrap_or_else(|| panic!("the manifest has no such human edit: {manifest}"));
    assert_eq!(recorded["actor"]["kind"], "claude_code", "{recorded}");
}

#[test] // spec: CORE-034
fn notify_without_by_stays_a_human_edit() {
    let mut s = served_without_watcher();
    let edited = b"edited by hand";
    edit_outside(&sealed(&s), edited);
    notify(&s, &[], &sealed(&s));

    let entry = edit_with(&mut s.pipe, &sha256_bytes(edited));
    // The caller was `cli`: the entry gets that actor and stays a human edit.
    assert_eq!(entry["actor"]["kind"], "cli", "{entry}");
    assert!(
        entry.get("relabelled").is_none_or(Value::is_null),
        "{entry}"
    );
    assert!(
        !AGENT_KINDS.iter().any(|k| entry["actor"]["kind"] == *k),
        "{entry}"
    );
}

#[test] // spec: CORE-034, APP-039
fn ai_edit_relabels_one_entry_from_the_app_only() {
    let mut s = served_without_watcher();
    let (one, two) = (
        b"first outside edit".as_slice(),
        b"second outside edit".as_slice(),
    );
    edit_outside(&sealed(&s), one);
    notify(&s, &[], &sealed(&s));
    edit_outside(&sealed(&s), two);
    notify(&s, &[], &sealed(&s));
    let (sha_one, sha_two) = (sha256_bytes(one), sha256_bytes(two));
    let params = json!({"project": PROJECT, "asset_id": CRANE, "path": CRANE_SEALED,
                        "sha256": sha_one});

    // Only the app may relabel.
    assert_eq!(
        app_error(&mut s.pipe, "app/asset.ai_edit", params.clone()),
        "caller.not_allowed"
    );
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    let result = app_ok(&mut app, "app/asset.ai_edit", params);
    let relabelled = &result["human_edit"];
    assert_eq!(relabelled["sha256"], sha_one.as_str(), "{result}");
    assert_eq!(relabelled["actor"]["kind"], "claude_code", "{result}");
    assert!(relabelled["relabelled"]["utc"].is_string(), "{result}");

    let first = edit_with(&mut s.pipe, &sha_one);
    assert_eq!(first["actor"]["kind"], "claude_code", "{first}");
    assert!(first["relabelled"]["utc"].is_string(), "{first}");
    // Nothing relabels in bulk: the other entry stays a human edit.
    let second = edit_with(&mut s.pipe, &sha_two);
    assert!(
        !AGENT_KINDS.iter().any(|k| second["actor"]["kind"] == *k),
        "{second}"
    );
    assert!(
        second.get("relabelled").is_none_or(Value::is_null),
        "{second}"
    );

    // No entry with that path and hash.
    assert_eq!(
        app_error(
            &mut app,
            "app/asset.ai_edit",
            json!({"project": PROJECT, "asset_id": CRANE, "path": CRANE_SEALED,
                   "sha256": sha256_bytes(b"never written")}),
        ),
        "asset.unknown"
    );
}
