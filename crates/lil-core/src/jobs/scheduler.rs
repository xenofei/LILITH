//! The GPU scheduler (ARCH-071, ARCH-072, CORE-126; Appendix F "The suite-held queue"): the
//! [`Admission`] of the `gpu-heavy` lane.
//!
//! - **Free VRAM.** Before a `gpu-heavy` job starts, [`free_vram_mib`] reads the latest
//!   [`VramView`]: `min(total − Σ others, nvml_free + Σ engine) − 512 MiB`, never below 0, where
//!   `engine` is the PDH `Local Usage` of the job's engine processes ([`EngineControl::pids`])
//!   and `others` that of every other process. `Dedicated Usage` is never read: it overstates
//!   some processes past the card's size (the M0 result in ARCH-071). A job whose
//!   `vram_need_mib` doesn't fit waits in `waiting_for_vram`; the core calls [`Jobs::kick`] on
//!   every telemetry sample, which asks again.
//! - **A need above the card** (`> total − 512 MiB`, ruling P3-R16 as amended) is clamped to
//!   `total − 512 MiB` and never refused for its size. Such a job runs once the card is as
//!   free as it can be (the driver and the desktop always hold some): the `gpu-heavy` lane is
//!   idle (it is only asked about then), every other engine holding VRAM has been unloaded (the
//!   scheduler asks it) or holds none, and its free VRAM has not risen (by more than
//!   [`RISE_MIB`] from one sample to the next) for [`FLAT_SAMPLES`] consecutive samples. The engine then offloads; out of memory is retried.
//! - **Without a check.** On a machine with no GPU source at all ([`Telemetry::has_gpu`] is
//!   `false`, so no NVML) the job runs unchecked, marked `vram_unchecked`. With NVML but no
//!   live reading yet (before the first sample, or while the GPU is lost) it waits for the next
//!   sample. When per-process GPU memory can't be read, the formula can't credit an engine its
//!   own memory: a job of the engine that holds the lease already runs unchecked (it manages
//!   its own models; ruling P3-R17), any other job is checked with no per-process figures.
//! - **The lease.** The `gpu-heavy` lane holds the GPU lease. It passes when the lane's next job
//!   is admitted (asked, with the lane free), not when it starts: a job for another engine
//!   must not wait for memory that only the previous engine's unload frees. When it passes to
//!   a job for another engine, the previous engine is asked to unload ([`EngineControl::unload`],
//!   `/free` for ComfyUI); a job of no engine leaves it where it is.
//! - **Busy.** While jobs run or wait for VRAM, per-process GPU memory is read every second
//!   ([`Telemetry::set_busy`], CORE-061), so each re-check sees fresh figures.
//! - **The watchdog** ([`watchdog_slow`]): a job running more than 3× its expected time since
//!   `execution_start` is flagged `watchdog: "slow"` and never cancelled. The runner keeps the
//!   time its attempt became `executing` (for a ComfyUI job, the engine's `execution_start`)
//!   and checks on every [`Jobs::kick`].
//!
//! [`Jobs::kick`]: super::Jobs::kick

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::{Admission, Admit, EngineKind, JobSpec, Lane};
use crate::telemetry::{Telemetry, VramView};

/// Kept free on top of every job's need (ARCH-071).
pub const VRAM_MARGIN_MIB: u64 = 512;

/// A job whose need is above the card runs once its free VRAM has not risen for this many
/// consecutive samples (ruling P3-R16).
pub const FLAT_SAMPLES: u32 = 3;

/// Free VRAM "rises" (restarting that count) only when it grows by more than this since the
/// previous sample: small drift doesn't.
pub const RISE_MIB: u64 = 64;

/// A job's watchdog flags it once it runs longer than this many times its expected time.
const WATCHDOG_FACTOR: u64 = 3;

