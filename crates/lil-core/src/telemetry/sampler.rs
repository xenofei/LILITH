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

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use lil_platform::hw::{
    CpuEnergySample, CpuEnergySource, GpuMemSource, GpuSample, GpuSource, ProcGpuMem, SysSource,
    ThrottleReasons,
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
    AlertView, Basis, CpuView, DiskView, GpuView, HwSources, NetView, Part, PowerSettings,
    ProcessView, RamView, Sizes, Snapshot, SystemPart, VramView, format_ts,
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
            gpu_opener: None,
            sizes_probe: None,
            reopen_wait: 0,
            reopen_gap: REOPEN_AFTER,
            power,
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
        if due.procs
            && let Some(source) = self.gpu_mem.as_mut()
        {
            match source.sample() {
                Ok(list) => {
                    self.mem_failing = false;
                    self.mem_cache = list;
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

        // The processes worth showing: tracked engines, anything using the GPU.
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
                });
            }
        }
        let live_pids: BTreeSet<u32> = processes.iter().map(|p| p.pid).collect();
        self.names.retain(|pid, _| live_pids.contains(pid));

        // Alerts, from a real reading only (a gap leaves them as they were).
        let live_sample = gpu.sample.as_ref().filter(|_| gpu.live);
        let events = self.update_alerts(live_sample, input.utc_ms);

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
        };

        let mut row = Row {
            ts,
            run: input.run,
            mono: input.mono_ms,
            dt: i64::try_from(dt_ms).unwrap_or(i64::MAX),
            est: 0,
            values: [None; 12],
            cpu_ms_all: sys.cpu_permille as u64 * cores * dt_ms / 1000,
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

        let vram = gpu.sample.as_ref().filter(|_| gpu.live).map(|s| VramView {
            total_mib: s.vram_total_mib,
            nvml_free_mib: s.vram_total_mib.saturating_sub(s.vram_used_mib),
            local_by_pid: self
                .mem_cache
                .iter()
                .map(|p| (p.pid, p.local_mib))
                .collect(),
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

    /// Raises and clears the alerts for a real GPU reading; returns the events.
    fn update_alerts(&mut self, gpu: Option<&GpuSample>, now_ms: i64) -> Vec<Value> {
        let Some(gpu) = gpu else {
            return Vec::new();
        };
        let conditions = [
            (
                "gpu_temperature",
                "temperature",
                gpu.temp_c >= TEMP_ALERT_C,
                i64::from(gpu.temp_c),
            ),
            (
                "gpu_throttle_thermal",
                "throttle_thermal",
                gpu.throttle.thermal(),
                (gpu.throttle.0 & 0x60) as i64,
            ),
            (
                "gpu_throttle_power_brake",
                "throttle_power_brake",
                gpu.throttle.power_brake(),
                (gpu.throttle.0 & 0x80) as i64,
            ),
        ];
        let mut events = Vec::new();
        for (id, kind, on, value) in conditions {
            match (on, self.alerts.get_mut(id)) {
                (true, None) => {
                    let active = Active {
                        kind,
                        value,
                        since_ms: now_ms,
                    };
                    events.push(json!({"kind": "alert", "alert": alert_json(id, &active)}));
                    self.alerts.insert(id, active);
                }
                (true, Some(active)) => active.value = value,
                (false, Some(_)) => {
                    self.alerts.remove(id);
                    events.push(json!({"kind": "alert_cleared", "id": id}));
                }
                (false, None) => {}
            }
        }
        events
    }

    fn alerts_view(&self) -> Vec<AlertView> {
        self.alerts
            .iter()
            .map(|(id, a)| AlertView {
                id: (*id).to_string(),
                kind: a.kind.to_string(),
                value: a.value,
                since: format_ts(a.since_ms),
            })
            .collect()
    }
}

fn alert_json(id: &str, a: &Active) -> Value {
    json!({"id": id, "kind": a.kind, "value": a.value, "since": format_ts(a.since_ms)})
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
