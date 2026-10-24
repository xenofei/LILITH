//! Energy arithmetic (CORE-063, CORE-064): counters that reset, integer accumulation that never
//! drifts, the whole-system sum, and the idle median. Pure functions, no I/O.

/// Integer accumulation of a raw quantity into whole output units, so no rounding builds up:
/// the output total is always `floor(total_raw × num / den)`, and each call returns what that
/// total grew by. The Energy Meter's picowatt-hours (`36 / 10⁷` to mJ, CORE-063 and
/// `pwh_to_mj`) and an integrated power (`mW·ms / 1000` to mJ) both use it.
#[derive(Debug, Clone)]
pub(crate) struct Accum {
    total: u128,
    num: u128,
    den: u128,
    reported: u128,
}

impl Accum {
    pub(crate) fn new(num: u128, den: u128) -> Accum {
        Accum {
            total: 0,
            num,
            den,
            reported: 0,
        }
    }

    /// Adds `raw`; returns the whole output units gained.
    pub(crate) fn add(&mut self, raw: u64) -> u64 {
        self.total += u128::from(raw);
        let out = self.total * self.num / self.den;
        let gained = out - self.reported;
        self.reported = out;
        u64::try_from(gained).unwrap_or(u64::MAX)
    }
}

/// Picowatt-hours → millijoules accumulation.
pub(crate) fn pwh_accum() -> Accum {
    Accum::new(36, 10_000_000)
}

/// Integrated power (mW × ms) → millijoules accumulation.
pub(crate) fn power_accum() -> Accum {
    Accum::new(1, 1000)
}

/// What a monotonically growing hardware counter did since its previous reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CounterStep {
    /// No earlier reading to compare with: this one is the baseline.
    Baseline,
    /// The counter grew by this much.
    Delta(u64),
    /// The counter went down (a driver reload or a reboot reset it): the interval's delta is
    /// the new value, and it is the new baseline (CORE-063).
    Reset(u64),
}

/// Follows one counter.
#[derive(Debug, Default, Clone)]
pub(crate) struct CounterTracker {
    prev: Option<u64>,
}

impl CounterTracker {
    pub(crate) fn step(&mut self, cur: u64) -> CounterStep {
        match self.prev.replace(cur) {
            None => CounterStep::Baseline,
            Some(prev) if cur >= prev => CounterStep::Delta(cur - prev),
            Some(_) => CounterStep::Reset(cur),
        }
    }

    /// The source was lost: the next reading is a baseline again.
    pub(crate) fn forget(&mut self) {
        self.prev = None;
    }
}

/// `util_permille` of `tdp_mw`: the CPU estimate where there is no Energy Meter (CORE-059).
pub(crate) fn cpu_estimate_mw(util_permille: u16, tdp_mw: u32) -> u32 {
    u32::try_from(u64::from(util_permille.min(1000)) * u64::from(tdp_mw) / 1000).unwrap_or(u32::MAX)
}

/// The whole-system figure (CORE-064): `(gpu + cpu + rest) / psu_efficiency`, rounded down. An
/// unknown part is `None` and left out. `None` when nothing is known at all (no part and no
/// rest-of-system baseline).
pub(crate) fn system_power_mw(
    gpu_mw: Option<u32>,
    cpu_mw: Option<u32>,
    rest_mw: u32,
    psu_permille: u32,
) -> Option<u64> {
    if gpu_mw.is_none() && cpu_mw.is_none() && rest_mw == 0 {
        return None;
    }
    let sum = u64::from(gpu_mw.unwrap_or(0)) + u64::from(cpu_mw.unwrap_or(0)) + u64::from(rest_mw);
    Some(sum * 1000 / u64::from(psu_permille.max(1)))
}

/// `v × num / den`, rounded half to even; 0 when `den` is 0 (as `meter`'s attribution shares
/// round; `telemetry` uses no `meter` item).
pub(crate) fn share(v: u64, num: u64, den: u64) -> u64 {
    if den == 0 {
        return 0;
    }
    let (p, d) = (u128::from(v) * u128::from(num), u128::from(den));
    let (q, rem) = (p / d, p % d);
    let up = match (2 * rem).cmp(&d) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => q % 2 == 1,
        std::cmp::Ordering::Less => false,
    };
    u64::try_from(q + u128::from(up)).unwrap_or(u64::MAX)
}

