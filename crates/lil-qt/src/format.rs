//! The Jobs panel's and the cost and time chip's words for estimates and times (APP-114,
//! APP-140; CORE-161, P4a-R40): Qt-free, so the bridge's `JobText` (`src/qt/job_text.rs`) only
//! parses JSON and calls these.
//!
//! - [`chip_text`]: the chip, from the estimate's parts: `local` reads "local · ~8 s · ~2 Wh"
//!   (GPU-seconds and watt-hours, their p50s), `money` "≈ $0.04 · 3 % of cap" (its p50, and the
//!   share of the monthly cap when there is one), both joined by " · "; with neither but a
//!   `wall_s` part, "~8 s"; with no part at all, "no estimate yet". `unknown` entries add
//!   nothing.
//! - [`eta_text`]: a running job's time left, its `wall_s` p50 less the time it has run, never
//!   below 0 ("about 12 s left", "under 1 s left"), or "—" with no `wall_s`.
//! - [`queued_eta_text`]: a queued job's finish (Appendix G "Adding estimates"): the running
//!   job's time left, plus the `wall_s` of each job ahead in lane order, plus its own, summed by
//!   [`sum_part`]; jobs among them with no `wall_s` are counted and named ("about 3 min, after 2
//!   jobs with no estimate"), never taken as 0, and nothing estimated reads as not estimated.
//! - [`elapsed_text`]: how long a job has run ("12 s", "3 min 4 s", "1 h 2 min").
//! - [`cap_share_permille`]: the share of the `monthly` cap an estimate's `money` p50 is, when
//!   the cap is in the estimate's currency.

use lil_protocol::app::CapsView;
use lil_protocol::estimate::{Estimate, MoneyPart, Spread, sum_part};

/// What an estimate with no part reads (APP-140).
pub const NO_ESTIMATE: &str = "no estimate yet";
/// A time left, or a figure, that can't be told.
pub const NO_TIME: &str = "—";

/// The chip's text (module docs). `cap_share_permille` is the share of the cap the estimate's
/// money is ([`cap_share_permille`]), shown only with a `money` part.
pub fn chip_text(e: &Estimate, cap_share_permille: Option<u32>) -> String {
    let mut parts = Vec::new();
    if let Some(local) = &e.local
        && let (Some(gpu), Some(wh)) = (local.gpu_s(), local.wh())
    {
        parts.push("local".to_string());
        parts.push(approx_secs(gpu.p50()));
        parts.push(approx_wh(wh.p50()));
    }
    if let Some(text) = e.money.as_ref().and_then(money_text) {
        parts.push(text);
        if let Some(permille) = cap_share_permille {
            parts.push(share_text(permille));
        }
    }
    if parts.is_empty()
        && let Some(wall) = e.wall_s.as_ref().and_then(|w| w.spread())
    {
        parts.push(approx_secs(wall.p50()));
    }
    if parts.is_empty() {
        return NO_ESTIMATE.to_string();
    }
    parts.join(" · ")
}

/// A running job's time left (module docs).
pub fn eta_text(e: &Estimate, elapsed_s: f64) -> String {
    match running_left(e, elapsed_s) {
        None => NO_TIME.to_string(),
        Some(left) if left < 0.5 => "under 1 s left".to_string(),
        Some(left) => format!("about {} left", duration(left)),
    }
}

/// A running job's seconds left by its `wall_s` p50, never below 0; `None` without `wall_s`.
pub fn running_left(e: &Estimate, elapsed_s: f64) -> Option<f64> {
    let wall = e.wall_s.as_ref()?.spread()?;
    let elapsed = if elapsed_s.is_finite() {
        elapsed_s
    } else {
        0.0
    };
    Some((wall.p50() - elapsed).max(0.0))
}

