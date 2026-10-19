//! The replaceable stages of the CORE-015 pipeline: one trait per stage that plan 3 replaces
//! (settings cascade, cost estimates, confirmations and caps, reservations, metering,
//! recording), in their plan-3 signatures. [`PipelineHooks::plan2`] holds the plan-2
//! behaviour: [`CascadeSettings`] resolves the settings cascade (CORE-057), the estimate is
//! [`Estimate::Zero`], [`ConfirmStage`] fails closed (QA-016: an agent's `destructive` or
//! `exec` call has no way to ask you yet, so it gets `confirm.unavailable` and never runs),
//! nothing is reserved or settled, and [`WorkspaceRecord`] runs the Record stage (CORE-020).
//! The core replaces the confirm stage with `crate::confirm::AgentConfirm` (plan 3, Task 14),
//! which makes such a call `pending_confirmation` while it can be answered and decides what a
//! retry with its token gets ([`ConfirmStage::on_replay`]); it also asks when a cap would be
//! passed, at stage 5 or when the reserve stage finds one passed after it
//! ([`ConfirmStage::cap_needed`]). The estimate stage is [`ToolEstimates`] and the reserve stage
//! the caps' (Task 16).
//!
//! [`BillingBasis`], [`ConfirmedVia`] and [`CapHit`] are the shapes the stage signatures need.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use lil_platform::names::{LEGACY_WORKSPACE_FILE, WORKSPACE_FILE};
use lil_protocol::codes::PARAMS_INVALID;
use lil_protocol::{ErrorBody, Outcome, PendingConfirmation, ToolDescriptor, ToolOutput};
use lil_schema::workspace_yaml::{
    ProjectYaml, WorkspaceYaml, read_project_yaml, read_workspace_yaml,
};
use lil_schema::{Currency, Money};
use serde_json::{Map, Value};

use super::record_stage::WorkspaceRecord;
use super::registry::CallCtx;
use crate::pipeline::RecordPlan;
use crate::session::Session;
use crate::settings::cascade;
pub use crate::settings::{SettingSource, Settings};
use crate::state::{StateDb, StateError};
use crate::store::{Store, WsPath};
use crate::workspace::OpenWorkspace;

/// How an external service bills a call (CORE-069, Appendix F `billing_basis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BillingBasis {
    Payg,
    PrepaidCredits,
    SubscriptionQuota,
    FreeTier,
    Byok,
    Local,
}

impl BillingBasis {
    /// Every basis.
    pub const ALL: [BillingBasis; 6] = [
        BillingBasis::Payg,
        BillingBasis::PrepaidCredits,
        BillingBasis::SubscriptionQuota,
        BillingBasis::FreeTier,
        BillingBasis::Byok,
        BillingBasis::Local,
    ];

    /// The basis of its wire name.
    pub fn parse(s: &str) -> Option<BillingBasis> {
        BillingBasis::ALL.into_iter().find(|b| b.wire() == s)
    }

    /// `payg`, `prepaid_credits`, `subscription_quota`, `free_tier`, `byok`, `local`.
    pub fn wire(self) -> &'static str {
        match self {
            BillingBasis::Payg => "payg",
            BillingBasis::PrepaidCredits => "prepaid_credits",
            BillingBasis::SubscriptionQuota => "subscription_quota",
            BillingBasis::FreeTier => "free_tier",
            BillingBasis::Byok => "byok",
            BillingBasis::Local => "local",
        }
    }
}

/// Who confirmed a call (Appendix F, Confirmations): wire `app`, `claude_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfirmedVia {
    App,
    ClaudeCode,
}

impl ConfirmedVia {
    pub fn wire(self) -> &'static str {
        match self {
            ConfirmedVia::App => "app",
            ConfirmedVia::ClaudeCode => "claude_code",
        }
    }
}

