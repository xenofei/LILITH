//! Fake hardware (QA-066): a script of readings that stands in for NVML, PDH and `sysinfo`.
//!
//! The test kit's `FakeHwScript` builds the four sources a sampler takes from a script in
//! process, and a spawned core on a test channel builds them from the file `LIL_TEST_FAKE_HW`
//! names ([`sources_from_file`]). Both use this one reader, so they read a script the same way.
//! Nothing here is used off a test channel.
//!
//! The script is JSON:
//!
//! ```json
//! {"gpu": [GpuSample, …] | null,
//!  "gpu_mem": [[ProcGpuMem, …], …] | null,
//!  "cpu_energy": [CpuEnergySample, …] | null,
//!  "sys": [SysSample, …]}
//! ```
//!
//! Each list holds one entry per reading of that source: the n-th `sample()` returns entry n,
//! and once the list runs out its last entry is repeated. A missing or `null` list means the
//! source is absent (`None`; `sys` is never absent, and a missing `sys` reads as one empty
//! sample). An entry `{"error": "<text>"}` makes that reading fail with that text (a lost
//! source). The objects use the field names of [`crate::hw`]'s types, every field optional (0,
//! empty or `None` when missing; `name` defaults to `"Fake GPU"`, `driver` to `"0.0"`), with
//! these forms for the composite fields: `throttle` is the reason bits as a number, `sm_by_pid`
//! is `[[pid, permille], …]`, `procs` is `[[pid, cpu_ms, ram_mib], …]`, `net` is
//! `[[adapter, rx_bytes, tx_bytes], …]` and `disks` is `[[mount, free_mib], …]`. A GPU entry
//! without `power_mw` is a GPU that doesn't support the power reading.
//! The GPU source's features are what its entries use: `energy_counter` when any has
//! `energy_mj`, `instant_power` with `power_instant_mw`, `process_sm` with a non-empty
//! `sm_by_pid`, `fan` with `fan_pct`. `SysSource::sample` returns only the `procs` rows of the
//! pids it is asked to track, and the `disks` rows only when asked for disks.

use std::io;
use std::path::Path;

use serde_json::Value;

use crate::hw::{
    CpuEnergySample, CpuEnergySource, GpuFeatures, GpuMemSource, GpuSample, GpuSource, ProcGpuMem,
    SysSample, SysSource, ThrottleReasons,
};

/// The GPU, per-process GPU memory, CPU energy and system sources, in that order.
pub type FakeHwSources = (
    Option<Box<dyn GpuSource>>,
    Option<Box<dyn GpuMemSource>>,
    Option<Box<dyn CpuEnergySource>>,
    Box<dyn SysSource>,
);

/// The sources of the script in the file `path`.
///
/// # Errors
/// When the file can't be read or isn't JSON.
///
/// # Panics
/// If a list is neither an array nor `null`, or a number is out of range (a test script that
/// is wrong should stop the test loudly).
pub fn sources_from_file(path: &Path) -> io::Result<FakeHwSources> {
    let text = std::fs::read_to_string(path)?;
    let script: Value = serde_json::from_str(&text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("not JSON: {e}")))?;
    Ok(sources(&script))
}

/// The sources of `script` (module docs).
///
/// # Panics
/// If a list is neither an array nor `null`, or a number is out of range.
pub fn sources(script: &Value) -> FakeHwSources {
    let gpu = list(script, "gpu").map(|entries| {
        let samples: Vec<io::Result<GpuSample>> =
            entries.iter().map(|e| reading(e, gpu_sample)).collect();
        let features = features(&samples);
        Box::new(Script {
            entries: samples,
            next: 0,
            features,
        }) as Box<dyn GpuSource>
    });
    let gpu_mem = list(script, "gpu_mem").map(|entries| {
        Box::new(Script {
            entries: entries.iter().map(|e| reading(e, proc_mems)).collect(),
            next: 0,
            features: (),
        }) as Box<dyn GpuMemSource>
    });
    let cpu_energy = list(script, "cpu_energy").map(|entries| {
        Box::new(Script {
            entries: entries.iter().map(|e| reading(e, cpu_energy)).collect(),
            next: 0,
            features: (),
        }) as Box<dyn CpuEnergySource>
    });
    let sys_entries = list(script, "sys").unwrap_or_default();
    let sys = Box::new(Script {
        entries: sys_entries.iter().map(sys_sample).collect::<Vec<_>>(),
        next: 0,
        features: (),
    }) as Box<dyn SysSource>;
    (gpu, gpu_mem, cpu_energy, sys)
}