/// A queued job's finish (module docs). `running_left` is the time left of the job running in
/// its lane when that job has a `wall_s` part ([`running_left`]); a running job without one is
/// an entry of `ahead` with `None`, as is each job ahead that the caller can't see. `ahead` is
/// the jobs before it in lane order, `own` its own `wall_s`.
pub fn queued_eta_text(
    running_left: Option<f64>,
    ahead: &[Option<&Spread>],
    own: Option<&Spread>,
) -> String {
    if ahead.is_empty() && running_left.is_none() && own.is_none() {
        return NO_ESTIMATE.to_string();
    }
    let mut jobs: Vec<Option<&Spread>> = ahead.to_vec();
    jobs.push(own);
    let sum = sum_part(&jobs);
    let running = running_left.filter(|s| s.is_finite()).map(|s| s.max(0.0));
    let unestimated = match sum.not_estimated {
        0 => None,
        1 => Some("after 1 job with no estimate".to_string()),
        n => Some(format!("after {n} jobs with no estimate")),
    };
    let known = match (sum.total, running) {
        (None, None) => None,
        (total, running) => Some(total.map_or(0.0, |t| t.p50()) + running.unwrap_or(0.0)),
    };
    match (known, unestimated) {
        (Some(t), None) => format!("about {}", duration(t)),
        (Some(t), Some(rest)) => format!("about {}, {rest}", duration(t)),
        (None, Some(rest)) => rest,
        (None, None) => NO_ESTIMATE.to_string(),
    }
}

