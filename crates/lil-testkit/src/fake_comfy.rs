//! `FakeComfy`: an in-process ComfyUI for tests (QA-066), HTTP and websocket on
//! `127.0.0.1:<free port>`. The `lil-fake-comfyui` binary serves the same fake for tests that
//! need a separate process.
//!
//! It answers the endpoints the core uses, in ComfyUI 0.41.0's shapes as far as the core reads
//! them (spec Appendix B.3): `GET /system_stats`, `POST /prompt`, `GET /queue`, `GET /api/jobs`,
//! `GET /api/jobs/{id}`, `POST /api/jobs/{id}/cancel`, `POST /api/jobs/cancel` (the batch
//! cancel), `GET /history[/{id}]`, `GET /view`,
//! `POST /upload/image`, `POST /free`, `GET /object_info[/{class}]`, `GET /node_replacements`
//! and the websocket `/ws?clientId=<id>`. Every route also answers under `/api` (except the
//! jobs routes, which live there), as ComfyUI's do. `POST /interrupt` answers 500, so a suite
//! that posts it fails visibly (ARCH-021). Every request is recorded ([`FakeComfy::requests`]).
//! As ComfyUI 0.42.1 does, `/api/jobs` answers 400 to a bad `status`, `sort_by`, `sort_order`,
//! `limit` or `offset`, and a websocket client whose first message is `feature_flags` gets the
//! server's own `feature_flags` back. The drift check against recorded ComfyUI shapes is
//! [`crate::comfy_shapes`].
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
    /// `POST /prompt` answers 400 with `{"error": {"type": "prompt_outputs_failed_validation"},
    /// "node_errors": <node_errors>}` (no `node_errors` key when it is `null`) and queues
    /// nothing.
    Reject400 { node_errors: Value },
    /// `POST /prompt` drops the connection before answering, with nothing queued yet; the
    /// prompt is queued 1.5 s later, as a ComfyUI still validating a prompt whose client gave
    /// up would.
    AcceptLate,
}

impl FakeFailure {
    /// Whether it answers the `POST /prompt` itself instead of failing the run.
    fn at_post(&self) -> bool {
        matches!(
            self,
            FakeFailure::Refuse503 | FakeFailure::Reject400 { .. } | FakeFailure::AcceptLate
        )
    }
}

