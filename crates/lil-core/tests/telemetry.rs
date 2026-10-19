//! The `telemetry` module in process (CORE-004, CORE-059–067): sampling rates, `telemetry.db`
//! with exact energy downsampling and retention, counter resets, a lost GPU, process churn,
//! the CPU estimate, the whole-system sum, alerts, the hardware profile, a damaged database and
//! the shapes of Appendix F.
//!
//! Every test drives a manual-mode `Telemetry` over a fake hardware script and a `FakeClock`:
//! one `tick` is one nominal second of samples, and the clock moves only when the test moves
//! it. Nothing waits for the 1 Hz sampler except the one test of the real loop, which polls
//! for its samples against a deadline.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::telemetry::{
    HwSources, JobEnergy, JobEnergyQuery, Measured, PowerSettings, SeriesQuery, Sizes, Snapshot,
    Telemetry, TickMode, format_ts, parse_ts, profile_check,
};
use lil_core::workspace::{WorkspaceHub, WorkspaceRef};
use lil_platform::fs::SystemProbe;
use lil_platform::hw::{
    GpuFeatures, GpuMemSource, GpuSample, GpuSource, ProcGpuMem, SysSample, SysSource,
    ThrottleReasons,
};
use lil_platform::{Clock, IdSource};
use lil_testkit::{FakeClock, FakeHwScript, SeqIds, TestEnv, sha256_file};
use serde_json::{Value, json};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------------------------

/// One minute-aligned instant before the tests start (the `FakeClock`'s UTC origin).
const T0_MS: i64 = 1_800_000_000_000;

struct Rig {
    telemetry: Arc<Telemetry>,
    clock: Arc<FakeClock>,
    events: Arc<EventBus>,
    db: PathBuf,
    _dir: Option<tempfile::TempDir>,
}

impl Rig {
    fn new(script: &Value) -> Rig {
        Rig::with(script, PowerSettings::default())
    }

    fn with(script: &Value, power: PowerSettings) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let sources = HwSources::from_fake(FakeHwScript::new(script.clone()).sources());
        Rig::over(sources, power, dir)
    }

    fn over(sources: HwSources, power: PowerSettings, dir: tempfile::TempDir) -> Rig {
        let db = dir.path().join("telemetry.db");
        Rig::at(sources, power, db, Some(dir))
    }

    /// A telemetry on the database at `db` (which may be another rig's), with a clock of its own.
    fn at(
        sources: HwSources,
        power: PowerSettings,
        db: PathBuf,
        dir: Option<tempfile::TempDir>,
    ) -> Rig {
        let clock = FakeClock::new();
        let events = Arc::new(EventBus::new(clock.clone()));
        let telemetry = Telemetry::start(
            sources,
            db.clone(),
            clock.clone(),
            power,
            events.clone(),
            TickMode::Manual,
        );
        Rig {
            telemetry,
            clock,
            events,
            db,
            _dir: dir,
        }
    }

    /// `n` samples, a second of the clock after each.
    async fn run(&self, n: u32) -> Snapshot {
        let mut last = None;
        for _ in 0..n {
            last = Some(self.telemetry.tick(1).await);
            self.clock.advance(Duration::from_secs(1));
        }
        last.expect("at least one sample")
    }

    async fn series(&self, metric: &str, resolution: &str) -> Vec<(i64, f64, f64, f64)> {
        let q = SeriesQuery {
            metrics: vec![metric.to_string()],
            from: "1970-01-01T00:00:00Z".into(),
            to: "2100-01-01T00:00:00Z".into(),
            resolution: Some(resolution.into()),
        };
        let series = serde_json::to_value(self.telemetry.series(q).await.unwrap()).unwrap();
        series["series"][0]["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    parse_ts(p[0].as_str().unwrap()).unwrap(),
                    p[1].as_f64().unwrap(),
                    p[2].as_f64().unwrap(),
                    p[3].as_f64().unwrap(),
                )
            })
            .collect()
    }

    /// The sum of a metric's average column.
    async fn total(&self, metric: &str, resolution: &str) -> f64 {
        self.series(metric, resolution)
            .await
            .iter()
            .map(|p| p.2)
            .sum()
    }

    /// The `data` of every event on the `telemetry` topic so far, oldest first.
    async fn events(&self) -> Vec<Value> {
        let (tx, mut rx) = mpsc::channel(4096);
        self.events.attach(77, tx);
        let since = BTreeMap::from([("telemetry".to_string(), 0)]);
        let states = self
            .events
            .subscribe(77, &["telemetry".to_string()], &since)
            .await;
        let current = states[0].seq;
        let (mut last, mut seen) = (0, Vec::new());
        while last < current {
            let line = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("the replay arrives")
                .expect("the channel stays open");
            let message: Value = serde_json::from_str(&line).unwrap();
            let params = &message["params"];
            last = params["seq"].as_u64().unwrap();
            if params["kind"] == "event" {
                seen.push(params["data"].clone());
            }
        }
        self.events.drop_client(77);
        seen
    }

    /// The `est` bits of every stored sample, oldest first.
    fn est_bits(&self) -> Vec<i64> {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        let mut stmt = conn.prepare("SELECT est FROM s1 ORDER BY ts").unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

/// A GPU reading.
fn gpu(power_mw: u64, energy_mj: u64) -> Value {
    json!({"name": "Fake GPU", "driver": "999.99", "util_pct": 50, "power_mw": power_mw,
           "energy_mj": energy_mj, "temp_c": 60, "sm_clock_mhz": 1800, "mem_clock_mhz": 9000,
           "throttle": 0, "vram_total_mib": 16384, "vram_used_mib": 2048})
}

/// A system reading at `cpu_permille`, on two cores.
fn sys(cpu_permille: u64) -> Value {
    json!({"cpu_permille": cpu_permille, "per_core_permille": [cpu_permille, cpu_permille],
           "ram_total_mib": 32768, "ram_used_mib": 8192})
}

fn script(gpu: Vec<Value>, sys_entries: Vec<Value>) -> Value {
    json!({"gpu": gpu, "gpu_mem": [[]], "cpu_energy": null, "sys": sys_entries})
}

fn part<'a>(snap: &'a Snapshot, name: &str) -> &'a lil_core::telemetry::SystemPart {
    let want = match name {
        "gpu" => lil_core::telemetry::Part::Gpu,
        "cpu" => lil_core::telemetry::Part::Cpu,
        _ => lil_core::telemetry::Part::Rest,
    };
    snap.system_parts.iter().find(|p| p.part == want).unwrap()
}

fn basis(snap: &Snapshot, name: &str) -> String {
    serde_json::to_value(part(snap, name).measured)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// Rates (CORE-061).
// ---------------------------------------------------------------------------------------------

struct CountingGpu(Arc<AtomicU64>);

impl GpuSource for CountingGpu {
    fn sample(&mut self) -> io::Result<GpuSample> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(GpuSample {
            name: "Counting GPU".into(),
            driver: "1".into(),
            util_pct: 0,
            power_mw: Some(30_000),
            power_instant_mw: None,
            energy_mj: None,
            temp_c: 40,
            sm_clock_mhz: 0,
            mem_clock_mhz: 0,
            fan_pct: None,
            throttle: ThrottleReasons(0),
            vram_total_mib: 100,
            vram_used_mib: 10,
            sm_by_pid: Vec::new(),
        })
    }

    fn features(&self) -> GpuFeatures {
        GpuFeatures::default()
    }
}

struct CountingMem(Arc<AtomicU64>);

