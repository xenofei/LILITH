//! S0.4's analysis (P4b-R19, P4b-R37), pure: the idle level, each fitting run's maximum above
//! idle, each spilling run's 10 s sustained level above idle, and the margin.

use serde_json::{Value, json};

/// One phase of the run: `idle`, `fit` or `spill`, from `start_ms` to `end_ms` (Unix ms).
/// `idle` 0 is the cold idle after start (the verdict's); `idle` 1, after the fitting runs, is
/// the warm idle (recorded).
#[derive(Debug, Clone, PartialEq)]
pub struct Phase {
    pub kind: String,
    pub index: u32,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// The values of the samples `(t_ms, value)` inside `p`, in time order.
fn inside(samples: &[(u64, f64)], p: &Phase) -> Vec<f64> {
    samples
        .iter()
        .filter(|(t, _)| *t >= p.start_ms && *t <= p.end_ms)
        .map(|(_, v)| *v)
        .collect()
}

/// The largest minimum over any `window` consecutive values (the level held for the whole
/// window), or `None` when there are fewer than `window` values.
pub fn sustained(values: &[f64], window: usize) -> Option<f64> {
    if window == 0 || values.len() < window {
        return None;
    }
    values
        .windows(window)
        .map(|w| w.iter().copied().fold(f64::INFINITY, f64::min))
        .max_by(|a, b| a.total_cmp(b))
}

/// The margin for the gap `(low, high)`: a multiple of 128 MiB strictly inside it, the
/// midpoint rounded down when that is above `low`, else the smallest multiple above `low`;
/// `None` when no multiple of 128 lies strictly inside.
pub fn margin(low: f64, high: f64) -> Option<f64> {
    if low >= high {
        return None;
    }
    let down = ((low + high) / 2.0 / 128.0).floor() * 128.0;
    if down > low {
        return Some(down);
    }
    let up = ((low / 128.0).floor() + 1.0) * 128.0;
    (up < high).then_some(up)
}

/// The levels above `idle`: each fitting run's maximum and each spilling run's sustained level.
fn measure(samples: &[(u64, f64)], phases: &[Phase], window: usize, idle: f64) -> Value {
    let of = |kind: &'static str| phases.iter().filter(move |p| p.kind == kind);
    let fit: Vec<(u32, Option<f64>)> = of("fit")
        .map(|p| {
            let v = inside(samples, p);
            let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (p.index, max.is_finite().then_some(max - idle))
        })
        .collect();
    let spill: Vec<(u32, Option<f64>)> = of("spill")
        .map(|p| {
            (
                p.index,
                sustained(&inside(samples, p), window).map(|s| s - idle),
            )
        })
        .collect();
    let complete = !fit.is_empty()
        && !spill.is_empty()
        && fit.iter().all(|f| f.1.is_some())
        && spill.iter().all(|s| s.1.is_some());
    let low = fit
        .iter()
        .filter_map(|f| f.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let high = spill
        .iter()
        .filter_map(|s| s.1)
        .fold(f64::INFINITY, f64::min);
    let (verdict, why, gap, m) = if !complete {
        (
            "INVALID",
            Some("a run has too few samples"),
            Value::Null,
            None,
        )
    } else if low >= high {
        ("FAIL", Some("no gap: the runs overlap"), Value::Null, None)
    } else {
        let gap = json!({"low": low, "high": high});
        match margin(low, high) {
            Some(m) => ("PASS", None, gap, Some(m)),
            None => (
                "FAIL",
                Some("gap narrower than the 128 MiB step"),
                gap,
                None,
            ),
        }
    };
    json!({
        "idle_mib": idle,
        "fit": fit.iter().map(|(i, v)| json!({"index": i, "max_above_idle_mib": v})).collect::<Vec<_>>(),
        "spill": spill.iter().map(|(i, v)| json!({"index": i, "sustained_above_idle_mib": v})).collect::<Vec<_>>(),
        "fit_max_mib": if low.is_finite() { json!(low) } else { Value::Null },
        "spill_min_mib": if high.is_finite() { json!(high) } else { Value::Null },
        "gap": gap,
        "margin_mib": m,
        "why": why,
        "verdict": verdict,
    })
}

/// The minimum over the samples of the `idle` phases with `index`.
fn idle_level(samples: &[(u64, f64)], phases: &[Phase], index: u32) -> Option<(f64, usize)> {
    let v: Vec<f64> = phases
        .iter()
        .filter(|p| p.kind == "idle" && p.index == index)
        .flat_map(|p| inside(samples, p))
        .collect();
    let min = v.iter().copied().fold(f64::INFINITY, f64::min);
    min.is_finite().then_some((min, v.len()))
}

/// The analysis against the cold idle (`idle` 0; the verdict), with the warm idle's margin
/// (`idle` 1) recorded beside it when that phase exists. `window` is the number of consecutive
/// samples that make 10 s.
pub fn analyse(samples: &[(u64, f64)], phases: &[Phase], window: usize) -> Value {
    let Some((idle, n)) = idle_level(samples, phases, 0) else {
        return json!({"verdict": "INVALID", "why": "no idle samples"});
    };
    let mut r = measure(samples, phases, window, idle);
    r["idle_samples"] = json!(n);
    r["window_samples"] = json!(window);
    if let Some((warm, _)) = idle_level(samples, phases, 1) {
        let w = measure(samples, phases, window, warm);
        r["warm_idle_mib"] = json!(warm);
        r["margin_vs_warm_idle_mib"] = w["margin_mib"].clone();
        r["gap_vs_warm_idle"] = w["gap"].clone();
        r["verdict_vs_warm_idle"] = w["verdict"].clone();
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ph(kind: &str, index: u32, start: u64, end: u64) -> Phase {
        Phase {
            kind: kind.into(),
            index,
            start_ms: start,
            end_ms: end,
        }
    }

    #[test]
    fn sustained_is_the_largest_window_minimum() {
        assert_eq!(
            sustained(&[1.0, 5.0, 5.0, 2.0, 6.0, 6.0, 6.0], 3),
            Some(6.0)
        );
        assert_eq!(sustained(&[1.0, 9.0, 1.0, 9.0], 2), Some(1.0));
        assert_eq!(sustained(&[1.0], 2), None);
    }

    #[test]
    fn a_clean_gap_gives_the_margin_rounded_down() {
        // idle 100; fits peak 300 above idle; spills hold 1100 above idle for 3 samples.
        let mut s = vec![(0, 120.0), (1, 100.0), (2, 110.0)];
        s.extend([(10, 150.0), (11, 400.0), (12, 200.0)]);
        s.extend([(15, 180.0), (16, 160.0)]);
        s.extend([(20, 1300.0), (21, 1200.0), (22, 1250.0), (23, 1400.0)]);
        let phases = [
            ph("idle", 0, 0, 2),
            ph("fit", 1, 10, 12),
            ph("idle", 1, 15, 16),
            ph("spill", 1, 20, 23),
        ];
        let r = analyse(&s, &phases, 3);
        assert_eq!(r["verdict"], "PASS");
        assert_eq!(r["idle_mib"], 100.0);
        assert_eq!(r["fit_max_mib"], 300.0);
        assert_eq!(r["spill_min_mib"], 1100.0);
        // the middle is 700, rounded down to 640
        assert_eq!(r["margin_mib"], 640.0);
        // warm idle 160: fit 240, spill 1040, middle 640
        assert_eq!(r["warm_idle_mib"], 160.0);
        assert_eq!(r["margin_vs_warm_idle_mib"], 640.0);
    }

    #[test]
    fn a_narrow_gap_needs_a_multiple_of_128_inside_it() {
        // 300..310: no multiple of 128 inside, so FAIL (the midpoint rounds down to 256).
        assert_eq!(margin(300.0, 310.0), None);
        // 250..260: the midpoint rounds down to 128, below the gap; 256 is inside.
        assert_eq!(margin(250.0, 260.0), Some(256.0));
        assert_eq!(margin(256.0, 384.0), None, "both ends are excluded");
        assert_eq!(margin(300.0, 1100.0), Some(640.0));
        let s = vec![(0, 0.0), (10, 300.0), (20, 310.0), (21, 310.0)];
        let phases = [
            ph("idle", 0, 0, 0),
            ph("fit", 1, 10, 10),
            ph("spill", 1, 20, 21),
        ];
        let r = analyse(&s, &phases, 2);
        assert_eq!(r["verdict"], "FAIL");
        assert_eq!(r["why"], "gap narrower than the 128 MiB step");
        assert_eq!(r["gap"]["low"], 300.0);
    }

    #[test]
    fn overlap_fails_and_a_short_spill_is_invalid() {
        let s = vec![(0, 0.0), (10, 500.0), (20, 400.0), (21, 400.0)];
        let phases = [
            ph("idle", 0, 0, 0),
            ph("fit", 1, 10, 10),
            ph("spill", 1, 20, 21),
        ];
        assert_eq!(analyse(&s, &phases, 2)["verdict"], "FAIL");
        assert_eq!(analyse(&s, &phases, 3)["verdict"], "INVALID");
    }
}
