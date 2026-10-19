//! Per-job energy attribution (CORE-065; Appendix F "Telemetry"): a job's ComfyUI execution
//! windows, each with the engine's processes when it closed, and the telemetry of each window
//! ([`EnergySlice`], from `Telemetry::energy_between`).
//!
//! Within a window:
//! - the GPU energy is split by the engine processes' share of SM utilisation, over every
//!   process the GPU reported (a game or a browser alongside keeps its own share);
//! - the CPU energy is split by their share of CPU time, over the machine's CPU time: the
//!   slice's `cpu_ms_by_pid` holds every untracked process together under pid 0, so its sum,
//!   pid 0 included, is the denominator (never the tracked processes alone);
//! - above idle is the window's energy less its idle baseline (idle power × the window's
//!   length, never below 0), split by the same share; without a baseline the above-idle figure
//!   is the gross one and the result says `idle_unknown`;
//! - GPU-seconds are SM-weighted: the engine's SM permille-seconds, which are milli-GPU-seconds;
//! - the whole system is (GPU + CPU + `[power] rest_of_system_w` × the window's length) /
//!   `psu_efficiency` (CORE-064), with a CPU that can't be measured left out (it is 0).
//!
//! Shares are integer ratios, rounded half to even to a millijoule.

use std::collections::BTreeSet;

use crate::jobs::ExecWindow;
use crate::telemetry::{EnergySlice, JobEnergy, Measured, PowerSettings};

/// `v × num / den`, rounded half to even; 0 when `den` is 0.
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

/// `mw` for `ms` milliseconds, in mJ.
fn mw_for(mw: u32, ms: u64) -> u64 {
    u64::from(mw).saturating_mul(ms) / 1000
}

/// A job's energy over its execution windows (module docs). `slices[i]` is the telemetry of
/// `windows[i]` (a missing slice counts as an empty one). `from` and `to` are left empty: they
/// are wall times, which the caller maps from the first window's start and the last one's end
/// (`Telemetry::wall_ms`).
pub fn attribute(
    job_id: &str,
    workspace: &str,
    windows: &[ExecWindow],
    slices: &[EnergySlice],
    power: &PowerSettings,
) -> JobEnergy {
    let empty = EnergySlice::default();
    let mut e = JobEnergy {
        job_id: job_id.to_string(),
        workspace: workspace.to_string(),
        from: String::new(),
        to: String::new(),
        gpu_mj_gross: 0,
        gpu_mj_above_idle: 0,
        cpu_mj_gross: 0,
        cpu_mj_above_idle: 0,
        system_mj: 0,
        gpu_seconds_milli: 0,
        measured: Measured {
            gpu: !windows.is_empty(),
            cpu: !windows.is_empty(),
        },
        idle_unknown: false,
    };
    let mut rest_mj: u64 = 0;
    for (i, w) in windows.iter().enumerate() {
        let s = slices.get(i).unwrap_or(&empty);
        let ms = u64::try_from(w.end.saturating_sub(w.start).as_millis()).unwrap_or(u64::MAX);
        let pids: BTreeSet<u32> = w.engine_pids.iter().copied().filter(|p| *p != 0).collect();
        let engine = |by_pid: &std::collections::BTreeMap<u32, u64>| -> u64 {
            pids.iter().filter_map(|p| by_pid.get(p)).sum()
        };

        let sm_all: u64 = s.sm_by_pid.values().sum();
        let sm_engine = engine(&s.sm_by_pid);
        let gpu_above = s
            .idle_gpu_mw
            .map_or(s.gpu_mj, |idle| s.gpu_mj.saturating_sub(mw_for(idle, ms)));
        e.gpu_mj_gross += share(s.gpu_mj, sm_engine, sm_all);
        e.gpu_mj_above_idle += share(gpu_above, sm_engine, sm_all);
        e.gpu_seconds_milli += sm_engine;

        let cpu_all: u64 = s.cpu_ms_by_pid.values().sum();
        let cpu_engine = engine(&s.cpu_ms_by_pid);
        let cpu_above = s
            .idle_cpu_mw
            .map_or(s.cpu_mj, |idle| s.cpu_mj.saturating_sub(mw_for(idle, ms)));
        e.cpu_mj_gross += share(s.cpu_mj, cpu_engine, cpu_all);
        e.cpu_mj_above_idle += share(cpu_above, cpu_engine, cpu_all);

        e.measured.gpu &= s.gpu_measured;
        e.measured.cpu &= s.cpu_measured;
        e.idle_unknown |=
            (s.idle_gpu_mw.is_none() && s.gpu_mj > 0) || (s.idle_cpu_mw.is_none() && s.cpu_mj > 0);
        rest_mj += mw_for(power.rest_of_system_mw, ms);
    }
    let psu = u64::from(power.psu_efficiency_permille.clamp(1, 1000));
    e.system_mj = share(e.gpu_mj_gross + e.cpu_mj_gross + rest_mj, 1000, psu);
    e
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;

    #[test] // spec: CORE-065
    fn shares_round_half_to_even() {
        assert_eq!(share(10, 1, 4), 2, "2.5 → 2");
        assert_eq!(share(14, 1, 4), 4, "3.5 → 4");
        assert_eq!(share(7, 0, 0), 0, "no denominator, no share");
    }

    #[test] // spec: CORE-064
    fn the_whole_system_adds_the_rest_and_divides_by_the_psu() {
        let w = ExecWindow {
            engine_pids: vec![7],
            start: Duration::from_secs(0),
            end: Duration::from_secs(2),
            ok: true,
        };
        let s = EnergySlice {
            gpu_mj: 900,
            sm_by_pid: BTreeMap::from([(7, 1000)]),
            gpu_measured: true,
            idle_gpu_mw: Some(0),
            ..EnergySlice::default()
        };
        let power = PowerSettings {
            rest_of_system_mw: 0,
            ..PowerSettings::default()
        };
        assert_eq!(
            attribute(
                "j",
                "w",
                std::slice::from_ref(&w),
                std::slice::from_ref(&s),
                &power
            )
            .system_mj,
            1000
        );
        let with_rest = PowerSettings {
            rest_of_system_mw: 45,
            ..PowerSettings::default()
        };
        // (900 + 45 mW × 2 s) / 0.9 = 1100.
        assert_eq!(attribute("j", "w", &[w], &[s], &with_rest).system_mj, 1100);
    }
}