/// What [`FakeComfy::on_prompt`] calls with each `POST /prompt` body.
type PromptHook = Arc<dyn Fn(&Value) + Send + Sync>;

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
            uploads: Notify::new(),
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
            st.set_running(&id);
        } else if st.running.is_none() {
            st.set_running(&id);
        } else {
            st.queue.push(id.clone());
        }
        st.broadcast_status();
        drop(st);
        self.shared.wake.notify_one();
        id
    }

    /// The next prompt the suite submits fails as `kind` says ([`FakeFailure`]). Calls queue
    /// up: each `POST /prompt` takes the oldest one.
    pub fn fail_next(&self, kind: FakeFailure) {
        self.shared.lock().fail_next.push_back(kind);
    }

    /// The next suite prompt that runs also reports an output named `filename` (under a node
    /// `extra<n>`), as a custom save node might; with `write` the fake writes that file into
    /// `<root>\output` first, otherwise it only reports it.
    pub fn report_extra_output(&self, filename: &str, write: bool) {
        self.shared
            .lock()
            .extra_outputs
            .push((filename.to_string(), write));
    }

    /// While on, every HTTP request but the websocket's is answered 503 (ComfyUI busy or
    /// stalled); the websocket stays connected.
    pub fn refuse_http(&self, on: bool) {
        self.shared.lock().refuse_http = on;
    }

    /// While on, each `POST /upload/image` is recorded and then left unanswered until it is
    /// turned off (a test holds an attempt between its start and its `POST /prompt`).
    pub fn hold_uploads(&self, on: bool) {
        self.shared.lock().hold_uploads = on;
        self.shared.uploads.notify_waiters();
    }

    /// Calls `hook` with the body of each `POST /prompt` when it arrives, before the fake
    /// does anything with it (on the fake's thread: keep it short).
    pub fn on_prompt(&self, hook: impl Fn(&Value) + Send + Sync + 'static) {
        self.shared.lock().on_prompt = Some(Arc::new(hook));
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

    /// The `system.argv` `GET /system_stats` reports (default: `["main.py"]`).
    pub fn set_argv(&self, argv: &[&str]) {
        self.shared.lock().argv = Some(argv.iter().map(|a| (*a).to_string()).collect());
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
    /// Wakes the uploads [`FakeComfy::hold_uploads`] holds.
    uploads: Notify,
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
    /// `[event, data]` pairs, as `/history` reports them: the `execution_*` events only.
    messages: Vec<Value>,
    /// Node id -> its `progress_state` entry, for the nodes that are no longer pending.
    node_states: Map<String, Value>,
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
            node_states: Map::new(),
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

    /// The first saved image with its node id and media type, as ComfyUI 0.42.1's job items
    /// show it (`null`, so pruned, without one).
    fn preview_output(&self) -> Value {
        self.outputs
            .iter()
            .find_map(|(node, o)| {
                let mut image = o["images"].as_array()?.first()?.clone();
                image["nodeId"] = json!(node);
                image["mediaType"] = json!("images");
                Some(image)
            })
            .unwrap_or(Value::Null)
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
            "previewable_outputs_count": self.outputs_count(),
            "preview_output": self.preview_output(),
            "workflow_id": Value::Null,
        });
        if let Some(error) = &self.error {
            job["execution_error"] = error.clone();
        }
        // As ComfyUI 0.41.0: only a finished job's detail has these (a pending or running
        // prompt's graph is in `/queue` only).
        if detail && self.finished() {
            job["workflow"] = json!({"prompt": self.graph, "extra_data": self.extra_data});
            job["outputs"] = Value::Object(self.outputs.clone());
            job["execution_status"] = self.status();
        }
        // ComfyUI prunes fields that are `None`.
        if let Value::Object(fields) = &mut job {
            fields.retain(|_, v| !v.is_null());
        }
        job
    }

    /// The history `status`: `{status_str, completed, messages}`.
    fn status(&self) -> Value {
        json!({
            "status_str": if self.phase == Phase::Completed { "success" } else { "error" },
            "completed": self.phase == Phase::Completed,
            "messages": self.messages,
        })
    }

    fn history(&self) -> Value {
        json!({
            "prompt": self.queue_entry(),
            "outputs": self.outputs,
            "status": self.status(),
            "meta": self.meta(),
        })
    }

    /// The history `meta`: one entry per output node (ComfyUI 0.42.1).
    fn meta(&self) -> Value {
        let meta: Map<String, Value> = self
            .outputs
            .keys()
            .map(|node| {
                let entry = json!({"node_id": node, "display_node": node,
                                   "parent_node": null, "real_node_id": node});
                (node.clone(), entry)
            })
            .collect();
        Value::Object(meta)
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
    fail_next: std::collections::VecDeque<FakeFailure>,
    on_prompt: Option<PromptHook>,
    extra_outputs: Vec<(String, bool)>,
    refuse_http: bool,
    hold_uploads: bool,
    drop_after_accept: bool,
    restart_after: Option<u32>,
    outputs_since_armed: u32,
    /// Bumped by each restart; a run started before it stops.
    generation: u64,
    requests: Vec<FakeRequest>,
    object_info: Value,
    /// `system.argv` (`None`: `["main.py"]`).
    argv: Option<Vec<String>>,
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

    /// Sends `kind` to the prompt's client; an `execution_*` event is also kept in the
    /// prompt's history messages, as ComfyUI keeps only those.
    fn event(&mut self, id: &str, kind: &str, data: Value) {
        let to = self.prompt(id).and_then(|p| p.client_id.clone());
        self.send(to.as_deref(), kind, data.clone());
        if kind.starts_with("execution_")
            && let Some(p) = self.prompt_mut(id)
        {
            p.messages.push(json!([kind, data]));
        }
    }

    /// Node `node` of prompt `id` is `running` or `finished`: a `progress_state` with every
    /// node that is no longer pending, as ComfyUI 0.42.1 sends it.
    fn node_state(&mut self, id: &str, node: &str, state: &str) {
        let Some(p) = self.prompt_mut(id) else {
            return;
        };
        let value = if state == "finished" { 1.0 } else { 0.0 };
        p.node_states.insert(
            node.to_string(),
            json!({"value": value, "max": 1.0, "state": state, "node_id": node,
                   "prompt_id": id, "display_node_id": node, "parent_node_id": null,
                   "real_node_id": node}),
        );
        let data = json!({"prompt_id": id, "nodes": p.node_states});
        self.event(id, "progress_state", data);
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
            // ComfyUI sends `progress` only from nodes that report steps (samplers); these
            // report their state.
            self.node_state(&id, &node, "running");
            self.event(
                &id,
                "executing",
                json!({"node": node, "display_node": node, "prompt_id": id}),
            );
            self.node_state(&id, &node, "finished");
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
            self.running = None;
            if !self.queue.is_empty() {
                let next = self.queue.remove(0);
                self.set_running(&next);
            }
        }
        self.broadcast_status();
    }

    /// Makes `id` the running prompt: `in_progress` from now on, though the executor sends its
    /// `execution_start` a moment later.
    fn set_running(&mut self, id: &str) {
        self.running = Some(id.to_string());
        if let Some(p) = self.prompt_mut(id) {
            p.phase = Phase::Running;
        }
    }

    fn fail(&mut self, id: &str, failure: &FakeFailure, node: (String, String)) {
        let (exception_type, message) = match failure {
            FakeFailure::Oom => (
                "torch.OutOfMemoryError",
                "Allocation on device 0 would exceed allowed memory. CUDA out of memory. Tried to allocate 2.00 GiB".to_string(),
            ),
            FakeFailure::Execution { message } => ("RuntimeError", message.clone()),
            FakeFailure::Refuse503 | FakeFailure::Reject400 { .. } | FakeFailure::AcceptLate => {
                ("RuntimeError", "refused".to_string())
            }
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
        {
            let mut st = shared.lock();
            st.node_state(&job.id, &node, "running");
            let data = json!({"node": node, "display_node": node, "prompt_id": job.id});
            st.event(&job.id, "executing", data);
        }
        let (subfolder, file) = write_output(&shared.root.join("output"), &prefix);
        // In a block, so the guard is gone before the await.
        let go_on = {
            let mut st = shared.lock();
            if st.generation != job.generation {
                return;
            }
            // The fake's PNG is 1x1; ComfyUI 0.42.1 adds the file's media metadata.
            let image = json!({"filename": file, "subfolder": subfolder, "type": "output",
                               "metadata": {"kind": "image", "width": 1, "height": 1}});
            if let Some(p) = st.prompt_mut(&job.id) {
                p.outputs
                    .insert(node.clone(), json!({"images": [image.clone()]}));
            }
            let data = json!({"node": node, "display_node": node,
                              "output": {"images": [image]}, "prompt_id": job.id});
            st.event(&job.id, "executed", data);
            st.node_state(&job.id, &node, "finished");
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
    let extra = std::mem::take(&mut st.extra_outputs);
    for (n, (filename, write)) in extra.into_iter().enumerate() {
        if write {
            let path = shared.root.join("output").join(&filename);
            std::fs::write(&path, PNG)
                .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        }
        let image = json!({"filename": filename, "subfolder": "", "type": "output"});
        if let Some(p) = st.prompt_mut(&job.id) {
            p.outputs
                .insert(format!("extra{n}"), json!({"images": [image]}));
        }
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
    if shared.lock().refuse_http && segments.as_slice() != ["ws"] {
        return Ok(json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"error": "the fake ComfyUI is not answering"}),
        ));
    }
    let response = match (&method, segments.as_slice()) {
        (&Method::GET, ["ws"]) => upgrade_ws(&shared, &mut req, &query),
        (&Method::GET, ["system_stats"]) => {
            let argv = shared
                .lock()
                .argv
                .clone()
                .unwrap_or_else(|| vec!["main.py".to_string()]);
            json_response(StatusCode::OK, &system_stats(&argv))
        }
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
        (&Method::GET, ["api", "jobs"]) => match list_jobs(&shared, &query) {
            Ok(list) => json_response(StatusCode::OK, &list),
            Err(why) => json_response(StatusCode::BAD_REQUEST, &json!({"error": why})),
        },
        (&Method::POST, ["api", "jobs", "cancel"]) => cancel_batch(&shared, &bytes),
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
        (&Method::POST, ["upload", "image"]) => {
            loop {
                let released = shared.uploads.notified();
                if !shared.lock().hold_uploads {
                    break;
                }
                released.await;
            }
            upload(&shared.root, &req, &bytes)
        }
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

fn post_prompt(shared: &Arc<Shared>, body: Option<Value>) -> Result<Response<Full>, io::Error> {
    let Some(body) = body else {
        return Ok(bad_request("invalid_json", "The body is not JSON"));
    };
    let hook = shared.lock().on_prompt.clone();
    if let Some(hook) = hook {
        hook(&body);
    }
    let mut st = shared.lock();
    match st.fail_next.front() {
        Some(FakeFailure::Refuse503) => {
            st.fail_next.pop_front();
            return Ok(json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error": "the fake ComfyUI refused the prompt"}),
            ));
        }
        Some(FakeFailure::Reject400 { node_errors }) => {
            let mut answer = json!({"error": {"type": "prompt_outputs_failed_validation"}});
            if !node_errors.is_null() {
                answer["node_errors"] = node_errors.clone();
            }
            st.fail_next.pop_front();
            return Ok(json_response(StatusCode::BAD_REQUEST, &answer));
        }
        Some(FakeFailure::AcceptLate) => {
            st.fail_next.pop_front();
            drop(st);
            let late = Arc::clone(shared);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let _ = accept(&late, &body);
            });
            return Err(io::Error::other(
                "the fake ComfyUI drops the connection before it accepts the prompt",
            ));
        }
        _ => {}
    }
    drop(st);
    let (id, number) = match accept(shared, &body) {
        Ok(accepted) => accepted,
        Err(refused) => return Ok(*refused),
    };
    let dropped = shared.lock().drop_after_accept;
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

