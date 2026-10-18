//! `FakeComfy`: an in-process ComfyUI for tests (QA-066), HTTP and websocket on
//! `127.0.0.1:<free port>`. The `lil-fake-comfyui` binary serves the same fake for tests that
//! need a separate process.
//!
//! It answers the endpoints the core uses, in ComfyUI 0.41.0's shapes as far as the core reads
//! them (spec Appendix B.3): `GET /system_stats`, `POST /prompt`, `GET /queue`, `GET /api/jobs`,
//! `GET /api/jobs/{id}`, `POST /api/jobs/{id}/cancel`, `GET /history[/{id}]`, `GET /view`,
//! `POST /upload/image`, `POST /free`, `GET /object_info[/{class}]`, `GET /node_replacements`
//! and the websocket `/ws?clientId=<id>`. Every route also answers under `/api` (except the
//! jobs routes, which live there), as ComfyUI's do. `POST /interrupt` answers 500, so a suite
//! that posts it fails visibly (ARCH-021). Every request is recorded ([`FakeComfy::requests`]).
//!
//! **Execution.** One prompt runs at a time, in the order accepted. Which prompt runs is decided
//! when a prompt is accepted or one ends, so `/queue` is exact as soon as a call returns. A run
//! sends `execution_start`, `execution_cached`, `executing` and `progress` for each node that
//! saves nothing, then (unless it is held, [`FakeComfy::hold_prompts`]) writes one PNG per
//! `Save*` node with a `filename_prefix` input into [`FakeComfy::output_dir`] as ComfyUI names
//! them (`<prefix>_00001_.png`), sending `executed` for each, then `execution_success` and
//! `executing` with `node: null`. Events go to the websocket clients whose `clientId` is the
//! prompt's `client_id` (every client when it has none); `status` goes to every client.
//!
//! Prompts are never real work: no time passes in the fake except what the scheduling of its
//! own task takes.

use std::convert::Infallible;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::path::{Component, Path, PathBuf};
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt as _, StreamExt as _};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

/// A failure [`FakeComfy::fail_next`] injects into the next prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeFailure {
    /// The prompt runs and ends with an `execution_error` whose `exception_type` is
    /// `torch.OutOfMemoryError` and whose message contains `CUDA out of memory`.
    Oom,
    /// The prompt runs and ends with an `execution_error` with this message.
    Execution { message: String },
    /// `POST /prompt` answers 503 and queues nothing.
    Refuse503,
}

/// One HTTP request the fake received.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeRequest {
    pub method: String,
    /// The path as sent, without the query.
    pub path: String,
    /// The query string, without `?` (empty when there is none).
    pub query: String,
    /// The body, when it is JSON.
    pub body: Option<Value>,
}

/// The fake ComfyUI. Dropping it stops the server; its temporary root (from
/// [`FakeComfy::start`]) is removed.
pub struct FakeComfy {
    shared: Arc<Shared>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    _root: Option<tempfile::TempDir>,
}

impl FakeComfy {
    /// Serves on `127.0.0.1` with a port the system picked, rooted in a new temporary folder
    /// holding `output\` and `input\`.
    ///
    /// # Panics
    /// If the port or the folder cannot be had.
    pub fn start() -> FakeComfy {
        let root = tempfile::Builder::new()
            .prefix("lil-fake-comfy-")
            .tempdir()
            .expect("cannot create the fake ComfyUI's folder");
        let mut fake = FakeComfy::serve(crate::ports::loopback_listener(), root.path());
        fake._root = Some(root);
        fake
    }