impl GpuMemSource for CountingMem {
    fn sample(&mut self) -> io::Result<Vec<ProcGpuMem>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

struct CountingSys(Arc<AtomicU64>, Arc<AtomicU64>);

impl SysSource for CountingSys {
    fn sample(&mut self, _tracked: &[u32], disks: bool) -> SysSample {
        self.0.fetch_add(1, Ordering::SeqCst);
        if disks {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
        SysSample {
            ram_total_mib: 100,
            disks: if disks {
                vec![("C:\\".into(), 500)]
            } else {
                Vec::new()
            },
            ..SysSample::default()
        }
    }
}

/// NVML and `sysinfo` are read every sample; per-process VRAM every 5th while idle and every
/// one while jobs run; the folder sizes every 300th (CORE-061).
#[tokio::test] // spec: CORE-061
async fn rates_follow_core_061() {
    let (gpu, mem, sys_reads, disk_reads) = (
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
    );
    let sources = HwSources {
        gpu: Some(Box::new(CountingGpu(gpu.clone()))),
        gpu_mem: Some(Box::new(CountingMem(mem.clone()))),
        cpu_energy: None,
        sys: Box::new(CountingSys(sys_reads.clone(), disk_reads.clone())),
    };
    let rig = Rig::over(
        sources,
        PowerSettings::default(),
        tempfile::tempdir().unwrap(),
    );
    let probes = Arc::new(AtomicU64::new(0));
    let counted = probes.clone();
    rig.telemetry.set_sizes_probe(Box::new(move || Sizes {
        workspace_mib: Some(counted.fetch_add(1, Ordering::SeqCst) + 1),
        models_mib: None,
    }));
    let reads = || {
        (
            gpu.load(Ordering::SeqCst),
            sys_reads.load(Ordering::SeqCst),
            mem.load(Ordering::SeqCst),
            probes.load(Ordering::SeqCst),
        )
    };

    // 301 idle seconds (0 to 300): per-process memory at 0, 5, … 300; sizes at 0 and 300.
    let first = rig.telemetry.tick(1).await;
    assert_eq!(reads(), (1, 1, 1, 1), "the first sample reads everything");
    assert_eq!(
        disk_reads.load(Ordering::SeqCst),
        1,
        "disks are read at the first sample"
    );
    assert_eq!(first.sizes.workspace_mib, Some(1));
    assert_eq!(first.disks.len(), 1);
    rig.telemetry.tick(299).await;
    assert_eq!(reads(), (300, 300, 60, 1));
    assert_eq!(
        disk_reads.load(Ordering::SeqCst),
        1,
        "not every second (Task 9 review M4)"
    );
    assert_eq!(rig.telemetry.snapshot().sizes.workspace_mib, Some(1));
    let at_300 = rig.telemetry.tick(1).await;
    assert_eq!(reads(), (301, 301, 61, 2));
    assert_eq!(
        disk_reads.load(Ordering::SeqCst),
        2,
        "again after 5 minutes"
    );
    assert_eq!(at_300.sizes.workspace_mib, Some(2));

    // Busy: per-process memory at every sample.
    rig.telemetry.set_busy(true);
    rig.telemetry.tick(10).await;
    assert_eq!(reads(), (311, 311, 71, 2));

    // Idle again: the next read is 5 s after the last one.
    rig.telemetry.set_busy(false);
    rig.telemetry.tick(4).await;
    assert_eq!(reads().2, 71);
    rig.telemetry.tick(1).await;
    assert_eq!(reads().2, 72);
    assert_eq!(disk_reads.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------------------------
// Storage (CORE-062).
// ---------------------------------------------------------------------------------------------

/// Three minutes of samples: the 1-minute and hourly rows have the true min, average and max
/// of each minute and the hour, and their energy is the exact sum of the counter's deltas.
#[tokio::test] // spec: CORE-062
async fn downsampling_keeps_min_avg_max_and_exact_energy() {
    // Power cycles 100, 300, 200 W (so each minute averages 200 W); the counter climbs by an
    // irregular 7i + 13 mJ per second.
    let mut counter = 5_000;
    let readings: Vec<Value> = (0..180)
        .map(|i| {
            if i > 0 {
                counter += 7 * i + 13;
            }
            gpu([100_000, 300_000, 200_000][i as usize % 3], counter)
        })
        .collect();
    let expected = counter - 5_000;
    let rig = Rig::new(&script(readings, vec![sys(100)]));
    rig.run(180).await;

    let seconds = rig.series("gpu.energy", "1s").await;
    assert_eq!(seconds.len(), 180);
    assert_eq!(seconds.iter().map(|p| p.2).sum::<f64>(), expected as f64);

    let minutes = rig.series("gpu.energy", "1m").await;
    assert_eq!(minutes.len(), 3);
    assert_eq!(minutes.iter().map(|p| p.2).sum::<f64>(), expected as f64);
    for p in &minutes {
        assert_eq!(
            (p.1, p.2),
            (p.2, p.3),
            "an energy point's min, avg and max are its sum"
        );
    }
    assert_eq!(rig.total("gpu.energy", "1h").await, expected as f64);

    let power = rig.series("gpu.power", "1m").await;
    assert_eq!(power.len(), 3);
    for (i, p) in power.iter().enumerate() {
        assert_eq!(
            p.0,
            T0_MS + i as i64 * 60_000,
            "a minute row is stamped with its minute"
        );
        assert_eq!((p.1, p.2, p.3), (100_000.0, 200_000.0, 300_000.0));
    }
    let hour = rig.series("gpu.power", "1h").await;
    assert_eq!(hour.len(), 1);
    assert_eq!(
        (hour[0].1, hour[0].2, hour[0].3),
        (100_000.0, 200_000.0, 300_000.0)
    );
}

/// 1-second samples live 24 h, 1-minute rows 30 days, hourly rows forever.
#[tokio::test] // spec: CORE-062
async fn retention_prunes_24h_30d_keeps_hourly() {
    let rig = Rig::new(&script(vec![gpu(100_000, 1000)], vec![sys(100)]));
    rig.telemetry.tick(1).await;
    for res in ["1s", "1m", "1h"] {
        assert_eq!(rig.series("ram.used", res).await.len(), 1, "{res}");
    }

    // 25 hours later the first second is gone, its minute and hour are not.
    rig.clock.advance(Duration::from_secs(25 * 3600));
    rig.telemetry.tick(1).await;
    assert_eq!(
        rig.series("ram.used", "1s").await.len(),
        1,
        "only the new sample"
    );
    assert_eq!(rig.series("ram.used", "1m").await.len(), 2);
    assert_eq!(rig.series("ram.used", "1h").await.len(), 2);

    // 31 days after the start the first minute is gone too; both hours are still there.
    rig.clock.advance(Duration::from_secs(30 * 24 * 3600));
    rig.telemetry.tick(1).await;
    let minutes = rig.series("ram.used", "1m").await;
    assert_eq!(
        minutes.len(),
        2,
        "the first minute (past 30 days) is pruned: {minutes:?}"
    );
    assert!(minutes.iter().all(|p| p.0 > T0_MS + 60_000));
    let hours = rig.series("ram.used", "1h").await;
    assert_eq!(hours.len(), 3, "hourly rows are kept forever");
    assert_eq!(hours[0].0, T0_MS);
    assert_eq!(hours[0].2, 8192.0);
}

// ---------------------------------------------------------------------------------------------
// Energy from counters (CORE-063).
// ---------------------------------------------------------------------------------------------

/// A GPU counter that goes down was reset: that interval's delta is the new value, so the
/// ticks' energy adds up to 1000 + 100 + 200, never negative.
#[tokio::test] // spec: CORE-063
async fn counter_reset_rebaselines_without_negative_energy() {
    let counters = [1000, 2000, 100, 300];
    let rig = Rig::new(&script(
        counters.iter().map(|e| gpu(100_000, *e)).collect(),
        vec![sys(100)],
    ));
    rig.run(4).await;
    let energy = rig.series("gpu.energy", "1s").await;
    assert_eq!(
        energy.iter().map(|p| p.2).collect::<Vec<_>>(),
        [0.0, 1000.0, 100.0, 200.0]
    );
    assert!(
        rig.est_bits().iter().all(|bits| bits & 2 == 0),
        "all measured"
    );

    // The Energy Meter resets the same way: 5e6 → 1e6 is a reset (delta 1e6), then +2e6.
    let raws = [5_000_000_u64, 1_000_000, 3_000_000]
        .map(|raw| json!({"package_energy_pwh": raw, "package_power_mw": 30_000}));
    let meter = json!({"gpu": null, "gpu_mem": null, "sys": [sys(100)], "cpu_energy": raws});
    let rig = Rig::new(&meter);
    rig.run(3).await;
    // 3e6 pWh in all is 10.8 mJ, 10 whole.
    assert_eq!(rig.total("cpu.energy", "1s").await, 10.0);
}

/// The Energy Meter's raw picowatt-hours are accumulated and converted once: 0 → 1e6 → 2e6
/// → 3e6 pWh is 10 mJ, where converting each 3.6 mJ step on its own would give 9.
#[tokio::test] // spec: CORE-059, CORE-063
async fn energy_meter_pwh_accumulates_raw_then_converts() {
    let raws = [0_u64, 1_000_000, 2_000_000, 3_000_000]
        .map(|raw| json!({"package_energy_pwh": raw, "package_power_mw": 30_000}));
    let rig =
        Rig::new(&json!({"gpu": null, "gpu_mem": null, "sys": [sys(100)], "cpu_energy": raws}));
    let snap = rig.run(4).await;
    assert_eq!(rig.total("cpu.energy", "1s").await, 10.0);
    assert!(snap.cpu.measured);
    assert_eq!(snap.cpu.package_power_mw, 30_000);
    assert!(
        rig.est_bits().iter().all(|bits| bits & 8 == 0),
        "measured, not estimated"
    );
}

// ---------------------------------------------------------------------------------------------
// A lost GPU (Review Focus 5).
// ---------------------------------------------------------------------------------------------

/// NVML errors for 3 samples and is then opened again: the gap's rows are estimated from the
/// last power, the first reading after it is a new baseline (its interval estimated too), and
/// then the counter is followed again. The source is opened again only after the third failure.
#[tokio::test] // spec: CORE-059, CORE-063
async fn nvml_lost_mid_run_reopens_and_labels_the_gap() {
    let lost = json!({"error": "NVML: the driver was unloaded"});
    let first = script(
        vec![gpu(100_000, 1000), lost.clone(), lost.clone(), lost],
        vec![sys(100)],
    );
    let rig = Rig::new(&first);
    let reopened = Arc::new(AtomicU64::new(0));
    let opened = reopened.clone();
    rig.telemetry.set_gpu_opener(Box::new(move || {
        opened.fetch_add(1, Ordering::SeqCst);
        // The driver is back: its counter starts again at 5000.
        FakeHwScript::new(script(
            vec![gpu(120_000, 5000), gpu(120_000, 5200)],
            vec![sys(100)],
        ))
        .sources()
        .0
    }));

    let before = rig.run(1).await;
    assert!(before.gpu.as_ref().unwrap().measured);
    assert_eq!(basis(&before, "gpu"), "measured");

    for n in 1..=3 {
        let gap = rig.run(1).await;
        let held = gap.gpu.as_ref().expect("the last reading is kept");
        assert!(
            !held.measured,
            "gap sample {n}: not a reading of the device"
        );
        assert_eq!(basis(&gap, "gpu"), "estimated", "gap sample {n}");
        assert_eq!(part(&gap, "gpu").mw, 100_000, "held at the last power");
        // The whole-system figure keeps the estimate in.
        assert_eq!(gap.system_power_mw, 111_111);
        assert!(
            rig.telemetry.vram().is_none(),
            "no VRAM check while the GPU is lost"
        );
        assert_eq!(
            reopened.load(Ordering::SeqCst),
            0,
            "not reopened within the 3 failures"
        );
    }

    let back = rig.run(1).await;
    assert_eq!(
        reopened.load(Ordering::SeqCst),
        1,
        "reopened once, after the third failure"
    );
    assert!(back.gpu.as_ref().unwrap().measured);
    assert_eq!(basis(&back, "gpu"), "measured");
    assert!(rig.telemetry.vram().is_some());
    let steady = rig.run(1).await;
    assert!(steady.gpu.as_ref().unwrap().measured);
    assert_eq!(reopened.load(Ordering::SeqCst), 1);

    // 1: GPU power and energy measured; 2 to 4: both estimated; 5: power measured, the
    // interval since the gap estimated; 6: counted again.
    assert_eq!(rig.est_bits(), [0, 3, 3, 3, 2, 0]);
    // Energy: 0 (baseline), three held seconds of 100 W, one estimated second at 120 W, +200.
    assert_eq!(
        rig.series("gpu.energy", "1s")
            .await
            .iter()
            .map(|p| p.2)
            .collect::<Vec<_>>(),
        [0.0, 100_000.0, 100_000.0, 100_000.0, 120_000.0, 200.0]
    );
}

/// A GPU that never read at all is unknown, not estimated.
#[tokio::test] // spec: CORE-059
async fn a_gpu_that_never_reads_is_unknown_not_estimated() {
    let rig = Rig::new(&script(
        vec![json!({"error": "NVML: no device"})],
        vec![sys(100)],
    ));
    let snap = rig.run(2).await;
    assert!(snap.gpu.is_none());
    assert_eq!(basis(&snap, "gpu"), "unknown");
    assert_eq!(snap.system_power_mw, 0);
    assert!(rig.telemetry.vram().is_none());
    assert!(rig.series("gpu.power", "1s").await.is_empty());
    assert!(rig.series("gpu.energy", "1s").await.is_empty());
}

/// One GPU field that returns NotSupported (the power reading on some GPUs) must not fail the
/// whole sample: the other fields stay measured, and power is the instantaneous reading, else
/// the energy counter's rate (estimated), else unknown (Task 5 review M4).
#[tokio::test] // spec: CORE-059
async fn a_gpu_without_a_power_reading_still_samples() {
    let no_power = |energy: Option<u64>| {
        let mut g = gpu(0, 0);
        g.as_object_mut().unwrap().shift_remove("power_mw");
        match energy {
            Some(e) => g["energy_mj"] = json!(e),
            None => {
                g.as_object_mut().unwrap().shift_remove("energy_mj");
            }
        }
        g
    };

    // With the energy counter: power is its rate over the second, estimated.
    let rig = Rig::new(&script(
        vec![no_power(Some(1000)), no_power(Some(4000))],
        vec![sys(100)],
    ));
    let first = rig.run(1).await;
    let g = first.gpu.as_ref().expect("the sample stands");
    assert_eq!((g.temp_c, g.util_pct, g.vram_used_mib), (60, 50, 2048));
    assert!(g.measured, "the other fields are measured");
    assert_eq!(
        basis(&first, "gpu"),
        "unknown",
        "no earlier reading to take a rate from"
    );
    let second = rig.run(1).await;
    assert_eq!(part(&second, "gpu").mw, 3000);
    assert_eq!(basis(&second, "gpu"), "estimated");
    assert_eq!(second.system_power_mw, 3333);
    assert_eq!(
        rig.total("gpu.energy", "1s").await,
        3000.0,
        "energy is still counted"
    );
    assert_eq!(rig.est_bits(), [0, 1], "only the power is estimated");

    // The instantaneous field stands in when the averaged one is missing.
    let mut instant = no_power(Some(1000));
    instant["power_instant_mw"] = json!(90_000);
    let rig = Rig::new(&script(vec![instant], vec![sys(100)]));
    let snap = rig.run(1).await;
    assert_eq!(part(&snap, "gpu").mw, 90_000);
    assert_eq!(basis(&snap, "gpu"), "measured");

    // Neither a power nor a counter: the GPU part is unknown, the rest of the sample is not.
    let rig = Rig::new(&script(vec![no_power(None)], vec![sys(100)]));
    let snap = rig.run(2).await;
    assert!(snap.gpu.as_ref().unwrap().measured);
    assert_eq!(snap.gpu.as_ref().unwrap().temp_c, 60);
    assert_eq!(basis(&snap, "gpu"), "unknown");
    assert!(rig.series("gpu.power", "1s").await.is_empty());
    assert_eq!(rig.series("gpu.temp", "1s").await.len(), 2);
}

// ---------------------------------------------------------------------------------------------
// Processes (Review Focus 5).
// ---------------------------------------------------------------------------------------------

/// PDH instances come and go with processes: a process that is no longer listed drops out of
/// the snapshot and the scheduler's view, and its CPU baseline is forgotten, so a new process
/// under the same pid starts from zero.
#[tokio::test] // spec: CORE-059, ARCH-071
async fn pdh_instance_churn_drops_dead_processes() {
    let mem = |procs: &[(u32, u64)]| {
        Value::Array(
            procs
                .iter()
                .map(|(pid, local)| json!({"pid": pid, "local_mib": local, "committed_mib": local * 2}))
                .collect(),
        )
    };
    let procs = |rows: &[(u32, u64, u64)]| {
        let mut s = sys(500);
        s["procs"] = Value::Array(
            rows.iter()
                .map(|(p, cpu, ram)| json!([p, cpu, ram]))
                .collect(),
        );
        s
    };
    let rig = Rig::new(&json!({
        "gpu": [gpu(100_000, 1000)],
        "gpu_mem": [mem(&[(10, 100), (11, 200), (900, 5)]), mem(&[(10, 100)]), mem(&[(10, 100)]),
                    mem(&[(10, 100), (11, 50)])],
        "cpu_energy": null,
        "sys": [procs(&[(10, 1000, 50), (11, 500, 40)]), procs(&[(10, 2000, 50)]),
                procs(&[(10, 3000, 50)]), procs(&[(10, 3000, 50), (11, 100, 10)])],
    }));
    rig.telemetry.track(10, "comfyui");
    rig.telemetry.track(11, "helper");
    rig.telemetry.set_busy(true);

    let pids = |snap: &Snapshot| snap.processes.iter().map(|p| p.pid).collect::<Vec<_>>();
    let one = rig.run(1).await;
    assert_eq!(pids(&one), [10, 11, 900]);
    assert_eq!(
        rig.telemetry.vram().unwrap().local_by_pid,
        Some(BTreeMap::from([(10, 100), (11, 200), (900, 5)]))
    );
    let roles: Vec<Option<&str>> = one.processes.iter().map(|p| p.role.as_deref()).collect();
    assert_eq!(roles, [Some("comfyui"), Some("helper"), None]);

    let two = rig.run(1).await;
    assert_eq!(pids(&two), [10], "11 and 900 are gone");
    assert_eq!(
        rig.telemetry.vram().unwrap().local_by_pid,
        Some(BTreeMap::from([(10, 100)]))
    );
    let engine = &two.processes[0];
    // 1000 ms of CPU in one second on two cores is 500 permille of the machine.
    assert_eq!(engine.cpu_permille, Some(500));
    assert_eq!(
        (engine.ram_mib, engine.vram_local_mib),
        (Some(50), Some(100))
    );

    rig.run(1).await;
    let four = rig.run(1).await;
    assert_eq!(pids(&four), [10, 11]);
    let returned = four.processes.iter().find(|p| p.pid == 11).unwrap();
    assert_eq!(
        returned.cpu_permille,
        Some(0),
        "a new process under an old pid starts from a baseline, not a huge or negative delta"
    );

    rig.telemetry.untrack(11);
    assert_eq!(
        pids(&rig.run(1).await),
        [10, 11],
        "still using the GPU, so still listed"
    );
}

// ---------------------------------------------------------------------------------------------
// Power (CORE-059, CORE-064).
// ---------------------------------------------------------------------------------------------

/// Without the Energy Meter the CPU is utilisation × `cpu_tdp_w`, estimated; with no TDP it is
/// unknown and left out of the whole-system figure.
#[tokio::test] // spec: CORE-059, CORE-064
async fn cpu_power_without_the_meter_uses_cpu_tdp_w_or_is_unknown() {
    let hw = script(vec![gpu(100_000, 1000)], vec![sys(500)]);
    let tdp = PowerSettings {
        cpu_tdp_mw: Some(65_000),
        ..PowerSettings::default()
    };
    let rig = Rig::with(&hw, tdp);
    let snap = rig.run(3).await;
    assert_eq!(part(&snap, "cpu").mw, 32_500);
    assert_eq!(basis(&snap, "cpu"), "estimated");
    assert!(!snap.cpu.measured);
    assert_eq!(snap.cpu.package_power_mw, 32_500);
    assert_eq!(snap.system_power_mw, 147_222, "(100000 + 32500) / 0.9");
    assert_eq!(
        rig.total("cpu.energy", "1s").await,
        65_000.0,
        "two seconds at 32.5 W"
    );
    assert!(
        rig.est_bits().iter().all(|bits| bits & 12 == 12),
        "CPU power and energy are estimated"
    );

    let rig = Rig::new(&hw);
    let snap = rig.run(3).await;
    assert_eq!(basis(&snap, "cpu"), "unknown");
    assert_eq!(part(&snap, "cpu").mw, 0);
    assert_eq!(
        snap.system_power_mw, 111_111,
        "100000 / 0.9: the CPU is left out"
    );
    assert!(
        rig.series("cpu.power", "1s").await.is_empty(),
        "never guessed"
    );
    assert!(rig.series("cpu.energy", "1s").await.is_empty());
    assert_eq!(
        rig.series("cpu.util", "1s").await.len(),
        3,
        "utilisation is still known"
    );
}

/// Whole system = (GPU + CPU package + rest) / PSU efficiency, rounded down.
#[tokio::test] // spec: CORE-064
async fn whole_system_divides_by_psu_efficiency() {
    let hw = json!({"gpu": [gpu(200_000, 1000)], "gpu_mem": null, "sys": [sys(100)],
        "cpu_energy": [{"package_energy_pwh": 0, "package_power_mw": 60_000}]});
    let rig = Rig::new(&hw);
    let snap = rig.run(1).await;
    assert_eq!(snap.system_power_mw, 288_888);
    assert_eq!(
        basis(&snap, "rest"),
        "unknown",
        "0 means not set, and is shown so"
    );
    assert_eq!(rig.series("system.power", "1s").await[0].2, 288_888.0);

    let power = PowerSettings {
        rest_of_system_mw: 40_000,
        psu_efficiency_permille: 800,
        ..PowerSettings::default()
    };
    let snap = Rig::with(&hw, power).run(1).await;
    assert_eq!(
        snap.system_power_mw, 375_000,
        "(200000 + 60000 + 40000) / 0.8"
    );
    assert_eq!(part(&snap, "rest").mw, 40_000);
    assert_eq!(basis(&snap, "rest"), "estimated");
}

/// `[power]` of `suite.toml`: decimal strings, integers or floats; a value that doesn't read
/// keeps its default.
#[test] // spec: CORE-064
fn power_settings_read_the_suite_toml_table() {
    let read =
        |text: &str| PowerSettings::from_toml(&lil_schema::SuiteToml::parse(text).unwrap().power());
    let set = read(
        "[power]\npsu_efficiency = \"0.85\"\nrest_of_system_w = 40\ncpu_tdp_w = \"65.5\"\n\
         electricity = {rate = \"0.25\", currency = \"EUR\"}\n",
    );
    assert_eq!(set.psu_efficiency_permille, 850);
    assert_eq!(set.rest_of_system_mw, 40_000);
    assert_eq!(set.cpu_tdp_mw, Some(65_500));
    let (rate, currency) = set.electricity.unwrap();
    assert_eq!(
        (rate.to_decimal_string(), currency.as_str()),
        ("0.25".into(), "EUR")
    );

    assert_eq!(read(""), PowerSettings::default());
    assert_eq!(PowerSettings::default().psu_efficiency_permille, 900);
    let bad =
        read("[power]\npsu_efficiency = \"1.5\"\ncpu_tdp_w = \"0\"\nrest_of_system_w = \"x\"\n");
    assert_eq!(bad, PowerSettings::default());
}

// ---------------------------------------------------------------------------------------------
// Alerts (CORE-066).
// ---------------------------------------------------------------------------------------------

fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["kind"].as_str().unwrap()).collect()
}

/// 84, 85, 90, 84 °C: one `alert` at the second sample, one `alert_cleared` at the fourth.
#[tokio::test] // spec: CORE-066
async fn alerts_raise_once_and_clear_once() {
    let hot = |temp: u64| {
        let mut g = gpu(100_000, 1000);
        g["temp_c"] = json!(temp);
        g
    };
    let rig = Rig::new(&script([84, 85, 90, 84].map(hot).to_vec(), vec![sys(100)]));
    let one = rig.run(1).await;
    assert!(one.alerts.is_empty());
    let two = rig.run(1).await;
    assert_eq!(two.alerts.len(), 1);
    let alert = &two.alerts[0];
    assert_eq!(
        (alert.id.as_str(), alert.kind.as_str(), alert.value),
        ("gpu_temperature", "temperature", 85)
    );
    assert!(lil_core::telemetry::parse_ts(&alert.since).is_some());
    let three = rig.run(1).await;
    assert_eq!(three.alerts.len(), 1);
    assert_eq!(three.alerts[0].value, 90, "the value follows");
    assert_eq!(
        three.alerts[0].since, alert.since,
        "since is when it was raised"
    );
    let four = rig.run(1).await;
    assert!(four.alerts.is_empty());

    let events = rig.events().await;
    assert_eq!(
        kinds(&events),
        [
            "sample",
            "alert",
            "sample",
            "sample",
            "alert_cleared",
            "sample"
        ],
        "the alert comes just before the sample that raised it"
    );
    assert_eq!(events[1]["alert"]["id"], "gpu_temperature");
    assert_eq!(events[1]["alert"]["value"], 85);
    assert_eq!(events[4]["id"], "gpu_temperature");
    assert_eq!(events[2]["snapshot"]["alerts"][0]["id"], "gpu_temperature");
}

/// Thermal and power-brake throttling raise their own alerts, each once.
#[tokio::test] // spec: CORE-066
async fn throttle_reasons_raise_their_own_alerts() {
    let with = |bits: u64| {
        let mut g = gpu(100_000, 1000);
        g["throttle"] = json!(bits);
        g
    };
    // 0x40 hardware thermal slowdown, 0x80 hardware power brake, 0x4 a software power cap
    // (neither of the two).
    let rig = Rig::new(&script(
        [0x4, 0x40 | 0x80 | 0x4, 0x80, 0].map(with).to_vec(),
        vec![sys(100)],
    ));
    let one = rig.run(1).await;
    assert!(
        one.alerts.is_empty(),
        "a software power cap alone is not an alert"
    );
    assert_eq!(one.gpu.as_ref().unwrap().throttle, ["sw_power_cap"]);
    let two = rig.run(1).await;
    let ids: Vec<&str> = two.alerts.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["gpu_throttle_power_brake", "gpu_throttle_thermal"]);
    assert_eq!(
        two.gpu.as_ref().unwrap().throttle,
        [
            "sw_power_cap",
            "hw_thermal_slowdown",
            "hw_power_brake_slowdown"
        ]
    );
    let three = rig.run(1).await;
    assert_eq!(three.alerts.len(), 1);
    rig.run(1).await;
    let events = rig.events().await;
    let alerts: Vec<(&str, &str)> = events
        .iter()
        .filter(|e| e["kind"] != "sample")
        .map(|e| {
            (
                e["kind"].as_str().unwrap(),
                e["alert"]["id"].as_str().or(e["id"].as_str()).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        alerts,
        [
            ("alert", "gpu_throttle_thermal"),
            ("alert", "gpu_throttle_power_brake"),
            ("alert_cleared", "gpu_throttle_thermal"),
            ("alert_cleared", "gpu_throttle_power_brake"),
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// The snapshot (Appendix F, CORE-060).
// ---------------------------------------------------------------------------------------------

fn keys_with_temp(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let here = format!("{path}.{k}");
                if k.to_ascii_lowercase().contains("temp") {
                    out.push(here.clone());
                }
                keys_with_temp(child, &here, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                keys_with_temp(child, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// The snapshot has the pinned shape, and no CPU temperature anywhere (CORE-060).
#[tokio::test] // spec: CORE-059, CORE-060
async fn no_cpu_temperature_field() {
    let mut full = gpu(120_000, 1000);
    full["fan_pct"] = json!(35);
    let rig = Rig::new(&json!({
        "gpu": [full], "gpu_mem": [[{"pid": 5, "local_mib": 10}]],
        "cpu_energy": [{"package_energy_pwh": 0, "package_power_mw": 25_000}],
        "sys": [{"cpu_permille": 100, "per_core_permille": [100, 100], "ram_total_mib": 32768,
                 "ram_used_mib": 4096, "net": [["Ethernet", 1000, 2000]],
                 "disks": [["C:\\", 123456]]}]}));
    let snap = rig.run(1).await;
    let value = serde_json::to_value(&snap).unwrap();
    let mut temps = Vec::new();
    keys_with_temp(&value, "", &mut temps);
    assert_eq!(temps, [".gpu.temp_c"], "only the GPU has a temperature");

    assert!(
        value["ts"]
            .as_str()
            .is_some_and(|ts| ts.len() == 24 && ts.ends_with('Z'))
    );
    assert_eq!(value["ts"], format_ts(T0_MS));
    let shown = &value["gpu"];
    assert_eq!(shown["name"], "Fake GPU");
    assert_eq!(shown["measured"], true, "a boolean");
    assert_eq!(shown["fan_pct"], 35);
    assert_eq!(shown["vram_total_mib"], 16384);
    assert_eq!(shown["energy_mj"], 1000);
    assert_eq!(value["cpu"]["measured"], true);
    assert_eq!(value["cpu"]["package_power_mw"], 25_000);
    assert_eq!(value["cpu"]["per_core_permille"], json!([100, 100]));
    assert_eq!(value["ram"], json!({"total_mib": 32768, "used_mib": 4096}));
    assert_eq!(
        value["disks"],
        json!([{"volume": "C:\\", "free_mib": 123456}])
    );
    assert_eq!(value["sizes"], json!({}));
    assert_eq!(
        value["net"],
        json!([{"adapter": "Ethernet", "rx_bps": 0, "tx_bps": 0}])
    );
    let parts: Vec<(&str, &str)> = value["system_parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["part"].as_str().unwrap(), p["measured"].as_str().unwrap()))
        .collect();
    assert_eq!(
        parts,
        [
            ("gpu", "measured"),
            ("cpu", "measured"),
            ("rest", "unknown")
        ]
    );
    assert_eq!(value["alerts"], json!([]));
    // Before any sample the snapshot is empty but whole.
    let fresh = Rig::new(&script(vec![gpu(1, 1)], vec![sys(0)]));
    let empty = serde_json::to_value(fresh.telemetry.snapshot()).unwrap();
    assert!(empty.get("gpu").is_none() && empty["processes"] == json!([]));
}

/// Network rates are the adapters' running totals over the second.
#[tokio::test] // spec: CORE-059
async fn network_rates_come_from_the_running_totals() {
    let net = |rx: u64, tx: u64| {
        let mut s = sys(0);
        s["net"] = json!([["Ethernet", rx, tx], ["Wi-Fi", 10, 10]]);
        s
    };
    let rig = Rig::new(&json!({"gpu": null, "gpu_mem": null, "cpu_energy": null,
        "sys": [net(1_000, 500), net(4_000, 900), net(100, 100)]}));
    rig.run(1).await;
    let two = rig.run(1).await;
    assert_eq!(two.net[0].rx_bps, 3_000);
    assert_eq!(two.net[0].tx_bps, 400);
    assert_eq!(two.net[1].rx_bps, 0);
    let three = rig.run(1).await;
    assert_eq!(
        three.net[0].rx_bps, 0,
        "a total that went down was reset: no negative rate"
    );
    assert_eq!(
        rig.series("net.rx", "1s")
            .await
            .iter()
            .map(|p| p.2)
            .collect::<Vec<_>>(),
        [0.0, 3000.0, 0.0]
    );
    assert_eq!(
        rig.series("net.tx", "1s")
            .await
            .iter()
            .map(|p| p.2)
            .collect::<Vec<_>>(),
        [0.0, 400.0, 0.0]
    );
}

// ---------------------------------------------------------------------------------------------
// The series tool's rules (Appendix F).
// ---------------------------------------------------------------------------------------------

#[tokio::test] // spec: CORE-062, CORE-146
async fn series_rejects_what_it_cannot_answer() {
    let rig = Rig::new(&script(vec![gpu(1, 1)], vec![sys(0)]));
    rig.run(2).await;
    let query = |metrics: &[&str], from: &str, to: &str, res: Option<&str>| SeriesQuery {
        metrics: metrics.iter().map(|m| (*m).to_string()).collect(),
        from: from.into(),
        to: to.into(),
        resolution: res.map(str::to_string),
    };
    let (a, b) = ("2027-01-15T08:00:00Z", "2027-01-15T09:00:00Z");
    for bad in [
        query(&[], a, b, None),
        query(&["cpu.temp"], a, b, None),
        query(&["gpu.util"], "yesterday", b, None),
        query(&["gpu.util"], b, a, None),
        query(&["gpu.util"], a, "2027-01-15T09:00:00+02:00", None),
        query(&["gpu.util"], a, b, Some("5s")),
    ] {
        let err = rig.telemetry.series(bad).await.unwrap_err();
        assert_eq!(err.code, "params.invalid", "{}", err.message);
    }
    // Both bounds are included: a range of exactly the first sample's millisecond has it.
    let first = format_ts(T0_MS);
    let got = rig
        .telemetry
        .series(query(&["gpu.util"], &first, &first, Some("1s")))
        .await
        .unwrap();
    assert_eq!(got.series[0].points.len(), 1);
    let none = rig
        .telemetry
        .series(query(
            &["gpu.util"],
            &format_ts(T0_MS + 1),
            &format_ts(T0_MS + 1),
            Some("1s"),
        ))
        .await
        .unwrap();
    assert!(none.series[0].points.is_empty());
    // More than 3,600 points of one metric is params.invalid.
    let long = Rig::new(&script(vec![gpu(1, 1)], vec![sys(0)]));
    long.telemetry.tick(3601).await;
    let err = long
        .telemetry
        .series(query(
            &["gpu.util"],
            "1970-01-01T00:00:00Z",
            "2100-01-01T00:00:00Z",
            Some("1s"),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code, "params.invalid");
    let ok = long
        .telemetry
        .series(query(
            &["gpu.util"],
            "1970-01-01T00:00:00Z",
            "2100-01-01T00:00:00Z",
            Some("1m"),
        ))
        .await
        .unwrap();
    assert_eq!(ok.series[0].points.len(), 1);
}

#[test] // spec: CORE-146
fn timestamps_are_rfc_3339_utc_with_milliseconds() {
    assert_eq!(format_ts(T0_MS), "2027-01-15T08:00:00.000Z");
    assert_eq!(format_ts(T0_MS + 123), "2027-01-15T08:00:00.123Z");
    assert_eq!(format_ts(-1), "1969-12-31T23:59:59.999Z");
    for ms in [0, 1, 999, T0_MS, T0_MS + 86_399_123, 4_107_542_400_000] {
        assert_eq!(parse_ts(&format_ts(ms)), Some(ms), "{ms}");
    }
    assert_eq!(parse_ts("2027-01-15T08:00:00Z"), Some(T0_MS));
    assert_eq!(parse_ts("2027-01-15T08:00:00.5Z"), Some(T0_MS + 500));
    assert_eq!(parse_ts("2027-01-15T08:00:00.123456Z"), Some(T0_MS + 123));
    for bad in [
        "2027-01-15T08:00:00",
        "2027-01-15 08:00:00Z",
        "2027-01-15T08:00:00+00:00",
        "2027-13-15T08:00:00Z",
        "2027-02-30T08:00:00Z",
        "2027-01-15T24:00:00Z",
        "2027-01-15T08:00:00.Z",
        "",
    ] {
        assert_eq!(parse_ts(bad), None, "{bad:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// A damaged telemetry.db (CORE-004).
// ---------------------------------------------------------------------------------------------

/// 4 KiB of 0xAB is not a database: it is renamed to `telemetry.db.damaged-<utc>` with its
/// bytes intact, and a new database answers. Nothing is deleted.
#[tokio::test] // spec: CORE-004, QA-055
async fn damaged_db_is_set_aside_and_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("telemetry.db");
    lil_testkit::files::write_file(&db, &[0xAB_u8; 4096]);
    let damaged = sha256_file(&db);
    let rig = Rig::over(
        HwSources::from_fake(
            FakeHwScript::new(script(vec![gpu(100_000, 1000)], vec![sys(100)])).sources(),
        ),
        PowerSettings::default(),
        dir,
    );
    rig.run(2).await;
    assert_eq!(
        rig.series("gpu.power", "1s").await.len(),
        2,
        "the new database answers"
    );

    let aside = rig
        .db
        .with_file_name("telemetry.db.damaged-20270115T080000Z");
    assert_eq!(sha256_file(&aside), damaged, "the old bytes are kept");
    let names: Vec<String> = std::fs::read_dir(rig.db.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names
            .iter()
            .filter(|n| n.starts_with("telemetry.db.damaged-"))
            .count(),
        1,
        "{names:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Jobs' energy (Appendix F, CORE-065).
// ---------------------------------------------------------------------------------------------

fn job(id: &str, from: i64, to: i64, idle_unknown: bool) -> JobEnergy {
    JobEnergy {
        job_id: id.into(),
        workspace: "loose".into(),
        from: format_ts(from),
        to: format_ts(to),
        gpu_mj_gross: 1_500_000,
        gpu_mj_above_idle: 1_200_000,
        cpu_mj_gross: 75_000,
        cpu_mj_above_idle: 50_000,
        system_mj: 1_900_000,
        gpu_seconds_milli: 7_500,
        measured: Measured {
            gpu: true,
            cpu: false,
        },
        idle_unknown,
    }
}

/// `put_job_energy` and `job_energy` round-trip the Appendix F shape; `idle_unknown` is there
/// only when true; a second write for a job replaces the first.
#[tokio::test] // spec: CORE-065, CORE-146
async fn job_energy_round_trips_the_appendix_f_shape() {
    let rig = Rig::new(&script(vec![gpu(1, 1)], vec![sys(0)]));
    let t = rig.telemetry.clone();
    t.put_job_energy(&job("j_aaaaaaaaaaaaaaaa", T0_MS, T0_MS + 10_000, false))
        .await
        .unwrap();
    t.put_job_energy(&job(
        "j_bbbbbbbbbbbbbbbb",
        T0_MS + 20_000,
        T0_MS + 30_500,
        true,
    ))
    .await
    .unwrap();

    let all = t.job_energy(JobEnergyQuery::default()).await.unwrap();
    assert_eq!(
        all.iter().map(|j| j.job_id.as_str()).collect::<Vec<_>>(),
        ["j_bbbbbbbbbbbbbbbb", "j_aaaaaaaaaaaaaaaa"],
        "newest first"
    );
    let a = serde_json::to_value(&all[1]).unwrap();
    assert_eq!(
        a,
        json!({"job_id": "j_aaaaaaaaaaaaaaaa", "workspace": "loose",
               "from": "2027-01-15T08:00:00.000Z", "to": "2027-01-15T08:00:10.000Z",
               "gpu_mj_gross": 1_500_000, "gpu_mj_above_idle": 1_200_000,
               "cpu_mj_gross": 75_000, "cpu_mj_above_idle": 50_000, "system_mj": 1_900_000,
               "gpu_seconds_milli": 7_500, "measured": {"gpu": true, "cpu": false}}),
        "no idle_unknown key when it is false"
    );
    let b = serde_json::to_value(&all[0]).unwrap();
    assert_eq!(b["idle_unknown"], true);
    assert_eq!(b["to"], "2027-01-15T08:00:30.500Z");

    let by_id = t
        .job_energy(JobEnergyQuery {
            job_ids: Some(vec!["j_aaaaaaaaaaaaaaaa".into()]),
            ..JobEnergyQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(by_id.len(), 1);
    // Jobs that overlap a range.
    let in_range = t
        .job_energy(JobEnergyQuery {
            from: Some(format_ts(T0_MS + 15_000)),
            to: Some(format_ts(T0_MS + 25_000)),
            ..JobEnergyQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(in_range.len(), 1);
    assert_eq!(in_range[0].job_id, "j_bbbbbbbbbbbbbbbb");
    let limited = t
        .job_energy(JobEnergyQuery {
            limit: Some(1),
            ..JobEnergyQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);

    // A second write replaces.
    let mut again = job("j_aaaaaaaaaaaaaaaa", T0_MS, T0_MS + 10_000, true);
    again.gpu_mj_gross = 7;
    t.put_job_energy(&again).await.unwrap();
    let replaced = t
        .job_energy(JobEnergyQuery {
            job_ids: Some(vec!["j_aaaaaaaaaaaaaaaa".into()]),
            ..JobEnergyQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(
        (
            replaced.len(),
            replaced[0].gpu_mj_gross,
            replaced[0].idle_unknown
        ),
        (1, 7, true)
    );

    for bad in [
        JobEnergyQuery {
            from: Some("soon".into()),
            ..JobEnergyQuery::default()
        },
        JobEnergyQuery {
            limit: Some(0),
            ..JobEnergyQuery::default()
        },
        JobEnergyQuery {
            limit: Some(501),
            ..JobEnergyQuery::default()
        },
    ] {
        assert_eq!(t.job_energy(bad).await.unwrap_err().code, "params.invalid");
    }
    let mut broken = job("j_cccccccccccccccc", T0_MS, T0_MS, false);
    broken.from = "never".into();
    assert_eq!(
        t.put_job_energy(&broken).await.unwrap_err().code,
        "params.invalid"
    );
}

/// A GPU script with a start at `lead` seconds of idle samples (`quiet` of them at the given
/// idle powers, the rest at 150 W and 50 % utilisation), then 10 window seconds at 200 W.
fn window_script(lead: usize, quiet: &[(usize, u64)]) -> Value {
    let mut counter = 0;
    let mut readings = Vec::new();
    for i in 0..lead + 10 {
        let in_window = i >= lead;
        let idle = quiet.iter().find(|(at, _)| *at == i);
        let (util, power) = match (in_window, idle) {
            (true, _) => (50, 200_000),
            (false, Some((_, mw))) => (2, *mw),
            (false, None) => (50, 150_000),
        };
        counter += power; // mW for one second is mJ
        let mut g = gpu(power, counter);
        g["util_pct"] = json!(util);
        g["sm_by_pid"] = json!([[200, 750], [100, 250]]);
        readings.push(g);
    }
    let mut busy = sys(500);
    busy["procs"] = json!([[200, 0, 70]]);
    json!({"gpu": readings, "gpu_mem": [[]], "cpu_energy": null, "sys": [busy]})
}

/// `energy_between` over a 10-second window: the GPU energy of its samples, each process's SM
/// share (permille-seconds), the tracked process's CPU time with every other process together
/// under pid 0, and the idle baseline (CORE-065 clarification).
#[tokio::test] // spec: CORE-065
async fn energy_between_reports_totals_shares_and_idle_baselines() {
    // (a) three samples under 5 % in the 60 s before the window, at 40, 42 and 44 W: 42 W.
    let rig = Rig::new(&window_script(
        70,
        &[(10, 40_000), (30, 42_000), (50, 44_000), (5, 90_000)],
    ));
    // The last quiet one is 65 s before the window: it doesn't count.
    rig.telemetry.track(200, "comfyui");
    rig.run(80).await;
    let (from, to) = (Duration::from_secs(69), Duration::from_secs(79));
    let slice = rig.telemetry.energy_between(from, to).await;
    assert_eq!(slice.idle_gpu_mw, Some(42_000));
    assert_eq!(slice.gpu_mj, 2_000_000, "ten seconds at 200 W");
    assert!(slice.gpu_measured);
    assert!(!slice.cpu_measured, "no Energy Meter and no TDP");
    assert_eq!(slice.cpu_mj, 0);
    assert_eq!(slice.sm_by_pid, BTreeMap::from([(100, 2500), (200, 7500)]));
    // 500 ‰ of two cores for 10 s is 10 000 ms; the engine used 0 here, the rest is pid 0's.
    assert_eq!(slice.cpu_ms_by_pid.get(&0), Some(&10_000));
}

#[tokio::test] // spec: CORE-065
async fn idle_baseline_falls_back_to_the_lowest_minute_then_to_unknown() {
    // (b) No sample under 5 % utilisation in the 60 s before the window; 1-minute averages of
    // 80, 55 and 70 W over the hour before it: 55 W.
    let mut readings = Vec::new();
    let mut counter = 0;
    for minute_power in [80_000_u64, 55_000, 70_000] {
        for _ in 0..60 {
            counter += minute_power;
            readings.push(gpu(minute_power, counter));
        }
    }
    for _ in 0..11 {
        counter += 200_000;
        readings.push(gpu(200_000, counter));
    }
    let rig = Rig::new(&script(readings, vec![sys(0)]));
    rig.run(191).await;
    let slice = rig
        .telemetry
        .energy_between(Duration::from_secs(180), Duration::from_secs(190))
        .await;
    assert_eq!(slice.idle_gpu_mw, Some(55_000));
    assert_eq!(slice.gpu_mj, 2_000_000);

    // (c) A fresh database: no baseline at all.
    let fresh = Rig::new(&script(vec![gpu(200_000, 1000)], vec![sys(0)]));
    fresh.run(10).await;
    let none = fresh
        .telemetry
        .energy_between(Duration::from_secs(0), Duration::from_secs(9))
        .await;
    assert_eq!((none.idle_gpu_mw, none.idle_cpu_mw), (None, None));
}

// ---------------------------------------------------------------------------------------------
// The scheduler's hooks.
// ---------------------------------------------------------------------------------------------

/// `on_sample` runs after every sample, `vram` shows the latest GPU reading and the processes'
/// local usage, and with no GPU there is no VRAM view (the scheduler then does no VRAM check).
#[tokio::test] // spec: ARCH-071
async fn vram_and_the_sample_hook_serve_the_scheduler() {
    let hw = json!({"gpu": [gpu(100_000, 1000)], "cpu_energy": null, "sys": [sys(0)],
        "gpu_mem": [[{"pid": 4, "local_mib": 300}, {"pid": 9, "local_mib": 1000}]]});
    let rig = Rig::new(&hw);
    assert!(rig.telemetry.vram().is_none(), "nothing has been read");
    let samples = Arc::new(AtomicU64::new(0));
    let counted = samples.clone();
    rig.telemetry.on_sample(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    rig.run(3).await;
    assert_eq!(samples.load(Ordering::SeqCst), 3);
    assert_eq!(rig.telemetry.sample_count(), 3);
    let vram = rig.telemetry.vram().unwrap();
    assert_eq!((vram.total_mib, vram.nvml_free_mib), (16384, 16384 - 2048));
    assert_eq!(
        vram.local_by_pid,
        Some(BTreeMap::from([(4, 300), (9, 1000)]))
    );

    let no_gpu =
        Rig::new(&json!({"gpu": null, "gpu_mem": null, "cpu_energy": null, "sys": [sys(0)]}));
    no_gpu.run(2).await;
    assert!(no_gpu.telemetry.vram().is_none());
    assert!(!no_gpu.telemetry.has_gpu());
}

/// A real-mode telemetry samples on its own, once a second, and stops when told to. The test
/// runs on tokio's paused clock and the `FakeClock`: the sampler's timer jumps ahead while the
/// test waits, so it takes no wall time (Task 9 review M8).
#[tokio::test(start_paused = true)] // spec: CORE-061
async fn the_real_sampler_runs_on_its_own_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let clock = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let sources = HwSources::from_fake(
        FakeHwScript::new(script(vec![gpu(100_000, 1000)], vec![sys(100)])).sources(),
    );
    let telemetry = Telemetry::start(
        sources,
        dir.path().join("telemetry.db"),
        clock,
        PowerSettings::default(),
        events,
        TickMode::Real,
    );
    let snap = telemetry.wait_for_samples(2).await;
    assert!(snap.gpu.is_some());
    // `tick` belongs to the manual mode: the real sampler takes no extra samples for it.
    let before = telemetry.sample_count();
    telemetry.tick(50).await;
    assert!(telemetry.sample_count() <= before + 1);
    // `shutdown` returns once the sampler task has ended, so the count can't move again.
    telemetry.shutdown().await;
    let after = telemetry.sample_count();
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        telemetry.sample_count(),
        after,
        "no samples after the shutdown"
    );
}

// ---------------------------------------------------------------------------------------------
// The hardware profile (CORE-067).
// ---------------------------------------------------------------------------------------------

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

async fn open_workspace(
    env: &TestEnv,
    ws: &Path,
) -> (Arc<WorkspaceHub>, Arc<lil_core::workspace::OpenWorkspace>) {
    let states = Arc::new(
        StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
    );
    let clock: Arc<dyn Clock> = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states,
        events,
        clock,
        ids,
        FailPoints::none(),
        Arc::new(SystemProbe),
        lil_core::watch::WatchConfig::default(),
    );
    let open = hub.open(&ws_ref(ws)).await.expect("the workspace opens");
    (hub, open)
}

async fn snapshot_of(gpu_name: &str, vram_mib: u64, ram_mib: u64) -> Snapshot {
    let mut g = gpu(100_000, 1000);
    g["name"] = json!(gpu_name);
    g["vram_total_mib"] = json!(vram_mib);
    let mut s = sys(100);
    s["ram_total_mib"] = json!(ram_mib);
    Rig::new(&script(vec![g], vec![s])).telemetry.tick(1).await
}

/// `text` with the line ending of `like`.
fn eol_of(like: &str, text: &str) -> String {
    if like.contains("\r\n") {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// The workspace file's `hardware` key is rewritten, in place and leaving the rest of the file
/// as it was, only when `gpu`, `vram_gb` or `ram_gb` differs (CORE-067).
#[tokio::test(flavor = "multi_thread")] // spec: CORE-067
async fn profile_written_only_on_change_through_a_targeted_yaml_edit() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("hardware");
    env.register_workspace(&ws, true);
    let file = ws.join("lilith.yaml");
    let original = std::fs::read_to_string(&file).unwrap();
    let with_old = format!(
        "{original}{}",
        eol_of(
            &original,
            "\nhardware:\n  gpu: Old GPU\n  vram_gb: 8\n  ram_gb: 16\n  detected: 2026-01-01\n"
        )
    );
    lil_testkit::files::write_file(&file, with_old.as_bytes());
    let (_hub, open) = open_workspace(&env, &ws).await;

    let snap = snapshot_of("Fake RTX 9090", 16_384, 32_768).await;
    assert!(
        profile_check(&open, &snap).await.unwrap(),
        "the profile differs"
    );
    let updated = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        updated,
        format!(
            "{original}{}",
            eol_of(
                &original,
                "\nhardware:\n  gpu: Fake RTX 9090\n  vram_gb: 16\n  ram_gb: 32\n  detected: 2027-01-15\n"
            )
        ),
        "only the hardware block changed"
    );

    let before = sha256_file(&file);
    assert!(
        !profile_check(&open, &snap).await.unwrap(),
        "nothing changed"
    );
    assert_eq!(sha256_file(&file), before, "the file is not rewritten");
    // Sizes within the same gigabyte are not a change.
    let near = snapshot_of("Fake RTX 9090", 16_376, 32_616).await;
    assert!(!profile_check(&open, &near).await.unwrap());

    // Another GPU is a change; so is a RAM upgrade.
    assert!(
        profile_check(&open, &snapshot_of("Other GPU", 16_384, 32_768).await)
            .await
            .unwrap()
    );
    assert!(
        profile_check(&open, &snapshot_of("Other GPU", 16_384, 65_536).await)
            .await
            .unwrap()
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("gpu: Other GPU") && text.contains("ram_gb: 64"),
        "{text}"
    );

    // Before anything is read there is nothing to write.
    let empty = Rig::new(&script(vec![gpu(1, 1)], vec![sys(0)]))
        .telemetry
        .snapshot();
    assert!(!profile_check(&open, &empty).await.unwrap());
}

/// A workspace file with no `hardware` key gets the block appended; a machine without a GPU
/// is written as `none`.
#[tokio::test(flavor = "multi_thread")] // spec: CORE-067
async fn profile_appends_a_missing_hardware_block() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("hardware-new");
    env.register_workspace(&ws, true);
    let file = ws.join("lilith.yaml");
    let original = std::fs::read_to_string(&file).unwrap();
    let (_hub, open) = open_workspace(&env, &ws).await;

    let no_gpu =
        Rig::new(&json!({"gpu": null, "gpu_mem": null, "cpu_energy": null, "sys": [sys(0)]}));
    let snap = no_gpu.run(1).await;
    assert!(profile_check(&open, &snap).await.unwrap());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!(
            "{original}{}",
            eol_of(
                &original,
                "hardware:\n  gpu: none\n  vram_gb: 0\n  ram_gb: 32\n  detected: 2027-01-15\n"
            )
        )
    );
    assert!(!profile_check(&open, &snap).await.unwrap());
}

/// Subscribing to the `telemetry` topic with a cursor the history can't serve gets the current
/// `Snapshot` (ARCH-044; the topic's snapshot provider).
#[tokio::test] // spec: CORE-068
async fn the_topic_snapshot_is_the_snapshot() {
    let rig = Rig::new(&script(vec![gpu(100_000, 1000)], vec![sys(100)]));
    lil_core::telemetry::register_snapshot(&rig.events, &rig.telemetry);
    rig.run(2).await;
    let (tx, mut rx) = mpsc::channel(64);
    rig.events.attach(5, tx);
    // A cursor from an earlier core is beyond the topic's sequence number.
    let since = BTreeMap::from([("telemetry".to_string(), 999)]);
    rig.events
        .subscribe(5, &["telemetry".to_string()], &since)
        .await;
    let line = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the snapshot arrives")
        .unwrap();
    let message: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(message["params"]["kind"], "snapshot");
    assert_eq!(
        message["params"]["data"],
        serde_json::to_value(rig.telemetry.snapshot()).unwrap()
    );
}

/// A window is open at its start: two windows that meet at a sample count it once, and the
/// sample at exactly the start belongs to the window before (Task 9 review M5).
#[tokio::test] // spec: CORE-065
async fn windows_that_meet_count_their_shared_sample_once() {
    let rig = Rig::new(&script(
        (0..30).map(|i| gpu(100_000, i * 100_000)).collect(),
        vec![sys(0)],
    ));
    rig.run(30).await;
    let at = Duration::from_secs;
    let first = rig.telemetry.energy_between(at(0), at(10)).await;
    let second = rig.telemetry.energy_between(at(10), at(20)).await;
    let whole = rig.telemetry.energy_between(at(0), at(20)).await;
    assert_eq!(
        (first.gpu_mj, second.gpu_mj, whole.gpu_mj),
        (1_000_000, 1_000_000, 2_000_000),
        "ten one-second samples each; the one at 10 s is in the first window only"
    );
}

// ---------------------------------------------------------------------------------------------
// Fixes of Task 9's review.
// ---------------------------------------------------------------------------------------------

/// A GPU that answers NotSupported for one field (the NVML source turns that into an unknown
/// field: power `None`, temperature, clocks and throttle reasons 0, no per-process shares) is not
/// a lost GPU: the sample stays the device's own, the VRAM view stays, and no alert follows from
/// a 0 (I1). The same GPU with each field missing in turn.
#[tokio::test] // spec: CORE-059, ARCH-071
async fn a_gpu_missing_any_one_optional_field_is_never_read_as_lost() {
    for missing in [
        "power_mw",
        "temp_c",
        "sm_clock_mhz",
        "mem_clock_mhz",
        "throttle",
        "sm_by_pid",
    ] {
        let mut entry = gpu(100_000, 1000);
        entry["sm_by_pid"] = json!([[7, 500]]);
        entry.as_object_mut().unwrap().shift_remove(missing);
        let rig = Rig::new(&script(vec![entry], vec![sys(100)]));
        let snap = rig.run(3).await;
        let shown = snap
            .gpu
            .as_ref()
            .unwrap_or_else(|| panic!("{missing}: no GPU"));
        assert!(shown.measured, "{missing}: the sample is the device's own");
        assert_eq!(shown.vram_total_mib, 16384, "{missing}");
        assert!(
            rig.telemetry.vram().is_some(),
            "{missing}: the VRAM check stays"
        );
        assert!(snap.alerts.is_empty(), "{missing}: nothing to alert on");
        assert_eq!(rig.series("gpu.util", "1s").await.len(), 3, "{missing}");
        assert_eq!(rig.series("gpu.energy", "1s").await.len(), 3, "{missing}");
    }
}

/// A damaged database's side files are set aside with their bytes: the check reads the file
/// read-only, so the old `-wal` is not checkpointed into it and deleted (I2).
#[tokio::test] // spec: CORE-004
async fn a_damaged_databases_wal_is_set_aside_with_its_bytes() {
    let source = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (db, wal) = (
        dir.path().join("telemetry.db"),
        dir.path().join("telemetry.db-wal"),
    );
    {
        // A database of another version whose newest pages are still in the WAL, as after a
        // crash: copied while its connection is open, with no checkpoint.
        let conn = rusqlite::Connection::open(source.path().join("telemetry.db")).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE t (x); INSERT INTO t VALUES (1), (2), (3); PRAGMA user_version=7;",
        )
        .unwrap();
        let copy = |from: &str, to: &Path| {
            lil_testkit::files::write_file(
                to,
                &std::fs::read(source.path().join(from)).expect("the source file is there"),
            );
        };
        copy("telemetry.db", &db);
        copy("telemetry.db-wal", &wal);
    }
    let (db_sha, wal_sha) = (sha256_file(&db), sha256_file(&wal));
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);

    let rig = Rig::over(
        HwSources::from_fake(FakeHwScript::new(script(vec![gpu(1, 1)], vec![sys(0)])).sources()),
        PowerSettings::default(),
        dir,
    );
    rig.run(2).await;
    assert_eq!(
        rig.series("ram.used", "1s").await.len(),
        2,
        "a new database answers"
    );
    let folder = rig.db.parent().unwrap();
    let kept = |name: &str| folder.join(format!("{name}.damaged-20270115T080000Z"));
    assert_eq!(
        sha256_file(&kept("telemetry.db")),
        db_sha,
        "the old file is as it was"
    );
    assert_eq!(
        sha256_file(&kept("telemetry.db-wal")),
        wal_sha,
        "the old write-ahead log was renamed aside, not consumed"
    );
}

/// A sample that misses the energy counter between two that have it does not count its
/// interval twice: the next delta carries the whole interval, measured (I3).
#[tokio::test] // spec: CORE-063
async fn a_missed_counter_reading_does_not_count_its_interval_twice() {
    let mut missed = gpu(100_000, 0);
    missed.as_object_mut().unwrap().shift_remove("energy_mj");
    let rig = Rig::new(&script(
        vec![gpu(100_000, 0), missed, gpu(100_000, 200_000)],
        vec![sys(0)],
    ));
    rig.run(3).await;
    assert_eq!(rig.total("gpu.energy", "1s").await, 200_000.0);
    assert_eq!(
        rig.series("gpu.energy", "1s").await.len(),
        2,
        "no energy for the missed row"
    );
    assert!(
        rig.est_bits().iter().all(|bits| bits & 2 == 0),
        "nothing was estimated"
    );
}

/// The default resolution never promises more than the point limit, and does not pick a
/// resolution whose rows are gone (M1).
#[tokio::test] // spec: CORE-062
async fn the_default_resolution_fits_the_limit_and_the_retention() {
    let rig = Rig::new(&script(vec![gpu(1, 1)], vec![sys(0)]));
    rig.run(3601).await;
    let range = |from: i64, to: i64| SeriesQuery {
        metrics: vec!["gpu.util".into()],
        from: format_ts(from),
        to: format_ts(to),
        resolution: None,
    };
    // One hour of 1 Hz samples, both ends included, is 3,601 of them: it must not be asked of
    // the 1-second rows.
    let got = rig
        .telemetry
        .series(range(T0_MS, T0_MS + 3_600_000))
        .await
        .expect("the default resolution fits");
    assert_eq!(got.series[0].points.len(), 61, "minute rows");
    let short = rig
        .telemetry
        .series(range(T0_MS, T0_MS + 3_599_000))
        .await
        .unwrap();
    assert_eq!(short.series[0].points.len(), 3600, "one second apart");

    // Two days on, a half-hour range from the first day has no 1-second rows left: minute rows.
    rig.clock.advance(Duration::from_secs(2 * 86_400));
    rig.telemetry.tick(1).await;
    let old = rig
        .telemetry
        .series(range(T0_MS, T0_MS + 1_800_000))
        .await
        .unwrap();
    assert_eq!(
        old.series[0].points.len(),
        31,
        "minute rows, not an empty answer"
    );
}

/// Samples taken at once are stored and published in the order they were taken (M2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // spec: CORE-066
async fn concurrent_samples_are_published_in_order() {
    let rig = Rig::new(&script(vec![gpu(100_000, 1000)], vec![sys(100)]));
    let ticks: Vec<_> = (0..12)
        .map(|_| {
            let t = rig.telemetry.clone();
            tokio::spawn(async move { t.tick(1).await })
        })
        .collect();
    for t in ticks {
        t.await.unwrap();
    }
    let times: Vec<String> = rig
        .events()
        .await
        .iter()
        .filter(|e| e["kind"] == "sample")
        .map(|e| e["snapshot"]["ts"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(times.len(), 12);
    assert!(times.windows(2).all(|w| w[0] < w[1]), "{times:?}");
    assert_eq!(rig.telemetry.snapshot().ts, *times.last().unwrap());
}

/// A lost GPU that stays lost is not opened again every second: the attempts back off, 3, 6, 12,
/// 24 … steps apart (M6).
#[tokio::test] // spec: CORE-059
async fn reopening_a_lost_gpu_backs_off() {
    let lost = json!({"error": "NVML: the driver was unloaded"});
    let rig = Rig::new(&script(vec![gpu(100_000, 1000), lost], vec![sys(100)]));
    let attempts = Arc::new(AtomicU64::new(0));
    let counted = attempts.clone();
    rig.telemetry.set_gpu_opener(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        None
    }));
    rig.run(61).await;
    // At the starts of steps 5, 9, 16, 29 and 54 (3, 7, 14, 27 and 52 failures in).
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
}

/// A database that already holds samples from a run whose clock was ahead does not make this
/// run's samples collide with them (M7).
#[tokio::test] // spec: CORE-062
async fn a_clock_set_back_between_runs_does_not_lose_samples() {
    let hw = script(vec![gpu(100_000, 1000)], vec![sys(100)]);
    let first = Rig::new(&hw);
    first.run(3).await;
    // The second run's clock starts at the same instant as the first run's did.
    let second = Rig::at(
        HwSources::from_fake(FakeHwScript::new(hw).sources()),
        PowerSettings::default(),
        first.db.clone(),
        None,
    );
    second.run(3).await;
    assert_eq!(first.series("ram.used", "1s").await.len(), 6);
}

/// The folder-size probe never blocks the sampler: it answers nothing at first, walks on a
/// thread of its own, and then answers with the size (M9); and the sampler asks again at every
/// step until the first answer, then every 5 minutes.
#[tokio::test] // spec: CORE-059
async fn folder_sizes_are_measured_off_the_sampler_thread() {
    let dir = tempfile::tempdir().unwrap();
    let mib = vec![1_u8; 1024 * 1024];
    lil_testkit::files::write_file(
        &dir.path().join("a.bin"),
        &[mib.clone(), mib.clone()].concat(),
    );
    lil_testkit::files::write_file(&dir.path().join("sub").join("b.bin"), &mib);
    lil_testkit::files::write_file(&dir.path().join("sub").join("small.txt"), b"x");
    let mut probe = lil_core::telemetry::folder_size_probe(dir.path().to_path_buf());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let sizes = loop {
        let sizes = probe();
        if sizes.workspace_mib.is_some() {
            break sizes;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the walk never finished"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!((sizes.workspace_mib, sizes.models_mib), (Some(3), None));

    let rig = Rig::new(&script(vec![gpu(1, 1)], vec![sys(0)]));
    let calls = Arc::new(AtomicU64::new(0));
    let counted = calls.clone();
    rig.telemetry.set_sizes_probe(Box::new(move || {
        match counted.fetch_add(1, Ordering::SeqCst) {
            0 | 1 => Sizes::default(),
            _ => Sizes {
                workspace_mib: Some(7),
                models_mib: None,
            },
        }
    }));
    let snap = rig.run(10).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "asked until it answered, then left alone"
    );
    assert_eq!(snap.sizes.workspace_mib, Some(7));
}

/// A source that opens again but whose reads keep failing is backed off too, not reopened every
/// second (re-review N1): the same 5 attempts in 61 steps as one that cannot be opened.
#[tokio::test] // spec: CORE-059
async fn a_source_that_opens_but_cannot_be_read_is_backed_off() {
    let lost = json!({"error": "NVML: the driver was unloaded"});
    let rig = Rig::new(&script(
        vec![gpu(100_000, 1000), lost.clone()],
        vec![sys(100)],
    ));
    let attempts = Arc::new(AtomicU64::new(0));
    let counted = attempts.clone();
    rig.telemetry.set_gpu_opener(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        FakeHwScript::new(script(vec![lost.clone()], vec![sys(100)]))
            .sources()
            .0
    }));
    rig.run(61).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
}

/// A run that stored sample times far in the future (a wrong clock that was corrected) is not
/// followed: the next run stamps its samples with the clock (re-review N2). A clock only a little
/// behind is still followed (see the test above).
#[tokio::test] // spec: CORE-062
async fn a_forward_clock_jump_does_not_stick_across_restarts() {
    let hw = script(vec![gpu(100_000, 1000)], vec![sys(100)]);
    let first = Rig::new(&hw);
    first.clock.advance(Duration::from_secs(10 * 86_400));
    first.run(1).await;
    let second = Rig::at(
        HwSources::from_fake(FakeHwScript::new(hw).sources()),
        PowerSettings::default(),
        first.db.clone(),
        None,
    );
    let snap = second.run(1).await;
    assert_eq!(
        snap.ts,
        format_ts(T0_MS),
        "the new run is back in real time"
    );
}
