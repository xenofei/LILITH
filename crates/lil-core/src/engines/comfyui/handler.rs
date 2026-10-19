//! The `comfy.prompt` jobs (Appendix F "ComfyUI client", "Retries and recovery"; ARCH-021,
//! ARCH-033, ARCH-060, ARCH-069, ARCH-072, ARCH-080, CORE-065, CORE-120, CORE-123, CORE-128,
//! CORE-130, QA-009): one ComfyUI prompt per job, in the `gpu-heavy` lane, so the suite has at
//! most one prompt pending in ComfyUI at a time (CORE-123). Arguments `{graph,
//! preview_method?, inputs?}`; an input `{node, input, path}` is a file inside the job's
//! workspace or its cache folder ([`confine_input`]), uploaded once by content hash
//! ([`super::uploads`]); its upload name becomes `graph[node].inputs[input]`.
//!
//! An attempt:
//! 1. **What an earlier attempt sent** (CORE-120). The `jobs` row's `external_id` is read back
//!    (and is known to the shutdown step at once): a prompt ComfyUI still has is followed, one
//!    that succeeded is ingested from `/history`. Any other is replaced by a new prompt for a
//!    local job; a job with **external side effects** (paid or hosted work) ends `Unknown`
//!    (`interrupted_unknown`) instead and is never posted again (CORE-128, ruling P3-R21).
//! 2. **A new prompt.** The graph is checked first ([`super::graph`]): partner nodes are
//!    `comfyui.partner_nodes` (until plan 6b), model files missing from the refreshed
//!    inventory `comfyui.execution_failed`, both before anything is uploaded or posted. Every
//!    `SaveImage`-type node's `filename_prefix` (linked or not) becomes
//!    `lil_<first 16 hex of the job id>_` ([`prefix_outputs`]); the inputs are checked, then
//!    uploaded; the websocket is opened; a UUID is minted and persisted as the row's
//!    `external_id` (the row is `submitting` already) before `POST /prompt` with the channel's
//!    stable `client_id` and `extra_data.preview_method` (default `"taesd"`). A POST that
//!    fails without an answer, or with 429 or 5xx, is followed by a look at `/api/jobs/{id}`
//!    and `/history/{id}` before anything else: a prompt that arrived is followed. It is posted
//!    once more only when it provably wasn't sent (no connection, or 429/503), or, for a local
//!    job, when the look finds it gone; otherwise a job with external side effects ends
//!    `Unknown`. A `400` fails the job at once with `comfyui.execution_failed` and the reply's
//!    node errors as untrusted detail ([`rejection_detail`]).
//! 3. **Following it**: `queued_in_engine` once posted, `executing` at `execution_start` (the
//!    watchdog's start), progress from `progress`, previews of at most 1 MB while it executes
//!    on `preview:<job_id>` as `{format, data_b64}`. The websocket is backed by a look at the
//!    prompt every 2 s. ComfyUI not answering is never taken for a lost prompt, except when
//!    the core's own (owned) engine has stopped.
//! 4. **The end.** Success: `ingesting`, the outputs from `/history` (names checked first,
//!    [`safe_name`]) streamed through `/view` into the staging handle
//!    ([`super::salvage::Staging`]), each verified (size, and the staged file's SHA-256 equal to
//!    the stream's); only then, with `root` known, each ComfyUI-side copy **with the job's
//!    prefix** is removed. The executed graph comes from `/history`. Every attempt that
//!    executed reports its window (from `execution_start`, with the engine's processes) through
//!    [`JobCtx::add_window`], whatever its outcome (CORE-065); a success's is also in
//!    [`JobDone::windows`]. An `execution_error` is `comfyui.execution_failed` with ComfyUI's
//!    text as untrusted detail (ARCH-060); CUDA out of memory ([`is_oom`]) posts `/free`,
//!    re-reads the VRAM and, for a local job, is retried while `attempt <= OOM_RETRIES`; it
//!    then fails with `comfyui.out_of_memory` (ARCH-072). An `execution_interrupted`, a prompt
//!    `/api/jobs` reports `cancelled`, or a queued prompt that disappears while the websocket
//!    stays connected (ComfyUI did not restart), when the job didn't ask for it, was an outside
//!    cancel: `comfyui.execution_failed`, "cancelled from outside the suite", never retried
//!    (ARCH-069). A prompt ComfyUI lost in a restart (absent from `/api/jobs`, `/history` and
//!    `/queue` after the websocket closed) makes the job interrupted, with the outputs already
//!    written salvaged by the prefix ([`super::salvage::salvage`]); the runner re-runs an
//!    `agent` job once, unless it has external side effects (CORE-128).
//!
//! **Cancel** ([`JobHandler::cancel_in_engine`]) posts `POST /api/jobs/{id}/cancel` once, never
//! `/interrupt` (ARCH-021); the POST and the cancel take one lock, so a prompt is either never
//! posted or cancelled after it was. The `CancelEnginePrompts` shutdown step
//! ([`SuitePrompts::cancel_all`]) cancels every suite prompt the same way and closes the gate:
//! no attempt starts or posts after it, and such a job ends interrupted. A prompt an attempt
//! abandons (it ends `Unknown`) is cancelled too, best effort, so it never runs on behind the
//! suite's back.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use base64::Engine as _;
use lil_protocol::{CodeSpec, Detail, ErrorBody, FileRef, codes};
use serde::Deserialize;
use serde_json::{Value, json};

use super::client::{ComfyClient, ComfyError, OutputFile, RemoteState};
use super::graph;
use super::salvage::{self, Staging, sha256_file};
use super::supervisor::{Comfy, ComfyEndpoint};
use super::uploads::Uploads;
use super::ws::{ComfyEvent, ComfySocket, MAX_PREVIEW_BYTES};
use crate::engines::{EngineClass, Engines};
use crate::jobs::record;
use crate::jobs::{
    EngineKind, ExecWindow, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobId, JobSpec,
    Lane, Priority, Substatus, cancelled_error,
};
use crate::store::{WsPath, fsops};

