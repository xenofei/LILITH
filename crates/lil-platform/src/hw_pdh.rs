//! Performance counters through PDH (CORE-059, ARCH-071), readable without admin rights:
//! `GPU Process Memory` for each process's memory on the NVIDIA adapter, the adapter's
//! `GPU Adapter Memory` and `GPU Engine` load (`docs/spikes/12-browse-core.md`, S0.1), and the CPU
//! package's `Energy Meter` (RAPL). Counter and instance names are the English ones
//! (`PdhAddEnglishCounterW`), so a localised Windows reads the same paths. The adapter's LUID
//! comes from DXGI.

use std::collections::BTreeMap;
use std::io;

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA, PDH_RAW_COUNTER, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue, PdhGetRawCounterValue,
    PdhOpenQueryW,
};
use windows::core::{HSTRING, PCWSTR};

use crate::hw::{
    AdapterMemory, CpuEnergySample, CpuEnergySource, EngineUtil, GpuCounters, GpuMemSource,
    ProcGpuMem,
};
use crate::hw_nvml::nvml_identity;

const MIB: u64 = 1024 * 1024;
const NVIDIA_VENDOR_ID: u32 = 0x10DE;

/// The `(LowPart, HighPart)` LUID of the DXGI adapter that is the GPU [`nvml_gpu`] reads
/// (NVML device 0), so per-process memory comes from the same card as the GPU figures.
///
/// DXGI and NVML share no id, so the adapter is matched by **name and total VRAM**: the
/// NVIDIA hardware adapter whose description equals NVML's name (ignoring case and surrounding
/// spaces) and whose dedicated video memory is within 1/16 of NVML's total. `None` when NVML
/// is missing or when no adapter, or more than one (two identical cards), matches: the
/// per-process GPU memory is then unknown, never read from another GPU.
///
/// [`nvml_gpu`]: crate::hw::nvml_gpu
pub fn nvidia_adapter_luid() -> Option<(u32, i32)> {
    let (name, vram_bytes) = nvml_identity()?;
    match_adapter(&name, vram_bytes, &nvidia_adapters())
}

/// One NVIDIA hardware adapter as DXGI lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DxgiAdapter {
    description: String,
    dedicated_bytes: u64,
    luid: (u32, i32),
}

fn nvidia_adapters() -> Vec<DxgiAdapter> {
    let mut found = Vec::new();
    // SAFETY: plain call; the factory is released when dropped.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return found;
    };
    for index in 0.. {
        // SAFETY: the factory is live; an index past the last adapter is an error, which ends
        // the loop.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: the adapter is live.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        if desc.VendorId == NVIDIA_VENDOR_ID && !software {
            let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
            found.push(DxgiAdapter {
                description: String::from_utf16_lossy(&desc.Description[..len]),
                dedicated_bytes: desc.DedicatedVideoMemory as u64,
                luid: (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
            });
        }
    }
    found
}

/// The one adapter matching `name` and `vram_bytes` (see [`nvidia_adapter_luid`]), or `None`
/// when none or several do.
fn match_adapter(name: &str, vram_bytes: u64, adapters: &[DxgiAdapter]) -> Option<(u32, i32)> {
    let name = name.trim();
    let mut matches = adapters.iter().filter(|a| {
        let close =
            a.dedicated_bytes.abs_diff(vram_bytes) <= a.dedicated_bytes.max(vram_bytes) / 16;
        close && a.description.trim().eq_ignore_ascii_case(name)
    });
    match (matches.next(), matches.next()) {
        (Some(only), None) => Some(only.luid),
        _ => None,
    }
}

/// An open PDH query, closed on drop.
struct Query(PDH_HQUERY);

// SAFETY: a PDH query handle isn't tied to the thread that opened it; `Query` is used by one
// thread at a time (`&mut self` for every collection).
unsafe impl Send for Query {}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the handle came from `PdhOpenQueryW` and is closed only here.
        unsafe { PdhCloseQuery(self.0) };
    }
}

fn pdh_error(what: &str, status: u32) -> io::Error {
    io::Error::other(format!("PDH {what}: status 0x{status:08X}"))
}