/// A cap a call would pass (Task 16): `cap` is `monthly`, `per_call_max` or
/// `provider:<id>`; the amounts are in the display currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapHit {
    pub cap: String,
    pub limit: Money,
    pub settled: Money,
    pub held: Money,
    pub estimate: Money,
}

/// What a call is expected to cost (CORE-015 stage 4): nothing, local work, or an external
/// call's upper bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Estimate {
    Zero,
    Local {
        gpu: bool,
    },
    External {
        provider: String,
        amount: Money,
        currency: Currency,
        basis: BillingBasis,
    },
}

/// The call's idempotency row: its scope (caller kind + workspace), key and arguments hash.
#[derive(Debug, Clone, Copy)]
pub struct IdemRef<'a> {
    pub scope: &'a str,
    pub key: &'a str,
    pub args_hash: &'a str,
}

/// What the confirm-and-caps stage decides from.
#[derive(Clone, Copy)]
pub struct ConfirmInput<'a> {
    pub session: &'a Session,
    pub descriptor: &'a ToolDescriptor,
    pub estimate: &'a Estimate,
    pub args: &'a Value,
    /// `None` for a `read` tool (no key).
    pub idem: Option<IdemRef<'a>>,
    pub workspace: Option<&'a OpenWorkspace>,
    /// The call's resolved project (ARCH-088); `None` without a workspace. A confirmation is
    /// for the call in this project.
    pub project: Option<&'a str>,
    /// The call runs again for a `job.retry` (CORE-146): its own tool was not what the caller
    /// called, so no prompt of the caller's (Claude Code's) was about it.
    pub retry: bool,
    /// An agent's call needs your confirmation whatever the tool's effect: a `job.retry` of a
    /// job with external side effects that was `interrupted` (ruling P3-R21: it may have run,
    /// so paid work could be paid twice).
    pub always_confirm: bool,
}

impl ConfirmInput<'_> {
    /// Whether an agent's confirmation is needed (ARCH-052, CORE-109): an agent caller and a
    /// `destructive` or `exec` tool, or [`ConfirmInput::always_confirm`]. Your own actions in
    /// an app never ask.
    pub fn needs_agent_confirmation(&self) -> bool {
        self.session.caller.is_agent()
            && (self.descriptor.effect.needs_agent_confirmation() || self.always_confirm)
    }
}

/// A confirmation you approved, as a retry with its token runs it (CORE-091, Appendix F): the
/// tool, the arguments and the project you saw. The call runs with stage 5 satisfied only when
/// it is that tool with those arguments in that project; anything else asks again.
#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    /// `cf_<16 hex>`.
    pub confirmation_id: String,
    pub tool: String,
    pub arguments: Value,
    /// The call's resolved project when you approved it (`None` without a workspace).
    pub project: Option<String>,
    pub confirmed_via: ConfirmedVia,
    /// The cap action of the answer (`allow_once`, `raise`), for the cap half (Task 16).
    pub cap_action: Option<String>,
}

impl Approval {
    /// Whether this approval is for a call of `tool` with `args` in `project`.
    pub fn covers_call(&self, tool: &str, args: &Value, project: Option<&str>) -> bool {
        self.tool == tool
            && self.project.as_deref() == project
            && lil_protocol::args_hash(&self.arguments) == lil_protocol::args_hash(args)
    }
}

/// What a retry with the token of a stored `pending_confirmation` gets (Appendix F
/// "Confirmations"): run the call once, the same pending confirmation, or the end of it.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayDecision {
    /// Approved: the call runs (once) with stage 5 satisfied for what was approved.
    Run(Approval),
    /// Not answered yet: the same `pending_confirmation`.
    StillPending(PendingConfirmation),
    /// `confirm.denied`; the key is released.
    Denied,
    /// `confirm.expired` (also a prompt cancelled unanswered); the key is released.
    Expired,
}