fn list(script: &Value, key: &str) -> Option<Vec<Value>> {
    match script.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::Array(entries)) => Some(entries.clone()),
        Some(other) => panic!("fake hardware `{key}` must be a list or null, not {other}"),
    }
}

/// One source's readings, the last one repeated.
struct Script<T, F> {
    entries: Vec<T>,
    next: usize,
    features: F,
}

impl<T: Clone, F> Script<T, F> {
    /// The next entry; `None` for an empty list.
    fn advance(&mut self) -> Option<T> {
        let entry = self
            .entries
            .get(self.next.min(self.entries.len().saturating_sub(1)))?
            .clone();
        self.next += 1;
        Some(entry)
    }
}

fn reading<T>(entry: &Value, parse: fn(&Value) -> T) -> io::Result<T> {
    match entry.get("error").and_then(Value::as_str) {
        Some(text) => Err(io::Error::other(text.to_string())),
        None => Ok(parse(entry)),
    }
}

fn next_reading<T: Clone, F>(script: &mut Script<io::Result<T>, F>) -> io::Result<T> {
    let at = script.next.min(script.entries.len().saturating_sub(1));
    script.next += 1;
    match script.entries.get(at) {
        Some(Ok(v)) => Ok(v.clone()),
        Some(Err(e)) => Err(io::Error::new(e.kind(), e.to_string())),
        None => Err(io::Error::other("the fake hardware script has no entries")),
    }
}

impl GpuSource for Script<io::Result<GpuSample>, GpuFeatures> {
    fn sample(&mut self) -> io::Result<GpuSample> {
        next_reading(self)
    }

    fn features(&self) -> GpuFeatures {
        self.features
    }
}

impl GpuMemSource for Script<io::Result<Vec<ProcGpuMem>>, ()> {
    fn sample(&mut self) -> io::Result<Vec<ProcGpuMem>> {
        next_reading(self)
    }
}

impl CpuEnergySource for Script<io::Result<CpuEnergySample>, ()> {
    fn sample(&mut self) -> io::Result<CpuEnergySample> {
        next_reading(self)
    }
}

impl SysSource for Script<SysSample, ()> {
    fn sample(&mut self, tracked: &[u32], disks: bool) -> SysSample {
        let mut sample = self.advance().unwrap_or_default();
        sample.procs.retain(|(pid, _, _)| tracked.contains(pid));
        if !disks {
            sample.disks.clear();
        }
        sample
    }
}

fn features(samples: &[io::Result<GpuSample>]) -> GpuFeatures {
    let ok = || samples.iter().filter_map(|s| s.as_ref().ok());
    GpuFeatures {
        energy_counter: ok().any(|s| s.energy_mj.is_some()),
        instant_power: ok().any(|s| s.power_instant_mw.is_some()),
        process_sm: ok().any(|s| !s.sm_by_pid.is_empty()),
        fan: ok().any(|s| s.fan_pct.is_some()),
    }
}