fn valid(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

impl Query {
    fn open() -> io::Result<Query> {
        let mut handle = PDH_HQUERY::default();
        // SAFETY: the live data source (no log file); `handle` is a valid out-pointer.
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) };
        if status != 0 {
            return Err(pdh_error("open", status));
        }
        Ok(Query(handle))
    }

    fn add(&self, path: &str) -> io::Result<PDH_HCOUNTER> {
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: the query is open; the path is a NUL-terminated wide string that outlives
        // the call; `counter` is a valid out-pointer.
        let status =
            unsafe { PdhAddEnglishCounterW(self.0, &HSTRING::from(path), 0, &mut counter) };
        if status != 0 {
            return Err(pdh_error(path, status));
        }
        Ok(counter)
    }

    fn collect(&mut self) -> io::Result<()> {
        // SAFETY: the query is open.
        let status = unsafe { PdhCollectQueryData(self.0) };
        if status != 0 {
            return Err(pdh_error("collect", status));
        }
        Ok(())
    }
}

/// Every instance of a wildcard counter as `(instance name, value)`, skipping instances whose
/// value isn't valid: the 64-bit integer form.
fn instances(counter: PDH_HCOUNTER) -> io::Result<Vec<(String, i64)>> {
    // SAFETY: PDH_FMT_LARGE fills `largeValue`.
    instances_as(counter, PDH_FMT_LARGE, |v| unsafe {
        v.Anonymous.largeValue
    })
}

/// [`instances`] in the floating-point form (a percentage).
fn instances_f64(counter: PDH_HCOUNTER) -> io::Result<Vec<(String, f64)>> {
    // SAFETY: PDH_FMT_DOUBLE fills `doubleValue`.
    instances_as(counter, PDH_FMT_DOUBLE, |v| unsafe {
        v.Anonymous.doubleValue
    })
}

fn instances_as<T>(
    counter: PDH_HCOUNTER,
    format: PDH_FMT,
    value: impl Fn(&PDH_FMT_COUNTERVALUE) -> T,
) -> io::Result<Vec<(String, T)>> {
    let (mut size, mut count) = (0u32, 0u32);
    // SAFETY: the size query: no buffer; PDH sets `size` and returns PDH_MORE_DATA.
    let status =
        unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut size, &mut count, None) };
    if status != PDH_MORE_DATA {
        // No instance at all (no process uses the GPU) reports success with nothing to read.
        return if status == 0 {
            Ok(Vec::new())
        } else {
            Err(pdh_error("array size", status))
        };
    }
    // `u64`s so the items are aligned; the names follow them in the same buffer.
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    // SAFETY: `buf` is writable for `size` bytes and aligned for the items.
    let status = unsafe {
        PdhGetFormattedCounterArrayW(counter, format, &mut size, &mut count, Some(items))
    };
    if status != 0 {
        // Instances that appeared since the size query: the next sample reads them.
        return Err(pdh_error("array", status));
    }
    // SAFETY: PDH wrote `count` items at the start of `buf`.
    let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
    Ok(items
        .iter()
        .filter(|item| valid(item.FmtValue.CStatus))
        .map(|item| {
            // SAFETY: each name is a NUL-terminated wide string inside `buf`.
            let name = unsafe { item.szName.to_string().unwrap_or_default() };
            (name, value(&item.FmtValue))
        })
        .collect())
}

/// The four `GPU Process Memory` counters, in `ProcGpuMem` order.
const GPU_MEMORY_COUNTERS: [&str; 4] = [
    "Local Usage",
    "Dedicated Usage",
    "Shared Usage",
    "Total Committed",
];

/// `\GPU Process Memory(pid_*_luid_0x<hi>_0x<lo>_phys_*)\…` for the adapter `adapter_luid`
/// (`(LowPart, HighPart)`), or for every adapter when it is `None`. `None` when the counter set
/// can't be opened.
pub fn pdh_gpu_memory(adapter_luid: Option<(u32, i32)>) -> Option<Box<dyn GpuMemSource>> {
    let query = Query::open().ok()?;
    let mut counters = Vec::with_capacity(GPU_MEMORY_COUNTERS.len());
    for name in GPU_MEMORY_COUNTERS {
        counters.push(query.add(&format!(r"\GPU Process Memory(*)\{name}")).ok()?);
    }
    let luid = adapter_luid.map(|(low, high)| format!("_luid_0x{:08x}_0x{low:08x}_", high as u32));
    let mut source = PdhGpuMemory {
        query,
        counters,
        luid,
    };
    source.sample().ok()?;
    Some(Box::new(source))
}

struct PdhGpuMemory {
    query: Query,
    counters: Vec<PDH_HCOUNTER>,
    /// `_luid_0x<hi>_0x<lo>_`, lower case, as it appears in the instance names.
    luid: Option<String>,
}

