//! One sampling step (CORE-059–061, CORE-063, CORE-064, CORE-066): what a 1 Hz tick reads, how
//! the readings become figures, and the snapshot, the row to store and the events that follow.
//!
//! The [`Sampler`] owns the sources and every piece of state a step carries over (counter
//! baselines, the last good GPU reading, per-process CPU time, alert state); it does no file
//! I/O, so a test can drive it with fake sources. `Telemetry` calls [`Sampler::step`] once a
//! second (or once per `test.hw_tick`), stores the row, and publishes the events.
//!
//! - **Rates** ([`Cadence`]): the GPU, the Energy Meter and `sysinfo` are read at every step;
//!   per-process GPU memory every 5 s while idle and every second while jobs run; disk free
//!   space and the folder sizes every 5 minutes.
//! - **A lost GPU** (NVML gone mid-run, a driver update): the step's rows keep going. The
//!   gap's figures are estimated from the last good power (for at most a minute), the counter
//!   is forgotten, after 3 failed reads the source is opened again, and the first good reading
//!   after the gap is a new baseline whose interval is estimated too. A GPU that never read at
//!   all is unknown, not estimated.
//! - **A field the GPU doesn't have** (`power_mw` is `None`): the sample stands. Power falls
//!   back to the instantaneous reading, then to the energy counter's rate (estimated), then
//!   to unknown.
//! - **CPU power** is the Energy Meter's (measured), else utilisation × `[power] cpu_tdp_w`
//!   (estimated), else unknown, which is left out of the whole-system sum.
//! - **Counters** (CORE-063): a counter that goes down was reset and the interval's delta is the
//!   new value; the Energy Meter's raw picowatt-hours are accumulated as integers and converted
//!   once.
//! - **Adapter counters** (CORE-059, APP-127): the adapter's dedicated and shared memory and the
//!   busiest engine per type are read at every step; the headline load is the busiest engine's,
//!   else NVML's. They are for the Hardware display only.
//! - **The fit signal** ([`FitTracker`]; ARCH-071, P4b-R37 as Appendix H states it): an engine
//!   is its processes, and its figure is the sum of their `GPU Process Memory\Shared Usage`,
//!   never the adapter's. Its idle level is the lowest figure since it became ready, fixed for
//!   a job when the job starts; it resets when the engine becomes ready again.
//! - **Alerts** (CORE-066, LT-04, APP-132): the temperature raises at >= 85 °C and clears after
//!   30 s at <= 82 °C; a throttle alert clears after 30 s without its bit; neither raises again
//!   within 60 s of clearing. `gpu_spill` raises after 10 s with the job's engine above its idle
//!   level by more than [`SPILL_MARGIN_MIB`], and clears after 30 s at or below half of it,
//!   when the job ends, or when the adapter counters can't be read. Times are the step's
//!   (`StepIn::now`): the monotonic clock, or the nominal seconds of manual mode.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::Mutex;
use std::time::Duration;

use lil_platform::hw::{
    CpuEnergySample, CpuEnergySource, GpuCounters, GpuMemSource, GpuSample, GpuSource, ProcGpuMem,
    SysSource, ThrottleReasons,
};
use lil_platform::process::{ProcessInspector, SystemInspector};
use serde_json::{Value, json};

use super::db::{
    EST_CPU_ENERGY, EST_CPU_POWER, EST_GPU_ENERGY, EST_GPU_POWER, Metric, ProcRow, Row,
};
use super::energy::{
    Accum, CounterStep, CounterTracker, cpu_estimate_mw, power_accum, pwh_accum, system_power_mw,
};
use super::{
    APP_ROLE, AlertView, Basis, CpuView, DiskView, EngineState, Fit, GPU_USERS, GpuEngineView,
    GpuMemoryView, GpuUser, GpuView, HeadlineUtil, HwSources, NetView, Part, PowerSettings,
    ProcessView, RamView, SPILL_MARGIN_MIB, Sizes, Snapshot, SystemPart, UtilSource, VramView,
    format_ts, lock,
};

/// Per-process GPU memory while no job runs (CORE-061).
const PROCS_IDLE: Duration = Duration::from_secs(5);
/// Per-process GPU memory while jobs run.
const PROCS_BUSY: Duration = Duration::from_secs(1);
/// Disk free space and folder sizes.
const DISKS_EVERY: Duration = Duration::from_secs(300);
/// A real step may come a little early; this much counts as on time.
const SLACK: Duration = Duration::from_millis(100);

/// Consecutive failed GPU reads before the source is opened again.
const REOPEN_AFTER: u32 = 3;
/// The most steps between two attempts to open a lost GPU again.
const MAX_REOPEN_GAP: u32 = 60;
/// A lost GPU's figures are estimated from its last power for at most this many steps.
const HOLD_MAX_STEPS: u32 = 60;
/// The GPU temperature that raises an alert (CORE-066).
const TEMP_ALERT_C: u8 = 85;
/// The temperature alert clears only after [`CLEAR_AFTER`] at or below this (LT-04).
const TEMP_CLEAR_C: u8 = 82;
/// A temperature or throttle alert clears after this long calm, and so does `gpu_spill`.
const CLEAR_AFTER: Duration = Duration::from_secs(30);
/// A cleared temperature or throttle alert doesn't raise again within this (LT-04).
const REARM_AFTER: Duration = Duration::from_secs(60);
/// `gpu_spill` raises after this long above the margin (APP-132).
const SPILL_RAISE_AFTER: Duration = Duration::from_secs(10);
/// The finished jobs' fits kept for `meter` to take.
const FITS_KEPT: usize = 256;

