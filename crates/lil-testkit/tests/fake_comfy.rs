//! The fake ComfyUI (QA-066): the queue, recorded requests, restarts, preview frames, outside
//! prompts, a dropped connection after accepting a prompt, and the `lil-fake-comfyui`
//! binary's child mode. Everything runs on `127.0.0.1`; waits poll the fake's own state, with
//! deadlines that only bound a failure (QA-031).

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use lil_testkit::{FakeComfy, free_loopback_port};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const DEADLINE: Duration = Duration::from_secs(30);

/// One HTTP/1.1 request over a fresh connection: `(status, JSON body or null)`.
fn http(port: u16, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let raw = raw_http(port, method, path, body);
    let text = String::from_utf8_lossy(&raw);
    let status = text
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("{method} {path}: no status line in {text:?}"));
    let body = text.split_once("\r\n\r\n").map_or(Value::Null, |(_, b)| {
        serde_json::from_str(b).unwrap_or(Value::Null)
    });
    (status, body)
}

/// The raw bytes a request got back (empty when the connection closed or reset without any).
fn raw_http(port: u16, method: &str, path: &str, body: Option<&Value>) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the fake");
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let body = body.map(Value::to_string).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    raw
}

fn get(fake: &FakeComfy, path: &str) -> Value {
    let (status, body) = http(fake.port(), "GET", path, None);
    assert_eq!(status, 200, "GET {path}: {body}");
    body
}

/// A canonical lowercase UUID.
fn uuid(n: u32) -> String {
    format!("{n:08x}-1111-4222-8333-444455556666")
}

/// `EmptyImage` into one `SaveImage` per prefix.
fn graph(prefixes: &[&str]) -> Value {
    let mut g = json!({"1": {"class_type": "EmptyImage",
        "inputs": {"width": 8, "height": 8, "batch_size": 1, "color": 0}}});
    for (i, prefix) in prefixes.iter().enumerate() {
        g[(i + 2).to_string()] = json!({"class_type": "SaveImage",
            "inputs": {"images": ["1", 0], "filename_prefix": prefix}});
    }
    g
}

fn submit(fake: &FakeComfy, id: &str, prefixes: &[&str]) {
    let body = json!({"prompt": graph(prefixes), "client_id": "suite", "prompt_id": id,
                      "extra_data": {"preview_method": "taesd"}});
    let (status, answer) = http(fake.port(), "POST", "/prompt", Some(&body));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["prompt_id"], id, "{answer}");
}

/// The ids in `/queue`: `(running, pending)`.
fn queue_ids(fake: &FakeComfy) -> (Vec<String>, Vec<String>) {
    let q = get(fake, "/queue");
    let ids = |key: &str| -> Vec<String> {
        q[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e[1].as_str().unwrap().to_string())
            .collect()
    };
    (ids("queue_running"), ids("queue_pending"))
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn output_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test] // spec: QA-066, CORE-123
fn fake_comfy_queues_one_at_a_time_and_records_requests() {
    let fake = FakeComfy::start();
    fake.hold_prompts(true);
    let (a, b) = (uuid(1), uuid(2));
    submit(&fake, &a, &["lil_a_"]);
    submit(&fake, &b, &["lil_b_"]);
    assert_eq!(queue_ids(&fake), (vec![a.clone()], vec![b.clone()]));
    assert_eq!(
        get(&fake, &format!("/api/jobs/{a}"))["status"],
        "in_progress"
    );
    assert_eq!(get(&fake, &format!("/api/jobs/{b}"))["status"], "pending");
    // Held: nothing was written yet.
    assert!(output_files(&fake.output_dir()).is_empty());

    fake.release(&a);
    wait_until("b runs", || queue_ids(&fake) == (vec![b.clone()], vec![]));
    let history = get(&fake, &format!("/history/{a}"));
    assert_eq!(history[&a]["status"]["status_str"], "success", "{history}");
    assert_eq!(
        history[&a]["outputs"]["2"]["images"][0]["filename"], "lil_a__00001_.png",
        "{history}"
    );
    assert_eq!(output_files(&fake.output_dir()), ["lil_a__00001_.png"]);
    fake.hold_prompts(false);
    wait_until("b is done", || queue_ids(&fake) == (vec![], vec![]));

    let prompts: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path == "/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0].body.as_ref().unwrap()["prompt_id"], a.as_str());
    assert_eq!(prompts[1].body.as_ref().unwrap()["prompt_id"], b.as_str());
    assert_eq!(
        prompts[0].body.as_ref().unwrap()["extra_data"]["preview_method"],
        "taesd"
    );
    assert!(
        fake.requests()
            .iter()
            .any(|r| r.method == "GET" && r.path == "/queue")
    );
    // An id that isn't a canonical lowercase UUID is refused, as ComfyUI 0.41.0 does.
    let bad = json!({"prompt": graph(&[]), "prompt_id": "NOT-A-UUID"});
    let (status, answer) = http(fake.port(), "POST", "/prompt", Some(&bad));
    assert_eq!(
        (status, &answer["error"]["type"]),
        (400, &json!("invalid_prompt_id"))
    );
}

