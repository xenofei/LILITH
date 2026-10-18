//! The GPU scheduler (ARCH-071, ARCH-072, CORE-126; Appendix F "The suite-held queue"): the
//! [`Admission`] of the `gpu-heavy` lane.
//!
//! - **Free VRAM.** Before a `gpu-heavy` job starts, [`free_vram_mib`] reads the latest
//!   [`VramView`]: `min(total − Σ others, nvml_free + Σ engine) − 512 MiB`, never below 0, where
//!   `engine` is the PDH `Local Usage` of the job's engine processes ([`EngineControl::pids`])
//!   and `others` that of every other process. `Dedicated Usage` is never read: it overstates
//!   some processes past the card's size (the M0 result in ARCH-071). A job whose
//!   `vram_need_mib` doesn't fit waits in `waiting_for_vram`; the core calls [`Jobs::kick`] on
//!   every telemetry sample, which asks again. A need above `total − 512 MiB` counts as
//!   `total − 512 MiB` (ruling P3-R16): such a job waits until the card is as free as it can
//!   be, then runs (the engine offloads; out of memory is retried), and is never refused for
//!   its size.
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

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::{Admission, Admit, EngineKind, JobSpec, Lane};
use crate::telemetry::{Telemetry, VramView};

/// Kept free on top of every job's need (ARCH-071).
pub const VRAM_MARGIN_MIB: u64 = 512;

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

/// The GPU scheduler (module docs).
pub struct GpuScheduler {
    telemetry: Arc<Telemetry>,
    engines: Arc<dyn EngineControl>,
    /// The engine of the job that holds the GPU lease last.
    lease: Mutex<Option<EngineKind>>,
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
        })
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
        let previous = self
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(to);
        if let Some(previous) = previous
            && previous != to
        {
            tracing::info!(
                from = previous.wire(),
                to = to.wire(),
                "the GPU lease passes to another engine; the previous one is asked to unload"
            );
            let engines = self.engines.clone();
            tokio::spawn(async move { engines.unload(previous).await });
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
        let need_mib = need_mib.min(view.total_mib.saturating_sub(VRAM_MARGIN_MIB));
        let pids = spec
            .engine
            .map(|e| self.engines.pids(e))
            .unwrap_or_default();
        let free_mib = free_vram_mib(&view, &pids);
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
