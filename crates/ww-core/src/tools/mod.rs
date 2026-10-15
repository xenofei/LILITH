//! The `tools` module (CORE-014): the tool registry and the dispatcher pipeline every
//! `tools/call` runs through (CORE-015–017), with the stages plan 3 replaces, and the
//! test-channel tools.

pub mod dispatcher;
pub mod registry;
pub mod stages;
pub mod test_tools;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use dispatcher::Dispatcher;
pub use registry::{CallCtx, Registry, Tool, ToolFuture, typed_tool};
pub use stages::{
    ConfirmDecision, ConfirmStage, Estimate, EstimateStage, MeterStage, PipelineHooks, RecordStage,
    ReserveStage, Settings, SettingsStage,
};
pub use test_tools::{TEST_MODULE, TestGates, register_test_tools};

use crate::state::StateRegistry;

/// The registry a core serves. Plan 2 has no product tools yet; on a test channel
/// (`test_tools`) it holds the test-channel tools.
pub fn core_registry(
    test_tools: bool,
    gates: &Arc<TestGates>,
    states: &Arc<StateRegistry>,
) -> Registry {
    let mut reg = Registry::new();
    if test_tools {
        register_test_tools(&mut reg, gates.clone(), states.clone());
    }
    reg
}
