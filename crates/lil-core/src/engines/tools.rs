//! The engine tools (CORE-108, CORE-131; Appendix F "Product tools"), module `engines`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `engines.status` | read | `{}` | `{engines: [EngineStatus]}` |
//! | `engines.start` | write | `{engine: "comfyui"}` | `{engine}` |
//! | `engines.stop`, `engines.restart` | destructive | `{engine: "comfyui"}` | `{engine}`; `engine.attached` for an attached engine |
//! | `engines.comfyui.install_node` | exec | `{id, version}` | `{job}` |
//! | `models.report` | read | `{}` | [`ModelsReport`] (CORE-164) |
//! | `engines.environment` | read | `{}` | [`EnvironmentView`] (CORE-130) |
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

use serde_json::{Value, json};

use super::comfyui::install::{self, ExactVersion, NodeId};
use super::comfyui::{
    EnvironmentView, ModelsReport, PROMPT_KIND, comfy_estimate_key, environment, inventory,
    prompt_spec,
};
use super::{ENGINES_MODULE, EngineStatus, Engines, unknown_engine};
use crate::jobs::{EngineKind, EstimateKey, JobSummary, Priority};
use crate::tools::{CallCtx, Registry, TEST_MODULE, typed_tool, with_estimate_key};

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

/// `{job}`: the result of a tool that submits a job.
#[derive(Debug, Serialize, JsonSchema)]
struct SubmitResult {
    job: JobSummary,
}

const STATUS_DESCRIPTION: &str = "Returns each engine the core manages (ComfyUI): whether the core \
    started it (owned) or someone else did (attached), its state, URL, process, version, where it \
    listens, prompts from outside the suite (suite jobs wait for them), whether a node install \
    needs a restart, how many model files its inventory holds, the launch profile an owned \
    ComfyUI starts with, and whether ComfyUI's asset catalogue is on.";
const REPORT_DESCRIPTION: &str = "Returns ComfyUI's model files from the newest inventory: each \
    file's path, size, SHA-256, model family (from its header) and states: orphan (no \
    registry/models.yaml entry has its hash), duplicate (another file has the same hash) and \
    unknown_family; and the registry entries whose files are missing. Paths and families are \
    untrusted. Nothing is moved or deleted.";
const ENVIRONMENT_DESCRIPTION: &str = "Returns the newest two recorded environments of ComfyUI \
    (system_stats, driver, the revision of ComfyUI and of each custom node), newest first, and \
    what changed between them. The suite records environments; it never restores one.";
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ComfySubmitArgs {
    /// `"tiny"` (the default) or an API-format graph.
    #[serde(default)]
    graph: Option<Value>,
    #[serde(default)]
    priority: Option<Priority>,
    /// The `SaveImage` nodes of the tiny graph (default 1).
    #[serde(default)]
    outputs: Option<u32>,
    #[serde(default)]
    vram_need_mib: Option<u64>,
    #[serde(default)]
    external_side_effects: bool,
}

/// The tiny graph: one `EmptyImage` node and `outputs` `SaveImage` nodes.
fn tiny_graph(outputs: u32) -> Value {
    let mut graph = serde_json::Map::new();
    graph.insert(
        "1".into(),
        json!({"class_type": "EmptyImage",
               "inputs": {"width": 64, "height": 64, "batch_size": 1, "color": 0}}),
    );
    for n in 0..outputs {
        graph.insert(
            (n + 2).to_string(),
            json!({"class_type": "SaveImage",
                   "inputs": {"images": ["1", 0], "filename_prefix": "ComfyUI"}}),
        );
    }
    Value::Object(graph)
}

/// The graph a `test.comfy_submit` call sends: the tiny one (the default, or `"tiny"`), or
/// `graph` itself; `None` for anything else.
fn submit_graph(graph: Option<&Value>, outputs: Option<u32>) -> Option<Value> {
    match graph {
        None => Some(tiny_graph(outputs.unwrap_or(1))),
        Some(Value::String(s)) if s == "tiny" => Some(tiny_graph(outputs.unwrap_or(1))),
        Some(g @ Value::Object(_)) => Some(g.clone()),
        Some(_) => None,
    }
}

