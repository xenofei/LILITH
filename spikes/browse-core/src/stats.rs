//! Series summaries: n, p50, p95, p99, max (spikes/README.md rule 3), nearest-rank percentiles.

use serde_json::{Value, json};

/// The nearest-rank percentile `p` (0–100) of `samples`, or `None` when empty.
pub fn percentile(samples: &[f64], p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let rank = ((p / 100.0) * s.len() as f64).ceil() as usize;
    Some(s[rank.clamp(1, s.len()) - 1])
}

/// The median (nearest-rank p50).
pub fn median(samples: &[f64]) -> Option<f64> {
    percentile(samples, 50.0)
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// `{n, p50, p95, p99, max, mean}` of `samples`, rounded to 0.001.
pub fn summary(samples: &[f64]) -> Value {
    let n = samples.len();
    let max = samples.iter().copied().fold(f64::NAN, f64::max);
    let mean = if n == 0 {
        f64::NAN
    } else {
        samples.iter().sum::<f64>() / n as f64
    };
    let p = |q: f64| percentile(samples, q).map(round3);
    json!({
        "n": n,
        "p50": p(50.0),
        "p95": p(95.0),
        "p99": p(99.0),
        "max": if n == 0 { None } else { Some(round3(max)) },
        "mean": if n == 0 { None } else { Some(round3(mean)) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let s: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&s, 50.0), Some(50.0));
        assert_eq!(percentile(&s, 95.0), Some(95.0));
        assert_eq!(percentile(&s, 99.0), Some(99.0));
        assert_eq!(percentile(&s, 100.0), Some(100.0));
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 50.0), Some(2.0));
        assert_eq!(percentile(&[7.0], 99.0), Some(7.0));
        assert_eq!(percentile(&[], 50.0), None);
    }
}