/// Queues the prompt of a `POST /prompt` body: `(prompt id, number)`, or the 400 answer.
fn accept(shared: &Shared, body: &Value) -> Result<(String, u64), Box<Response<Full>>> {
    let mut st = shared.lock();
    let graph = match body.get("prompt") {
        Some(g @ Value::Object(_)) => g.clone(),
        _ => return Err(Box::new(bad_request("no_prompt", "No prompt provided"))),
    };
    let id = match body.get("prompt_id") {
        None | Some(Value::Null) => st.mint_id(),
        Some(Value::String(id)) if is_canonical_uuid(id) => id.clone(),
        Some(_) => {
            return Err(Box::new(bad_request(
                "invalid_prompt_id",
                "prompt_id must be a canonical lowercase UUID",
            )));
        }
    };
    let client_id = body
        .get("client_id")
        .and_then(Value::as_str)
        .map(String::from);
    let extra = body.get("extra_data").cloned().unwrap_or(json!({}));
    let mut prompt = Prompt::new(&mut st, id.clone(), graph, client_id, extra);
    prompt.held = st.hold;
    prompt.fail = match st.fail_next.front() {
        Some(f) if !f.at_post() => st.fail_next.pop_front(),
        _ => None,
    };
    let number = prompt.number;
    st.prompts.push(prompt);
    if st.running.is_none() {
        st.set_running(&id);
    } else {
        st.queue.push(id.clone());
    }
    st.broadcast_status();
    drop(st);
    shared.wake.notify_one();
    Ok((id, number))
}

