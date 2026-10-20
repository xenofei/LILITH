//! ComfyUI shape fixtures and the drift check (QA-067).
//!
//! [`capture`] runs one fixed probe against a ComfyUI on a loopback URL, the real one (by the
//! `real_*` tests, which `lil test --real --record <dir>` asks to record) or the fake
//! ([`crate::FakeComfy`]). The probe:
//! - opens the websocket `/ws?clientId=<id>` and announces an empty `feature_flags`;
//! - posts one tiny prompt (`EmptyImage` 8×8 into `SaveImage` with the prefix
//!   [`CAPTURE_PREFIX`]; no model) and keeps every text message until the prompt ends;
//! - reads `/api/jobs/{id}`, `/api/jobs` (its own row only), `/api/jobs` with a bad `status`,
//!   the per-job and the batch cancel of its own, finished prompt, `/queue` and
//!   `/history/{id}`.
//!
//! It never cancels, frees or interrupts any other prompt, and it reads no model. The tiny
//! prompt leaves one 8×8 PNG named `lil_shape_probe_*` in ComfyUI's `output\` folder.
//!
//! A recording is the [`Capture::scrubbed`] probe of one ComfyUI version, in
//! `crates/lil-testkit/fixtures/comfyui/<version>/`: `ws_messages.jsonl` (one message per
//! line), `api_jobs.json`, `queue.json` and `history.json`. Scrubbing replaces every string
//! with `"<string>"` (prompts, file names, paths and ids), except each websocket message's
//! `type`; numbers, booleans and every key stay, and the probe's prompt id as a key of
//! `/history` becomes `"<prompt_id>"`. The drift check compares key sets and JSON types
//! ([`shape`], [`shape_drift`]); the behaviour check compares the answers' status codes and
//! the values that aren't strings.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tokio_tungstenite::tungstenite::Message;

/// The `SaveImage` prefix of the probe's prompt.
pub const CAPTURE_PREFIX: &str = "lil_shape_probe";
/// What every string becomes in a recording.
pub const SCRUBBED: &str = "<string>";
/// The key the probe's prompt id becomes in `history.json`.
pub const PROMPT_ID_KEY: &str = "<prompt_id>";
/// The fixture files of one recorded version.
pub const FILES: [&str; 4] = [
    "ws_messages.jsonl",
    "api_jobs.json",
    "queue.json",
    "history.json",
];

/// How long the probe waits for its prompt, or for any one answer.
const WAIT: Duration = Duration::from_secs(120);

/// One probe of one ComfyUI (module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    /// `system_stats.system.comfyui_version`.
    pub version: String,
    /// Every text message of the websocket, in order.
    pub ws_messages: Vec<Value>,
    /// `{post_prompt, item, list, bad_status, cancel_one, cancel_batch}`: each an answer
    /// `{status, body}`, except `list`, the `/api/jobs` body with only the probe's row.
    pub api_jobs: Value,
    /// The `/queue` body.
    pub queue: Value,
    /// The `/history/{id}` body, keyed by [`PROMPT_ID_KEY`].
    pub history: Value,
}

/// The fixtures folder of the test kit: `crates/lil-testkit/fixtures/comfyui`.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("comfyui")
}

/// The newest version recorded under `dir` (folders named `<major>.<minor>.<patch>`), with its
/// folder; `None` when nothing is recorded yet.
pub fn newest_recorded(dir: &Path) -> Option<(String, PathBuf)> {
    let parse = |name: &str| -> Option<Vec<u64>> {
        let parts: Vec<u64> = name
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<_>>()?;
        (parts.len() == 3).then_some(parts)
    };
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            parse(&name).map(|v| (v, name, e.path()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, name, path)| (name, path))
}

impl Capture {
    /// The capture with every string scrubbed (module docs).
    pub fn scrubbed(&self) -> Capture {
        Capture {
            version: self.version.clone(),
            ws_messages: self
                .ws_messages
                .iter()
                .map(|m| {
                    let mut out = scrub(m);
                    if let (Some(o), Some(kind)) = (out.as_object_mut(), m.get("type")) {
                        o.insert("type".into(), kind.clone());
                    }
                    out
                })
                .collect(),
            api_jobs: scrub(&self.api_jobs),
            queue: scrub(&self.queue),
            history: scrub(&self.history),
        }
    }

