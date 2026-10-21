//! The ComfyUI HTTP client (Appendix F "ComfyUI client"): loopback URLs only (ARCH-068):
//! `GET /system_stats` (attached detection and readiness), `POST /free` (the idle free, the
//! scheduler's unload, an out-of-memory retry), and for the `comfy.prompt` jobs `POST /prompt`
//! with the channel's stable `client_id` ([`ComfyClient::with_client_id`]), the per-job cancel
//! `POST /api/jobs/{id}/cancel` (never a bare `/interrupt`, ARCH-021), where a prompt is
//! ([`ComfyClient::state`]: `/api/jobs/{id}`, `/history/{id}`, `/queue`), `/view` streamed into
//! a new file with its SHA-256 taken on the way, `/upload/image`, `/queue`, `/api/jobs`,
//! `/object_info` and `/node_replacements`; for the outside-jobs poll, `/api/queue` and
//! `/api/jobs/{id}`. The websocket is in [`super::ws`].
//!
//! The client never uses a proxy and never follows a redirect (a 3xx is
//! [`ComfyError::Protocol`]): every request goes straight to the loopback address, and only
//! there. A URL with a user name or password is refused.

use std::path::Path;
use std::time::Duration;

use lil_protocol::{ErrorBody, codes};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::store::fsops;

/// How long one request may take, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a connection may take. A refused connection on Windows loopback isn't immediate (the
/// stack retries it), so a probe of a stopped engine takes about this long, 2 s.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// The most of a failed response's body an error keeps.
const MAX_ERROR_BODY: usize = 4096;
/// The most of a `POST /prompt` rejection's body an error keeps: its `node_errors` are read
/// from it.
const MAX_PROMPT_ERROR_BODY: usize = 1024 * 1024;
/// The largest output a `/view` download takes (a larger one fails, and its file is removed).
pub const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// How long a `/view` download or an `/upload/image` may take: outputs can be large.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(600);

/// Why a request to ComfyUI failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComfyError {
    /// No connection was made, so nothing was sent (CORE-128: the request provably wasn't
    /// processed).
    Refused(String),
    /// Nothing answered after the connection was made (reset, timed out): a request may have
    /// arrived.
    Unreachable(String),
    /// It answered with this status; the body is ComfyUI's text, untrusted (ARCH-060).
    Http(u16, String),
    /// It answered, but not as ComfyUI does.
    Protocol(String),
    /// A local file could not be read or written (a `/view` download, an upload's source).
    Io(String),
}

impl std::fmt::Display for ComfyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComfyError::Refused(why) => {
                write!(f, "ComfyUI is not answering (no connection): {why}")
            }
            ComfyError::Unreachable(why) => write!(f, "ComfyUI is not answering: {why}"),
            ComfyError::Http(status, _) => write!(f, "ComfyUI answered HTTP {status}"),
            ComfyError::Protocol(why) => write!(f, "ComfyUI's answer was not understood: {why}"),
            ComfyError::Io(why) => write!(f, "a local file for ComfyUI failed: {why}"),
        }
    }
}

/// Where a prompt is in ComfyUI ([`ComfyClient::state`]).
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteState {
    /// Waiting in ComfyUI's queue.
    Queued,
    Running,
    /// Ended (whatever way): its `/history` entry.
    Done(Value),
    /// `/api/jobs/{id}` says `cancelled`, with no history entry: someone cancelled it.
    Cancelled,
    /// Absent from `/api/jobs`, `/history` and `/queue`: ComfyUI never had it, or lost it in a
    /// restart (CORE-128).
    Gone,
}

/// One file of a prompt's outputs, as `/history` and `/view` name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub filename: String,
    pub subfolder: String,
    /// `output`, `temp` or `input`.
    pub kind: String,
}

/// One `/queue` entry: `[number, prompt_id, prompt, extra_data, outputs_to_execute]`.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueEntry {
    pub number: u64,
    pub prompt_id: String,
    pub graph: Value,
    pub extra_data: Value,
}

/// `GET /queue`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueueView {
    pub running: Vec<QueueEntry>,
    pub pending: Vec<QueueEntry>,
}