/// The expected time of a `test.comfy_submit` job (the watchdog's yardstick).
const TEST_PROMPT_SECS: u32 = 60;

/// Registers the engines' test-channel tool (the server does, on `test-*` channels only):
///
/// | Tool | Effect | Arguments | `structured` |
/// |---|---|---|---|
/// | `test.comfy_submit` | write | `{graph?: "tiny" \| object, priority?, outputs? (default 1), vram_need_mib?, external_side_effects?}` | `{job}`: a job of kind `comfy.prompt` |
///
/// `"tiny"` (the default) is an `EmptyImage` node and `outputs` `SaveImage` nodes; an object
/// is sent as it is. The spec is [`prompt_spec`]'s, in the caller's class unless `priority`
/// says otherwise. Its estimate key is the graph's ([`comfy_estimate_key`]), as the job's.
pub fn register_engine_test_tools(reg: &mut Registry) {
    let tool = typed_tool(
        "test.comfy_submit",
        "Test channels only: submits a comfy.prompt job: the tiny graph (an EmptyImage and \
         `outputs` SaveImage nodes) or `graph`.",
        EffectClass::Write,
        TEST_MODULE,
        |ctx, args: ComfySubmitArgs| {
            Box::pin(async move {
                let Some(graph) = submit_graph(args.graph.as_ref(), args.outputs) else {
                    return Err(ErrorBody::new(
                        &codes::PARAMS_INVALID,
                        "graph is \"tiny\" or an API-format graph object",
                        ctx.trace_id.clone(),
                    ));
                };
                let priority = args
                    .priority
                    .unwrap_or_else(|| Priority::for_caller(&ctx.session.caller, false));
                let mut spec = prompt_spec(
                    json!({ "graph": graph }),
                    priority,
                    TEST_MODULE,
                    ctx.project.clone(),
                    TEST_PROMPT_SECS,
                );
                spec.vram_need_mib = args.vram_need_mib;
                spec.external_side_effects = args.external_side_effects;
                let job = ctx.jobs.submit(&ctx.job_caller(), spec).await?;
                let summary = format!("job {} is {}", job.job_id, job.status.wire());
                Ok((SubmitResult { job }, summary))
            })
        },
    );
    reg.register(with_estimate_key(tool, |args| {
        let outputs = args
            .get("outputs")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        let graph = submit_graph(args.get("graph"), outputs)?;
        Some(EstimateKey {
            kind: PROMPT_KIND,
            key: comfy_estimate_key(&graph),
        })
    }));
}

/// Registers `engines.status`, `engines.start`, `engines.stop`, `engines.restart`,
/// `models.report`, `engines.environment` and `engines.comfyui.install_node` on `reg`.
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
    let report_slot = slot.clone();
    reg.register(typed_tool(
        "models.report",
        REPORT_DESCRIPTION,
        EffectClass::Read,
        ENGINES_MODULE,
        move |ctx, _args: NoArgs| {
            let slot = report_slot.clone();
            Box::pin(async move {
                let engines = engines_of(&slot, ctx)?;
                let report: ModelsReport = inventory::models_report(&engines)
                    .await
                    .map_err(|e| traced(e, ctx))?;
                let summary = format!(
                    "{} model file(s), {} missing",
                    report.models.len(),
                    report.missing.len()
                );
                Ok((report, summary))
            })
        },
    ));
    let environment_slot = slot.clone();
    reg.register(typed_tool(
        "engines.environment",
        ENVIRONMENT_DESCRIPTION,
        EffectClass::Read,
        ENGINES_MODULE,
        move |ctx, _args: NoArgs| {
            let slot = environment_slot.clone();
            Box::pin(async move {
                let engines = engines_of(&slot, ctx)?;
                let view: EnvironmentView = environment::environment(&engines)
                    .await
                    .map_err(|e| traced(e, ctx))?;
                let summary = format!(
                    "{} capture(s), {} change(s)",
                    view.captures.len(),
                    view.changes.len()
                );
                Ok((view, summary))
            })
        },
    ));
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
                Ok((SubmitResult { job }, summary))
            })
        },
    ));
}
