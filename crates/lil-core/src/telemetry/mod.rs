//! The `telemetry` module (CORE-004, CORE-005, CORE-059–068; Appendix F "Telemetry"):
//! hardware sampling, `telemetry.db`, energy, the whole-system estimate, alerts and the
//! hardware profile.
//!
//! - **Sampling** ([`sampler`]). Every second the GPU (NVML), the CPU package's Energy Meter and
//!   `sysinfo` are read; per-process GPU memory (PDH) every 5 s, every second while jobs run
//!   ([`Telemetry::set_busy`]); disk free space every 5 minutes (CORE-061). A source that is
//!   missing is `None`, never an error; a source that fails is estimated around, and its gap
//!   labelled (see `sampler`). CPU temperature is never sampled (CORE-060).
//! - **Storage** ([`db`]). Each sample is a row of `telemetry.db`, rolled into minute and hour
//!   rows as it is written; energy is the hardware counters' deltas, so totals stay exact
//!   across downsampling and counter resets (CORE-062, CORE-063).
//! - **Modes.** [`TickMode::Real`] samples once a second on a task. [`TickMode::Manual`] (test
//!   channels) samples only when [`Telemetry::tick`] is called (`test.hw_tick`), each sample
//!   standing for one second of nominal time; its slower sources follow that nominal time too.
//! - **Topic.** `telemetry` carries `{kind: "sample", snapshot}` once per sample,
//!   `{kind: "alert", alert: {id, kind, value, since}}` when an alert is raised and
//!   `{kind: "alert_cleared", id}` when it clears; subscribing gets the current [`Snapshot`].
//! - **Consumers.** The GPU scheduler reads [`Telemetry::vram`], sets
//!   [`Telemetry::set_busy`], and re-checks on every [`Telemetry::on_sample`] callback (the
//!   core kicks the jobs there); the engines [`Telemetry::track`] their
//!   processes; `meter` takes a job's [`EnergySlice`] with [`Telemetry::energy_between`] and
//!   writes its [`JobEnergy`] with [`Telemetry::put_job_energy`], the only writer; the
//!   `telemetry.jobs` tool reads them back with [`Telemetry::job_energy`]. `meter` also writes
//!   each succeeded job's history row ([`Telemetry::put_job_stats`]) when it has an estimate
//!   key, which the estimator reads ([`Telemetry::history`], CORE-161); `telemetry.energy`
//!   sums a time range ([`Telemetry::energy`], APP-131). The server tracks each verified app's
//!   process with role `app` while it is connected (P4a-R27): it is listed in `processes` for
//!   that long, its connection being the proof that it runs.
//!   The hardware profile of a workspace is checked once, at start ([`profile`]).
//! - **Times** are RFC 3339 UTC with milliseconds (`2026-10-18T12:00:00.123Z`) wherever a
//!   caller sees one.

pub mod profile;

mod db;
mod energy;
mod sampler;

pub(crate) use energy::share;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lil_platform::Clock;
use lil_platform::hw::{CpuEnergySource, GpuMemSource, GpuSource, SysSample, SysSource};
use lil_protocol::estimate::Cost;
use lil_protocol::{ErrorBody, codes};
use lil_schema::{Currency, Money, PowerToml};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use self::db::{Db, Metric, RawPoint, Resolution};
pub use self::sampler::{GpuOpener, SizesProbe};
use self::sampler::{Sampler, StepIn};
use crate::events::{EventBus, SnapshotProvider};

/// The module the telemetry tools declare.
pub const TELEMETRY_MODULE: &str = "telemetry";
/// The topic.
pub const TOPIC: &str = "telemetry";
/// The role a connected app's process is tracked with (P4a-R27).
pub const APP_ROLE: &str = "app";
/// The most points `telemetry.series` returns per metric (Appendix F).
pub const MAX_POINTS: usize = 3600;
/// A step that took longer than this since the previous one counts as this much (a machine
/// that slept must not integrate a power over the sleep).
const MAX_DT_MS: u64 = 5000;
/// How far ahead of the clock a stored sample time may be and still be followed (a clock set
/// back between runs).
const MAX_SEED_AHEAD_MS: i64 = 24 * 3_600_000;
/// Retention is applied this often.
const PRUNE_EVERY_MS: i64 = 60_000;

// ---------------------------------------------------------------------------------------------
// The sources and settings.
// ---------------------------------------------------------------------------------------------

/// The hardware a core samples. A missing source is `None`; `sys` is always there.
pub struct HwSources {
    pub gpu: Option<Box<dyn GpuSource>>,
    pub gpu_mem: Option<Box<dyn GpuMemSource>>,
    pub cpu_energy: Option<Box<dyn CpuEnergySource>>,
    pub sys: Box<dyn SysSource>,
}

impl std::fmt::Debug for HwSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwSources")
            .field("gpu", &self.gpu.is_some())
            .field("gpu_mem", &self.gpu_mem.is_some())
            .field("cpu_energy", &self.cpu_energy.is_some())
            .finish_non_exhaustive()
    }
}

/// A `SysSource` that reads nothing.
struct NoSys;

impl SysSource for NoSys {
    fn sample(&mut self, _tracked: &[u32], _disks: bool) -> SysSample {
        SysSample::default()
    }
}

impl HwSources {
    /// This machine's hardware. Blocking: it loads `nvml.dll`, asks DXGI which adapter the GPU
    /// is, and opens the PDH counters. Per-process GPU memory is read only from the NVML GPU's
    /// own adapter, never from another one.
    pub fn real() -> HwSources {
        let gpu = lil_platform::hw::nvml_gpu();
        let gpu_mem = gpu
            .as_ref()
            .and_then(|_| lil_platform::hw::nvidia_adapter_luid())
            .and_then(|luid| lil_platform::hw::pdh_gpu_memory(Some(luid)));
        HwSources {
            gpu,
            gpu_mem,
            cpu_energy: lil_platform::hw::energy_meter(),
            sys: lil_platform::hw::sysinfo_source(),
        }
    }

