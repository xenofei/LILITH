//! The tool registry (ARCH-003, ARCH-037, CORE-058): every tool the core serves, by name.
//!
//! A [`Tool`] has a [`ToolDescriptor`] (name, description of at most 2000 characters, effect
//! class, module, and JSON Schema 2020-12 input and output schemas), decides who may see it,
//! checks its arguments and runs. [`typed_tool`] builds one from a typed async function: its
//! schemas come from `schemars`, and its arguments are checked by deserialising them.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lil_protocol::{
    CallerKind, EffectClass, ErrorBody, MAX_DESCRIPTION_CHARS, ToolDescriptor, ToolOutput, codes,
};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::stages::Settings;
use crate::events::EventBus;
use crate::pipeline::RecordSlot;
use crate::session::Session;
use crate::workspace::OpenWorkspace;

/// What a tool's `run` returns.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolOutput, ErrorBody>> + Send + 'a>>;

/// What a running tool knows about its call.
#[derive(Debug, Clone)]
pub struct CallCtx {
    pub session: Arc<Session>,
    pub trace_id: String,
    pub settings: Settings,
    /// The core's event bus, for tools that publish (`test.emit`; plan 2b's store).
    pub events: Arc<EventBus>,
    /// The session's workspace, opened by the Resolve stage; `None` for a session without one.
    pub workspace: Option<Arc<OpenWorkspace>>,
    /// The call's project, resolved by the dispatcher (ARCH-088, ARCH-098): its `project`
    /// argument, else the session's active project, else the workspace's `state.json` hint,
    /// else `sketchbook`. `None` without a workspace.
    pub project: Option<String>,
    /// Where the tool leaves what the Record stage writes (CORE-020): a tool that records an
    /// operation sets its [`RecordPlan`](crate::pipeline::RecordPlan) here during Execute.
    pub record: Arc<RecordSlot>,
}

impl CallCtx {
    /// [`CallCtx::project`], or `sketchbook` when the call has none.
    pub fn project_or_default(&self) -> String {
        self.project
            .clone()
            .unwrap_or_else(|| crate::projects::DEFAULT_PROJECT.to_string())
    }

    /// The session's open workspace.
    ///
    /// # Errors
    /// `workspace.required` when the session has none (QA-005).
    pub fn workspace(&self) -> Result<&Arc<OpenWorkspace>, ErrorBody> {
        self.workspace.as_ref().ok_or_else(|| {
            ErrorBody::new(
                &codes::WORKSPACE_REQUIRED,
                "This tool works in a workspace, and the session has none. Connect with a \
                 workspace (`lil workspace use <path>` sets the default).",
                self.trace_id.clone(),
            )
        })
    }
}

/// One tool.
pub trait Tool: Send + Sync + 'static {
    fn descriptor(&self) -> &ToolDescriptor;
    /// Whether `caller` may list and call the tool. Plan 3: the engine-only `approve`.
    fn visible_to(&self, _caller: &CallerKind) -> bool {
        true
    }
    /// Stage 2: checks the arguments. `Err` is the `params.invalid` message.
    ///
    /// # Errors
    /// Why the arguments are not acceptable.
    fn validate(&self, args: &serde_json::Value) -> Result<(), String>;
    /// Stage 7: runs the call with arguments `validate` accepted.
    fn run<'a>(&'a self, ctx: &'a CallCtx, args: serde_json::Value) -> ToolFuture<'a>;
}

/// A JSON Schema 2020-12 root schema for `T`, with its `$schema` (ARCH-037).
pub(crate) fn schema_2020_12<T: JsonSchema>() -> serde_json::Value {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}

type RunFn = dyn for<'a> Fn(&'a CallCtx, serde_json::Value) -> ToolFuture<'a> + Send + Sync;

/// A tool made by [`typed_tool`].
struct TypedTool {
    descriptor: ToolDescriptor,
    validate: fn(&serde_json::Value) -> Result<(), String>,
    run: Box<RunFn>,
}

impl Tool for TypedTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn validate(&self, args: &serde_json::Value) -> Result<(), String> {
        (self.validate)(args)
    }

    fn run<'a>(&'a self, ctx: &'a CallCtx, args: serde_json::Value) -> ToolFuture<'a> {
        (self.run)(ctx, args)
    }
}

/// Checks `args` by deserialising them as `P`.
fn validate_as<P: DeserializeOwned>(args: &serde_json::Value) -> Result<(), String> {
    P::deserialize(args).map(drop).map_err(|e| e.to_string())
}

