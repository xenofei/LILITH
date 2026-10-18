//! The replaceable stages of the CORE-015 pipeline: one trait per stage that plan 3 replaces
//! (settings cascade, cost estimates, confirmations and caps, reservations, metering,
//! recording). [`PipelineHooks::plan2`] holds the plan-2 versions: [`CascadeSettings`]
//! resolves the settings cascade (CORE-057), [`ConfirmStage`] fails closed (QA-016: an
//! agent's `destructive` or `exec` call has no way to ask you yet, so it gets
//! `confirm.unavailable` and never runs), [`WorkspaceRecord`] runs the Record stage (CORE-020),
//! and the others do nothing.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lil_protocol::codes::PARAMS_INVALID;
use lil_protocol::{ErrorBody, Outcome, PendingConfirmation, ToolDescriptor, ToolOutput};
use lil_schema::workspace_yaml::{
    ProjectYaml, WorkspaceYaml, read_project_yaml, read_workspace_yaml,
};
use serde_json::{Map, Value};

use super::record_stage::WorkspaceRecord;
use super::registry::CallCtx;
use crate::pipeline::RecordPlan;
use crate::session::Session;
use crate::settings::cascade;
pub use crate::settings::{SettingSource, Settings};
use crate::store::{Store, WsPath};
use crate::workspace::OpenWorkspace;

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

/// The settings cascade (CORE-057): the workspace's `worldwright.yaml` `defaults:`, then the
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

/// The file layers of `open`: `worldwright.yaml` and `project`'s `project.yaml`.
async fn file_layers(
    open: &OpenWorkspace,
    project: Option<&str>,
) -> (WorkspaceYaml, Option<ProjectYaml>) {
    let ws = match read_text(&open.store, "worldwright.yaml").await {
        None => WorkspaceYaml::default(),
        Some(text) => read_workspace_yaml(&text).unwrap_or_else(|why| {
            tracing::warn!(
                why,
                "worldwright.yaml doesn't parse; its defaults are ignored"
            );
            WorkspaceYaml::default()
        }),
    };
    let Some(slug) = project.filter(|p| lil_schema::is_slug(p)) else {
        return (ws, None);
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
    (ws, project)
}

impl SettingsStage for CascadeSettings {
    fn resolve<'a>(
        &'a self,
        input: SettingsInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Settings, ErrorBody>> + Send + 'a>> {
        Box::pin(async move {
            let (ws, project) = match input.workspace {
                None => (WorkspaceYaml::default(), None),
                Some(open) => file_layers(open, input.project).await,
            };
            let (settings, warnings) =
                cascade(&ws, project.as_ref(), input.request).map_err(|why| {
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