    /// No GPU, no Energy Meter, nothing read by `sys`: a test channel with no fake hardware
    /// script samples this (tests never read real hardware counters).
    pub fn none() -> HwSources {
        HwSources {
            gpu: None,
            gpu_mem: None,
            cpu_energy: None,
            sys: Box::new(NoSys),
        }
    }

    /// The sources of a fake hardware script (`lil_platform::hw_fake`).
    pub fn from_fake(sources: lil_platform::hw_fake::FakeHwSources) -> HwSources {
        let (gpu, gpu_mem, cpu_energy, sys) = sources;
        HwSources {
            gpu,
            gpu_mem,
            cpu_energy,
            sys,
        }
    }
}

/// How the sampler is driven (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMode {
    Real,
    /// Only `test.hw_tick` advances it.
    Manual,
}

/// `suite.toml` `[power]` (CORE-064).
#[derive(Debug, Clone, PartialEq)]
pub struct PowerSettings {
    /// The PSU's efficiency in permille (900 = 0.9); 1–1000.
    pub psu_efficiency_permille: u32,
    /// The rest of the system's baseline, mW; 0 is "not set".
    pub rest_of_system_mw: u32,
    /// `[power] cpu_tdp_w` in mW. `None`: the CPU is unknown without the Energy Meter.
    pub cpu_tdp_mw: Option<u32>,
    /// What a kWh costs (an optional rate turns energy into cost).
    pub electricity: Option<(Money, Currency)>,
}

impl Default for PowerSettings {
    fn default() -> Self {
        PowerSettings {
            psu_efficiency_permille: 900,
            rest_of_system_mw: 0,
            cpu_tdp_mw: None,
            electricity: None,
        }
    }
}

/// A decimal text with at most 6 decimals, in thousandths (`"0.9"` is 900, `"65"` is 65000).
fn thousandths(text: &str) -> Option<u64> {
    let micros = Money::parse_decimal(text.trim()).ok()?.micros();
    u64::try_from(micros / 1000).ok()
}

