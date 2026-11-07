//! The `tools` module (CORE-014): the tool registry and the dispatcher pipeline every
//! `tools/call` runs through (CORE-015–017), with the stages plan 3 replaces, the Record stage
//! (CORE-020), the product tools (the job tools among them), the live topics' snapshot
//! providers and the test-channel tools.

pub mod asset_read_tools;
pub mod asset_tools;
pub mod desktop_tools;
pub mod dispatcher;
pub mod doctor_tools;
pub mod job_tools;
pub mod project_tools;
pub mod record_stage;
pub mod ref_tools;
pub mod registry;
pub mod services;
pub mod stages;
pub mod telemetry_tools;
pub mod test_tools;
pub mod topics;
pub mod trash_tools;
pub mod watch_tools;
pub mod workspace_tools;

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::sync::{Arc, OnceLock};

pub use asset_read_tools::register_asset_read_tools;
pub use asset_tools::register_asset_tools;
pub use desktop_tools::register_desktop_tools;
pub use dispatcher::{Dispatcher, ErrorSink, Redispatch};
pub use doctor_tools::{DoctorArgs, DoctorCheck, register_doctor_tools};
pub use job_tools::{JOB_RETRY, register_job_tools};
pub use project_tools::register_project_tools;
pub use record_stage::WorkspaceRecord;
pub use ref_tools::register_ref_tools;
pub use registry::{
    CallCtx, ConfirmPolicy, FactsFn, FactsFuture, Registry, Tool, ToolFuture, ToolMeta, TouchDecl,
    facts_fn, fill_touches, typed_tool, with_estimate_key,
};
pub use services::{AssetServices, AssetUse, InUse, ResourceIds, ThumbState, Thumbnails};
pub use stages::{
    AllowOnce, Approval, BillingBasis, CapHit, CascadeSettings, ConfirmDecision, ConfirmInput,
    ConfirmStage, ConfirmedVia, Estimate, EstimateStage, FACTS_SHOWN, IdemRef, MeterStage,
    PipelineHooks, RecordStage, ReplayDecision, ReserveInput, ReserveOutcome, ReserveStage,
    SealedFacts, SettingSource, Settings, SettingsInput, SettingsStage, SettleInput, ToolEstimates,
    cap_exceeded, cap_hits_json, seal_facts,
};
pub use telemetry_tools::register_telemetry_tools;
pub use test_tools::{
    TEST_JOB_KIND, TEST_MODULE, TestGates, TestJobHandler, register_hw_tick, register_test_desktop,
    register_test_tools,
};
pub use topics::{AssetSnapshots, ProjectSnapshots, register_snapshots};
pub use trash_tools::register_trash_tools;
pub use watch_tools::register_watch_tools;
pub use workspace_tools::{WORKSPACE_MODULE, register_workspace_tools};

use lil_platform::Env;
use lil_platform::fs::Probe;

use crate::state::StateRegistry;
use crate::store::Personal;
use crate::workspace::WorkspaceHub;

/// The registry a core serves: the product tools on every channel and, on a test channel
/// (`test_tools`), the test-channel tools. `git_env` is added to every `git` the doctor runs
/// (`CoreConfig.git_env`). `services` are the asset services' late-bound slots (P4b-R32), which
/// the asset tools keep.
#[allow(
    clippy::too_many_arguments,
    reason = "the plan's signature: today's arguments, then the asset services"
)]
pub fn core_registry(
    test_tools: bool,
    gates: &Arc<TestGates>,
    states: &Arc<StateRegistry>,
    hub: &Arc<WorkspaceHub>,
    env: &Env,
    probe: &Arc<dyn Probe>,
    git_env: Arc<Vec<(OsString, OsString)>>,
    services: &AssetServices,
) -> Registry {
    let mut reg = Registry::new();
    register_workspace_tools(&mut reg, hub.clone(), env.clone(), probe.clone());
    register_doctor_tools(&mut reg, probe.clone(), git_env);
    let personal = Arc::new(Personal::new(
        env.data_dir().join("resources"),
        hub.clock().clone(),
        hub.ids().clone(),
        probe.clone(),
    ));
    register_trash_tools(&mut reg, &personal, services);
    register_asset_read_tools(&mut reg);
    register_asset_tools(&mut reg, services);
    register_ref_tools(&mut reg);
    register_watch_tools(&mut reg);
    register_job_tools(&mut reg);
    let all_tools = Arc::new(OnceLock::new());
    register_project_tools(&mut reg, all_tools.clone());
    if test_tools {
        register_test_tools(&mut reg, gates.clone(), states.clone());
    }
    // `project.context` suggests from every tool registered here.
    let _ = all_tools.set(reg.descriptors());
    reg
}
