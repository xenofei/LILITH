//! Stable error codes (QA-003–006): one constant per row of the spec Appendix D table.
//!
//! Rows marked `provisional` are defined now and confirmed by the plan that first returns them.

use crate::outcome::{ErrorBody, ErrorCategory, RetryPolicy};

/// One error code with its fixed category and retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeSpec {
    pub code: &'static str,
    pub category: ErrorCategory,
    pub retry: RetryPolicy,
    pub provisional: bool,
}

impl ErrorBody {
    /// An error with `spec`'s code, category and retry policy and no detail, fix or data.
    pub fn new(
        spec: &CodeSpec,
        message: impl Into<String>,
        trace_id: impl Into<String>,
    ) -> ErrorBody {
        ErrorBody {
            code: spec.code.to_string(),
            category: spec.category,
            message: message.into(),
            detail: None,
            fix: None,
            data: None,
            trace_id: trace_id.into(),
            retry: spec.retry,
        }
    }
}

const fn spec(
    code: &'static str,
    category: ErrorCategory,
    retry: RetryPolicy,
    provisional: bool,
) -> CodeSpec {
    CodeSpec {
        code,
        category,
        retry,
        provisional,
    }
}

use ErrorCategory as C;
use RetryPolicy as R;

pub const CONFIRM_DENIED: CodeSpec = spec("confirm.denied", C::Declined, R::Never, false);
pub const CONFIRM_EXPIRED: CodeSpec = spec("confirm.expired", C::Cancelled, R::AfterResolve, true);
pub const CONFIRM_UNAVAILABLE: CodeSpec = spec(
    "confirm.unavailable",
    C::UserFixable,
    R::AfterResolve,
    false,
);
pub const CAP_EXCEEDED: CodeSpec = spec("cap.exceeded", C::UserFixable, R::AfterResolve, false);
pub const DOC_HELD_ELSEWHERE: CodeSpec =
    spec("doc.held_elsewhere", C::DataConflict, R::AfterResolve, true);
pub const STORE_HASH_CONFLICT: CodeSpec = spec(
    "store.hash_conflict",
    C::DataConflict,
    R::AfterResolve,
    true,
);
pub const WORKSPACE_REQUIRED: CodeSpec =
    spec("workspace.required", C::UserFixable, R::AfterResolve, true);
/// `data.core_protocol` carries the core's `"M.m"` (frozen across majors).
pub const PROTOCOL_INCOMPATIBLE: CodeSpec = spec(
    "protocol.incompatible",
    C::UserFixable,
    R::AfterResolve,
    false,
);
pub const ENGINE_CLAUDE_CODE_UNTESTED_VERSION: CodeSpec = spec(
    "engine.claude_code.untested_version",
    C::UserFixable,
    R::AfterResolve,
    true,
);
pub const ENGINE_CLAUDE_CODE_NOT_SIGNED_IN: CodeSpec = spec(
    "engine.claude_code.not_signed_in",
    C::UserFixable,
    R::AfterResolve,
    false,
);
pub const COMFYUI_UNREACHABLE: CodeSpec =
    spec("comfyui.unreachable", C::ExternalDown, R::Auto, true);
pub const MODEL_MISSING: CodeSpec = spec("model.missing", C::UserFixable, R::AfterResolve, true);
pub const PROVIDER_RATE_LIMITED: CodeSpec =
    spec("provider.rate_limited", C::Transient, R::Auto, true);
pub const PROVIDER_SPEND_LIMIT: CodeSpec =
    spec("provider.spend_limit", C::ExternalDown, R::Never, false);
pub const QUOTA_PLAN_LIMIT: CodeSpec =
    spec("quota.plan_limit", C::ExternalDown, R::AfterResolve, true);
pub const WORKSPACE_UNKNOWN: CodeSpec =
    spec("workspace.unknown", C::UserFixable, R::AfterResolve, false);
/// The message names the active default workspace and how to change it.
pub const WORKSPACE_UNSUPPORTED: CodeSpec = spec(
    "workspace.unsupported",
    C::UserFixable,
    R::AfterResolve,
    false,
);
pub const TOOL_UNKNOWN: CodeSpec = spec("tool.unknown", C::UserFixable, R::AfterResolve, false);
pub const PARAMS_INVALID: CodeSpec = spec("params.invalid", C::UserFixable, R::AfterResolve, false);
pub const IDEMPOTENCY_KEY_REQUIRED: CodeSpec = spec(
    "idempotency.key_required",
    C::UserFixable,
    R::AfterResolve,
    false,
);
pub const IDEMPOTENCY_CONFLICT: CodeSpec =
    spec("idempotency.conflict", C::DataConflict, R::Never, false);
pub const IDEMPOTENCY_IN_PROGRESS: CodeSpec = spec(
    "idempotency.in_progress",
    C::Transient,
    R::SameIdempotencyKey,
    false,
);
pub const CALLER_NOT_ALLOWED: CodeSpec = spec("caller.not_allowed", C::Declined, R::Never, false);
pub const CALLER_ENGINE_TOKEN_INVALID: CodeSpec =
    spec("caller.engine_token_invalid", C::Declined, R::Never, false);
pub const CORE_DRAINING: CodeSpec =
    spec("core.draining", C::Transient, R::SameIdempotencyKey, false);
pub const CORE_BUG: CodeSpec = spec("core.bug", C::Bug, R::Never, false);
pub const STATE_UNAVAILABLE: CodeSpec = spec(
    "state.unavailable",
    C::Transient,
    R::SameIdempotencyKey,
    false,
);