impl PowerSettings {
    /// The settings `suite.toml` gives; a value that doesn't read keeps its default (and is
    /// logged), so a typo never stops the core.
    pub fn from_toml(t: &PowerToml) -> PowerSettings {
        let mut out = PowerSettings::default();
        let bad = |key: &str, text: &str| {
            tracing::warn!(
                key,
                value = text,
                "[power] value is not usable; the default stays"
            );
        };
        if let Some(text) = &t.psu_efficiency {
            match thousandths(text).filter(|p| (1..=1000).contains(p)) {
                Some(p) => out.psu_efficiency_permille = u32::try_from(p).unwrap_or(900),
                None => bad("psu_efficiency", text),
            }
        }
        if let Some(text) = &t.rest_of_system_w {
            match thousandths(text).and_then(|mw| u32::try_from(mw).ok()) {
                Some(mw) => out.rest_of_system_mw = mw,
                None => bad("rest_of_system_w", text),
            }
        }
        if let Some(text) = &t.cpu_tdp_w {
            match thousandths(text)
                .filter(|mw| *mw > 0)
                .and_then(|mw| u32::try_from(mw).ok())
            {
                Some(mw) => out.cpu_tdp_mw = Some(mw),
                None => bad("cpu_tdp_w", text),
            }
        }
        if let (Some(rate), Some(currency)) = (&t.electricity_rate, &t.electricity_currency) {
            match (Money::parse_decimal(rate), Currency::parse(currency)) {
                (Ok(rate), Ok(currency)) => out.electricity = Some((rate, currency)),
                _ => bad("electricity", rate),
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Times.
// ---------------------------------------------------------------------------------------------

/// `ms` since the Unix epoch as RFC 3339 UTC with milliseconds.
pub fn format_ts(ms: i64) -> String {
    let base = lil_schema::time::rfc3339(ms.div_euclid(1000));
    format!("{}.{:03}Z", &base[..19], ms.rem_euclid(1000))
}

/// Unix milliseconds of an RFC 3339 UTC time `YYYY-MM-DDTHH:MM:SS[.fraction]Z`.
pub fn parse_ts(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b.last() != Some(&b'Z')
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let millis = match &b[19..b.len() - 1] {
        [] => 0,
        [b'.', digits @ ..] if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) => {
            let first3: String = digits.iter().take(3).map(|c| char::from(*c)).collect();
            format!("{first3:0<3}").parse::<i64>().ok()?
        }
        _ => return None,
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if d < 1 || d > days_in || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60 * 1000 + sec * 1000 + millis)
}

fn unix_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

fn mono_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------------------------
// The shapes (Appendix F).
// ---------------------------------------------------------------------------------------------

/// How a figure was come by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Measured,
    Estimated,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Gpu,
    Cpu,
    Rest,
}

/// The GPU, as last read. `measured` says the reading is the device's own; a GPU that was lost
/// shows its last reading with `measured: false`. `power_mw` is 0 when the power is unknown
/// (see `system_parts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GpuView {
    pub name: String,
    pub driver: String,
    pub util_pct: u8,
    pub power_mw: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_mj: Option<u64>,
    pub temp_c: u8,
    pub sm_clock_mhz: u32,
    pub mem_clock_mhz: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fan_pct: Option<u8>,
    pub throttle: Vec<String>,
    pub vram_total_mib: u64,
    pub vram_used_mib: u64,
    pub measured: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessView {
    pub pid: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sm_permille: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_local_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_committed_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_permille: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_mib: Option<u64>,
}

/// `package_power_mw` is the Energy Meter's reading, or the TDP estimate, or 0 when unknown;
/// `measured` is true only for the meter's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CpuView {
    pub util_permille: u16,
    pub per_core_permille: Vec<u16>,
    pub package_power_mw: u32,
    pub measured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct RamView {
    pub total_mib: u64,
    pub used_mib: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiskView {
    pub volume: String,
    pub free_mib: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Sizes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_mib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetView {
    pub adapter: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
}

/// One part of the whole-system figure; `measured` is its label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SystemPart {
    pub part: Part,
    pub mw: u32,
    pub measured: Basis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AlertView {
    pub id: String,
    pub kind: String,
    pub value: i64,
    pub since: String,
}

/// One of the processes with the most dedicated GPU memory (Appendix G `gpu_users`); `name` is
/// untrusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GpuUser {
    pub pid: u32,
    pub name: String,
    pub vram_local_mib: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sm_permille: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The most processes `gpu_users` lists.
pub const GPU_USERS: usize = 8;

/// The current hardware state: the `telemetry.snapshot` result and the `telemetry` topic's
/// snapshot (Appendix F; `gpu_users` from plan 4a, Appendix G). There is no CPU temperature
/// (CORE-060).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Snapshot {
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuView>,
    pub processes: Vec<ProcessView>,
    pub cpu: CpuView,
    pub ram: RamView,
    pub disks: Vec<DiskView>,
    pub sizes: Sizes,
    pub net: Vec<NetView>,
    pub system_power_mw: u64,
    pub system_parts: Vec<SystemPart>,
    pub alerts: Vec<AlertView>,
    /// The [`GPU_USERS`] processes with the most dedicated GPU memory, tracked or not, the
    /// most first ("who's using the GPU", APP-127).
    #[serde(default)]
    pub gpu_users: Vec<GpuUser>,
}

impl Snapshot {
    /// What there is before the first sample: nothing read, every part unknown.
    pub(crate) fn empty(ts_ms: i64) -> Snapshot {
        let part = |part, measured| SystemPart {
            part,
            mw: 0,
            measured,
        };
        Snapshot {
            ts: format_ts(ts_ms),
            gpu: None,
            processes: Vec::new(),
            cpu: CpuView {
                util_permille: 0,
                per_core_permille: Vec::new(),
                package_power_mw: 0,
                measured: false,
            },
            ram: RamView::default(),
            disks: Vec::new(),
            sizes: Sizes::default(),
            net: Vec::new(),
            system_power_mw: 0,
            system_parts: vec![
                part(Part::Gpu, Basis::Unknown),
                part(Part::Cpu, Basis::Unknown),
                part(Part::Rest, Basis::Unknown),
            ],
            alerts: Vec::new(),
            gpu_users: Vec::new(),
        }
    }
}

/// What the GPU scheduler reads (ARCH-071), from a live GPU reading ([`Telemetry::vram`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VramView {
    pub total_mib: u64,
    /// What NVML says is free.
    pub nvml_free_mib: u64,
    /// Each process's `Local Usage` on the GPU (PDH); `None` when per-process GPU memory can't
    /// be read (no PDH source, or its last read failed).
    pub local_by_pid: Option<BTreeMap<u32, u64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Measured {
    pub gpu: bool,
    pub cpu: bool,
}

/// A job's energy (Appendix F), written by `meter` when the job completes. `idle_unknown` is
/// serialised only when true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobEnergy {
    pub job_id: String,
    /// The workspace id, or `"loose"`.
    pub workspace: String,
    pub from: String,
    pub to: String,
    pub gpu_mj_gross: u64,
    pub gpu_mj_above_idle: u64,
    pub cpu_mj_gross: u64,
    pub cpu_mj_above_idle: u64,
    pub system_mj: u64,
    pub gpu_seconds_milli: u64,
    pub measured: Measured,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub idle_unknown: bool,
}

/// One succeeded job of the estimator's history (`telemetry.db` `job_stats`, Appendix G):
/// `wall_ms` is the sum of its execution windows, `gpu_seconds_milli` and `system_mj` its
/// `JobEnergy`'s (0 when it had none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStatsRow {
    pub job_id: String,
    pub kind: String,
    pub estimate_key: String,
    /// Unix ms.
    pub finished_ms: i64,
    pub wall_ms: u64,
    pub gpu_seconds_milli: u64,
    pub system_mj: u64,
}

/// `telemetry.energy`'s result (Appendix G, APP-131): the energy of the samples in a range,
/// both bounds included, and the whole system by `JobEnergy`'s formula.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnergyTotals {
    pub from: String,
    pub to: String,
    pub gpu_mj: u64,
    pub cpu_mj: u64,
    pub system_mj: u64,
    pub measured: Measured,
    /// `system_mj` at `[power] electricity`, in that rate's currency, when it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_cost: Option<Cost>,
}

/// The `telemetry.jobs` arguments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobEnergyQuery {
    #[serde(default)]
    pub job_ids: Option<Vec<String>>,
    /// RFC 3339 UTC: jobs that ended at or after it.
    #[serde(default)]
    pub from: Option<String>,
    /// RFC 3339 UTC: jobs that began at or before it.
    #[serde(default)]
    pub to: Option<String>,
    /// 1–500, default 100.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// The raw material of a job's energy over a stretch of time (`energy_between`): the totals of
