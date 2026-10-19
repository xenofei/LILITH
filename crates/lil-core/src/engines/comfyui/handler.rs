//! The `comfy.prompt` jobs (Appendix F "ComfyUI client", "Retries and recovery"; ARCH-021,
//! ARCH-033, ARCH-060, ARCH-072, ARCH-080, CORE-120, CORE-123, CORE-128, CORE-130, QA-009):
//! one ComfyUI prompt per job, in the `gpu-heavy` lane, so the suite has at most one prompt
//! pending in ComfyUI at a time (CORE-123). Arguments `{graph, preview_method?, inputs?}`; an
//! input `{node, input, path}` is a local file uploaded once by content hash
//! ([`super::uploads`]) whose upload name becomes `graph[node].inputs[input]`.
//!
//! An attempt:
//! 1. **What an earlier attempt sent** (CORE-120). The `jobs` row's `external_id` is read back:
//!    a prompt ComfyUI still has is followed, one that succeeded is ingested from `/history`,
//!    and only one that failed or is gone is replaced by a new prompt.
//! 2. **A new prompt.** Every `SaveImage`-type node's `filename_prefix` becomes
//!    `lil_<first 16 hex of the job id>_` ([`prefix_outputs`]); the inputs are uploaded; the
//!    websocket is opened; a UUID is minted and persisted as the row's `external_id` (the row is
//!    `submitting` already) before `POST /prompt` with the channel's stable `client_id` and
//!    `extra_data.preview_method` (default `"taesd"`). A POST that fails before an answer, or
//!    with 429 or 5xx, is followed by a look at `/api/jobs/{id}` and `/history/{id}` before
//!    anything else: a prompt that arrived is followed, a gone one is posted once more. A `400`
//!    fails the job at once with `comfyui.execution_failed` and the reply's node errors as
//!    untrusted detail ([`rejection_detail`]).
//! 3. **Following it**: `queued_in_engine` once posted, `executing` at `execution_start` (the
//!    watchdog's start), progress from `progress`, previews of at most 1 MB on
//!    `preview:<job_id>` as `{format, data_b64}`. The websocket is backed by a look at the
//!    prompt every 2 s, so a closed socket or a missed event is caught.
//! 4. **The end.** Success: `ingesting`, the outputs from `/history` streamed through `/view`
//!    into the staging handle ([`super::salvage::Staging`]), each verified (size, and the
//!    staged file's SHA-256 equal to the stream's); only then, with `root` known, each
//!    ComfyUI-side copy is removed. The executed graph comes from `/history`, and the
//!    execution window (from `execution_start`, with the engine's pid) goes to
//!    [`JobDone::windows`]. An `execution_error` is `comfyui.execution_failed` with ComfyUI's
//!    text as untrusted detail (ARCH-060); CUDA out of memory ([`is_oom`]) posts `/free`, re-reads
//!    the VRAM and is retried while `attempt <= OOM_RETRIES`, then fails with
//!    `comfyui.out_of_memory` (ARCH-072). An `execution_interrupted` the job didn't ask for was
//!    an outside cancel: `comfyui.execution_failed`, "cancelled from outside the suite", never
//!    retried. A prompt ComfyUI lost (a restart: absent from `/api/jobs`, `/history` and
//!    `/queue`, or no answer for 30 s) makes the job interrupted, with the outputs already
//!    written salvaged by the prefix ([`super::salvage::salvage`]); the runner re-runs an
//!    `agent` job once (CORE-128).
//!
//! **Cancel** ([`JobHandler::cancel_in_engine`]) posts `POST /api/jobs/{id}/cancel` once, never
//! `/interrupt` (ARCH-021); the POST and the cancel take one lock, so a prompt is either never
//! posted or cancelled after it was. The `CancelEnginePrompts` shutdown step cancels every
//! suite prompt the same way ([`SuitePrompts::cancel_all`]); such a job ends interrupted.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use base64::Engine as _;
use lil_protocol::{CodeSpec, Detail, ErrorBody, FileRef, codes};
use serde::Deserialize;
use serde_json::{Value, json};

use super::client::{ComfyClient, ComfyError, OutputFile, RemoteState};
use super::salvage::{self, Staging, sha256_file};
use super::supervisor::{Comfy, ComfyEndpoint};
use super::uploads::Uploads;
use super::ws::{ComfyEvent, ComfySocket, MAX_PREVIEW_BYTES};
use crate::engines::Engines;
use crate::jobs::record;
use crate::jobs::{
    EngineKind, ExecWindow, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobId, JobSpec,
    Lane, Priority, Substatus, cancelled_error,
};
use crate::store::fsops;

