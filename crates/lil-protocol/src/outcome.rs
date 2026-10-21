//! The result/error model (ARCH-036, ARCH-038, CORE-017, QA-001–004).
//!
//! Every method's JSON-RPC `result` is an [`Outcome`], tagged by `status`:
//! `{"status":"ok", …T's fields}`, `{"status":"pending_confirmation", …}` or
//! `{"status":"error","error":{…}}`. A pending confirmation is a normal result, not an error
//! (QA-002). Tool failures are `Outcome` errors, never JSON-RPC errors (QA-010).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Exactly one of `ok`, `pending_confirmation` or `error` (QA-001). `T` must serialise as a
/// JSON object: its fields sit next to `"status":"ok"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
// `ErrorBody` holds `serde_json::Value`s, which `preserve_order` (ARCH-076) makes ordered maps.
// Boxing it would change every `Outcome::Error { error }` match for no measurable gain.
#[allow(clippy::large_enum_variant)]
pub enum Outcome<T> {
    Ok(T),
    PendingConfirmation(PendingConfirmation),
    Error { error: ErrorBody },
}

/// A tool's ok payload (ARCH-038): `{"structured","artifacts":[{"path","sha256"?}],"summary_text"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolOutput {
    pub structured: serde_json::Value,
    pub artifacts: Vec<FileRef>,
    pub summary_text: String,
}

/// A file a tool produced or touched. `sha256` is 64 lowercase hex when present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileRef {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// The call is waiting for you (CORE-017, QA-002). `covers` says what the confirmation is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingConfirmation {
    pub confirmation_id: String,
    pub retry_token: String,
    pub expires_at: String,
    pub covers: Vec<Cover>,
}

/// What a pending confirmation covers: an agent confirmation, a cap, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cover {
    Confirm,
    Cap,
}

/// An error result (QA-003). Build one from a [`crate::codes::CodeSpec`] with
/// [`ErrorBody::new`]; the optional fields are omitted from the wire when `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    /// Stable, namespaced code (`crate::codes`).
    pub code: String,
    pub category: ErrorCategory,
    /// A short message.
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    pub trace_id: String,
    pub retry: RetryPolicy,
}

/// Optional detail text. Raw engine or provider text is `untrusted` (ARCH-060) and is wrapped
/// as data whenever an agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Detail {
    pub text: String,
    pub untrusted: bool,
}

/// The fix, in two forms (QA-003): a UI action and, optionally, an agent hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fix {
    pub ui_action: UiAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_hint: Option<AgentHint>,
}

/// A UI action such as "Start ComfyUI": a stable id and its label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiAction {
    pub id: String,
    pub label: String,
}

/// An agent hint: a tool name plus arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentHint {
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// Error categories (QA-003): a closed set of seven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    UserFixable,
    Transient,
    ExternalDown,
    /// Denied by you or by a permission preset.
    Declined,
    DataConflict,
    Cancelled,
    Bug,
}

