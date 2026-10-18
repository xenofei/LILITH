//! The project tools (CORE-055, CORE-039, CORE-058; Global Constraints, Product tools),
//! module `workspace`. They work in the session's workspace (`workspace.required` without
//! one).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `project.create` | write | `{project, name, type?}` | `{project, path}` |
//! | `project.set_status` | write | `{project, status: "active"\|"paused"\|"archived"}` | `{project, status, stale: [ref]}` |
//! | `project.status` | read | `{project?}` | `{project, status, assets_by_status, orphans: [path], needs_repair: [path], missing: [path]}` |
//! | `project.context` | read | `{project?}` | `{project, settings, links, suggested_tools: [name]}` |
//!
//! - `project.create` writes `projects/<slug>/project.yaml` from the template (`status:
//!   active`, `links: []`), then `progress.md` (the template, or the file that is there) with
//!   the entry `- <date>: project created` under `## Done`. A taken slug is `project.exists`,
//!   one that isn't a slug `params.invalid`. `type` is one of I4's project types (default
//!   `custom`). A create that wrote `project.yaml` but stopped before the `progress.md` entry
//!   is finished by a retry: when `project.yaml` is byte for byte what this call would write
//!   and `progress.md` has no `- <date>: project created` line, the entry is added and the
//!   call succeeds; anything else at that path is `project.exists`.
//! - `project.set_status` sets `status` in `project.yaml` with the comment-preserving YAML
//!   edit. Every status but `archived` runs the staleness check ([`restore_staleness`]), so a
//!   project leaving `archived` catches up (to `active` or to `paused`), and a retry after a
//!   check that failed midway finishes it. The check is idempotent: an asset already `stale`
//!   is not marked again. `stale` lists the assets this call marked.
//! - `project.status` is [`project_status`].
//! - `project.context` gives the call's settings with their sources, the project's `links`,
//!   and `suggested_tools`: the tools whose module is `workspace` or one of the project's
//!   `modules` (CORE-058).
//!
//! An omitted `project` is the call's resolved project (`CallCtx::project`). Changes are
//! published on the topic `project:<slug>` as `{kind: "status"}` and `{kind: "progress"}`.

use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

use lil_protocol::codes::{PARAMS_INVALID, PROJECT_EXISTS};
use lil_protocol::{EffectClass, ErrorBody, ToolDescriptor};
use lil_schema::YamlEdit;
use lil_schema::workspace_yaml::{PROGRESS_TEMPLATE, new_project_yaml};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::registry::{CallCtx, Registry, typed_tool};
use super::stages::Settings;
use super::workspace_tools::WORKSPACE_MODULE;
use crate::projects::{
    PROJECT_STATUSES, ProjectFile, ProjectStatus, add_done_entry, progress_path, project_status,
    project_yaml_path, read_project, restore_staleness, unknown_project,
};
use crate::store::{Expect, StoreError};

/// I4's project types.
pub const PROJECT_TYPES: &[&str] = &[
    "game",
    "asset-pack",
    "vtuber",
    "animation",
    "audio",
    "media",
    "custom",
];

const CREATE_DESCRIPTION: &str = "Create a project in the workspace: projects/<project>/ with \
    project.yaml (name, type, status active, no links) and progress.md, whose Done list gets \
    the entry 'project created'. project is the slug (lowercase letters, digits and '-'); \
    type is one of game, asset-pack, vtuber, animation, audio, media and custom (default \
    custom). A slug that is taken fails with project.exists.";

const SET_STATUS_DESCRIPTION: &str = "Set a project's status to active, paused or archived in \
    its project.yaml (comments are kept). Archived projects are skipped when dependents are \
    looked for; setting active or paused runs the staleness check, and stale lists the refs \
    of the assets it marked stale (their inputs got newer approved versions while the project \
    was archived and aren't pinned).";

const STATUS_DESCRIPTION: &str = "A project's status (active, paused or archived), the number \
    of its assets in each status, and the paths that need attention: orphans (left by an \
    interrupted operation; never deleted, only listed), files and manifests that need repair, \
    and files the suite expects that are missing. project defaults to the session's active \
    project.";