/// `POST /api/jobs/{id}/cancel`: a pending prompt is dequeued and forgotten (it never reaches
/// `/history`, as in ComfyUI), a running one interrupted.
fn cancel(shared: &Shared, id: &str) -> bool {
    let mut st = shared.lock();
    let Some(phase) = st.prompt(id).map(|p| p.phase) else {
        return false;
    };
    let cancelled = match phase {
        Phase::Pending if st.queue.iter().any(|q| q == id) => {
            st.queue.retain(|q| q != id);
            st.prompts.retain(|p| p.id != id);
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

/// `POST /api/jobs/cancel {"job_ids": [...]}`: every id must be a canonical lowercase UUID
/// (else 400 naming them); each running or pending one is cancelled as by the per-job cancel,
/// and `{"cancelled": true}` says at least one was (ComfyUI 0.42.1).
fn cancel_batch(shared: &Shared, bytes: &[u8]) -> Response<Full> {
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "Request body must be valid JSON"}),
        );
    };
    let Some(ids) = body.get("job_ids").and_then(Value::as_array) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "job_ids must be a list"}),
        );
    };
    let invalid: Vec<Value> = ids
        .iter()
        .filter(|id| !id.as_str().is_some_and(is_canonical_uuid))
        .map(|id| match id {
            Value::String(s) => Value::from(s.as_str()),
            other => Value::from(other.to_string()),
        })
        .collect();
    if !invalid.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "job_ids contains invalid id(s)", "invalid_ids": invalid}),
        );
    }
    let mut cancelled = false;
    for id in ids.iter().filter_map(Value::as_str) {
        cancelled |= cancel(shared, id);
    }
    json_response(StatusCode::OK, &json!({"cancelled": cancelled}))
}