/// The job kind.
pub const PROMPT_KIND: &str = "comfy.prompt";
/// How often an out-of-memory failure of a local job is retried (ARCH-072): attempts 1 and 2.
pub const OOM_RETRIES: u32 = 2;
/// `extra_data.preview_method` unless the arguments say otherwise (CORE-130).
pub const DEFAULT_PREVIEW_METHOD: &str = "taesd";
/// The `expected_secs` of a prompt whose submitter gives none (the watchdog's yardstick).
pub const DEFAULT_EXPECTED_SECS: u32 = 300;
/// The `expected_output_bytes` [`prompt_spec`] sets (the disk preflight's estimate); a caller
/// that knows better sets the field itself.
pub const DEFAULT_EXPECTED_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;
/// The most bytes of engine text an error's detail holds.
pub const MAX_DETAIL_BYTES: usize = 2048;
/// The most node errors a rejected prompt's detail lists.
pub const MAX_NODE_ERRORS: usize = 20;
/// How often a followed prompt is looked at besides its websocket.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// How long a still-connected websocket must stay open after its prompt disappeared for the
/// disappearance to count as an outside cancel rather than a restart.
const RESTART_GRACE: Duration = Duration::from_secs(1);
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
    /// The file: an absolute path inside the job's workspace or its cache folder.
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
        expected_output_bytes: DEFAULT_EXPECTED_OUTPUT_BYTES,
        vram_need_mib: None,
        engine: Some(EngineKind::ComfyUi),
        group: None,
        external_side_effects: false,
    }
}

/// Sets every `SaveImage`-type node's `filename_prefix` (every node with a `filename_prefix`
/// input, a string or a link to another node) to the job's prefix and returns it:
/// `lil_<first 16 hex of the job id>_` (CORE-130).
pub fn prefix_outputs(graph: &mut Value, job: &JobId) -> String {
    let prefix = job_prefix(job);
    if let Some(nodes) = graph.as_object_mut() {
        for node in nodes.values_mut() {
            if let Some(slot) = node
                .get_mut("inputs")
                .and_then(|i| i.get_mut("filename_prefix"))
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

/// Whether `name`, a file name ComfyUI reported (untrusted, ARCH-060), is one plain Windows
/// file name: not empty, at most 255 bytes, none of `<>:"/\|?*` or a control character, not
/// ending in `.` or a space, not `.` or `..`, and not a device name (`CON`, `NUL`, `COM1`, …).
/// Nothing is joined to a path before this holds, so a name can't leave its folder.
pub fn safe_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    !device
}

/// Whether `subfolder` (ComfyUI's, `/`-separated) is empty or made only of [`safe_name`]s.
fn safe_subfolder(subfolder: &str) -> bool {
    subfolder.is_empty() || subfolder.split('/').all(safe_name)
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

/// `path` with `root`'s components stripped, comparing names as Windows does (ignoring case);
/// `None` when `path` is not below `root`.
fn below<'p>(path: &'p Path, root: &Path) -> Option<Vec<Component<'p>>> {
    let key = |c: Component<'_>| c.as_os_str().to_string_lossy().to_lowercase();
    let mut rest = path.components();
    for want in root.components() {
        match rest.next() {
            Some(got) if key(got) == key(want) => {}
            _ => return None,
        }
    }
    Some(rest.collect())
}

/// Checks a job input's path (review I3): an absolute path, without `.` or `..`, of a regular
/// file inside the job's workspace (a valid workspace path, not in a root the store keeps to
/// itself, by its long name: either data folder, `.git`, `.trash` or the ledger) or directly in the job's cache folder (`cache_dir`), reached through no
/// link or junction. Returns the reason it is refused.
pub fn confine_input(
    path: &Path,
    workspace: Option<&Path>,
    cache_dir: &Path,
) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("it is not an absolute path".to_string());
    }
    if path
        .components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err("it holds `.` or `..`".to_string());
    }
    let (root, rest) = if let Some(rest) = below(path, cache_dir).filter(|r| r.len() == 1) {
        (cache_dir, rest)
    } else if let Some((root, rest)) =
        workspace.and_then(|root| below(path, root).map(|rest| (root, rest)))
    {
        let names: Vec<String> = rest
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let rel =
            WsPath::parse(&names.join("/")).map_err(|e| format!("not a workspace path: {e:?}"))?;
        // Decided on the long form too, as the store decides (Task 12 re-review N1): an 8.3
        // short name (`LILITH~1`, `GIT~1`, `TRASH~1`) is refused like the long name, and so
        // is every root the store keeps to itself (either data folder, `.git`, `.trash`, the
        // ledger). A long form that can't be read refuses the path.
        crate::store::paths::reserved_on_disk(
            root,
            &rel,
            "uploaded to ComfyUI",
            &lil_platform::fs::long_name,
        )
        .map_err(|e| match e {
            crate::store::StoreError::PathInvalid { why, .. } => why,
            other => other.to_string(),
        })?;
        (root, rest)
    } else {
        return Err("it is not inside the job's workspace or its cache folder".to_string());
    };
    if rest.iter().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("it is not a plain path below its folder".to_string());
    }
    let mut at = root.to_path_buf();
    for (i, part) in rest.iter().enumerate() {
        at.push(part.as_os_str());
        let meta = std::fs::symlink_metadata(lil_platform::fs::long_path(&at))
            .map_err(|e| format!("it can't be read: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err("a link or junction is on its way".to_string());
        }
        let last = i + 1 == rest.len();
        if (last && !meta.is_file()) || (!last && !meta.is_dir()) {
            return Err("it is not a file".to_string());
        }
    }
    Ok(())
}

/// The suite's own prompts in ComfyUI, by job (cancel, the `CancelEnginePrompts` step,
/// `engines.status`'s `suite_job`).
#[derive(Debug, Default)]
pub(crate) struct SuitePrompts {
    live: Mutex<Prompts>,
}

