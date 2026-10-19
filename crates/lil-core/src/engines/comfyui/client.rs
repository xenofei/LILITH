//! The ComfyUI HTTP client (Appendix F "ComfyUI client"): loopback URLs only (ARCH-068). Task
//! 11 starts it with the calls the supervisor needs, `GET /system_stats` (attached detection
//! and readiness) and `POST /free` (the idle free and the scheduler's unload); Task 12 adds the
//! rest.
//!
//! The client never uses a proxy and never follows a redirect (a 3xx is
//! [`ComfyError::Protocol`]): every request goes straight to the loopback address, and only
//! there. A URL with a user name or password is refused.

use std::time::Duration;

use lil_protocol::{ErrorBody, codes};
use reqwest::Url;

/// How long one request may take, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a connection may take: on loopback a refused one is immediate.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// The most of a failed response's body an error keeps.
const MAX_ERROR_BODY: usize = 4096;

/// Why a request to ComfyUI failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComfyError {
    /// Nothing answered (refused, reset, timed out).
    Unreachable(String),
    /// It answered with this status; the body is ComfyUI's text, untrusted (ARCH-060).
    Http(u16, String),
    /// It answered, but not as ComfyUI does.
    Protocol(String),
}

impl std::fmt::Display for ComfyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComfyError::Unreachable(why) => write!(f, "ComfyUI is not answering: {why}"),
            ComfyError::Http(status, _) => write!(f, "ComfyUI answered HTTP {status}"),
            ComfyError::Protocol(why) => write!(f, "ComfyUI's answer was not understood: {why}"),
        }
    }
}

/// A client of one ComfyUI.
#[derive(Debug, Clone)]
pub struct ComfyClient {
    base: Url,
    http: reqwest::Client,
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
        Ok(ComfyClient { base, http })
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

    /// `GET /system_stats`: ComfyUI's version, Python, devices.
    ///
    /// # Errors
    /// [`ComfyError`].
    pub async fn system_stats(&self) -> Result<serde_json::Value, ComfyError> {
        let response = self
            .http
            .get(self.endpoint("system_stats")?)
            .send()
            .await
            .map_err(unreachable)?;
        let response = checked(response).await?;
        let stats: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ComfyError::Protocol(e.to_string()))?;
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
        let body = serde_json::json!({"unload_models": unload_models, "free_memory": free_memory});
        let response = self
            .http
            .post(self.endpoint("free")?)
            .json(&body)
            .send()
            .await
            .map_err(unreachable)?;
        checked(response).await.map(drop)
    }
}

fn unreachable(e: reqwest::Error) -> ComfyError {
    ComfyError::Unreachable(e.to_string())
}

/// `response` when its status is a success; otherwise [`ComfyError::Http`] with (the start of)
/// its body.
async fn checked(response: reqwest::Response) -> Result<reqwest::Response, ComfyError> {
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
    if body.len() > MAX_ERROR_BODY {
        let mut end = MAX_ERROR_BODY;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    Err(ComfyError::Http(status.as_u16(), body))
}
