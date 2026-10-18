//! Fake hardware (QA-066): a script of readings that stands in for NVML, PDH and `sysinfo`.
//!
//! In process, [`FakeHwScript::sources`] gives the four sources a sampler takes. A spawned core
//! reads the same script from the file `LIL_TEST_FAKE_HW` names ([`FakeHwScript::write`]).
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
//! source). The objects use the field names of `lil_platform::hw`'s types, every field
//! optional (0, empty or `None` when missing; `name` defaults to `"Fake GPU"`, `driver` to
//! `"0.0"`), with these forms for the composite fields: `throttle` is the reason bits as a
//! number, `sm_by_pid` is `[[pid, permille], …]`, `procs` is `[[pid, cpu_ms, ram_mib], …]`,
//! `net` is `[[adapter, rx_bytes, tx_bytes], …]` and `disks` is `[[mount, free_mib], …]`.
//! The GPU source's features are what its entries use: `energy_counter` when any has
//! `energy_mj`, `instant_power` with `power_instant_mw`, `process_sm` with a non-empty
//! `sm_by_pid`, `fan` with `fan_pct`. `SysSource::sample` returns only the `procs` rows of the
//! pids it is asked to track.
//!
//! An in-process `FakeComfy` runs in the test process, so its pid is the test's own.

use std::io;
use std::path::Path;

use lil_platform::hw::{
    CpuEnergySample, CpuEnergySource, GpuFeatures, GpuMemSource, GpuSample, GpuSource, ProcGpuMem,
    SysSample, SysSource, ThrottleReasons,
};
use serde_json::Value;

/// The GPU, per-process GPU memory, CPU energy and system sources, in that order.
pub type FakeHwSources = (
    Option<Box<dyn GpuSource>>,
    Option<Box<dyn GpuMemSource>>,
    Option<Box<dyn CpuEnergySource>>,
    Box<dyn SysSource>,
);

/// A fake hardware script (module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct FakeHwScript(pub Value);

impl FakeHwScript {
    pub fn new(script: Value) -> FakeHwScript {
        FakeHwScript(script)
    }

    /// Writes the script to `path`, for `LIL_TEST_FAKE_HW`.
    ///
    /// # Panics
    /// If the file cannot be written.
    pub fn write(&self, path: &Path) {
        crate::files::write_file(path, self.0.to_string().as_bytes());
    }

    /// Reads a script [`FakeHwScript::write`] wrote.
    ///
    /// # Panics
    /// If the file cannot be read or is not JSON.
    pub fn read(path: &Path) -> FakeHwScript {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        FakeHwScript(
            serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display())),
        )
    }

    /// The GPU, per-process GPU memory, CPU energy and system sources, in that order (the
    /// fields of the core's `HwSources`).
    ///
    /// # Panics
    /// If a list is neither an array nor `null`.
    pub fn sources(&self) -> FakeHwSources {
        let gpu = self.list("gpu").map(|entries| {
            let samples: Vec<io::Result<GpuSample>> =
                entries.iter().map(|e| reading(e, gpu_sample)).collect();
            let features = features(&samples);
            Box::new(Script {
                entries: samples,
                next: 0,
                features,
            }) as Box<dyn GpuSource>
        });
        let gpu_mem = self.list("gpu_mem").map(|entries| {
            Box::new(Script {
                entries: entries.iter().map(|e| reading(e, proc_mems)).collect(),
                next: 0,
                features: (),
            }) as Box<dyn GpuMemSource>
        });
        let cpu_energy = self.list("cpu_energy").map(|entries| {
            Box::new(Script {
                entries: entries.iter().map(|e| reading(e, cpu_energy)).collect(),
                next: 0,
                features: (),
            }) as Box<dyn CpuEnergySource>
        });
        let sys_entries = self.list("sys").unwrap_or_default();
        let sys = Box::new(Script {
            entries: sys_entries.iter().map(sys_sample).collect::<Vec<_>>(),
            next: 0,
            features: (),
        }) as Box<dyn SysSource>;
        (gpu, gpu_mem, cpu_energy, sys)
    }

    fn list(&self, key: &str) -> Option<Vec<Value>> {
        match self.0.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Array(entries)) => Some(entries.clone()),
            Some(other) => panic!("fake hardware `{key}` must be a list or null, not {other}"),
        }
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
    fn sample(&mut self, tracked: &[u32]) -> SysSample {
        let mut sample = self.advance().unwrap_or_default();
        sample.procs.retain(|(pid, _, _)| tracked.contains(pid));
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
        power_mw: narrow(u64_of(v, "power_mw"), "power_mw"),
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

    /// Each source reads its own list, repeats the last entry, fails on an `error` entry, and
    /// a `null` list is an absent source.
    #[test] // spec: QA-066
    fn fake_hw_script_repeats_the_last_entry_and_fails_on_error_entries() {
        let script = FakeHwScript::new(json!({
            "gpu": [{"util_pct": 10, "energy_mj": 5}, {"error": "NVML lost"}, {"util_pct": 30}],
            "gpu_mem": [[{"pid": 7, "local_mib": 100}]],
            "cpu_energy": null,
            "sys": [{"cpu_permille": 500, "procs": [[7, 10, 20], [8, 1, 2]]}],
        }));
        let (gpu, gpu_mem, cpu, mut sys) = script.sources();
        let mut gpu = gpu.expect("a GPU");
        assert!(gpu.features().energy_counter && !gpu.features().fan);
        assert_eq!(gpu.sample().unwrap().util_pct, 10);
        assert_eq!(gpu.sample().unwrap_err().to_string(), "NVML lost");
        for _ in 0..3 {
            assert_eq!(gpu.sample().unwrap().util_pct, 30);
        }
        let mut gpu_mem = gpu_mem.expect("per-process GPU memory");
        for _ in 0..2 {
            assert_eq!(gpu_mem.sample().unwrap()[0].local_mib, 100);
        }
        assert!(cpu.is_none());
        let first = sys.sample(&[7]);
        assert_eq!((first.cpu_permille, first.procs), (500, vec![(7, 10, 20)]));
        assert_eq!(sys.sample(&[]).procs, vec![]);
    }
}