/// One entry of `GET /api/jobs`.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteJob {
    pub id: String,
    /// `pending`, `in_progress`, `completed`, `failed` or `cancelled`.
    pub status: String,
    /// The whole entry, untrusted.
    pub raw: Value,
}

/// A client of one ComfyUI.
#[derive(Debug, Clone)]
pub struct ComfyClient {
    base: Url,
    http: reqwest::Client,
    /// The channel's stable id (`POST /prompt`'s `client_id`, the websocket's `clientId`).
    client_id: Option<String>,
}

impl ComfyClient {
    /// A client of the ComfyUI at `url` (`http://127.0.0.1:8188`).
    ///
    /// # Errors
    /// `comfyui.not_loopback` unless the host is `127.0.0.1`, `::1` or `localhost` (no
    /// request is ever sent elsewhere); `params.invalid` for a URL that doesn't parse or isn't
    /// `http`; `core.bug` if the HTTP client can't be built.
    pub fn new(url: &str) -> Result<ComfyClient, ErrorBody> {
        let base = Url::parse(url).map_err(|e| {
            ErrorBody::new(
                &codes::PARAMS_INVALID,
                format!("{url:?} is not a URL: {e}"),
                "",
            )
        })?;
        if base.scheme() != "http" {
            return Err(ErrorBody::new(
                &codes::PARAMS_INVALID,
                format!("{url:?}: ComfyUI is reached over http on loopback"),
                "",
            ));
        }
        // The URL parser normalises the host: IPv4 to dotted decimal, IPv6 to its short
        // bracketed form, names to lower case.
        let loopback = matches!(base.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
        // A URL with a user name or password is refused too: it would end up in logs.
        if !loopback || !base.username().is_empty() || base.password().is_some() {
            return Err(ErrorBody::new(
                &codes::COMFYUI_NOT_LOOPBACK,
                format!(
                    "{url:?} is not on this machine's loopback: the suite talks to ComfyUI on \
                     127.0.0.1, ::1 or localhost only (ARCH-068)."
                ),
                "",
            ));
        }
        // Never a redirect: it could send a request (and its body) off loopback.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| {
                ErrorBody::new(
                    &codes::CORE_BUG,
                    format!("the HTTP client could not be built: {e}"),
                    "",
                )
            })?;
        Ok(ComfyClient {
            base,
            http,
            client_id: None,
        })
    }

    /// The same client with the channel's stable `client_id` (from the suite `state.db`).
    #[must_use]
    pub fn with_client_id(mut self, client_id: String) -> ComfyClient {
        self.client_id = Some(client_id);
        self
    }

    /// The `client_id` the prompts and the websocket use.
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }

    /// The URL the client talks to.
    pub fn url(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    fn endpoint(&self, path: &str) -> Result<Url, ComfyError> {
        self.base
            .join(path)
            .map_err(|e| ComfyError::Protocol(format!("{path}: {e}")))
    }

    /// The websocket URL, `ws://<host>:<port>/ws?clientId=<id>`, on the client's own (checked)
    /// loopback host.
    pub(crate) fn ws_url(&self) -> Result<Url, ComfyError> {
        let mut url = self.endpoint("ws")?;
        url.set_scheme("ws")
            .map_err(|()| ComfyError::Protocol("no websocket URL".to_string()))?;
        if let Some(id) = &self.client_id {
            url.query_pairs_mut().append_pair("clientId", id);
        }
        Ok(url)
    }

    /// `GET <path>` as JSON.
    async fn get_json(&self, path: &str) -> Result<Value, ComfyError> {
        let response = self
            .http
            .get(self.endpoint(path)?)
            .send()
            .await
            .map_err(unreachable)?;
        checked(response)
            .await?
            .json()
            .await
            .map_err(|e| ComfyError::Protocol(format!("/{path}: {e}")))
    }

    /// `GET <path>` as JSON whose parts may come from an outside client (its `extra_data`,
    /// its workflow): anything nested deeper than [`MAX_OUTSIDE_DEPTH`] is read as `null`
    /// ([`cap_depth`]), so a deep value can't fail the whole answer.
    async fn get_outside_json(&self, path: &str) -> Result<Value, ComfyError> {
        let response = self
            .http
            .get(self.endpoint(path)?)
            .send()
            .await
            .map_err(unreachable)?;
        let text = checked(response).await?.text().await.map_err(unreachable)?;
        serde_json::from_str(&cap_depth(&text, MAX_OUTSIDE_DEPTH))
            .map_err(|e| ComfyError::Protocol(format!("/{path}: {e}")))
    }

    /// `GET /system_stats`: ComfyUI's version, Python, devices.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn system_stats(&self) -> Result<Value, ComfyError> {
        let stats = self.get_json("system_stats").await?;
        if !stats.is_object() {
            return Err(ComfyError::Protocol(
                "/system_stats is not an object".to_string(),
            ));
        }
        Ok(stats)
    }

    /// `POST /free {unload_models, free_memory}`.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn free(&self, unload_models: bool, free_memory: bool) -> Result<(), ComfyError> {
        let body = json!({"unload_models": unload_models, "free_memory": free_memory});
        let response = self
            .http
            .post(self.endpoint("free")?)
            .json(&body)
            .send()
            .await
            .map_err(unreachable)?;
        checked(response).await.map(drop)
    }

    /// `POST /prompt {prompt, client_id, prompt_id, extra_data: {preview_method,
    /// lil_trace_id}}` (Appendix F). `prompt_id` is the core-minted UUID, already persisted.
    ///
    /// # Errors
    /// [`ComfyError::Http`] with ComfyUI's body (a `400` holds its `node_errors`); an answer
    /// naming another prompt id is [`ComfyError::Protocol`].
    pub async fn submit(
        &self,
        prompt_id: &str,
        graph: &Value,
        preview_method: &str,
        trace_id: &str,
    ) -> Result<(), ComfyError> {
        let mut body = json!({
            "prompt": graph,
            "prompt_id": prompt_id,
            "extra_data": {"preview_method": preview_method, "lil_trace_id": trace_id},
        });
        if let Some(id) = &self.client_id {
            body["client_id"] = Value::from(id.as_str());
        }
        let response = self
            .http
            .post(self.endpoint("prompt")?)
            .json(&body)
            .send()
            .await
            .map_err(unreachable)?;
        let answer: Value = checked_up_to(response, MAX_PROMPT_ERROR_BODY)
            .await?
            .json()
            .await
            .map_err(|e| ComfyError::Protocol(format!("/prompt: {e}")))?;
        match answer.get("prompt_id").and_then(Value::as_str) {
            Some(id) if id != prompt_id => Err(ComfyError::Protocol(format!(
                "/prompt answered for another prompt id, {id:?}"
            ))),
            _ => Ok(()),
        }
    }

    /// `GET /history/{id}`: the prompt's entry once it ended, else `None`.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn history(&self, prompt_id: &str) -> Result<Option<Value>, ComfyError> {
        let all = self.get_json(&format!("history/{prompt_id}")).await?;
        Ok(all.get(prompt_id).cloned())
    }

    /// Where `prompt_id` is: `/api/jobs/{id}` first, then `/history/{id}`, then `/queue`; it is
    /// [`RemoteState::Gone`] only when none of them has it (CORE-120, CORE-128).
    ///
    /// # Errors
    /// [`ComfyError`]: then nothing is known.
    pub async fn state(&self, prompt_id: &str) -> Result<RemoteState, ComfyError> {
        let response = self
            .http
            .get(self.endpoint(&format!("api/jobs/{prompt_id}"))?)
            .send()
            .await
            .map_err(unreachable)?;
        if response.status() != reqwest::StatusCode::NOT_FOUND {
            let job: Value = checked(response)
                .await?
                .json()
                .await
                .map_err(|e| ComfyError::Protocol(format!("/api/jobs/{{id}}: {e}")))?;
            match job.get("status").and_then(Value::as_str) {
                Some("pending") => return Ok(RemoteState::Queued),
                Some("in_progress") => return Ok(RemoteState::Running),
                Some("cancelled") => {
                    return Ok(match self.history(prompt_id).await? {
                        Some(history) => RemoteState::Done(history),
                        None => RemoteState::Cancelled,
                    });
                }
                _ => {}
            }
        }
        if let Some(history) = self.history(prompt_id).await? {
            return Ok(RemoteState::Done(history));
        }
        let queue = self.queue().await?;
        if queue.running.iter().any(|e| e.prompt_id == prompt_id) {
            return Ok(RemoteState::Running);
        }
        if queue.pending.iter().any(|e| e.prompt_id == prompt_id) {
            return Ok(RemoteState::Queued);
        }
        Ok(RemoteState::Gone)
    }

    /// `POST /api/jobs/{id}/cancel`: the per-job cancel, the only one the suite sends
    /// (ARCH-021, CORE-130).
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn cancel(&self, prompt_id: &str) -> Result<(), ComfyError> {
        let response = self
            .http
            .post(self.endpoint(&format!("api/jobs/{prompt_id}/cancel"))?)
            .send()
            .await
            .map_err(unreachable)?;
        checked(response).await.map(drop)
    }

    /// `GET /view` of `file`, streamed into the new file `dest` (never an existing one), with
    /// the SHA-256 of the stream taken on the way: `(bytes, sha256)`. A failed download, or one
    /// over [`MAX_OUTPUT_BYTES`], leaves no file.
    ///
    /// # Errors
    /// [`ComfyError`]; [`ComfyError::Io`] when `dest` can't be created or written.
    pub async fn view_to(
        &self,
        file: &OutputFile,
        dest: &Path,
    ) -> Result<(u64, String), ComfyError> {
        let mut url = self.endpoint("view")?;
        url.query_pairs_mut()
            .append_pair("filename", &file.filename)
            .append_pair("subfolder", &file.subfolder)
            .append_pair("type", &file.kind);
        let response = self
            .http
            .get(url)
            .timeout(TRANSFER_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        let mut response = checked(response).await?;
        let io = |e: std::io::Error| ComfyError::Io(format!("{}: {e}", dest.display()));
        // The file calls run on blocking threads, never a runtime worker (QA1 M4); the bytes
        // are written in batches of up to WRITE_BATCH. A file dropped unfinished is removed.
        let path = dest.to_path_buf();
        let mut out = off_runtime(move || fsops::create_streamed(&path))
            .await
            .map_err(io)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut batch: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
            hash.update(&chunk);
            size += chunk.len() as u64;
            if size > MAX_OUTPUT_BYTES {
                return Err(ComfyError::Protocol(format!(
                    "the output is larger than {MAX_OUTPUT_BYTES} bytes"
                )));
            }
            batch.extend_from_slice(&chunk);
            if batch.len() >= WRITE_BATCH {
                out = write_batch(out, std::mem::take(&mut batch))
                    .await
                    .map_err(io)?;
            }
        }
        out = write_batch(out, batch).await.map_err(io)?;
        off_runtime(move || out.finish()).await.map_err(io)?;
        Ok((size, hex(&hash.finalize())))
    }

    /// `POST /upload/image` of the file `src` as `name`, overwriting a file of that name (the
    /// suite's upload names are content hashes, so it holds the same bytes).
    ///
    /// # Errors
    /// [`ComfyError`]; [`ComfyError::Io`] when `src` can't be read; an answer that kept the
    /// file under another name is [`ComfyError::Protocol`].
    pub async fn upload_image(&self, name: &str, src: &Path) -> Result<(), ComfyError> {
        let path = src.to_path_buf();
        let bytes = off_runtime(move || std::fs::read(&path))
            .await
            .map_err(|e| ComfyError::Io(format!("{}: {e}", src.display())))?;
        self.upload_bytes(name, &bytes).await
    }

    /// [`ComfyClient::upload_image`] of `bytes` already read (so what is sent is what was
    /// hashed).
    ///
    /// # Errors
    /// As [`ComfyClient::upload_image`].
    pub async fn upload_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), ComfyError> {
        if name.is_empty() || name.contains(['"', '\\', '/', '\r', '\n']) {
            return Err(ComfyError::Protocol(format!(
                "{name:?} is not an upload name"
            )));
        }
        let boundary = format!("lil-upload-{}", &hex(&Sha256::digest(bytes))[..24]);
        let mut body = Vec::with_capacity(bytes.len() + 512);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; \
                 filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(
            format!(
                "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\n\
                 true\r\n--{boundary}--\r\n"
            )
            .as_bytes(),
        );
        let response = self
            .http
            .post(self.endpoint("upload/image")?)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .timeout(TRANSFER_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        let answer: Value = checked(response)
            .await?
            .json()
            .await
            .map_err(|e| ComfyError::Protocol(format!("/upload/image: {e}")))?;
        match answer.get("name").and_then(Value::as_str) {
            Some(kept) if kept == name => Ok(()),
            other => Err(ComfyError::Protocol(format!(
                "/upload/image kept the file as {other:?}, not {name:?}"
            ))),
        }
    }

    /// `GET /queue`: the running and the pending prompts.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn queue(&self) -> Result<QueueView, ComfyError> {
        Ok(queue_view(&self.get_json("queue").await?))
    }

    /// `GET /api/queue`: the same as [`ComfyClient::queue`], under the `/api` prefix that
    /// ComfyUI serves every route under too. The outside-jobs poll uses it, so its requests
    /// are told apart from a prompt's own look at `/queue`.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn outside_queue(&self) -> Result<QueueView, ComfyError> {
        Ok(queue_view(&self.get_outside_json("api/queue").await?))
    }

    /// `GET /api/jobs/{id}`: the entry, or `None` when ComfyUI doesn't know it (404). The id
    /// may be an outside client's text, so it is percent-encoded as one path segment.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn job_detail(&self, prompt_id: &str) -> Result<Option<Value>, ComfyError> {
        // `.` and `..` stay dot segments even percent-encoded: no such id is looked up.
        if prompt_id.is_empty() || prompt_id.bytes().all(|b| b == b'.') {
            return Ok(None);
        }
        match self
            .get_outside_json(&format!("api/jobs/{}", path_segment(prompt_id)))
            .await
        {
            Ok(v) => Ok(Some(v)),
            Err(ComfyError::Http(404, _)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `GET /api/jobs?limit=<limit>&sort_order=desc`: the newest `limit` prompts (ComfyUI keeps
    /// up to 10,000 and has no default limit).
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn jobs(&self, limit: u32) -> Result<Vec<RemoteJob>, ComfyError> {
        let page = self
            .get_outside_json(&format!("api/jobs?limit={limit}&sort_order=desc"))
            .await?;
        Ok(page
            .get("jobs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|j| {
                Some(RemoteJob {
                    id: j.get("id")?.as_str()?.to_string(),
                    status: j.get("status")?.as_str()?.to_string(),
                    raw: j.clone(),
                })
            })
            .collect())
    }

    /// `GET /object_info`.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn object_info(&self) -> Result<Value, ComfyError> {
        self.get_json("object_info").await
    }

    /// `GET /node_replacements`.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn node_replacements(&self) -> Result<Value, ComfyError> {
        self.get_json("node_replacements").await
    }
}

