//! The GPU through NVML (CORE-059). `nvml.dll` comes with the NVIDIA driver and is loaded at
//! run time, so builds and tests need no GPU. NVML can't report per-process VRAM on GeForce
//! under WDDM; that comes from PDH (`hw_pdh`).

use std::collections::BTreeMap;
use std::io;

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::enums::device::SampleValue;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::struct_wrappers::device::FieldValueSample;
use nvml_wrapper::structs::device::FieldId;
use nvml_wrapper::sys_exports::field_id::NVML_FI_DEV_POWER_INSTANT;

use crate::hw::{GpuFeatures, GpuSample, GpuSource, ThrottleReasons};

const MIB: u64 = 1024 * 1024;

/// The first NVIDIA GPU, or `None` without `nvml.dll` or a GPU.
pub fn nvml_gpu() -> Option<Box<dyn GpuSource>> {
    let nvml = Nvml::init().ok()?;
    let device = nvml.device_by_index(0).ok()?;
    let name = device.name().ok()?;
    let driver = nvml.sys_driver_version().ok()?;
    let features = GpuFeatures {
        energy_counter: device.total_energy_consumption().is_ok(),
        instant_power: instant_power(
            &device.field_values_for(&[FieldId(NVML_FI_DEV_POWER_INSTANT)]),
        )
        .is_some(),
        // NotFound means no process has used the GPU lately, which is still support.
        process_sm: matches!(
            device.process_utilization_stats(None),
            Ok(_) | Err(NvmlError::NotFound)
        ),
        fan: device.num_fans().is_ok_and(|n| n > 0) && device.fan_speed(0).is_ok(),
    };
    Some(Box::new(NvmlGpu {
        nvml,
        name,
        driver,
        features,
        last_seen_us: None,
    }))
}

/// NVML device 0's name and total memory in bytes: what [`crate::hw::nvidia_adapter_luid`]
/// matches the DXGI adapter by.
pub(crate) fn nvml_identity() -> Option<(String, u64)> {
    let nvml = Nvml::init().ok()?;
    let device = nvml.device_by_index(0).ok()?;
    Some((device.name().ok()?, device.memory_info().ok()?.total))
}

struct NvmlGpu {
    nvml: Nvml,
    name: String,
    driver: String,
    features: GpuFeatures,
    /// The newest per-process sample timestamp seen, so each sample covers only new ones.
    last_seen_us: Option<u64>,
}

fn nvml_error(e: NvmlError) -> io::Error {
    io::Error::other(format!("NVML: {e}"))
}

/// A field the GPU may not have: `NotSupported` is `Ok(None)` (the field is unknown and the
/// rest of the sample stands); any other error is still an error.
fn supported<T>(read: Result<T, NvmlError>) -> Result<Option<T>, NvmlError> {
    match read {
        Ok(v) => Ok(Some(v)),
        Err(NvmlError::NotSupported) => Ok(None),
        Err(e) => Err(e),
    }
}

type FieldValues = Result<Vec<Result<FieldValueSample, NvmlError>>, NvmlError>;

fn instant_power(values: &FieldValues) -> Option<u32> {
    let sample = values.as_ref().ok()?.first()?.as_ref().ok()?;
    match sample.value.as_ref().ok()? {
        SampleValue::U32(v) => Some(*v),
        SampleValue::U64(v) => u32::try_from(*v).ok(),
        SampleValue::I64(v) => u32::try_from(*v).ok(),
        SampleValue::F64(v) => Some(v.round() as u32),
    }
}