/// The job kind.
pub const PROMPT_KIND: &str = "comfy.prompt";
/// How often an out-of-memory failure is retried (ARCH-072): attempts 1 and 2 retry.
pub const OOM_RETRIES: u32 = 2;
/// `extra_data.preview_method` unless the arguments say otherwise (CORE-130).
pub const DEFAULT_PREVIEW_METHOD: &str = "taesd";
/// The `expected_secs` of a prompt whose submitter gives none (the watchdog's yardstick).
pub const DEFAULT_EXPECTED_SECS: u32 = 300;
/// The most bytes of engine text an error's detail holds.
pub const MAX_DETAIL_BYTES: usize = 2048;
/// The most node errors a rejected prompt's detail lists.
pub const MAX_NODE_ERRORS: usize = 20;
/// How often a followed prompt is looked at besides its websocket.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// How long ComfyUI may not answer before a followed prompt counts as lost.
const LOST_AFTER: Duration = Duration::from_secs(30);
/// How long a cancelled job waits for ComfyUI to drop its prompt.
const CANCEL_WAIT: Duration = Duration::from_secs(10);
/// How long a finished prompt's `/history` entry may take to appear.
const HISTORY_WAIT: Duration = Duration::from_secs(5);

/// One input file of a prompt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptInput {
    /// The node id in the graph.
    pub node: String,
    /// The input name, set to the upload's name.
    pub input: String,
    /// The local file (absolute).
    pub path: PathBuf,
}

/// The job's arguments.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptArgs {
    /// The API-format graph.
    pub graph: Value,
    #[serde(default)]
    pub preview_method: Option<String>,
    #[serde(default)]
    pub inputs: Vec<PromptInput>,
}

/// The `JobSpec` of a `comfy.prompt` job with `args` (`{graph, preview_method?, inputs?}`).
pub fn prompt_spec(
    args: Value,
    priority: Priority,
    module: &str,
    project: Option<String>,
    expected_secs: u32,
) -> JobSpec {
    JobSpec {
        kind: PROMPT_KIND,
        lane: Lane::GpuHeavy,
        priority,
        module: module.to_string(),
        args,
        project,
        asset_id: None,
        doc_id: None,
        expected_secs,
        expected_output_bytes: 0,
        vram_need_mib: None,
        engine: Some(EngineKind::ComfyUi),
        group: None,
        external_side_effects: false,
    }
}

/// Sets every `SaveImage`-type node's `filename_prefix` (every node with a string
/// `filename_prefix` input) to the job's prefix and returns it: `lil_<first 16 hex of the
/// job id>_` (CORE-130).
pub fn prefix_outputs(graph: &mut Value, job: &JobId) -> String {
    let prefix = job_prefix(job);
    if let Some(nodes) = graph.as_object_mut() {
        for node in nodes.values_mut() {
            if let Some(slot) = node
                .get_mut("inputs")
                .and_then(|i| i.get_mut("filename_prefix"))
                .filter(|v| v.is_string())
            {
                *slot = Value::from(prefix.as_str());
            }
        }
    }
    prefix
}

/// `lil_<first 16 hex of the job id>_`.
fn job_prefix(job: &JobId) -> String {
    let hex = job.as_str().strip_prefix("j_").unwrap_or(job.as_str());
    format!("lil_{}_", &hex[..hex.len().min(16)])
}

/// Waits until `cancel` turns `true`; never ready when its sender is gone while it is `false`.
async fn cancel_set(cancel: &mut tokio::sync::watch::Receiver<bool>) {
    if cancel.wait_for(|c| *c).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// CUDA out of memory (Appendix F): `exception_type` contains `OutOfMemoryError`, or the
/// message contains `CUDA out of memory`.
pub fn is_oom(exception_type: &str, message: &str) -> bool {
    exception_type.contains("OutOfMemoryError") || message.contains("CUDA out of memory")
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The detail of a `POST /prompt` answered `400` (Appendix F): with `node_errors`, compact JSON
/// of up to [`MAX_NODE_ERRORS`] entries `{node_id, class_type, input, message}`, one per error,
/// whole entries only, at most [`MAX_DETAIL_BYTES`]; otherwise the body cut at that size.
pub fn rejection_detail(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let nodes = parsed
        .as_ref()
        .and_then(|v| v.get("node_errors"))
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty());
    let Some(nodes) = nodes else {
        return cut(body, MAX_DETAIL_BYTES).to_string();
    };
    let text_of = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    let mut entries: Vec<Value> = Vec::new();
    let mut text = "[]".to_string();
    'nodes: for (node_id, node) in nodes {
        let class_type = text_of(node.get("class_type"));
        for error in node
            .get("errors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if entries.len() == MAX_NODE_ERRORS {
                break 'nodes;
            }
            entries.push(json!({
                "node_id": node_id,
                "class_type": class_type,
                "input": text_of(error.get("extra_info").and_then(|e| e.get("input_name"))),
                "message": text_of(error.get("message")),
            }));
            let next = serde_json::to_string(&entries).expect("JSON values serialise");
            if next.len() > MAX_DETAIL_BYTES {
                entries.pop();
                break 'nodes;
            }
            text = next;
        }
    }
    text
}

