//! The replaceable stages of the CORE-015 pipeline: one trait per stage that plan 3 replaces
//! (settings cascade, cost estimates, confirmations and caps, reservations, metering,
//! recording). [`PipelineHooks::plan2`] holds the plan-2 versions: they do nothing, except
//! [`ConfirmStage`], which fails closed (QA-016): an agent's `destructive` or `exec` call has
//! no way to ask you yet, so it gets `confirm.unavailable` and never runs.

use std::sync::Arc;

use ww_protocol::{ErrorBody, Outcome, PendingConfirmation, ToolDescriptor, ToolOutput};

use crate::session::Session;

/// The settings a call runs with. Empty in plan 2 (plan 2b adds the cascade).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings;

/// What a call is expected to cost. Plan 3 adds external estimates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Estimate {
    Zero,
}

/// What the confirm-and-caps stage decided (CORE-017).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmDecision {
    /// Run the call.
    Proceed,
    /// The call waits for you; the caller gets `pending_confirmation`.
    Pending(PendingConfirmation),
    /// The call needs your confirmation and this session has no way to ask:
    /// `confirm.unavailable` (QA-016).
    Unavailable,
}

/// Stage 3: the settings for the session.
pub trait SettingsStage: Send + Sync {
    fn resolve(&self, s: &Session) -> Settings;
}

/// Stage 4: the cost estimate.
pub trait EstimateStage: Send + Sync {
    fn estimate(&self, d: &ToolDescriptor) -> Estimate;
}

/// Stage 5: confirmations and caps.
pub trait ConfirmStage: Send + Sync {
    fn check(&self, s: &Session, d: &ToolDescriptor, e: &Estimate) -> ConfirmDecision;
}

/// Stage 6: reserves budget for the estimate.
pub trait ReserveStage: Send + Sync {
    /// # Errors
    /// The error result the call ends with (plan 3: `cap.exceeded`).
    fn reserve(&self, s: &Session, e: &Estimate) -> Result<(), ErrorBody>;
}

/// Stage 8: settles the reservation against what the call used.
pub trait MeterStage: Send + Sync {
    fn settle(&self, s: &Session, out: &Outcome<ToolOutput>);
}

/// Stage 9: records the call (plan 2b) and may add to its outcome.
pub trait RecordStage: Send + Sync {
    fn record(&self, s: &Session, d: &ToolDescriptor, out: &mut Outcome<ToolOutput>);
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
    /// The plan-2 stages: empty settings, a zero estimate, fail-closed agent confirmations,
    /// and no reservation, metering or recording.
    pub fn plan2() -> PipelineHooks {
        PipelineHooks {
            settings: Arc::new(Plan2),
            estimate: Arc::new(Plan2),
            confirm: Arc::new(Plan2),
            reserve: Arc::new(Plan2),
            meter: Arc::new(Plan2),
            record: Arc::new(Plan2),
        }
    }
}

/// Every plan-2 stage.
#[derive(Debug, Clone, Copy)]
struct Plan2;

impl SettingsStage for Plan2 {
    fn resolve(&self, _s: &Session) -> Settings {
        Settings
    }
}

impl EstimateStage for Plan2 {
    fn estimate(&self, _d: &ToolDescriptor) -> Estimate {
        Estimate::Zero
    }
}

impl ConfirmStage for Plan2 {
    /// An agent (`assistant`, `mcp`, `cli`) calling a `destructive` or `exec` tool needs your
    /// confirmation (ARCH-052), and plan 2 has no way to ask: [`ConfirmDecision::Unavailable`],
    /// interactive session or not (QA-016). Your own actions in an app never ask (CORE-109).
    fn check(&self, s: &Session, d: &ToolDescriptor, _e: &Estimate) -> ConfirmDecision {
        if s.caller.is_agent() && d.effect.needs_agent_confirmation() {
            ConfirmDecision::Unavailable
        } else {
            ConfirmDecision::Proceed
        }
    }
}

impl ReserveStage for Plan2 {
    fn reserve(&self, _s: &Session, _e: &Estimate) -> Result<(), ErrorBody> {
        Ok(())
    }
}

impl MeterStage for Plan2 {
    fn settle(&self, _s: &Session, _out: &Outcome<ToolOutput>) {}
}

impl RecordStage for Plan2 {
    fn record(&self, _s: &Session, _d: &ToolDescriptor, _out: &mut Outcome<ToolOutput>) {}
}