/// How deep an outside answer is read ([`ComfyClient::get_outside_json`]): far below serde's
/// limit of 128, far above where the suite reads anything (a graph's class is at depth 5).
pub const MAX_OUTSIDE_DEPTH: usize = 64;

/// `text` (JSON) with every array or object nested deeper than `max` replaced by `null`.
/// Strings are skipped whole (with their escapes), so brackets inside them don't count. Text
/// that isn't JSON stays as broken as it was.
pub fn cap_depth(text: &str, max: usize) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut i = 0;
    let mut kept = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'[' | b'{' if depth == max => {
                // Skip the whole value, strings included.
                out.push_str(&text[kept..i]);
                out.push_str("null");
                let mut inner = 0usize;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            while i < bytes.len() && bytes[i] != b'"' {
                                i += if bytes[i] == b'\\' { 2 } else { 1 };
                            }
                        }
                        b'[' | b'{' => inner += 1,
                        b']' | b'}' => {
                            inner -= 1;
                            if inner == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                kept = i.min(bytes.len());
            }
            b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    out.push_str(&text[kept.min(text.len())..]);
    out
}

/// `s` percent-encoded as one URL path segment (everything but unreserved characters).
/// How many bytes of a `/view` download [`ComfyClient::view_to`] writes per blocking call.
const WRITE_BATCH: usize = 1024 * 1024;