/// the samples in it (the stretch is open at its start and closed at its end: a sample covers
/// the second before its time, so two stretches that meet count the sample at the meeting point
/// once), the shares of the GPU and the CPU by process, and the idle baselines
/// before it.
///
/// `sm_by_pid` is each process's SM utilisation summed over the stretch, in permille-seconds,
/// for every process the GPU reported. `cpu_ms_by_pid` is the CPU time of each tracked
/// process, and **pid 0 holds every other process together**, so the map's sum is the
/// machine's CPU time and a share of it is a real share. `idle_*_mw` are `None` when no idle
/// baseline exists (`idle_unknown`). `*_measured` say every sample of the stretch had that
/// energy from a hardware counter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnergySlice {
    pub gpu_mj: u64,
    pub cpu_mj: u64,
    pub sm_by_pid: BTreeMap<u32, u64>,
    pub cpu_ms_by_pid: BTreeMap<u32, u64>,
    pub idle_gpu_mw: Option<u32>,
    pub idle_cpu_mw: Option<u32>,
    pub gpu_measured: bool,
    pub cpu_measured: bool,
}

/// The resolution `telemetry.series` uses when none is asked for: the finest that gives at most
/// [`MAX_POINTS`] points of a range (a bucket counts when it holds any of the range, so one
/// more than the span's whole buckets) and that the retention still holds: 1-second samples are
/// kept 24 hours and 1-minute rows 30 days, so an older range gets a coarser answer than a
/// finer one that would be empty.
fn default_resolution(from: i64, to: i64, now: i64) -> Resolution {
    const SECOND: i64 = 1000;
    const MINUTE: i64 = 60 * SECOND;
    let span = to - from;
    if span < MAX_POINTS as i64 * SECOND && from >= now - db::KEEP_S1_MS {
        Resolution::Second
    } else if span <= (MAX_POINTS as i64 - 2) * MINUTE && from >= now - db::KEEP_M1_MS {
        Resolution::Minute
    } else {
        Resolution::Hour
    }
}

/// One point of a series: `[ts, min, average, max]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub ts: String,
    pub min: Value,
    pub avg: Value,
    pub max: Value,
}

impl Serialize for Point {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (&self.ts, &self.min, &self.avg, &self.max).serialize(s)
    }
}

