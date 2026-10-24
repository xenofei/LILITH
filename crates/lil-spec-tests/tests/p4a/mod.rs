//! Helpers shared by the plan-4a spec tests (QA-017, QA-018), next to [`crate::support`]. They
//! drive only the stable surfaces: `app/*` calls from `TestApp`, the `test.desktop` and `test.job`
//! test-channel tools, the `jobs.list` lanes and the files an outside program reads. Each test
//! binary uses a part of them, so unused ones are allowed here.
//!
//! `test.desktop` (a test contract of plan 4a, not in the spec) sets the core's `FakeDesktop` and
//! returns what it recorded: `{toasts: [{op: "post" | "remove", tag, group, title?, body?,
//! launch?, silent?}], power_held, polls}`. The toast tests also read each posted toast's XML
//! (`xml`, the `Toast.xml` the core handed the desktop) for its actions and escaping.

#![allow(
    dead_code,
    reason = "each spec-test binary uses a different part of these helpers"
)]

use std::path::Path;

use lil_testkit::{TestApp, TestEnv};
use serde_json::{Value, json};

use crate::support::*;

/// Protocol 1.2's capabilities, in order (Appendix D): what a plan-4a app offers in its hello.
pub const CAPS_4A: &[&str] = &[
    "events.v1",
    "tools.v1",
    "jobs.v1",
    "confirmations.v1",
    "caps.v1",
    "queue.v1",
    "desktop.v1",
    "context.v1",
];

/// The `ui:test` app offering protocol 1.2's capabilities.
pub fn app_4a(env: &TestEnv, ws: Option<&Path>, topics: &[&str]) -> TestApp {
    TestApp::start_with_capabilities(env, ws, topics, CAPS_4A)
}

/// An `app/*` call that must succeed: its `result` (the `Outcome`'s fields).
pub fn app_ok(c: &mut impl Caller, method: &str, params: Value) -> Value {
    let response = c.call(method, params.clone());
    assert_eq!(
        response["result"]["status"], "ok",
        "{method} {params} did not succeed: {response}"
    );
    response["result"].clone()
}

