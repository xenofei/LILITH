//! The tool registry (ARCH-003, ARCH-037, CORE-058): every tool the core serves, by name.
//!
//! A [`Tool`] has a [`ToolDescriptor`] (name, description of at most 2000 characters, effect
//! class, module, and JSON Schema 2020-12 input and output schemas), decides who may see it,
//! checks its arguments and runs. [`typed_tool`] builds one from a typed async function: its
//! schemas come from `schemars`, and its arguments are checked by deserialising them.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use lil_protocol::{
    CallerKind, EffectClass, ErrorBody, MAX_DESCRIPTION_CHARS, ToolDescriptor, ToolOutput, codes,
};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::dispatcher::Redispatch;
use super::stages::{ConfirmedVia, Settings};
use crate::events::EventBus;
use crate::jobs::{EstimateKey, JobCaller, JobId, JobOrigin, Jobs, SubmittedSlot};
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
    /// The core's jobs: a tool whose work runs long submits a job and returns at once
    /// (ARCH-034).
    pub jobs: Arc<Jobs>,
    /// Set by [`Jobs::submit`]: the dispatcher then leaves Meter and Record to the job.
    pub submitted: Arc<SubmittedSlot>,
    /// The cap reservation the reserve stage made for the call (`rv_<16 hex>`); a job the
    /// call submits takes it over.
    pub reservation: Option<String>,
    /// Who confirmed the call, when a confirmation let it run.
    pub confirmed_via: Option<ConfirmedVia>,
    /// The call's idempotency key; `None` for a `read` tool.
    pub idem_key: Option<String>,
    /// The call's id (`c_<16 hex>`), under which the reserve stage reserved.
    pub call_id: String,
    /// The tool called and its (valid) arguments: a job the call submits keeps them, for
    /// `job.retry` to dispatch them again.
    pub tool: String,
    pub arguments: serde_json::Value,
    /// Set when the call runs as a retry of that job (`job.retry`).
    pub retry_of: Option<JobId>,
    /// The dispatcher, for `job.retry`; `None` outside one.
    pub redispatch: Option<Redispatch>,
}

impl CallCtx {
    /// [`CallCtx::project`], or `sketchbook` when the call has none.
    pub fn project_or_default(&self) -> String {
        self.project
            .clone()
            .unwrap_or_else(|| crate::projects::DEFAULT_PROJECT.to_string())
    }

    /// The jobs module's view of the call (CORE-010: `jobs` takes no `tools` type).
    pub fn job_caller(&self) -> JobCaller {
        JobCaller {
            caller: self.session.caller.clone(),
            idem_scope: format!("{}|{}", self.session.caller, self.session.workspace_id()),
            workspace: self.workspace.clone(),
            trace_id: self.trace_id.clone(),
            idem_key: self.idem_key.clone(),
            call_id: self.call_id.clone(),
            reservation: self.reservation.clone(),
            submitted: self.submitted.clone(),
            origin: (!self.tool.is_empty()).then(|| JobOrigin {
                tool: self.tool.clone(),
                arguments: self.arguments.clone(),
            }),
            retry_of: self.retry_of.clone(),
        }
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
    /// The kind and estimate key of the job a call with arguments `args` (which `validate`
    /// accepted) would submit (`job.estimate`, CORE-161; Appendix G): the same key the job's
    /// handler gives it at submission. `None` (the default): the call is estimated `no_key`.
    fn estimate_key(&self, _args: &serde_json::Value) -> Option<EstimateKey> {
        None
    }
}

/// `tool` with an estimate key: `key` gives it from a call's arguments
/// ([`Tool::estimate_key`]).
pub fn with_estimate_key(
    tool: Arc<dyn Tool>,
    key: fn(&serde_json::Value) -> Option<EstimateKey>,
) -> Arc<dyn Tool> {
    Arc::new(Keyed { tool, key })
}

/// A tool with an estimate key ([`with_estimate_key`]).
struct Keyed {
    tool: Arc<dyn Tool>,
    key: fn(&serde_json::Value) -> Option<EstimateKey>,
}

impl Tool for Keyed {
    fn descriptor(&self) -> &ToolDescriptor {
        self.tool.descriptor()
    }

    fn visible_to(&self, caller: &CallerKind) -> bool {
        self.tool.visible_to(caller)
    }

    fn validate(&self, args: &serde_json::Value) -> Result<(), String> {
        self.tool.validate(args)
    }

    fn run<'a>(&'a self, ctx: &'a CallCtx, args: serde_json::Value) -> ToolFuture<'a> {
        self.tool.run(ctx, args)
    }

    fn estimate_key(&self, args: &serde_json::Value) -> Option<EstimateKey> {
        (self.key)(args)
    }
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
    /// Every tool's descriptor, set once by [`Registry::seal`].
    all_tools: Arc<OnceLock<Vec<ToolDescriptor>>>,
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

    /// A handle to every tool's descriptor, filled by [`Registry::seal`] once every tool is
    /// registered: for a tool that describes the others (`workspace.doctor`'s Claude Code
    /// rules, AI-114).
    pub fn all_tools(&self) -> Arc<OnceLock<Vec<ToolDescriptor>>> {
        self.all_tools.clone()
    }

    /// Fills [`Registry::all_tools`] with [`Registry::descriptors`]; the dispatcher calls it
    /// when it takes the registry. Only the first call counts.
    pub fn seal(&self) {
        let _ = self.all_tools.set(self.descriptors());
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