#[test] // spec: QA-066, ARCH-021
fn fake_comfy_interrupt_is_an_error() {
    let fake = FakeComfy::start();
    let (status, _) = http(fake.port(), "POST", "/interrupt", Some(&json!({})));
    assert_eq!(status, 500);
    let last = fake.requests().pop().expect("the request was recorded");
    assert_eq!(
        (last.method.as_str(), last.path.as_str()),
        ("POST", "/interrupt")
    );
    // The per-job cancel is the way, and answers as ComfyUI does.
    fake.hold_prompts(true);
    let id = uuid(3);
    submit(&fake, &id, &["lil_c_"]);
    let (status, answer) = http(fake.port(), "POST", &format!("/api/jobs/{id}/cancel"), None);
    assert_eq!((status, answer), (200, json!({"cancelled": true})));
    wait_until("the prompt is cancelled", || {
        get(&fake, &format!("/api/jobs/{id}"))["status"] == "cancelled"
    });
    assert!(output_files(&fake.output_dir()).is_empty());
}

#[test] // spec: QA-066, QA-009
fn fake_comfy_restart_forgets_prompts_and_keeps_outputs() {
    let fake = FakeComfy::start();
    // Armed: the restart comes after one of the prompt's two outputs.
    fake.restart(1);
    let id = uuid(4);
    submit(&fake, &id, &["lil_r_", "lil_r_"]);
    wait_until("the prompt is forgotten", || {
        http(fake.port(), "GET", &format!("/api/jobs/{id}"), None).0 == 404
    });
    assert_eq!(output_files(&fake.output_dir()), ["lil_r__00001_.png"]);
    assert_eq!(get(&fake, "/history"), json!({}));
    assert_eq!(queue_ids(&fake), (vec![], vec![]));

    // A later prompt runs as usual; an immediate restart forgets it and keeps its files.
    let next = uuid(5);
    submit(&fake, &next, &["lil_r_"]);
    wait_until("the next prompt is done", || {
        get(&fake, &format!("/api/jobs/{next}"))["status"] == "completed"
    });
    fake.restart(0);
    assert_eq!(get(&fake, &format!("/history/{next}")), json!({}));
    assert_eq!(
        output_files(&fake.output_dir()),
        ["lil_r__00001_.png", "lil_r__00002_.png"]
    );
}

/// A websocket client of the fake, on its own runtime.
struct Ws {
    rt: tokio::runtime::Runtime,
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Ws {
    fn connect(fake: &FakeComfy, client_id: &str) -> Ws {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let url = format!("ws://127.0.0.1:{}/ws?clientId={client_id}", fake.port());
        let (stream, _) = rt
            .block_on(tokio_tungstenite::connect_async(url))
            .expect("the websocket handshake");
        Ws { rt, stream }
    }

    fn send_text(&mut self, text: Value) {
        let Ws { rt, stream } = self;
        rt.block_on(stream.send(Message::text(text.to_string())))
            .unwrap();
    }

    /// The next binary frame; text frames are skipped.
    fn next_binary(&mut self) -> Vec<u8> {
        let Ws { rt, stream } = self;
        rt.block_on(async {
            loop {
                let message = tokio::time::timeout(DEADLINE, stream.next())
                    .await
                    .expect("a frame within the deadline")
                    .expect("the connection is open")
                    .expect("a frame");
                if let Message::Binary(bytes) = message {
                    return bytes.to_vec();
                }
            }
        })
    }
}

fn be_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[test] // spec: QA-066, CORE-130
fn fake_comfy_sends_preview_frames_with_the_header() {
    let fake = FakeComfy::start();
    let mut plain = Ws::connect(&fake, "suite");
    fake.send_preview(1, 16);
    let frame = plain.next_binary();
    assert_eq!(frame.len(), 24);
    assert_eq!(
        (be_u32(&frame, 0), be_u32(&frame, 4)),
        (1, 1),
        "event 1, JPEG"
    );

    // A client that announced `supports_preview_metadata` gets event 4 instead, as ComfyUI
    // 0.41.0 does (the core never announces it).
    let mut announced = Ws::connect(&fake, "other");
    announced.send_text(json!({"type": "feature_flags",
                               "data": {"supports_preview_metadata": true}}));
    let deadline = Instant::now() + DEADLINE;
    let frame = loop {
        fake.send_preview(2, 4);
        let frame = announced.next_binary();
        if be_u32(&frame, 0) == 4 {
            break frame;
        }
        assert!(
            Instant::now() < deadline,
            "the feature flag never took effect"
        );
    };
    let meta_len = be_u32(&frame, 4) as usize;
    let meta: Value = serde_json::from_slice(&frame[8..8 + meta_len]).unwrap();
    assert_eq!(meta["image_type"], "image/png");
    assert_eq!(frame.len(), 8 + meta_len + 4);
    // The plain client still gets event 1.
    assert_eq!(be_u32(&plain.next_binary(), 0), 1);
}

#[test] // spec: QA-066, ARCH-033
fn fake_comfy_injects_preview_frames_of_a_given_size() {
    let fake = FakeComfy::start();
    let mut ws = Ws::connect(&fake, "suite");
    fake.send_preview(2, 1_048_577);
    let frame = ws.next_binary();
    assert_eq!(frame.len(), 1_048_585);
    assert_eq!(be_u32(&frame, 0), 1, "event type 1");
    assert_eq!(be_u32(&frame, 4), 2, "image type 2, PNG");
}

#[test] // spec: QA-066, ARCH-069
fn fake_comfy_outside_prompts_carry_their_graph() {
    let fake = FakeComfy::start();
    let g = json!({"7": {"class_type": "PartnerImageNode", "inputs": {"prompt": "x"}}});
    let id = fake.add_outside_prompt(true, Some(g.clone()));
    let queue = get(&fake, "/queue");
    let running = &queue["queue_running"][0];
    assert_eq!((&running[1], &running[2]), (&json!(id), &g), "{queue}");
    // As in ComfyUI 0.41.0, a running job's detail has no graph yet: `/queue` holds it.
    let job = get(&fake, &format!("/api/jobs/{id}"));
    assert_eq!(job["status"], "in_progress", "{job}");
    assert!(job.get("workflow").is_none(), "{job}");
    let listed = get(&fake, "/api/jobs");
    assert!(
        listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == id.as_str()),
        "{listed}"
    );

