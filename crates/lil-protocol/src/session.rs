//! The handshake (`core/hello`, ARCH-029), `core/drain`, protocol versions and caller kinds.
//!
//! Frozen across every major version: the `core/hello` method name and its `protocol` field,
//! which the core reads with [`peek_protocol`] before it checks any other field; the
//! major-mismatch refusal ([`incompatible_refusal`]); and `core/drain` with no parameters and
//! the result `{"status":"ok","draining":true}`.

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::canonical::{canonical_json, sha256_hex};
use crate::codes::PROTOCOL_INCOMPATIBLE;
use crate::events::{
    EventParams, SubscribeParams, SubscribeResult, UnsubscribeParams, UnsubscribeResult,
};
use crate::jsonrpc::{RpcError, rpc_code};
use crate::outcome::{ErrorBody, Outcome, PendingConfirmation, ToolOutput};
use crate::tools::{ToolCallParams, ToolsListParams, ToolsListResult};

/// A protocol version, `"M.m"` on the wire (both parts decimal digits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

/// The protocol this build speaks: 1.2 (plan 4a, Appendix D). 1.0 and 1.1 clients keep
/// working.
pub const PROTOCOL: ProtocolVersion = ProtocolVersion { major: 1, minor: 2 };

/// The capabilities this build offers. A minor version adds features only as new strings:
/// 1.0 had `events.v1` and `tools.v1`; 1.1 adds `jobs.v1`, `confirmations.v1` and `caps.v1`
/// (the `app/*` methods need `confirmations.v1` or `caps.v1` in the shared capabilities); 1.2
/// adds `queue.v1` (`app/queue.pause` and `app/queue.resume` need it), `desktop.v1`
/// (`app/desktop.set` needs it) and `context.v1` (`app/set_context` needs it), and plan 4a's
/// later capabilities follow them.
pub const CAPABILITIES: &[&str] = &[
    "events.v1",
    "tools.v1",
    "jobs.v1",
    "confirmations.v1",
    "caps.v1",
    "queue.v1",
    "desktop.v1",
    "context.v1",
];

/// `<CARGO_PKG_VERSION>+<LIL_BUILD_ID or "local">`; the suffix is fixed at compile time by
/// this crate's build script.
pub const BUILD_ID: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("LIL_PROTOCOL_BUILD_SUFFIX")
);

/// The `workspace` value of a session without a workspace.
pub const NO_WORKSPACE: &str = "none";

/// A string that isn't `"M.m"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVersionError;

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a protocol version is \"major.minor\", for example \"1.0\"")
    }
}

impl std::error::Error for ParseVersionError {}

impl FromStr for ProtocolVersion {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        fn part(p: &str) -> Result<u16, ParseVersionError> {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ParseVersionError);
            }
            p.parse().map_err(|_| ParseVersionError)
        }
        let (major, minor) = s.split_once('.').ok_or(ParseVersionError)?;
        Ok(ProtocolVersion {
            major: part(major)?,
            minor: part(minor)?,
        })
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The outcome of version negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Negotiation {
    /// Same major; the session runs at the lower of the two minors.
    Compatible { minor: u16 },
    /// Different majors.
    Incompatible,
}

pub fn negotiate(client: ProtocolVersion, core: ProtocolVersion) -> Negotiation {
    if client.major == core.major {
        Negotiation::Compatible {
            minor: client.minor.min(core.minor),
        }
    } else {
        Negotiation::Incompatible
    }
}

/// Reads only `protocol` from raw `core/hello` params, before any other field is checked.
/// Anything but an object with a `"M.m"` string there is -32602.
pub fn peek_protocol(raw_params: &serde_json::Value) -> Result<ProtocolVersion, RpcError> {
    let invalid = || RpcError {
        code: rpc_code::INVALID_PARAMS,
        message: "core/hello params need a \"protocol\" string of the form \"major.minor\"".into(),
        data: None,
    };
    raw_params
        .get("protocol")
        .and_then(|p| p.as_str())
        .and_then(|p| p.parse().ok())
        .ok_or_else(invalid)
}

/// The capabilities both sides offer, in the core's order.
pub fn shared_capabilities(client: &[String], core: &[&str]) -> Vec<String> {
    core.iter()
        .filter(|c| client.iter().any(|x| x == *c))
        .map(|c| c.to_string())
        .collect()
}