    /// Serves on `listener`, with ComfyUI's folder `root` (outputs go to `<root>\output`,
    /// uploads to `<root>\input`, both created). For `lil-fake-comfyui`.
    ///
    /// # Panics
    /// If the folders cannot be created or the server thread cannot start.
    pub fn serve(listener: TcpListener, root: &Path) -> FakeComfy {
        for sub in ["output", "input"] {
            std::fs::create_dir_all(root.join(sub))
                .unwrap_or_else(|e| panic!("cannot create {}: {e}", root.join(sub).display()));
        }
        let addr = listener.local_addr().expect("the listener has an address");
        listener
            .set_nonblocking(true)
            .expect("the listener can be made non-blocking");
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                object_info: default_object_info(),
                next_number: 1,
                ..State::default()
            }),
            wake: Notify::new(),
            root: root.to_path_buf(),
            addr,
        });
        let (stop, stopped) = oneshot::channel::<()>();
        let in_thread = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("fake-comfyui".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the fake ComfyUI's runtime");
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("the fake ComfyUI's listener");
                    tokio::spawn(executor(Arc::clone(&in_thread)));
                    tokio::spawn(accept_loop(listener, in_thread));
                    let _ = stopped.await;
                });
                // Dropping the runtime here ends every connection and task.
            })
            .expect("cannot start the fake ComfyUI's thread");
        FakeComfy {
            shared,
            stop: Some(stop),
            thread: Some(thread),
            _root: None,
        }
    }

    pub fn port(&self) -> u16 {
        self.shared.addr.port()
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    /// ComfyUI's folder: `output\` and `input\` are in it.
    pub fn root(&self) -> PathBuf {
        self.shared.root.clone()
    }

    /// `<root>\output`, where prompts write their images.
    pub fn output_dir(&self) -> PathBuf {
        self.shared.root.join("output")
    }

    /// While on, every prompt the suite submits stays running (after its `progress` events,
    /// before any output) until [`FakeComfy::release`]. Turning it off releases them all.
    pub fn hold_prompts(&self, on: bool) {
        let mut st = self.shared.lock();
        st.hold = on;
        for p in st
            .prompts
            .iter_mut()
            .filter(|p| !p.outside && !p.finished())
        {
            p.held = on;
        }
        drop(st);
        self.shared.wake.notify_one();
    }

    /// Lets a held prompt (or an outside one) finish. Unknown ids are ignored.
    pub fn release(&self, prompt_id: &str) {
        if let Some(p) = self.shared.lock().prompt_mut(prompt_id) {
            p.held = false;
        }
        self.shared.wake.notify_one();
    }

    /// A prompt the suite didn't submit (client id `outside`), held until
    /// [`FakeComfy::release`]: running at once when `running` is true, else queued behind the
    /// running prompt (and so running at once when there is none, as in ComfyUI). Its graph
    /// (default: a one-node `EmptyImage` graph) appears in `/queue` and in `/api/jobs/{id}`'s
    /// `workflow.prompt`. Returns its prompt id.
    ///
    /// # Panics
    /// If `running` is true while another prompt runs (ComfyUI runs one at a time).
    pub fn add_outside_prompt(&self, running: bool, graph: Option<Value>) -> String {
        let graph = graph.unwrap_or_else(|| {
            json!({"1": {"class_type": "EmptyImage",
                         "inputs": {"width": 64, "height": 64, "batch_size": 1, "color": 0}}})
        });
        let mut st = self.shared.lock();
        let id = st.mint_id();
        let mut prompt = Prompt::new(
            &mut st,
            id.clone(),
            graph,
            Some("outside".into()),
            json!({}),
        );
        prompt.outside = true;
        prompt.held = true;
        st.prompts.push(prompt);
        if running {
            assert!(
                st.running.is_none(),
                "add_outside_prompt(true, ..) while another prompt runs"
            );
            st.running = Some(id.clone());
        } else if st.running.is_none() {
            st.running = Some(id.clone());
        } else {
            st.queue.push(id.clone());
        }
        st.broadcast_status();
        drop(st);
        self.shared.wake.notify_one();
        id
    }

    /// The next prompt the suite submits fails as `kind` says ([`FakeFailure`]).
    pub fn fail_next(&self, kind: FakeFailure) {
        self.shared.lock().fail_next = Some(kind);
    }

    /// While on, each `POST /prompt` is queued as usual, then the connection is closed before
    /// any response is written (no status line).
    pub fn drop_after_accept(&self, on: bool) {
        self.shared.lock().drop_after_accept = on;
    }

    /// Sends one binary frame of `payload_len` bytes of image data to every websocket client:
    /// event type 1 (4 bytes, big-endian), the image type (4 bytes: 1 JPEG, 2 PNG), then the
    /// payload, to a client that hasn't announced `supports_preview_metadata`; event type 4
    /// (4-byte metadata length, JSON metadata, then the payload) to one that has, as ComfyUI
    /// 0.41.0 does.
    pub fn send_preview(&self, image_type: u32, payload_len: usize) {
        let payload: Vec<u8> = (0..payload_len).map(|i| (i % 251) as u8).collect();
        let mime = if image_type == 2 {
            "image/png"
        } else {
            "image/jpeg"
        };
        let metadata = json!({"image_type": mime}).to_string();
        let st = self.shared.lock();
        for client in &st.clients {
            let mut frame = Vec::with_capacity(payload.len() + 8 + metadata.len());
            if client.preview_metadata.load(Ordering::SeqCst) {
                frame.extend_from_slice(&4_u32.to_be_bytes());
                frame.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
                frame.extend_from_slice(metadata.as_bytes());
            } else {
                frame.extend_from_slice(&1_u32.to_be_bytes());
                frame.extend_from_slice(&image_type.to_be_bytes());
            }
            frame.extend_from_slice(&payload);
            let _ = client.tx.send(Message::binary(frame));
        }
    }

    /// Restarts the fake: forgets every prompt (queued, running and finished) and drops every
    /// websocket client; files already written stay. With `after_outputs` 0 it happens now;
    /// otherwise once that many more outputs have been written (by a prompt that is running
    /// or starts later), before that prompt ends.
    pub fn restart(&self, after_outputs: u32) {
        let mut st = self.shared.lock();
        if after_outputs == 0 {
            st.restart_now();
        } else {
            st.restart_after = Some(after_outputs);
            st.outputs_since_armed = 0;
        }
        drop(st);
        self.shared.wake.notify_one();
    }

    /// Every request so far, in arrival order.
    pub fn requests(&self) -> Vec<FakeRequest> {
        self.shared.lock().requests.clone()
    }

    /// What `GET /object_info` answers (default: `EmptyImage` and `SaveImage`).
    pub fn set_object_info(&self, v: Value) {
        self.shared.lock().object_info = v;
    }
}