    // A queued outside prompt waits for the running one; outside prompts end only when
    // released.
    let queued = fake.add_outside_prompt(false, None);
    assert_eq!(queue_ids(&fake), (vec![id.clone()], vec![queued.clone()]));
    fake.release(&id);
    wait_until("the queued one runs", || {
        queue_ids(&fake) == (vec![queued.clone()], vec![])
    });
    // Once finished, the detail carries the graph, the outputs and the execution status.
    let done = get(&fake, &format!("/api/jobs/{id}"));
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["workflow"]["prompt"], g, "{done}");
    assert_eq!(done["outputs"], json!({}), "{done}");
    assert_eq!(done["execution_status"]["status_str"], "success", "{done}");
}

#[test] // spec: QA-066, ARCH-021
fn fake_comfy_cancelled_pending_prompt_is_forgotten() {
    let fake = FakeComfy::start();
    fake.hold_prompts(true);
    let (a, b) = (uuid(7), uuid(8));
    submit(&fake, &a, &["lil_p_"]);
    submit(&fake, &b, &["lil_q_"]);
    let (status, answer) = http(fake.port(), "POST", &format!("/api/jobs/{b}/cancel"), None);
    assert_eq!((status, answer), (200, json!({"cancelled": true})));
    // Dequeued and forgotten, as ComfyUI deletes it from its queue.
    assert_eq!(queue_ids(&fake), (vec![a.clone()], vec![]));
    assert_eq!(
        http(fake.port(), "GET", &format!("/api/jobs/{b}"), None).0,
        404
    );
    fake.release(&a);
    wait_until("a is done", || {
        get(&fake, &format!("/api/jobs/{a}"))["status"] == "completed"
    });
    let history = get(&fake, "/history");
    assert!(
        history.get(&a).is_some() && history.get(&b).is_none(),
        "{history}"
    );
}

#[test] // spec: QA-066, CORE-120
fn fake_comfy_drop_after_accept_queues_then_closes() {
    let fake = FakeComfy::start();
    fake.drop_after_accept(true);
    let id = uuid(6);
    let body = json!({"prompt": graph(&["lil_d_"]), "client_id": "suite", "prompt_id": id});
    let raw = raw_http(fake.port(), "POST", "/prompt", Some(&body));
    assert!(
        !raw.starts_with(b"HTTP/"),
        "a status line came back: {:?}",
        String::from_utf8_lossy(&raw)
    );
    fake.drop_after_accept(false);
    let job = get(&fake, &format!("/api/jobs/{id}"));
    assert_eq!(job["id"], id.as_str(), "{job}");
}

/// Kills the processes it names on drop.
struct Reap(Vec<u32>);