/// The suite's own prompts in ComfyUI, by job (cancel, the `CancelEnginePrompts` step,
/// `engines.status`'s `suite_job`).
#[derive(Debug, Default)]
pub(crate) struct SuitePrompts {
    live: Mutex<HashMap<String, Arc<Live>>>,
}

/// One running `comfy.prompt` attempt.
#[derive(Debug, Default)]
pub(crate) struct Live {
    /// Held across the POST and across a cancel: a prompt is cancelled only after it was
    /// posted, and never posted once cancelled.
    posted: tokio::sync::Mutex<Posted>,
    /// The shutdown step cancelled it: the job ends interrupted.
    shutdown: AtomicBool,
}

#[derive(Debug, Default)]
struct Posted {
    prompt_id: Option<String>,
    cancel_sent: bool,
}

impl SuitePrompts {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Live>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enter(self: &Arc<Self>, job: &JobId) -> LiveGuard {
        let live = Arc::new(Live::default());
        self.lock().insert(job.to_string(), live.clone());
        LiveGuard {
            prompts: self.clone(),
            job: job.to_string(),
            live,
        }
    }

    fn get(&self, job: &JobId) -> Option<Arc<Live>> {
        self.lock().get(job.as_str()).cloned()
    }

    /// The job whose prompt is in ComfyUI now, if any.
    pub(crate) fn current(&self) -> Option<String> {
        self.lock().keys().next().cloned()
    }

    /// The `CancelEnginePrompts` step: every posted suite prompt gets its per-job cancel once.
    pub(crate) async fn cancel_all(&self, client: &ComfyClient) {
        let live: Vec<(String, Arc<Live>)> = self
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (job, live) in live {
            live.shutdown.store(true, Ordering::SeqCst);
            send_cancel(&live, client, &job).await;
        }
    }
}

/// Posts the per-job cancel of `live`'s prompt, once, after its POST.
async fn send_cancel(live: &Live, client: &ComfyClient, job: &str) {
    let mut posted = live.posted.lock().await;
    let Some(prompt_id) = posted.prompt_id.clone() else {
        return;
    };
    if posted.cancel_sent {
        return;
    }
    posted.cancel_sent = true;
    match client.cancel(&prompt_id).await {
        Ok(()) => tracing::info!(
            job_id = job,
            prompt_id,
            "the suite's prompt was cancelled in ComfyUI"
        ),
        Err(e) => tracing::warn!(job_id = job, prompt_id, error = %e, "the per-job cancel failed"),
    }
}

/// Removes the attempt from [`SuitePrompts`] when it ends.
struct LiveGuard {
    prompts: Arc<SuitePrompts>,
    job: String,
    live: Arc<Live>,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        let mut live = self.prompts.lock();
        if live
            .get(&self.job)
            .is_some_and(|l| Arc::ptr_eq(l, &self.live))
        {
            live.remove(&self.job);
        }
    }
}

/// Runs `comfy.prompt` jobs (module docs).
#[derive(Debug)]
pub struct ComfyPromptHandler {
    engines: Weak<Engines>,
}

impl ComfyPromptHandler {
    pub fn new(engines: Weak<Engines>) -> ComfyPromptHandler {
        ComfyPromptHandler { engines }
    }
}

fn error(spec: &CodeSpec, message: impl Into<String>, job: &JobCtx) -> ErrorBody {
    ErrorBody::new(spec, message, job.trace_id.clone())
}

fn untrusted(mut e: ErrorBody, text: &str) -> ErrorBody {
    e.detail = Some(Detail {
        text: cut(text, MAX_DETAIL_BYTES).to_string(),
        untrusted: true,
    });
    e
}

/// The failure of a request to ComfyUI that didn't answer: retried with backoff.
fn unreachable(e: &ComfyError, job: &JobCtx) -> JobFailure {
    JobFailure::Retryable(error(
        &codes::COMFYUI_UNREACHABLE,
        format!("ComfyUI did not answer the job's request: {e}"),
        job,
    ))
}