// SAFETY: the counter handles belong to `query`, which is `Send`; they are used only through
// `&mut self`.
unsafe impl Send for PdhGpuMemory {}

/// The pid in `pid_<pid>_luid_…`.
fn instance_pid(instance: &str) -> Option<u32> {
    instance
        .strip_prefix("pid_")?
        .split('_')
        .next()?
        .parse()
        .ok()
}

impl GpuMemSource for PdhGpuMemory {
    fn sample(&mut self) -> io::Result<Vec<ProcGpuMem>> {
        self.query.collect()?;
        let mut by_pid: BTreeMap<u32, ProcGpuMem> = BTreeMap::new();
        for (index, &counter) in self.counters.iter().enumerate() {
            for (instance, value) in instances(counter)? {
                let instance = instance.to_ascii_lowercase();
                if self
                    .luid
                    .as_ref()
                    .is_some_and(|l| !instance.contains(l.as_str()))
                {
                    continue;
                }
                let Some(pid) = instance_pid(&instance) else {
                    continue;
                };
                let entry = by_pid.entry(pid).or_insert(ProcGpuMem {
                    pid,
                    ..ProcGpuMem::default()
                });
                // A process can have several instances (one per physical adapter part): sum.
                let mib = u64::try_from(value).unwrap_or(0) / MIB;
                let field = match index {
                    0 => &mut entry.local_mib,
                    1 => &mut entry.dedicated_mib,
                    2 => &mut entry.shared_mib,
                    _ => &mut entry.committed_mib,
                };
                *field += mib;
            }
        }
        Ok(by_pid.into_values().filter(|p| p.pid != 0).collect())
    }
}

/// `luid_0x<hi>_0x<lo>_`, lower case, as it appears in the GPU counters' instance names, for
/// the adapter `(LowPart, HighPart)`.
fn luid_tag((low, high): (u32, i32)) -> String {
    format!("luid_0x{:08x}_0x{low:08x}_", high as u32)
}

/// `\GPU Adapter Memory(*)\Dedicated Usage` and `Shared Usage`, and
/// `\GPU Engine(*)\Utilization Percentage`, for the adapter `adapter_luid` (`(LowPart,
/// HighPart)`). `None` when the counters can't be added or read (CORE-059's "not shown" path).
/// The engine load is a rate between two collections, so it has its own query: each
/// [`GpuCounters`] call collects only what it reads.
pub fn pdh_gpu_counters(adapter_luid: (u32, i32)) -> Option<Box<dyn GpuCounters>> {
    let memory = Query::open().ok()?;
    let dedicated = memory.add(r"\GPU Adapter Memory(*)\Dedicated Usage").ok()?;
    let shared = memory.add(r"\GPU Adapter Memory(*)\Shared Usage").ok()?;
    let engine = Query::open().ok()?;
    let utilization = engine.add(r"\GPU Engine(*)\Utilization Percentage").ok()?;
    let mut counters = PdhGpuCounters {
        memory,
        dedicated,
        shared,
        engine,
        utilization,
        luid: luid_tag(adapter_luid),
    };
    counters.adapter_memory()?;
    // The first collection is the base of the first rate.
    counters.engine.collect().ok()?;
    Some(Box::new(counters))
}

struct PdhGpuCounters {
    memory: Query,
    dedicated: PDH_HCOUNTER,
    shared: PDH_HCOUNTER,
    engine: Query,
    utilization: PDH_HCOUNTER,
    luid: String,
}

// SAFETY: as for `PdhGpuMemory`.
unsafe impl Send for PdhGpuCounters {}

impl GpuCounters for PdhGpuCounters {
    fn adapter_memory(&mut self) -> Option<AdapterMemory> {
        self.memory.collect().ok()?;
        let dedicated = instances(self.dedicated).ok()?;
        let shared = instances(self.shared).ok()?;
        Some(AdapterMemory {
            dedicated_mib: adapter_mib(&dedicated, &self.luid)?,
            shared_mib: adapter_mib(&shared, &self.luid)?,
        })
    }

    fn engines(&mut self) -> Option<Vec<EngineUtil>> {
        self.engine.collect().ok()?;
        let found = instances_f64(self.utilization).ok()?;
        Some(busiest_engines(&found, &self.luid))
    }
}

/// The adapter's figure in MiB: its `luid_<luid>_phys_<n>` instances summed (one per physical
/// part); `None` when the adapter has no instance.
fn adapter_mib(found: &[(String, i64)], luid: &str) -> Option<u64> {
    let mut parts = found
        .iter()
        .filter(|(name, _)| name.to_ascii_lowercase().contains(luid))
        .peekable();
    parts.peek()?;
    Some(
        parts
            .map(|(_, v)| u64::try_from(*v).unwrap_or(0))
            .sum::<u64>()
            / MIB,
    )
}

