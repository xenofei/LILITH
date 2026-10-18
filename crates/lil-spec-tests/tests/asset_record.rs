//! Asset tools and the Record stage: imports become candidate runs with a hand-off block,
//! `asset.request_signoff`, the asset's live topic with events and snapshots, `proj:` inputs
//! recorded in the project's `links`, and minimal-diff edits of a legacy manifest (spec
//! ARCH-032, ARCH-044, ARCH-076, ARCH-077, CORE-019, CORE-020, CORE-023, CORE-033, CORE-047,
//! CORE-053, CORE-148; Appendix E "Product tools", "Topics", "Manifest fields this plan
//! writes"). Driven through the asset tools and `lil/subscribe` on a real core, and the files
//! on disk (QA-018).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lil_testkit::{CoreFixture, RawPipe, TestEnv, utc};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
/// The new asset these tests import; the fixture has no asset of that id.
const KITE: &str = "prop_kite";

/// A fresh idempotency key for every write call (ARCH-035).
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-ar-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A copy of the fixture workspace, registered as this channel's default.
fn default_workspace(env: &TestEnv) -> PathBuf {
    let ws = env.copy_fixture_workspace("assets");
    env.register_workspace(&ws, true);
    ws
}

/// A connection whose hello names the workspace `ws`, checked to succeed.
fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(
        hello["result"]["status"],
        "ok",
        "hello naming {}: {hello}",
        ws.display()
    );
    pipe
}

/// The file at the workspace-relative `rel` (with `/` separators).
fn file(ws: &Path, rel: &str) -> PathBuf {
    ws.join(rel.replace('/', "\\"))
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn manifest_path(ws: &Path, asset_id: &str) -> PathBuf {
    file(
        ws,
        &format!("projects/sketchbook/assets/{asset_id}/{asset_id}.manifest.json"),
    )
}

fn manifest(ws: &Path, asset_id: &str) -> Value {
    let path = manifest_path(ws, asset_id);
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// `asset.import` (copy) of a file made outside the workspace into the new asset; returns the
/// `structured` result.
fn import_kite(env: &TestEnv, pipe: &mut RawPipe) -> Value {
    let source = env.root().join("import-src").join("kite.png");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"kite one").unwrap();
    pipe.tool_ok(
        "asset.import",
        json!({
            "project": "sketchbook",
            "asset_id": KITE,
            "type": "image",
            "files": [source.to_str().unwrap()],
            "mode": "copy",
        }),
        Some(&key()),
    )
}

/// True for a UUID v4 in its canonical lowercase-or-uppercase hex form.
fn is_uuid_v4(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    let hex = |g: &str| g.bytes().all(|b| b.is_ascii_hexdigit());
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(n, g)| g.len() == *n && hex(g))
        && groups[2].starts_with('4')
        && groups[3].starts_with(['8', '9', 'a', 'b', 'A', 'B'])
}

/// The lines directly under `## Done` that are entries (`- …`).
fn done_entries(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| l.trim_end() != "## Done")
        .skip(1)
        .take_while(|l| l.starts_with("- "))
        .map(str::to_string)
        .collect()
}

#[test] // spec: CORE-020, CORE-023, CORE-047
fn import_creates_candidate_run_with_handoff_and_uuid() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let progress = file(&ws, "projects/sketchbook/progress.md");
    let before = done_entries(&read_text(&progress));
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let day_before = utc::today();
    let result = import_kite(&env, &mut pipe);
    let day_after = utc::today();

    let handoff = &result["handoff"];
    for list in ["unlocks", "made_stale", "refs"] {
        assert!(
            handoff[list].is_array(),
            "handoff.{list} is not a list: {result}"
        );
    }
    let id = env.workspace_id(&ws);
    assert!(
        result["ref"]
            .as_str()
            .is_some_and(|r| r.starts_with(&format!("lil://{id}/sketchbook/{KITE}@candidate/"))),
        "the import's ref is the first file's candidate ref: {result}"
    );

    let m = manifest(&ws, KITE);
    let uuid = m["creation_uuid"].as_str().unwrap_or_default();
    assert!(
        is_uuid_v4(uuid),
        "creation_uuid {uuid:?} is not a UUID v4: {m}"
    );
    assert_eq!(
        m["candidate_runs"].as_array().map(Vec::len),
        Some(1),
        "one candidate run: {m}"
    );
    assert_eq!(m["status"], "candidate", "{m}");

    let after = done_entries(&read_text(&progress));
    assert_eq!(
        after.len(),
        before.len() + 1,
        "one new `## Done` line: {after:?}"
    );
    assert_eq!(
        after[1..],
        before[..],
        "the new entry goes first: {after:?}"
    );
    assert!(
        [&day_before, &day_after]
            .iter()
            .any(|d| after[0].starts_with(&format!("- {d}: "))),
        "{:?} is not `- YYYY-MM-DD: …`",
        after[0]
    );
}

#[test] // spec: CORE-053
fn request_signoff_sets_in_review() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    assert_eq!(manifest(&ws, "chr_paper_crane")["status"], "candidate");

    let result = pipe.tool_ok(
        "asset.request_signoff",
        json!({"project": "sketchbook", "asset_id": "chr_paper_crane"}),
        Some(&key()),
    );
    assert_eq!(result["status"], "in_review", "{result}");
    assert_eq!(manifest(&ws, "chr_paper_crane")["status"], "in_review");
}