impl JobHandler for ComfyPromptHandler {
    fn kind(&self) -> &'static str {
        PROMPT_KIND
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(async move {
            let Some(engines) = self.engines.upgrade() else {
                return Err(JobFailure::Final(error(
                    &codes::CORE_BUG,
                    "The engines are gone. This is a bug in LILITH.",
                    job,
                )));
            };
            let args: PromptArgs = serde_json::from_value(job.spec.args.clone()).map_err(|e| {
                JobFailure::Final(error(
                    &codes::PARAMS_INVALID,
                    format!("comfy.prompt arguments: {e}"),
                    job,
                ))
            })?;
            let comfy = engines.comfy().clone();
            let endpoint = engines
                .ensure_ready(EngineKind::ComfyUi)
                .await
                .map_err(|mut e| {
                    e.trace_id.clone_from(&job.trace_id);
                    if e.code == codes::COMFYUI_UNREACHABLE.code {
                        JobFailure::Retryable(e)
                    } else {
                        JobFailure::Final(e)
                    }
                })?;
            let client = comfy.prompt_client(|| job.uuid()).await.map_err(|mut e| {
                e.trace_id.clone_from(&job.trace_id);
                JobFailure::Final(e)
            })?;
            let guard = comfy.prompts().enter(&job.id);
            let mut attempt = Attempt {
                job,
                comfy: &comfy,
                client,
                uploads: Uploads::new(comfy.suite_state(), comfy.clock.clone()),
                pid: endpoint.pid.unwrap_or(0),
                endpoint,
                live: guard.live.clone(),
                prefix: String::new(),
                uploaded: Vec::new(),
                node: None,
                started: None,
            };
            let result = attempt.run(args).await;
            for name in &attempt.uploaded {
                attempt.uploads.release(name).await;
            }
            drop(guard);
            result
        })
    }

    fn cancel_in_engine<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let Some(engines) = self.engines.upgrade() else {
                return;
            };
            let comfy = engines.comfy();
            let (Some(live), Some(client)) = (comfy.prompts().get(&job.id), comfy.client()) else {
                return;
            };
            send_cancel(&live, client, job.id.as_str()).await;
        })
    }
}

/// One attempt of one job.
struct Attempt<'a> {
    job: &'a JobCtx,
    comfy: &'a Arc<Comfy>,
    client: ComfyClient,
    uploads: Uploads,
    endpoint: ComfyEndpoint,
    pid: u32,
    live: Arc<Live>,
    /// The job's filename prefix.
    prefix: String,
    /// The uploads this attempt holds a reference to.
    uploaded: Vec<String>,
    /// The node `executing` names (progress events carry it).
    node: Option<String>,
    /// When the prompt's `execution_start` came (the core's monotonic clock).
    started: Option<Duration>,
}

/// How a followed prompt ended, before its outcome is made.
enum Ended {
    Success(Option<Value>),
    Error {
        exception_type: String,
        message: String,
    },
    Interrupted,
    Lost,
}

