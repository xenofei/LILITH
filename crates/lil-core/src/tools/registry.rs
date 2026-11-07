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

use lil_protocol::app::{ConfirmFacts, Touch, TouchAccess};
use lil_protocol::{
    CallerKind, EffectClass, ErrorBody, MAX_DESCRIPTION_CHARS, ToolDescriptor, ToolOutput, codes,
};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::dispatcher::Redispatch;
use super::stages::{ConfirmInput, ConfirmedVia, Settings};
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

/// Who a tool's call asks for your confirmation (P4b-R9, P4b-R33; CORE-109, CORE-157).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfirmPolicy {
    /// An agent's call of a `destructive` or `exec` tool (ARCH-052): every tool but these.
    #[default]
    AgentsOnly,
    /// Every caller's call, your own included, always in the core's dialog (`asset.trash`).
    EveryCaller,
}

/// A path a tool's call touches, as a template over its arguments (P4b-R33; Appendix H
/// "Confirmations"): `{name}` is the argument `name` (a string), `{project}` falls back to the
/// call's project, and `{name[]}` gives one touch per string of the array argument `name`
/// (`{files[]}` for outside files).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchDecl {
    pub template: &'static str,
    pub access: TouchAccess,
}

impl TouchDecl {
    /// The argument names its placeholders name, in order, each with whether it is an array
    /// placeholder (`{name[]}`, `[]` left off the name) or a single one (`{name}`).
    pub fn placeholders(&self) -> Vec<(&'static str, bool)> {
        let mut found = Vec::new();
        let mut rest = self.template;
        while let Some((name, after)) = next_placeholder(rest) {
            found.push(match name.strip_suffix("[]") {
                Some(field) => (field, true),
                None => (name, false),
            });
            rest = after;
        }
        found
    }
}

/// The first `{…}` of `rest`: its name and the text after it.
fn next_placeholder(rest: &str) -> Option<(&str, &str)> {
    let open = rest.find('{')?;
    let close = rest[open..].find('}')? + open;
    Some((&rest[open + 1..close], &rest[close + 1..]))
}

/// The touches of a call with `args` in `project` (Appendix H "Confirmations"): each
/// declaration filled from the arguments. A declaration whose placeholder names an argument the
/// call doesn't give (or not as a string; `{project}` with neither the argument nor a call
/// project) is skipped, and the others are still listed; `{name[]}` gives one touch per string
/// of the array, so an empty array gives none.
pub fn fill_touches(
    decls: &[TouchDecl],
    args: &serde_json::Value,
    project: Option<&str>,
) -> Vec<Touch> {
    decls
        .iter()
        .flat_map(|decl| {
            fill_one(decl.template, args, project)
                .into_iter()
                .map(|path| Touch {
                    path,
                    access: decl.access,
                })
        })
        .collect()
}

/// The paths of one template (none when an argument it names is missing).
fn fill_one(template: &str, args: &serde_json::Value, project: Option<&str>) -> Vec<String> {
    let mut paths = vec![String::new()];
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some((name, after)) = next_placeholder(rest) else {
            break;
        };
        let literal = &rest[..open];
        let values: Vec<&str> = match name.strip_suffix("[]") {
            Some(field) => args
                .get(field)
                .and_then(serde_json::Value::as_array)
                .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
                .unwrap_or_default(),
            None => args
                .get(name)
                .and_then(serde_json::Value::as_str)
                .or(if name == "project" { project } else { None })
                .into_iter()
                .collect(),
        };
        if values.is_empty() {
            return Vec::new();
        }
        paths = paths
            .iter()
            .flat_map(|p| values.iter().map(move |v| format!("{p}{literal}{v}")))
            .collect();
        rest = after;
    }
    paths.into_iter().map(|p| p + rest).collect()
}

/// What a tool's facts function returns (P4b-R33): the full facts, `None` when it has none.
pub type FactsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<ConfirmFacts>, ErrorBody>> + Send + 'a>>;

/// A tool's facts function: what its confirmation tells you, recomputed at the approved retry
/// (P4b-R9, P4b-R33). It returns the full lists, every dependent and the versions, in any
/// order: the core hashes both sorted, shows the dependents sorted (the first 50, with the
/// full count) and the versions in the order given ([`super::stages::seal_facts`]).
/// `Ok(None)` means "this call has no facts", and that is a fact too: the confirmation stores
/// its hash (`args_hash(null)`, the SHA-256 of `null`) like any other, so facts that appear
/// before the approved retry, or vanish, ask again (review I1). Only a tool without a facts
/// function, or a row stored before the hash existed, has no hash and is never checked.
pub type FactsFn = Arc<dyn for<'a> Fn(&'a ConfirmInput<'a>) -> FactsFuture<'a> + Send + Sync>;

/// `f` as a [`FactsFn`] (a closure's higher-ranked signature is inferred from this bound).
pub fn facts_fn<F>(f: F) -> FactsFn
where
    F: for<'a> Fn(&'a ConfirmInput<'a>) -> FactsFuture<'a> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// What the core keeps beside a tool's descriptor and never puts on the wire (P4b-R33): its
/// confirmation policy, the paths its calls touch and its facts function. The default:
/// `AgentsOnly`, no touches, no facts.
#[derive(Clone, Default)]
pub struct ToolMeta {
    pub confirm_policy: ConfirmPolicy,
    pub touches: Vec<TouchDecl>,
    pub facts: Option<FactsFn>,
}

impl std::fmt::Debug for ToolMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolMeta")
            .field("confirm_policy", &self.confirm_policy)
            .field("touches", &self.touches)
            .field("facts", &self.facts.is_some())
            .finish()
    }
}

/// The meta of a tool registered without one.
static DEFAULT_META: ToolMeta = ToolMeta {
    confirm_policy: ConfirmPolicy::AgentsOnly,
    touches: Vec::new(),
    facts: None,
};

/// Every tool the core serves, by name.
#[derive(Default)]
pub struct Registry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Each tool's [`ToolMeta`], for tools registered with one ([`Registry::register_with`]).
    metas: BTreeMap<String, ToolMeta>,
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
        self.register_with(t, ToolMeta::default());
    }

    /// Adds `t` with its core-internal `meta` (P4b-R33).
    ///
    /// # Panics
    /// As [`Registry::register`]; and for an `EveryCaller` tool that only reads (a `read` call
    /// has no idempotency key, so it could never be asked about).
    pub fn register_with(&mut self, t: Arc<dyn Tool>, meta: ToolMeta) {
        let d = t.descriptor();
        assert!(
            meta.confirm_policy != ConfirmPolicy::EveryCaller || d.effect != EffectClass::Read,
            "tool {}: a read tool can't ask every caller",
            d.name
        );
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
        if meta.confirm_policy != ConfirmPolicy::AgentsOnly
            || !meta.touches.is_empty()
            || meta.facts.is_some()
        {
            self.metas.insert(name.clone(), meta);
        }
        self.tools.insert(name, t);
    }

    /// The core-internal meta of the tool `name` ([`ToolMeta::default`] for one registered
    /// without, or unknown).
    pub fn meta(&self, name: &str) -> &ToolMeta {
        self.metas.get(name).unwrap_or(&DEFAULT_META)
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