/// Per engine type of the adapter `luid`, its busiest engine's load (CORE-059): an engine is an
/// instance name without its `pid_<pid>_` prefix, and its load is the sum over the processes
/// that use it. The type is everything after `engtype_` (it may hold spaces and underscores).
fn busiest_engines(found: &[(String, f64)], luid: &str) -> Vec<EngineUtil> {
    let mut by_engine: BTreeMap<String, (String, f64)> = BTreeMap::new();
    for (name, pct) in found {
        let lower = name.to_ascii_lowercase();
        let (Some(at), Some(type_at)) = (lower.find(luid), lower.find("_engtype_")) else {
            continue;
        };
        let engtype = name[type_at + "_engtype_".len()..].to_string();
        let entry = by_engine
            .entry(lower[at..].to_string())
            .or_insert((engtype, 0.0));
        entry.1 += pct.max(0.0);
    }
    let mut by_type: BTreeMap<String, u16> = BTreeMap::new();
    for (engtype, pct) in by_engine.into_values() {
        let permille = (pct * 10.0).round().clamp(0.0, 1000.0) as u16;
        let busiest = by_type.entry(engtype).or_insert(0);
        *busiest = (*busiest).max(permille);
    }
    by_type
        .into_iter()
        .map(|(engtype, util_permille)| EngineUtil {
            engtype,
            util_permille,
        })
        .collect()
}

/// The CPU package's `\Energy Meter(RAPL_Package0_PKG)\Energy` (picowatt-hours, kept raw) and
/// `\Power` (milliwatts). `None` when the counter set or the instance is missing.
///
/// `Power` is an average-count counter: PDH computes it from the energy and time between two
/// collections, so each sample's power is the average since the previous sample (or since this
/// call, for the first).
pub fn energy_meter() -> Option<Box<dyn CpuEnergySource>> {
    let query = Query::open().ok()?;
    let energy = query.add(r"\Energy Meter(RAPL_Package0_PKG)\Energy").ok()?;
    let power = query.add(r"\Energy Meter(RAPL_Package0_PKG)\Power").ok()?;
    let mut meter = EnergyMeter {
        query,
        energy,
        power,
    };
    // The first collection: it proves the instance exists and is the base `Power` needs.
    meter.query.collect().ok()?;
    meter.energy_pwh().ok()?;
    Some(Box::new(meter))
}

struct EnergyMeter {
    query: Query,
    energy: PDH_HCOUNTER,
    power: PDH_HCOUNTER,
}

// SAFETY: as for `PdhGpuMemory`.
unsafe impl Send for EnergyMeter {}

impl EnergyMeter {
    /// The raw `Energy` value of the last collection.
    fn energy_pwh(&self) -> io::Result<u64> {
        let mut raw = PDH_RAW_COUNTER::default();
        // SAFETY: the counter belongs to the open query; `raw` is a valid out-pointer.
        let status = unsafe { PdhGetRawCounterValue(self.energy, None, &mut raw) };
        if status != 0 || !valid(raw.CStatus) {
            return Err(pdh_error(
                "Energy",
                if status != 0 { status } else { raw.CStatus },
            ));
        }
        Ok(u64::try_from(raw.FirstValue).unwrap_or(0))
    }
}