const CONTEXT_DESCRIPTION: &str = "A project's context: the settings that apply to it (each \
    value with its source: workspace, project or request), the sources it links to, and the \
    tools suggested for it (those of the workspace module and of the project's enabled \
    modules). project defaults to the session's active project.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    /// The new project's slug.
    project: String,
    /// Its display name.
    name: String,
    /// One of I4's project types; default `custom`.
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CreateResult {
    project: String,
    /// The project's folder, workspace-relative.
    path: String,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StatusArg {
    Active,
    Paused,
    Archived,
}

impl StatusArg {
    fn as_str(self) -> &'static str {
        match self {
            StatusArg::Active => "active",
            StatusArg::Paused => "paused",
            StatusArg::Archived => "archived",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetStatusArgs {
    project: String,
    status: StatusArg,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SetStatusResult {
    project: String,
    status: String,
    /// The refs of the assets the staleness check marked stale.
    stale: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectArg {
    /// A project slug; omitted: the session's active project.
    project: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ContextResult {
    project: String,
    settings: Settings,
    links: Vec<String>,
    suggested_tools: Vec<String>,
}

fn invalid(ctx: &CallCtx, why: String) -> ErrorBody {
    ErrorBody::new(&PARAMS_INVALID, why, ctx.trace_id.clone())
}

/// `e` with the call's trace id.
fn traced(ctx: &CallCtx, mut e: ErrorBody) -> ErrorBody {
    e.trace_id.clone_from(&ctx.trace_id);
    e
}

/// Today's UTC date by the store's clock.
fn today(ctx: &CallCtx) -> Result<String, ErrorBody> {
    let store = &ctx.workspace()?.store;
    let secs = store
        .clock()
        .utc_now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    Ok(lil_schema::time::date(secs))
}

async fn create(ctx: &CallCtx, args: CreateArgs) -> Result<(CreateResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let store = &open.store;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let slug = args.project;
    let yaml_rel = project_yaml_path(&slug).map_err(|e| traced(ctx, e))?;
    let progress_rel = progress_path(&slug).map_err(|e| traced(ctx, e))?;
    let name = args.name.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(invalid(
            ctx,
            format!("a project name is one non-empty line, not {:?}", args.name),
        ));
    }
    let kind = args.kind.as_deref().unwrap_or("custom");
    if !PROJECT_TYPES.contains(&kind) {
        return Err(invalid(
            ctx,
            format!(
                "{kind:?} is not a project type; the types are {}",
                PROJECT_TYPES.join(", ")
            ),
        ));
    }
    let exists = || {
        let mut e = ErrorBody::new(
            &PROJECT_EXISTS,
            format!("There is a project `{slug}` already; pick another slug."),
            ctx.trace_id.clone(),
        );
        e.data = Some(json!({ "project": slug }));
        e
    };
    let date = today(ctx)?;
    let yaml = new_project_yaml(&slug, name, kind, &date);
    let entry = format!("- {date}: project created");
    match store.read(&yaml_rel).await.map_err(error)? {
        None => match store
            .write(&yaml_rel, yaml.clone().into_bytes(), Expect::New)
            .await
        {
            Ok(_) => {}
            // Another writer made it after the read above.
            Err(StoreError::HashConflict {
                cloud_only: false, ..
            }) => return Err(exists()),
            Err(e) => return Err(error(e)),
        },
        // A create that stopped after `project.yaml` (module docs): finished only when the
        // file is exactly this call's and the entry isn't there yet.
        Some(current) if current == yaml.as_bytes() => {
            let progress = store.read(&progress_rel).await.map_err(error)?;
            let has_entry = progress.is_some_and(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .any(|l| l.trim_end() == entry)
            });
            if has_entry {
                return Err(exists());
            }
            tracing::info!(project = %slug, "finishing a project create that stopped before progress.md");
        }
        Some(_) => return Err(exists()),
    }
    let template = PROGRESS_TEMPLATE.replace("<name>", name);
    store
        .edit_text(&progress_rel, &move |current: &str| {
            let text = if current.is_empty() {
                template.as_str()
            } else {
                current
            };
            add_done_entry(text, &date, "project created").map_err(|why| StoreError::Io {
                path: "progress.md".to_string(),
                why,
            })
        })
        .await
        .map_err(error)?;
    ctx.events
        .publish(&format!("project:{slug}"), json!({"kind": "progress"}));
    let summary = format!("created the project {slug}");
    Ok((
        CreateResult {
            path: format!("projects/{slug}"),
            project: slug,
        },
        summary,
    ))
}

async fn set_status(
    ctx: &CallCtx,
    args: SetStatusArgs,
) -> Result<(SetStatusResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let slug = args.project;
    let yaml_rel = project_yaml_path(&slug).map_err(|e| traced(ctx, e))?;
    match read_project(&open.store, &slug).await.map_err(error)? {
        ProjectFile::Missing => return Err(traced(ctx, unknown_project(&slug))),
        ProjectFile::Parsed(_) => {}
        ProjectFile::Unreadable(why) => {
            return Err(error(StoreError::YamlUnsupported(format!(
                "{yaml_rel} doesn't parse ({why}); fix it first"
            ))));
        }
    };
    let status = args.status.as_str();
    open.store
        .edit_yaml(
            &yaml_rel,
            &[YamlEdit::SetScalar {
                path: vec!["status".to_string()],
                value: status.to_string(),
            }],
        )
        .await
        .map_err(error)?;
    // Not only on a move out of `archived`: a check that failed midway (after `status` was
    // written) must be finished by the retry, which then finds the status already set.
    let stale = if args.status != StatusArg::Archived {
        restore_staleness(open, &slug).await.map_err(error)?
    } else {
        Vec::new()
    };
    ctx.events.publish(
        &format!("project:{slug}"),
        json!({"kind": "status", "status": status}),
    );
    debug_assert!(PROJECT_STATUSES.contains(&status));
    let summary = format!(
        "{slug} is {status}{}",
        if stale.is_empty() {
            String::new()
        } else {
            format!("; {} asset(s) marked stale", stale.len())
        }
    );
    Ok((
        SetStatusResult {
            project: slug,
            status: status.to_string(),
            stale,
        },
        summary,
    ))
}

async fn status(ctx: &CallCtx, args: ProjectArg) -> Result<(ProjectStatus, String), ErrorBody> {
    let open = ctx.workspace()?;
    let project = args.project.unwrap_or_else(|| ctx.project_or_default());
    let status = project_status(open, &project)
        .await
        .map_err(|e| traced(ctx, e))?;
    let summary = format!("{project} is {}", status.status);
    Ok((status, summary))
}

async fn context(
    ctx: &CallCtx,
    args: ProjectArg,
    tools: &OnceLock<Vec<ToolDescriptor>>,
) -> Result<(ContextResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let project = args.project.unwrap_or_else(|| ctx.project_or_default());
    project_yaml_path(&project).map_err(|e| traced(ctx, e))?;
    let links = match read_project(&open.store, &project)
        .await
        .map_err(|e| e.to_error(&ctx.trace_id))?
    {
        ProjectFile::Missing => return Err(traced(ctx, unknown_project(&project))),
        ProjectFile::Parsed(p) => p.links,
        ProjectFile::Unreadable(_) => Vec::new(),
    };
    let modules = ctx.settings.modules();
    let suggested_tools: Vec<String> = tools
        .get()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|d| d.module == WORKSPACE_MODULE || modules.contains(&d.module))
        .map(|d| d.name.clone())
        .collect();
    let summary = format!("{project}: {} suggested tools", suggested_tools.len());
    Ok((
        ContextResult {
            project,
            settings: ctx.settings.clone(),
            links,
            suggested_tools,
        },
        summary,
    ))
}

/// Registers the project tools (module `workspace`). `project.context` suggests from `tools`,
/// which `core_registry` fills with every registered tool once registration is done.
pub fn register_project_tools(reg: &mut Registry, tools: Arc<OnceLock<Vec<ToolDescriptor>>>) {
    reg.register(typed_tool(
        "project.create",
        CREATE_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: CreateArgs| Box::pin(create(ctx, args)),
    ));
    reg.register(typed_tool(
        "project.set_status",
        SET_STATUS_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: SetStatusArgs| Box::pin(set_status(ctx, args)),
    ));
    reg.register(typed_tool(
        "project.status",
        STATUS_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        |ctx, args: ProjectArg| Box::pin(status(ctx, args)),
    ));
    reg.register(typed_tool(
        "project.context",
        CONTEXT_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        move |ctx, args: ProjectArg| {
            let tools = tools.clone();
            Box::pin(async move { context(ctx, args, &tools).await })
        },
    ));
}