/// The VRAM a job of the engine whose processes are `engine_pids` may use (module docs);
/// saturating, so never below 0.
pub fn free_vram_mib(v: &VramView, engine_pids: &[u32]) -> u64 {
    let (engine, others) =
        v.local_by_pid
            .iter()
            .flatten()
            .fold((0u64, 0u64), |(engine, others), (pid, mib)| {
                if engine_pids.contains(pid) {
                    (engine.saturating_add(*mib), others)
                } else {
                    (engine, others.saturating_add(*mib))
                }
            });
    v.total_mib
        .saturating_sub(others)
        .min(v.nvml_free_mib.saturating_add(engine))
        .saturating_sub(VRAM_MARGIN_MIB)
}

/// Whether a job expected to take `expected_secs` is slow after `since_execution_start`: more
/// than 3× (ARCH-072). A job with no expected time (0) is never slow.
pub fn watchdog_slow(expected_secs: u32, since_execution_start: Duration) -> bool {
    expected_secs > 0
        && since_execution_start
            > Duration::from_secs(u64::from(expected_secs).saturating_mul(WATCHDOG_FACTOR))
}

/// What the scheduler needs of the engines (Task 11 implements it; `jobs` never uses
/// `engines`).
pub trait EngineControl: Send + Sync {
    /// The engine's processes (ARCH-064): an owned engine's Job Object, an attached one's
    /// listener and its descendants. Empty when it doesn't run.
    fn pids(&self, e: EngineKind) -> Vec<u32>;
    /// Asks the engine to free its VRAM (`/free` for ComfyUI); best effort.
    fn unload<'a>(&'a self, e: EngineKind) -> BoxFuture<'a, ()>;
}

/// No engines (until Task 11): no processes, nothing to unload.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEngines;

impl EngineControl for NoEngines {
    fn pids(&self, _e: EngineKind) -> Vec<u32> {
        Vec::new()
    }

    fn unload<'a>(&'a self, _e: EngineKind) -> BoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// The engines asked to unload: in flight, and done since they last took the lease.
#[derive(Debug, Default)]
struct Unloads {
    asked: HashSet<EngineKind>,
    done: HashSet<EngineKind>,
}

/// The free VRAM of an oversized job's engine over consecutive samples (ruling P3-R16).
#[derive(Debug, Clone, Copy)]
struct Flat {
    /// The telemetry sample it was read at.
    sample: u64,
    engine: Option<EngineKind>,
    free_mib: u64,
    /// The consecutive samples since it last rose.
    flat: u32,
}

/// The GPU scheduler (module docs).
pub struct GpuScheduler {
    telemetry: Arc<Telemetry>,
    engines: Arc<dyn EngineControl>,
    /// The engine of the job that holds the GPU lease last.
    lease: Mutex<Option<EngineKind>>,
    unloads: Arc<Mutex<Unloads>>,
    flat: Mutex<Option<Flat>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl std::fmt::Debug for GpuScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuScheduler")
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}

impl GpuScheduler {
    pub fn new(t: Arc<Telemetry>, e: Arc<dyn EngineControl>) -> Arc<Self> {
        Arc::new(GpuScheduler {
            telemetry: t,
            engines: e,
            lease: Mutex::new(None),
            unloads: Arc::default(),
            flat: Mutex::new(None),
        })
    }

    /// Asks `e` to unload, unless that is under way already; once it is done, `e` counts as
    /// unloaded until it takes the lease again.
    fn unload(&self, e: EngineKind) {
        if !lock(&self.unloads).asked.insert(e) {
            return;
        }
        let (engines, unloads) = (self.engines.clone(), self.unloads.clone());
        tokio::spawn(async move {
            engines.unload(e).await;
            let mut u = lock(&unloads);
            u.asked.remove(&e);
            u.done.insert(e);
        });
    }