impl GpuSource for NvmlGpu {
    fn sample(&mut self) -> io::Result<GpuSample> {
        let device = self.nvml.device_by_index(0).map_err(nvml_error)?;
        let util = device.utilization_rates().map_err(nvml_error)?;
        let memory = device.memory_info().map_err(nvml_error)?;
        let features = self.features;

        let mut sm_by_pid = Vec::new();
        if features.process_sm {
            // No process used the GPU in the window, or the driver won't say: either way this
            // sample has no per-process shares, and the rest of it stands.
            if let Ok(samples) = device.process_utilization_stats(self.last_seen_us) {
                // Average each process's samples over the window.
                let mut per_pid: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
                for s in &samples {
                    let entry = per_pid.entry(s.pid).or_default();
                    entry.0 += u64::from(s.sm_util.min(100));
                    entry.1 += 1;
                    self.last_seen_us = self.last_seen_us.max(Some(s.timestamp));
                }
                sm_by_pid = per_pid
                    .into_iter()
                    .filter(|&(pid, _)| pid != 0)
                    .map(|(pid, (sum, n))| (pid, (sum * 10 / n) as u16))
                    .collect();
            }
        }

        let mut sample = assemble(
            (&self.name, &self.driver),
            Readings {
                util: Ok(util.gpu),
                memory: Ok((memory.total, memory.used)),
                power: device.power_usage(),
                temp: device.temperature(TemperatureSensor::Gpu),
                sm_clock: device.clock_info(Clock::SM),
                mem_clock: device.clock_info(Clock::Memory),
                throttle: device.current_throttle_reasons().map(|r| r.bits()),
            },
        )?;
        if features.instant_power {
            sample.power_instant_mw =
                instant_power(&device.field_values_for(&[FieldId(NVML_FI_DEV_POWER_INSTANT)]));
        }
        if features.energy_counter {
            sample.energy_mj = device.total_energy_consumption().ok();
        }
        if features.fan {
            sample.fan_pct = device.fan_speed(0).ok().map(|v| v.min(100) as u8);
        }
        sample.sm_by_pid = sm_by_pid;
        Ok(sample)
    }

    fn features(&self) -> GpuFeatures {
        self.features
    }
}

/// What one sample read from the device, field by field.
struct Readings {
    util: Result<u32, NvmlError>,
    /// `(total, used)` in bytes.
    memory: Result<(u64, u64), NvmlError>,
    power: Result<u32, NvmlError>,
    temp: Result<u32, NvmlError>,
    sm_clock: Result<u32, NvmlError>,
    mem_clock: Result<u32, NvmlError>,
    throttle: Result<u64, NvmlError>,
}