/// The error code of an `app/*` call that must fail.
pub fn app_error(c: &mut impl Caller, method: &str, params: Value) -> String {
    let response = c.call(method, params.clone());
    response["result"]["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("{method} {params} did not fail with a code: {response}"))
        .to_string()
}

/// The channel's activation scheme: `lil-test-<id>` (Appendix G "Activation and registration").
pub fn scheme(env: &TestEnv) -> String {
    format!("lil-{}", env.env.channel())
}

/// The channel's `<id>`, the part after `test-`.
pub fn channel_id(env: &TestEnv) -> String {
    let channel = env.env.channel().to_string();
    channel
        .strip_prefix("test-")
        .unwrap_or_else(|| panic!("not a test channel: {channel}"))
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// The fake desktop

/// `test.desktop` with `args`: sets the fake desktop and returns what it recorded.
pub fn desktop(c: &mut impl Caller, args: Value) -> Value {
    write_ok(c, "test.desktop", args)
}

/// A desktop where toasts can reach you: toasts ready, the shortcut and the handlers in place,
/// another program in front, no fullscreen; the recorded toasts cleared.
pub fn ready_desktop(c: &mut impl Caller) {
    desktop(
        c,
        json!({"toasts": "ready", "shortcut": true, "handlers": true, "foreground": "other",
               "fullscreen": "no", "clear": true}),
    );
}

/// Every toast operation recorded so far, in order.
pub fn toast_ops(c: &mut impl Caller) -> Vec<Value> {
    let seen = desktop(c, json!({}));
    seen["toasts"]
        .as_array()
        .unwrap_or_else(|| panic!("test.desktop recorded no toasts list: {seen}"))
        .clone()
}

/// The toasts posted in `group` (a category), in order.
pub fn posts_in(c: &mut impl Caller, group: &str) -> Vec<Value> {
    toast_ops(c)
        .into_iter()
        .filter(|t| t["op"] == "post" && t["group"] == group)
        .collect()
}

/// Polls until at least one toast is posted in `group`; returns the posts then.
pub fn until_posted(c: &mut impl Caller, group: &str) -> Vec<Value> {
    poll(&format!("a {group} toast"), || {
        let posts = posts_in(c, group);
        (!posts.is_empty()).then_some(posts)
    })
}

/// `desktop.get {fresh: true}`: the desktop settings with the notification path recomputed.
pub fn desktop_get(c: &mut impl Caller) -> Value {
    ok(c, "desktop.get", json!({"fresh": true}), None)
}

/// Checks that the notification path can reach you (Appendix G "Reachable"), recomputed now.
pub fn path_ready(c: &mut impl Caller) {
    let settings = desktop_get(c);
    let path = &settings["notification_path"];
    assert_eq!(path["toasts"], "ready", "{settings}");
    assert_eq!(path["shortcut"], "ok", "{settings}");
    assert_eq!(path["handlers"], "ok", "{settings}");
    assert_eq!(
        settings["notifications"]["confirmation_waiting"], true,
        "{settings}"
    );
}

/// The posted toast's XML.
pub fn toast_xml(toast: &Value) -> String {
    toast["xml"]
        .as_str()
        .unwrap_or_else(|| panic!("the recorded toast carries no xml: {toast}"))
        .to_string()
}

/// The `(content, arguments)` of each `<action>` in the toast's XML, in order.
pub fn toast_actions(toast: &Value) -> Vec<(String, String)> {
    let xml = toast_xml(toast);
    let attr = |element: &str, name: &str| -> String {
        let key = format!("{name}=\"");
        element
            .find(&key)
            .map(|at| {
                let rest = &element[at + key.len()..];
                rest[..rest.find('"').unwrap_or(rest.len())].to_string()
            })
            .unwrap_or_default()
    };
    xml.split("<action ")
        .skip(1)
        .map(|element| {
            let element = &element[..element.find('>').unwrap_or(element.len())];
            (attr(element, "content"), attr(element, "arguments"))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Jobs and lanes

/// The `QueueState` of `lane` in `jobs.list`'s `lanes`.
pub fn lane_state(c: &mut impl Caller, lane: &str) -> Value {
    let list = ok(c, "jobs.list", json!({}), None);
    list["lanes"]
        .as_array()
        .and_then(|lanes| lanes.iter().find(|l| l["lane"] == lane))
        .cloned()
        .unwrap_or_else(|| panic!("jobs.list has no {lane} lane: {list}"))
}

/// The strings of a JSON array, sorted (for reason sets).
pub fn sorted_strings(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .map(|s| {
            s.as_str()
                .unwrap_or_else(|| panic!("not a string: {s}"))
                .to_string()
        })
        .collect();
    out.sort();
    out
}

/// The job ids of a `{jobs}` result.
pub fn job_ids_of(structured: &Value) -> Vec<String> {
    structured["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("no jobs list in {structured}"))
        .iter()
        .map(|j| {
            j["job_id"]
                .as_str()
                .unwrap_or_else(|| panic!("a job without a job_id: {j}"))
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Formats

/// The value text of `key` in `[table]` of a TOML document, read line by line as an outside
/// program would (`key = value`, comments after `#` dropped).
pub fn toml_value(text: &str, table: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().to_string();
        } else if current == table
            && let Some((k, v)) = line.split_once('=')
            && k.trim().trim_matches('"') == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// `s` is `n` lowercase hex digits.
pub fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding (RFC 4648 §5).
pub fn b64url(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(B64URL[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// The bytes of unpadded base64url text, or `None` when it isn't that.
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0_u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = B64URL.iter().position(|b| *b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (text.len() % 4 != 1).then_some(out)
}

/// The target of a `<scheme>:open/<base64url>` URL.
pub fn open_target(scheme: &str, url: &str) -> String {
    let prefix = format!("{scheme}:open/");
    let b64 = url
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("{url:?} is not a {prefix} URL"));
    let bytes = b64url_decode(b64).unwrap_or_else(|| panic!("{b64:?} is not base64url"));
    String::from_utf8(bytes).unwrap_or_else(|e| panic!("{url:?} decodes to no UTF-8: {e}"))
}
