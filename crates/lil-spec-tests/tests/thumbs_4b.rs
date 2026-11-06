//! Plan 4b's thumbnails: `app/thumbs.request` answers `ready` or `pending` (then announced on the
//! `thumbs` topic) with a cached PNG at `round(tier × scale)` pixels, one cache entry per bytes,
//! the `icon` state for anything without a renderer, only with `assets.v1`; and `handlers.for`
//! with no handler says so (spec APP-038, APP-039; Appendix D "Protocol 1.4", Appendix H
//! "Thumbnails"). Images are made with `lil_testkit::png`. Driven through `app/thumbs.request`
//! from the `ui:test` app, `asset.import` and `handlers.for` from a `cli` connection, and the
//! cached files (QA-018).

mod p4b;
mod support;

use std::path::{Path, PathBuf};

use lil_testkit::{TestApp, png, sha256_bytes};
use p4b::*;
use serde_json::{Value, json};
use support::*;

const KITE: &str = "prop_kite";
const GLIDER: &str = "prop_glider";

/// A `w` × `h` PNG with a pattern in it.
fn picture(w: u32, h: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
    png::rgba(w, h, &pixels)
}

/// The workspace-relative path of `name` in run `run_id` of `asset_id`.
fn run_path(asset_id: &str, run_id: &str, name: &str) -> String {
    format!("projects/{PROJECT}/assets/{asset_id}/candidates/{run_id}/{name}")
}

/// `app/thumbs.request` of `paths` with `extra` arguments (`tier`, `scale`, …): its `thumbs`.
fn request(app: &mut TestApp, paths: &[String], extra: Value) -> Vec<Value> {
    let mut params =
        json!({ "items": paths.iter().map(|p| json!({ "path": p })).collect::<Vec<_>>() });
    for (k, v) in extra.as_object().into_iter().flatten() {
        params[k] = v.clone();
    }
    let result = app_ok(app, "app/thumbs.request", params);
    let thumbs = result["thumbs"]
        .as_array()
        .unwrap_or_else(|| panic!("no thumbs list: {result}"))
        .clone();
    assert_eq!(thumbs.len(), paths.len(), "one answer per item: {result}");
    thumbs
}

/// The next `thumbs` event of `kind` for `sha256`; others are skipped.
fn thumbs_event(app: &mut TestApp, kind: &str, sha256: &Value) -> Value {
    poll(&format!("a {kind} thumbs event for {sha256}"), || {
        let event = app.next_event(std::time::Duration::from_millis(200))?;
        let data = &event["data"];
        (event["topic"] == "thumbs" && data["kind"] == kind && &data["sha256"] == sha256)
            .then(|| data.clone())
    })
}

/// The cached file of a `ready` thumb, or of a `pending` one once its `ready` event comes.
fn ready_file(app: &mut TestApp, thumb: &Value) -> PathBuf {
    let file = match thumb["state"].as_str() {
        Some("ready") => thumb["file"].clone(),
        Some("pending") => thumbs_event(app, "ready", &thumb["sha256"])["file"].clone(),
        _ => panic!("not ready or pending: {thumb}"),
    };
    PathBuf::from(
        file.as_str()
            .unwrap_or_else(|| panic!("a ready thumb names no file: {thumb}")),
    )
}

/// Checks that `file` is the cache entry `<local dir>\thumbs\<sha256[0..2]>\<sha256>.….<px>.png`.
fn assert_cache_entry(env_local: &Path, file: &Path, sha256: &str, px: u32) {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    assert!(
        name.starts_with(&format!("{sha256}.")) && name.ends_with(&format!(".{px}.png")),
        "{}",
        file.display()
    );
    let shard = file
        .parent()
        .unwrap_or_else(|| panic!("{}", file.display()));
    assert_eq!(
        shard.file_name().unwrap_or_default().to_string_lossy(),
        sha256[..2],
        "{}",
        file.display()
    );
    let thumbs = shard
        .parent()
        .unwrap_or_else(|| panic!("{}", file.display()));
    assert_eq!(
        std::fs::canonicalize(thumbs).ok(),
        std::fs::canonicalize(env_local.join("thumbs")).ok(),
        "{} is not under <local dir>\\thumbs",
        file.display()
    );
}

