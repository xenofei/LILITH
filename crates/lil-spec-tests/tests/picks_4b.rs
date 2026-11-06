//! Plan 4b's pick log: a review decision from the app (a `ui:*` call that carries
//! `review_view`) appends one line to `picks.jsonl` in the workspace's local folder (ARCH-097,
//! `TestEnv::workspace_local_dir`) and copies the 96 px thumbnail to its `picks\thumbs\`; the same
//! mark from elsewhere appends nothing (spec CORE-176; Appendix H "Pick log"). Images are made
//! with `lil_testkit::png`. Driven through `asset.mark` from the `ui:test` app and a `cli`
//! connection, and the local files (QA-018).

mod p4b;
mod support;

use lil_testkit::{png, sha256_bytes};
use p4b::*;
use serde_json::json;
use support::*;

const KITE: &str = "prop_kite";

#[test] // spec: CORE-176
fn review_picks_from_the_app_are_logged() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    let pixels: Vec<u8> = (0..200 * 100 * 4).map(|i| (i % 253) as u8).collect();
    let bytes = png::rgba(200, 100, &pixels);
    let sha = sha256_bytes(&bytes);
    let run_id = run_id_of(&import(&mut s.pipe, &s.env, KITE, &[("kite.png", &bytes)]));
    // The workspace's local folder (Appendix H's `<local dir>` here; ARCH-097, CORE-176).
    let local = s.env.workspace_local_dir(&s.ws);
    let log = local.join("picks.jsonl");

    write_ok(
        &mut app,
        "asset.mark",
        json!({"project": PROJECT, "asset_id": KITE, "run_id": run_id, "take": 1,
               "flag": "pick", "review_view": "loupe"}),
    );
    let lines = poll("a line in picks.jsonl", || {
        let lines = jsonl(&log);
        (!lines.is_empty()).then_some(lines)
    });
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["decision"], "pick", "{line}");
    assert_eq!(line["view"], "loupe", "{line}");
    assert_eq!(line["project"], PROJECT, "{line}");
    assert_eq!(line["asset_id"], KITE, "{line}");
    assert_eq!(line["run_id"], run_id.as_str(), "{line}");
    assert!(
        line["file"]
            .as_str()
            .is_some_and(|f| f == "kite.png" || f.ends_with("/kite.png")),
        "{line}"
    );
    assert_eq!(line["sha256"], sha.as_str(), "{line}");
    assert_eq!(line["take"], 1, "{line}");
    assert_eq!(line["primary"], true, "{line}");
    // The take hash: SHA-256 of `<sha256>  <file name>\n` per file, sorted by name.
    let take_sha = sha256_bytes(format!("{sha}  kite.png\n").as_bytes());
    assert_eq!(line["take_sha256"], take_sha.as_str(), "{line}");
    assert!(line["utc"].is_string(), "{line}");

    // The same decision from a cli caller isn't a review decision.
    write_ok(
        &mut s.pipe,
        "asset.mark",
        json!({"project": PROJECT, "asset_id": KITE, "run_id": run_id, "take": 1,
               "flag": "reject", "review_view": "loupe"}),
    );

    let thumb = local.join(r"picks\thumbs").join(format!("{sha}.png"));
    let cached = poll(&format!("{}", thumb.display()), || {
        std::fs::read(&thumb).ok()
    });
    assert_eq!(png::long_side(&cached), 96, "the 96 px tier at scale 1");
    assert_eq!(jsonl(&log).len(), 1, "a cli call appends nothing");
}