impl Attempt<'_> {
    fn cancelled(&self) -> bool {
        *self.job.cancel.borrow()
    }

    async fn run(&mut self, args: PromptArgs) -> Result<JobDone, JobFailure> {
        self.prefix = job_prefix(&self.job.id);
        // CORE-120: an earlier attempt's prompt is asked about before anything is posted.
        let earlier = record::get(&self.job.state, self.job.id.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|r| r.external_id);
        if let Some(prompt_id) = earlier {
            match self.client.state(&prompt_id).await {
                Ok(state @ (RemoteState::Queued | RemoteState::Running)) => {
                    tracing::info!(
                        prompt_id,
                        "the job's earlier prompt is still in ComfyUI; it is followed"
                    );
                    self.live.posted.lock().await.prompt_id = Some(prompt_id.clone());
                    if state == RemoteState::Running {
                        // Its execution_start went by unseen: the watchdog starts now.
                        self.started = Some(self.comfy.clock.monotonic());
                        self.job.set_substatus(Substatus::Executing).await;
                    } else {
                        self.job.set_substatus(Substatus::QueuedInEngine).await;
                    }
                    return self.follow(&prompt_id, None).await;
                }
                Ok(RemoteState::Done(history)) if succeeded(&history) => {
                    tracing::info!(
                        prompt_id,
                        "the job's earlier prompt succeeded; its outputs are taken"
                    );
                    self.live.posted.lock().await.prompt_id = Some(prompt_id.clone());
                    return self.ingest(&prompt_id, Some(history)).await;
                }
                Ok(_) => {}
                Err(e) => return Err(unreachable(&e, self.job)),
            }
        }
        let mut graph = args.graph;
        if !graph.is_object() {
            return Err(JobFailure::Final(error(
                &codes::PARAMS_INVALID,
                "comfy.prompt's graph must be an API-format object of nodes",
                self.job,
            )));
        }
        self.prefix = prefix_outputs(&mut graph, &self.job.id);
        for input in &args.inputs {
            let name = self
                .uploads
                .acquire(&self.client, &input.path, self.endpoint.root.as_deref())
                .await
                .map_err(|e| match e {
                    ComfyError::Io(why) => JobFailure::Final(error(
                        &codes::STORE_IO,
                        format!("A job input can't be read: {why}"),
                        self.job,
                    )),
                    other => unreachable(&other, self.job),
                })?;
            self.uploaded.push(name.clone());
            match graph
                .get_mut(&input.node)
                .and_then(|n| n.get_mut("inputs"))
                .and_then(Value::as_object_mut)
            {
                Some(inputs) => {
                    inputs.insert(input.input.clone(), Value::from(name));
                }
                None => {
                    return Err(JobFailure::Final(error(
                        &codes::PARAMS_INVALID,
                        format!("the graph has no node {:?} with inputs", input.node),
                        self.job,
                    )));
                }
            }
        }
        // Open before the POST, so no event of the prompt is missed.
        let socket = self
            .client
            .connect_ws()
            .await
            .map_err(|e| unreachable(&e, self.job))?;
        let prompt_id = self.job.uuid();
        // Persisted before it is sent (CORE-120).
        self.job
            .set_external_id(&prompt_id)
            .await
            .map_err(JobFailure::Final)?;
        let method = args
            .preview_method
            .as_deref()
            .unwrap_or(DEFAULT_PREVIEW_METHOD);
        self.post(&prompt_id, &graph, method).await?;
        self.job.failpoint("jobs.after_post").map_err(|e| {
            JobFailure::Final(error(
                &codes::STORE_IO,
                format!("a fail point stopped the job: {e}"),
                self.job,
            ))
        })?;
        self.job.set_substatus(Substatus::QueuedInEngine).await;
        self.follow(&prompt_id, Some(socket)).await
    }

    /// `POST /prompt`, under the cancel lock; a POST that may not have arrived is checked for
    /// before it is posted once more (CORE-120).
    async fn post(&self, prompt_id: &str, graph: &Value, method: &str) -> Result<(), JobFailure> {
        let mut posted = self.live.posted.lock().await;
        if self.cancelled() {
            return Err(JobFailure::Final(cancelled_error(&self.job.trace_id)));
        }
        for resubmit in [false, true] {
            if resubmit {
                tracing::warn!(
                    prompt_id,
                    "ComfyUI doesn't have the prompt; it is posted once more"
                );
            }
            let failed = match self
                .client
                .submit(prompt_id, graph, method, &self.job.trace_id)
                .await
            {
                Ok(()) => {
                    posted.prompt_id = Some(prompt_id.to_string());
                    return Ok(());
                }
                Err(ComfyError::Http(400, body)) => {
                    return Err(JobFailure::Final(untrusted(
                        error(
                            &codes::COMFYUI_EXECUTION_FAILED,
                            "ComfyUI refused the prompt: the detail lists the nodes it found \
                             wrong.",
                            self.job,
                        ),
                        &rejection_detail(&body),
                    )));
                }
                Err(ComfyError::Http(status, body)) if status != 429 && status < 500 => {
                    return Err(JobFailure::Final(untrusted(
                        error(
                            &codes::COMFYUI_EXECUTION_FAILED,
                            format!("ComfyUI refused the prompt with HTTP {status}."),
                            self.job,
                        ),
                        &body,
                    )));
                }
                Err(e) => e,
            };
            // It may have arrived: ask before anything else.
            match self.client.state(prompt_id).await {
                Ok(RemoteState::Gone) => {}
                Ok(_) => {
                    tracing::info!(prompt_id, error = %failed, "the POST failed but ComfyUI has the prompt");
                    posted.prompt_id = Some(prompt_id.to_string());
                    return Ok(());
                }
                Err(_) if self.job.spec.external_side_effects => {
                    return Err(JobFailure::Unknown(error(
                        &codes::COMFYUI_UNREACHABLE,
                        format!(
                            "ComfyUI stopped answering while the prompt was sent ({failed}); \
                             whether it ran is unknown."
                        ),
                        self.job,
                    )));
                }
                Err(_) => return Err(unreachable(&failed, self.job)),
            }
            if resubmit {
                return Err(unreachable(&failed, self.job));
            }
        }
        unreachable!("the loop returns")
    }

    /// Follows the prompt until it ends.
    async fn follow(
        &mut self,
        prompt_id: &str,
        socket: Option<ComfySocket>,
    ) -> Result<JobDone, JobFailure> {
        let mut socket = match socket {
            Some(socket) => Some(socket),
            None => self.client.connect_ws().await.ok(),
        };
        let mut poll = tokio::time::interval(POLL_EVERY);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        poll.tick().await;
        let mut cancel = self.job.cancel.clone();
        let mut silent_since: Option<tokio::time::Instant> = None;
        let ended = loop {
            if self.cancelled() {
                return self.cancel_wait(prompt_id, socket).await;
            }
            tokio::select! {
                event = next_event(&mut socket) => match event {
                    None => {
                        socket = None;
                        poll.reset_immediately();
                    }
                    Some(event) => {
                        if let Some(ended) = self.on_event(prompt_id, event).await {
                            break ended;
                        }
                    }
                },
                _ = poll.tick() => match self.client.state(prompt_id).await {
                    Ok(RemoteState::Queued | RemoteState::Running) => {
                        silent_since = None;
                        if socket.is_none() {
                            socket = self.client.connect_ws().await.ok();
                        }
                    }
                    Ok(RemoteState::Done(history)) => break ended_from_history(history),
                    Ok(RemoteState::Gone) => break Ended::Lost,
                    Err(e) => {
                        let since = *silent_since.get_or_insert_with(tokio::time::Instant::now);
                        tracing::warn!(prompt_id, error = %e, "ComfyUI doesn't answer about the job's prompt");
                        if since.elapsed() >= LOST_AFTER {
                            break Ended::Lost;
                        }
                    }
                },
                () = cancel_set(&mut cancel) => {}
            }
        };
        self.ended(prompt_id, ended).await
    }

    /// One websocket event of the followed prompt; `Some` once it ended.
    async fn on_event(&mut self, prompt_id: &str, event: ComfyEvent) -> Option<Ended> {
        if event.prompt_id().is_some_and(|p| p != prompt_id) {
            return None;
        }
        match event {
            ComfyEvent::ExecutionStart { .. } => {
                self.started = Some(self.comfy.clock.monotonic());
                self.job.set_substatus(Substatus::Executing).await;
            }
            ComfyEvent::Executing { node, .. } => self.node = node,
            ComfyEvent::Progress { value, max, .. } => {
                self.job.progress(value, max, self.node.as_deref());
            }
            ComfyEvent::Executed { .. } => {}
            ComfyEvent::Success { .. } => return Some(Ended::Success(None)),
            ComfyEvent::Error {
                exception_type,
                message,
                ..
            } => {
                return Some(Ended::Error {
                    exception_type,
                    message,
                });
            }
            ComfyEvent::Interrupted { .. } => return Some(Ended::Interrupted),
            ComfyEvent::Preview { format, bytes } => {
                if bytes.len() <= MAX_PREVIEW_BYTES {
                    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    self.job.events.publish(
                        &format!("preview:{}", self.job.id),
                        json!({"format": format.wire(), "data_b64": data_b64}),
                    );
                } else {
                    tracing::debug!(bytes = bytes.len(), "a preview over 1 MB is dropped");
                }
            }
        }
        None
    }

    /// The job was cancelled: the per-job cancel is (being) sent by `cancel_in_engine`; the
    /// attempt ends once ComfyUI no longer has the prompt queued or running, or after
    /// [`CANCEL_WAIT`].
    async fn cancel_wait(
        &mut self,
        prompt_id: &str,
        mut socket: Option<ComfySocket>,
    ) -> Result<JobDone, JobFailure> {
        // `cancel_in_engine` sends it; should it have come before the prompt was known (an
        // earlier attempt's prompt being taken up), it is sent here, still once.
        send_cancel(&self.live, &self.client, self.job.id.as_str()).await;
        let deadline = tokio::time::Instant::now() + CANCEL_WAIT;
        loop {
            tokio::select! {
                event = next_event(&mut socket) => match event {
                    None => socket = None,
                    Some(event) => {
                        if event.prompt_id() == Some(prompt_id)
                            && matches!(event, ComfyEvent::Success { .. }
                                | ComfyEvent::Error { .. }
                                | ComfyEvent::Interrupted { .. })
                        {
                            break;
                        }
                    }
                },
                () = tokio::time::sleep(Duration::from_millis(250)) => {
                    if !matches!(
                        self.client.state(prompt_id).await,
                        Ok(RemoteState::Queued | RemoteState::Running)
                    ) {
                        break;
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    tracing::warn!(prompt_id, "ComfyUI still has the cancelled job's prompt");
                    break;
                }
            }
        }
        Err(JobFailure::Final(cancelled_error(&self.job.trace_id)))
    }

    fn window(&self, ok: bool) -> Vec<ExecWindow> {
        self.started
            .map(|start| ExecWindow {
                engine_pid: self.pid,
                start,
                end: self.comfy.clock.monotonic(),
                ok,
            })
            .into_iter()
            .collect()
    }

    async fn ended(&mut self, prompt_id: &str, ended: Ended) -> Result<JobDone, JobFailure> {
        match ended {
            Ended::Success(history) => self.ingest(prompt_id, history).await,
            Ended::Error {
                exception_type,
                message,
            } => Err(self.engine_error(&exception_type, &message).await),
            Ended::Interrupted if self.live.shutdown.load(Ordering::SeqCst) => Err(self
                .lost(prompt_id, "the core stopped during the job")
                .await),
            Ended::Interrupted if self.cancelled() => {
                Err(JobFailure::Final(cancelled_error(&self.job.trace_id)))
            }
            Ended::Interrupted => Err(JobFailure::Final(error(
                &codes::COMFYUI_EXECUTION_FAILED,
                "The prompt was cancelled from outside the suite; it is not run again.",
                self.job,
            ))),
            Ended::Lost => Err(self
                .lost(
                    prompt_id,
                    "ComfyUI restarted or stopped during the job and lost its prompt",
                )
                .await),
        }
    }

    /// An `execution_error` (ARCH-060, ARCH-072).
    async fn engine_error(&self, exception_type: &str, message: &str) -> JobFailure {
        let text = format!("{exception_type}: {message}");
        if !is_oom(exception_type, message) {
            return JobFailure::Final(untrusted(
                error(
                    &codes::COMFYUI_EXECUTION_FAILED,
                    "ComfyUI could not run the prompt; the detail is ComfyUI's own text.",
                    self.job,
                ),
                &text,
            ));
        }
        let e = untrusted(
            error(
                &codes::COMFYUI_OUT_OF_MEMORY,
                "ComfyUI ran out of GPU memory.",
                self.job,
            ),
            &text,
        );
        if self.job.attempt > OOM_RETRIES {
            return JobFailure::Final(e);
        }
        // Free, then read the VRAM again, then retry (the runner's backoff).
        match self.client.free(true, true).await {
            Ok(()) => match self.client.system_stats().await {
                Ok(stats) => tracing::info!(
                    vram_free = %stats["devices"][0]["vram_free"],
                    "ComfyUI freed its memory after running out; the job is retried"
                ),
                Err(e) => tracing::warn!(error = %e, "the VRAM could not be read after /free"),
            },
            Err(e) => {
                tracing::warn!(error = %e, "ComfyUI's /free after running out of memory failed")
            }
        }
        JobFailure::Retryable(e)
    }

    /// The prompt is gone: the job is interrupted, with what it wrote salvaged by its prefix
    /// (when `root` is known).
    async fn lost(&self, prompt_id: &str, why: &str) -> JobFailure {
        tracing::warn!(prompt_id, "{why}");
        let mut salvaged = Vec::new();
        if let Some(root) = &self.endpoint.root {
            match Staging::for_job(self.job, self.comfy.local_dir()).await {
                Ok(staging) => salvaged = salvage::salvage(root, &self.prefix, &staging).await,
                Err(e) => {
                    tracing::warn!(code = %e.code, error = %e.message, "nowhere to salvage the job's outputs");
                }
            }
        }
        JobFailure::Interrupted {
            error: error(&codes::COMFYUI_UNREACHABLE, format!("{why}."), self.job),
            salvaged,
        }
    }

    /// The outputs of a prompt that succeeded (module docs, step 4).
    async fn ingest(
        &mut self,
        prompt_id: &str,
        history: Option<Value>,
    ) -> Result<JobDone, JobFailure> {
        let windows = self.window(true);
        self.job.set_substatus(Substatus::Ingesting).await;
        let history = match history {
            Some(history) => history,
            None => self.wait_history(prompt_id).await?,
        };
        let files = output_files(&history);
        let staging = Staging::for_job(self.job, self.comfy.local_dir())
            .await
            .map_err(JobFailure::Final)?;
        let mut staged: Vec<(OutputFile, PathBuf, String)> = Vec::new();
        let mut bad: Option<String> = None;
        for file in files {
            let dest = staging.dir.join(&file.filename);
            match self.client.view_to(&file, &dest).await {
                Ok((size, sha256)) => {
                    let read_back = sha256_file(&dest).ok();
                    let checked = self.job.failpoint("jobs.after_outputs");
                    staged.push((file.clone(), dest, sha256.clone()));
                    if checked.is_err() || read_back != Some((size, sha256)) {
                        bad = Some(format!("{} does not match ComfyUI's copy", file.filename));
                        break;
                    }
                }
                Err(e) => {
                    bad = Some(format!("{} could not be fetched: {e}", file.filename));
                    break;
                }
            }
        }
        if let Some(why) = bad {
            for (_, dest, _) in &staged {
                let _ = fsops::discard_unverified(&staging.dir, dest);
            }
            return Err(JobFailure::Final(error(
                &codes::STORE_IO,
                format!(
                    "The job's outputs could not be staged ({why}); ComfyUI's copies are kept. \
                     Retry the job."
                ),
                self.job,
            )));
        }
        // Verified: now the ComfyUI-side copies go.
        if let Some(root) = &self.endpoint.root {
            for (file, _, _) in &staged {
                let at = root
                    .join(&file.kind)
                    .join(&file.subfolder)
                    .join(&file.filename);
                if let Err(e) = fsops::remove_engine_file(root, &at) {
                    tracing::warn!(file = %at.display(), error = %e, "an output's ComfyUI copy could not be removed");
                }
            }
        }
        let artifacts: Vec<FileRef> = staged
            .iter()
            .map(|(file, _, sha256)| staging.file_ref(&file.filename, sha256))
            .collect();
        let executed = history
            .get("prompt")
            .and_then(|p| p.get(2))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(JobDone {
            structured: json!({
                "prompt_id": prompt_id,
                "outputs": artifacts.len(),
                "executed_graph": executed,
            }),
            artifacts,
            record: None,
            windows,
        })
    }

    /// The `/history` entry of a prompt that just ended (it may come a moment after the event).
    async fn wait_history(&self, prompt_id: &str) -> Result<Value, JobFailure> {
        let deadline = tokio::time::Instant::now() + HISTORY_WAIT;
        loop {
            match self.client.history(prompt_id).await {
                Ok(Some(history)) => return Ok(history),
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok(None) => {
                    return Err(JobFailure::Final(error(
                        &codes::COMFYUI_EXECUTION_FAILED,
                        "ComfyUI reported the prompt done but has no history of it.",
                        self.job,
                    )));
                }
                Err(e) => return Err(unreachable(&e, self.job)),
            }
        }
    }
}