/// The first 16 hex characters of SHA-256 over the canonical JSON of the JSON array of the
/// `schemars` schemas of every wire type, in a fixed order. Computed once per process.
pub fn schema_hash() -> String {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        let schemas = [
            schemars::schema_for!(HelloParams),
            schemars::schema_for!(HelloResult),
            schemars::schema_for!(DrainParams),
            schemars::schema_for!(DrainResult),
            schemars::schema_for!(ToolsListParams),
            schemars::schema_for!(ToolsListResult),
            schemars::schema_for!(ToolCallParams),
            schemars::schema_for!(Outcome<ToolOutput>),
            schemars::schema_for!(ErrorBody),
            schemars::schema_for!(PendingConfirmation),
            schemars::schema_for!(SubscribeParams),
            schemars::schema_for!(SubscribeResult),
            schemars::schema_for!(UnsubscribeParams),
            schemars::schema_for!(UnsubscribeResult),
            schemars::schema_for!(EventParams),
            // Protocol 1.1.
            schemars::schema_for!(crate::app::ConfirmationsListParams),
            schemars::schema_for!(crate::app::ConfirmationsListResult),
            schemars::schema_for!(crate::app::ConfirmationAnswerParams),
            schemars::schema_for!(crate::app::ConfirmationAnswerResult),
            schemars::schema_for!(crate::app::CapsSetParams),
            schemars::schema_for!(crate::app::CapsView),
            // Protocol 1.2.
            schemars::schema_for!(crate::app::QueueParams),
            schemars::schema_for!(crate::app::QueueState),
            schemars::schema_for!(crate::app::DesktopSetParams),
            schemars::schema_for!(crate::app::DesktopSettings),
            schemars::schema_for!(crate::app::SetContextParams),
            schemars::schema_for!(crate::app::SetContextResult),
        ];
        let all = serde_json::Value::Array(
            schemas
                .iter()
                .map(|s| serde_json::to_value(s).expect("a schema serialises"))
                .collect(),
        );
        sha256_hex(canonical_json(&all).as_bytes())[..16].to_string()
    })
    .clone()
}

/// Who is calling. Wire strings: `"ui:<app>"` (app = `[a-z][a-z0-9-]*`), `"assistant"`,
/// `"mcp"`, `"cli"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallerKind {
    Ui(String),
    Assistant,
    Mcp,
    Cli,
}

impl CallerKind {
    /// `assistant`, `mcp` and `cli` act for an agent or script; `ui:*` is you in an app.
    pub fn is_agent(&self) -> bool {
        !matches!(self, CallerKind::Ui(_))
    }
}

/// A string that isn't a caller kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseCallerKindError;

impl fmt::Display for ParseCallerKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "a caller kind is \"ui:<app>\" (app: a-z, then a-z, 0-9 or -), \"assistant\", \"mcp\" or \"cli\"",
        )
    }
}

impl std::error::Error for ParseCallerKindError {}

impl FromStr for CallerKind {
    type Err = ParseCallerKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "assistant" => Ok(CallerKind::Assistant),
            "mcp" => Ok(CallerKind::Mcp),
            "cli" => Ok(CallerKind::Cli),
            _ => {
                let app = s.strip_prefix("ui:").ok_or(ParseCallerKindError)?;
                let mut bytes = app.bytes();
                let head_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase());
                if head_ok
                    && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                {
                    Ok(CallerKind::Ui(app.to_string()))
                } else {
                    Err(ParseCallerKindError)
                }
            }
        }
    }
}

impl fmt::Display for CallerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallerKind::Ui(app) => write!(f, "ui:{app}"),
            CallerKind::Assistant => f.write_str("assistant"),
            CallerKind::Mcp => f.write_str("mcp"),
            CallerKind::Cli => f.write_str("cli"),
        }
    }
}

/// `core/hello` params (ARCH-029). Unknown fields are refused (-32602). `Debug` never shows
/// the engine token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HelloParams {
    /// `"M.m"`.
    pub protocol: String,
    pub build_id: String,
    /// 16 hex.
    pub schema_hash: String,
    pub capabilities: Vec<String>,
    /// `"ui:<app>"`, `"assistant"`, `"mcp"` or `"cli"`.
    pub caller_kind: String,
    pub non_interactive: bool,
    /// 16-hex workspace id, or `"none"`.
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

