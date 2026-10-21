//! ComfyUI's websocket, `/ws?clientId=<id>` (Appendix F "ComfyUI client"): the events of the
//! channel's prompts and its binary preview frames.
//!
//! Text frames are `{type, data}`: `execution_start`, `executing`, `progress`,
//! `progress_state` (ComfyUI's per-node aggregate: the node it names `running` is read as
//! progress, so a ComfyUI that sends only these still reports it), `executed`,
//! `execution_success`, `execution_error` and `execution_interrupted` become [`ComfyEvent`]s;
//! the others (`status`, `execution_cached`, …) are skipped. A binary frame whose first 4 bytes
//! (big-endian) are event type 1 is a preview: 4 more bytes of image type (1 JPEG, 2 PNG), then
//! the image. ComfyUI sends that form only to a client that hasn't announced the
//! `supports_preview_metadata` feature flag, so the core never sends a `feature_flags` message
//! (ruling P3-R6). Everything in an event is ComfyUI's, untrusted (ARCH-060).

use futures_util::StreamExt as _;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::client::{ComfyClient, ComfyError};

/// The largest preview the core publishes (ARCH-033); a larger one is dropped.
pub const MAX_PREVIEW_BYTES: usize = 1_048_576;
/// How long the websocket handshake may take.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A preview's image type: frame image type 1 and 2, wire `"jpeg"` and `"png"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewFormat {
    Jpeg,
    Png,
}

impl PreviewFormat {
    pub fn wire(self) -> &'static str {
        match self {
            PreviewFormat::Jpeg => "jpeg",
            PreviewFormat::Png => "png",
        }
    }
}

/// One event of the websocket.
#[derive(Debug, Clone, PartialEq)]
pub enum ComfyEvent {
    ExecutionStart {
        prompt_id: String,
    },
    /// `node: None` once the prompt is done.
    Executing {
        prompt_id: String,
        node: Option<String>,
    },
    Progress {
        prompt_id: String,
        value: u64,
        max: u64,
    },
    /// The `running` node of a `progress_state`, its value and max made whole (down and up).
    ProgressState {
        prompt_id: String,
        node: String,
        value: u64,
        max: u64,
    },
    Executed {
        prompt_id: String,
        node: String,
        output: Value,
    },
    Success {
        prompt_id: String,
    },
    Error {
        prompt_id: String,
        exception_type: String,
        message: String,
    },
    Interrupted {
        prompt_id: String,
    },
    Preview {
        format: PreviewFormat,
        bytes: Vec<u8>,
    },
}

impl ComfyEvent {
    /// The prompt it is about; `None` for a preview (it belongs to the running prompt).
    pub fn prompt_id(&self) -> Option<&str> {
        match self {
            ComfyEvent::ExecutionStart { prompt_id }
            | ComfyEvent::Executing { prompt_id, .. }
            | ComfyEvent::Progress { prompt_id, .. }
            | ComfyEvent::ProgressState { prompt_id, .. }
            | ComfyEvent::Executed { prompt_id, .. }
            | ComfyEvent::Success { prompt_id }
            | ComfyEvent::Error { prompt_id, .. }
            | ComfyEvent::Interrupted { prompt_id } => Some(prompt_id),
            ComfyEvent::Preview { .. } => None,
        }
    }
}

/// The event of a text frame, `None` for one the core doesn't use.
pub fn parse_text(text: &str) -> Option<ComfyEvent> {
    let v: Value = serde_json::from_str(text).ok()?;
    let data = v.get("data")?;
    let prompt_id = || {
        data.get("prompt_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let text_of = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Some(match v.get("type")?.as_str()? {
        "execution_start" => ComfyEvent::ExecutionStart {
            prompt_id: prompt_id()?,
        },
        "executing" => ComfyEvent::Executing {
            prompt_id: prompt_id()?,
            node: data.get("node").and_then(Value::as_str).map(str::to_string),
        },
        "progress" => ComfyEvent::Progress {
            prompt_id: prompt_id()?,
            value: data.get("value").and_then(Value::as_u64).unwrap_or(0),
            max: data.get("max").and_then(Value::as_u64).unwrap_or(0),
        },
        "progress_state" => {
            let nodes = data.get("nodes")?.as_object()?;
            let (node, entry) = nodes
                .iter()
                .find(|(_, e)| e.get("state").and_then(Value::as_str) == Some("running"))?;
            let number = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|f| f.is_finite() && *f >= 0.0)
            };
            ComfyEvent::ProgressState {
                prompt_id: prompt_id()?,
                node: node.clone(),
                value: whole(number("value")?.floor()),
                max: whole(number("max")?.ceil()),
            }
        }
        "executed" => ComfyEvent::Executed {
            prompt_id: prompt_id()?,
            node: data.get("node").and_then(Value::as_str)?.to_string(),
            output: data.get("output").cloned().unwrap_or(Value::Null),
        },
        "execution_success" => ComfyEvent::Success {
            prompt_id: prompt_id()?,
        },
        "execution_error" => ComfyEvent::Error {
            prompt_id: prompt_id()?,
            exception_type: text_of("exception_type"),
            message: text_of("exception_message"),
        },
        "execution_interrupted" => ComfyEvent::Interrupted {
            prompt_id: prompt_id()?,
        },
        _ => return None,
    })
}

