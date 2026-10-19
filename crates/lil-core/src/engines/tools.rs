//! The engine tools (CORE-108, CORE-131; Appendix F "Product tools"), module `engines`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `engines.status` | read | `{}` | `{engines: [EngineStatus]}` |
//! | `engines.start` | write | `{engine: "comfyui"}` | `{engine}` |
//! | `engines.stop`, `engines.restart` | destructive | `{engine: "comfyui"}` | `{engine}`; `engine.attached` for an attached engine |
//! | `engines.comfyui.install_node` | exec | `{id, version}` | `{job}` |
//!
//! The effect classes are static (ARCH-051): stopping or restarting can kill outside jobs, and
//! an install runs third-party code, so an agent's call waits for your confirmation. An engine
//! name goes through [`EngineKind::parse`]: any other name is `engine.unknown`.
//!
//! The server registers them ([`register_engine_tools`]) with a slot it fills once the engines
//! exist: the engines need the jobs, which the dispatcher builds from the registry.

use std::sync::{Arc, OnceLock};

use lil_protocol::{EffectClass, ErrorBody, codes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::comfyui::install::{self, ExactVersion, NodeId};
use super::{ENGINES_MODULE, EngineStatus, Engines, unknown_engine};
use crate::jobs::{EngineKind, JobSummary, Priority};
use crate::tools::{CallCtx, Registry, typed_tool};

/// Where the engine tools find the engines, filled by the server before it serves.
pub type EnginesSlot = Arc<OnceLock<Arc<Engines>>>;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EngineArgs {
    /// `"comfyui"`.
    engine: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct StatusResult {
    engines: Vec<EngineStatus>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct EngineResult {
    engine: EngineStatus,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InstallParams {
    /// The node pack's registry id.
    id: NodeId,
    /// The exact version to install, `major.minor.patch`.
    version: ExactVersion,
}

#[derive(Debug, Serialize, JsonSchema)]
struct InstallResult {
    job: JobSummary,
}

const STATUS_DESCRIPTION: &str = "Returns each engine the core manages (ComfyUI): whether the core \
    started it (owned) or someone else did (attached), its state, URL, process, version, where it \
    listens, prompts from outside the suite, and whether a node install needs a restart.";
const START_DESCRIPTION: &str = "Starts ComfyUI as an owned engine, from [engines.comfyui] in \
    suite.toml, listening on 127.0.0.1 only. A ComfyUI that already runs (started by the suite or \
    by you) is returned as it is.";
const STOP_DESCRIPTION: &str = "Stops the ComfyUI the suite started, with every process it runs; \
    prompts running in it, including ones from outside the suite, are lost. A ComfyUI started \
    elsewhere (attached) is never stopped: that is engine.attached.";
const RESTART_DESCRIPTION: &str = "Restarts the ComfyUI the suite started (after a node install, \
    say); prompts running in it, including ones from outside the suite, are lost. A ComfyUI \
    started elsewhere (attached) is never restarted: that is engine.attached.";
const INSTALL_DESCRIPTION: &str = "Installs a custom-node pack from the ComfyUI registry with \
    comfy-cli, at an exact version (major.minor.patch, never latest), as a job. It runs \
    third-party code, so an agent's call waits for your confirmation. Once it succeeds ComfyUI \
    needs a restart (engines.status shows restart_needed); restarting stops the jobs running \
    in it.";

/// The engines, from the slot.
fn engines_of(slot: &EnginesSlot, ctx: &CallCtx) -> Result<Arc<Engines>, ErrorBody> {
    slot.get().cloned().ok_or_else(|| {
        ErrorBody::new(
            &codes::CORE_BUG,
            "The engines are not running. This is a bug in LILITH.",
            ctx.trace_id.clone(),
        )
    })
}

/// The engine `name` names.
fn engine_of(name: &str, ctx: &CallCtx) -> Result<EngineKind, ErrorBody> {
    EngineKind::parse(name).ok_or_else(|| traced(unknown_engine(name), ctx))
}

fn traced(mut e: ErrorBody, ctx: &CallCtx) -> ErrorBody {
    e.trace_id.clone_from(&ctx.trace_id);
    e
}

/// One of the `{engine}` tools.
#[derive(Debug, Clone, Copy)]
enum Act {
    Start,
    Stop,
    Restart,
}

fn register_act(
    reg: &mut Registry,
    slot: &EnginesSlot,
    name: &'static str,
    description: &'static str,
    effect: EffectClass,
    act: Act,
) {
    let slot = slot.clone();
    reg.register(typed_tool(
        name,
        description,
        effect,
        ENGINES_MODULE,
        move |ctx, args: EngineArgs| {
            let slot = slot.clone();
            Box::pin(async move {
                let engines = engines_of(&slot, ctx)?;
                let e = engine_of(&args.engine, ctx)?;
                let done = match act {
                    Act::Start => engines.start(e).await,
                    Act::Stop => engines.stop(e).await,
                    Act::Restart => engines.restart(e).await,
                };
                let engine = done.map_err(|err| traced(err, ctx))?;
                let summary = format!(
                    "{} is {}",
                    e.wire(),
                    serde_json::to_value(engine.state)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                );
                Ok((EngineResult { engine }, summary))
            })
        },
    ));
}

/// Registers `engines.status`, `engines.start`, `engines.stop`, `engines.restart` and
/// `engines.comfyui.install_node` on `reg`.
pub fn register_engine_tools(reg: &mut Registry, slot: &EnginesSlot) {
    let status_slot = slot.clone();
    reg.register(typed_tool(
        "engines.status",
        STATUS_DESCRIPTION,
        EffectClass::Read,
        ENGINES_MODULE,
        move |ctx, _args: NoArgs| {
            let slot = status_slot.clone();
            Box::pin(async move {
                let engines = engines_of(&slot, ctx)?.status().await;
                let summary = format!("{} engine(s)", engines.len());
                Ok((StatusResult { engines }, summary))
            })
        },
    ));
    register_act(
        reg,
        slot,
        "engines.start",
        START_DESCRIPTION,
        EffectClass::Write,
        Act::Start,
    );
    register_act(
        reg,
        slot,
        "engines.stop",
        STOP_DESCRIPTION,
        EffectClass::Destructive,
        Act::Stop,
    );
    register_act(
        reg,
        slot,
        "engines.restart",
        RESTART_DESCRIPTION,
        EffectClass::Destructive,
        Act::Restart,
    );
    let install_slot = slot.clone();
    reg.register(typed_tool(
        install::INSTALL_TOOL,
        INSTALL_DESCRIPTION,
        EffectClass::Exec,
        ENGINES_MODULE,
        move |ctx, args: InstallParams| {
            let slot = install_slot.clone();
            Box::pin(async move {
                let engines = engines_of(&slot, ctx)?;
                install::install_plan(&engines).map_err(|e| traced(e, ctx))?;
                let caller = &ctx.session.caller;
                let spec = install::job_spec(
                    &args.id,
                    &args.version,
                    &caller.to_string(),
                    Priority::for_caller(caller, false),
                    ctx.project.clone(),
                );
                let job = ctx.jobs.submit(&ctx.job_caller(), spec).await?;
                let summary = format!(
                    "installing {} {} as job {}",
                    args.id.0, args.version.0, job.job_id
                );
                Ok((InstallResult { job }, summary))
            })
        },
    ));
}