/// A source that opens the GPU again (`lil_platform::hw::nvml_gpu`).
pub type GpuOpener = Box<dyn FnMut() -> Option<Box<dyn GpuSource>> + Send>;
/// Measures the workspace and model folders (every 5 minutes).
pub type SizesProbe = Box<dyn FnMut() -> Sizes + Send>;

/// Which of the slower sources a step reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Due {
    pub procs: bool,
    pub disks: bool,
}

/// When the slower sources are due (module docs). Pure: `now` is the step's time on any
/// monotonic scale.
#[derive(Debug, Default)]
pub(crate) struct Cadence {
    procs: Option<Duration>,
    disks: Option<Duration>,
}

impl Cadence {
    pub(crate) fn due(&mut self, now: Duration, busy: bool) -> Due {
        let every = if busy { PROCS_BUSY } else { PROCS_IDLE };
        let is_due = |last: Option<Duration>, every: Duration| {
            last.is_none_or(|last| now < last || now + SLACK >= last + every)
        };
        let due = Due {
            procs: is_due(self.procs, every),
            disks: is_due(self.disks, DISKS_EVERY),
        };
        if due.procs {
            self.procs = Some(now);
        }
        if due.disks {
            self.disks = Some(now);
        }
        due
    }
}

/// What one step is told.
pub(crate) struct StepIn<'a> {
    /// The step's time for the cadence: the monotonic clock (real), or whole seconds (manual).
    pub now: Duration,
    /// The time the step's sample stands for, ms; 0 for the first.
    pub dt_ms: u64,
    pub mono_ms: i64,
    /// The sample time, Unix ms: unique and increasing ([`Sampler::next_ts`]).
    pub utc_ms: i64,
    /// The core run (its start, Unix ms), stored with the sample.
    pub run: i64,
    pub busy: bool,
    pub tracked: &'a BTreeMap<u32, &'static str>,
    /// Locked only after the sources are read.
    pub fits: &'a Mutex<FitTracker>,
}

/// What one step produced.
pub(crate) struct StepOut {
    pub snapshot: Snapshot,
    pub row: Row,
    /// `alert` and `alert_cleared` event data, in order.
    pub events: Vec<Value>,
    pub vram: Option<VramView>,
}

struct Active {
    kind: &'static str,
    value: i64,
    since_ms: i64,
    spill: Option<SpillNow>,
}

/// The timers of one alert id (LT-04): since when its raise condition held, since when its
/// clear condition held, and when it last cleared.
#[derive(Default)]
struct Hysteresis {
    on_since: Option<Duration>,
    calm_since: Option<Duration>,
    cleared_at: Option<Duration>,
}

/// How an alert raises and clears.
struct Rule {
    raise_after: Duration,
    clear_after: Duration,
    rearm_after: Duration,
}

/// One alert condition at a step.
struct Check {
    id: &'static str,
    kind: &'static str,
    /// The raise condition holds.
    raise: bool,
    /// The clear condition holds.
    calm: bool,
    value: i64,
    spill: Option<SpillNow>,
}

/// A running job's engine above its idle level, at a step.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpillNow {
    job_id: String,
    pid: Option<u32>,
    above_idle_mib: u64,
}

/// One engine's state and fit figure.
struct EngineTrack {
    state: EngineState,
    /// The lowest figure since the engine became ready.
    low: Option<u64>,
    /// The latest figure, and the engine process with the most `Shared Usage` in it.
    figure: Option<(u64, Option<u32>)>,
}

/// A job running on an engine.
struct RunningJob {
    engine: String,
    idle: Option<u64>,
    max_above: u64,
}

/// The engines' fit figures and the jobs running on them (module docs, "The fit signal").
#[derive(Default)]
pub(crate) struct FitTracker {
    engines: BTreeMap<String, EngineTrack>,
    running: BTreeMap<String, RunningJob>,
    /// Ended jobs' fits, oldest first, at most [`FITS_KEPT`].
    finished: VecDeque<(String, Option<Fit>)>,
}

impl FitTracker {
    pub(crate) fn set_engine(&mut self, s: EngineState) {
        match self.engines.get_mut(&s.engine) {
            Some(track) => {
                if track.state.ready_at_ms != s.ready_at_ms {
                    track.low = None;
                    track.figure = None;
                }
                track.state = s;
            }
            None => {
                self.engines.insert(
                    s.engine.clone(),
                    EngineTrack {
                        state: s,
                        low: None,
                        figure: None,
                    },
                );
            }
        }
    }

    pub(crate) fn job_started(&mut self, job_id: &str, engine: &str) {
        if self.running.contains_key(job_id) {
            return;
        }
        let earlier = self
            .finished
            .iter()
            .position(|(id, _)| id == job_id)
            .and_then(|at| self.finished.remove(at))
            .and_then(|(_, fit)| fit);
        let now = self
            .engines
            .get(engine)
            .filter(|t| t.state.ready_at_ms.is_some())
            .and_then(|t| t.low);
        self.running.insert(
            job_id.to_string(),
            RunningJob {
                engine: engine.to_string(),
                idle: earlier.map(|f| f.shared_idle_mib).or(now),
                max_above: earlier.map_or(0, |f| f.shared_above_idle_max_mib),
            },
        );
    }

    pub(crate) fn job_ended(&mut self, job_id: &str) {
        let Some(job) = self.running.remove(job_id) else {
            return;
        };
        let fit = job.idle.map(|idle| Fit {
            shared_idle_mib: idle,
            shared_above_idle_max_mib: job.max_above,
        });
        self.finished.push_back((job_id.to_string(), fit));
        while self.finished.len() > FITS_KEPT {
            self.finished.pop_front();
        }
    }