#[test] // spec: ARCH-044, ARCH-032
fn asset_topic_delivers_events_and_snapshots() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut a = session(&env, &ws);
    let imported = import_kite(&env, &mut a);
    let run_id = imported["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("asset.import returned no run_id: {imported}"))
        .to_string();

    let topic = format!("asset:lil://{}/sketchbook/{KITE}", env.workspace_id(&ws));
    let subscribed = a.call("lil/subscribe", json!({"topics": [topic], "since_seq": {}}));
    assert_eq!(subscribed["result"]["status"], "ok", "{subscribed}");

    a.tool_ok(
        "asset.signoff",
        json!({"project": "sketchbook", "asset_id": KITE, "run_id": run_id, "files": ["kite.png"]}),
        Some(&key()),
    );
    loop {
        let event = a
            .next_event(DEADLINE)
            .unwrap_or_else(|| panic!("no version_sealed event on {topic} within {DEADLINE:?}"));
        if event["topic"] == topic.as_str()
            && event["kind"] == "event"
            && event["data"]["kind"] == "version_sealed"
        {
            assert_eq!(event["data"]["version"], "1.0.0", "{event}");
            break;
        }
    }

    let mut b = session(&env, &ws);
    let mut since = serde_json::Map::new();
    since.insert(topic.clone(), json!(999));
    let subscribed = b.call(
        "lil/subscribe",
        json!({"topics": [topic], "since_seq": since}),
    );
    assert_eq!(subscribed["result"]["status"], "ok", "{subscribed}");
    let first = b
        .next_event(DEADLINE)
        .unwrap_or_else(|| panic!("no snapshot on {topic} within {DEADLINE:?}"));
    assert_eq!(first["topic"], topic.as_str(), "{first}");
    assert_eq!(first["kind"], "snapshot", "{first}");
    assert_eq!(first["data"]["asset_id"], KITE, "{first}");
}

#[test] // spec: CORE-019
fn proj_input_is_recorded_in_project_links() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let project_yaml = file(&ws, "projects/sketchbook/project.yaml");
    let before = read_text(&project_yaml);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    pipe.tool_ok(
        "asset.pin_input",
        json!({
            "project": "sketchbook",
            "asset_id": "chr_paper_crane",
            "ref": "proj:harbor/chr_x@1.0.0",
            "reason": "a reference from another project",
        }),
        Some(&key()),
    );

    let after = read_text(&project_yaml);
    assert!(
        after.contains("proj:harbor"),
        "`links` did not gain proj:harbor:\n{after}"
    );
    assert!(
        !after.lines().any(|l| l.trim_end() == "links: []"),
        "`links` is still empty:\n{after}"
    );
    // Every other line, the comment first among them, is kept in its order.
    let kept: Vec<&str> = before
        .lines()
        .filter(|l| l.trim_end() != "links: []")
        .collect();
    let mut rest = after.lines();
    for line in &kept {
        assert!(
            rest.any(|l| l == *line),
            "{line:?} was lost or moved:\n{after}"
        );
    }
    assert!(
        before.lines().next().is_some_and(|l| l.starts_with('#'))
            && after.lines().next() == before.lines().next(),
        "the comment line changed:\n{after}"
    );
}

#[test] // spec: ARCH-076, ARCH-077, CORE-033, CORE-148
fn legacy_manifest_edit_is_a_minimal_diff() {
    const LAMP: &str = "prop_signal_lamp";
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let path = manifest_path(&ws, LAMP);
    let before = read_text(&path);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let detail = pipe.tool_ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": LAMP}),
        None,
    );
    let versions = detail["versions"]
        .as_array()
        .unwrap_or_else(|| panic!("asset.get has no versions: {detail}"));
    assert!(
        versions
            .iter()
            .any(|v| v["version"] == "1.0.0" && v["legacy"] == true),
        "v1.0.0 of the legacy asset is not listed as legacy: {detail}"
    );

    let result = pipe.tool_ok(
        "asset.request_signoff",
        json!({"project": "sketchbook", "asset_id": LAMP}),
        Some(&key()),
    );
    assert_eq!(result["status"], "in_review", "{result}");

    let after = read_text(&path);
    let parsed: Value = serde_json::from_str(&after)
        .unwrap_or_else(|e| panic!("not JSON after the edit ({e}):\n{after}"));
    assert_eq!(parsed["status"], "in_review", "{after}");
    assert!(
        !after.contains("\"schema_version\""),
        "a legacy manifest never gains schema_version:\n{after}"
    );

    // Line by line: only the status line differs, then new keys may follow the last old key
    // (which then gains a comma) before the closing brace.
    let old: Vec<&str> = before.split('\n').collect();
    let new: Vec<&str> = after.split('\n').collect();
    let status = old
        .iter()
        .position(|l| l.trim_start().starts_with("\"status\":"))
        .expect("the fixture has a status line");
    let close = old
        .iter()
        .rposition(|l| l.trim_end() == "}")
        .expect("the fixture ends with a closing brace");
    let last_key = close - 1;
    assert!(new.len() >= old.len(), "lines were removed:\n{after}");
    assert_eq!(
        new[..status],
        old[..status],
        "lines before status changed:\n{after}"
    );
    assert_eq!(
        new[status],
        old[status].replace("\"candidate\"", "\"in_review\""),
        "{after}"
    );
    assert_eq!(
        new[status + 1..last_key],
        old[status + 1..last_key],
        "lines after status changed:\n{after}"
    );
    assert!(
        new[last_key] == old[last_key] || new[last_key] == format!("{},", old[last_key]),
        "the last old key changed: {:?} -> {:?}",
        old[last_key],
        new[last_key]
    );
    let appended = new.len() - old.len();
    assert_eq!(
        new[close + appended..],
        old[close..],
        "the end of the file changed:\n{after}"
    );
    if appended == 0 {
        assert_eq!(new[last_key], old[last_key], "{after}");
    }
}