    /// Writes the [`Capture::scrubbed`] capture to `<dir>/<version>/`, replacing a recording
    /// of the same version. Returns that folder.
    ///
    /// # Errors
    /// The file system's, or a version that isn't `<major>.<minor>.<patch>`.
    pub fn record(&self, dir: &Path) -> std::io::Result<PathBuf> {
        let ok = self.version.split('.').count() == 3
            && self
                .version
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        if !ok {
            return Err(std::io::Error::other(format!(
                "ComfyUI reported the version {:?}, not <major>.<minor>.<patch>",
                self.version
            )));
        }
        let clean = self.scrubbed();
        let at = dir.join(&self.version);
        std::fs::create_dir_all(&at)?;
        let mut lines = String::new();
        for m in &clean.ws_messages {
            lines.push_str(&m.to_string());
            lines.push('\n');
        }
        std::fs::write(at.join(FILES[0]), lines)?;
        for (name, value) in [
            (FILES[1], &clean.api_jobs),
            (FILES[2], &clean.queue),
            (FILES[3], &clean.history),
        ] {
            let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
            std::fs::write(at.join(name), text + "\n")?;
        }
        Ok(at)
    }

    /// A recording read back from its folder `<dir>/<version>`.
    ///
    /// # Errors
    /// A missing file or one that isn't JSON (each line, for `ws_messages.jsonl`).
    pub fn load(folder: &Path) -> Result<Capture, String> {
        let read = |name: &str| {
            std::fs::read_to_string(folder.join(name))
                .map_err(|e| format!("{}: {e}", folder.join(name).display()))
        };
        let json = |name: &str| -> Result<Value, String> {
            serde_json::from_str(&read(name)?).map_err(|e| format!("{name}: {e}"))
        };
        let ws_messages = read(FILES[0])?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}", FILES[0])))
            .collect::<Result<_, _>>()?;
        Ok(Capture {
            version: folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ws_messages,
            api_jobs: json(FILES[1])?,
            queue: json(FILES[2])?,
            history: json(FILES[3])?,
        })
    }

    /// The websocket messages by `type`: the set of [`shape`]s of each.
    pub fn ws_shapes(&self) -> BTreeMap<String, Vec<Value>> {
        let mut by_type: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for m in &self.ws_messages {
            let kind = m["type"].as_str().unwrap_or_default().to_string();
            let s = shape(m);
            let seen = by_type.entry(kind).or_default();
            if !seen.contains(&s) {
                seen.push(s);
            }
        }
        for shapes in by_type.values_mut() {
            shapes.sort_by_key(Value::to_string);
        }
        by_type
    }

    /// The `data` of the first `feature_flags` message, the server's answer to the probe's.
    pub fn feature_flags_reply(&self) -> Option<&Value> {
        self.ws_messages
            .iter()
            .find(|m| m["type"] == "feature_flags")
            .map(|m| &m["data"])
    }
}

/// `v` with every string replaced by [`SCRUBBED`]; keys stay.
pub fn scrub(v: &Value) -> Value {
    match v {
        Value::String(_) => Value::from(SCRUBBED),
        Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), scrub(v)))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// The key sets and JSON types of `v`: a leaf is `"null"`, `"bool"`, `"number"` or
/// `"string"`, an object maps each key to its shape, and an array is the sorted set of its
/// elements' shapes.
pub fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(items) => {
            let mut set: Vec<Value> = Vec::new();
            for s in items.iter().map(shape) {
                if !set.contains(&s) {
                    set.push(s);
                }
            }
            set.sort_by_key(Value::to_string);
            Value::Array(set)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), shape(v)))
                .collect::<Map<_, _>>(),
        ),
    }
}

/// Where the shapes `want` (recorded) and `got` (the fake's) differ, one line per difference,
/// named by its path from `at`: keys on one side only, a JSON type that differs, and array
/// element shapes on one side only.
pub fn shape_drift(at: &str, want: &Value, got: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                match g.get(k) {
                    Some(gv) => out.extend(shape_drift(&format!("{at}.{k}"), wv, gv)),
                    None => out.push(format!("{at}.{k}: recorded, missing from the fake")),
                }
            }
            for k in g.keys().filter(|k| !w.contains_key(*k)) {
                out.push(format!("{at}.{k}: in the fake, not recorded"));
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            for s in w.iter().filter(|s| !g.contains(s)) {
                out.push(format!(
                    "{at}[]: recorded element shape {s} is not the fake's"
                ));
            }
            for s in g.iter().filter(|s| !w.contains(s)) {
                out.push(format!(
                    "{at}[]: the fake's element shape {s} is not recorded"
                ));
            }
        }
        _ if want == got => {}
        _ => out.push(format!("{at}: recorded {want}, the fake has {got}")),
    }
    out
}