    pub(crate) fn take(&mut self, job_id: &str) -> Option<Fit> {
        let at = self.finished.iter().position(|(id, _)| id == job_id)?;
        self.finished.remove(at).and_then(|(_, fit)| fit)
    }

    /// A per-process GPU memory reading at `utc_ms`: each ready engine's figure, its lowest
    /// since ready, and its running jobs' rise above their idle levels.
    fn observe(&mut self, mem: &[ProcGpuMem], utc_ms: i64) {
        for (name, track) in &mut self.engines {
            if track.state.ready_at_ms.is_none_or(|ready| utc_ms < ready) {
                continue;
            }
            let ours = mem.iter().filter(|p| track.state.pids.contains(&p.pid));
            let figure: u64 = ours.clone().map(|p| p.shared_mib).sum();
            let top = ours.max_by_key(|p| (p.shared_mib, std::cmp::Reverse(p.pid)));
            track.figure = Some((figure, top.map(|p| p.pid)));
            track.low = Some(track.low.map_or(figure, |low| low.min(figure)));
            for job in self.running.values_mut().filter(|j| j.engine == *name) {
                if let Some(idle) = job.idle {
                    job.max_above = job.max_above.max(figure.saturating_sub(idle));
                }
            }
        }
    }

    /// The running job whose engine is the furthest above its idle level, if any job has an
    /// idle level and its engine a figure.
    fn spill(&self) -> Option<SpillNow> {
        self.running
            .iter()
            .filter_map(|(job_id, job)| {
                let idle = job.idle?;
                let (figure, pid) = self.engines.get(&job.engine)?.figure?;
                Some(SpillNow {
                    job_id: job_id.clone(),
                    pid,
                    above_idle_mib: figure.saturating_sub(idle),
                })
            })
            .max_by_key(|s| s.above_idle_mib)
    }

    fn is_running(&self, job_id: &str) -> bool {
        self.running.contains_key(job_id)
    }
}

#[derive(Default)]
struct GpuState {
    tracker: CounterTracker,
    est: Option<Accum>,
    last: Option<GpuSample>,
    last_power: Option<u32>,
    errors: u32,
    gap_steps: u32,
    after_gap: bool,
    /// A counter reading has been seen from this device: a sample without one is then a missed
    /// reading, not a device that has none.
    counted: bool,
}

struct GpuFigures {
    sample: Option<GpuSample>,
    /// The reading is from the device this step.
    live: bool,
    power: Option<(u32, Basis)>,
    energy: Option<(u64, Basis)>,
}

impl GpuState {
    fn figures(&mut self, read: Option<io::Result<GpuSample>>, dt_ms: u64) -> GpuFigures {
        let est = self.est.get_or_insert_with(power_accum);
        let integrate = |est: &mut Accum, mw: u32| est.add(u64::from(mw) * dt_ms);
        match read {
            None => GpuFigures {
                sample: None,
                live: false,
                power: None,
                energy: None,
            },
            Some(Ok(s)) => {
                self.errors = 0;
                self.gap_steps = 0;
                let after_gap = std::mem::take(&mut self.after_gap);
                let direct = s.power_mw.or(s.power_instant_mw);
                let mut derived = None;
                self.counted |= s.energy_mj.is_some();
                let energy = match s.energy_mj {
                    Some(cur) => match self.tracker.step(cur) {
                        CounterStep::Baseline if after_gap => {
                            direct.map(|p| (integrate(est, p), Basis::Estimated))
                        }
                        CounterStep::Baseline => Some((0, Basis::Measured)),
                        CounterStep::Delta(d) | CounterStep::Reset(d) => {
                            if direct.is_none() && dt_ms > 0 {
                                derived = u32::try_from(d.saturating_mul(1000) / dt_ms).ok();
                            }
                            Some((d, Basis::Measured))
                        }
                    },
                    // A counter that missed one reading: no energy for this row. The baseline
                    // stays, so the next delta carries this interval too, measured; integrating
                    // the power here as well would count the interval twice (CORE-063).
                    None if self.counted => None,
                    None => direct.map(|p| (integrate(est, p), Basis::Estimated)),
                };
                let power = match direct {
                    Some(p) => Some((p, Basis::Measured)),
                    None => derived.map(|p| (p, Basis::Estimated)),
                };
                if let Some((p, _)) = power {
                    self.last_power = Some(p);
                }
                self.last = Some(s.clone());
                GpuFigures {
                    sample: Some(s),
                    live: true,
                    power,
                    energy,
                }
            }
            Some(Err(e)) => {
                if self.errors == 0 {
                    tracing::warn!(error = %e, "the GPU can't be read; its figures are estimated from the last reading until it can");
                }
                self.errors += 1;
                self.gap_steps += 1;
                self.tracker.forget();
                self.after_gap = true;
                let held = self.last_power.filter(|_| self.gap_steps <= HOLD_MAX_STEPS);
                GpuFigures {
                    sample: self.last.clone(),
                    live: false,
                    power: held.map(|p| (p, Basis::Estimated)),
                    energy: held.map(|p| (integrate(est, p), Basis::Estimated)),
                }
            }
        }
    }
}

struct CpuState {
    tracker: CounterTracker,
    pwh: Accum,
    est: Accum,
    after_gap: bool,
    failing: bool,
}

struct CpuFigures {
    power: Option<(u32, Basis)>,
    energy: Option<(u64, Basis)>,
}

impl CpuState {
    fn new() -> CpuState {
        CpuState {
            tracker: CounterTracker::default(),
            pwh: pwh_accum(),
            est: power_accum(),
            after_gap: false,
            failing: false,
        }
    }

