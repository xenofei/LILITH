//! The `tools` module (CORE-014): the tool registry and the dispatcher pipeline every
//! `tools/call` runs through (CORE-015–017), with the stages plan 3 replaces, the product
//! tools and the test-channel tools.

pub mod asset_read_tools;
pub mod dispatcher;
pub mod project_tools;
pub mod registry;
pub mod stages;
pub mod test_tools;
pub mod trash_tools;
pub mod watch_tools;
pub mod workspace_tools;

#[cfg(test)]
mod tests;

use std::sync::{Arc, OnceLock};

pub use asset_read_tools::register_asset_read_tools;
pub use dispatcher::Dispatcher;
pub use project_tools::register_project_tools;
pub use registry::{CallCtx, Registry, Tool, ToolFuture, typed_tool};
pub use stages::{
    CascadeSettings, ConfirmDecision, ConfirmStage, Estimate, EstimateStage, MeterStage,
    PipelineHooks, RecordStage, ReserveStage, SettingSource, Settings, SettingsInput,
    SettingsStage,
};
pub use test_tools::{TEST_MODULE, TestGates, register_test_tools};
pub use trash_tools::register_trash_tools;
pub use watch_tools::register_watch_tools;
pub use workspace_tools::{WORKSPACE_MODULE, register_workspace_tools};

use ww_platform::Env;
use ww_platform::fs::Probe;

use crate::state::StateRegistry;
use crate::workspace::WorkspaceHub;

/// The registry a core serves: the product tools on every channel and, on a test channel
/// (`test_tools`), the test-channel tools.
pub fn core_registry(
    test_tools: bool,
    gates: &Arc<TestGates>,
    states: &Arc<StateRegistry>,
    hub: &Arc<WorkspaceHub>,
    env: &Env,
    probe: &Arc<dyn Probe>,
) -> Registry {
    let mut reg = Registry::new();
    register_workspace_tools(&mut reg, hub.clone(), env.clone(), probe.clone());
    register_trash_tools(&mut reg);
    register_asset_read_tools(&mut reg);
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
