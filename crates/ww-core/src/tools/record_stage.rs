//! The Record stage (CORE-015 stage 9, CORE-020, CORE-025): [`WorkspaceRecord`], the plan-2b
//! [`RecordStage`].
//!
//! It runs only when the outcome is `ok` and the tool left a
//! [`RecordPlan`](crate::pipeline::RecordPlan): it calls [`apply_record`] in the call's
//! workspace, on the store clock's UTC day, and sets the result's `structured.handoff` from
//! the returned [`HandOff`](crate::pipeline::HandOff). A commit error replaces the outcome with
//! the store error (outputs already moved stay as listed orphans, ARCH-075). A plan left by a
//! call that didn't end `ok` is dropped unwritten, and with it any version-lock guard.
//! Repairs and QA (CORE-025's middle steps) are the empty stages between ingest (Execute) and
//! this one.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use ww_protocol::{ErrorBody, Outcome, ToolDescriptor, ToolOutput, codes};

use super::registry::CallCtx;
use super::stages::RecordStage;
use crate::pipeline::{RecordPlan, apply_record};

/// The workspace Record stage (module docs).
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkspaceRecord;

impl RecordStage for WorkspaceRecord {
    fn record<'a>(
        &'a self,
        ctx: &'a CallCtx,
        d: &'a ToolDescriptor,
        plan: Option<RecordPlan>,
        out: &'a mut Outcome<ToolOutput>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let Some(plan) = plan else {
                return;
            };
            let Outcome::Ok(output) = out else {
                tracing::debug!(tool = %d.name, "a call that didn't end ok records nothing");
                return;
            };
            let Some(open) = ctx.workspace.as_ref() else {
                tracing::error!(tool = %d.name, "a record plan without a workspace");
                *out = Outcome::Error {
                    error: ErrorBody::new(
                        &codes::CORE_BUG,
                        format!(
                            "{} left a record without a workspace. This is a bug in Worldwright.",
                            d.name
                        ),
                        ctx.trace_id.clone(),
                    ),
                };
                return;
            };
            let today = ww_schema::time::date(open.store.now_utc());
            match apply_record(
                &open.store,
                &open.index,
                &ctx.events,
                &open.ws.id,
                &today,
                plan,
            )
            .await
            {
                Ok(handoff) => {
                    let handoff =
                        serde_json::to_value(&handoff).expect("a hand-off block serialises");
                    match &mut output.structured {
                        Value::Object(map) => {
                            map.insert("handoff".into(), handoff);
                        }
                        other => {
                            tracing::error!(tool = %d.name, "a recorded result that isn't an object gets no hand-off block");
                            let _ = other;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(tool = %d.name, trace_id = %ctx.trace_id, error = %e, "the Record stage failed");
                    *out = Outcome::Error {
                        error: e.to_error(&ctx.trace_id),
                    };
                }
            }
        })
    }
}