impl Drop for FakeComfy {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the server thread and the [`FakeComfy`] handle share.
struct Shared {
    state: Mutex<State>,
    /// Wakes the executor: a prompt arrived, was released, cancelled or forgotten.
    wake: Notify,
    root: PathBuf,
    addr: SocketAddr,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl Phase {
    /// The `/api/jobs` status.
    fn as_str(self) -> &'static str {
        match self {
            Phase::Pending => "pending",
            Phase::Running => "in_progress",
            Phase::Completed => "completed",
            Phase::Failed => "failed",
            Phase::Cancelled => "cancelled",
        }
    }
}

struct Prompt {
    id: String,
    number: u64,
    graph: Value,
    extra_data: Value,
    client_id: Option<String>,
    phase: Phase,
    /// The executor has sent its `execution_start`.
    started: bool,
    held: bool,
    outside: bool,
    cancel: bool,
    fail: Option<FakeFailure>,
    /// Node id → `{images: [{filename, subfolder, type}]}`.
    outputs: Map<String, Value>,
    /// `[event, data]` pairs, as `/history` reports them.
    messages: Vec<Value>,
    create_time: u64,
    start_time: Option<u64>,
    end_time: Option<u64>,
    error: Option<Value>,
}

impl Prompt {
    fn new(
        st: &mut State,
        id: String,
        graph: Value,
        client_id: Option<String>,
        extra_data: Value,
    ) -> Prompt {
        let number = st.next_number;
        st.next_number += 1;
        let create_time = now_ms();
        let mut extra = match extra_data {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        extra.insert("client_id".into(), json!(client_id));
        extra.insert("create_time".into(), json!(create_time));
        Prompt {
            id,
            number,
            graph,
            extra_data: Value::Object(extra),
            client_id,
            phase: Phase::Pending,
            started: false,
            held: false,
            outside: false,
            cancel: false,
            fail: None,
            outputs: Map::new(),
            messages: Vec::new(),
            create_time,
            start_time: None,
            end_time: None,
            error: None,
        }
    }

    fn finished(&self) -> bool {
        matches!(
            self.phase,
            Phase::Completed | Phase::Failed | Phase::Cancelled
        )
    }

    /// `[number, prompt_id, prompt, extra_data, outputs_to_execute]`, as `/queue` and
    /// `/history` hold it.
    fn queue_entry(&self) -> Value {
        let outputs: Vec<String> = save_nodes(&self.graph)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        json!([self.number, self.id, self.graph, self.extra_data, outputs])
    }

    fn outputs_count(&self) -> usize {
        self.outputs
            .values()
            .map(|o| o["images"].as_array().map_or(0, Vec::len))
            .sum()
    }

    fn job(&self, detail: bool) -> Value {
        let mut job = json!({
            "id": self.id,
            "status": self.phase.as_str(),
            "priority": self.number,
            "create_time": self.create_time,
            "execution_start_time": self.start_time,
            "execution_end_time": self.end_time,
            "outputs_count": self.outputs_count(),
            "workflow_id": Value::Null,
        });
        if let Some(error) = &self.error {
            job["execution_error"] = error.clone();
        }
        if detail {
            job["workflow"] = json!({"prompt": self.graph, "extra_data": self.extra_data});
            job["outputs"] = Value::Object(self.outputs.clone());
        }
        job
    }

    fn history(&self) -> Value {
        json!({
            "prompt": self.queue_entry(),
            "outputs": self.outputs,
            "status": {
                "status_str": if self.phase == Phase::Completed { "success" } else { "error" },
                "completed": self.phase == Phase::Completed,
                "messages": self.messages,
            },
            "meta": {},
        })
    }
}

struct Client {
    serial: u64,
    client_id: String,
    tx: mpsc::UnboundedSender<Message>,
    preview_metadata: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    /// Every prompt this run of the fake knows, in the order accepted.
    prompts: Vec<Prompt>,
    /// Pending prompts after the running one, in order.
    queue: Vec<String>,
    /// Never `None` while `queue` holds anything.
    running: Option<String>,
    hold: bool,
    fail_next: Option<FakeFailure>,
    drop_after_accept: bool,
    restart_after: Option<u32>,
    outputs_since_armed: u32,
    /// Bumped by each restart; a run started before it stops.
    generation: u64,
    requests: Vec<FakeRequest>,
    object_info: Value,
    clients: Vec<Client>,
    next_number: u64,
    next_id: u64,
    next_serial: u64,
}

/// What the executor needs of the prompt it runs.
struct Job {
    id: String,
    graph: Value,
    generation: u64,
}

impl State {
    fn prompt_mut(&mut self, id: &str) -> Option<&mut Prompt> {
        self.prompts.iter_mut().find(|p| p.id == id)
    }

    fn prompt(&self, id: &str) -> Option<&Prompt> {
        self.prompts.iter().find(|p| p.id == id)
    }