/// What the confirm-and-caps stage decided (CORE-017).
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "the plan's signature: CapExceeded holds the wire ErrorBody, as the crate's Results do"
)]
pub enum ConfirmDecision {
    /// Run the call; `confirmed_via` records who confirmed it, `cap_override` is the cap a
    /// confirmation allowed once (handed to the reserve stage as `allow_once`).
    Proceed {
        confirmed_via: Option<ConfirmedVia>,
        cap_override: Option<String>,
    },
    /// The call waits for you; the caller gets `pending_confirmation`.
    Pending(PendingConfirmation),
    /// The call needs your confirmation and this session has no way to ask:
    /// `confirm.unavailable` (QA-016).
    Unavailable,
    /// A cap would be passed and nobody can be asked: the error (`cap.exceeded`).
    CapExceeded(ErrorBody),
    /// The stage could not decide (its `state.db` failed): the error the call ends with.
    Failed(ErrorBody),
}

/// `cap.exceeded` (QA-016) for a call of `tool` that would pass `hits`: the caps, and the
/// amounts as decimal strings in `data.caps`.
pub fn cap_exceeded(tool: &str, hits: &[CapHit]) -> ErrorBody {
    let names: Vec<&str> = hits.iter().map(|h| h.cap.as_str()).collect();
    let mut error = ErrorBody::new(
        &lil_protocol::codes::CAP_EXCEEDED,
        format!(
            "{tool} would pass the cap {}. Raise the cap or allow the call once in the LILITH app, or run the call where you can confirm it.",
            names.join(", ")
        ),
        "",
    );
    error.data = Some(serde_json::json!({ "caps": cap_hits_json(hits) }));
    error
}

/// `[{cap, limit, settled, held, estimate}]`, amounts as decimal strings in the display
/// currency.
pub fn cap_hits_json(hits: &[CapHit]) -> Value {
    Value::Array(
        hits.iter()
            .map(|h| {
                serde_json::json!({
                    "cap": h.cap,
                    "limit": h.limit.to_decimal_string(),
                    "settled": h.settled.to_decimal_string(),
                    "held": h.held.to_decimal_string(),
                    "estimate": h.estimate.to_decimal_string(),
                })
            })
            .collect(),
    )
}

/// What the reserve stage did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveOutcome {
    /// Nothing to reserve.
    None,
    /// Reserved: `rv_<16 hex>`.
    Reserved(String),
    /// The caps it would pass, found at reservation time.
    NeedsCap(Vec<CapHit>),
}

/// What the reserve stage reserves for.
#[derive(Debug, Clone, Copy)]
pub struct ReserveInput<'a> {
    pub session: &'a Session,
    pub estimate: &'a Estimate,
    pub workspace_id: &'a str,
    /// The call's id, `c_<16 hex>`; a job takes the reservation over (`JobCtx::reservation`).
    pub call_id: &'a str,
    /// The cap a confirmation allowed once.
    pub allow_once: Option<&'a str>,
}

/// What the meter stage settles (inline calls only: a call that submitted a job leaves it
/// to the job's completion).
#[derive(Debug, Clone, Copy)]
pub struct SettleInput<'a> {
    pub session: &'a Session,
    pub reservation: Option<&'a str>,
    pub call_id: &'a str,
    pub outcome: &'a Outcome<ToolOutput>,
}

/// What the settings stage resolves a call's settings from.
#[derive(Debug, Clone, Copy)]
pub struct SettingsInput<'a> {
    pub session: &'a Session,
    pub descriptor: &'a ToolDescriptor,
    /// The session's open workspace; `None` without one.
    pub workspace: Option<&'a OpenWorkspace>,
    /// The call's resolved project (ARCH-088); `None` without a workspace.
    pub project: Option<&'a str>,
    /// The request layer: the `settings` argument of a tool whose input schema declares one
    /// (CORE-057 clarification); `None` for every product tool.
    pub request: Option<&'a Map<String, Value>>,
}

/// Stage 3: the settings of the call.
pub trait SettingsStage: Send + Sync {
    /// `Err` is the error result the call ends with (`params.invalid` for a request layer the
    /// cascade refuses).
    fn resolve<'a>(
        &'a self,
        input: SettingsInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Settings, ErrorBody>> + Send + 'a>>;
}