/// The `/api/jobs` statuses.
const JOB_STATUSES: [&str; 5] = ["pending", "in_progress", "completed", "failed", "cancelled"];

/// `GET /api/jobs`, or why its parameters are refused (a 400, as ComfyUI 0.42.1 answers a bad
/// `status`, `sort_by`, `sort_order`, `limit` or `offset`).
fn list_jobs(shared: &Shared, query: &str) -> Result<Value, String> {
    let statuses: Option<Vec<String>> = query_param(query, "status").map(|s| {
        s.split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let bad: Vec<String> = statuses
        .iter()
        .flatten()
        .filter(|s| !JOB_STATUSES.contains(&s.as_str()))
        .cloned()
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "Invalid status value(s): {}. Valid values: {}",
            bad.join(", "),
            JOB_STATUSES.join(", ")
        ));
    }
    let lower = |key: &str, default: &str| {
        query_param(query, key).map_or_else(|| default.to_string(), |s| s.to_lowercase())
    };
    let sort_by = lower("sort_by", "created_at");
    if !matches!(sort_by.as_str(), "created_at" | "execution_duration") {
        return Err("sort_by must be 'created_at' or 'execution_duration'".into());
    }
    let sort_order = lower("sort_order", "desc");
    if !matches!(sort_order.as_str(), "asc" | "desc") {
        return Err("sort_order must be 'asc' or 'desc'".into());
    }
    let limit = match query_param(query, "limit") {
        None => None,
        Some(l) => match l.trim().parse::<i64>() {
            Ok(n) if n > 0 => Some(usize::try_from(n).unwrap_or(usize::MAX)),
            Ok(_) => return Err("limit must be a positive integer".into()),
            Err(_) => return Err("limit must be an integer".into()),
        },
    };
    let offset = match query_param(query, "offset") {
        None => 0,
        Some(o) => match o.trim().parse::<i64>() {
            Ok(n) => usize::try_from(n).unwrap_or(0),
            Err(_) => return Err("offset must be an integer".into()),
        },
    };
    let st = shared.lock();
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
    if sort_order != "asc" {
        jobs.reverse();
    }
    let total = jobs.len();
    let page: Vec<Value> = jobs
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let has_more = offset + page.len() < total;
    Ok(
        json!({"jobs": page, "pagination": {"offset": offset, "limit": limit, "total": total,
                                        "has_more": has_more}}),
    )
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
    let reply_tx;
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
        st.clients.retain(|c| c.client_id != client_id);
        // Weak: a restart drops the client's sender, which must close its socket.
        reply_tx = tx.downgrade();
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
                let mut first = true;
                while let Some(Ok(message)) = stream.next().await {
                    match message {
                        Message::Text(text) => {
                            let flags: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                            // Only a first message announces flags, and the server answers
                            // with its own (ComfyUI 0.42.1).
                            if first && flags["type"] == "feature_flags" {
                                if flags["data"]["supports_preview_metadata"] == true {
                                    preview_metadata.store(true, Ordering::SeqCst);
                                }
                                let reply =
                                    json!({"type": "feature_flags", "data": server_features()});
                                if let Some(tx) = reply_tx.upgrade() {
                                    let _ = tx.send(Message::text(reply.to_string()));
                                }
                            }
                            first = false;
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

/// The server's `feature_flags` reply: ComfyUI 0.42.1's core flags, with its default 100 MB
/// upload size.
fn server_features() -> Value {
    json!({
        "supports_preview_metadata": true,
        "supports_model_type_tags": true,
        "max_upload_size": 100 * 1024 * 1024,
        "extension": {"manager": {"supports_v4": true, "supports_csrf_post": true}},
        "node_replacements": true,
        "assets": true,
    })
}

fn system_stats(argv: &[String]) -> Value {
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
            "argv": argv,
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