impl Drop for Reap {
    fn drop(&mut self) {
        for pid in &self.0 {
            if let Ok(p) = lil_platform::process::open(*pid) {
                let _ = p.kill();
            }
        }
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[test] // spec: QA-066, ARCH-064
fn fake_comfyui_child_mode_listens_from_the_child() {
    let root = tempfile::Builder::new()
        .prefix("lil-fake-comfyui-")
        .tempdir()
        .unwrap();
    let port = free_loopback_port();
    let mut launcher: Child = Command::new(env!("CARGO_BIN_EXE_lil-fake-comfyui"))
        .args(["main.py", "--listen", "127.0.0.1", "--port"])
        .arg(port.to_string())
        .arg("--child")
        .current_dir(root.path())
        .spawn()
        .expect("start lil-fake-comfyui");
    let mut reap = Reap(vec![launcher.id()]);

    let child_file = root.path().join("fake-comfyui-child.json");
    wait_until("the child listens", || read_json(&child_file).is_some());
    let child = read_json(&child_file).unwrap();
    let child_pid = u32::try_from(child["pid"].as_u64().unwrap()).unwrap();
    reap.0.push(child_pid);
    let launched = read_json(&root.path().join("fake-comfyui.json")).expect("the launcher's file");

    assert_eq!(launched["pid"], launcher.id());
    assert_ne!(child_pid, launcher.id());
    let args = |doc: &Value| doc["argv"].as_array().unwrap()[1..].to_vec();
    assert_eq!(args(&child), args(&launched), "the same arguments");
    assert_eq!(
        (&child["listen"], &child["port"]),
        (&json!("127.0.0.1"), &json!(port))
    );
    let (status, stats) = http(port, "GET", "/system_stats", None);
    assert_eq!(status, 200, "{stats}");
    assert_eq!(
        lil_platform::sockets::pid_listening_on(port).unwrap(),
        Some(child_pid),
        "the child, not the launcher, listens"
    );
    // The launcher waits for its child.
    assert!(launcher.try_wait().unwrap().is_none());
    drop(reap);
    let _ = launcher.wait();
}

/// Runs `lil-fake-comfyui main.py <args>` in `dir`.
fn launch(dir: &Path, args: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_lil-fake-comfyui"))
        .arg("main.py")
        .args(args)
        .current_dir(dir)
        .spawn()
        .expect("start lil-fake-comfyui")
}

fn exit_code(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        assert!(Instant::now() < deadline, "lil-fake-comfyui still runs");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The fake listens on 127.0.0.1 only (P3-R11). `--listen` is read as ComfyUI reads it (the
/// last one wins; a bare one means all interfaces) and anything but loopback is refused. With
/// `fake-child-any` the child still binds loopback and scripts its row as `0.0.0.0` instead.
#[test] // spec: QA-066, ARCH-068
fn fake_comfyui_listens_on_loopback_only() {
    let root = tempfile::Builder::new()
        .prefix("lil-fake-comfyui-")
        .tempdir()
        .unwrap();
    let port = free_loopback_port().to_string();
    for args in [
        vec!["--listen", "0.0.0.0", "--port", &port],
        vec![
            "--listen",
            "127.0.0.1",
            "--listen",
            "0.0.0.0",
            "--port",
            &port,
        ],
        vec!["--listen", "127.0.0.1,0.0.0.0", "--port", &port],
        vec!["--port", &port, "--listen"],
    ] {
        let mut fake = launch(root.path(), &args);
        assert_eq!(exit_code(&mut fake), Some(2), "{args:?}");
    }
    assert!(read_json(&root.path().join("fake-comfyui.json")).is_none());

    std::fs::write(root.path().join("fake-child-any"), "").unwrap();
    let args = [
        "--listen",
        "0.0.0.0",
        "--listen",
        "127.0.0.1",
        "--port",
        &port,
        "--child",
    ];
    let mut launcher = launch(root.path(), &args);
    let mut reap = Reap(vec![launcher.id()]);
    let child_file = root.path().join("fake-comfyui-child.json");
    wait_until("the child listens", || read_json(&child_file).is_some());
    let child_pid =
        u32::try_from(read_json(&child_file).unwrap()["pid"].as_u64().unwrap()).unwrap();
    reap.0.push(child_pid);

    let real = lil_platform::sockets::listening_sockets(child_pid).unwrap();
    assert!(
        !real.is_empty() && real.iter().all(|a| a.ip().is_loopback()),
        "{real:?}"
    );
    let rows = std::fs::read_to_string(root.path().join("fake-sockets.json")).unwrap();
    let rows: Vec<Value> = rows
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rows,
        [json!({"pid": child_pid, "port": port.parse::<u16>().unwrap(), "as": "0.0.0.0"})]
    );
    drop(reap);
    let _ = launcher.wait();
}