/// Every code, in table order.
pub const ALL: &[&CodeSpec] = &[
    &CONFIRM_DENIED,
    &CONFIRM_EXPIRED,
    &CONFIRM_UNAVAILABLE,
    &CAP_EXCEEDED,
    &DOC_HELD_ELSEWHERE,
    &STORE_HASH_CONFLICT,
    &WORKSPACE_REQUIRED,
    &PROTOCOL_INCOMPATIBLE,
    &ENGINE_CLAUDE_CODE_UNTESTED_VERSION,
    &ENGINE_CLAUDE_CODE_NOT_SIGNED_IN,
    &COMFYUI_UNREACHABLE,
    &MODEL_MISSING,
    &PROVIDER_RATE_LIMITED,
    &PROVIDER_SPEND_LIMIT,
    &QUOTA_PLAN_LIMIT,
    &WORKSPACE_UNKNOWN,
    &WORKSPACE_UNSUPPORTED,
    &TOOL_UNKNOWN,
    &PARAMS_INVALID,
    &IDEMPOTENCY_KEY_REQUIRED,
    &IDEMPOTENCY_CONFLICT,
    &IDEMPOTENCY_IN_PROGRESS,
    &CALLER_NOT_ALLOWED,
    &CALLER_ENGINE_TOKEN_INVALID,
    &CORE_DRAINING,
    &CORE_BUG,
    &STATE_UNAVAILABLE,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::ErrorCategory as C;
    use crate::outcome::RetryPolicy as R;

    /// The Global Constraints / spec Appendix D error-code table, row by row.
    const TABLE: &[(&str, C, R, bool)] = &[
        ("confirm.denied", C::Declined, R::Never, false),
        ("confirm.expired", C::Cancelled, R::AfterResolve, true),
        (
            "confirm.unavailable",
            C::UserFixable,
            R::AfterResolve,
            false,
        ),
        ("cap.exceeded", C::UserFixable, R::AfterResolve, false),
        ("doc.held_elsewhere", C::DataConflict, R::AfterResolve, true),
        (
            "store.hash_conflict",
            C::DataConflict,
            R::AfterResolve,
            true,
        ),
        ("workspace.required", C::UserFixable, R::AfterResolve, true),
        (
            "protocol.incompatible",
            C::UserFixable,
            R::AfterResolve,
            false,
        ),
        (
            "engine.claude_code.untested_version",
            C::UserFixable,
            R::AfterResolve,
            true,
        ),
        (
            "engine.claude_code.not_signed_in",
            C::UserFixable,
            R::AfterResolve,
            false,
        ),
        ("comfyui.unreachable", C::ExternalDown, R::Auto, true),
        ("model.missing", C::UserFixable, R::AfterResolve, true),
        ("provider.rate_limited", C::Transient, R::Auto, true),
        ("provider.spend_limit", C::ExternalDown, R::Never, false),
        ("quota.plan_limit", C::ExternalDown, R::AfterResolve, true),
        ("workspace.unknown", C::UserFixable, R::AfterResolve, false),
        (
            "workspace.unsupported",
            C::UserFixable,
            R::AfterResolve,
            false,
        ),
        ("tool.unknown", C::UserFixable, R::AfterResolve, false),
        ("params.invalid", C::UserFixable, R::AfterResolve, false),
        (
            "idempotency.key_required",
            C::UserFixable,
            R::AfterResolve,
            false,
        ),
        ("idempotency.conflict", C::DataConflict, R::Never, false),
        (
            "idempotency.in_progress",
            C::Transient,
            R::SameIdempotencyKey,
            false,
        ),
        ("caller.not_allowed", C::Declined, R::Never, false),
        ("caller.engine_token_invalid", C::Declined, R::Never, false),
        ("core.draining", C::Transient, R::SameIdempotencyKey, false),
        ("core.bug", C::Bug, R::Never, false),
        (
            "state.unavailable",
            C::Transient,
            R::SameIdempotencyKey,
            false,
        ),
    ];

    /// `^[a-z_]+(\.[a-z_]+)+$`
    fn is_namespaced(code: &str) -> bool {
        let parts: Vec<&str> = code.split('.').collect();
        parts.len() >= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    }

    #[test] // spec: QA-003, QA-004, QA-005
    fn code_specs_follow_the_table() {
        assert_eq!(TABLE.len(), 27);
        assert_eq!(ALL.len(), TABLE.len());
        for (spec, (code, category, retry, provisional)) in ALL.iter().zip(TABLE) {
            assert_eq!(spec.code, *code);
            assert_eq!(spec.category, *category, "{code}");
            assert_eq!(spec.retry, *retry, "{code}");
            assert_eq!(spec.provisional, *provisional, "{code}");
            assert!(is_namespaced(spec.code), "{code} is not namespaced");
        }
        let unique: std::collections::BTreeSet<_> = ALL.iter().map(|s| s.code).collect();
        assert_eq!(unique.len(), ALL.len(), "duplicate codes");

        for bad in [
            "core",
            "Core.bug",
            "core.",
            ".bug",
            "core..bug",
            "core.bug1",
            "core-x.bug",
        ] {
            assert!(!is_namespaced(bad), "{bad}");
        }

        let body = ErrorBody::new(&IDEMPOTENCY_IN_PROGRESS, "busy", "t1");
        assert_eq!(body.code, "idempotency.in_progress");
        assert_eq!(body.category, C::Transient);
        assert_eq!(body.retry, R::SameIdempotencyKey);
        assert_eq!(body.message, "busy");
        assert_eq!(body.trace_id, "t1");
        assert!(body.detail.is_none() && body.fix.is_none() && body.data.is_none());
    }
}
