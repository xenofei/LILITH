//! Estimates in parts (CORE-161, APP-140; Appendix G "Estimates"): what `job.estimate`
//! returns and a job's summary keeps from its submission, and how estimates add up. Pure, so
//! the apps use the same rules as the core.
//!
//! A part is present only when it was estimated, with its own `source`; `unknown` names each
//! part the call needs but couldn't estimate, and why. Plan 4a produces `local` and `wall_s`
//! from measured history; `money` comes with prices (plan 6b), and `simulated` and `quota`
//! are reserved. Seconds are decimal strings with one decimal, Wh with three.
//!
//! **Adding** ([`sum_part`]): a part adds only to the same part; the p50s add, and the p90 of
//! the sum is that sum plus the square root of the summed squares of each (p90 − p50). A job
//! without the part is counted, never taken as zero (`of`, `not_estimated`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a part comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EstimateSource {
    /// The newest similar jobs that succeeded (P4a-R14).
    MeasuredHistory,
    /// A partner node's price table (plan 6b).
    PriceTable,
    /// `prices.yaml` (plan 6b).
    PricesYaml,
}

/// A part of an estimate, by its wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PartName {
    Local,
    WallS,
    Money,
    Simulated,
    Quota,
}

/// Why a part the call needs wasn't estimated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// Fewer than 3 similar jobs.
    NoHistory,
    /// The tool has no `estimate_key`.
    NoKey,
    /// No price for it.
    NoPrice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnknownPart {
    pub part: PartName,
    pub reason: UnknownReason,
}

/// An amount of money in a currency (a decimal string and its code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cost {
    pub amount: String,
    pub currency: String,
}

/// Local work: SM-weighted GPU-seconds and whole-system watt-hours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalPart {
    pub source: EstimateSource,
    pub samples: u32,
    pub gpu_s_p50: String,
    pub gpu_s_p90: String,
    pub wh_p50: String,
    pub wh_p90: String,
    /// `wh_p50` at `[power] electricity`, in the display currency, when that is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_cost: Option<Cost>,
}

impl LocalPart {
    /// The GPU-seconds as a [`Spread`] (`None` when a figure doesn't read).
    pub fn gpu_s(&self) -> Option<Spread> {
        Spread::parse(&self.gpu_s_p50, &self.gpu_s_p90)
    }

    /// The watt-hours as a [`Spread`] (`None` when a figure doesn't read).
    pub fn wh(&self) -> Option<Spread> {
        Spread::parse(&self.wh_p50, &self.wh_p90)
    }
}

/// Wall-clock seconds of execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WallPart {
    pub source: EstimateSource,
    pub samples: u32,
    pub p50: String,
    pub p90: String,
}

impl WallPart {
    /// The seconds as a [`Spread`] (`None` when a figure doesn't read).
    pub fn spread(&self) -> Option<Spread> {
        Spread::parse(&self.p50, &self.p90)
    }
}

/// Money a call spends (plan 6b produces it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MoneyPart {
    pub source: EstimateSource,
    pub p50: String,
    pub p90: String,
    pub upper_bound: String,
    pub currency: String,
}

/// An estimate in parts (module docs). With no part it is "no estimate yet".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Estimate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_s: Option<WallPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub money: Option<MoneyPart>,
    #[serde(default)]
    pub unknown: Vec<UnknownPart>,
}

/// One job's p50 and p90 of a part, as numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    pub p50: f64,
    pub p90: f64,
}

impl Spread {
    pub fn new(p50: f64, p90: f64) -> Spread {
        Spread { p50, p90 }
    }

    fn parse(p50: &str, p90: &str) -> Option<Spread> {
        Some(Spread::new(p50.parse().ok()?, p90.parse().ok()?))
    }
}

/// A part summed over jobs: `of` jobs, `not_estimated` of them without the part.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpreadSum {
    pub p50: f64,
    pub p90: f64,
    pub of: u32,
    pub not_estimated: u32,
}

/// The sum of one part over jobs, one entry per job (`None`: the job has no such part): the
/// p50s add, and the p90 is that sum plus the root of the summed squares of each job's
/// (p90 − p50); p90s are never added (Appendix G "Adding estimates").
pub fn sum_part(parts: &[Option<&Spread>]) -> SpreadSum {
    let (mut p50, mut squares, mut missing) = (0.0, 0.0, 0_u32);
    for part in parts {
        match part {
            Some(s) => {
                p50 += s.p50;
                let spread = s.p90 - s.p50;
                squares += spread * spread;
            }
            None => missing += 1,
        }
    }
    SpreadSum {
        p50,
        p90: p50 + f64::sqrt(squares),
        of: u32::try_from(parts.len()).unwrap_or(u32::MAX),
        not_estimated: missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-161
    fn sum_part_of_nothing_estimated_is_zero_and_counted() {
        let sum = sum_part(&[None, None]);
        assert_eq!(
            (sum.p50, sum.p90, sum.of, sum.not_estimated),
            (0.0, 0.0, 2, 2)
        );
        let wall = WallPart {
            source: EstimateSource::MeasuredHistory,
            samples: 3,
            p50: "15.0".into(),
            p90: "23.0".into(),
        };
        assert_eq!(wall.spread(), Some(Spread::new(15.0, 23.0)));
    }

    #[test] // spec: CORE-161
    fn an_estimate_always_lists_its_unknown_parts() {
        let e = Estimate {
            unknown: vec![UnknownPart {
                part: PartName::WallS,
                reason: UnknownReason::NoHistory,
            }],
            ..Estimate::default()
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({"unknown": [{"part": "wall_s", "reason": "no_history"}]})
        );
        assert_eq!(
            serde_json::to_value(Estimate::default()).unwrap(),
            serde_json::json!({"unknown": []})
        );
    }
}
