//! Hardware telemetry sources (CORE-059, CORE-060): the GPU through NVML, per-process GPU
//! memory and the CPU package's Energy Meter through PDH, and CPU, RAM, processes, network and
//! disks through `sysinfo`.
//!
//! Each source is a trait so the core's sampler and its tests can use fakes. A missing source
//! (no NVIDIA GPU, no `nvml.dll`, no Energy Meter counter set) is `None`, never an error: the
//! telemetry shows the part as unknown. CPU temperature is never read, and no CPU power is
//! guessed here: an estimate from `[power] cpu_tdp_w` is the telemetry module's business.

use std::io;

use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System,
};

pub use crate::hw_nvml::nvml_gpu;
pub use crate::hw_pdh::{energy_meter, nvidia_adapter_luid, pdh_gpu_memory};

/// NVML's clock event ("throttle") reason bits, as the driver reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThrottleReasons(pub u64);

impl ThrottleReasons {
    /// `SW_THERMAL_SLOWDOWN` (0x20) or `HW_THERMAL_SLOWDOWN` (0x40).
    pub fn thermal(self) -> bool {
        self.0 & 0x60 != 0
    }

    /// `HW_POWER_BRAKE_SLOWDOWN` (0x80): an external power brake, such as a PSU signal.
    pub fn power_brake(self) -> bool {
        self.0 & 0x80 != 0
    }
}

/// One GPU reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSample {
    pub name: String,
    pub driver: String,
    pub util_pct: u8,
    /// NVML's power reading (averaged by the driver); `None` where the GPU doesn't support it
    /// (NVML says `NotSupported`). That one field missing must not fail the whole sample: the
    /// telemetry falls back to `power_instant_mw`, then to the energy counter's rate, and
    /// labels what it used.
    pub power_mw: Option<u32>,
    /// The instantaneous power field, where the GPU has it.
    pub power_instant_mw: Option<u32>,
    /// The total energy counter since the driver loaded, where the GPU has it.
    pub energy_mj: Option<u64>,
    pub temp_c: u8,
    pub sm_clock_mhz: u32,
    pub mem_clock_mhz: u32,
    pub fan_pct: Option<u8>,
    pub throttle: ThrottleReasons,
    pub vram_total_mib: u64,
    pub vram_used_mib: u64,
    /// Each process's SM (3D and compute) utilisation over the time since the previous sample,
    /// in permille of the GPU.
    pub sm_by_pid: Vec<(u32, u16)>,
}

/// What a GPU source can report; a `false` field is `None` (or empty) in every sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuFeatures {
    pub energy_counter: bool,
    pub instant_power: bool,
    pub process_sm: bool,
    pub fan: bool,
}

pub trait GpuSource: Send {
    fn sample(&mut self) -> io::Result<GpuSample>;
    fn features(&self) -> GpuFeatures;
}

/// One process's GPU memory on the NVIDIA adapter, from PDH `GPU Process Memory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcGpuMem {
    pub pid: u32,
    /// `Local Usage`: what the scheduler's free-VRAM sum reads (ARCH-071).
    pub local_mib: u64,
    /// `Dedicated Usage`, which overstates some processes (ARCH-071, M0 result).
    pub dedicated_mib: u64,
    /// `Shared Usage`: system memory the GPU maps for the process (the fit signal).
    pub shared_mib: u64,
    /// `Total Committed`.
    pub committed_mib: u64,
}

pub trait GpuMemSource: Send {
    /// Every process with GPU memory on the adapter, one entry per pid.
    fn sample(&mut self) -> io::Result<Vec<ProcGpuMem>>;
}

/// One Energy Meter reading for the CPU package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuEnergySample {
    /// The raw `Energy` counter in picowatt-hours. Accumulate deltas of this value and convert
    /// with [`pwh_to_mj`] only when a figure is read, so no rounding builds up.
    pub package_energy_pwh: u64,
    /// The `Power` counter.
    pub package_power_mw: u32,
}

/// Picowatt-hours to millijoules: 1 pWh = 3.6 × 10⁻⁶ mJ. Exact in 128-bit arithmetic, then
/// truncated.
pub fn pwh_to_mj(pwh: u64) -> u64 {
    (u128::from(pwh) * 36 / 10_000_000) as u64
}

pub trait CpuEnergySource: Send {
    fn sample(&mut self) -> io::Result<CpuEnergySample>;
}

/// One CPU, memory, process, network and disk reading.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SysSample {
    /// All cores, since the previous sample.
    pub cpu_permille: u16,
    pub per_core_permille: Vec<u16>,
    pub ram_total_mib: u64,
    pub ram_used_mib: u64,
    /// For each tracked pid still running: `(pid, CPU time used so far in ms, resident MiB)`.
    pub procs: Vec<(u32, u64, u64)>,
    /// For each network adapter: `(name, bytes received, bytes sent)`, the adapter's running
    /// totals; rates come from the difference between samples.
    pub net: Vec<(String, u64, u64)>,
    /// For each mounted volume: `(mount point, free MiB)`.
    pub disks: Vec<(String, u64)>,
}

pub trait SysSource: Send {
    /// Reads everything; `tracked` names the processes whose CPU time and memory are wanted.
    fn sample(&mut self, tracked: &[u32]) -> SysSample;
}

/// The `sysinfo` source.
pub fn sysinfo_source() -> Box<dyn SysSource> {
    let system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    Box::new(SysinfoSource {
        system,
        networks: Networks::new_with_refreshed_list(),
        disks: Disks::new_with_refreshed_list(),
    })
}

struct SysinfoSource {
    system: System,
    networks: Networks,
    disks: Disks,
}

const MIB: u64 = 1024 * 1024;

fn permille(pct: f32) -> u16 {
    (pct * 10.0).round().clamp(0.0, 1000.0) as u16
}

impl SysSource for SysinfoSource {
    fn sample(&mut self, tracked: &[u32]) -> SysSample {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        let pids: Vec<Pid> = tracked.iter().map(|&p| Pid::from_u32(p)).collect();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        self.networks.refresh(true);
        self.disks.refresh(true);
        SysSample {
            cpu_permille: permille(self.system.global_cpu_usage()),
            per_core_permille: self
                .system
                .cpus()
                .iter()
                .map(|c| permille(c.cpu_usage()))
                .collect(),
            ram_total_mib: self.system.total_memory() / MIB,
            ram_used_mib: self.system.used_memory() / MIB,
            procs: pids
                .iter()
                .filter_map(|&pid| {
                    let p = self.system.process(pid)?;
                    Some((pid.as_u32(), p.accumulated_cpu_time(), p.memory() / MIB))
                })
                .collect(),
            net: self
                .networks
                .list()
                .iter()
                .map(|(name, data)| {
                    (
                        name.clone(),
                        data.total_received(),
                        data.total_transmitted(),
                    )
                })
                .collect(),
            disks: self
                .disks
                .list()
                .iter()
                .map(|d| {
                    (
                        d.mount_point().display().to_string(),
                        d.available_space() / MIB,
                    )
                })
                .collect(),
        }
    }
}