    /// A canonical lowercase UUID no prompt of this fake has.
    fn mint_id(&mut self) -> String {
        self.next_id += 1;
        format!("00000000-0000-4000-8000-{:012x}", self.next_id)
    }

    /// Sends `{type, data}` to the clients with `client_id` `to`, or to every client.
    fn send(&self, to: Option<&str>, kind: &str, data: Value) {
        let text = json!({"type": kind, "data": data}).to_string();
        for client in &self.clients {
            if to.is_none_or(|id| id == client.client_id) {
                let _ = client.tx.send(Message::text(text.clone()));
            }
        }
    }

    fn queue_remaining(&self) -> usize {
        self.queue.len() + usize::from(self.running.is_some())
    }

    fn broadcast_status(&self) {
        let data = json!({"status": {"exec_info": {"queue_remaining": self.queue_remaining()}}});
        self.send(None, "status", data);
    }

    /// Sends `kind` to the prompt's client and keeps it in the prompt's history messages.
    fn event(&mut self, id: &str, kind: &str, data: Value) {
        let to = self.prompt(id).and_then(|p| p.client_id.clone());
        self.send(to.as_deref(), kind, data.clone());
        if let Some(p) = self.prompt_mut(id) {
            p.messages.push(json!([kind, data]));
        }
    }

    /// The running prompt, if the executor has not started it yet: starts it.
    fn next_job(&mut self) -> Option<Job> {
        let id = self.running.clone()?;
        let generation = self.generation;
        let p = self.prompt_mut(&id)?;
        if p.started {
            return None;
        }
        p.started = true;
        p.phase = Phase::Running;
        p.start_time = Some(now_ms());
        let job = Job {
            id: id.clone(),
            graph: p.graph.clone(),
            generation,
        };
        let ts = now_ms();
        self.event(
            &id,
            "execution_start",
            json!({"prompt_id": id, "timestamp": ts}),
        );
        self.event(
            &id,
            "execution_cached",
            json!({"nodes": [], "prompt_id": id, "timestamp": ts}),
        );
        let saving: Vec<String> = save_nodes(&job.graph).into_iter().map(|(n, _)| n).collect();
        for node in node_ids(&job.graph) {
            if saving.contains(&node) {
                continue;
            }
            self.event(
                &id,
                "executing",
                json!({"node": node, "display_node": node, "prompt_id": id}),
            );
            self.event(
                &id,
                "progress",
                json!({"value": 1, "max": 1, "prompt_id": id, "node": node}),
            );
        }
        Some(job)
    }

    /// Ends the running prompt `id` in `phase` and starts the next one.
    fn finish(&mut self, id: &str, phase: Phase) {
        if let Some(p) = self.prompt_mut(id) {
            p.phase = phase;
            p.end_time = Some(now_ms());
        }
        self.event(
            id,
            "executing",
            json!({"node": Value::Null, "display_node": Value::Null, "prompt_id": id}),
        );
        if self.running.as_deref() == Some(id) {
            self.running = if self.queue.is_empty() {
                None
            } else {
                Some(self.queue.remove(0))
            };
        }
        self.broadcast_status();
    }

    fn fail(&mut self, id: &str, failure: &FakeFailure, node: (String, String)) {
        let (exception_type, message) = match failure {
            FakeFailure::Oom => (
                "torch.OutOfMemoryError",
                "Allocation on device 0 would exceed allowed memory. CUDA out of memory. Tried to allocate 2.00 GiB".to_string(),
            ),
            FakeFailure::Execution { message } => ("RuntimeError", message.clone()),
            FakeFailure::Refuse503 => ("RuntimeError", "refused".to_string()),
        };
        let data = json!({
            "prompt_id": id, "node_id": node.0, "node_type": node.1, "executed": [],
            "exception_message": message, "exception_type": exception_type, "traceback": [],
            "current_inputs": {}, "current_outputs": {}, "timestamp": now_ms(),
        });
        if let Some(p) = self.prompt_mut(id) {
            p.error = Some(data.clone());
        }
        self.event(id, "execution_error", data);
        self.finish(id, Phase::Failed);
    }

    fn interrupt(&mut self, id: &str, node: (String, String)) {
        let data = json!({"prompt_id": id, "node_id": node.0, "node_type": node.1,
                          "executed": [], "timestamp": now_ms()});
        self.event(id, "execution_interrupted", data);
        self.finish(id, Phase::Cancelled);
    }