#[derive(Debug, Default)]
struct Prompts {
    jobs: HashMap<String, Arc<Live>>,
    /// Set by the `CancelEnginePrompts` step: no attempt enters, none posts.
    closed: bool,
    /// Every prompt id the suite's attempts took up or minted in this core run, claimed
    /// before the POST: the outside-jobs poll tells the suite's prompts apart by id only.
    ids: HashSet<String>,
}

/// One running `comfy.prompt` attempt.
#[derive(Debug, Default)]
pub(crate) struct Live {
    /// Held across the POST and across a cancel: a prompt is cancelled only after it was
    /// posted, and never posted once cancelled or once the shutdown step ran.
    posted: tokio::sync::Mutex<Posted>,
    /// The shutdown step ran: the job ends interrupted.
    shutdown: AtomicBool,
}

#[derive(Debug, Default)]
struct Posted {
    prompt_id: Option<String>,
    cancel_sent: bool,
}

impl SuitePrompts {
    fn lock(&self) -> std::sync::MutexGuard<'_, Prompts> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers an attempt; `None` once the shutdown step ran.
    fn enter(self: &Arc<Self>, job: &JobId) -> Option<LiveGuard> {
        let live = Arc::new(Live::default());
        let mut prompts = self.lock();
        if prompts.closed {
            return None;
        }
        prompts.jobs.insert(job.to_string(), live.clone());
        Some(LiveGuard {
            prompts: self.clone(),
            job: job.to_string(),
            live,
        })
    }

    fn get(&self, job: &JobId) -> Option<Arc<Live>> {
        self.lock().jobs.get(job.as_str()).cloned()
    }

    /// The job whose prompt is in ComfyUI now, if any.
    pub(crate) fn current(&self) -> Option<String> {
        self.lock().jobs.keys().next().cloned()
    }

    /// Claims `prompt_id` as the suite's, before it is posted or followed.
    pub(crate) fn claim(&self, prompt_id: &str) {
        self.lock().ids.insert(prompt_id.to_string());
    }

    /// Every prompt id the suite claimed in this core run ([`SuitePrompts::claim`]).
    pub(crate) fn prompt_ids(&self) -> HashSet<String> {
        self.lock().ids.clone()
    }

