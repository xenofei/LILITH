//! The `tools` module (CORE-014): the tool registry and the dispatcher pipeline every
//! `tools/call` runs through (CORE-015–017), with the stages plan 3 replaces, the Record stage
//! (CORE-020), the product tools, the live topics' snapshot providers and the test-channel
//! tools.

pub mod asset_read_tools;
pub mod asset_tools;
pub mod dispatcher;
pub mod doctor_tools;
pub mod project_tools;
pub mod record_stage;
pub mod registry;
pub mod stages;
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
pub use dispatcher::Dispatcher;
pub use doctor_tools::{DoctorArgs, DoctorCheck, register_doctor_tools};
pub use project_tools::register_project_tools;
pub use record_stage::WorkspaceRecord;
pub use registry::{CallCtx, Registry, Tool, ToolFuture, typed_tool};
pub use stages::{
    CascadeSettings, ConfirmDecision, ConfirmStage, Estimate, EstimateStage, MeterStage,
    PipelineHooks, RecordStage, ReserveStage, SettingSource, Settings, SettingsInput,
    SettingsStage,
};
pub use test_tools::{TEST_MODULE, TestGates, register_test_tools};
pub use topics::{AssetSnapshots, ProjectSnapshots, register_snapshots};
pub use trash_tools::register_trash_tools;
pub use watch_tools::register_watch_tools;
pub use workspace_tools::{WORKSPACE_MODULE, register_workspace_tools};

use lil_platform::Env;
use lil_platform::fs::Probe;

use crate::state::StateRegistry;
use crate::workspace::WorkspaceHub;

/// The registry a core serves: the product tools on every channel and, on a test channel
/// (`test_tools`), the test-channel tools. `git_env` is added to every `git` the doctor runs
/// (`CoreConfig.git_env`).
pub fn core_registry(
    test_tools: bool,
    gates: &Arc<TestGates>,
    states: &Arc<StateRegistry>,
    hub: &Arc<WorkspaceHub>,
    env: &Env,
    probe: &Arc<dyn Probe>,
    git_env: Arc<Vec<(OsString, OsString)>>,
) -> Registry {
    let mut reg = Registry::new();
    register_workspace_tools(&mut reg, hub.clone(), env.clone(), probe.clone());
    register_doctor_tools(&mut reg, probe.clone(), git_env);
    register_trash_tools(&mut reg);
    register_asset_read_tools(&mut reg);
    register_asset_tools(&mut reg);
    register_watch_tools(&mut reg);
    let all_tools = Arc::new(OnceLock::new());
    register_project_tools(&mut reg, all_tools.clone());
    if test_tools {
        register_test_tools(&mut reg, gates.clone(), states.clone());
    }
    // `project.context` suggests from every tool registered here.
    let _ = all_tools.set(reg.descriptors());
    reg
}
