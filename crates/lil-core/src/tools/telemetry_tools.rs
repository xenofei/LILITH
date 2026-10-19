//! The telemetry tools (CORE-146; Appendix F "Product tools"), module `telemetry`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `telemetry.snapshot` | read | `{}` | the [`Snapshot`](crate::telemetry::Snapshot) |
//! | `telemetry.series` | read | `{metrics, from, to, resolution?}` | `{series: [{metric, points: [[ts, min, avg, max]]}]}` |
//!
//! They live in `tools`, not in `telemetry`, so that `telemetry` depends on no `tools` type
//! (CORE-010); `telemetry.jobs` is `meter`'s (plan 3 Task 15) and goes here too:
//! [`Telemetry::job_energy`] is ready for it.

use std::sync::Arc;

use lil_protocol::EffectClass;
use schemars::JsonSchema;
use serde::Deserialize;

use super::registry::{Registry, typed_tool};

use crate::telemetry::{Series, SeriesQuery, Snapshot, TELEMETRY_MODULE, Telemetry};
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

/// Registers `telemetry.snapshot` and `telemetry.series` on `reg`.
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
}