/// The next event of `socket`; never ready without one. `None` once it closed.
async fn next_event(socket: &mut Option<ComfySocket>) -> Option<ComfyEvent> {
    match socket {
        Some(socket) => socket.next().await,
        None => std::future::pending().await,
    }
}

/// Whether a `/history` entry is a success.
fn succeeded(history: &Value) -> bool {
    history["status"]["status_str"] == "success" || history["status"]["completed"] == true
}

/// How a prompt ended, from its `/history` entry.
fn ended_from_history(history: Value) -> Ended {
    if succeeded(&history) {
        return Ended::Success(Some(history));
    }
    let messages = history["status"]["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for m in messages.iter().rev() {
        match m.get(0).and_then(Value::as_str) {
            Some("execution_error") => {
                let data = &m[1];
                return Ended::Error {
                    exception_type: data["exception_type"].as_str().unwrap_or("").to_string(),
                    message: data["exception_message"].as_str().unwrap_or("").to_string(),
                };
            }
            Some("execution_interrupted") => return Ended::Interrupted,
            _ => {}
        }
    }
    Ended::Error {
        exception_type: String::new(),
        message: "ComfyUI recorded the prompt as failed without an error message".to_string(),
    }
}

/// The output files of a `/history` entry: every `{filename, subfolder, type: "output"}` under
/// a node's outputs, in node order.
fn output_files(history: &Value) -> Vec<OutputFile> {
    let mut files = Vec::new();
    for node in history["outputs"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        for list in node.as_object().into_iter().flat_map(|m| m.values()) {
            for item in list.as_array().into_iter().flatten() {
                let (Some(filename), Some(kind)) = (
                    item.get("filename").and_then(Value::as_str),
                    item.get("type").and_then(Value::as_str),
                ) else {
                    continue;
                };
                if kind != "output" || filename.contains(['/', '\\']) || filename.is_empty() {
                    continue;
                }
                files.push(OutputFile {
                    filename: filename.to_string(),
                    subfolder: item
                        .get("subfolder")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    kind: kind.to_string(),
                });
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-130
    fn every_save_node_gets_the_job_prefix() {
        let mut graph = json!({
            "1": {"class_type": "EmptyImage", "inputs": {"width": 8}},
            "2": {"class_type": "SaveImage", "inputs": {"filename_prefix": "ComfyUI"}},
            "3": {"class_type": "SaveAnimatedWEBP", "inputs": {"filename_prefix": "x/y"}},
            "4": {"class_type": "SaveImage", "inputs": {"filename_prefix": ["5", 0]}},
        });
        let prefix = prefix_outputs(&mut graph, &JobId("j_0123456789abcdef".into()));
        assert_eq!(prefix, "lil_0123456789abcdef_");
        assert_eq!(graph["2"]["inputs"]["filename_prefix"], prefix.as_str());
        assert_eq!(graph["3"]["inputs"]["filename_prefix"], prefix.as_str());
        // A linked input is not a prefix of ours to set.
        assert_eq!(graph["4"]["inputs"]["filename_prefix"], json!(["5", 0]));
        assert_eq!(graph["1"]["inputs"], json!({"width": 8}));
    }

    #[test] // spec: ARCH-072
    fn out_of_memory_is_recognised_by_type_or_message() {
        assert!(is_oom("torch.OutOfMemoryError", "anything"));
        assert!(is_oom(
            "RuntimeError",
            "CUDA out of memory. Tried to allocate"
        ));
        assert!(!is_oom("RuntimeError", "out of memory"));
    }

    #[test] // spec: ARCH-060
    fn rejection_detail_keeps_whole_entries_within_2048_bytes() {
        let body =
            |nodes: Vec<(String, String)>| {
                let map: serde_json::Map<String, Value> = nodes
                .into_iter()
                .map(|(id, msg)| {
                    (id, json!({"class_type": "KSampler", "errors": [{"type": "t", "message": msg,
                        "extra_info": {"input_name": "seed"}}]}))
                })
                .collect();
                json!({"error": {"type": "prompt_outputs_failed_validation"}, "node_errors": map})
                    .to_string()
            };
        let many = rejection_detail(&body(
            (1..=25).map(|i| (i.to_string(), "m".into())).collect(),
        ));
        assert_eq!(serde_json::from_str::<Vec<Value>>(&many).unwrap().len(), 20);
        let big = "x".repeat(900);
        let three = rejection_detail(&body(
            (1..=3).map(|i| (i.to_string(), big.clone())).collect(),
        ));
        assert!(three.len() <= MAX_DETAIL_BYTES, "{}", three.len());
        assert_eq!(serde_json::from_str::<Vec<Value>>(&three).unwrap().len(), 2);
        // No node errors: the body itself, cut on a character boundary.
        let plain = format!("{{\"error\": \"{}\"}}", "é".repeat(2000));
        let cut_text = rejection_detail(&plain);
        assert!(cut_text.len() <= MAX_DETAIL_BYTES && plain.starts_with(&cut_text));
    }
}