/// Every shape difference between a recording and a capture of the fake (both scrubbed or
/// not: only shapes are compared).
pub fn capture_drift(recorded: &Capture, fake: &Capture) -> Vec<String> {
    let mut out = Vec::new();
    let (want, got) = (recorded.ws_shapes(), fake.ws_shapes());
    for (kind, shapes) in &want {
        match got.get(kind) {
            None => out.push(format!("ws {kind}: recorded, the fake never sent it")),
            Some(fake_shapes) => out.extend(shape_drift(
                &format!("ws {kind}"),
                &Value::Array(shapes.clone()),
                &Value::Array(fake_shapes.clone()),
            )),
        }
    }
    for kind in got.keys().filter(|k| !want.contains_key(*k)) {
        out.push(format!("ws {kind}: sent by the fake, not recorded"));
    }
    for (name, w, g) in [
        ("api_jobs", &recorded.api_jobs, &fake.api_jobs),
        ("queue", &recorded.queue, &fake.queue),
        ("history", &recorded.history, &fake.history),
    ] {
        out.extend(shape_drift(name, &shape(w), &shape(g)));
    }
    out
}

/// Where the fake's behaviour differs from a recording's (Task 6's drift list): the status of
/// `/api/jobs` with a bad parameter, the keys of the `feature_flags` reply, and the answers
/// (status and body) of the per-job and the batch cancel.
pub fn behaviour_drift(recorded: &Capture, fake: &Capture) -> Vec<String> {
    let mut out = Vec::new();
    let status = |c: &Capture, k: &str| c.api_jobs[k]["status"].clone();
    if status(recorded, "bad_status") != status(fake, "bad_status") {
        out.push(format!(
            "/api/jobs?status=<bad>: recorded {}, the fake answers {}",
            status(recorded, "bad_status"),
            status(fake, "bad_status")
        ));
    }
    let keys = |c: &Capture| -> Option<Vec<String>> {
        c.feature_flags_reply()
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
    };
    if keys(recorded) != keys(fake) {
        out.push(format!(
            "feature_flags reply keys: recorded {:?}, the fake {:?}",
            keys(recorded),
            keys(fake)
        ));
    }
    for k in ["cancel_one", "cancel_batch"] {
        let (w, g) = (scrub(&recorded.api_jobs[k]), scrub(&fake.api_jobs[k]));
        if w != g {
            out.push(format!("{k}: recorded {w}, the fake answers {g}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The probe.
// ---------------------------------------------------------------------------------------------

/// Runs the probe (module docs) against the ComfyUI at `base`, `http://127.0.0.1:<port>`
/// (or `localhost`, `[::1]`); anything else is refused before any request.
///
/// # Errors
/// What went wrong, in words.
pub fn capture(base: &str) -> Result<Capture, String> {
    let port = loopback_port(base)?;
    let stats = answer(port, "GET", "/system_stats", None)?;
    let version = stats.1["system"]["comfyui_version"]
        .as_str()
        .ok_or_else(|| format!("/system_stats has no system.comfyui_version: {}", stats.1))?
        .trim_start_matches('v')
        .to_string();
    let prompt_id = uuid_v4();
    let client_id = uuid_v4().replace('-', "");

    let (tx, rx) = mpsc::channel::<Result<Value, String>>();
    let (ws_client, ws_prompt) = (client_id.clone(), prompt_id.clone());
    let reader = std::thread::spawn(move || ws_messages(port, &ws_client, &ws_prompt, &tx));
    // The first message is the server's `status`; the post waits for the connection.
    let mut ws = vec![
        rx.recv_timeout(WAIT)
            .map_err(|_| "the websocket said nothing".to_string())??,
    ];

    let graph = json!({
        "1": {"class_type": "EmptyImage",
              "inputs": {"width": 8, "height": 8, "batch_size": 1, "color": 0}},
        "2": {"class_type": "SaveImage",
              "inputs": {"images": ["1", 0], "filename_prefix": CAPTURE_PREFIX}},
    });
    let body = json!({"prompt": graph, "client_id": client_id, "prompt_id": prompt_id,
                      "extra_data": {"preview_method": "taesd"}});
    let posted = answer(port, "POST", "/prompt", Some(&body))?;
    if posted.0 != 200 {
        return Err(format!("POST /prompt answered {}: {}", posted.0, posted.1));
    }
    for message in rx.iter() {
        ws.push(message?);
    }
    let _ = reader.join();

    let item = answer(port, "GET", &format!("/api/jobs/{prompt_id}"), None)?;
    let mut list = answer(port, "GET", "/api/jobs?limit=100", None)?.1;
    if let Some(jobs) = list.get_mut("jobs").and_then(Value::as_array_mut) {
        jobs.retain(|j| j["id"] == prompt_id.as_str());
    }
    let bad_status = answer(port, "GET", "/api/jobs?status=not_a_status", None)?;
    let cancel_one = answer(
        port,
        "POST",
        &format!("/api/jobs/{prompt_id}/cancel"),
        Some(&json!({})),
    )?;
    let cancel_batch = answer(
        port,
        "POST",
        "/api/jobs/cancel",
        Some(&json!({"job_ids": [prompt_id]})),
    )?;
    let queue = answer(port, "GET", "/queue", None)?.1;
    let mut history = answer(port, "GET", &format!("/history/{prompt_id}"), None)?.1;
    if let Some(map) = history.as_object_mut()
        && let Some(entry) = map.remove(&prompt_id)
    {
        map.insert(PROMPT_ID_KEY.into(), entry);
    }
    let pair = |(status, body): (u16, Value)| json!({"status": status, "body": body});
    Ok(Capture {
        version,
        ws_messages: ws,
        api_jobs: json!({
            "post_prompt": pair(posted),
            "item": pair(item),
            "list": list,
            "bad_status": pair(bad_status),
            "cancel_one": pair(cancel_one),
            "cancel_batch": pair(cancel_batch),
        }),
        queue,
        history,
    })
}

/// The port of a loopback `http://` URL.
fn loopback_port(base: &str) -> Result<u16, String> {
    let rest = base
        .trim_end_matches('/')
        .strip_prefix("http://")
        .ok_or_else(|| format!("{base}: not an http:// URL"))?;
    let (host, port) = rest
        .rsplit_once(':')
        .ok_or_else(|| format!("{base}: no port"))?;
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return Err(format!("{base}: the probe talks to loopback only"));
    }
    port.parse().map_err(|e| format!("{base}: {e}"))
}

/// A random canonical lowercase UUID (version 4), as ComfyUI wants a prompt id.
pub fn uuid_v4() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seed = format!(
        "{nanos}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let mut b: [u8; 16] = Sha256::digest(seed.as_bytes())[..16]
        .try_into()
        .expect("16 bytes");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// The websocket's text messages to `tx`, after announcing an empty `feature_flags`, until
/// the prompt `prompt_id` ends (`execution_success`, `execution_error` or
/// `execution_interrupted`) or [`WAIT`] passes.
fn ws_messages(
    port: u16,
    client_id: &str,
    prompt_id: &str,
    tx: &mpsc::Sender<Result<Value, String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Err(format!("no runtime for the websocket: {e}")));
            return;
        }
    };
    runtime.block_on(async {
        let url = format!("ws://127.0.0.1:{port}/ws?clientId={client_id}");
        let (mut socket, _) = match tokio_tungstenite::connect_async(url).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(Err(format!("the websocket did not open: {e}")));
                return;
            }
        };
        let flags = json!({"type": "feature_flags", "data": {}});
        if let Err(e) = socket.send(Message::text(flags.to_string())).await {
            let _ = tx.send(Err(format!("the feature_flags message was not sent: {e}")));
            return;
        }
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let next = tokio::time::timeout_at(deadline, socket.next()).await;
            let Ok(Some(Ok(message))) = next else {
                let _ = tx.send(Err(format!(
                    "the prompt {prompt_id} did not end on the websocket"
                )));
                return;
            };
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let ended = matches!(
                value["type"].as_str(),
                Some("execution_success" | "execution_error" | "execution_interrupted")
            ) && value["data"]["prompt_id"] == prompt_id;
            if tx.send(Ok(value)).is_err() || ended {
                let _ = socket.close(None).await;
                return;
            }
        }
    });
}