/// A tool from a typed async function. The input schema is `P`'s, the output schema `R`'s;
/// `validate` deserialises the arguments as `P` (so a `deny_unknown_fields` `P` refuses
/// unknown arguments); `run` returns `R` as `structured`, no artifacts, and the summary.
pub fn typed_tool<P, R, F>(
    name: &'static str,
    description: &'static str,
    effect: EffectClass,
    module: &'static str,
    f: F,
) -> Arc<dyn Tool>
where
    P: DeserializeOwned + JsonSchema + Send + 'static,
    // `'static` is added to the plan's bounds: the boxed future awaits `R`'s future, and a
    // `ToolFuture<'a>` for every `'a` can only hold it if `R: 'static`.
    R: Serialize + JsonSchema + 'static,
    F: for<'a> Fn(
            &'a CallCtx,
            P,
        )
            -> Pin<Box<dyn Future<Output = Result<(R, String), ErrorBody>> + Send + 'a>>
        + Send
        + Sync
        + 'static,
{
    let descriptor = ToolDescriptor {
        name: name.to_string(),
        description: description.to_string(),
        effect,
        module: module.to_string(),
        input_schema: schema_2020_12::<P>(),
        output_schema: schema_2020_12::<R>(),
        annotations: BTreeMap::new(),
    };
    let run: Box<RunFn> = Box::new(move |ctx: &CallCtx, args: serde_json::Value| {
        let params = match P::deserialize(args) {
            Ok(params) => params,
            Err(e) => {
                let error = ErrorBody::new(
                    &codes::PARAMS_INVALID,
                    format!("invalid arguments for {name}: {e}"),
                    ctx.trace_id.clone(),
                );
                return Box::pin(std::future::ready(Err(error))) as ToolFuture<'_>;
            }
        };
        let call = f(ctx, params);
        Box::pin(async move {
            let (result, summary_text) = call.await?;
            let structured = serde_json::to_value(result).map_err(|e| {
                ErrorBody::new(
                    &codes::CORE_BUG,
                    format!("{name} returned a result that is not JSON: {e}"),
                    ctx.trace_id.clone(),
                )
            })?;
            Ok(ToolOutput {
                structured,
                artifacts: Vec::new(),
                summary_text,
            })
        })
    });
    Arc::new(TypedTool {
        descriptor,
        validate: validate_as::<P>,
        run,
    })
}

/// Every tool the core serves, by name.
#[derive(Default)]
pub struct Registry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Adds `t`.
    ///
    /// # Panics
    /// If a tool of the same name is registered already, its description is longer than
    /// 2000 characters (ARCH-037), it declares no module (CORE-058), or its input schema is an
    /// object schema without `"additionalProperties": false` (inputs reject unknown fields;
    /// `#[serde(deny_unknown_fields)]` gives it). These are programming errors, caught by the
    /// registry tests.
    pub fn register(&mut self, t: Arc<dyn Tool>) {
        let d = t.descriptor();
        let chars = d.description.chars().count();
        assert!(
            chars <= MAX_DESCRIPTION_CHARS,
            "tool {}: the description has {chars} characters; at most {MAX_DESCRIPTION_CHARS} (ARCH-037)",
            d.name
        );
        assert!(
            !d.module.is_empty(),
            "tool {} declares no module (CORE-058)",
            d.name
        );
        let schema = &d.input_schema;
        assert!(
            schema.get("type").and_then(serde_json::Value::as_str) != Some("object")
                || schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)),
            "tool {}: its input schema allows unknown properties; inputs reject unknown fields \
             (add #[serde(deny_unknown_fields)])",
            d.name
        );
        let name = d.name.clone();
        assert!(
            !self.tools.contains_key(&name),
            "tool {name} is registered twice"
        );
        self.tools.insert(name, t);
    }

    /// The tool called `name`, whoever may see it.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// The descriptors of every tool, by name, whoever may see it.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools
            .values()
            .map(|t| t.descriptor().clone())
            .collect()
    }

    /// The descriptors of the tools `caller` may see ([`Tool::visible_to`]), by name.
    pub fn list_for(&self, caller: &CallerKind) -> Vec<ToolDescriptor> {
        self.tools
            .values()
            .filter(|t| t.visible_to(caller))
            .map(|t| t.descriptor().clone())
            .collect()
    }
}
