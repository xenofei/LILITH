//! `JobsFeed`: 8 fake jobs at `rate_hz` (10 by default) and 6 telemetry series at 1 Hz.
//!
//! A background thread pre-encodes 12 JPEG frames (512x512) per job, then ticks at `rate_hz`.
//! Each tick is one closure queued onto the GUI thread that emits `frame` once per job; once a
//! second it also emits `telemetry` once per series. The GUI thread only copies bytes into a
//! `QByteArray` and emits; decoding happens in the image provider.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qbytearray.h");
        type QByteArray = cxx_qt_lib::QByteArray;
    }

    extern "RustQt" {
        #[qobject]
        #[qproperty(bool, running)]
        #[qproperty(i32, ticks)]
        type JobsFeed = super::JobsFeedRust;

        /// One preview frame and the job's progress (0..1).
        #[qsignal]
        fn frame(self: Pin<&mut JobsFeed>, job: i32, jpeg: QByteArray, progress: f64);

        /// One telemetry point (series 0..5).
        #[qsignal]
        fn telemetry(self: Pin<&mut JobsFeed>, series: i32, value: f64);

        #[qinvokable]
        fn start(self: Pin<&mut JobsFeed>, rate_hz: i32);

        #[qinvokable]
        fn stop(self: Pin<&mut JobsFeed>);
    }

    impl cxx_qt::Threading for JobsFeed {}
}

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QByteArray;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const JOBS: usize = 8;
pub const SERIES: i32 = 6;
const FRAMES_PER_JOB: usize = 12;
const SIDE: u16 = 512;

#[derive(Default)]
pub struct JobsFeedRust {
    running: bool,
    ticks: i32,
    stop_flag: Option<Arc<AtomicBool>>,
}

impl Drop for JobsFeedRust {
    fn drop(&mut self) {
        if let Some(flag) = &self.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

impl qobject::JobsFeed {
    pub fn start(mut self: Pin<&mut Self>, rate_hz: i32) {
        if *self.running() {
            return;
        }
        let rate = rate_hz.clamp(1, 60) as u64;
        let flag = Arc::new(AtomicBool::new(false));
        self.as_mut().rust_mut().stop_flag = Some(flag.clone());
        self.as_mut().set_running(true);
        let thread = self.qt_thread();
        std::thread::spawn(move || {
            let pool = encode_pool();
            let period = Duration::from_nanos(1_000_000_000 / rate);
            let mut next = Instant::now();
            let mut tick: u64 = 0;
            while !flag.load(Ordering::SeqCst) {
                let frames: Vec<(i32, Vec<u8>, f64)> = (0..JOBS)
                    .map(|job| {
                        let bytes = pool[job][(tick as usize) % FRAMES_PER_JOB].clone();
                        // Each job takes 20 + 4*job seconds, then starts again.
                        let len = (20 + 4 * job as u64) * rate;
                        let progress = (tick % len) as f64 / len as f64;
                        (job as i32, bytes, progress)
                    })
                    .collect();
                let points: Vec<(i32, f64)> = if tick % rate == 0 {
                    let t = (tick / rate) as f64;
                    (0..SERIES).map(|s| (s, telemetry_value(s, t))).collect()
                } else {
                    Vec::new()
                };
                let queued = thread.queue(move |mut feed| {
                    for (job, bytes, progress) in frames {
                        feed.as_mut().frame(job, QByteArray::from(bytes.as_slice()), progress);
                    }
                    for (series, value) in points {
                        feed.as_mut().telemetry(series, value);
                    }
                    let ticks = *feed.ticks() + 1;
                    feed.as_mut().set_ticks(ticks);
                });
                if queued.is_err() {
                    break; // the QObject is gone
                }
                tick += 1;
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now; // fell behind: don't burst to catch up
                }
            }
        });
    }

    pub fn stop(mut self: Pin<&mut Self>) {
        if let Some(flag) = self.as_mut().rust_mut().stop_flag.take() {
            flag.store(true, Ordering::SeqCst);
        }
        self.as_mut().set_running(false);
    }
}

/// A smooth, bounded value per series: GPU %, VRAM GiB, CPU %, RAM GiB, power W, temperature C.
fn telemetry_value(series: i32, t: f64) -> f64 {
    let s = series as f64;
    let wave = (t / (7.0 + 3.0 * s)).sin() * 0.5 + (t / (2.3 + s)).sin() * 0.15;
    let base = [70.0, 9.0, 35.0, 24.0, 250.0, 62.0][series as usize % 6];
    let amp = [25.0, 4.0, 20.0, 6.0, 120.0, 10.0][series as usize % 6];
    base + amp * wave
}

/// 12 frames per job: a job-tinted gradient with a moving band and a frame-dependent block.
fn encode_pool() -> Vec<Vec<Vec<u8>>> {
    let side = SIDE as usize;
    (0..JOBS)
        .map(|job| {
            (0..FRAMES_PER_JOB)
                .map(|f| {
                    let mut rgb = vec![0u8; side * side * 3];
                    let band = (f * side / FRAMES_PER_JOB) as i64;
                    for y in 0..side {
                        for x in 0..side {
                            let i = (y * side + x) * 3;
                            let d = ((x as i64 + y as i64) / 2 - band).abs();
                            let glow = if d < 24 { (24 - d) as u32 * 8 } else { 0 };
                            let r = (x * 255 / side) as u32 / 2 + 30 * job as u32 + glow;
                            let g = (y * 255 / side) as u32 / 2 + glow;
                            let b = ((x ^ y) & 0xff) as u32 / 3 + 120 - 10 * job as u32;
                            rgb[i] = r.min(255) as u8;
                            rgb[i + 1] = g.min(255) as u8;
                            rgb[i + 2] = b.min(255) as u8;
                        }
                    }
                    let mut out = Vec::new();
                    jpeg_encoder::Encoder::new(&mut out, 82)
                        .encode(&rgb, SIDE, SIDE, jpeg_encoder::ColorType::Rgb)
                        .expect("JPEG encode");
                    out
                })
                .collect()
        })
        .collect()
}