    fn restart_now(&mut self) {
        self.generation += 1;
        self.prompts.clear();
        self.queue.clear();
        self.running = None;
        // Dropping the senders ends every websocket connection.
        self.clients.clear();
        self.restart_after = None;
        self.outputs_since_armed = 0;
    }
}

/// Runs the prompts, one at a time.
async fn executor(shared: Arc<Shared>) {
    loop {
        let job = shared.lock().next_job();
        match job {
            Some(job) => run(&shared, job).await,
            None => shared.wake.notified().await,
        }
    }
}

/// The node a failure or an interruption is reported on: the first one.
fn first_node(graph: &Value) -> (String, String) {
    let id = node_ids(graph).into_iter().next().unwrap_or_default();
    let class = graph[&id]["class_type"].as_str().unwrap_or("").to_string();
    (id, class)
}

/// Whether the run of `job` may go on: `false` when the fake restarted or the prompt was
/// cancelled (then ended here).
fn still_running(st: &mut State, job: &Job) -> bool {
    if st.generation != job.generation {
        return false;
    }
    match st.prompt(&job.id) {
        None => false,
        Some(p) if p.cancel => {
            st.interrupt(&job.id, first_node(&job.graph));
            false
        }
        Some(_) => true,
    }
}

async fn run(shared: &Shared, job: Job) {
    // Held prompts wait here until released (or cancelled, or forgotten).
    loop {
        {
            let mut st = shared.lock();
            if !still_running(&mut st, &job) {
                return;
            }
            if st.prompt(&job.id).is_some_and(|p| !p.held) {
                break;
            }
        }
        shared.wake.notified().await;
    }
    {
        let mut st = shared.lock();
        let failure = st.prompt_mut(&job.id).and_then(|p| p.fail.take());
        if let Some(failure) = failure {
            st.fail(&job.id, &failure, first_node(&job.graph));
            return;
        }
    }
    for (node, prefix) in save_nodes(&job.graph) {
        if !still_running(&mut shared.lock(), &job) {
            return;
        }
        let (subfolder, file) = write_output(&shared.root.join("output"), &prefix);
        // In a block, so the guard is gone before the await.
        let go_on = {
            let mut st = shared.lock();
            if st.generation != job.generation {
                return;
            }
            let image = json!({"filename": file, "subfolder": subfolder, "type": "output"});
            if let Some(p) = st.prompt_mut(&job.id) {
                p.outputs
                    .insert(node.clone(), json!({"images": [image.clone()]}));
            }
            let data = json!({"node": node, "display_node": node,
                              "output": {"images": [image]}, "prompt_id": job.id});
            st.event(&job.id, "executed", data);
            match st.restart_after {
                Some(after) => {
                    st.outputs_since_armed += 1;
                    let restart = st.outputs_since_armed >= after;
                    if restart {
                        st.restart_now();
                    }
                    !restart
                }
                None => true,
            }
        };
        if !go_on {
            return;
        }
        tokio::task::yield_now().await;
    }
    let mut st = shared.lock();
    if !still_running(&mut st, &job) {
        return;
    }
    let data = json!({"prompt_id": job.id, "timestamp": now_ms()});
    st.event(&job.id, "execution_success", data);
    st.finish(&job.id, Phase::Completed);
}

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Writes the next `<prefix>_<n:05>_.png` under `output` (a `/` in the prefix names a
/// subfolder); returns `(subfolder, file name)`.
fn write_output(output: &Path, prefix: &str) -> (String, String) {
    let (subfolder, base) = prefix.rsplit_once('/').unwrap_or(("", prefix));
    let dir = output.join(subfolder);
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
    let start = format!("{base}_");
    let taken = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    name.strip_prefix(&start)?
                        .strip_suffix("_.png")?
                        .parse::<u32>()
                        .ok()
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let file = format!("{base}_{:05}_.png", taken + 1);
    std::fs::write(dir.join(&file), PNG)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", dir.join(&file).display()));
    (subfolder.to_string(), file)
}

/// The graph's node ids, numeric ones in numeric order.
fn node_ids(graph: &Value) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    ids.sort_by_key(|id| (id.parse::<u64>().unwrap_or(u64::MAX), id.clone()));
    ids
}

/// `(node id, filename_prefix)` of each `Save*` node with a string `filename_prefix`.
fn save_nodes(graph: &Value) -> Vec<(String, String)> {
    node_ids(graph)
        .into_iter()
        .filter_map(|id| {
            let node = &graph[&id];
            let class = node["class_type"].as_str()?;
            let prefix = node["inputs"]["filename_prefix"].as_str()?;
            class
                .starts_with("Save")
                .then(|| (id.clone(), prefix.to_string()))
        })
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A whole response body.
struct Full(Option<Bytes>);

impl Full {
    fn new(bytes: impl Into<Bytes>) -> Full {
        Full(Some(bytes.into()))
    }
}

impl Body for Full {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(self.get_mut().0.take().map(|b| Ok(Frame::data(b))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.0.as_ref().map_or(0, |b| b.len() as u64))
    }
}

fn respond(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Response<Full> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(body))
        .expect("a valid response")
}

fn json_response(status: StatusCode, v: &Value) -> Response<Full> {
    respond(status, "application/json", v.to_string())
}

async fn accept_loop(listener: tokio::net::TcpListener, shared: Arc<Shared>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let service = service_fn(move |req| handle(Arc::clone(&shared), req));
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}

async fn read_body(body: &mut Incoming) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await {
        let Ok(frame) = frame else { break };
        if let Ok(data) = frame.into_data() {
            bytes.extend_from_slice(&data);
        }
    }
    bytes
}

/// One request. An `Err` closes the connection without a response
/// ([`FakeComfy::drop_after_accept`]).
async fn handle(
    shared: Arc<Shared>,
    mut req: Request<Incoming>,
) -> Result<Response<Full>, io::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let bytes = read_body(req.body_mut()).await;
    let body: Option<Value> = serde_json::from_slice(&bytes).ok();
    shared.lock().requests.push(FakeRequest {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        body: body.clone(),
    });
    // ComfyUI serves every route under `/api` too; its jobs routes live only there.
    let route = match path.strip_prefix("/api") {
        Some(rest) if rest.starts_with('/') && !rest.starts_with("/jobs") => rest,
        _ => path.as_str(),
    };
    let segments: Vec<&str> = route.trim_start_matches('/').split('/').collect();
    let response = match (&method, segments.as_slice()) {
        (&Method::GET, ["ws"]) => upgrade_ws(&shared, &mut req, &query),
        (&Method::GET, ["system_stats"]) => json_response(StatusCode::OK, &system_stats()),
        (&Method::POST, ["prompt"]) => return post_prompt(&shared, body),
        (&Method::GET, ["queue"]) => {
            let st = shared.lock();
            let entry = |id: &String| st.prompt(id).map(Prompt::queue_entry);
            let running: Vec<Value> = st.running.iter().filter_map(entry).collect();
            let pending: Vec<Value> = st.queue.iter().filter_map(entry).collect();
            json_response(
                StatusCode::OK,
                &json!({"queue_running": running, "queue_pending": pending}),
            )
        }
        (&Method::GET, ["api", "jobs"]) => {
            json_response(StatusCode::OK, &list_jobs(&shared, &query))
        }
        (&Method::GET, ["api", "jobs", id]) => match shared.lock().prompt(id) {
            Some(p) => json_response(StatusCode::OK, &p.job(true)),
            None => json_response(StatusCode::NOT_FOUND, &json!({"error": "Job not found"})),
        },
        (&Method::POST, ["api", "jobs", id, "cancel"]) => {
            let cancelled = cancel(&shared, id);
            json_response(StatusCode::OK, &json!({"cancelled": cancelled}))
        }
        (&Method::GET, ["history"]) => {
            let st = shared.lock();
            let all: Map<String, Value> = st
                .prompts
                .iter()
                .filter(|p| p.finished())
                .map(|p| (p.id.clone(), p.history()))
                .collect();
            json_response(StatusCode::OK, &Value::Object(all))
        }
        (&Method::GET, ["history", id]) => {
            let st = shared.lock();
            let one: Map<String, Value> = st
                .prompt(id)
                .filter(|p| p.finished())
                .map(|p| (p.id.clone(), p.history()))
                .into_iter()
                .collect();
            json_response(StatusCode::OK, &Value::Object(one))
        }
        (&Method::GET, ["view"]) => view(&shared.root, &query),
        (&Method::POST, ["upload", "image"]) => upload(&shared.root, &req, &bytes),
        (&Method::POST, ["interrupt"]) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"error": "the fake ComfyUI refuses a bare /interrupt"}),
        ),
        (&Method::POST, ["free"]) => json_response(StatusCode::OK, &json!({})),
        (&Method::GET, ["object_info"]) => {
            json_response(StatusCode::OK, &shared.lock().object_info.clone())
        }
        (&Method::GET, ["object_info", class]) => {
            let info = shared.lock().object_info.get(*class).cloned();
            let mut one = Map::new();
            if let Some(info) = info {
                one.insert((*class).to_string(), info);
            }
            json_response(StatusCode::OK, &Value::Object(one))
        }
        (&Method::GET, ["node_replacements"]) => json_response(StatusCode::OK, &json!({})),
        _ => json_response(StatusCode::NOT_FOUND, &json!({"error": "not found"})),
    };
    Ok(response)
}