    fn figures(
        &mut self,
        read: Option<io::Result<CpuEnergySample>>,
        util_permille: u16,
        tdp_mw: Option<u32>,
        dt_ms: u64,
    ) -> CpuFigures {
        match read {
            Some(Ok(s)) => {
                self.failing = false;
                let after_gap = std::mem::take(&mut self.after_gap);
                let power = s.package_power_mw;
                // The first reading after the meter was lost has no earlier one to count from, so
                // its interval is integrated from the power instead.
                let energy = match self.tracker.step(s.package_energy_pwh) {
                    CounterStep::Baseline if after_gap => {
                        (self.est.add(u64::from(power) * dt_ms), Basis::Estimated)
                    }
                    CounterStep::Baseline => (0, Basis::Measured),
                    CounterStep::Delta(d) | CounterStep::Reset(d) => {
                        (self.pwh.add(d), Basis::Measured)
                    }
                };
                return CpuFigures {
                    power: Some((power, Basis::Measured)),
                    energy: Some(energy),
                };
            }
            Some(Err(e)) => {
                if !self.failing {
                    tracing::warn!(error = %e, "the CPU Energy Meter can't be read");
                }
                self.failing = true;
                self.tracker.forget();
                self.after_gap = true;
            }
            None => {}
        }
        match tdp_mw {
            Some(tdp) => {
                let mw = cpu_estimate_mw(util_permille, tdp);
                CpuFigures {
                    power: Some((mw, Basis::Estimated)),
                    energy: Some((self.est.add(u64::from(mw) * dt_ms), Basis::Estimated)),
                }
            }
            None => CpuFigures {
                power: None,
                energy: None,
            },
        }
    }
}

/// The sampling state machine (module docs).
pub(crate) struct Sampler {
    gpu: Option<Box<dyn GpuSource>>,
    gpu_mem: Option<Box<dyn GpuMemSource>>,
    cpu_energy: Option<Box<dyn CpuEnergySource>>,
    sys: Box<dyn SysSource>,
    gpu_counters: Option<Box<dyn GpuCounters>>,
    gpu_opener: Option<GpuOpener>,
    sizes_probe: Option<SizesProbe>,
    /// Steps to wait before the next attempt to open the GPU, and the wait after a failure.
    reopen_wait: u32,
    reopen_gap: u32,
    power: PowerSettings,
    cadence: Cadence,
    gpu_state: GpuState,
    cpu_state: CpuState,
    mem_cache: Vec<ProcGpuMem>,
    mem_failing: bool,
    disks: Vec<DiskView>,
    sizes: Sizes,
    net_prev: BTreeMap<String, (u64, u64)>,
    proc_prev: BTreeMap<u32, u64>,
    names: BTreeMap<u32, String>,
    look_up_names: bool,
    alerts: BTreeMap<&'static str, Active>,
    hysteresis: BTreeMap<&'static str, Hysteresis>,
    /// The whole-system energy of each sample, accumulated so it never drifts (`e15`).
    system_energy: Accum,
    last_ts: i64,
}

impl Sampler {
    /// `look_up_names`: ask the system for the image name of a process that isn't a tracked
    /// engine (real sampling only; fake hardware has no real pids).
    pub(crate) fn new(src: HwSources, power: PowerSettings, look_up_names: bool) -> Sampler {
        Sampler {
            gpu: src.gpu,
            gpu_mem: src.gpu_mem,
            cpu_energy: src.cpu_energy,
            sys: src.sys,
            gpu_counters: src.gpu_counters,
            gpu_opener: None,
            sizes_probe: None,
            reopen_wait: 0,
            reopen_gap: REOPEN_AFTER,
            cadence: Cadence::default(),
            gpu_state: GpuState::default(),
            cpu_state: CpuState::new(),
            mem_cache: Vec::new(),
            mem_failing: false,
            disks: Vec::new(),
            sizes: Sizes::default(),
            net_prev: BTreeMap::new(),
            proc_prev: BTreeMap::new(),
            names: BTreeMap::new(),
            look_up_names,
            alerts: BTreeMap::new(),
            hysteresis: BTreeMap::new(),
            system_energy: Accum::new(1, u128::from(power.psu_efficiency_permille.clamp(1, 1000))),
            power,
            last_ts: i64::MIN,
        }
    }

    pub(crate) fn set_gpu_opener(&mut self, opener: GpuOpener) {
        self.gpu_opener = Some(opener);
    }

    pub(crate) fn set_sizes_probe(&mut self, probe: SizesProbe) {
        self.sizes_probe = Some(probe);
    }

    /// Whether the machine has a GPU source at all.
    pub(crate) fn has_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// Starts the sample times after `ts`, the newest a previous run stored: a clock that was
    /// set back must not make this run write over rows that are there.
    pub(crate) fn seed_ts(&mut self, ts: i64) {
        self.last_ts = self.last_ts.max(ts);
    }

    /// The sample time for a step at `utc_ms`: strictly after the previous one.
    pub(crate) fn next_ts(&mut self, utc_ms: i64) -> i64 {
        let ts = utc_ms.max(self.last_ts.saturating_add(1));
        self.last_ts = ts;
        ts
    }