    /// The `CancelEnginePrompts` step: the gate closes (no attempt enters or posts any more),
    /// and every posted suite prompt gets its per-job cancel once.
    pub(crate) async fn cancel_all(&self, client: &ComfyClient) {
        let live: Vec<(String, Arc<Live>)> = {
            let mut prompts = self.lock();
            prompts.closed = true;
            for live in prompts.jobs.values() {
                live.shutdown.store(true, Ordering::SeqCst);
            }
            prompts
                .jobs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        for (job, live) in live {
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

/// How often, and how many times, an abandoned prompt is looked at after its cancel
/// ([`cancel_if_it_lands_late`]).
const LATE_LOOK_EVERY: Duration = Duration::from_secs(2);
const LATE_LOOKS: u32 = 5;

/// Task 12 re-review N2: a prompt ComfyUI was still validating when its per-job cancel came
/// (the cancel found nothing) can still land and run. For about 10 s it is looked at every
/// 2 s, and cancelled once if it shows up queued or running; a prompt that is done or
/// cancelled ends the watch. It runs on its own, after the job has ended.
fn cancel_if_it_lands_late(client: ComfyClient, prompt_id: String, job: String) {
    tokio::spawn(async move {
        for _ in 0..LATE_LOOKS {
            tokio::time::sleep(LATE_LOOK_EVERY).await;
            match client.state(&prompt_id).await {
                Ok(RemoteState::Queued | RemoteState::Running) => {
                    match client.cancel(&prompt_id).await {
                        Ok(()) => tracing::info!(
                            job_id = job,
                            prompt_id,
                            "an abandoned prompt that landed late was cancelled in ComfyUI"
                        ),
                        Err(e) => {
                            tracing::warn!(job_id = job, prompt_id, error = %e, "the late cancel of an abandoned prompt failed")
                        }
                    }
                    return;
                }
                Ok(RemoteState::Done(_) | RemoteState::Cancelled) => return,
                Ok(RemoteState::Gone) | Err(_) => {}
            }
        }
    });
}

/// Removes the attempt from [`SuitePrompts`] when it ends.
struct LiveGuard {
    prompts: Arc<SuitePrompts>,
    job: String,
    live: Arc<Live>,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        let mut prompts = self.prompts.lock();
        if prompts
            .jobs
            .get(&self.job)
            .is_some_and(|l| Arc::ptr_eq(l, &self.live))
        {
            prompts.jobs.remove(&self.job);
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

/// Untrusted detail from a process's last output lines (ComfyUI's at a failed start, comfy-cli's
/// at a failed install; ARCH-060), held to the same [`MAX_DETAIL_BYTES`] as every other engine
/// text, since it lands in the jobs row, its log line and every `job:` event: the newest whole
/// lines that fit, joined by `\n` (the error is usually last), or the end of the last line when
/// that alone is longer. `None` for no output.
pub(crate) fn output_tail_detail(lines: &[String]) -> Option<Detail> {
    let last = lines.last()?;
    let mut used = 0;
    let mut first = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        let len = line.len() + usize::from(i + 1 < lines.len());
        if used + len > MAX_DETAIL_BYTES {
            break;
        }
        used += len;
        first = i;
    }
    let text = if first == lines.len() {
        let mut start = last.len() - MAX_DETAIL_BYTES;
        while !last.is_char_boundary(start) {
            start += 1;
        }
        last[start..].to_string()
    } else {
        lines[first..].join("\n")
    };
    Some(Detail {
        text,
        untrusted: true,
    })
}

/// `e` with `text` (an engine's or the outside world's, ARCH-060) as untrusted detail.
fn untrusted(mut e: ErrorBody, text: &str) -> ErrorBody {
    e.detail = Some(Detail {
        text: cut(text, MAX_DETAIL_BYTES).to_string(),
        untrusted: true,
    });
    e
}

/// The failure of a request to ComfyUI that didn't answer: retried with backoff. What it
/// said is detail, untrusted.
fn unreachable(e: &ComfyError, job: &JobCtx) -> JobFailure {
    JobFailure::Retryable(untrusted(
        error(
            &codes::COMFYUI_UNREACHABLE,
            "ComfyUI did not answer the job's request.",
            job,
        ),
        &e.to_string(),
    ))
}

/// The prompt id an earlier attempt persisted (CORE-120), read from the job's row. A row that
/// can't be read is no answer (Task 12 re-review N3): a job with external side effects ends
/// `Unknown`, since an earlier prompt may have been sent and paid for; any other job is retried
/// later. Never "no earlier prompt", which would post a new one.
async fn earlier_prompt(job: &JobCtx) -> Result<Option<String>, JobFailure> {
    match record::get(&job.state, job.id.as_str()).await {
        Ok(row) => Ok(row.and_then(|r| r.external_id)),
        Err(e) => {
            tracing::error!(job_id = %job.id, error = %e, "the job's row can't be read before its attempt");
            let e = error(
                &codes::STATE_UNAVAILABLE,
                "The job's state can't be read, so whether an earlier attempt sent its prompt \
                 is unknown.",
                job,
            );
            Err(if job.spec.external_side_effects {
                JobFailure::Unknown(e)
            } else {
                JobFailure::Retryable(e)
            })
        }
    }
}

/// The job stops because the core is stopping (the shutdown step ran): interrupted.
fn stopping(job: &JobCtx) -> JobFailure {
    JobFailure::Interrupted {
        error: error(
            &codes::COMFYUI_UNREACHABLE,
            "The core stopped before the prompt was sent.",
            job,
        ),
        salvaged: Vec::new(),
    }
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
            let comfy = engines.comfy().clone();
            // The shutdown step's gate: no attempt starts after it (ARCH-021 step 2).
            let Some(guard) = comfy.prompts().enter(&job.id) else {
                return Err(stopping(job));
            };
            let args: PromptArgs = serde_json::from_value(job.spec.args.clone()).map_err(|e| {
                JobFailure::Final(error(
                    &codes::PARAMS_INVALID,
                    format!("comfy.prompt arguments: {e}"),
                    job,
                ))
            })?;
            let external = job.spec.external_side_effects;
            let endpoint = match engines.ensure_ready(EngineKind::ComfyUi).await {
                Ok(endpoint) => endpoint,
                Err(mut e) => {
                    e.trace_id.clone_from(&job.trace_id);
                    // An earlier prompt with external side effects that can't be looked at is
                    // unknown (fail closed, CORE-128); so is a row that can't be read.
                    let earlier = match earlier_prompt(job).await {
                        Ok(earlier) => earlier.is_some(),
                        Err(failure) if external => return Err(failure),
                        Err(_) => false,
                    };
                    return Err(if external && earlier {
                        JobFailure::Unknown(e)
                    } else if e.code == codes::COMFYUI_UNREACHABLE.code {
                        JobFailure::Retryable(e)
                    } else {
                        JobFailure::Final(e)
                    });
                }
            };
            let client = comfy.prompt_client(|| job.uuid()).await.map_err(|mut e| {
                e.trace_id.clone_from(&job.trace_id);
                JobFailure::Final(e)
            })?;
            let mut attempt = Attempt {
                job,
                comfy: &comfy,
                client,
                uploads: Uploads::new(comfy.suite_state(), comfy.clock.clone()),
                endpoint,
                live: guard.live.clone(),
                external,
                prefix: job_prefix(&job.id),
                uploaded: Vec::new(),
                node: None,
                started: None,
                accepted: false,
                abandoned: false,
            };
            let mut result = attempt.run(args).await;
            // CORE-128: work with external side effects that ComfyUI accepted is never retried
            // automatically; what can't be accounted for is unknown.
            if external
                && attempt.accepted
                && let Err(JobFailure::Retryable(e)) = result
            {
                result = Err(JobFailure::Unknown(e));
            }
            // A prompt the attempt abandons doesn't run on behind the suite's back (M4).
            if matches!(result, Err(JobFailure::Unknown(_))) || attempt.abandoned {
                send_cancel(&attempt.live, &attempt.client, job.id.as_str()).await;
                let prompt_id = attempt.live.posted.lock().await.prompt_id.clone();
                if let Some(prompt_id) = prompt_id {
                    cancel_if_it_lands_late(attempt.client.clone(), prompt_id, job.id.to_string());
                }
            }
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
    live: Arc<Live>,
    /// The job has external side effects (paid or hosted work, CORE-128).
    external: bool,
    /// The job's filename prefix.
    prefix: String,
    /// The uploads this attempt holds a reference to.
    uploaded: Vec<String>,
    /// The node `executing` names (progress events carry it).
    node: Option<String>,
    /// When the prompt's `execution_start` came (the core's monotonic clock).
    started: Option<Duration>,
    /// ComfyUI has (or had) the attempt's prompt.
    accepted: bool,
    /// The attempt leaves its prompt behind (a fail point stopped it after the POST).
    abandoned: bool,
}

/// How a followed prompt ended, before its outcome is made.
enum Ended {
    Success(Option<Value>),
    Error {
        exception_type: String,
        message: String,
    },
    /// Cancelled in ComfyUI: by the job, the shutdown step or an outside client.
    Interrupted,
    /// Lost in a restart (or with the core's own engine).
    Lost,
}

impl Attempt<'_> {
    fn cancelled(&self) -> bool {
        *self.job.cancel.borrow()
    }

    fn shut(&self) -> bool {
        self.live.shutdown.load(Ordering::SeqCst)
    }

    /// `Unknown`: whether the work ran (and was charged) can't be told (CORE-128).
    fn unknown(&self, spec: &CodeSpec, message: &str, text: Option<&str>) -> JobFailure {
        let e = error(spec, message, self.job);
        JobFailure::Unknown(match text {
            Some(text) => untrusted(e, text),
            None => e,
        })
    }

    async fn run(&mut self, args: PromptArgs) -> Result<JobDone, JobFailure> {
        // CORE-120: an earlier attempt's prompt is asked about before anything is posted. A
        // row that can't be read says nothing about one, so nothing is posted (re-review N3:
        // fail closed; paid work may have been sent).
        let earlier = earlier_prompt(self.job).await?;
        if let Some(prompt_id) = earlier {
            // Known to the shutdown step and to a cancel at once (I2), registered before the
            // shutdown check: a step that ran meanwhile missed it, so it is cancelled here
            // (`run` cancels an unknown or abandoned attempt's prompt), and paid work that may
            // be running is unknown, never plain interrupted (re-review N4c, P3-R21).
            self.comfy.prompts().claim(&prompt_id);
            self.live.posted.lock().await.prompt_id = Some(prompt_id.clone());
            if self.shut() {
                if self.external {
                    return Err(self.unknown(
                        &codes::COMFYUI_UNREACHABLE,
                        "The core stopped while it took up the job's earlier prompt, which has \
                         external side effects: whether it ran (and was charged) is unknown.",
                        None,
                    ));
                }
                self.abandoned = true;
                return Err(stopping(self.job));
            }
            match self.client.state(&prompt_id).await {
                Ok(state @ (RemoteState::Queued | RemoteState::Running)) => {
                    tracing::info!(
                        prompt_id,
                        "the job's earlier prompt is still in ComfyUI; it is followed"
                    );
                    self.accepted = true;
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
                    self.accepted = true;
                    return self.ingest(&prompt_id, Some(history)).await;
                }
                _ if self.external => {
                    // Paid or hosted work is never posted again (CORE-128, ruling P3-R21).
                    return Err(self.unknown(
                        &codes::COMFYUI_EXECUTION_FAILED,
                        "The job's earlier prompt didn't finish in ComfyUI, and it has external \
                         side effects: whether it ran (and was charged) is unknown, so it is \
                         not sent again. Check it, then resolve the job.",
                        None,
                    ));
                }
                Ok(_) => {
                    self.live.posted.lock().await.prompt_id = None;
                }
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
        self.check_graph(&graph).await?;
        self.prefix = prefix_outputs(&mut graph, &self.job.id);
        self.upload_inputs(&mut graph, &args.inputs).await?;
        // Open before the POST, so no event of the prompt is missed.
        let socket = self
            .client
            .connect_ws()
            .await
            .map_err(|e| unreachable(&e, self.job))?;
        let prompt_id = self.job.uuid();
        self.comfy.prompts().claim(&prompt_id);
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
        if let Err(e) = self.job.failpoint("jobs.after_post") {
            self.abandoned = true;
            return Err(JobFailure::Final(error(
                &codes::STORE_IO,
                format!("a fail point stopped the job: {e}"),
                self.job,
            )));
        }
        self.job.set_substatus(Substatus::QueuedInEngine).await;
        self.follow(&prompt_id, Some(socket)).await
    }

    /// The graph checks before anything is uploaded or posted (CORE-018, ARCH-057; module
    /// docs of [`super::graph`]), on a copy with ComfyUI's node replacements applied (for the
    /// classes ComfyUI doesn't have, as it applies them; the graph is posted as it came):
    /// partner nodes fail with `comfyui.partner_nodes` (until plan 6b), their classes as
    /// untrusted detail; model references the inventory doesn't hold fail with
    /// `comfyui.execution_failed`, checked only when `root` is known, the inventory was
    /// refreshed since the engine became ready, and `<root>\extra_model_paths.yaml` doesn't
    /// exist (models may live elsewhere then).
    async fn check_graph(&self, graph: &Value) -> Result<(), JobFailure> {
        let info = self
            .comfy
            .graph_info(&self.client)
            .await
            .map_err(|e| unreachable(&e, self.job))?;
        let missing_classes: serde_json::Map<String, Value> = info
            .replacements
            .as_object()
            .into_iter()
            .flatten()
            // As ComfyUI: only for a class it doesn't have, and only to one it has (review M9).
            .filter(|(class, _)| info.object_info.get(class.as_str()).is_none())
            .filter(|(_, entry)| {
                graph::replacement_class(entry)
                    .is_some_and(|new| info.object_info.get(new).is_some())
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut checked = graph.clone();
        for (old, new) in
            graph::apply_node_replacements(&mut checked, &Value::Object(missing_classes))
        {
            tracing::info!(job_id = %self.job.id, old, new, "a node of the graph is replaced as ComfyUI will replace it");
        }
        let partners = graph::partner_nodes(&checked, &info.object_info);
        if !partners.is_empty() {
            return Err(JobFailure::Final(untrusted(
                error(
                    &codes::COMFYUI_PARTNER_NODES,
                    "The graph has partner (paid API) nodes, listed in the detail; the suite \
                     doesn't run them yet (plan 6b). Remove them, or run the graph in \
                     ComfyUI itself.",
                    self.job,
                ),
                &partners.join(", "),
            )));
        }
        let Some(root) = &self.endpoint.root else {
            return Ok(());
        };
        let Some(inventory) = self.comfy.current_inventory() else {
            return Ok(());
        };
        // Models may live elsewhere then (review I2).
        let extra_args = self
            .comfy
            .config()
            .map(|c| c.extra_args.clone())
            .unwrap_or_default();
        if root.join("extra_model_paths.yaml").exists()
            || graph::names_other_model_paths(&extra_args)
            || graph::names_other_model_paths(&inventory.argv)
        {
            return Ok(());
        }
        let refs = graph::model_refs(&checked);
        let mut missing = graph::missing_models(&refs, &inventory.paths());
        if !missing.is_empty() {
            // A model added since the inventory (review I2): the folder is looked at again,
            // names only, before anything is refused.
            let models = root.join("models");
            let now = tokio::task::spawn_blocking(move || {
                super::inventory::list_paths(&models, &lil_platform::fs::SystemProbe)
            })
            .await
            .unwrap_or_default();
            missing = graph::missing_models(&refs, &now);
        }
        if missing.is_empty() {
            return Ok(());
        }
        let shown = graph::lines_within(&missing, MAX_NODE_ERRORS, MAX_DETAIL_BYTES);
        let mut e = untrusted(
            error(
                &codes::COMFYUI_EXECUTION_FAILED,
                format!(
                    "The graph names {} model file(s) that ComfyUI's models folder doesn't \
                     hold (listed in the detail); nothing was sent. engines.status shows the \
                     inventory.",
                    missing.len()
                ),
                self.job,
            ),
            &shown,
        );
        e.fix = Some(lil_protocol::Fix {
            ui_action: lil_protocol::UiAction {
                id: "engines.status".to_string(),
                label: "Show ComfyUI's models".to_string(),
            },
            agent_hint: Some(lil_protocol::AgentHint {
                tool: "engines.status".to_string(),
                arguments: json!({}),
            }),
        });
        Err(JobFailure::Final(e))
    }

    /// Checks every input (its node and its path) before anything is uploaded, then uploads
    /// them and writes their names into the graph.
    async fn upload_inputs(
        &mut self,
        graph: &mut Value,
        inputs: &[PromptInput],
    ) -> Result<(), JobFailure> {
        let invalid =
            |message: String| JobFailure::Final(error(&codes::PARAMS_INVALID, message, self.job));
        let cache = self
            .comfy
            .local_dir()
            .join("cache")
            .join("jobs")
            .join(self.job.id.as_str());
        let workspace = self
            .job
            .workspace
            .as_ref()
            .map(|w| w.store.root().to_path_buf());
        for (i, input) in inputs.iter().enumerate() {
            if graph
                .get(&input.node)
                .and_then(|n| n.get("inputs"))
                .is_none_or(|i| !i.is_object())
            {
                return Err(invalid(format!(
                    "inputs[{i}]: the graph has no node {:?} with inputs",
                    input.node
                )));
            }
            confine_input(&input.path, workspace.as_deref(), &cache).map_err(|why| {
                JobFailure::Final(untrusted(
                    error(
                        &codes::PARAMS_INVALID,
                        format!(
                            "inputs[{i}].path is refused: an input is a file inside the job's \
                             workspace or its cache folder ({why})."
                        ),
                        self.job,
                    ),
                    &input.path.display().to_string(),
                ))
            })?;
        }
        for input in inputs {
            let name = self
                .uploads
                .acquire(&self.client, &input.path, self.endpoint.root.as_deref())
                .await
                .map_err(|e| match &e {
                    ComfyError::Io(_) => JobFailure::Final(untrusted(
                        error(&codes::STORE_IO, "A job input can't be read.", self.job),
                        &e.to_string(),
                    )),
                    _ => unreachable(&e, self.job),
                })?;
            self.uploaded.push(name.clone());
            if let Some(slot) = graph
                .get_mut(&input.node)
                .and_then(|n| n.get_mut("inputs"))
                .and_then(Value::as_object_mut)
            {
                slot.insert(input.input.clone(), Value::from(name));
            }
        }
        Ok(())
    }

    /// `POST /prompt`, under the cancel lock (module docs, step 2).
    async fn post(
        &mut self,
        prompt_id: &str,
        graph: &Value,
        method: &str,
    ) -> Result<(), JobFailure> {
        let live = self.live.clone();
        let mut posted = live.posted.lock().await;
        if self.shut() {
            return Err(stopping(self.job));
        }
        if self.cancelled() {
            return Err(JobFailure::Final(cancelled_error(&self.job.trace_id)));
        }
        for resubmit in [false, true] {
            if resubmit {
                tracing::warn!(
                    prompt_id,
                    "the prompt didn't reach ComfyUI; it is posted once more"
                );
            }
            let failed = match self
                .client
                .submit(prompt_id, graph, method, &self.job.trace_id)
                .await
            {
                Ok(()) => {
                    posted.prompt_id = Some(prompt_id.to_string());
                    self.accepted = true;
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
            // Provably not processed: no connection, or refused before acceptance (CORE-128).
            let unsent = matches!(
                failed,
                ComfyError::Refused(_) | ComfyError::Http(429 | 503, _)
            );
            // It may have arrived: ask before anything else (CORE-120).
            match self.client.state(prompt_id).await {
                Ok(RemoteState::Gone) | Err(_) if unsent => {}
                Ok(RemoteState::Gone) if !self.external => {}
                Ok(RemoteState::Gone) | Err(_) if self.external => {
                    posted.prompt_id = Some(prompt_id.to_string());
                    return Err(self.unknown(
                        &codes::COMFYUI_UNREACHABLE,
                        "The prompt may have reached ComfyUI, and it has external side effects: \
                         whether it ran (and was charged) is unknown, so it is not sent again.",
                        Some(&failed.to_string()),
                    ));
                }
                Err(_) => return Err(unreachable(&failed, self.job)),
                Ok(_) => {
                    tracing::info!(prompt_id, "the POST failed but ComfyUI has the prompt");
                    posted.prompt_id = Some(prompt_id.to_string());
                    self.accepted = true;
                    return Ok(());
                }
            }
            if resubmit {
                if self.external {
                    // It never left: a later attempt may send a new one.
                    let _ = self.job.clear_external_id().await;
                }
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
        let mut silent = false;
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
                        silent = false;
                        if socket.is_none() {
                            socket = self.client.connect_ws().await.ok();
                        }
                    }
                    Ok(RemoteState::Done(history)) => break ended_from_history(history),
                    Ok(RemoteState::Cancelled) => break Ended::Interrupted,
                    Ok(RemoteState::Gone) => {
                        // A restart closes the websocket; a queued prompt someone cancelled
                        // disappears while it stays connected (I1).
                        break if still_connected(&mut socket).await {
                            Ended::Interrupted
                        } else {
                            Ended::Lost
                        };
                    }
                    Err(e) => {
                        // Silence is not a lost prompt (I4): only the core's own engine
                        // stopping proves that.
                        if !silent {
                            tracing::warn!(prompt_id, error = %e, "ComfyUI doesn't answer about the job's prompt; the job keeps waiting");
                            silent = true;
                        }
                        if self.endpoint.class == EngineClass::Owned
                            && self.comfy.owned_gone(self.endpoint.pid)
                        {
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
            // Only while the job's own prompt executes (M2): before that, a preview is another
            // prompt's.
            ComfyEvent::Preview { format, bytes } if self.started.is_some() => {
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
            ComfyEvent::Preview { .. } => {}
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
        // `cancel_in_engine` sends it; should it have come before the prompt was known, it is
        // sent here, still once.
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
        self.report_window(false);
        Err(JobFailure::Final(cancelled_error(&self.job.trace_id)))
    }

    /// The window from `execution_start` to now, if the prompt started: with the engine's
    /// processes as they are now (the interpreter a venv launcher started included, ARCH-064),
    /// else the endpoint's own process; none when neither is known, so no window ever names a
    /// pid that isn't the engine's.
    fn window(&self, ok: bool) -> Option<ExecWindow> {
        let start = self.started?;
        let mut engine_pids = self.comfy.pids();
        if engine_pids.is_empty() {
            engine_pids.extend(self.endpoint.pid.filter(|p| *p != 0));
        }
        if engine_pids.is_empty() {
            tracing::warn!(job_id = %self.job.id, "the engine's processes are unknown; the job's execution window is not reported");
            return None;
        }
        Some(ExecWindow {
            engine_pids,
            start,
            end: self.comfy.clock.monotonic(),
            ok,
        })
    }

    /// Reports the windows of a run whose prompt succeeded but whose outputs could not be
    /// taken, as not `ok`: the GPU time was spent whatever the outcome (CORE-065, re-review N7).
    fn report_failed_windows(&self, windows: &[ExecWindow]) {
        for w in windows {
            self.job.add_window(ExecWindow {
                ok: false,
                ..w.clone()
            });
        }
    }

    /// Reports the window of a run that didn't succeed (CORE-065).
    fn report_window(&mut self, ok: bool) {
        if let Some(w) = self.window(ok) {
            self.job.add_window(w);
        }
        self.started = None;
    }

    async fn ended(&mut self, prompt_id: &str, ended: Ended) -> Result<JobDone, JobFailure> {
        if !matches!(ended, Ended::Success(_)) {
            self.report_window(false);
        }
        match ended {
            Ended::Success(history) => self.ingest(prompt_id, history).await,
            Ended::Error {
                exception_type,
                message,
            } => Err(self.engine_error(&exception_type, &message).await),
            Ended::Interrupted if self.shut() => Err(self
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
        let retry = !self.external && self.job.attempt <= OOM_RETRIES;
        if !retry && !self.external {
            return JobFailure::Final(e);
        }
        // Free, then read the VRAM again (before a retry; and for paid work, which is never
        // retried on its own, so the engine is usable again).
        match self.client.free(true, true).await {
            Ok(()) => match self.client.system_stats().await {
                Ok(stats) => tracing::info!(
                    vram_free = %stats["devices"][0]["vram_free"],
                    "ComfyUI freed its memory after running out"
                ),
                Err(e) => tracing::warn!(error = %e, "the VRAM could not be read after /free"),
            },
            Err(e) => {
                tracing::warn!(error = %e, "ComfyUI's /free after running out of memory failed")
            }
        }
        // A prompt with external side effects ran (and may have been charged): never again
        // on its own (CORE-128, C1). A local one is retried (the runner's backoff).
        if retry {
            JobFailure::Retryable(e)
        } else {
            JobFailure::Final(e)
        }
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
        let windows: Vec<ExecWindow> = self.window(true).into_iter().collect();
        self.job.set_substatus(Substatus::Ingesting).await;
        let history = match history {
            Some(history) => history,
            None => match self.wait_history(prompt_id).await {
                Ok(history) => history,
                Err(failure) => {
                    self.report_failed_windows(&windows);
                    return Err(failure);
                }
            },
        };
        let staging = match Staging::for_job(self.job, self.comfy.local_dir()).await {
            Ok(staging) => staging,
            Err(e) => {
                self.report_failed_windows(&windows);
                return Err(JobFailure::Final(e));
            }
        };
        let mut staged: Vec<(OutputFile, PathBuf, String)> = Vec::new();
        let mut taken: HashSet<String> = HashSet::new();
        let mut bad: Option<String> = None;
        for file in output_files(&history) {
            // Untrusted names (ARCH-060): checked before anything is joined (I7).
            if !safe_name(&file.filename) || !safe_subfolder(&file.subfolder) {
                tracing::warn!(name = ?file.filename, subfolder = ?file.subfolder, "an output with a name the suite doesn't take is left in ComfyUI");
                continue;
            }
            // Two outputs of one name (in two subfolders) are staged side by side.
            let mut name = file.filename.clone();
            let mut n = 1;
            while !taken.insert(name.to_ascii_lowercase()) {
                n += 1;
                name = format!("{n}_{}", file.filename);
            }
            let dest = staging.dir.join(&name);
            if dest.parent() != Some(staging.dir.as_path()) {
                continue;
            }
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
            self.report_failed_windows(&windows);
            return Err(JobFailure::Final(untrusted(
                error(
                    &codes::STORE_IO,
                    "The job's outputs could not be staged; ComfyUI's copies are kept. Retry \
                     the job.",
                    self.job,
                ),
                &why,
            )));
        }
        // Verified: now the ComfyUI-side copies with the job's prefix go (I5); the others stay.
        if let Some(root) = &self.endpoint.root {
            for (file, _, _) in &staged {
                if !file.filename.starts_with(&self.prefix) {
                    tracing::info!(name = ?file.filename, "an output without the job's prefix is left in ComfyUI's folder");
                    continue;
                }
                let mut at = root.join(&file.kind);
                for part in file.subfolder.split('/').filter(|p| !p.is_empty()) {
                    at.push(part);
                }
                at.push(&file.filename);
                if let Err(e) = fsops::remove_engine_file(root, &at) {
                    tracing::warn!(file = %at.display(), error = %e, "an output's ComfyUI copy could not be removed");
                }
            }
        }
        let artifacts: Vec<FileRef> = staged
            .iter()
            .map(|(_, dest, sha256)| {
                let name = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                staging.file_ref(&name, sha256)
            })
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

/// Whether `socket` stays open for [`RESTART_GRACE`] (events meanwhile are dropped: the
/// prompt they would be about is gone). A closed or missing socket is `false`.
async fn still_connected(socket: &mut Option<ComfySocket>) -> bool {
    if socket.is_none() {
        return false;
    }
    let deadline = tokio::time::Instant::now() + RESTART_GRACE;
    loop {
        tokio::select! {
            event = next_event(socket) => {
                if event.is_none() {
                    *socket = None;
                    return false;
                }
            }
            () = tokio::time::sleep_until(deadline) => return true,
        }
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
/// a node's outputs, in node order (their names are checked by the caller).
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
                if kind != "output" {
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

    /// Task 12 re-review N1: an input inside a root the store keeps to itself is refused by its
    /// long name and by its 8.3 short name (`LILITH~1`, `GIT~1`, `TRASH~1`), as the store
    /// refuses it; a plain workspace file is accepted. On a volume with 8.3 names off the
    /// short-name half is skipped.
    #[test] // spec: CORE-130, ARCH-060
    fn inputs_in_reserved_roots_are_refused_by_long_and_short_name() {
        use lil_platform::names::{LEGACY_WORKSPACE_DIR, WORKSPACE_DIR};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        let cache = dir.path().join("cache");
        let reserved = [
            format!("{WORKSPACE_DIR}/private.png"),
            format!("{LEGACY_WORKSPACE_DIR}/private.png"),
            ".git/config".to_string(),
            ".trash/t.png".to_string(),
            "registry/ledger.csv".to_string(),
        ];
        for rel in reserved
            .iter()
            .chain(std::iter::once(&"art/ok.png".to_string()))
        {
            lil_testkit::files::write_file(&ws.join(rel), b"x");
        }
        assert_eq!(
            confine_input(&ws.join(r"art\ok.png"), Some(&ws), &cache),
            Ok(())
        );
        let mut shorts = 0;
        for rel in &reserved {
            let long = ws.join(rel.replace('/', "\\"));
            assert!(confine_input(&long, Some(&ws), &cache).is_err(), "{rel}");
            let first = ws.join(rel.split('/').next().unwrap());
            let short = lil_platform::fs::short_name(&first).unwrap();
            if short != first {
                shorts += 1;
                let rest = long.strip_prefix(&first).unwrap();
                let by_short = short.join(rest);
                let refused = confine_input(&by_short, Some(&ws), &cache);
                assert!(refused.is_err(), "{}: {refused:?}", by_short.display());
            }
        }
        if shorts == 0 {
            println!("8.3 names are off on this volume: the short-name half is skipped");
        }
    }

    /// QA1 M3: a failed start's or install's output tail is held to the same 2,048 bytes as
    /// every other engine text: the newest whole lines, or the end of a longer last line.
    #[test] // spec: ARCH-060
    fn output_tails_keep_the_newest_lines_within_2048_bytes() {
        assert_eq!(output_tail_detail(&[]), None);
        let lines: Vec<String> = (0..60)
            .map(|i| format!("{i:02}{}", "x".repeat(98)))
            .collect();
        let detail = output_tail_detail(&lines).unwrap();
        assert!(detail.untrusted);
        assert!(
            detail.text.len() <= MAX_DETAIL_BYTES,
            "{}",
            detail.text.len()
        );
        assert_eq!(
            detail.text,
            lines[40..].join("\n"),
            "the newest 20 whole lines"
        );
        let long = vec![
            "first".to_string(),
            format!("é{}end", "y".repeat(16 * 1024)),
        ];
        let detail = output_tail_detail(&long).unwrap();
        assert!(detail.text.len() <= MAX_DETAIL_BYTES && detail.text.ends_with("end"));
    }

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
        // A linked prefix is replaced too (review I5): every output carries the job's prefix.
        for node in ["2", "3", "4"] {
            assert_eq!(graph[node]["inputs"]["filename_prefix"], prefix.as_str());
        }
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

    #[test] // spec: ARCH-060, CORE-130
    fn output_names_that_could_leave_staging_are_refused() {
        for bad in [
            "C:evil.png",
            "a.png:stream",
            "..",
            ".",
            "dir\\x.png",
            "x/y.png",
            "x.png.",
            "x.png ",
            "NUL.png",
            "com1",
            "",
            "a\u{1}b",
        ] {
            assert!(!safe_name(bad), "{bad:?}");
        }
        for good in [
            "lil_0123456789abcdef__00001_.png",
            "ComfyUI 00001.webp",
            "COM.png",
        ] {
            assert!(safe_name(good), "{good:?}");
        }
        assert!(safe_subfolder("") && safe_subfolder("a/b"));
        assert!(!safe_subfolder("C:x") && !safe_subfolder("a/../b"));
    }
}
