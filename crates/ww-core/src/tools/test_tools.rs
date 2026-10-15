//! The test-channel tools (spec Appendix D): registered only when the channel is `test-*`,
//! module `"test"`. Spec and E2E tests depend on them.
//!
//! | Tool | Effect | Arguments | Result (`structured`) |
//! |---|---|---|---|
//! | `test.echo` | read | `{text}` | `{text}` |
//! | `test.write` | write | `{label, fail?}` | `{label, executions}`; `fail: true` → `params.invalid` |
//! | `test.block` | write | `{gate}` | `{gate, executions}` once a test opens the gate |
//! | `test.panic` | read | `{}` | panics |
//! | `test.state_panic` | read | `{}` | panics the suite state actor (QA-012) |
//! | `test.destructive` | destructive | `{}` | `{executed: true}` |
//!
//! `executions` counts the runs of that tool with that label or gate in this core, failed runs
//! included. `test.emit` comes with the event bus (Task 11).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ww_protocol::{EffectClass, ErrorBody, codes};

use super::registry::{Registry, typed_tool};
use crate::state::StateRegistry;

/// The module every test tool declares.
pub const TEST_MODULE: &str = "test";

/// The gates `test.block` waits on. A gate, once opened, stays open.
#[derive(Debug)]
pub struct TestGates {
    open: watch::Sender<HashSet<String>>,
}

impl Default for TestGates {
    fn default() -> Self {
        TestGates {
            open: watch::Sender::new(HashSet::new()),
        }
    }
}

impl TestGates {
    pub fn new() -> TestGates {
        TestGates::default()
    }

    /// Opens `gate`: every `test.block` waiting on it, and every later one, goes on.
    pub fn open(&self, gate: &str) {
        self.open.send_modify(|open| {
            open.insert(gate.to_string());
        });
    }

    /// Waits until `gate` is open.
    async fn wait(&self, gate: &str) {
        let mut rx = self.open.subscribe();
        // The sender lives as long as `self`, so this cannot fail while we borrow it.
        let _ = rx.wait_for(|open| open.contains(gate)).await;
    }
}

/// Run counters per tool and label.
#[derive(Debug, Default)]
struct Counters(Mutex<HashMap<(&'static str, String), u64>>);

impl Counters {
    /// Counts one more run of `tool` with `label`; returns the new count.
    fn bump(&self, tool: &'static str, label: &str) -> u64 {
        let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let n = counts.entry((tool, label.to_string())).or_insert(0);
        *n += 1;
        *n
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct EchoResult {
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    label: String,
    #[serde(default)]
    fail: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct WriteResult {
    label: String,
    executions: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BlockArgs {
    gate: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct BlockResult {
    gate: String,
    executions: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// What a tool that never returns normally would have returned.
#[derive(Debug, Serialize, JsonSchema)]
struct Nothing {}

#[derive(Debug, Serialize, JsonSchema)]
struct StatePanicResult {
    requested: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DestructiveResult {
    executed: bool,
}

/// `test.panic`'s body.
fn boom() -> Result<(Nothing, String), ErrorBody> {
    panic!("test.panic was called")
}

/// Registers the test-channel tools on `reg`. `test.block` waits on `gates`;
/// `test.state_panic` panics the actor of `states`' suite database.
pub fn register_test_tools(reg: &mut Registry, gates: Arc<TestGates>, states: Arc<StateRegistry>) {
    let counters = Arc::new(Counters::default());

    reg.register(typed_tool(
        "test.echo",
        "Test channels only: returns its text.",
        EffectClass::Read,
        TEST_MODULE,
        |_ctx, args: EchoArgs| {
            Box::pin(async move {
                let summary = format!("echoed {} characters", args.text.chars().count());
                Ok((EchoResult { text: args.text }, summary))
            })
        },
    ));

    let write_counters = counters.clone();
    reg.register(typed_tool(
        "test.write",
        "Test channels only: a write that counts its runs per label. `fail: true` returns params.invalid.",
        EffectClass::Write,
        TEST_MODULE,
        move |ctx, args: WriteArgs| {
            let executions = write_counters.bump("test.write", &args.label);
            Box::pin(async move {
                if args.fail {
                    return Err(ErrorBody::new(
                        &codes::PARAMS_INVALID,
                        "test.write was asked to fail",
                        ctx.trace_id.clone(),
                    ));
                }
                let summary = format!("run {executions}");
                Ok((
                    WriteResult {
                        label: args.label,
                        executions,
                    },
                    summary,
                ))
            })
        },
    ));

    let block_counters = counters;
    reg.register(typed_tool(
        "test.block",
        "Test channels only: a write that waits until an in-process test opens its gate.",
        EffectClass::Write,
        TEST_MODULE,
        move |_ctx, args: BlockArgs| {
            let gates = gates.clone();
            let counters = block_counters.clone();
            Box::pin(async move {
                gates.wait(&args.gate).await;
                let executions = counters.bump("test.block", &args.gate);
                Ok((
                    BlockResult {
                        gate: args.gate,
                        executions,
                    },
                    format!("run {executions}"),
                ))
            })
        },
    ));

    reg.register(typed_tool(
        "test.panic",
        "Test channels only: panics (QA-012).",
        EffectClass::Read,
        TEST_MODULE,
        |_ctx, _args: NoArgs| Box::pin(async move { boom() }),
    ));

    reg.register(typed_tool(
        "test.state_panic",
        "Test channels only: makes the suite state actor panic, which stops the core (QA-012).",
        EffectClass::Read,
        TEST_MODULE,
        move |_ctx, _args: NoArgs| {
            states.suite().panic_actor();
            Box::pin(async move {
                Ok((
                    StatePanicResult { requested: true },
                    "the suite state actor was asked to panic".to_string(),
                ))
            })
        },
    ));

    reg.register(typed_tool(
        "test.destructive",
        "Test channels only: a destructive tool that does nothing.",
        EffectClass::Destructive,
        TEST_MODULE,
        |_ctx, _args: NoArgs| {
            Box::pin(async move { Ok((DestructiveResult { executed: true }, "executed".into())) })
        },
    ));
}