fn bad_request(kind: &str, message: &str) -> Response<Full> {
    json_response(
        StatusCode::BAD_REQUEST,
        &json!({"error": {"type": kind, "message": message, "details": "", "extra_info": {}},
                "node_errors": {}}),
    )
}

/// A canonical lowercase hyphenated UUID, as `POST /prompt` requires of `prompt_id`.
fn is_canonical_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

fn post_prompt(shared: &Shared, body: Option<Value>) -> Result<Response<Full>, io::Error> {
    let Some(body) = body else {
        return Ok(bad_request("invalid_json", "The body is not JSON"));
    };
    let mut st = shared.lock();
    if st.fail_next == Some(FakeFailure::Refuse503) {
        st.fail_next = None;
        return Ok(json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"error": "the fake ComfyUI refused the prompt"}),
        ));
    }
    let graph = match body.get("prompt") {
        Some(g @ Value::Object(_)) => g.clone(),
        _ => return Ok(bad_request("no_prompt", "No prompt provided")),
    };
    let id = match body.get("prompt_id") {
        None | Some(Value::Null) => st.mint_id(),
        Some(Value::String(id)) if is_canonical_uuid(id) => id.clone(),
        Some(_) => {
            return Ok(bad_request(
                "invalid_prompt_id",
                "prompt_id must be a canonical lowercase UUID",
            ));
        }
    };
    let client_id = body
        .get("client_id")
        .and_then(Value::as_str)
        .map(String::from);
    let extra = body.get("extra_data").cloned().unwrap_or(json!({}));
    let mut prompt = Prompt::new(&mut st, id.clone(), graph, client_id, extra);
    prompt.held = st.hold;
    prompt.fail = match st.fail_next.take() {
        Some(FakeFailure::Refuse503) | None => None,
        other => other,
    };
    let number = prompt.number;
    st.prompts.push(prompt);
    if st.running.is_none() {
        st.running = Some(id.clone());
    } else {
        st.queue.push(id.clone());
    }
    st.broadcast_status();
    let dropped = st.drop_after_accept;
    drop(st);
    shared.wake.notify_one();
    if dropped {
        return Err(io::Error::other(
            "the fake ComfyUI drops the connection after accepting the prompt",
        ));
    }
    Ok(json_response(
        StatusCode::OK,
        &json!({"prompt_id": id, "number": number, "node_errors": {}}),
    ))
}