/// A whole number as an integer, anything else as a float.
fn number(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

impl From<RawPoint> for Point {
    fn from(p: RawPoint) -> Point {
        Point {
            ts: format_ts(p.ts),
            min: json!(p.min),
            avg: number(p.avg),
            max: json!(p.max),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MetricSeries {
    pub metric: String,
    #[schemars(with = "Vec<(String, f64, f64, f64)>")]
    pub points: Vec<Point>,
}

/// The `telemetry.series` result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Series {
    pub series: Vec<MetricSeries>,
}

/// The `telemetry.series` arguments.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SeriesQuery {
    /// Any of `gpu.util`, `gpu.power`, `gpu.energy`, `gpu.temp`, `gpu.vram_used`, `cpu.util`,
    /// `cpu.power`, `cpu.energy`, `ram.used`, `system.power`, `net.rx`, `net.tx`.
    pub metrics: Vec<String>,
    /// RFC 3339 UTC; the range includes `from` and `to`.
    pub from: String,
    pub to: String,
    /// `1s`, `1m` or `1h`; by default the finest that gives at most 3,600 points.
    #[serde(default)]
    pub resolution: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Telemetry.
// ---------------------------------------------------------------------------------------------

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each holder leaves its data usable before anything can panic.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn params_invalid(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(&codes::PARAMS_INVALID, message, "")
}

fn db_error(what: &str, e: &dyn std::fmt::Display) -> ErrorBody {
    ErrorBody::new(&codes::STORE_IO, format!("telemetry.db: {what}: {e}"), "")
}

/// The sampler and what only a step touches.
struct Stepper {
    sampler: Sampler,
    /// Manual mode: the next sample's nominal second.
    virtual_secs: u64,
    /// Real mode: the previous step's monotonic time.
    last_now: Option<Duration>,
    last_prune_ms: i64,
}

type SampleHook = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    clock: Arc<dyn Clock>,
    events: Arc<EventBus>,
    mode: TickMode,
    /// `[power]`: the whole-system formula of `telemetry.energy`.
    power: PowerSettings,
    db: Db,
    /// The core run: its start, Unix ms. Samples store it, because monotonic times only compare
    /// within a run.
    run: i64,
    /// The monotonic time of the start, ms: with `run`, it maps a monotonic time to a wall time.
    run_mono_ms: i64,
    /// Held for a whole sample, from the step to the last hook, so concurrent samples (two
    /// `test.hw_tick` calls) are stored and published in the order they were taken.
    order: Mutex<()>,
    stepper: Mutex<Stepper>,
    latest: Mutex<Snapshot>,
    vram: Mutex<Option<VramView>>,
    tracked: Mutex<BTreeMap<u32, &'static str>>,
    busy: AtomicBool,
    /// Whether the machine has a GPU source (kept beside the sampler so asking never waits for a
    /// step).
    has_gpu: AtomicBool,
    hooks: Mutex<Vec<SampleHook>>,
    samples: watch::Sender<u64>,
}

impl Inner {
    /// One sample: reads, stores, publishes.
    fn sample_once(&self) {
        let _in_order = lock(&self.order);
        let mono = self.clock.monotonic();
        let out = {
            let mut stepper = lock(&self.stepper);
            let (now, dt_ms) = match self.mode {
                TickMode::Manual => {
                    let k = stepper.virtual_secs;
                    stepper.virtual_secs += 1;
                    (Duration::from_secs(k), if k == 0 { 0 } else { 1000 })
                }
                TickMode::Real => {
                    let dt = stepper.last_now.map_or(0, |last| {
                        u64::try_from(mono.saturating_sub(last).as_millis())
                            .unwrap_or(MAX_DT_MS)
                            .min(MAX_DT_MS)
                    });
                    stepper.last_now = Some(mono);
                    (mono, dt)
                }
            };
            let ts = stepper.sampler.next_ts(unix_ms(self.clock.utc_now()));
            let tracked = lock(&self.tracked).clone();
            let out = stepper.sampler.step(&StepIn {
                now,
                dt_ms,
                mono_ms: mono_ms(mono),
                utc_ms: ts,
                run: self.run,
                busy: self.busy.load(Ordering::Relaxed),
                tracked: &tracked,
            });
            self.has_gpu
                .store(stepper.sampler.has_gpu(), Ordering::Relaxed);
            if ts.saturating_sub(stepper.last_prune_ms) >= PRUNE_EVERY_MS {
                stepper.last_prune_ms = ts;
                if let Err(e) = self.db.prune(ts) {
                    tracing::warn!(error = %e, "telemetry.db retention failed");
                }
            }
            out
        };
        if let Err(e) = self.db.write(&out.row) {
            tracing::warn!(error = %e, "a telemetry sample could not be stored");
        }
        *lock(&self.latest) = out.snapshot.clone();
        *lock(&self.vram) = out.vram;
        for event in out.events {
            self.events.publish(TOPIC, event);
        }
        self.events.publish(
            TOPIC,
            json!({"kind": "sample", "snapshot": serde_json::to_value(&out.snapshot)
                .unwrap_or(Value::Null)}),
        );
        // Counted before the hooks run: a hook's `sample_count` is this sample's.
        self.samples.send_modify(|n| *n += 1);
        let hooks: Vec<SampleHook> = lock(&self.hooks).clone();
        for hook in hooks {
            hook();
        }
    }
}

/// The core's hardware telemetry (module docs).
pub struct Telemetry {
    inner: Arc<Inner>,
    cancel: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Telemetry")
            .field("mode", &self.inner.mode)
            .finish_non_exhaustive()
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Telemetry {
    /// Opens `telemetry.db` at `db_path` (setting a damaged one aside, CORE-004) and starts
    /// sampling in `mode`. Blocking for the length of the open; in [`TickMode::Real`] it spawns
    /// the sampler task, so it must run inside a tokio runtime.
    pub fn start(
        src: HwSources,
        db_path: PathBuf,
        clock: Arc<dyn Clock>,
        power: PowerSettings,
        events: Arc<EventBus>,
        mode: TickMode,
    ) -> Arc<Telemetry> {
        // Both clocks at one instant: `run` and `run_mono_ms` map monotonic times to wall
        // times, so they must not straddle the open below.
        let (started, started_mono) = (clock.utc_now(), clock.monotonic());
        let db = Db::open(
            &db_path,
            started
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0)),
        );
        let (run, run_mono_ms) = (unix_ms(started), mono_ms(started_mono));
        let mut sampler = Sampler::new(src, power.clone(), mode == TickMode::Real);
        if let Some(newest) = db.max_ts() {
            // A newer row than a day ahead of the clock is a wrong clock that was corrected:
            // following it would keep every new sample in the future, and the old rows past
            // pruning, so the clock wins.
            let now = unix_ms(started);
            sampler.seed_ts(if newest > now + MAX_SEED_AHEAD_MS {
                now - 1
            } else {
                newest
            });
        }
        let has_gpu = sampler.has_gpu();
        let inner = Arc::new(Inner {
            clock,
            events,
            mode,
            power,
            db,
            run,
            run_mono_ms,
            order: Mutex::new(()),
            stepper: Mutex::new(Stepper {
                sampler,
                virtual_secs: 0,
                last_now: None,
                last_prune_ms: i64::MIN,
            }),
            latest: Mutex::new(Snapshot::empty(run)),
            vram: Mutex::new(None),
            tracked: Mutex::new(BTreeMap::new()),
            busy: AtomicBool::new(false),
            has_gpu: AtomicBool::new(has_gpu),
            hooks: Mutex::new(Vec::new()),
            samples: watch::Sender::new(0),
        });
        let cancel = CancellationToken::new();
        let task =
            (mode == TickMode::Real).then(|| tokio::spawn(run_real(inner.clone(), cancel.clone())));
        Arc::new(Telemetry {
            inner,
            cancel,
            task: Mutex::new(task),
        })
    }

    /// Opens the GPU source again after repeated failures, with `opener`
    /// (`lil_platform::hw::nvml_gpu`).
    pub fn set_gpu_opener(&self, opener: GpuOpener) {
        lock(&self.inner.stepper).sampler.set_gpu_opener(opener);
    }

    /// Measures the workspace and model folders every 5 minutes with `probe` (CORE-061); until
    /// one is set, `sizes` are missing.
    pub fn set_sizes_probe(&self, probe: SizesProbe) {
        lock(&self.inner.stepper).sampler.set_sizes_probe(probe);
    }

    /// Whether this machine has a GPU source at all.
    pub fn has_gpu(&self) -> bool {
        self.inner.has_gpu.load(Ordering::Relaxed)
    }

    /// The latest snapshot (before the first sample, an empty one).
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.inner.latest).clone()
    }

    /// What the GPU scheduler needs (ARCH-071), from the latest real GPU reading. `None`
    /// without a GPU source, before the first reading and while the GPU is lost (the scheduler
    /// tells these apart with [`Telemetry::has_gpu`]).
    pub fn vram(&self) -> Option<VramView> {
        lock(&self.inner.vram).clone()
    }

    /// Starts following `pid`'s CPU time and memory (an engine process), labelled `role`.
    pub fn track(&self, pid: u32, role: &'static str) {
        lock(&self.inner.tracked).insert(pid, role);
    }

    pub fn untrack(&self, pid: u32) {
        lock(&self.inner.tracked).remove(&pid);
    }

    /// Stops following `pid` only while it is tracked with `role`: a connected app's close
    /// never untracks an engine process that took over its pid.
    pub fn untrack_if(&self, pid: u32, role: &str) {
        let mut tracked = lock(&self.inner.tracked);
        if tracked.get(&pid).is_some_and(|r| *r == role) {
            tracked.remove(&pid);
        }
    }

    /// The processes [`Telemetry::track`] follows, with their roles, by pid.
    pub fn tracked(&self) -> Vec<(u32, &'static str)> {
        lock(&self.inner.tracked)
            .iter()
            .map(|(pid, role)| (*pid, *role))
            .collect()
    }

    /// Per-process GPU memory is read every second while jobs run, else every 5 s.
    pub fn set_busy(&self, busy: bool) {
        self.inner.busy.store(busy, Ordering::Relaxed);
    }

    /// What [`Telemetry::set_busy`] set last.
    pub fn busy(&self) -> bool {
        self.inner.busy.load(Ordering::Relaxed)
    }

    /// Calls `f` after every sample, once it is counted ([`Telemetry::sample_count`]): the
    /// scheduler's VRAM re-check. It runs on the sampler's thread, so it must not block.
    pub fn on_sample(&self, f: Box<dyn Fn() + Send + Sync>) {
        lock(&self.inner.hooks).push(Arc::from(f));
    }

    /// How many samples have been taken.
    pub fn sample_count(&self) -> u64 {
        *self.inner.samples.borrow()
    }

    /// Waits until at least `count` samples have been taken, and returns the snapshot.
    pub async fn wait_for_samples(&self, count: u64) -> Snapshot {
        let mut rx = self.inner.samples.subscribe();
        let _ = rx.wait_for(|n| *n >= count).await;
        self.snapshot()
    }

    /// Takes `samples` samples now and returns the snapshot after them. A manual-mode call only
    /// (`test.hw_tick`): the real sampler ignores it and returns the current snapshot.
    pub async fn tick(&self, samples: u32) -> Snapshot {
        if self.inner.mode != TickMode::Manual {
            return self.snapshot();
        }
        let inner = self.inner.clone();
        let done = tokio::task::spawn_blocking(move || {
            for _ in 0..samples {
                inner.sample_once();
            }
        })
        .await;
        if let Err(e) = done {
            tracing::error!(error = %e, "a manual telemetry sample failed");
        }
        self.snapshot()
    }

    /// `q`'s metrics between its bounds (both included).
    ///
    /// # Errors
    /// `params.invalid` for an unknown metric, a time that isn't RFC 3339 UTC, a range backwards,
    /// an unknown resolution or more than 3,600 points of one metric; `store.io` when the
    /// database can't be read.
    pub async fn series(&self, q: SeriesQuery) -> Result<Series, ErrorBody> {
        if q.metrics.is_empty() {
            return Err(params_invalid("metrics must name at least one metric"));
        }
        let mut metrics = Vec::new();
        for name in &q.metrics {
            let metric = Metric::parse(name).ok_or_else(|| {
                let known: Vec<&str> = Metric::ALL.iter().map(|m| m.name()).collect();
                params_invalid(format!(
                    "{name:?} is not a metric; the metrics are {}",
                    known.join(", ")
                ))
            })?;
            if !metrics.contains(&metric) {
                metrics.push(metric);
            }
        }
        let time = |what: &str, text: &str| {
            parse_ts(text).ok_or_else(|| {
                params_invalid(format!(
                    "{what} {text:?} is not an RFC 3339 UTC time like 2026-10-18T12:00:00Z"
                ))
            })
        };
        let (from, to) = (time("from", &q.from)?, time("to", &q.to)?);
        if from > to {
            return Err(params_invalid("from is after to"));
        }
        let res = match q.resolution.as_deref() {
            Some("1s") => Resolution::Second,
            Some("1m") => Resolution::Minute,
            Some("1h") => Resolution::Hour,
            Some(other) => {
                return Err(params_invalid(format!(
                    "resolution {other:?} is not 1s, 1m or 1h"
                )));
            }
            None => default_resolution(from, to, unix_ms(self.inner.clock.utc_now())),
        };
        let inner = self.inner.clone();
        let rows = tokio::task::spawn_blocking(move || {
            metrics
                .into_iter()
                .map(|m| {
                    inner
                        .db
                        .series(m, from, to, res, MAX_POINTS + 1)
                        .map(|points| (m, points))
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
        .map_err(|e| db_error("the read", &e))?
        .map_err(|e| db_error("the read", &e))?;
        let mut series = Vec::new();
        for (metric, points) in rows {
            if points.len() > MAX_POINTS {
                return Err(params_invalid(format!(
                    "{} has more than {MAX_POINTS} points in that range; ask for a coarser \
                     resolution or a shorter range",
                    metric.name()
                )));
            }
            series.push(MetricSeries {
                metric: metric.name().to_string(),
                points: points.into_iter().map(Point::from).collect(),
            });
        }
        Ok(Series { series })
    }

    /// The energy and process shares of the samples taken between two monotonic times of this
    /// core (a job's execution window), with the idle baselines before them. A database that
    /// can't be read gives an empty slice, so a job's figures are missing rather than wrong.
    pub async fn energy_between(&self, from: Duration, to: Duration) -> EnergySlice {
        let inner = self.inner.clone();
        let (from_ms, to_ms) = (mono_ms(from), mono_ms(to));
        let wall_from = inner.run + (from_ms - inner.run_mono_ms);
        let slice = tokio::task::spawn_blocking(move || {
            inner.db.slice(inner.run, from_ms, to_ms, wall_from)
        })
        .await;
        match slice {
            Ok(Ok(slice)) => slice,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "a job's energy could not be read from telemetry.db");
                EnergySlice::default()
            }
            Err(e) => {
                tracing::warn!(error = %e, "a job's energy read did not finish");
                EnergySlice::default()
            }
        }
    }

    /// The wall time (Unix ms) of a monotonic time of this core run, as the samples' times are
    /// mapped: a window's ends as `JobEnergy`'s `from` and `to` when it holds no sample.
    pub fn wall_ms(&self, mono: Duration) -> i64 {
        self.inner.run + (mono_ms(mono) - self.inner.run_mono_ms)
    }

    /// The times (Unix ms) of the first and the last sample [`Telemetry::energy_between`] takes
    /// for the same stretch, so a `telemetry.series` over them (both bounds included) holds
    /// exactly those samples; `None` when it holds none, or the database can't be read.
    pub async fn sample_span(&self, from: Duration, to: Duration) -> Option<(i64, i64)> {
        let inner = self.inner.clone();
        let (from_ms, to_ms) = (mono_ms(from), mono_ms(to));
        tokio::task::spawn_blocking(move || inner.db.sample_span(inner.run, from_ms, to_ms))
            .await
            .ok()
            .and_then(|r| {
                r.map_err(|e| tracing::warn!(error = %e, "a job's sample times could not be read"))
                    .ok()
                    .flatten()
            })
    }

    /// Stores a job's energy. The caller is `meter`, the only writer; a second write for the
    /// same job replaces the first.
    ///
    /// # Errors
    /// `params.invalid` when `from` or `to` isn't RFC 3339 UTC; `store.io` when the write fails.
    pub async fn put_job_energy(&self, e: &JobEnergy) -> Result<(), ErrorBody> {
        let (Some(from), Some(to)) = (parse_ts(&e.from), parse_ts(&e.to)) else {
            return Err(params_invalid(format!(
                "a job's from and to are RFC 3339 UTC times; got {:?} and {:?}",
                e.from, e.to
            )));
        };
        let (inner, e) = (self.inner.clone(), e.clone());
        tokio::task::spawn_blocking(move || inner.db.put_job_energy(&e, from, to))
            .await
            .map_err(|e| db_error("the write", &e))?
            .map_err(|e| db_error("the write", &e))
    }

    /// The jobs' energy rows `q` selects, newest first (`telemetry.jobs`).
    ///
    /// # Errors
    /// `params.invalid` for a bad time or limit; `store.io` when the read fails.
    pub async fn job_energy(&self, q: JobEnergyQuery) -> Result<Vec<JobEnergy>, ErrorBody> {
        let bound = |what: &str, text: &Option<String>| match text {
            None => Ok(None),
            Some(t) => parse_ts(t)
                .map(Some)
                .ok_or_else(|| params_invalid(format!("{what} {t:?} is not an RFC 3339 UTC time"))),
        };
        let (from, to) = (bound("from", &q.from)?, bound("to", &q.to)?);
        let limit = match q.limit {
            None => 100,
            Some(n @ 1..=500) => n as usize,
            Some(n) => {
                return Err(params_invalid(format!(
                    "limit {n} is not between 1 and 500"
                )));
            }
        };
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.db.job_energy(&q, from, to, limit))
            .await
            .map_err(|e| db_error("the read", &e))?
            .map_err(|e| db_error("the read", &e))
    }

    /// Stores a job's history row (`meter` is the only writer): once per job, a second write
    /// for the same job changing nothing, and only the newest 20 rows of its kind and key are
    /// kept (Appendix G).
    ///
    /// # Errors
    /// `store.io` when the write fails.
    pub async fn put_job_stats(&self, row: &JobStatsRow) -> Result<(), ErrorBody> {
        let (inner, row) = (self.inner.clone(), row.clone());
        tokio::task::spawn_blocking(move || inner.db.put_job_stats(&row))
            .await
            .map_err(|e| db_error("the write", &e))?
            .map(drop)
            .map_err(|e| db_error("the write", &e))
    }

    /// The newest `newest` history rows of jobs of `kind` with estimate key `key`, newest
    /// first (by finish time, then job id).
    ///
    /// # Errors
    /// `store.io` when the read fails.
    pub async fn history(
        &self,
        kind: &str,
        key: &str,
        newest: u32,
    ) -> Result<Vec<JobStatsRow>, ErrorBody> {
        let (inner, kind, key) = (self.inner.clone(), kind.to_string(), key.to_string());
        tokio::task::spawn_blocking(move || inner.db.history(&kind, &key, newest))
            .await
            .map_err(|e| db_error("the read", &e))?
            .map_err(|e| db_error("the read", &e))
    }

    /// The energy of the samples from `from` to `to` (Unix ms, both included), from the finest
    /// table whose retention still holds `from` (`s1`, `m1`, then `h1`; a coarser bucket counts
    /// when it holds any of the range), with `JobEnergy`'s whole-system formula: (GPU + CPU +
    /// `rest_of_system_w` over the samples' time) / `psu_efficiency` (APP-131; Appendix G).
    ///
    /// # Errors
    /// `params.invalid` when `from` is after `to`; `store.io` when the read fails.
    pub async fn energy(&self, from: i64, to: i64) -> Result<EnergyTotals, ErrorBody> {
        if from > to {
            return Err(params_invalid("from is after to"));
        }
        let now = unix_ms(self.inner.clock.utc_now());
        let res = if from >= now - db::KEEP_S1_MS {
            Resolution::Second
        } else if from >= now - db::KEEP_M1_MS {
            Resolution::Minute
        } else {
            Resolution::Hour
        };
        let inner = self.inner.clone();
        let found = tokio::task::spawn_blocking(move || inner.db.energy(from, to, res))
            .await
            .map_err(|e| db_error("the read", &e))?
            .map_err(|e| db_error("the read", &e))?;
        let power = &self.inner.power;
        let rest_mj = u64::from(power.rest_of_system_mw).saturating_mul(found.covered_ms) / 1000;
        let psu = u64::from(power.psu_efficiency_permille.clamp(1, 1000));
        let system_mj = share(found.gpu_mj + found.cpu_mj + rest_mj, 1000, psu);
        let energy_cost = power.electricity.as_ref().map(|(rate, currency)| {
            // A kWh is 3.6e9 mJ.
            let rate = u64::try_from(rate.micros()).unwrap_or(0);
            let micros = share(rate, system_mj, 3_600_000_000);
            Cost {
                amount: Money::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
                    .to_decimal_string(),
                currency: currency.as_str().to_string(),
            }
        });
        Ok(EnergyTotals {
            from: format_ts(from),
            to: format_ts(to),
            gpu_mj: found.gpu_mj,
            cpu_mj: found.cpu_mj,
            system_mj,
            measured: Measured {
                gpu: found.gpu_measured,
                cpu: found.cpu_measured,
            },
            energy_cost,
        })
    }

    /// Cancelled when the telemetry stops (its [`shutdown`](Telemetry::shutdown) or its drop).
    pub fn stopped(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Stops sampling and flushes `telemetry.db`.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let task = lock(&self.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let inner = self.inner.clone();
        let _ = tokio::task::spawn_blocking(move || inner.db.checkpoint()).await;
    }
}

/// The real sampler: a step each second, on a blocking thread (the sources block).
async fn run_real(inner: Arc<Inner>, cancel: CancellationToken) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = every.tick() => {}
        }
        let step = inner.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || step.sample_once()).await {
            tracing::error!(error = %e, "a telemetry sample failed");
        }
    }
}