/// `f` on tokio's blocking threads (QA1 M4): file calls never stall a runtime worker. A
/// blocking task that panicked is an error.
pub(crate) async fn off_runtime<T, F>(f: F) -> std::io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| {
        Err(std::io::Error::other(format!(
            "a file call did not run: {e}"
        )))
    })
}

/// Appends `bytes` to `out` on a blocking thread and hands it back; on an error `out` is
/// dropped there, which removes the unfinished file.
async fn write_batch(
    mut out: fsops::StreamedFile,
    bytes: Vec<u8>,
) -> std::io::Result<fsops::StreamedFile> {
    if bytes.is_empty() {
        return Ok(out);
    }
    off_runtime(move || out.write(&bytes).map(|()| out)).await
}

fn path_segment(s: &str) -> String {
    use std::fmt::Write as _;
    s.bytes()
        .fold(String::with_capacity(s.len()), |mut out, b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(b));
            } else {
                let _ = write!(out, "%{b:02X}");
            }
            out
        })
}

/// The running and pending entries of a `/queue` answer.
fn queue_view(queue: &Value) -> QueueView {
    let entries = |key: &str| -> Vec<QueueEntry> {
        queue
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                Some(QueueEntry {
                    number: e.get(0).and_then(Value::as_u64).unwrap_or(0),
                    prompt_id: e.get(1)?.as_str()?.to_string(),
                    graph: e.get(2).cloned().unwrap_or(Value::Null),
                    extra_data: e.get(3).cloned().unwrap_or(Value::Null),
                })
            })
            .collect()
    };
    QueueView {
        running: entries("queue_running"),
        pending: entries("queue_pending"),
    }
}
fn unreachable(e: reqwest::Error) -> ComfyError {
    if e.is_connect() {
        ComfyError::Refused(e.to_string())
    } else {
        ComfyError::Unreachable(e.to_string())
    }
}

/// Lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// `response` when its status is a success; otherwise [`ComfyError::Http`] with (the start of)
/// its body.
async fn checked(response: reqwest::Response) -> Result<reqwest::Response, ComfyError> {
    checked_up_to(response, MAX_ERROR_BODY).await
}

/// [`checked`], keeping at most `cap` bytes of a failed response's body.
async fn checked_up_to(
    response: reqwest::Response,
    cap: usize,
) -> Result<reqwest::Response, ComfyError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status.is_redirection() {
        return Err(ComfyError::Protocol(format!(
            "ComfyUI answered HTTP {} (a redirect); the suite never follows one",
            status.as_u16()
        )));
    }
    let mut body = response.text().await.unwrap_or_default();
    if body.len() > cap {
        let mut end = cap;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    Err(ComfyError::Http(status.as_u16(), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-060
    fn outside_ids_are_one_encoded_path_segment() {
        assert_eq!(
            path_segment("00000000-0000-4000-8000-000000000001"),
            "00000000-0000-4000-8000-000000000001"
        );
        assert_eq!(path_segment("a/b?c#d e%"), "a%2Fb%3Fc%23d%20e%25");
        assert_eq!(path_segment("..\\x"), "..%5Cx");
    }

    #[test] // spec: ARCH-069
    fn deep_values_are_read_as_null_and_strings_are_skipped() {
        let deep = format!(
            "{{\"a\": {}1{}, \"b\": \"]]{{\", \"c\": 2}}",
            "[".repeat(200),
            "]".repeat(200)
        );
        let v: Value = serde_json::from_str(&cap_depth(&deep, 64)).unwrap();
        assert_eq!((v["b"].as_str(), v["c"].as_u64()), (Some("]]{"), Some(2)));
    }
}