impl fmt::Debug for HelloParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelloParams")
            .field("protocol", &self.protocol)
            .field("build_id", &self.build_id)
            .field("schema_hash", &self.schema_hash)
            .field("capabilities", &self.capabilities)
            .field("caller_kind", &self.caller_kind)
            .field("non_interactive", &self.non_interactive)
            .field("workspace", &self.workspace)
            .field(
                "engine_token",
                &self.engine_token.as_ref().map(|_| "<redacted>"),
            )
            .field("profile_id", &self.profile_id)
            .finish()
    }
}

/// `core/hello` result. `capabilities` are the shared ones; `caller_kind` is the resolved one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResult {
    pub session_id: String,
    pub instance_id: String,
    pub protocol: String,
    pub build_id: String,
    pub schema_hash: String,
    pub capabilities: Vec<String>,
    pub caller_kind: String,
    /// `null` for a session without a workspace.
    pub workspace: Option<WorkspaceInfo>,
    pub core: CoreInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceInfo {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoreInfo {
    pub pid: u32,
    pub channel: String,
    pub pipe: String,
}

/// `core/drain` params: none (frozen).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DrainParams {}

/// `core/drain` result: `{"status":"ok","draining":true}` inside the `Outcome` (frozen).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DrainResult {
    pub draining: bool,
}