/// How long a measured folder size is kept before the folder is walked again.
const SIZE_REFRESH: Duration = Duration::from_secs(270);

/// A [`SizesProbe`] for the workspace folder (CORE-059): [`sizes_probe`] of `root` alone.
pub fn folder_size_probe(root: PathBuf) -> SizesProbe {
    sizes_probe(Some(root), None)
}

/// A [`SizesProbe`] for the workspace folder and ComfyUI's models folder (CORE-059):
/// `workspace_mib` and `models_mib` are the total size of the files under each, links not
/// followed (a folder not given stays missing). A walk of a big folder takes seconds, so the
/// probe never walks on the sampler's thread: each folder is walked on a thread of its own,
/// started by the first call and again when its last result is older than 4.5 minutes, and the
/// probe answers with the last results. Its first answer waits until every folder given has
/// been measured (until then it answers nothing, and the sampler asks again at its next step),
/// so both sizes appear together.
pub fn sizes_probe(workspace: Option<PathBuf>, models: Option<PathBuf>) -> SizesProbe {
    let mut workspace = workspace.map(FolderWalk::new);
    let mut models = models.map(FolderWalk::new);
    Box::new(move || {
        let workspace_mib = workspace.as_mut().map(FolderWalk::poll);
        let models_mib = models.as_mut().map(FolderWalk::poll);
        if workspace_mib.is_some_and(|m| m.is_none()) || models_mib.is_some_and(|m| m.is_none()) {
            return Sizes::default();
        }
        Sizes {
            workspace_mib: workspace_mib.flatten(),
            models_mib: models_mib.flatten(),
        }
    })
}