/// One HTTP/1.1 request on a fresh connection: `(status, JSON body or null)`.
fn answer(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), String> {
    let what = format!("{method} {path}");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("{what}: {e}"))?;
    stream
        .set_read_timeout(Some(WAIT))
        .map_err(|e| format!("{what}: {e}"))?;
    let body = body.map(Value::to_string).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("{what}: {e}"))?;
    let started = Instant::now();
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("{what}: {e} after {:?}", started.elapsed()))?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| format!("{what}: no complete answer"))?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut payload = raw[split + 4..].to_vec();
    let status = head
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("{what}: no status line"))?;
    if head
        .lines()
        .any(|l| l.to_ascii_lowercase().replace(' ', "") == "transfer-encoding:chunked")
    {
        payload = dechunk(&payload).ok_or_else(|| format!("{what}: a broken chunked body"))?;
    }
    Ok((
        status,
        serde_json::from_slice(&payload).unwrap_or(Value::Null),
    ))
}

/// A chunked HTTP body, joined.
fn dechunk(mut raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = raw.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&raw[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        raw = &raw[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(raw.get(..size)?);
        raw = raw.get(size + 2..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-067
    fn shapes_compare_keys_and_types_not_values() {
        let a = json!({"x": 1, "y": ["a", "b"], "z": {"k": null}});
        let b = json!({"x": 2, "y": ["c"], "z": {"k": null}});
        assert!(shape_drift("v", &shape(&a), &shape(&b)).is_empty());
        let c = json!({"x": "1", "y": [], "w": true});
        let drift = shape_drift("v", &shape(&a), &shape(&c));
        assert!(drift.iter().any(|d| d.starts_with("v.x:")), "{drift:?}");
        assert!(drift.iter().any(|d| d.starts_with("v.z:")), "{drift:?}");
        assert!(drift.iter().any(|d| d.starts_with("v.w:")), "{drift:?}");
        assert!(drift.iter().any(|d| d.starts_with("v.y[]")), "{drift:?}");
    }

    #[test] // spec: QA-067, QA-086
    fn scrubbing_keeps_keys_numbers_and_message_types_only() {
        let c = Capture {
            version: "0.42.1".into(),
            ws_messages: vec![json!({"type": "executed", "data": {
                "prompt_id": "p", "output": {"images": [{"filename": r"C:\x.png"}]}}})],
            api_jobs: json!({"bad_status": {"status": 400, "body": {"error": "bad"}}}),
            queue: json!({"queue_running": [], "queue_pending": []}),
            history: json!({PROMPT_ID_KEY: {"prompt": [1, "a prompt"]}}),
        };
        let s = c.scrubbed();
        let text = format!("{:?}", s);
        assert!(
            !text.contains(r"C:\x.png") && !text.contains("a prompt"),
            "{text}"
        );
        assert_eq!(s.ws_messages[0]["type"], "executed");
        assert_eq!(s.api_jobs["bad_status"]["status"], 400);
        assert_eq!(shape(&s.history), shape(&c.history));
    }

    #[test] // spec: QA-067
    fn the_newest_recorded_version_is_picked_by_number() {
        let dir = tempfile::tempdir().unwrap();
        for v in ["0.9.0", "0.10.2", "0.10.10", "notes"] {
            std::fs::create_dir(dir.path().join(v)).unwrap();
        }
        let (version, path) = newest_recorded(dir.path()).unwrap();
        assert_eq!(version, "0.10.10");
        assert_eq!(path, dir.path().join("0.10.10"));
        assert!(newest_recorded(&dir.path().join("none")).is_none());
    }

    #[test] // spec: QA-063
    fn the_probe_refuses_anything_but_loopback() {
        assert_eq!(loopback_port("http://127.0.0.1:8188"), Ok(8188));
        assert!(loopback_port("http://192.168.1.2:8188").is_err());
        assert!(loopback_port("https://127.0.0.1:8188").is_err());
    }
}