/// Stage 4: the cost estimate, from the tool, its valid arguments and the call's settings.
pub trait EstimateStage: Send + Sync {
    fn estimate(&self, d: &ToolDescriptor, args: &Value, s: &Settings) -> Estimate;
}

/// Stage 5: confirmations and caps.
pub trait ConfirmStage: Send + Sync {
    fn check<'a>(&'a self, i: ConfirmInput<'a>) -> BoxFuture<'a, ConfirmDecision>;

    /// The reserve stage found caps `hits` passed after this stage let the call through (another
    /// call took the room meanwhile): what the call gets. A stage that can ask returns a
    /// `pending_confirmation` covering the cap only (the call's safety confirmation, if any,
    /// was already given); the default is `cap.exceeded`.
    fn cap_needed<'a>(
        &'a self,
        i: ConfirmInput<'a>,
        hits: Vec<CapHit>,
    ) -> BoxFuture<'a, ConfirmDecision> {
        Box::pin(std::future::ready(ConfirmDecision::CapExceeded(
            cap_exceeded(&i.descriptor.name, &hits),
        )))
    }

    /// A retry found its idempotency row `idem` (in `db`) holding a stored
    /// `pending_confirmation`: what it gets ([`ReplayDecision`]). `None` replays the stored
    /// outcome as it is (a stage that never makes one pending).
    fn on_replay<'a>(
        &'a self,
        db: &'a StateDb,
        idem: IdemRef<'a>,
    ) -> BoxFuture<'a, Result<Option<ReplayDecision>, StateError>> {
        let _ = (db, idem);
        Box::pin(std::future::ready(Ok(None)))
    }
}

/// Stage 6: reserves budget for the estimate.
pub trait ReserveStage: Send + Sync {
    /// `Err` is the error result the call ends with.
    fn reserve<'a>(
        &'a self,
        i: ReserveInput<'a>,
    ) -> BoxFuture<'a, Result<ReserveOutcome, ErrorBody>>;
}

/// Stage 8: settles the reservation against what an inline call used.
pub trait MeterStage: Send + Sync {
    fn settle<'a>(&'a self, i: SettleInput<'a>) -> BoxFuture<'a, ()>;
}

/// Stage 9: records the call (plan 2b, CORE-020) and may change its outcome: `plan` is what
/// the tool left in `ctx.record` during Execute (`None` when it recorded nothing).
pub trait RecordStage: Send + Sync {
    fn record<'a>(
        &'a self,
        ctx: &'a CallCtx,
        d: &'a ToolDescriptor,
        plan: Option<RecordPlan>,
        out: &'a mut Outcome<ToolOutput>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// The replaceable stages a [`crate::tools::Dispatcher`] runs.
#[derive(Clone)]
pub struct PipelineHooks {
    pub settings: Arc<dyn SettingsStage>,
    pub estimate: Arc<dyn EstimateStage>,
    pub confirm: Arc<dyn ConfirmStage>,
    pub reserve: Arc<dyn ReserveStage>,
    pub meter: Arc<dyn MeterStage>,
    pub record: Arc<dyn RecordStage>,
}

impl std::fmt::Debug for PipelineHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineHooks").finish_non_exhaustive()
    }
}

impl PipelineHooks {
    /// The plan-2 stages: the settings cascade, a zero estimate, fail-closed agent
    /// confirmations, no reservation or metering, and the workspace Record stage
    /// ([`WorkspaceRecord`], plan 2b).
    pub fn plan2() -> PipelineHooks {
        PipelineHooks {
            settings: Arc::new(CascadeSettings),
            estimate: Arc::new(Plan2),
            confirm: Arc::new(Plan2),
            reserve: Arc::new(Plan2),
            meter: Arc::new(Plan2),
            record: Arc::new(WorkspaceRecord),
        }
    }
}

