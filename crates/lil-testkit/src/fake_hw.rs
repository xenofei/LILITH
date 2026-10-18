//! Fake hardware (QA-066): a script of readings that stands in for NVML, PDH and `sysinfo`.
//!
//! In process, [`FakeHwScript::sources`] gives the four sources a sampler takes. A spawned core
//! reads the same script from the file `LIL_TEST_FAKE_HW` names ([`FakeHwScript::write`]). The
//! script format and its reader are `lil_platform::hw_fake`, which the core uses too; its
//! module docs describe the JSON.
//!
//! An in-process `FakeComfy` runs in the test process, so its pid is the test's own.

use std::path::Path;

use lil_platform::hw_fake;
use serde_json::Value;

pub use hw_fake::FakeHwSources;

/// A fake hardware script (`lil_platform::hw_fake`).
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
        hw_fake::sources(&self.0)
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