/// The sample from the readings. GeForce support in NVML is limited (CORE-059): a field the GPU
/// answers `NotSupported` for is unknown, never a reason to fail the sample (and so to read the
/// GPU as lost). Power stays `None`; temperature, clocks and throttle reasons, which Appendix F
/// pins as numbers, read 0 (no alert follows from a 0). Utilisation and memory are the
/// sample's core: if the device can't give them, or any field gives an error other than
/// `NotSupported` (the GPU is gone, say), the sample fails.
fn assemble(identity: (&str, &str), r: Readings) -> io::Result<GpuSample> {
    let util = r.util.map_err(nvml_error)?;
    let (total, used) = r.memory.map_err(nvml_error)?;
    let optional = |read: Result<u32, NvmlError>| supported(read).map_err(nvml_error);
    Ok(GpuSample {
        name: identity.0.to_string(),
        driver: identity.1.to_string(),
        util_pct: util.min(100) as u8,
        power_mw: optional(r.power)?,
        power_instant_mw: None,
        energy_mj: None,
        temp_c: optional(r.temp)?.unwrap_or(0).min(255) as u8,
        sm_clock_mhz: optional(r.sm_clock)?.unwrap_or(0),
        mem_clock_mhz: optional(r.mem_clock)?.unwrap_or(0),
        fan_pct: None,
        throttle: ThrottleReasons(supported(r.throttle).map_err(nvml_error)?.unwrap_or(0)),
        vram_total_mib: total / MIB,
        vram_used_mib: used / MIB,
        sm_by_pid: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One field the GPU doesn't support is unknown; it doesn't fail the sample, and other
    /// errors still do (Task 5 review M4).
    #[test] // spec: CORE-059
    fn a_not_supported_field_is_unknown_not_an_error() {
        assert_eq!(supported::<u32>(Ok(7)).unwrap(), Some(7));
        assert_eq!(
            supported::<u32>(Err(NvmlError::NotSupported)).unwrap(),
            None
        );
        assert!(matches!(
            supported::<u32>(Err(NvmlError::Unknown)),
            Err(NvmlError::Unknown)
        ));
    }

    fn readings() -> Readings {
        Readings {
            util: Ok(42),
            memory: Ok((16 * 1024 * MIB, 2 * 1024 * MIB)),
            power: Ok(150_000),
            temp: Ok(61),
            sm_clock: Ok(1800),
            mem_clock: Ok(9000),
            throttle: Ok(0x40),
        }
    }

    /// A GeForce may answer NotSupported for any one of power, temperature, clocks and throttle
    /// reasons: each in turn leaves the sample whole (so the GPU is never read as lost), with
    /// that field unknown (0, or `None` for power) and the others as read (Task 9 review I1).
    #[test] // spec: CORE-059
    fn any_one_unsupported_optional_field_leaves_the_sample_whole() {
        let id = ("GPU", "1.0");
        let full = assemble(id, readings()).unwrap();
        assert_eq!(
            (
                full.power_mw,
                full.temp_c,
                full.sm_clock_mhz,
                full.mem_clock_mhz,
                full.throttle.0
            ),
            (Some(150_000), 61, 1800, 9000, 0x40)
        );
        assert_eq!(
            (full.util_pct, full.vram_total_mib, full.vram_used_mib),
            (42, 16384, 2048)
        );
        let no = || Err::<u32, _>(NvmlError::NotSupported);
        let no_flags = || Err::<u64, _>(NvmlError::NotSupported);
        let cases: [(Readings, &str); 5] = [
            (
                Readings {
                    power: no(),
                    ..readings()
                },
                "power",
            ),
            (
                Readings {
                    temp: no(),
                    ..readings()
                },
                "temp",
            ),
            (
                Readings {
                    sm_clock: no(),
                    ..readings()
                },
                "sm_clock",
            ),
            (
                Readings {
                    mem_clock: no(),
                    ..readings()
                },
                "mem_clock",
            ),
            (
                Readings {
                    throttle: no_flags(),
                    ..readings()
                },
                "throttle",
            ),
        ];
        for (r, which) in cases {
            let s = assemble(id, r).unwrap_or_else(|e| panic!("{which} unsupported: {e}"));
            assert_eq!((s.util_pct, s.vram_total_mib), (42, 16384), "{which}");
            let unknown = |name: &str| which == name;
            assert_eq!(s.power_mw.is_none(), unknown("power"), "{which}");
            assert_eq!(s.temp_c == 0, unknown("temp"), "{which}");
            assert_eq!(s.sm_clock_mhz == 0, unknown("sm_clock"), "{which}");
            assert_eq!(s.mem_clock_mhz == 0, unknown("mem_clock"), "{which}");
            assert_eq!(s.throttle.0 == 0, unknown("throttle"), "{which}");
        }
    }

    /// What is not tolerated: no utilisation or memory, and any other error in an optional
    /// field (a GPU that is gone is not an unsupported field).
    #[test] // spec: CORE-059
    fn a_missing_core_field_or_a_real_error_still_fails_the_sample() {
        let id = ("GPU", "1.0");
        assert!(
            assemble(
                id,
                Readings {
                    util: Err(NvmlError::NotSupported),
                    ..readings()
                }
            )
            .is_err()
        );
        assert!(
            assemble(
                id,
                Readings {
                    memory: Err(NvmlError::NotSupported),
                    ..readings()
                }
            )
            .is_err()
        );
        assert!(
            assemble(
                id,
                Readings {
                    temp: Err(NvmlError::GpuLost),
                    ..readings()
                }
            )
            .is_err()
        );
        assert!(
            assemble(
                id,
                Readings {
                    throttle: Err(NvmlError::Unknown),
                    ..readings()
                }
            )
            .is_err()
        );
    }
}