/// One folder of a [`sizes_probe`], walked off the caller's thread.
struct FolderWalk {
    root: PathBuf,
    /// The newest total in MiB plus one; 0 is "not measured yet".
    total: Arc<AtomicU64>,
    walking: Arc<AtomicBool>,
    started: Option<std::time::Instant>,
}

impl FolderWalk {
    fn new(root: PathBuf) -> FolderWalk {
        FolderWalk {
            root,
            total: Arc::new(AtomicU64::new(0)),
            walking: Arc::new(AtomicBool::new(false)),
            started: None,
        }
    }

    /// The last total, if any; starts a walk when there is none yet or it is stale.
    fn poll(&mut self) -> Option<u64> {
        let stale = self.started.is_none_or(|at| at.elapsed() >= SIZE_REFRESH);
        if stale && !self.walking.swap(true, Ordering::SeqCst) {
            self.started = Some(std::time::Instant::now());
            let (root, total, walking) =
                (self.root.clone(), self.total.clone(), self.walking.clone());
            std::thread::spawn(move || {
                total.store(folder_mib(&root) + 1, Ordering::SeqCst);
                walking.store(false, Ordering::SeqCst);
            });
        }
        self.total.load(Ordering::SeqCst).checked_sub(1)
    }
}

/// The size of the files under `root` in MiB (rounded down); links are not followed, and what
/// can't be read counts as nothing.
fn folder_mib(root: &std::path::Path) -> u64 {
    let (mut bytes, mut pending) = (0_u64, vec![root.to_path_buf()]);
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                bytes += entry.metadata().map_or(0, |m| m.len());
            }
        }
    }
    bytes / (1024 * 1024)
}

/// The `telemetry` topic's snapshot (ARCH-044): the current [`Snapshot`]. Held weakly, because
/// the telemetry holds the bus that holds the provider.
struct TopicSnapshots {
    telemetry: Weak<Telemetry>,
}

impl SnapshotProvider for TopicSnapshots {
    fn snapshot<'a>(
        &'a self,
        _topic: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            match self.telemetry.upgrade() {
                Some(t) => serde_json::to_value(t.snapshot()).unwrap_or(Value::Null),
                None => json!({"error": "the core is shutting down"}),
            }
        })
    }
}

/// Registers the `telemetry` topic's snapshot provider on `events`.
pub fn register_snapshot(events: &EventBus, telemetry: &Arc<Telemetry>) {
    events.register_snapshot(
        TOPIC,
        Arc::new(TopicSnapshots {
            telemetry: Arc::downgrade(telemetry),
        }),
    );
}

pub use profile::profile_check;
