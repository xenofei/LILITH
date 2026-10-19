//! The meter's tools (Appendix F "Product tools"; QA1 I3: they live here, not in `tools`, which
//! never names a `meter` item): `usage.summary` and `caps.get` (module `meter`; no tool changes
//! the caps, CORE-008), and on test channels `test.meter_external` (module `test`), an external
//! call to a fake provider through the pipeline's estimate, caps, write-ahead and settle.
//!
//! `test.meter_external` begins its write-ahead row and leaves it pending; stage 8 finishes it
//! at its estimate with the call's status (`failed` with `fail: true`). Its result is the row's
//! id, `{usage_id}`: the line's `seq` exists only once stage 8 has run, after the tool returns.

use std::sync::Arc;

use lil_protocol::{EffectClass, ErrorBody, codes};
use lil_schema::money::{Currency, Money};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::caps::Caps;
use super::{Attribution, METER_MODULE, Meter, SummaryQuery, UsageCtx, UsageDraft, UsageStatus};
use crate::tools::stages::BillingBasis;
use crate::tools::{Registry, TEST_MODULE, typed_tool};

const SUMMARY_DESCRIPTION: &str = "Summarises usage records by local calendar `period` (day or \
    month) between `from` and `to` (RFC 3339 UTC, both included; by default the current \
    period), grouped by any of provider, model, project, workspace, caller_kind, tool and \
    account. Billing bases are never added together: `bases` has money, subscription_quota \
    (its own units) and local work (GPU-seconds and energy), each with its `records`. \
    `by_currency` is the money per native currency; `display_total` is that money converted \
    at the dated rates of suite.toml, plus the fixed costs when period is month. Estimated and \
    reconciled costs are counted; unattributed spend (paid ComfyUI work submitted outside the \
    suite) is in `unattributed` alone. Money is a decimal string.";

const EXTERNAL_DESCRIPTION: &str = "Test channels: an external call to a fake provider \
    `provider` costing `amount` (a decimal string) in `currency` (default: the display \
    currency) on `basis` (default payg). It writes its pending usage record before the call and \
    leaves it for the meter stage to settle; `fail` makes the call fail.";

const CAPS_DESCRIPTION: &str = "The spending caps of this machine for the current local     calendar month (`period`, YYYY-MM), in the display currency: each cap set (`monthly`,     `per_call_max`, `provider:<id>`) with its `limit`, the money `settled` and `held` by calls     still running this month, and `ratio_permille` of the limit used. A cap marked `invalid`     doesn't parse in suite.toml and counts as exceeded. Caps are optional and only you change     them, in the LILITH app; no tool can.";

/// `caps.get`'s arguments: none.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// Registers `caps.get` (module `meter`).
pub fn register_caps_tools(reg: &mut Registry, caps: &Arc<Caps>) {
    let caps = caps.clone();
    reg.register(typed_tool(
        "caps.get",
        CAPS_DESCRIPTION,
        EffectClass::Read,
        METER_MODULE,
        move |ctx, _args: NoArgs| {
            let caps = caps.clone();
            Box::pin(async move {
                let view = caps.view().await.map_err(|mut e| {
                    e.trace_id.clone_from(&ctx.trace_id);
                    e
                })?;
                let text = match view.caps.len() {
                    0 => "no caps are set".to_string(),
                    n => format!("{n} caps for {}", view.period),
                };
                Ok((view, text))
            })
        },
    ));
}

/// `test.meter_external`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExternalArgs {
    provider: String,
    /// A decimal string.
    amount: String,
    #[serde(default)]
    currency: Option<String>,
    /// `payg`, `prepaid_credits`, `subscription_quota`, `free_tier`, `byok` or `local`.
    #[serde(default)]
    basis: Option<String>,
    #[serde(default)]
    fail: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ExternalResult {
    usage_id: String,
}

/// Registers `usage.summary`, and `test.meter_external` when `test_tools`.
pub fn register_meter_tools(reg: &mut Registry, meter: &Arc<Meter>, test_tools: bool) {
    let m = meter.clone();
    reg.register(typed_tool(
        "usage.summary",
        SUMMARY_DESCRIPTION,
        EffectClass::Read,
        METER_MODULE,
        move |ctx, args: SummaryQuery| {
            let m = m.clone();
            Box::pin(async move {
                let summary: Value =
                    m.summary(args, ctx.workspace.as_ref())
                        .await
                        .map_err(|mut e| {
                            e.trace_id.clone_from(&ctx.trace_id);
                            e
                        })?;
                let text = format!(
                    "{} in the display currency",
                    summary["display_total"].as_str().unwrap_or("0")
                );
                Ok((summary, text))
            })
        },
    ));
    if !test_tools {
        return;
    }
    let m = meter.clone();
    reg.register(typed_tool(
        "test.meter_external",
        EXTERNAL_DESCRIPTION,
        EffectClass::Write,
        TEST_MODULE,
        move |ctx, args: ExternalArgs| {
            let m = m.clone();
            Box::pin(async move {
                let invalid =
                    |why: String| ErrorBody::new(&codes::PARAMS_INVALID, why, ctx.trace_id.clone());
                let amount = Money::parse_decimal(&args.amount).map_err(invalid)?;
                if amount < Money::ZERO {
                    return Err(invalid(format!("amount {:?} is negative", args.amount)));
                }
                let currency = match &args.currency {
                    Some(c) => Currency::parse(c).map_err(invalid)?,
                    None => m.money_settings().await.display,
                };
                let basis = match args.basis.as_deref() {
                    None => BillingBasis::Payg,
                    Some(b) => BillingBasis::parse(b)
                        .ok_or_else(|| invalid(format!("{b:?} is not a billing basis")))?,
                };
                let text = format!("{} {} from {}", args.amount, currency, args.provider);
                let mut units = serde_json::Map::new();
                units.insert("requests".into(), json!(1));
                let draft = UsageDraft {
                    tool: "test.meter_external".to_string(),
                    provider: args.provider.clone(),
                    model: None,
                    route: "test".to_string(),
                    basis,
                    units,
                    est_cost: Some((amount, currency)),
                    account: None,
                    raw_usage: None,
                    attribution: Attribution::Suite,
                    status: UsageStatus::Pending,
                    latency_ms: None,
                };
                let uctx = UsageCtx {
                    workspace: ctx.workspace.clone(),
                    project: ctx.project.clone(),
                    asset: None,
                    job_id: None,
                    call_id: ctx.call_id.clone(),
                    caller_kind: ctx.session.caller.to_string(),
                    reservation: ctx.reservation.clone(),
                };
                let usage_id = m.begin(&uctx, draft).await.map_err(|mut e| {
                    e.trace_id.clone_from(&ctx.trace_id);
                    e
                })?;
                if args.fail {
                    return Err(ErrorBody::new(
                        &codes::PROVIDER_RATE_LIMITED,
                        format!("The fake provider {} refused the call.", args.provider),
                        ctx.trace_id.clone(),
                    ));
                }
                Ok((ExternalResult { usage_id }, text))
            })
        },
    ));
}
