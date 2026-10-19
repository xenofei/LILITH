//! The resource tools (APP-078, APP-082; Appendix F "Product tools"), module `resources`:
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `resource.save` | write | `{kind, id, name, data, scope?}` | `{resource, path, replaced_entry_id?}` |
//! | `resource.list` | read | `{kind?, scope?}` | `{resources: [{kind, id, name, scope, updated}]}` |
//! | `resource.get` | read | `{kind, id, scope?}` | `{resource, path}` |
//! | `resource.delete` | destructive | `{kind, id, scope?}` | `{entry_id}` |
//!
//! `path` is relative to the scope's resources folder, `<kind>/<id>.json` with a forward slash
//! (ruling P3-R13). `replaced_entry_id` is the trash entry of the version a save replaced.

use std::sync::Arc;

use lil_protocol::EffectClass;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RESOURCES_MODULE, Resource, ResourceScope, ResourceSummary, Resources, default_scope};
use crate::tools::{Registry, typed_tool};

const SAVE_DESCRIPTION: &str = "Save a Library resource: a swatch, palette, prompt_preset, \
    style_preset, model_profile, prompt_brief or workflow, as one JSON file. scope is workspace \
    or personal; without it the resource goes to the workspace when there is one, except a \
    model_profile, which goes to the personal scope. Saving over an existing resource never \
    loses it: the old file goes to the trash first (reason resource.save) and replaced_entry_id \
    names the entry, so trash.restore can bring it back. The resource keeps its first created \
    time.";

const LIST_DESCRIPTION: &str = "List Library resources: kind, id, name, scope and last update, \
    for one kind or all, in the workspace, the personal scope or (without scope) both.";

const GET_DESCRIPTION: &str = "Read one Library resource by kind and id. Without scope the \
    workspace is searched first, then the personal scope. path is relative to the scope's \
    resources folder.";

const DELETE_DESCRIPTION: &str = "Delete a Library resource. It is moved to the trash, never \
    destroyed: entry_id names the trash entry, and trash.restore brings it back for 30 days. \
    Without scope the workspace is searched first, then the personal scope. Asks the person \
    to confirm when an agent calls it.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    /// swatch, palette, prompt_preset, style_preset, model_profile, prompt_brief or workflow.
    kind: String,
    /// Lower-case letters, digits, `_` and `-`, starting with a letter; at most 63 characters.
    id: String,
    name: String,
    /// What the kind defines.
    data: Value,
    scope: Option<ResourceScope>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SaveResult {
    resource: Resource,
    /// Relative to the scope's resources folder: `<kind>/<id>.json`.
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    replaced_entry_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    kind: Option<String>,
    scope: Option<ResourceScope>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ListResult {
    resources: Vec<ResourceSummary>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TargetArgs {
    kind: String,
    id: String,
    scope: Option<ResourceScope>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct GetResult {
    resource: Resource,
    /// Relative to the scope's resources folder: `<kind>/<id>.json`.
    path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DeleteResult {
    /// The trash entry (`te_<16 hex>`).
    entry_id: String,
}

fn relative(kind: &str, id: &str) -> String {
    format!("{kind}/{id}.json")
}

/// Registers `resource.save`, `resource.list`, `resource.get` and `resource.delete` on `reg`.
pub fn register_resource_tools(reg: &mut Registry, resources: &Arc<Resources>) {
    let r = resources.clone();
    reg.register(typed_tool(
        "resource.save",
        SAVE_DESCRIPTION,
        EffectClass::Write,
        RESOURCES_MODULE,
        move |ctx, args: SaveArgs| {
            let r = r.clone();
            Box::pin(async move {
                let scope = args
                    .scope
                    .unwrap_or_else(|| default_scope(&args.kind, ctx.workspace.is_some()));
                let resource = Resource {
                    schema_version: super::RESOURCE_SCHEMA_VERSION,
                    kind: args.kind,
                    id: args.id,
                    name: args.name,
                    created: String::new(),
                    updated: String::new(),
                    data: args.data,
                };
                let (resource, _file, replaced_entry_id) = r.save(ctx, scope, resource).await?;
                let summary = format!(
                    "saved {} {} ({})",
                    resource.kind,
                    resource.id,
                    scope.as_str()
                );
                Ok((
                    SaveResult {
                        path: relative(&resource.kind, &resource.id),
                        resource,
                        replaced_entry_id,
                    },
                    summary,
                ))
            })
        },
    ));
    let r = resources.clone();
    reg.register(typed_tool(
        "resource.list",
        LIST_DESCRIPTION,
        EffectClass::Read,
        RESOURCES_MODULE,
        move |ctx, args: ListArgs| {
            let r = r.clone();
            Box::pin(async move {
                let resources = r.list(ctx, args.kind.as_deref(), args.scope).await?;
                let summary = format!("{} resource(s)", resources.len());
                Ok((ListResult { resources }, summary))
            })
        },
    ));
    let r = resources.clone();
    reg.register(typed_tool(
        "resource.get",
        GET_DESCRIPTION,
        EffectClass::Read,
        RESOURCES_MODULE,
        move |ctx, args: TargetArgs| {
            let r = r.clone();
            Box::pin(async move {
                let (resource, _file) = r.get(ctx, &args.kind, &args.id, args.scope).await?;
                let summary = format!("{} {}", resource.kind, resource.id);
                Ok((
                    GetResult {
                        path: relative(&resource.kind, &resource.id),
                        resource,
                    },
                    summary,
                ))
            })
        },
    ));
    let r = resources.clone();
    reg.register(typed_tool(
        "resource.delete",
        DELETE_DESCRIPTION,
        EffectClass::Destructive,
        RESOURCES_MODULE,
        move |ctx, args: TargetArgs| {
            let r = r.clone();
            Box::pin(async move {
                let entry_id = r.delete(ctx, &args.kind, &args.id, args.scope).await?;
                let summary = format!("moved {} {} to the trash", args.kind, args.id);
                Ok((DeleteResult { entry_id }, summary))
            })
        },
    ));
}