/// Every plan-2 stage.
#[derive(Debug, Clone, Copy)]
struct Plan2;

/// The settings cascade (CORE-057): the workspace file's (`lilith.yaml`) `defaults:`, then the
/// call's project's `project.yaml`, then the request layer ([`crate::settings::cascade`]).
/// Without a workspace only the request layer is there. A file that is missing, can't be read
/// or doesn't parse is an empty layer (logged), so a damaged `project.yaml` never stops every
/// call; the cascade's `SchemaWarning`s are logged too.
#[derive(Debug, Clone, Copy, Default)]
pub struct CascadeSettings;

/// `rel`'s text through the store, or `None` (logged) when it is missing or can't be read.
async fn read_text(store: &Store, rel: &str) -> Option<String> {
    let rel = WsPath::parse(rel).ok()?;
    match store.read(&rel).await {
        Ok(Some(bytes)) => String::from_utf8(bytes)
            .map_err(|_| tracing::warn!(path = %rel, "a settings file is not UTF-8; it is ignored"))
            .ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(path = %rel, error = %e, "a settings file can't be read; it is ignored");
            None
        }
    }
}

/// The workspace file's name and text: `lilith.yaml`, else the legacy workspace file when the
/// new one is missing (R4; it is only read). `None` (logged) when neither can be read.
async fn workspace_text(store: &Store) -> Option<(&'static str, String)> {
    for name in [WORKSPACE_FILE, LEGACY_WORKSPACE_FILE] {
        let rel = WsPath::parse(name).ok()?;
        match store.read(&rel).await {
            Ok(None) => {}
            Ok(Some(bytes)) => {
                return String::from_utf8(bytes)
                    .map_err(|_| tracing::warn!(path = %rel, "a settings file is not UTF-8; it is ignored"))
                    .ok()
                    .map(|text| (name, text));
            }
            Err(e) => {
                tracing::warn!(path = %rel, error = %e, "a settings file can't be read; it is ignored");
                return None;
            }
        }
    }
    None
}

/// The file layers of `open`: the workspace file (with the name it was read from) and
/// `project`'s `project.yaml`.
async fn file_layers(
    open: &OpenWorkspace,
    project: Option<&str>,
) -> (&'static str, WorkspaceYaml, Option<ProjectYaml>) {
    let (ws_file, text) = match workspace_text(&open.store).await {
        None => (WORKSPACE_FILE, None),
        Some((name, text)) => (name, Some(text)),
    };
    let ws = match text {
        None => WorkspaceYaml::default(),
        Some(text) => read_workspace_yaml(&text).unwrap_or_else(|why| {
            tracing::warn!(
                why,
                "the workspace file doesn't parse; its defaults are ignored"
            );
            WorkspaceYaml::default()
        }),
    };
    let Some(slug) = project.filter(|p| lil_schema::is_slug(p)) else {
        return (ws_file, ws, None);
    };
    let text = read_text(&open.store, &format!("projects/{slug}/project.yaml")).await;
    let project = text.and_then(|text| {
        read_project_yaml(&text)
            .map_err(|why| {
                tracing::warn!(
                    project = slug,
                    why,
                    "project.yaml doesn't parse; its settings are ignored"
                );
            })
            .ok()
    });
    (ws_file, ws, project)
}

impl SettingsStage for CascadeSettings {
    fn resolve<'a>(
        &'a self,
        input: SettingsInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Settings, ErrorBody>> + Send + 'a>> {
        Box::pin(async move {
            let (ws_file, ws, project) = match input.workspace {
                None => (WORKSPACE_FILE, WorkspaceYaml::default(), None),
                Some(open) => file_layers(open, input.project).await,
            };
            let (settings, warnings) = cascade(&ws, ws_file, project.as_ref(), input.request)
                .map_err(|why| {
                    ErrorBody::new(
                        &PARAMS_INVALID,
                        format!("Invalid settings for {}: {why}", input.descriptor.name),
                        "",
                    )
                })?;
            for w in warnings {
                tracing::warn!(tool = %input.descriptor.name, at = %w.pointer, "{}", w.message);
            }
            Ok(settings)
        })
    }
}

