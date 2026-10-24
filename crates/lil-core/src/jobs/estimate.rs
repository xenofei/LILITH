//! The estimator (CORE-161, APP-140; Appendix G "Estimates", P4a-R14): an [`Estimate`] in
//! parts from the measured history of similar jobs, before anything runs.
//!
//! - **Similar jobs** have the same `kind` and estimate key; the key is the job handler's
//!   ([`JobHandler::estimate_key`](super::JobHandler::estimate_key), from the job's own
//!   arguments) when a job is submitted, and the tool's ([`EstimateKey`], from the call's
//!   arguments) for `job.estimate`, which agree. A job or tool without a key lists `local`
//!   and `wall_s` as `no_key`.
//! - **History** is the newest 20 succeeded similar jobs in `telemetry.db` `job_stats`
//!   (`meter` writes them); fewer than 3 lists both parts as `no_history`. `local` and
//!   `wall_s` come from the same rows: nearest-rank p50 and p90, seconds with one decimal and
//!   Wh with three, rounded half to even. `local.energy_cost` is `wh_p50` at
//!   `[power] electricity`, given only while the rate is in the display currency (plan 4a
//!   converts no rate).
//! - **Money.** Plan 4a's jobs are local work and spend none, so no estimate has a `money`
//!   part or lists one (prices come with plan 6b).

use std::sync::Arc;

use lil_protocol::estimate::{
    Cost, Estimate, EstimateSource, LocalPart, PartName, UnknownPart, UnknownReason, WallPart,
};
use lil_schema::{Currency, Money};

use crate::telemetry::{JobStatsRow, PowerSettings, Telemetry, share};

/// How many of the newest similar jobs the history holds.
pub const HISTORY_NEWEST: u32 = 20;
/// The fewest similar jobs that are a history.
pub const HISTORY_MIN: usize = 3;

/// The kind and estimate key of the job a tool call would submit (`job.estimate`): a tool's
/// [`Tool::estimate_key`](crate::tools::Tool::estimate_key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimateKey {
    pub kind: &'static str,
    pub key: String,
}

/// The display currency, asked at each estimate.
pub type DisplayCurrency = Arc<dyn Fn() -> Currency + Send + Sync>;

/// The estimator (module docs).
pub struct Estimator {
    telemetry: Arc<Telemetry>,
    electricity: Option<(Money, Currency)>,
    display: DisplayCurrency,
}

impl std::fmt::Debug for Estimator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Estimator").finish_non_exhaustive()
    }
}

/// An estimate with no part: `local` and `wall_s` unknown for `reason`.
pub fn unknown(reason: UnknownReason) -> Estimate {
    Estimate {
        unknown: [PartName::Local, PartName::WallS]
            .into_iter()
            .map(|part| UnknownPart { part, reason })
            .collect(),
        ..Estimate::default()
    }
}

/// The nearest-rank `pct` percentile of `values` (sorted here): the value at rank
/// `ceil(pct / 100 × n)`. `None` for no values.
pub fn nearest_rank(values: &mut [u64], pct: u64) -> Option<u64> {
    values.sort_unstable();
    let n = u64::try_from(values.len()).ok()?;
    let rank = (pct * n).div_ceil(100).max(1);
    values.get(usize::try_from(rank - 1).ok()?).copied()
}

/// `units / 10^decimals` as a decimal string with exactly `decimals` decimals.
fn fixed(units: u64, decimals: u32) -> String {
    let scale = 10_u64.pow(decimals);
    let width = usize::try_from(decimals).unwrap_or(0);
    format!("{}.{:0width$}", units / scale, units % scale)
}

impl Estimator {
    /// An estimator over `telemetry`'s history, with `power`'s electricity rate and the
    /// display currency `display`.
    pub fn new(
        telemetry: Arc<Telemetry>,
        power: &PowerSettings,
        display: DisplayCurrency,
    ) -> Arc<Estimator> {
        Arc::new(Estimator {
            telemetry,
            electricity: power.electricity.clone(),
            display,
        })
    }

    /// The estimate of a job of `kind` with estimate key `key` (module docs). A history that
    /// can't be read is no history (logged).
    pub async fn estimate(&self, kind: &str, key: Option<&str>) -> Estimate {
        let Some(key) = key else {
            return unknown(UnknownReason::NoKey);
        };
        let rows = match self.telemetry.history(kind, key, HISTORY_NEWEST).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(kind, key, error = %e.message, "the estimator's history can't be read");
                Vec::new()
            }
        };
        if rows.len() < HISTORY_MIN {
            return unknown(UnknownReason::NoHistory);
        }
        self.of_history(&rows)
    }

    fn of_history(&self, rows: &[JobStatsRow]) -> Estimate {
        let samples = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        let spread = |of: fn(&JobStatsRow) -> u64| {
            let mut values: Vec<u64> = rows.iter().map(of).collect();
            let p50 = nearest_rank(&mut values, 50).unwrap_or(0);
            let p90 = nearest_rank(&mut values, 90).unwrap_or(0);
            (p50, p90)
        };
        // Tenths of a second, thousandths of a Wh (a mWh is 3600 mJ).
        let tenths = |ms: u64| fixed(share(ms, 1, 100), 1);
        let wh = |mj: u64| fixed(share(mj, 1, 3600), 3);
        let (wall_p50, wall_p90) = spread(|r| r.wall_ms);
        let (gpu_p50, gpu_p90) = spread(|r| r.gpu_seconds_milli);
        let (mj_p50, mj_p90) = spread(|r| r.system_mj);
        let energy_cost = self
            .electricity
            .as_ref()
            .filter(|(_, currency)| *currency == (self.display)())
            .map(|(rate, currency)| {
                // A kWh is 3.6e9 mJ.
                let rate = u64::try_from(rate.micros()).unwrap_or(0);
                let micros = share(rate, mj_p50, 3_600_000_000);
                Cost {
                    amount: Money::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
                        .to_decimal_string(),
                    currency: currency.as_str().to_string(),
                }
            });
        Estimate {
            local: Some(LocalPart {
                source: EstimateSource::MeasuredHistory,
                samples,
                gpu_s_p50: tenths(gpu_p50),
                gpu_s_p90: tenths(gpu_p90),
                wh_p50: wh(mj_p50),
                wh_p90: wh(mj_p90),
                energy_cost,
            }),
            wall_s: Some(WallPart {
                source: EstimateSource::MeasuredHistory,
                samples,
                p50: tenths(wall_p50),
                p90: tenths(wall_p90),
            }),
            money: None,
            unknown: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-161
    fn nearest_rank_and_fixed_decimals() {
        let mut five = vec![50, 10, 40, 20, 30];
        assert_eq!(nearest_rank(&mut five, 50), Some(30));
        assert_eq!(nearest_rank(&mut five, 90), Some(50));
        assert_eq!(nearest_rank(&mut [7], 90), Some(7));
        assert_eq!(nearest_rank(&mut [], 50), None);
        assert_eq!(fixed(150, 1), "15.0");
        assert_eq!(fixed(1250, 3), "1.250");
        assert_eq!(fixed(7, 3), "0.007");
    }
}