/// The frozen refusal for a major mismatch: `protocol.incompatible` with
/// `data: {"core_protocol": "M.m"}`.
pub fn incompatible_refusal(core: ProtocolVersion, trace_id: &str) -> ErrorBody {
    ErrorBody {
        data: Some(json!({ "core_protocol": core.to_string() })),
        ..ErrorBody::new(
            &PROTOCOL_INCOMPATIBLE,
            format!(
                "This LILITH core speaks protocol {core}, and this client speaks a different major version. Update the older of the two."
            ),
            trace_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::rpc_code;
    use serde_json::json;

    fn hello_json() -> serde_json::Value {
        json!({
            "protocol": "1.0",
            "build_id": "0.0.0+local",
            "schema_hash": "0123456789abcdef",
            "capabilities": ["events.v1", "tools.v1"],
            "caller_kind": "cli",
            "non_interactive": false,
            "workspace": "none",
            "engine_token": "secret-engine-token",
        })
    }

    fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    #[test] // spec: ARCH-029
    fn hello_params_reject_unknown_fields() {
        let ok: HelloParams = serde_json::from_value(hello_json()).unwrap();
        assert_eq!(ok.engine_token.as_deref(), Some("secret-engine-token"));
        assert!(ok.profile_id.is_none());

        let mut extra = hello_json();
        extra["future_field"] = json!(1);
        assert!(serde_json::from_value::<HelloParams>(extra).is_err());

        let mut missing = hello_json();
        missing.as_object_mut().unwrap().remove("workspace");
        assert!(serde_json::from_value::<HelloParams>(missing).is_err());

        assert!(serde_json::from_value::<DrainParams>(json!({"now": true})).is_err());
        serde_json::from_value::<DrainParams>(json!({})).unwrap();
    }

    #[test] // spec: ARCH-029
    fn engine_token_is_redacted_in_debug() {
        let p: HelloParams = serde_json::from_value(hello_json()).unwrap();
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("secret-engine-token"), "{dbg}");
        assert!(dbg.contains("engine_token"), "{dbg}");
        assert!(dbg.contains("cli"), "{dbg}");
        let alt = format!("{p:#?}");
        assert!(!alt.contains("secret-engine-token"), "{alt}");
    }

    #[test] // spec: ARCH-029
    fn peek_protocol_reads_only_the_protocol_field() {
        assert_eq!(
            peek_protocol(&json!({"protocol": "2.0", "whatever": 1})).unwrap(),
            v(2, 0)
        );
        for bad in [
            json!({}),
            json!({"protocol": 1}),
            json!({"protocol": null}),
            json!({"protocol": "1"}),
            json!({"protocol": "one.zero"}),
            json!(["1.0"]),
            json!("1.0"),
        ] {
            let err = peek_protocol(&bad).unwrap_err();
            assert_eq!(err.code, rpc_code::INVALID_PARAMS, "{bad}");
        }
    }

    #[test] // spec: ARCH-029, QA-005
    fn incompatible_refusal_has_the_frozen_shape() {
        let body = incompatible_refusal(v(1, 0), "t1");
        assert_eq!(body.code, "protocol.incompatible");
        assert_eq!(body.data, Some(json!({"core_protocol": "1.0"})));
        assert_eq!(body.trace_id, "t1");
        let wire =
            serde_json::to_value(crate::outcome::Outcome::<DrainResult>::Error { error: body })
                .unwrap();
        assert_eq!(wire["status"], "error");
        assert_eq!(wire["error"]["code"], "protocol.incompatible");
        assert_eq!(wire["error"]["data"], json!({"core_protocol": "1.0"}));
        assert_eq!(
            incompatible_refusal(PROTOCOL, "t").data,
            Some(json!({"core_protocol": PROTOCOL.to_string()}))
        );
        assert_eq!(
            incompatible_refusal(v(3, 7), "t").data,
            Some(json!({"core_protocol": "3.7"}))
        );
    }

    #[test] // spec: ARCH-029, ARCH-023
    fn drain_result_wire_is_frozen() {
        let ok = crate::outcome::Outcome::Ok(DrainResult { draining: true });
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"status":"ok","draining":true}"#
        );
    }

    #[test] // spec: ARCH-029
    fn hello_result_without_workspace_has_workspace_null() {
        let result = HelloResult {
            session_id: "s_1".into(),
            instance_id: "c_1".into(),
            protocol: "1.0".into(),
            build_id: BUILD_ID.into(),
            schema_hash: "0123456789abcdef".into(),
            capabilities: vec!["tools.v1".into()],
            caller_kind: "cli".into(),
            workspace: None,
            core: CoreInfo {
                pid: 7,
                channel: "test-x".into(),
                pipe: r"\\.\pipe\p".into(),
            },
        };
        let wire = serde_json::to_value(&result).unwrap();
        let fields = wire.as_object().unwrap();
        assert_eq!(fields.get("workspace"), Some(&serde_json::Value::Null));
        assert_eq!(fields.len(), 9, "{wire}");
        assert_eq!(serde_json::from_value::<HelloResult>(wire).unwrap(), result);
    }

    /// Protocol 1.2 (Appendix D): the five capabilities of 1.1, then `queue.v1`, `desktop.v1`
    /// and `context.v1`, shared in the core's order. Checked by position, so the later plan-4a
    /// capabilities may follow them.
    #[test] // spec: ARCH-029, APP-043
    fn protocol_is_1_2_and_offers_queue_v1() {
        assert_eq!(PROTOCOL.to_string(), "1.2");
        let of_1_1 = [
            "events.v1",
            "tools.v1",
            "jobs.v1",
            "confirmations.v1",
            "caps.v1",
        ];
        assert_eq!(&CAPABILITIES[..5], &of_1_1);
        assert_eq!(CAPABILITIES.get(5), Some(&"queue.v1"));
        assert_eq!(CAPABILITIES.get(6), Some(&"desktop.v1"));
        assert_eq!(CAPABILITIES.get(7), Some(&"context.v1"));
        let unique: std::collections::BTreeSet<_> = CAPABILITIES.iter().collect();
        assert_eq!(unique.len(), CAPABILITIES.len(), "{CAPABILITIES:?}");
        // A client offering them all, in another order and with an unknown one, gets them in
        // the core's order.
        let mut client: Vec<String> = CAPABILITIES.iter().rev().map(|s| s.to_string()).collect();
        client.insert(2, "future.v9".to_string());
        assert_eq!(shared_capabilities(&client, CAPABILITIES), CAPABILITIES);
        // A 1.1 client keeps working: it shares what it offers, never `queue.v1`.
        let old: Vec<String> = of_1_1.iter().map(|s| s.to_string()).collect();
        let shared = shared_capabilities(&old, CAPABILITIES);
        assert_eq!(shared, of_1_1);
        assert!(!shared.iter().any(|c| c == "queue.v1"));
        // A 1.0 client too.
        let older: Vec<String> = vec!["events.v1".into(), "tools.v1".into()];
        assert_eq!(
            shared_capabilities(&older, CAPABILITIES),
            ["events.v1", "tools.v1"]
        );
    }

    #[test] // spec: ARCH-029
    fn version_negotiation() {
        assert_eq!(
            negotiate(v(1, 0), PROTOCOL),
            Negotiation::Compatible { minor: 0 }
        );
        assert_eq!(
            negotiate(v(1, 0), v(1, 3)),
            Negotiation::Compatible { minor: 0 }
        );
        assert_eq!(
            negotiate(v(1, 7), v(1, 0)),
            Negotiation::Compatible { minor: 0 }
        );
        assert_eq!(
            negotiate(v(1, 2), v(1, 5)),
            Negotiation::Compatible { minor: 2 }
        );
        assert_eq!(negotiate(v(2, 0), v(1, 0)), Negotiation::Incompatible);
        assert_eq!(negotiate(v(0, 9), v(1, 0)), Negotiation::Incompatible);

        assert_eq!("1.3".parse::<ProtocolVersion>().unwrap(), v(1, 3));
        assert_eq!("12.40".parse::<ProtocolVersion>().unwrap(), v(12, 40));
        for bad in [
            "1", "1.x", "", ".", "1.", ".0", "1.0.0", "+1.0", " 1.0", "1.0 ", "70000.0",
        ] {
            assert!(bad.parse::<ProtocolVersion>().is_err(), "{bad:?}");
        }
    }

    #[test] // spec: ARCH-029
    fn shared_capabilities_are_the_intersection_in_core_order() {
        let client: Vec<String> = ["tools.v1", "x.v9", "events.v1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            shared_capabilities(&client, CAPABILITIES),
            vec!["events.v1".to_string(), "tools.v1".to_string()]
        );
        assert!(shared_capabilities(&[], CAPABILITIES).is_empty());
        assert_eq!(
            shared_capabilities(&["tools.v1".to_string()], CAPABILITIES),
            vec!["tools.v1".to_string()]
        );
    }

    #[test] // spec: ARCH-029
    fn caller_kind_wire_strings() {
        for (wire, kind) in [
            ("ui:image", CallerKind::Ui("image".into())),
            ("ui:sketch-book2", CallerKind::Ui("sketch-book2".into())),
            ("assistant", CallerKind::Assistant),
            ("mcp", CallerKind::Mcp),
            ("cli", CallerKind::Cli),
        ] {
            assert_eq!(wire.parse::<CallerKind>().unwrap(), kind);
            assert_eq!(kind.to_string(), wire);
        }
        for bad in [
            "ui:", "ui:Image", "robot", "", "ui", "ui:1a", "ui:-a", "CLI", "ui:a b",
        ] {
            assert!(bad.parse::<CallerKind>().is_err(), "{bad:?}");
        }
        assert!(!CallerKind::Ui("image".into()).is_agent());
        assert!(CallerKind::Assistant.is_agent());
        assert!(CallerKind::Mcp.is_agent());
        assert!(CallerKind::Cli.is_agent());
        assert_eq!(NO_WORKSPACE, "none");
    }

    /// The golden schema hash.
    ///
    /// When a wire type changes, bump `PROTOCOL` (major for a breaking change, minor otherwise)
    /// and update the literal in the same commit.
    ///
    /// When only a `schemars` upgrade changes the literal (no wire type changed), update the
    /// literal without a protocol bump and say so in the commit message.
    #[test] // spec: ARCH-029
    fn schema_hash_is_16_hex_and_golden() {
        let h = schema_hash();
        assert_eq!(h.len(), 16);
        assert!(
            h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(h, schema_hash());
        // Protocol 1.1 added the `app/*` confirmation types (with `project` on a confirmation)
        // and `app/caps.set`'s params and `CapsView`; 1.2 the `app/queue.*` params and
        // `QueueState`, the fix's optional `user_hint` (Appendix G "Out of memory"), and
        // `app/confirmation.answer`'s optional `toast_token` (Appendix G "Deny from a toast"),
        // and `app/set_context`'s params and result (Appendix G "Session and listing tools").
        // The `HandlersWire` description changed (the scheme only, plan 4a ruling 2026-10-25):
        // no wire value changed, so no protocol bump. `app/set_context`'s result gained the
        // additive `effective_project` within 1.2, which plan 4a hasn't released.
        assert_eq!(h, "cbd599bda7731f66");
    }

    #[test] // spec: ARCH-029
    fn build_id_is_version_plus_suffix() {
        let (version, suffix) = BUILD_ID.split_once('+').unwrap();
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert!(!suffix.is_empty());
    }
}
