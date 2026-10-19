//! Hardware sources (CORE-059). The `real_*` tests read this machine's NVIDIA GPU, PDH
//! `GPU Process Memory` and the Energy Meter; they are `#[ignore]`d and run only by
//! `lil test --real` (QA-063). They only read.

use std::time::{Duration, Instant};

use lil_platform::hw::{energy_meter, nvidia_adapter_luid, nvml_gpu, pdh_gpu_memory, pwh_to_mj};

#[test] // spec: CORE-059
fn pwh_to_mj_is_exact() {
    // 1 pWh = 3.6e-9 J = 3.6e-6 mJ, so 10^7 pWh = 36 mJ exactly.
    assert_eq!(pwh_to_mj(0), 0);
    assert_eq!(pwh_to_mj(10_000_000), 36);
    // 1 J = 277,777,777.8 pWh; the conversion truncates.
    assert_eq!(pwh_to_mj(277_777_778), 1000);
    assert_eq!(pwh_to_mj(277_777_777), 999);
    // The multiplication happens in 128 bits.
    assert_eq!(pwh_to_mj(u64::MAX), 66_408_278_665_354);
}

#[test]
#[ignore = "real hardware: an NVIDIA GPU and its driver"]
fn real_nvml_reports_energy_and_process_sm() {
    let mut gpu = nvml_gpu().expect("NVML loads and finds a GPU");
    let features = gpu.features();
    assert!(features.energy_counter, "{features:?}");
    assert!(features.process_sm, "{features:?}");
    let first = gpu.sample().unwrap();
    assert!(
        !first.name.is_empty() && !first.driver.is_empty(),
        "{first:?}"
    );
    assert!(first.vram_total_mib > 0 && first.vram_used_mib <= first.vram_total_mib);
    assert!(first.util_pct <= 100);
    let e0 = first.energy_mj.expect("the energy counter reads");
    std::thread::sleep(Duration::from_secs(2));
    let second = gpu.sample().unwrap();
    let e1 = second.energy_mj.expect("the energy counter reads");
    assert!(e1 > e0, "the energy counter moves: {e0} then {e1}");
    for (pid, permille) in &second.sm_by_pid {
        assert!(*pid != 0 && *permille <= 1000, "{:?}", second.sm_by_pid);
    }
}

#[test]
#[ignore = "real hardware: an NVIDIA GPU (PDH GPU Process Memory)"]
fn real_pdh_local_usage_reads_without_admin() {
    let luid = nvidia_adapter_luid().expect("DXGI lists an NVIDIA adapter");
    let mut source = pdh_gpu_memory(Some(luid)).expect("the GPU Process Memory counters open");
    let processes = source.sample().unwrap();
    assert!(
        processes.iter().any(|p| p.local_mib > 0),
        "some process (the desktop compositor at least) uses local memory: {processes:?}"
    );
    for p in &processes {
        assert!(p.pid != 0, "{processes:?}");
    }
}

#[test]
#[ignore = "real hardware: the Energy Meter counter set (RAPL)"]
fn real_energy_meter_power_matches_energy_delta() {
    let mut meter = energy_meter().expect("the Energy Meter counter set is present");
    // `Power` is the average since the previous collection, so the first sample waits a
    // moment after the meter's own first collection.
    std::thread::sleep(Duration::from_secs(1));
    let first = meter.sample().unwrap();
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_secs(5));
    let second = meter.sample().unwrap();
    let elapsed_ms = t0.elapsed().as_millis() as f64;
    assert!(second.package_energy_pwh > first.package_energy_pwh);
    let delta_mj = pwh_to_mj(second.package_energy_pwh - first.package_energy_pwh) as f64;
    let from_energy_mw = delta_mj * 1000.0 / elapsed_ms;
    // The second sample's power covers the same 5 s.
    let reported_mw = f64::from(second.package_power_mw);
    let ratio = from_energy_mw / reported_mw;
    assert!(
        (0.9..=1.1).contains(&ratio),
        "energy delta gives {from_energy_mw:.0} mW, Power reads {reported_mw:.0} mW"
    );
}