/// Retry policies (QA-004): a closed set of four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    /// The core retries by itself and emits progress events.
    Auto,
    /// The caller may retry with the same idempotency key.
    SameIdempotencyKey,
    /// Retry only after the fix action or your decision.
    AfterResolve,
    Never,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_ok() -> Outcome<ToolOutput> {
        Outcome::Ok(ToolOutput {
            structured: json!({"text": "Ünïcödé space"}),
            artifacts: vec![
                FileRef {
                    path: r"%USERPROFILE%\ws\a.png".into(),
                    sha256: Some("ab".repeat(32)),
                },
                FileRef {
                    path: "b.txt".into(),
                    sha256: None,
                },
            ],
            summary_text: "done".into(),
        })
    }

    fn pending() -> Outcome<ToolOutput> {
        Outcome::PendingConfirmation(PendingConfirmation {
            confirmation_id: "c1".into(),
            retry_token: "r1".into(),
            expires_at: "2026-10-15T12:00:00Z".into(),
            covers: vec![Cover::Confirm, Cover::Cap],
        })
    }

    fn error() -> Outcome<ToolOutput> {
        Outcome::Error {
            error: ErrorBody {
                code: "comfyui.unreachable".into(),
                category: ErrorCategory::ExternalDown,
                message: "ComfyUI isn't answering.".into(),
                detail: Some(Detail {
                    text: "connection refused".into(),
                    untrusted: true,
                }),
                fix: Some(Fix {
                    ui_action: UiAction {
                        id: "start_comfyui".into(),
                        label: "Start ComfyUI".into(),
                    },
                    agent_hint: Some(AgentHint {
                        tool: "engine.start".into(),
                        arguments: json!({"engine": "comfyui"}),
                    }),
                }),
                data: Some(json!({"port": 8188})),
                trace_id: "t1".into(),
                retry: RetryPolicy::Auto,
            },
        }
    }

    #[test] // spec: QA-001, QA-002, ARCH-036, ARCH-038, CORE-017
    fn outcome_wire_shapes() {
        let cases = [
            (
                tool_ok(),
                json!({
                    "status": "ok",
                    "structured": {"text": "Ünïcödé space"},
                    "artifacts": [
                        {"path": r"%USERPROFILE%\ws\a.png", "sha256": "ab".repeat(32)},
                        {"path": "b.txt"}
                    ],
                    "summary_text": "done"
                }),
            ),
            (
                pending(),
                json!({
                    "status": "pending_confirmation",
                    "confirmation_id": "c1",
                    "retry_token": "r1",
                    "expires_at": "2026-10-15T12:00:00Z",
                    "covers": ["confirm", "cap"]
                }),
            ),
            (
                error(),
                json!({
                    "status": "error",
                    "error": {
                        "code": "comfyui.unreachable",
                        "category": "external_down",
                        "message": "ComfyUI isn't answering.",
                        "detail": {"text": "connection refused", "untrusted": true},
                        "fix": {
                            "ui_action": {"id": "start_comfyui", "label": "Start ComfyUI"},
                            "agent_hint": {"tool": "engine.start", "arguments": {"engine": "comfyui"}}
                        },
                        "data": {"port": 8188},
                        "trace_id": "t1",
                        "retry": "auto"
                    }
                }),
            ),
        ];
        for (outcome, wire) in cases {
            assert_eq!(serde_json::to_value(&outcome).unwrap(), wire);
            let back: Outcome<ToolOutput> = serde_json::from_value(wire).unwrap();
            assert_eq!(back, outcome);
        }

        // Optional error fields are omitted, not null.
        let bare = Outcome::<ToolOutput>::Error {
            error: ErrorBody::new(&crate::codes::CORE_BUG, "boom", "t2"),
        };
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"status": "error", "error": {
                "code": "core.bug", "category": "bug", "message": "boom",
                "trace_id": "t2", "retry": "never"
            }})
        );

        // QA-001: exactly one of three.
        for bad in [
            json!({"status": "maybe"}),
            json!({"structured": {}, "artifacts": [], "summary_text": ""}),
        ] {
            assert!(serde_json::from_value::<Outcome<ToolOutput>>(bad).is_err());
        }
    }

    #[test] // spec: QA-003, QA-004
    fn categories_and_retry_policies_are_closed_sets() {
        // The matches have no wildcard arm, so a new variant fails to compile here.
        fn category_wire(c: ErrorCategory) -> &'static str {
            match c {
                ErrorCategory::UserFixable => "user_fixable",
                ErrorCategory::Transient => "transient",
                ErrorCategory::ExternalDown => "external_down",
                ErrorCategory::Declined => "declined",
                ErrorCategory::DataConflict => "data_conflict",
                ErrorCategory::Cancelled => "cancelled",
                ErrorCategory::Bug => "bug",
            }
        }
        fn retry_wire(r: RetryPolicy) -> &'static str {
            match r {
                RetryPolicy::Auto => "auto",
                RetryPolicy::SameIdempotencyKey => "same_idempotency_key",
                RetryPolicy::AfterResolve => "after_resolve",
                RetryPolicy::Never => "never",
            }
        }
        let categories = [
            ErrorCategory::UserFixable,
            ErrorCategory::Transient,
            ErrorCategory::ExternalDown,
            ErrorCategory::Declined,
            ErrorCategory::DataConflict,
            ErrorCategory::Cancelled,
            ErrorCategory::Bug,
        ];
        let wires: std::collections::BTreeSet<_> =
            categories.iter().map(|c| category_wire(*c)).collect();
        assert_eq!(wires.len(), 7);
        for c in categories {
            let wire = category_wire(c);
            assert_eq!(serde_json::to_value(c).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<ErrorCategory>(json!(wire)).unwrap(),
                c
            );
        }
        for bad in ["fatal", "UserFixable", "user-fixable", ""] {
            assert!(serde_json::from_value::<ErrorCategory>(json!(bad)).is_err());
        }

        let retries = [
            RetryPolicy::Auto,
            RetryPolicy::SameIdempotencyKey,
            RetryPolicy::AfterResolve,
            RetryPolicy::Never,
        ];
        let wires: std::collections::BTreeSet<_> = retries.iter().map(|r| retry_wire(*r)).collect();
        assert_eq!(wires.len(), 4);
        for r in retries {
            let wire = retry_wire(r);
            assert_eq!(serde_json::to_value(r).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<RetryPolicy>(json!(wire)).unwrap(),
                r
            );
        }
        for bad in ["sometimes", "Auto", ""] {
            assert!(serde_json::from_value::<RetryPolicy>(json!(bad)).is_err());
        }
    }
}