#[test] // spec: APP-038
fn thumbs_request_returns_ready_or_pending_then_ready() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &["thumbs"]);
    let bytes = picture(1024, 512);
    let sha = sha256_bytes(&bytes);
    let run_id = run_id_of(&import(&mut s.pipe, &s.env, KITE, &[("big.png", &bytes)]));
    let path = run_path(KITE, &run_id, "big.png");

    let thumbs = request(
        &mut app,
        std::slice::from_ref(&path),
        json!({"tier": 256, "scale": 1.5}),
    );
    let thumb = &thumbs[0];
    assert_eq!(thumb["path"], path.as_str(), "{thumb}");
    assert_eq!(thumb["sha256"], sha.as_str(), "{thumb}");
    assert_eq!(thumb["media_kind"], "image", "{thumb}");
    assert!(
        thumb["renderer"]
            .as_str()
            .is_some_and(|r| matches!(r, "image" | "image@1")),
        "{thumb}"
    );
    assert_eq!(thumb["px"], 384, "round(256 x 1.5): {thumb}");

    let file = ready_file(&mut app, thumb);
    assert_cache_entry(&s.env.env.local_dir(), &file, &sha, 384);
    let cached = std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert_eq!(png::long_side(&cached), 384, "{}", file.display());
}

#[test] // spec: APP-038
fn same_bytes_share_one_cache_entry() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &["thumbs"]);
    let bytes = picture(300, 200);
    let sha = sha256_bytes(&bytes);
    let kite_run = run_id_of(&import(&mut s.pipe, &s.env, KITE, &[("a.png", &bytes)]));
    let glider_run = run_id_of(&import(&mut s.pipe, &s.env, GLIDER, &[("b.png", &bytes)]));
    let paths = [
        run_path(KITE, &kite_run, "a.png"),
        run_path(GLIDER, &glider_run, "b.png"),
    ];

    let thumbs = request(&mut app, &paths, json!({"tier": 96}));
    let first = ready_file(&mut app, &thumbs[0]);
    // The second answer is ready now, or announced by the same render's event.
    let thumbs = request(&mut app, &paths, json!({"tier": 96}));
    let files: Vec<PathBuf> = thumbs.iter().map(|t| ready_file(&mut app, t)).collect();
    assert_eq!(files[0], first);
    assert_eq!(files[1], first, "same bytes, one cache entry");
    let shard = s.env.env.local_dir().join("thumbs").join(&sha[..2]);
    let entries = files_named(&shard, &sha, ".96.png");
    assert_eq!(entries.len(), 1, "{entries:?}");
}

#[test] // spec: APP-038
fn non_images_get_the_icon_state() {
    let mut s = Served::start();
    let mut app = app_4b(&s.env, Some(&s.ws), &["thumbs"]);
    let run_id = run_id_of(&import(
        &mut s.pipe,
        &s.env,
        KITE,
        &[
            ("notes.txt", b"plain text notes"),
            ("fake.png", b"this is not a png"),
        ],
    ));
    let paths = [
        run_path(KITE, &run_id, "notes.txt"),
        run_path(KITE, &run_id, "fake.png"),
    ];

    let thumbs = request(&mut app, &paths, json!({"tier": 160}));
    assert_eq!(thumbs[0]["media_kind"], "text", "{thumbs:?}");
    for thumb in &thumbs {
        let settled = match thumb["state"].as_str() {
            Some("icon") => thumb.clone(),
            Some("pending") => thumbs_event(&mut app, "icon", &thumb["sha256"]),
            _ => panic!("not icon or pending: {thumb}"),
        };
        assert!(
            settled.get("file").is_none_or(Value::is_null),
            "an icon has no file: {settled}"
        );
        if settled == *thumb {
            assert!(
                thumb["renderer"]
                    .as_str()
                    .is_some_and(|r| matches!(r, "icon" | "icon@1")),
                "{thumb}"
            );
        }
    }
}

#[test] // spec: APP-038, APP-039
fn thumbs_need_assets_v1() {
    let mut s = Served::start();
    let path = format!("projects/{PROJECT}/assets/{CRANE}/{CRANE_SEALED}");
    let params = json!({"items": [{"path": path}], "tier": 96});

    let mut app = app_4b(&s.env, Some(&s.ws), &[]);
    assert!(
        strings(&app.hello()["capabilities"]).contains(&"assets.v1".to_string()),
        "the core shares assets.v1: {}",
        app.hello()
    );
    app_ok(&mut app, "app/thumbs.request", params.clone());

    let mut older = TestApp::start_with_capabilities(&s.env, Some(&s.ws), &[], CAPS_13);
    assert_eq!(
        app_error(&mut older, "app/thumbs.request", params.clone()),
        "caller.not_allowed"
    );
    assert_eq!(
        app_error(&mut s.pipe, "app/thumbs.request", params),
        "caller.not_allowed"
    );
}

#[test] // spec: APP-038
fn handlers_for_says_no_studio_opens_this_yet() {
    let mut s = Served::start();
    let target = s.asset_dir(CRANE).join(CRANE_SEALED.replace('/', "\\"));
    let answer = ok(
        &mut s.pipe,
        "handlers.for",
        json!({ "target": path_str(&target) }),
        None,
    );
    assert_eq!(
        answer,
        json!({"handlers": [], "note": "no studio opens this yet"}),
        "{answer}"
    );
}