/// The plan-3 estimate stage (CORE-015 step 4, CORE-086). No plan-3 product tool calls a paid
/// service (partner nodes are refused, the node install is free), so every tool is
/// [`Estimate::Zero`] except the test channels' two paid ones:
/// - `test.destructive {cost}`: `External {provider: "test", amount: cost, currency: <display>,
///   basis: payg}`;
/// - `test.meter_external {provider, amount, currency?, basis?}`: that external estimate, the
///   currency defaulting to the display currency and the basis to `payg`.
///
/// Arguments that don't read give `Zero` (the tool refuses them). `display` gives the display
/// currency of `suite.toml` now.
pub struct ToolEstimates {
    display: Arc<dyn Fn() -> Currency + Send + Sync>,
}

impl std::fmt::Debug for ToolEstimates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEstimates").finish_non_exhaustive()
    }
}

impl ToolEstimates {
    pub fn new(display: Arc<dyn Fn() -> Currency + Send + Sync>) -> ToolEstimates {
        ToolEstimates { display }
    }
}

impl EstimateStage for ToolEstimates {
    fn estimate(&self, d: &ToolDescriptor, args: &Value, _s: &Settings) -> Estimate {
        let text = |k: &str| args.get(k).and_then(Value::as_str);
        let money = |k: &str| text(k).and_then(|t| Money::parse_decimal(t).ok());
        let (provider, amount, currency, basis) = match d.name.as_str() {
            "test.destructive" => match money("cost") {
                Some(amount) => ("test", amount, None, Some(BillingBasis::Payg)),
                None => return Estimate::Zero,
            },
            "test.meter_external" => match (text("provider"), money("amount")) {
                (Some(p), Some(amount)) => (
                    p,
                    amount,
                    text("currency"),
                    text("basis").map_or(Some(BillingBasis::Payg), BillingBasis::parse),
                ),
                _ => return Estimate::Zero,
            },
            _ => return Estimate::Zero,
        };
        let currency = match currency {
            None => (self.display)(),
            Some(c) => match Currency::parse(c) {
                Ok(c) => c,
                Err(_) => return Estimate::Zero,
            },
        };
        match basis {
            Some(basis) => Estimate::External {
                provider: provider.to_string(),
                amount,
                currency,
                basis,
            },
            None => Estimate::Zero,
        }
    }
}

impl EstimateStage for Plan2 {
    fn estimate(&self, _d: &ToolDescriptor, _args: &Value, _s: &Settings) -> Estimate {
        Estimate::Zero
    }
}

impl ConfirmStage for Plan2 {
    /// An agent (`assistant`, `mcp`, `cli`) calling a `destructive` or `exec` tool needs your
    /// confirmation (ARCH-052), and plan 2 has no way to ask: [`ConfirmDecision::Unavailable`],
    /// interactive session or not (QA-016). Your own actions in an app never ask (CORE-109).
    fn check<'a>(&'a self, i: ConfirmInput<'a>) -> BoxFuture<'a, ConfirmDecision> {
        let decision = if i.needs_agent_confirmation() {
            ConfirmDecision::Unavailable
        } else {
            ConfirmDecision::Proceed {
                confirmed_via: None,
                cap_override: None,
            }
        };
        Box::pin(std::future::ready(decision))
    }
}

impl ReserveStage for Plan2 {
    fn reserve<'a>(
        &'a self,
        _i: ReserveInput<'a>,
    ) -> BoxFuture<'a, Result<ReserveOutcome, ErrorBody>> {
        Box::pin(std::future::ready(Ok(ReserveOutcome::None)))
    }
}

impl MeterStage for Plan2 {
    fn settle<'a>(&'a self, _i: SettleInput<'a>) -> BoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}