/// A finite, non-negative whole `f64` as a `u64` (saturating at `u64::MAX`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole(f: f64) -> u64 {
    f as u64
}

/// The event of a binary frame: a preview (event type 1), else `None`.
pub fn parse_binary(frame: &[u8]) -> Option<ComfyEvent> {
    let word = |at: usize| -> Option<u32> {
        let bytes: [u8; 4] = frame.get(at..at + 4)?.try_into().ok()?;
        Some(u32::from_be_bytes(bytes))
    };
    if word(0)? != 1 {
        return None;
    }
    let format = match word(4)? {
        1 => PreviewFormat::Jpeg,
        2 => PreviewFormat::Png,
        _ => return None,
    };
    Some(ComfyEvent::Preview {
        format,
        bytes: frame[8..].to_vec(),
    })
}

/// A websocket connection to ComfyUI.
pub struct ComfySocket {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl std::fmt::Debug for ComfySocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComfySocket").finish_non_exhaustive()
    }
}

impl ComfySocket {
    /// The next event the core uses; `None` once the connection has closed (or failed).
    pub async fn next(&mut self) -> Option<ComfyEvent> {
        loop {
            let event = match self.stream.next().await? {
                Ok(Message::Text(text)) => parse_text(text.as_str()),
                Ok(Message::Binary(frame)) => parse_binary(&frame),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => None,
            };
            if event.is_some() {
                return event;
            }
        }
    }
}

impl ComfyClient {
    /// Opens the websocket with the client's `clientId` on the client's own loopback host. It
    /// announces no feature flags (module docs).
    ///
    /// # Errors
    /// [`ComfyError::Unreachable`] when the handshake fails or takes longer than 5 s.
    pub async fn connect_ws(&self) -> Result<ComfySocket, ComfyError> {
        let url = self.ws_url()?;
        let connect = tokio_tungstenite::connect_async(url.as_str());
        match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
            Ok(Ok((stream, _))) => Ok(ComfySocket { stream }),
            Ok(Err(e)) => Err(ComfyError::Unreachable(format!("the websocket: {e}"))),
            Err(_) => Err(ComfyError::Unreachable(
                "the websocket handshake timed out".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ComfyUI 0.42.1's `progress_state`: the `running` node is the progress; one with no
    /// running node (all finished or pending) says nothing.
    #[test] // spec: CORE-130, CORE-117
    fn progress_state_reads_the_running_node() {
        let frame = |nodes: serde_json::Value| {
            serde_json::json!({"type": "progress_state", "data": {"prompt_id": "p", "nodes": nodes}})
                .to_string()
        };
        assert_eq!(
            parse_text(&frame(serde_json::json!({
                "1": {"value": 1.0, "max": 1.0, "state": "finished"},
                "3": {"value": 7.0, "max": 20.0, "state": "running"},
            }))),
            Some(ComfyEvent::ProgressState {
                prompt_id: "p".into(),
                node: "3".into(),
                value: 7,
                max: 20
            })
        );
        assert_eq!(
            parse_text(&frame(serde_json::json!({
                "1": {"value": 0.5, "max": 2.5, "state": "running"}}))),
            Some(ComfyEvent::ProgressState {
                prompt_id: "p".into(),
                node: "1".into(),
                value: 0,
                max: 3
            })
        );
        assert_eq!(
            parse_text(&frame(serde_json::json!({
                "1": {"value": 1.0, "max": 1.0, "state": "finished"}}))),
            None
        );
        assert_eq!(
            parse_text(&frame(serde_json::json!({
                "1": {"value": -1.0, "max": 1.0, "state": "running"}}))),
            None
        );
    }

    #[test] // spec: ARCH-033, CORE-130
    fn preview_frames_are_event_type_1_with_an_image_type() {
        let frame = |event: u32, image: u32, len: usize| {
            let mut f = event.to_be_bytes().to_vec();
            f.extend_from_slice(&image.to_be_bytes());
            f.extend(std::iter::repeat_n(7u8, len));
            f
        };
        assert_eq!(
            parse_binary(&frame(1, 2, 3)),
            Some(ComfyEvent::Preview {
                format: PreviewFormat::Png,
                bytes: vec![7, 7, 7]
            })
        );
        assert!(matches!(
            parse_binary(&frame(1, 1, 0)),
            Some(ComfyEvent::Preview {
                format: PreviewFormat::Jpeg,
                ..
            })
        ));
        // Event type 4 (with metadata), an unknown image type, a short frame: none.
        assert_eq!(parse_binary(&frame(4, 2, 3)), None);
        assert_eq!(parse_binary(&frame(1, 9, 3)), None);
        assert_eq!(parse_binary(&[0, 0, 0, 1, 0]), None);
    }
}