/// The median of `values`, an even count averaging its two middle values (rounded down);
/// `None` for no values.
pub(crate) fn median(values: &mut [u64]) -> Option<u64> {
    values.sort_unstable();
    let n = values.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(values[n / 2]),
        _ => Some((values[n / 2 - 1] + values[n / 2]) / 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Energy Meter's raw picowatt-hours accumulate as integers and convert once:
    /// 0 → 1e6 → 2e6 → 3e6 pWh is 10 mJ in all, where converting each 3.6 mJ step on its own
    /// would give 9.
    #[test] // spec: CORE-063
    fn energy_meter_pwh_accumulates_raw_then_converts() {
        let mut tracker = CounterTracker::default();
        let mut acc = pwh_accum();
        let mut gained = Vec::new();
        for raw in [0_u64, 1_000_000, 2_000_000, 3_000_000] {
            let delta = match tracker.step(raw) {
                CounterStep::Baseline => 0,
                CounterStep::Delta(d) | CounterStep::Reset(d) => d,
            };
            gained.push(acc.add(delta));
        }
        assert_eq!(gained, [0, 3, 4, 3]);
        assert_eq!(gained.iter().sum::<u64>(), 10);
        assert_eq!(
            lil_platform::hw::pwh_to_mj(3_000_000),
            10,
            "the accumulator and pwh_to_mj agree"
        );
    }

    /// A counter that goes down was reset: the interval's delta is the new value, never
    /// negative, and the new value is the baseline (CORE-063).
    #[test] // spec: CORE-063
    fn counter_reset_rebaselines_without_negative_energy() {
        let mut tracker = CounterTracker::default();
        let steps: Vec<CounterStep> = [1000, 2000, 100, 300, 300]
            .into_iter()
            .map(|v| tracker.step(v))
            .collect();
        assert_eq!(
            steps,
            [
                CounterStep::Baseline,
                CounterStep::Delta(1000),
                CounterStep::Reset(100),
                CounterStep::Delta(200),
                CounterStep::Delta(0),
            ]
        );
        tracker.forget();
        assert_eq!(tracker.step(50), CounterStep::Baseline);
    }

    /// Integrated power keeps its remainders: ten steps of 1999 mW·ms are 19 mJ in all, where
    /// truncating each step would give 10.
    #[test] // spec: CORE-063
    fn integrated_power_does_not_lose_remainders() {
        let mut acc = power_accum();
        let total: u64 = (0..10).map(|_| acc.add(1999)).sum();
        assert_eq!(total, 19);
        let mut whole = power_accum();
        assert_eq!((0..3).map(|_| whole.add(1500 * 400)).sum::<u64>(), 1800);
    }

    #[test] // spec: CORE-064
    fn whole_system_divides_by_psu_efficiency() {
        // GPU 200000 mW + CPU 60000 mW + rest 0 at 900 ‰ is 288888.8…, rounded down.
        assert_eq!(
            system_power_mw(Some(200_000), Some(60_000), 0, 900),
            Some(288_888)
        );
        assert_eq!(
            system_power_mw(Some(200_000), None, 20_000, 1000),
            Some(220_000)
        );
        assert_eq!(system_power_mw(None, None, 0, 900), None);
        assert_eq!(system_power_mw(None, None, 9_000, 900), Some(10_000));
    }

    #[test] // spec: CORE-059
    fn cpu_estimate_is_utilisation_times_tdp() {
        assert_eq!(cpu_estimate_mw(500, 65_000), 32_500);
        assert_eq!(cpu_estimate_mw(0, 65_000), 0);
        assert_eq!(cpu_estimate_mw(5000, 65_000), 65_000, "capped at 100 %");
    }

    #[test] // spec: CORE-065
    fn median_of_odd_and_even_counts() {
        assert_eq!(median(&mut [44, 40, 42]), Some(42));
        assert_eq!(median(&mut [40, 44, 41, 100]), Some(42));
        assert_eq!(median(&mut []), None);
    }
}