fn u64_of(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn opt_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

fn narrow<T: TryFrom<u64>>(n: u64, key: &str) -> T {
    T::try_from(n).unwrap_or_else(|_| panic!("fake hardware `{key}` is out of range: {n}"))
}

fn rows(v: &Value, key: &str) -> Vec<Vec<Value>> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|r| r.as_array().cloned().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

fn cell(row: &[Value], i: usize) -> u64 {
    row.get(i).and_then(Value::as_u64).unwrap_or(0)
}

fn gpu_sample(v: &Value) -> GpuSample {
    let text = |key: &str, default: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_string()
    };
    GpuSample {
        name: text("name", "Fake GPU"),
        driver: text("driver", "0.0"),
        util_pct: narrow(u64_of(v, "util_pct"), "util_pct"),
        power_mw: opt_u64(v, "power_mw").map(|n| narrow(n, "power_mw")),
        power_instant_mw: opt_u64(v, "power_instant_mw").map(|n| narrow(n, "power_instant_mw")),
        energy_mj: opt_u64(v, "energy_mj"),
        temp_c: narrow(u64_of(v, "temp_c"), "temp_c"),
        sm_clock_mhz: narrow(u64_of(v, "sm_clock_mhz"), "sm_clock_mhz"),
        mem_clock_mhz: narrow(u64_of(v, "mem_clock_mhz"), "mem_clock_mhz"),
        fan_pct: opt_u64(v, "fan_pct").map(|n| narrow(n, "fan_pct")),
        throttle: ThrottleReasons(u64_of(v, "throttle")),
        vram_total_mib: u64_of(v, "vram_total_mib"),
        vram_used_mib: u64_of(v, "vram_used_mib"),
        sm_by_pid: rows(v, "sm_by_pid")
            .iter()
            .map(|r| (narrow(cell(r, 0), "pid"), narrow(cell(r, 1), "permille")))
            .collect(),
    }
}

fn proc_mems(v: &Value) -> Vec<ProcGpuMem> {
    v.as_array()
        .map(|procs| {
            procs
                .iter()
                .map(|p| ProcGpuMem {
                    pid: narrow(u64_of(p, "pid"), "pid"),
                    local_mib: u64_of(p, "local_mib"),
                    dedicated_mib: u64_of(p, "dedicated_mib"),
                    shared_mib: u64_of(p, "shared_mib"),
                    committed_mib: u64_of(p, "committed_mib"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn cpu_energy(v: &Value) -> CpuEnergySample {
    CpuEnergySample {
        package_energy_pwh: u64_of(v, "package_energy_pwh"),
        package_power_mw: narrow(u64_of(v, "package_power_mw"), "package_power_mw"),
    }
}

fn sys_sample(v: &Value) -> SysSample {
    let text = |row: &[Value]| {
        row.first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    SysSample {
        cpu_permille: narrow(u64_of(v, "cpu_permille"), "cpu_permille"),
        per_core_permille: v
            .get("per_core_permille")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|n| narrow(n.as_u64().unwrap_or(0), "per_core_permille"))
                    .collect()
            })
            .unwrap_or_default(),
        ram_total_mib: u64_of(v, "ram_total_mib"),
        ram_used_mib: u64_of(v, "ram_used_mib"),
        procs: rows(v, "procs")
            .iter()
            .map(|r| (narrow(cell(r, 0), "pid"), cell(r, 1), cell(r, 2)))
            .collect(),
        net: rows(v, "net")
            .iter()
            .map(|r| (text(r), cell(r, 1), cell(r, 2)))
            .collect(),
        disks: rows(v, "disks")
            .iter()
            .map(|r| (text(r), cell(r, 1)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A GPU entry without `power_mw` is a GPU that doesn't support the power reading; with it,
    /// the reading is there.
    #[test] // spec: QA-066, CORE-059
    fn a_gpu_entry_without_power_mw_has_no_power_reading() {
        let (gpu, ..) = sources(&json!({"gpu": [{"util_pct": 5}, {"power_mw": 7000}]}));
        let mut gpu = gpu.unwrap();
        assert_eq!(gpu.sample().unwrap().power_mw, None);
        assert_eq!(gpu.sample().unwrap().power_mw, Some(7000));
    }

    #[test] // spec: QA-066
    fn a_script_file_is_read_and_one_that_is_not_json_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("hw.json");
        std::fs::write(&good, r#"{"gpu": null, "sys": [{"cpu_permille": 7}]}"#).unwrap();
        let (gpu, _, _, mut sys) = sources_from_file(&good).unwrap();
        assert!(gpu.is_none());
        assert_eq!(sys.sample(&[], true).cpu_permille, 7);

        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        assert_eq!(
            sources_from_file(&bad).err().map(|e| e.kind()),
            Some(io::ErrorKind::InvalidData)
        );
        assert_eq!(
            sources_from_file(&dir.path().join("none.json"))
                .err()
                .map(|e| e.kind()),
            Some(io::ErrorKind::NotFound)
        );
    }
}
