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
            match device.process_utilization_stats(self.last_seen_us) {
                Ok(samples) => {
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
                // No process used the GPU in the window.
                Err(NvmlError::NotFound) => {}
                Err(e) => return Err(nvml_error(e)),
            }
        }

        Ok(GpuSample {
            name: self.name.clone(),
            driver: self.driver.clone(),
            util_pct: util.gpu.min(100) as u8,
            power_mw: device.power_usage().map_err(nvml_error)?,
            power_instant_mw: if features.instant_power {
                instant_power(&device.field_values_for(&[FieldId(NVML_FI_DEV_POWER_INSTANT)]))
            } else {
                None
            },
            energy_mj: if features.energy_counter {
                device.total_energy_consumption().ok()
            } else {
                None
            },
            temp_c: device
                .temperature(TemperatureSensor::Gpu)
                .map_err(nvml_error)?
                .min(255) as u8,
            sm_clock_mhz: device.clock_info(Clock::SM).map_err(nvml_error)?,
            mem_clock_mhz: device.clock_info(Clock::Memory).map_err(nvml_error)?,
            fan_pct: if features.fan {
                device.fan_speed(0).ok().map(|v| v.min(100) as u8)
            } else {
                None
            },
            throttle: ThrottleReasons(
                device
                    .current_throttle_reasons()
                    .map_err(nvml_error)?
                    .bits(),
            ),
            vram_total_mib: memory.total / MIB,
            vram_used_mib: memory.used / MIB,
            sm_by_pid,
        })
    }

    fn features(&self) -> GpuFeatures {
        self.features
    }
}
