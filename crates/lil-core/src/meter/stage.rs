//! Stage 8 of the pipeline for inline calls (CORE-015; a call that submitted a job leaves it to
//! the job's completion): [`MeterSettle`] finishes the call's still-pending usage rows at their
//! estimates, with the call's status. Task 16 adds the reservation's settle after the line.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::{Meter, outcome_status};
use crate::tools::stages::{MeterStage, SettleInput};

/// The meter stage (module docs).
#[derive(Debug, Clone)]
pub struct MeterSettle {
    meter: Arc<Meter>,
}

impl MeterSettle {
    pub fn new(meter: Arc<Meter>) -> MeterSettle {
        MeterSettle { meter }
    }
}

impl MeterStage for MeterSettle {
    fn settle<'a>(&'a self, i: SettleInput<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let status = outcome_status(i.outcome);
            if let Err(e) = self.meter.finish_call(i.call_id, status.wire()).await {
                tracing::error!(call_id = i.call_id, code = %e.code, error = %e.message, "a call's usage record could not be finished");
            }
        })
    }
}