impl CpuEnergySource for EnergyMeter {
    /// Fails when too little time has passed since the previous collection for PDH to compute
    /// `Power`; the next sample reads it.
    fn sample(&mut self) -> io::Result<CpuEnergySample> {
        self.query.collect()?;
        let package_energy_pwh = self.energy_pwh()?;
        let mut power = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: as above; `power` is a valid out-pointer.
        let status =
            unsafe { PdhGetFormattedCounterValue(self.power, PDH_FMT_LARGE, None, &mut power) };
        if status != 0 || !valid(power.CStatus) {
            return Err(pdh_error(
                "Power",
                if status != 0 { status } else { power.CStatus },
            ));
        }
        // SAFETY: PDH_FMT_LARGE fills `largeValue`.
        let power_mw = unsafe { power.Anonymous.largeValue };
        Ok(CpuEnergySample {
            package_energy_pwh,
            package_power_mw: u32::try_from(power_mw).unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn adapter(description: &str, dedicated_bytes: u64, low: u32) -> DxgiAdapter {
        DxgiAdapter {
            description: description.to_string(),
            dedicated_bytes,
            luid: (low, 0),
        }
    }

    #[test] // spec: CORE-059, ARCH-071
    fn match_adapter_needs_one_adapter_with_the_same_name_and_vram() {
        let card = "NVIDIA GeForce RTX 4080";
        let twelve = adapter("NVIDIA GeForce RTX 4070", 12 * GIB, 1);
        let sixteen = adapter("NVIDIA GeForce RTX 4080", 16 * GIB - 300 * MIB, 2);
        // NVML device 0 is the 16 GB card even when DXGI lists the other first; DXGI's
        // dedicated memory is a little under NVML's total.
        assert_eq!(
            match_adapter(card, 16 * GIB, &[twelve.clone(), sixteen.clone()]),
            Some((2, 0))
        );
        // Name compared ignoring case and surrounding spaces.
        assert_eq!(
            match_adapter(
                " nvidia geforce rtx 4080 ",
                16 * GIB,
                std::slice::from_ref(&sixteen)
            ),
            Some((2, 0))
        );
        // Same name, different memory: not this card.
        assert_eq!(
            match_adapter(card, 8 * GIB, std::slice::from_ref(&sixteen)),
            None
        );
        // No adapter of that name.
        assert_eq!(match_adapter(card, 16 * GIB, &[twelve]), None);
        // Two identical cards: ambiguous, so unknown.
        let twin = adapter("NVIDIA GeForce RTX 4080", 16 * GIB - 300 * MIB, 3);
        assert_eq!(match_adapter(card, 16 * GIB, &[sixteen, twin]), None);
        assert_eq!(match_adapter(card, 16 * GIB, &[]), None);
    }

    #[test] // spec: CORE-059
    fn instance_pid_reads_the_pid_of_a_gpu_process_memory_instance() {
        assert_eq!(
            instance_pid("pid_10424_luid_0x00000000_0x0000d1a3_phys_0"),
            Some(10424)
        );
        assert_eq!(instance_pid("_total"), None);
        assert_eq!(instance_pid("pid__luid_0x0_0x0_phys_0"), None);
    }

    /// Per engine, the sum over processes; per type, the busiest engine; another adapter's
    /// instances are left out, and a type keeps its spaces and underscores (S0.1's grammar).
    #[test] // spec: CORE-059, APP-127
    fn busiest_engines_sum_processes_per_engine_and_take_the_busiest_per_type() {
        let luid = luid_tag((0xd1a3, 0));
        assert_eq!(luid, "luid_0x00000000_0x0000d1a3_");
        let ours = "luid_0x00000000_0x0000D1A3_phys_0";
        let other = "luid_0x00000000_0x0000beef_phys_0";
        let found: Vec<(String, f64)> = [
            (format!("pid_10_{ours}_eng_0_engtype_3D"), 30.0),
            (format!("pid_11_{ours}_eng_0_engtype_3D"), 25.04),
            (format!("pid_10_{ours}_eng_1_engtype_3D"), 40.0),
            (format!("pid_12_{ours}_eng_2_engtype_Copy"), 3.0),
            (format!("pid_12_{ours}_eng_3_engtype_Video Codec_0"), 1.0),
            (format!("pid_13_{other}_eng_0_engtype_Compute 0"), 99.0),
            ("_total".to_string(), 100.0),
        ]
        .into_iter()
        .collect();
        let util = |engtype: &str, util_permille: u16| EngineUtil {
            engtype: engtype.to_string(),
            util_permille,
        };
        assert_eq!(
            busiest_engines(&found, &luid),
            [util("3D", 550), util("Copy", 30), util("Video Codec_0", 10)]
        );
        assert_eq!(busiest_engines(&[], &luid), []);
    }

    /// The adapter's figure is its own instances summed, in MiB; an adapter with none is
    /// unknown.
    #[test] // spec: CORE-059
    fn adapter_mib_sums_the_adapters_instances_only() {
        let luid = luid_tag((0xd1a3, 0));
        let mib = |n: i64| n * MIB as i64;
        let found = vec![
            ("luid_0x00000000_0x0000d1a3_phys_0".to_string(), mib(300)),
            ("luid_0x00000000_0x0000d1a3_phys_1".to_string(), mib(12)),
            ("luid_0x00000000_0x0000beef_phys_0".to_string(), mib(5000)),
        ];
        assert_eq!(adapter_mib(&found, &luid), Some(312));
        assert_eq!(adapter_mib(&found[2..], &luid), None);
    }
}