/// `POST /api/jobs/{id}/cancel`: a pending prompt is removed, a running one interrupted.
fn cancel(shared: &Shared, id: &str) -> bool {
    let mut st = shared.lock();
    let Some(phase) = st.prompt(id).map(|p| p.phase) else {
        return false;
    };
    let cancelled = match phase {
        Phase::Pending if st.queue.iter().any(|q| q == id) => {
            st.queue.retain(|q| q != id);
            if let Some(p) = st.prompt_mut(id) {
                p.phase = Phase::Cancelled;
                p.end_time = Some(now_ms());
            }
            st.broadcast_status();
            true
        }
        Phase::Pending | Phase::Running => {
            if let Some(p) = st.prompt_mut(id) {
                p.cancel = true;
            }
            true
        }
        Phase::Completed | Phase::Failed | Phase::Cancelled => false,
    };
    drop(st);
    shared.wake.notify_one();
    cancelled
}

fn list_jobs(shared: &Shared, query: &str) -> Value {
    let st = shared.lock();
    let statuses: Option<Vec<String>> =
        query_param(query, "status").map(|s| s.split(',').map(str::to_string).collect());
    let mut jobs: Vec<Value> = st
        .prompts
        .iter()
        .filter(|p| {
            statuses
                .as_ref()
                .is_none_or(|s| s.iter().any(|s| s == p.phase.as_str()))
        })
        .map(|p| p.job(false))
        .collect();
    if query_param(query, "sort_order").as_deref() != Some("asc") {
        jobs.reverse();
    }
    let total = jobs.len();
    let offset: usize = query_param(query, "offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let limit: Option<usize> = query_param(query, "limit").and_then(|s| s.parse().ok());
    let page: Vec<Value> = jobs
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let has_more = offset + page.len() < total;
    json!({"jobs": page, "pagination": {"offset": offset, "limit": limit, "total": total,
                                        "has_more": has_more}})
}

/// A query parameter, percent-decoded.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A relative path made only of normal components.
fn safe_join(base: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for part in parts.iter().filter(|p| !p.is_empty()) {
        let rel = Path::new(part);
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return None;
        }
        path.push(rel);
    }
    Some(path)
}

/// `GET /view?filename=&subfolder=&type=`: a file from `<root>\<type>\<subfolder>`.
fn view(root: &Path, query: &str) -> Response<Full> {
    let filename = query_param(query, "filename").unwrap_or_default();
    let subfolder = query_param(query, "subfolder").unwrap_or_default();
    let kind = query_param(query, "type").unwrap_or_else(|| "output".into());
    if !matches!(kind.as_str(), "output" | "input" | "temp") || filename.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "bad view request"}),
        );
    }
    match safe_join(root, &[&kind, &subfolder, &filename]).map(std::fs::read) {
        Some(Ok(bytes)) => respond(StatusCode::OK, "image/png", bytes),
        _ => json_response(StatusCode::NOT_FOUND, &json!({"error": "not found"})),
    }
}

/// `POST /upload/image` (multipart): the `image` part is written to `<root>\input`.
fn upload(root: &Path, req: &Request<Incoming>, body: &[u8]) -> Response<Full> {
    let boundary = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("boundary=").nth(1))
        .map(|b| b.trim_matches('"').to_string());
    let Some(boundary) = boundary else {
        return json_response(StatusCode::BAD_REQUEST, &json!({"error": "not multipart"}));
    };
    let delimiter = format!("--{boundary}");
    let text_parts = split_bytes(body, delimiter.as_bytes());
    for part in text_parts {
        let Some(split) = find_bytes(part, b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&part[..split]);
        let data = &part[split + 4..];
        let data = data.strip_suffix(b"\r\n").unwrap_or(data);
        if !headers.contains("name=\"image\"") {
            continue;
        }
        let Some(name) = headers
            .split("filename=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
        else {
            continue;
        };
        let Some(path) = safe_join(&root.join("input"), &[name]) else {
            return json_response(StatusCode::BAD_REQUEST, &json!({"error": "bad file name"}));
        };
        if std::fs::write(&path, data).is_err() {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error": "write"}),
            );
        }
        return json_response(
            StatusCode::OK,
            &json!({"name": name, "subfolder": "", "type": "input"}),
        );
    }
    json_response(StatusCode::BAD_REQUEST, &json!({"error": "no image part"}))
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn split_bytes<'a>(hay: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut rest = hay;
    while let Some(at) = find_bytes(rest, delimiter) {
        parts.push(&rest[..at]);
        rest = &rest[at + delimiter.len()..];
    }
    parts.push(rest);
    parts
}