    /// Reads every source that is due and builds the step's outputs.
    pub(crate) fn step(&mut self, input: &StepIn<'_>) -> StepOut {
        let due = self.cadence.due(input.now, input.busy);
        let dt_ms = input.dt_ms;
        let tracked_pids: Vec<u32> = input.tracked.keys().copied().collect();

        // The GPU: reopened after repeated failures, then read.
        // Each failed attempt doubles the steps before the next one, up to a minute: opening NVML
        // is a DLL load and an init, and a GPU that is gone for good must not cost that each second.
        if self.gpu_state.errors >= REOPEN_AFTER
            && let Some(open) = self.gpu_opener.as_mut()
        {
            if self.reopen_wait > 0 {
                self.reopen_wait -= 1;
            } else {
                if let Some(source) = open() {
                    tracing::info!("the GPU source is open again");
                    self.gpu = Some(source);
                }
                // The wait stands after a successful open too, until a read succeeds: a source
                // that opens but cannot be read is as much a loss as one that cannot be opened.
                self.reopen_wait = self.reopen_gap;
                self.reopen_gap = (self.reopen_gap * 2).min(MAX_REOPEN_GAP);
            }
        }
        let gpu_read = self.gpu.as_mut().map(|g| g.sample());
        if matches!(gpu_read, Some(Ok(_))) {
            self.reopen_wait = 0;
            self.reopen_gap = REOPEN_AFTER;
        }
        let gpu = self.gpu_state.figures(gpu_read, dt_ms);

        // Per-process GPU memory: replaced whole, so processes that ended drop out.
        let mut mem_read = false;
        if due.procs
            && let Some(source) = self.gpu_mem.as_mut()
        {
            match source.sample() {
                Ok(list) => {
                    self.mem_failing = false;
                    self.mem_cache = list;
                    mem_read = true;
                }
                Err(e) => {
                    if !self.mem_failing {
                        tracing::warn!(error = %e, "per-process GPU memory can't be read");
                    }
                    self.mem_failing = true;
                    self.mem_cache.clear();
                }
            }
        }

        // The adapter's counters (CORE-059): unknown when they can't be read.
        let (adapter, engines) = match self.gpu_counters.as_mut() {
            Some(c) => (c.adapter_memory(), c.engines()),
            None => (None, None),
        };
        let busiest = engines
            .as_ref()
            .and_then(|e| e.iter().map(|e| e.util_permille).max());

        let sys = self.sys.sample(&tracked_pids, due.disks);

        // CPU power and energy.
        let meter_read = self.cpu_energy.as_mut().map(|m| m.sample());
        let cpu =
            self.cpu_state
                .figures(meter_read, sys.cpu_permille, self.power.cpu_tdp_mw, dt_ms);

        if due.disks {
            self.disks = sys
                .disks
                .iter()
                .map(|(volume, free_mib)| DiskView {
                    volume: volume.clone(),
                    free_mib: *free_mib,
                })
                .collect();
        }
        // The folder sizes: every 5 minutes, and at every step until the first answer.
        if (due.disks || self.sizes == Sizes::default())
            && let Some(probe) = self.sizes_probe.as_mut()
        {
            self.sizes = probe();
        }

        // Network rates from the adapters' running totals.
        let seconds = dt_ms as f64 / 1000.0;
        let mut net = Vec::new();
        let mut net_now = BTreeMap::new();
        for (adapter, rx, tx) in &sys.net {
            let rate = |now: u64, before: Option<u64>| match before {
                Some(before) if now >= before && seconds > 0.0 => {
                    ((now - before) as f64 / seconds).round() as u64
                }
                _ => 0,
            };
            let before = self.net_prev.get(adapter);
            net.push(NetView {
                adapter: adapter.clone(),
                rx_bps: rate(*rx, before.map(|b| b.0)),
                tx_bps: rate(*tx, before.map(|b| b.1)),
            });
            net_now.insert(adapter.clone(), (*rx, *tx));
        }
        self.net_prev = net_now;

        // Per-process CPU time since the previous step. A pid that is gone drops out; a counter
        // that went down is a new process under an old pid.
        let mut cpu_delta: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
        let mut proc_now = BTreeMap::new();
        for (pid, cpu_ms, ram_mib) in &sys.procs {
            let delta = match self.proc_prev.get(pid) {
                Some(&before) if *cpu_ms >= before => *cpu_ms - before,
                Some(_) => *cpu_ms,
                None => 0,
            };
            cpu_delta.insert(*pid, (delta, *ram_mib));
            proc_now.insert(*pid, *cpu_ms);
        }
        self.proc_prev = proc_now;

        // The processes worth showing: tracked engines, anything using the GPU, and each
        // connected app, whose connection shows it runs even when the system source doesn't
        // report it (P4a-R27).
        let sm: BTreeMap<u32, u16> = gpu
            .sample
            .iter()
            .filter(|_| gpu.live)
            .flat_map(|s| s.sm_by_pid.iter().copied())
            .filter(|(_, permille)| *permille > 0)
            .collect();
        let mem: BTreeMap<u32, ProcGpuMem> = self
            .mem_cache
            .iter()
            .filter(|p| p.local_mib > 0 || p.committed_mib > 0)
            .map(|p| (p.pid, *p))
            .collect();
        let mut pids: BTreeSet<u32> = BTreeSet::new();
        pids.extend(cpu_delta.keys());
        pids.extend(sm.keys());
        pids.extend(mem.keys());
        pids.extend(
            input
                .tracked
                .iter()
                .filter(|(_, role)| **role == APP_ROLE)
                .map(|(pid, _)| *pid),
        );
        let cores = u64::try_from(sys.per_core_permille.len().max(1)).unwrap_or(1);
        let mut processes = Vec::new();
        let mut proc_rows = Vec::new();
        for pid in pids {
            let role = input.tracked.get(&pid).copied();
            let cpu_part = cpu_delta.get(&pid).copied();
            let mem_part = mem.get(&pid);
            let sm_part = sm.get(&pid).copied();
            processes.push(ProcessView {
                pid,
                name: self.name_of(pid, role),
                role: role.map(str::to_string),
                sm_permille: sm_part,
                vram_local_mib: mem_part.map(|p| p.local_mib),
                vram_committed_mib: mem_part.map(|p| p.committed_mib),
                cpu_permille: cpu_part.map(|(delta, _)| {
                    if dt_ms == 0 {
                        0
                    } else {
                        u32::try_from(delta * 1000 / (dt_ms * cores)).unwrap_or(u32::MAX)
                    }
                }),
                ram_mib: cpu_part.map(|(_, ram)| ram),
            });
            if role.is_some() || sm_part.is_some() {
                proc_rows.push(ProcRow {
                    pid,
                    sm: u32::from(sm_part.unwrap_or(0)),
                    cpu_ms: cpu_part.map_or(0, |(delta, _)| delta),
                    ram_mib: cpu_part.map(|(_, ram)| ram),
                    vram_local: mem_part.map(|p| p.local_mib),
                    vram_committed: mem_part.map(|p| p.committed_mib),
                    vram_shared: mem_part.map(|p| p.shared_mib),
                });
            }
        }
        let live_pids: BTreeSet<u32> = processes.iter().map(|p| p.pid).collect();
        self.names.retain(|pid, _| live_pids.contains(pid));
        let gpu_users = gpu_users(&processes);

        // The fit figures, then the alerts: the temperature and throttle from a real reading
        // only (a gap leaves them as they were), the spill while the adapter counters read.
        let (spill, spill_job_running) = {
            let mut fits = lock(input.fits);
            if mem_read {
                fits.observe(&self.mem_cache, input.utc_ms);
            }
            let raised = self.alerts.get("gpu_spill").and_then(|a| a.spill.as_ref());
            let running = raised.is_some_and(|s| fits.is_running(&s.job_id));
            (fits.spill(), running)
        };
        let live_sample = gpu.sample.as_ref().filter(|_| gpu.live);
        let mut events = self.update_alerts(live_sample, input.now, input.utc_ms);
        events.extend(self.update_spill(
            spill.filter(|_| adapter.is_some()),
            spill_job_running && adapter.is_some(),
            input.now,
            input.utc_ms,
        ));

        // The whole-system figure.
        let gpu_mw = gpu.power.map(|(mw, _)| mw);
        let cpu_mw = cpu.power.map(|(mw, _)| mw);
        let system = system_power_mw(
            gpu_mw,
            cpu_mw,
            self.power.rest_of_system_mw,
            self.power.psu_efficiency_permille,
        );
        let part = |part: Part, mw: Option<u32>, basis: Option<Basis>| SystemPart {
            part,
            mw: mw.unwrap_or(0),
            measured: basis.unwrap_or(Basis::Unknown),
        };
        let system_parts = vec![
            part(Part::Gpu, gpu_mw, gpu.power.map(|(_, b)| b)),
            part(Part::Cpu, cpu_mw, cpu.power.map(|(_, b)| b)),
            // The rest-of-system baseline is something you set: unset (0) shows as such.
            SystemPart {
                part: Part::Rest,
                mw: self.power.rest_of_system_mw,
                measured: if self.power.rest_of_system_mw == 0 {
                    Basis::Unknown
                } else {
                    Basis::Estimated
                },
            },
        ];

        let ts = input.utc_ms;
        let snapshot = Snapshot {
            ts: format_ts(ts),
            gpu: gpu.sample.as_ref().map(|s| GpuView {
                name: s.name.clone(),
                driver: s.driver.clone(),
                util_pct: s.util_pct,
                power_mw: gpu_mw.unwrap_or(0),
                energy_mj: s.energy_mj.filter(|_| gpu.live),
                temp_c: s.temp_c,
                sm_clock_mhz: s.sm_clock_mhz,
                mem_clock_mhz: s.mem_clock_mhz,
                fan_pct: s.fan_pct,
                throttle: throttle_names(s.throttle),
                vram_total_mib: s.vram_total_mib,
                vram_used_mib: s.vram_used_mib,
                measured: gpu.live,
                memory: adapter.map(|a| GpuMemoryView {
                    dedicated_used_mib: a.dedicated_mib,
                    shared_used_mib: a.shared_mib,
                }),
                engines: engines.as_ref().map(|list| {
                    list.iter()
                        .map(|e| GpuEngineView {
                            engtype: e.engtype.clone(),
                            util_permille: e.util_permille,
                        })
                        .collect()
                }),
                headline_util: match busiest {
                    Some(permille) => HeadlineUtil {
                        permille,
                        source: UtilSource::Engines,
                    },
                    None => HeadlineUtil {
                        permille: u16::from(s.util_pct) * 10,
                        source: UtilSource::Nvml,
                    },
                },
            }),
            processes,
            cpu: CpuView {
                util_permille: sys.cpu_permille,
                per_core_permille: sys.per_core_permille.clone(),
                package_power_mw: cpu_mw.unwrap_or(0),
                measured: matches!(cpu.power, Some((_, Basis::Measured))),
            },
            ram: RamView {
                total_mib: sys.ram_total_mib,
                used_mib: sys.ram_used_mib,
            },
            disks: self.disks.clone(),
            sizes: self.sizes.clone(),
            net: net.clone(),
            system_power_mw: system.unwrap_or(0),
            system_parts,
            alerts: self.alerts_view(),
            gpu_users,
        };

        let mut row = Row {
            ts,
            run: input.run,
            mono: input.mono_ms,
            dt: i64::try_from(dt_ms).unwrap_or(i64::MAX),
            est: 0,
            values: [None; Metric::COUNT],
            cpu_ms_all: sys.cpu_permille as u64 * cores * dt_ms / 1000,
            system_mj: None,
            procs: proc_rows,
        };
        let int = |v: u64| i64::try_from(v).ok();
        if let Some(s) = gpu.sample.as_ref().filter(|_| gpu.live) {
            row.set(Metric::GpuUtil, Some(i64::from(s.util_pct)));
            row.set(Metric::GpuTemp, Some(i64::from(s.temp_c)));
            row.set(Metric::GpuVramUsed, int(s.vram_used_mib));
        }
        if let Some((mw, basis)) = gpu.power {
            row.set(Metric::GpuPower, Some(i64::from(mw)));
            if basis == Basis::Estimated {
                row.est |= EST_GPU_POWER;
            }
        }
        if let Some((mj, basis)) = gpu.energy {
            row.set(Metric::GpuEnergy, int(mj));
            if basis == Basis::Estimated {
                row.est |= EST_GPU_ENERGY;
            }
        }
        row.set(Metric::CpuUtil, Some(i64::from(sys.cpu_permille)));
        if let Some((mw, basis)) = cpu.power {
            row.set(Metric::CpuPower, Some(i64::from(mw)));
            if basis == Basis::Estimated {
                row.est |= EST_CPU_POWER;
            }
        }
        if let Some((mj, basis)) = cpu.energy {
            row.set(Metric::CpuEnergy, int(mj));
            if basis == Basis::Estimated {
                row.est |= EST_CPU_ENERGY;
            }
        }
        row.set(Metric::RamUsed, int(sys.ram_used_mib));
        row.set(Metric::SystemPower, system.and_then(int));
        row.set(Metric::NetRx, int(net.iter().map(|n| n.rx_bps).sum()));
        row.set(Metric::NetTx, int(net.iter().map(|n| n.tx_bps).sum()));
        if let Some(a) = adapter {
            row.set(Metric::GpuMemDedicated, int(a.dedicated_mib));
            row.set(Metric::GpuMemShared, int(a.shared_mib));
        }
        row.set(Metric::GpuEngineBusiest, busiest.map(i64::from));
        // The whole-system energy of the sample, by `telemetry.energy`'s formula: (GPU + CPU +
        // the rest-of-system baseline over the sample's time) / PSU efficiency, accumulated in
        // microjoules so the quarter-hour sums never drift.
        let (gpu_mj, cpu_mj) = (gpu.energy.map(|(mj, _)| mj), cpu.energy.map(|(mj, _)| mj));
        if gpu_mj.is_some() || cpu_mj.is_some() || self.power.rest_of_system_mw > 0 {
            let micro = (gpu_mj.unwrap_or(0) + cpu_mj.unwrap_or(0)).saturating_mul(1000)
                + u64::from(self.power.rest_of_system_mw).saturating_mul(dt_ms);
            row.system_mj = Some(self.system_energy.add(micro));
        }

        let vram = gpu.sample.as_ref().filter(|_| gpu.live).map(|s| VramView {
            total_mib: s.vram_total_mib,
            nvml_free_mib: s.vram_total_mib.saturating_sub(s.vram_used_mib),
            local_by_pid: (self.gpu_mem.is_some() && !self.mem_failing).then(|| {
                self.mem_cache
                    .iter()
                    .map(|p| (p.pid, p.local_mib))
                    .collect()
            }),
        });

        StepOut {
            snapshot,
            row,
            events,
            vram,
        }
    }