    /// Whether every engine but `engine` holds no VRAM or has been unloaded; the ones that do
    /// hold some are asked to unload (ruling P3-R16).
    fn others_clear(&self, engine: Option<EngineKind>, view: &VramView) -> bool {
        let mut clear = true;
        for e in EngineKind::ALL {
            if Some(e) == engine || lock(&self.unloads).done.contains(&e) {
                continue;
            }
            let pids = self.engines.pids(e);
            let holds = !pids.is_empty()
                && view.local_by_pid.as_ref().is_none_or(|local| {
                    pids.iter()
                        .any(|p| local.get(p).is_some_and(|mib| *mib > 0))
                });
            if holds {
                clear = false;
                self.unload(e);
            }
        }
        clear
    }

    /// The consecutive samples, up to this one, over which the free VRAM `free_mib` of a job
    /// of `engine` has not risen by more than [`RISE_MIB`] a sample. Asked again within a sample, it answers the same; a sample
    /// missed, or another engine, starts the count again.
    fn flat_samples(&self, engine: Option<EngineKind>, free_mib: u64) -> u32 {
        let sample = self.telemetry.sample_count();
        let mut flat = lock(&self.flat);
        match flat.as_mut() {
            Some(f) if f.engine == engine && f.sample == sample => f.flat,
            Some(f) if f.engine == engine && f.sample + 1 == sample => {
                f.flat = if free_mib > f.free_mib.saturating_add(RISE_MIB) {
                    0
                } else {
                    f.flat + 1
                };
                f.free_mib = free_mib;
                f.sample = sample;
                f.flat
            }
            _ => {
                *flat = Some(Flat {
                    sample,
                    engine,
                    free_mib,
                    flat: 0,
                });
                0
            }
        }
    }

    /// The lease passes to a job of `to` (module docs): the previous engine, when another one,
    /// is asked to unload. Whether `to`'s engine held it already.
    ///
    /// Carry (the plan that adds `llm.generate`, spec-oracle's ARCH-071 exception): an
    /// `llm.generate` job must never ask ComfyUI to `/free`, so it needs a per-job "no unload"
    /// exception here.
    fn pass_lease(&self, to: Option<EngineKind>) -> bool {
        let Some(to) = to else {
            return false;
        };
        let previous = lock(&self.lease).replace(to);
        // The engine that takes the lease loads again.
        lock(&self.unloads).done.remove(&to);
        if let Some(previous) = previous
            && previous != to
        {
            tracing::info!(
                from = previous.wire(),
                to = to.wire(),
                "the GPU lease passes to another engine; the previous one is asked to unload"
            );
            self.unload(previous);
        }
        previous == Some(to)
    }
}

impl Admission for GpuScheduler {
    fn admit(&self, spec: &JobSpec) -> Admit {
        if spec.lane != Lane::GpuHeavy {
            return Admit::Go;
        }
        let held = self.pass_lease(spec.engine);
        let need_mib = spec.vram_need_mib.unwrap_or(0);
        let Some(view) = self.telemetry.vram() else {
            if !self.telemetry.has_gpu() {
                return Admit::GoUnchecked;
            }
            // NVML, but no live reading yet: the next sample tells.
            return Admit::WaitForVram {
                need_mib,
                free_mib: 0,
            };
        };
        if view.local_by_pid.is_none() && held {
            return Admit::GoUnchecked;
        }
        let pids = spec
            .engine
            .map(|e| self.engines.pids(e))
            .unwrap_or_default();
        let free_mib = free_vram_mib(&view, &pids);
        let card = view.total_mib.saturating_sub(VRAM_MARGIN_MIB);
        if need_mib > card {
            // As free as it can be (module docs). Both are evaluated: the other engines are
            // asked to unload while the samples are counted.
            let clear = self.others_clear(spec.engine, &view);
            if self.flat_samples(spec.engine, free_mib) >= FLAT_SAMPLES && clear {
                *lock(&self.flat) = None;
                return Admit::Go;
            }
            return Admit::WaitForVram {
                need_mib: card,
                free_mib,
            };
        }
        if need_mib <= free_mib {
            Admit::Go
        } else {
            Admit::WaitForVram { need_mib, free_mib }
        }
    }

    fn busy(&self, busy: bool) {
        self.telemetry.set_busy(busy);
    }
}