/// How long a job has run (module docs); a negative or non-finite time reads "0 s".
pub fn elapsed_text(s: f64) -> String {
    let s = if s.is_finite() && s > 0.0 {
        s.floor() as u64
    } else {
        0
    };
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

/// The share of the `monthly` cap, in permille rounded to the nearest, that `e`'s `money` p50
/// is: `None` without a `money` part, without a monthly cap, when the cap's currency (the
/// display currency) isn't the estimate's, or when either figure doesn't read.
pub fn cap_share_permille(e: &Estimate, caps: &CapsView) -> Option<u32> {
    let money = e.money.as_ref()?;
    if money.currency != caps.display_currency {
        return None;
    }
    let p50 = money.spread()?.p50();
    let cap = caps
        .caps
        .iter()
        .find(|c| c.cap == "monthly" && c.invalid != Some(true))?;
    let limit: f64 = cap.limit.parse().ok()?;
    if !limit.is_finite() || limit <= 0.0 {
        return None;
    }
    let permille = (p50 / limit * 1000.0).round();
    (permille.is_finite() && permille >= 0.0).then(|| permille.min(f64::from(u32::MAX)) as u32)
}

/// Whole seconds, minutes, or hours and minutes.
fn duration(s: f64) -> String {
    let r = s.round();
    if r < 60.0 {
        return format!("{} s", r as u64);
    }
    if s < 3570.0 {
        return format!("{} min", (s / 60.0).round() as u64);
    }
    let minutes = (s / 60.0).round() as u64;
    match minutes % 60 {
        0 => format!("{} h", minutes / 60),
        m => format!("{} h {m} min", minutes / 60),
    }
}

fn approx_secs(s: f64) -> String {
    if s < 0.5 {
        "<1 s".to_string()
    } else {
        format!("~{}", duration(s))
    }
}

fn approx_wh(wh: f64) -> String {
    if wh < 0.05 {
        "<0.1 Wh".to_string()
    } else if wh < 0.95 {
        format!("~{wh:.1} Wh")
    } else {
        format!("~{} Wh", wh.round() as u64)
    }
}

fn share_text(permille: u32) -> String {
    if permille > 0 && permille < 5 {
        "<1 % of cap".to_string()
    } else {
        format!("{} % of cap", (f64::from(permille) / 10.0).round() as u64)
    }
}

/// "≈ $0.04", "≈ €1.50", "≈ 2.00 CHF"; under a cent, "< $0.01".
fn money_text(m: &MoneyPart) -> Option<String> {
    let amount = m.spread()?.p50();
    let symbol = match m.currency.as_str() {
        "USD" => Some("$"),
        "EUR" => Some("€"),
        "GBP" => Some("£"),
        _ => None,
    };
    let (shown, prefix) = if amount > 0.0 && amount < 0.005 {
        (0.01, "< ")
    } else {
        (amount, "≈ ")
    };
    Some(match symbol {
        Some(sym) => format!("{prefix}{sym}{shown:.2}"),
        None => format!("{prefix}{shown:.2} {}", m.currency),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_protocol::app::CapRow;
    use lil_protocol::estimate::{
        EstimateSource, LocalPart, PartName, UnknownPart, UnknownReason, WallPart,
    };

    fn local(gpu: (&str, &str), wh: (&str, &str)) -> LocalPart {
        LocalPart {
            source: EstimateSource::MeasuredHistory,
            samples: 5,
            gpu_s_p50: gpu.0.into(),
            gpu_s_p90: gpu.1.into(),
            wh_p50: wh.0.into(),
            wh_p90: wh.1.into(),
            energy_cost: None,
        }
    }

    fn wall(p50: &str, p90: &str) -> WallPart {
        WallPart {
            source: EstimateSource::MeasuredHistory,
            samples: 5,
            p50: p50.into(),
            p90: p90.into(),
        }
    }

    fn money(p50: &str, currency: &str) -> MoneyPart {
        MoneyPart {
            source: EstimateSource::PricesYaml,
            p50: p50.into(),
            p90: p50.into(),
            upper_bound: p50.into(),
            currency: currency.into(),
        }
    }

    fn caps(display: &str, monthly: Option<&str>) -> CapsView {
        CapsView {
            display_currency: display.into(),
            caps: monthly
                .map(|limit| CapRow {
                    cap: "monthly".into(),
                    limit: limit.into(),
                    settled: "0".into(),
                    held: "0".into(),
                    ratio_permille: 0,
                    invalid: None,
                })
                .into_iter()
                .collect(),
            period: "2026-10".into(),
        }
    }

    #[test] // spec: APP-140, CORE-161
    fn chip_text_local() {
        let e = Estimate {
            local: Some(local(("8.2", "11.0"), ("2.104", "2.900"))),
            wall_s: Some(wall("9.0", "12.0")),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&e, None), "local · ~8 s · ~2 Wh");
        // The cap share is money's: a local estimate never shows one.
        assert_eq!(chip_text(&e, Some(30)), "local · ~8 s · ~2 Wh");
        // Small and large figures.
        let e = Estimate {
            local: Some(local(("0.4", "0.6"), ("0.040", "0.080"))),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&e, None), "local · <1 s · <0.1 Wh");
        let e = Estimate {
            local: Some(local(("185.0", "240.0"), ("0.512", "0.700"))),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&e, None), "local · ~3 min · ~0.5 Wh");
    }

    #[test] // spec: APP-140, CORE-161
    fn chip_text_money_with_its_cap_share() {
        let e = Estimate {
            money: Some(money("0.04", "USD")),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&e, Some(30)), "≈ $0.04 · 3 % of cap");
        assert_eq!(chip_text(&e, None), "≈ $0.04");
        assert_eq!(chip_text(&e, Some(4)), "≈ $0.04 · <1 % of cap");
        let eur = Estimate {
            money: Some(money("1.5", "EUR")),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&eur, None), "≈ €1.50");
        let chf = Estimate {
            money: Some(money("0.001", "CHF")),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&chf, None), "< 0.01 CHF");
        // Both parts, local first.
        let both = Estimate {
            local: Some(local(("8.0", "9.0"), ("2.000", "2.500"))),
            money: Some(money("0.04", "USD")),
            ..Estimate::default()
        };
        assert_eq!(
            chip_text(&both, Some(30)),
            "local · ~8 s · ~2 Wh · ≈ $0.04 · 3 % of cap"
        );

        // The share is of the monthly cap, in its currency only.
        assert_eq!(cap_share_permille(&e, &caps("USD", Some("1.33"))), Some(30));
        assert_eq!(cap_share_permille(&e, &caps("USD", None)), None);
        assert_eq!(cap_share_permille(&e, &caps("EUR", Some("1.33"))), None);
        assert_eq!(cap_share_permille(&e, &caps("USD", Some("0"))), None);
        assert_eq!(
            cap_share_permille(&Estimate::default(), &caps("USD", Some("5"))),
            None
        );
    }

    #[test] // spec: APP-140, CORE-161
    fn chip_text_none() {
        // An estimate with no part is "no estimate yet", whatever it lists as unknown.
        assert_eq!(chip_text(&Estimate::default(), None), NO_ESTIMATE);
        let unknown = Estimate {
            unknown: vec![
                UnknownPart {
                    part: PartName::Local,
                    reason: UnknownReason::NoHistory,
                },
                UnknownPart {
                    part: PartName::WallS,
                    reason: UnknownReason::NoKey,
                },
            ],
            ..Estimate::default()
        };
        assert_eq!(chip_text(&unknown, Some(30)), NO_ESTIMATE);
        // A part whose figures don't read is no estimate either.
        let bad = Estimate {
            local: Some(local(("x", "1"), ("1", "2"))),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&bad, None), NO_ESTIMATE);
        // Only wall time: its seconds.
        let only_wall = Estimate {
            wall_s: Some(wall("12.0", "15.0")),
            ..Estimate::default()
        };
        assert_eq!(chip_text(&only_wall, None), "~12 s");
    }

    /// The chip reads the `local` part as the flat estimate read its GPU-seconds and Wh: the
    /// text of a plan-4a estimate is the one the flat estimate gave (CORE-161: "the chip's text
    /// doesn't change").
    #[test] // spec: CORE-161
    fn chip_text_reads_the_local_part() {
        let e: Estimate = serde_json::from_value(serde_json::json!({
            "local": {"source": "measured_history", "samples": 4, "gpu_s_p50": "8.0",
                      "gpu_s_p90": "10.0", "wh_p50": "2.000", "wh_p90": "2.400"},
            "wall_s": {"source": "measured_history", "samples": 4, "p50": "30.0", "p90": "40.0"},
            "unknown": []
        }))
        .unwrap();
        // The GPU-seconds, never the wall time.
        assert_eq!(chip_text(&e, None), "local · ~8 s · ~2 Wh");
    }

    #[test] // spec: APP-114, CORE-161
    fn eta_never_goes_below_zero() {
        let e = Estimate {
            wall_s: Some(wall("20.0", "30.0")),
            ..Estimate::default()
        };
        assert_eq!(eta_text(&e, 8.0), "about 12 s left");
        assert_eq!(eta_text(&e, 19.6), "under 1 s left");
        assert_eq!(eta_text(&e, 45.0), "under 1 s left");
        assert_eq!(running_left(&e, 45.0), Some(0.0));
        assert_eq!(running_left(&e, 5.0), Some(15.0));
        let long = Estimate {
            wall_s: Some(wall("400.0", "500.0")),
            ..Estimate::default()
        };
        assert_eq!(eta_text(&long, 10.0), "about 7 min left");
        // No wall time: no ETA, and the GPU-seconds aren't one.
        let local_only = Estimate {
            local: Some(local(("8.0", "9.0"), ("2.0", "2.5"))),
            ..Estimate::default()
        };
        assert_eq!(eta_text(&local_only, 1.0), NO_TIME);
        assert_eq!(running_left(&local_only, 1.0), None);
        assert_eq!(eta_text(&Estimate::default(), 1.0), NO_TIME);
    }

    #[test] // spec: APP-114, CORE-161
    fn queued_eta_adds_the_work_ahead_and_counts_unestimated_jobs() {
        let s = |p50: f64, p90: f64| Spread::new(p50, p90).unwrap();
        let (a, b, own) = (s(60.0, 80.0), s(90.0, 100.0), s(20.0, 30.0));
        // 10 s left of the running job, 60 + 90 ahead, 20 its own: 180 s.
        assert_eq!(
            queued_eta_text(Some(10.0), &[Some(&a), Some(&b)], Some(&own)),
            "about 3 min"
        );
        // Two jobs ahead without wall_s: counted and named, never taken as 0.
        assert_eq!(
            queued_eta_text(Some(10.0), &[Some(&a), None, Some(&b), None], Some(&own)),
            "about 3 min, after 2 jobs with no estimate"
        );
        assert_eq!(
            queued_eta_text(None, &[None, Some(&a)], Some(&own)),
            "about 1 min, after 1 job with no estimate"
        );
        // Nothing estimated: not estimated, never 0.
        assert_eq!(
            queued_eta_text(None, &[None, None], None),
            "after 3 jobs with no estimate"
        );
        assert_eq!(queued_eta_text(None, &[], None), NO_ESTIMATE);
        // First in line, nothing running.
        assert_eq!(queued_eta_text(None, &[], Some(&own)), "about 20 s");
        // A running job past its estimate adds 0, never less.
        assert_eq!(queued_eta_text(Some(0.0), &[], Some(&own)), "about 20 s");
    }

    #[test] // spec: APP-114
    fn eta_elapsed_text_counts_up() {
        assert_eq!(elapsed_text(0.0), "0 s");
        assert_eq!(elapsed_text(12.7), "12 s");
        assert_eq!(elapsed_text(184.0), "3 min 4 s");
        assert_eq!(elapsed_text(3720.0), "1 h 2 min");
        assert_eq!(elapsed_text(-3.0), "0 s");
        assert_eq!(elapsed_text(f64::NAN), "0 s");
    }
}