    /// A process's name: the engine's role, else the image name the system gives, else
    /// `pid <n>`.
    fn name_of(&mut self, pid: u32, role: Option<&'static str>) -> String {
        if let Some(role) = role {
            return role.to_string();
        }
        if let Some(name) = self.names.get(&pid) {
            return name.clone();
        }
        let name = self
            .look_up_names
            .then(|| SystemInspector.inspect(pid).ok())
            .flatten()
            .and_then(|info| {
                info.image
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| format!("pid {pid}"));
        self.names.insert(pid, name.clone());
        name
    }

    /// Raises and clears the temperature and throttle alerts for a real GPU reading (LT-04);
    /// returns the events.
    fn update_alerts(&mut self, gpu: Option<&GpuSample>, now: Duration, now_ms: i64) -> Vec<Value> {
        let Some(gpu) = gpu else {
            return Vec::new();
        };
        let lt04 = Rule {
            raise_after: Duration::ZERO,
            clear_after: CLEAR_AFTER,
            rearm_after: REARM_AFTER,
        };
        let checks = [
            Check {
                id: "gpu_temperature",
                kind: "temperature",
                raise: gpu.temp_c >= TEMP_ALERT_C,
                calm: gpu.temp_c <= TEMP_CLEAR_C,
                value: i64::from(gpu.temp_c),
                spill: None,
            },
            Check {
                id: "gpu_throttle_thermal",
                kind: "throttle_thermal",
                raise: gpu.throttle.thermal(),
                calm: !gpu.throttle.thermal(),
                value: (gpu.throttle.0 & 0x60) as i64,
                spill: None,
            },
            Check {
                id: "gpu_throttle_power_brake",
                kind: "throttle_power_brake",
                raise: gpu.throttle.power_brake(),
                calm: !gpu.throttle.power_brake(),
                value: (gpu.throttle.0 & 0x80) as i64,
                spill: None,
            },
        ];
        let mut events = Vec::new();
        for check in checks {
            self.apply(check, &lt04, now, now_ms, &mut events);
        }
        events
    }

    /// Raises and clears `gpu_spill` (APP-132): `spill` is the running job's engine above its
    /// idle level (none while the adapter counters can't be read); `job_running` says the
    /// raised alert's job still runs with readable counters, else the alert clears now.
    fn update_spill(
        &mut self,
        spill: Option<SpillNow>,
        job_running: bool,
        now: Duration,
        now_ms: i64,
    ) -> Vec<Value> {
        let mut events = Vec::new();
        if self.alerts.contains_key("gpu_spill") && !job_running {
            self.alerts.remove("gpu_spill");
            self.hysteresis.remove("gpu_spill");
            events.push(json!({"kind": "alert_cleared", "id": "gpu_spill"}));
        }
        let rule = Rule {
            raise_after: SPILL_RAISE_AFTER,
            clear_after: CLEAR_AFTER,
            rearm_after: Duration::ZERO,
        };
        match spill {
            Some(s) => {
                let check = Check {
                    id: "gpu_spill",
                    kind: "spill",
                    raise: s.above_idle_mib > SPILL_MARGIN_MIB,
                    calm: s.above_idle_mib <= SPILL_MARGIN_MIB / 2,
                    value: i64::try_from(s.above_idle_mib).unwrap_or(i64::MAX),
                    spill: Some(s),
                };
                self.apply(check, &rule, now, now_ms, &mut events);
            }
            None => {
                self.hysteresis.remove("gpu_spill");
            }
        }
        events
    }

    /// One alert's step under `rule` (module docs, "Alerts").
    fn apply(
        &mut self,
        c: Check,
        rule: &Rule,
        now: Duration,
        now_ms: i64,
        events: &mut Vec<Value>,
    ) {
        let h = self.hysteresis.entry(c.id).or_default();
        if let Some(active) = self.alerts.get_mut(c.id) {
            active.value = c.value;
            if c.spill.is_some() {
                active.spill = c.spill;
            }
            if !c.calm {
                h.calm_since = None;
                return;
            }
            let calm_since = *h.calm_since.get_or_insert(now);
            if now.saturating_sub(calm_since) >= rule.clear_after {
                self.alerts.remove(c.id);
                *h = Hysteresis {
                    cleared_at: Some(now),
                    ..Hysteresis::default()
                };
                events.push(json!({"kind": "alert_cleared", "id": c.id}));
            }
            return;
        }
        if !c.raise {
            h.on_since = None;
            return;
        }
        let on_since = *h.on_since.get_or_insert(now);
        let rearmed = h
            .cleared_at
            .is_none_or(|at| now.saturating_sub(at) >= rule.rearm_after);
        if now.saturating_sub(on_since) >= rule.raise_after && rearmed {
            h.calm_since = None;
            let active = Active {
                kind: c.kind,
                value: c.value,
                since_ms: now_ms,
                spill: c.spill,
            };
            events.push(json!({"kind": "alert", "alert": alert_json(c.id, &active)}));
            self.alerts.insert(c.id, active);
        }
    }

    fn alerts_view(&self) -> Vec<AlertView> {
        self.alerts
            .iter()
            .map(|(id, a)| AlertView {
                id: (*id).to_string(),
                kind: a.kind.to_string(),
                value: a.value,
                since: format_ts(a.since_ms),
                job_id: a.spill.as_ref().map(|s| s.job_id.clone()),
                pid: a.spill.as_ref().and_then(|s| s.pid),
                above_idle_mib: a.spill.as_ref().map(|s| s.above_idle_mib),
                margin_mib: a.spill.as_ref().map(|_| SPILL_MARGIN_MIB),
            })
            .collect()
    }
}

/// The [`GPU_USERS`] processes with the most dedicated GPU memory, the most first (by pid
/// among equals), tracked or not (Appendix G `gpu_users`).
fn gpu_users(processes: &[ProcessView]) -> Vec<GpuUser> {
    let mut users: Vec<GpuUser> = processes
        .iter()
        .filter_map(|p| {
            let vram = p.vram_local_mib.filter(|mib| *mib > 0)?;
            Some(GpuUser {
                pid: p.pid,
                name: p.name.clone(),
                vram_local_mib: vram,
                sm_permille: p.sm_permille,
                role: p.role.clone(),
            })
        })
        .collect();
    users.sort_by(|a, b| {
        b.vram_local_mib
            .cmp(&a.vram_local_mib)
            .then(a.pid.cmp(&b.pid))
    });
    users.truncate(GPU_USERS);
    users
}

fn alert_json(id: &str, a: &Active) -> Value {
    let mut alert =
        json!({"id": id, "kind": a.kind, "value": a.value, "since": format_ts(a.since_ms)});
    if let Some(s) = &a.spill {
        alert["job_id"] = json!(s.job_id);
        if let Some(pid) = s.pid {
            alert["pid"] = json!(pid);
        }
        alert["above_idle_mib"] = json!(s.above_idle_mib);
        alert["margin_mib"] = json!(SPILL_MARGIN_MIB);
    }
    alert
}

/// NVML's clock event reasons by name.
fn throttle_names(reasons: ThrottleReasons) -> Vec<String> {
    const NAMES: [(u64, &str); 9] = [
        (0x1, "gpu_idle"),
        (0x2, "applications_clocks_setting"),
        (0x4, "sw_power_cap"),
        (0x8, "hw_slowdown"),
        (0x10, "sync_boost"),
        (0x20, "sw_thermal_slowdown"),
        (0x40, "hw_thermal_slowdown"),
        (0x80, "hw_power_brake_slowdown"),
        (0x100, "display_clock_setting"),
    ];
    NAMES
        .iter()
        .filter(|(bit, _)| reasons.0 & bit != 0)
        .map(|(_, name)| (*name).to_string())
        .collect()
}
