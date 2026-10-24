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

/// One job's p50 and p90 of a part, as numbers: each finite and 0 or more.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    pub p50: f64,
    pub p90: f64,
}

impl Spread {
    /// `None` unless both figures are finite and 0 or more: anything else (NaN, an infinity,
    /// a negative) is not an estimate (Appendix G "Adding estimates").
    pub fn new(p50: f64, p90: f64) -> Option<Spread> {
        let fine = |v: f64| v.is_finite() && v >= 0.0;
        (fine(p50) && fine(p90)).then_some(Spread { p50, p90 })
    }

    fn parse(p50: &str, p90: &str) -> Option<Spread> {
        Spread::new(p50.parse().ok()?, p90.parse().ok()?)
    }
}

impl MoneyPart {
    /// The amounts as a [`Spread`] (`None` when a figure doesn't read); its currency is
    /// `currency`.
    pub fn spread(&self) -> Option<Spread> {
        Spread::parse(&self.p50, &self.p90)
    }
}

/// A part summed over jobs: `of` jobs, `not_estimated` of them without the part. `total` is
/// `None` when no job had it: nothing estimated reads as not estimated, never as 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpreadSum {
    pub total: Option<Spread>,
    pub of: u32,
    pub not_estimated: u32,
}

/// The sum of one part over jobs, one entry per job (`None`: the job has no such part): the
/// p50s add, and the p90 is that sum plus the root of the summed squares of each job's
/// (p90 − p50); p90s are never added (Appendix G "Adding estimates"). For `money` use
/// [`sum_money`], which never adds across currencies.
pub fn sum_part(parts: &[Option<&Spread>]) -> SpreadSum {
    let (mut p50, mut squares, mut found) = (0.0, 0.0, 0_u32);
    for s in parts.iter().flatten() {
        found += 1;
        p50 += s.p50;
        let spread = s.p90 - s.p50;
        squares += spread * spread;
    }
    let of = u32::try_from(parts.len()).unwrap_or(u32::MAX);
    SpreadSum {
        total: (found > 0).then(|| Spread {
            p50,
            p90: p50 + f64::sqrt(squares),
        }),
        of,
        not_estimated: of.saturating_sub(found),
    }
}

/// One currency's `money` sum ([`sum_money`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MoneySum {
    pub currency: String,
    /// `of` is every job; `not_estimated` the jobs with no `money` part (or one whose figures
    /// don't read). Jobs priced in another currency are in that currency's sum.
    pub sum: SpreadSum,
}

/// The `money` parts of jobs summed per currency, never converted into one (Appendix G), by
/// currency code. No job with a readable `money` part: an empty list.
pub fn sum_money(parts: &[Option<&MoneyPart>]) -> Vec<MoneySum> {
    let priced: Vec<(&str, Spread)> = parts
        .iter()
        .flatten()
        .filter_map(|m| Some((m.currency.as_str(), m.spread()?)))
        .collect();
    let mut currencies: Vec<&str> = priced.iter().map(|(c, _)| *c).collect();
    currencies.sort_unstable();
    currencies.dedup();
    let of = u32::try_from(parts.len()).unwrap_or(u32::MAX);
    let not_estimated = of.saturating_sub(u32::try_from(priced.len()).unwrap_or(u32::MAX));
    currencies
        .into_iter()
        .map(|currency| {
            let mine: Vec<&Spread> = priced
                .iter()
                .filter(|(c, _)| *c == currency)
                .map(|(_, s)| s)
                .collect();
            let refs: Vec<Option<&Spread>> = mine.iter().copied().map(Some).collect();
            let total = sum_part(&refs).total;
            MoneySum {
                currency: currency.to_string(),
                sum: SpreadSum {
                    total,
                    of,
                    not_estimated,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-161
    fn nothing_estimated_is_never_zero_and_bad_figures_are_not_estimates() {
        let sum = sum_part(&[None, None]);
        assert_eq!((sum.total, sum.of, sum.not_estimated), (None, 2, 2));
        let wall = |p50: &str, p90: &str| WallPart {
            source: EstimateSource::MeasuredHistory,
            samples: 3,
            p50: p50.into(),
            p90: p90.into(),
        };
        assert_eq!(wall("15.0", "23.0").spread(), Spread::new(15.0, 23.0));
        for (p50, p90) in [("NaN", "1.0"), ("1.0", "inf"), ("-1.0", "2.0"), ("x", "1")] {
            assert_eq!(wall(p50, p90).spread(), None, "{p50} {p90}");
        }
        assert_eq!(Spread::new(f64::NAN, 1.0), None);
    }

    #[test] // spec: CORE-161
    fn money_adds_only_within_a_currency() {
        let money = |p50: &str, p90: &str, currency: &str| MoneyPart {
            source: EstimateSource::PricesYaml,
            p50: p50.into(),
            p90: p90.into(),
            upper_bound: p90.into(),
            currency: currency.into(),
        };
        let (a, b, c) = (
            money("1", "4", "USD"),
            money("2", "6", "USD"),
            money("5", "5", "EUR"),
        );
        let sums = sum_money(&[Some(&a), None, Some(&c), Some(&b)]);
        assert_eq!(sums.len(), 2);
        assert_eq!(sums[0].currency, "EUR");
        assert_eq!(sums[0].sum.total, Spread::new(5.0, 5.0));
        assert_eq!(sums[1].currency, "USD");
        assert_eq!(sums[1].sum.total, Spread::new(3.0, 8.0));
        assert_eq!((sums[1].sum.of, sums[1].sum.not_estimated), (4, 1));
        assert!(sum_money(&[None]).is_empty());
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