/// The websocket handshake. The client is registered before the 101 goes out, so an event
/// sent once the client's handshake has returned always reaches it.
fn upgrade_ws(shared: &Arc<Shared>, req: &mut Request<Incoming>, query: &str) -> Response<Full> {
    let Some(key) = req
        .headers()
        .get("sec-websocket-key")
        .map(|k| k.as_bytes().to_vec())
    else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "not a websocket"}),
        );
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let preview_metadata = Arc::new(AtomicBool::new(false));
    let serial = {
        let mut st = shared.lock();
        let client_id = query_param(query, "clientId")
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| st.mint_id());
        st.next_serial += 1;
        let serial = st.next_serial;
        let status = json!({"type": "status", "data": {
            "status": {"exec_info": {"queue_remaining": st.queue_remaining()}},
            "sid": client_id}});
        let _ = tx.send(Message::text(status.to_string()));
        st.clients.push(Client {
            serial,
            client_id,
            tx,
            preview_metadata: Arc::clone(&preview_metadata),
        });
        serial
    };
    let on_upgrade = hyper::upgrade::on(req);
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        if let Ok(upgraded) = on_upgrade.await {
            let ws =
                WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
            let (mut sink, mut stream) = ws.split();
            let reader = async {
                while let Some(Ok(message)) = stream.next().await {
                    match message {
                        Message::Text(text) => {
                            let flags: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                            if flags["type"] == "feature_flags"
                                && flags["data"]["supports_preview_metadata"] == true
                            {
                                preview_metadata.store(true, Ordering::SeqCst);
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            };
            let writer = async {
                while let Some(message) = rx.recv().await {
                    if sink.send(message).await.is_err() {
                        break;
                    }
                }
                let _ = sink.close().await;
            };
            futures_util::future::select(pin!(reader), pin!(writer)).await;
        }
        shared.lock().clients.retain(|c| c.serial != serial);
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-accept", derive_accept_key(&key))
        .body(Full(None))
        .expect("a valid response")
}

fn system_stats() -> Value {
    json!({
        "system": {
            "os": "nt",
            "ram_total": 34_359_738_368_u64,
            "ram_free": 17_179_869_184_u64,
            "comfyui_version": "0.41.0",
            "required_frontend_version": "1.0.0",
            "python_version": "3.12.0 (fake)",
            "pytorch_version": "2.11.0+cu130",
            "embedded_python": false,
            "argv": ["main.py"],
        },
        "devices": [{
            "name": "cuda:0 Fake GPU : cudaMallocAsync",
            "type": "cuda",
            "index": 0,
            "vram_total": 17_179_869_184_u64,
            "vram_free": 16_106_127_360_u64,
            "torch_vram_total": 0,
            "torch_vram_free": 0,
        }],
    })
}

fn default_object_info() -> Value {
    json!({
        "EmptyImage": {
            "input": {"required": {
                "width": ["INT", {"default": 512}], "height": ["INT", {"default": 512}],
                "batch_size": ["INT", {"default": 1}], "color": ["INT", {"default": 0}]}},
            "output": ["IMAGE"], "output_name": ["IMAGE"], "name": "EmptyImage",
            "display_name": "EmptyImage", "category": "image", "output_node": false,
            "api_node": false,
        },
        "SaveImage": {
            "input": {"required": {
                "images": ["IMAGE"], "filename_prefix": ["STRING", {"default": "ComfyUI"}]}},
            "output": [], "output_name": [], "name": "SaveImage", "display_name": "Save Image",
            "category": "image", "output_node": true, "api_node": false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-066
    fn query_params_are_percent_decoded() {
        let q = "filename=lil_a%20b_00001_.png&subfolder=&type=output&x=%zz";
        assert_eq!(
            query_param(q, "filename").as_deref(),
            Some("lil_a b_00001_.png")
        );
        assert_eq!(query_param(q, "subfolder").as_deref(), Some(""));
        assert_eq!(query_param(q, "x").as_deref(), Some("%zz"));
        assert_eq!(query_param(q, "missing"), None);
        assert!(is_canonical_uuid("0f1e2d3c-4b5a-4968-8776-655443322110"));
        assert!(!is_canonical_uuid("0F1E2D3C-4B5A-4968-8776-655443322110"));
        assert!(!is_canonical_uuid("0f1e2d3c4b5a49688776655443322110"));
    }
}
