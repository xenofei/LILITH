//! Stage 8 of the pipeline for inline calls (CORE-015; a call that submitted a job leaves it to
//! the job's completion): [`MeterSettle`] writes the lines of the call's still-pending usage
//! rows at their estimates, with the call's status, then (with [`MeterSettle::with_caps`])
//! settles the call's reservation, and only then removes the rows (CORE-088). A call that
//! succeeded is billed at its lines' cost (or, with none, at the reserved amount); one that
//! failed or was cancelled releases it (no plan-3 provider bills a failure). A crash between the
//! line and the settle leaves the `logging` rows, from which the next start's
//! [`super::Caps::repair`] settles the reservation once; settling twice changes nothing.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::caps::Caps;
use super::{Meter, UsageStatus, outcome_status};
use crate::tools::stages::{MeterStage, SettleInput};

/// The meter stage (module docs).
#[derive(Debug, Clone)]
pub struct MeterSettle {
    meter: Arc<Meter>,
    caps: Option<Arc<Caps>>,
}

impl MeterSettle {
    pub fn new(meter: Arc<Meter>) -> MeterSettle {
        MeterSettle { meter, caps: None }
    }

    /// Also settles each inline call's reservation, after its usage lines.
    #[must_use]
    pub fn with_caps(self, caps: Arc<Caps>) -> MeterSettle {
        MeterSettle {
            caps: Some(caps),
            ..self
        }
    }
}

impl MeterStage for MeterSettle {
    fn settle<'a>(&'a self, i: SettleInput<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let status = outcome_status(i.outcome);
            let lines = match self.meter.log_call(i.call_id, status.wire()).await {
                Ok(lines) => lines,
                Err(e) => {
                    // The rows stay; the reservation stays held until the next start settles
                    // it from them (fail closed).
                    tracing::error!(call_id = i.call_id, code = %e.code, error = %e.message, "a call's usage record could not be finished");
                    return;
                }
            };
            if lines.cut {
                return;
            }
            if let (Some(caps), Some(rv)) = (&self.caps, i.reservation) {
                let records: Vec<_> = lines.logged.iter().map(|l| l.record.clone()).collect();
                let billed = status == UsageStatus::Succeeded;
                if let Err(e) = caps.settle_from_records(rv, &records, billed).await {
                    // The rows stay, so the next start settles it.
                    tracing::error!(call_id = i.call_id, reservation = rv, code = %e.code, "a call's cap reservation could not be settled; it stays held");
                    return;
                }
            }
            self.meter.drop_logged(&lines).await;
        })
    }
}
