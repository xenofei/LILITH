//! The telemetry tools (CORE-146; Appendix F "Product tools"), module `telemetry`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `telemetry.snapshot` | read | `{}` | the [`Snapshot`](crate::telemetry::Snapshot) |
//! | `telemetry.series` | read | `{metrics, from, to, resolution?}` | `{series: [{metric, points: [[ts, min, avg, max]]}]}` |
//! | `telemetry.jobs` | read | `{job_ids?, from?, to?, limit?}` | `{jobs: [JobEnergy]}` (newest first) |
//!
//! They live in `tools`, not in `telemetry`, so that `telemetry` depends on no `tools` type
//! (CORE-010). `meter` writes the `JobEnergy` rows `telemetry.jobs` reads.

use std::sync::Arc;

use lil_protocol::EffectClass;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::registry::{Registry, typed_tool};

use crate::telemetry::{
    JobEnergy, JobEnergyQuery, Series, SeriesQuery, Snapshot, TELEMETRY_MODULE, Telemetry,
};
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

const SNAPSHOT_DESCRIPTION: &str = "Returns the hardware state now: the GPU (utilisation, power, \
    energy counter, temperature, clocks, fan, throttle reasons, VRAM), the engine processes and \
    any process using the GPU, the CPU, RAM, disks, network, the whole-system power estimate with \
    each part labelled measured, estimated or unknown, and the active alerts. A part that can't \
    be measured is unknown, never guessed. There is no CPU temperature: Windows doesn't expose it \
    without a driver.";

const SERIES_DESCRIPTION: &str = "Returns hardware metrics over time: gpu.util, gpu.power, \
    gpu.energy, gpu.temp, gpu.vram_used, cpu.util, cpu.power, cpu.energy, ram.used, system.power, \
    net.rx, net.tx. `from` and `to` are RFC 3339 UTC times and both are included. `resolution` is \
    1s, 1m or 1h (default: the finest with at most 3,600 points; more than 3,600 points of a \
    metric is params.invalid). Each point is [time, min, average, max]; for gpu.energy and \
    cpu.energy they are all the interval's summed energy in mJ, so the points of a range add up \
    to its energy. 1-second samples are kept 24 hours, 1-minute rows 30 days, hourly rows \
    forever.";

const JOBS_DESCRIPTION: &str = "Returns the energy of finished jobs, newest first: for each,     the job id, its workspace (or loose), from and to (RFC 3339 UTC), GPU and CPU energy gross     and above idle in mJ, the whole-system energy, SM-weighted GPU-seconds (milli), whether the     GPU and CPU parts were measured, and idle_unknown when no idle baseline existed. `job_ids`     picks jobs; `from` and `to` (RFC 3339 UTC) keep jobs that ended at or after `from` and began     at or before `to`; `limit` is 1-500 (default 100).";

#[derive(Debug, Serialize, JsonSchema)]
struct JobsResult {
    jobs: Vec<JobEnergy>,
}

/// Registers `telemetry.snapshot`, `telemetry.series` and `telemetry.jobs` on `reg`.
pub fn register_telemetry_tools(reg: &mut Registry, telemetry: &Arc<Telemetry>) {
    let t = telemetry.clone();
    reg.register(typed_tool(
        "telemetry.snapshot",
        SNAPSHOT_DESCRIPTION,
        EffectClass::Read,
        TELEMETRY_MODULE,
        move |_ctx, _args: NoArgs| {
            let t = t.clone();
            Box::pin(async move {
                let snapshot: Snapshot = t.snapshot();
                let summary = format!("system power {} mW", snapshot.system_power_mw);
                Ok((snapshot, summary))
            })
        },
    ));
    let t = telemetry.clone();
    reg.register(typed_tool(
        "telemetry.series",
        SERIES_DESCRIPTION,
        EffectClass::Read,
        TELEMETRY_MODULE,
        move |ctx, args: SeriesQuery| {
            let t = t.clone();
            Box::pin(async move {
                let series: Series = t.series(args).await.map_err(|mut e| {
                    e.trace_id.clone_from(&ctx.trace_id);
                    e
                })?;
                let points: usize = series.series.iter().map(|s| s.points.len()).sum();
                Ok((series, format!("{points} point(s)")))
            })
        },
    ));
    let t = telemetry.clone();
    reg.register(typed_tool(
        "telemetry.jobs",
        JOBS_DESCRIPTION,
        EffectClass::Read,
        TELEMETRY_MODULE,
        move |ctx, args: JobEnergyQuery| {
            let t = t.clone();
            Box::pin(async move {
                let jobs = t.job_energy(args).await.map_err(|mut e| {
                    e.trace_id.clone_from(&ctx.trace_id);
                    e
                })?;
                let summary = format!("{} job(s)", jobs.len());
                Ok((JobsResult { jobs }, summary))
            })
        },
    ));
}
