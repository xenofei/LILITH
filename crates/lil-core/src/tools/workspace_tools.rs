//! The workspace tools (ARCH-003, ARCH-090, ARCH-094, ARCH-098, CORE-007; owner decisions 1
//! and 2), module `workspace`: `workspace.create`, `workspace.set_default` and
//! `workspaces.list`. `lil workspace init|use` are thin wrappers over them, and so is the Home
//! panel later; the core stays the only writer of `suite.toml` ([`register_workspace`]).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `workspace.create` | write | `{path, name?, default?}` | `{id, path, created, default, warnings}` |
//! | `workspace.set_default` | write | `{path}` | `{id, path}` |
//! | `workspaces.list` | read | `{}` | `{workspaces: [{id, path, name?, default, served, missing}]}` |
//!
//! **`workspaces.list`** (the top bar's switcher, APP-006; Appendix G "Session and listing
//! tools") reads `suite.toml` and writes nothing: `default_workspace` first, then each
//! `[[workspaces]]` entry, each folder once (compared with `folder_key`). An existing folder
//! has the id of its canonical path and that path, and `name` from its workspace file when the
//! file has one; a folder that is gone is `missing`, with the path as registered and the id
//! of that path made absolute. `served` marks the workspace this core serves, the default
//! (owner decision 2), when its folder exists.
//!
//! **`workspace.create`:**
//! 1. The path is made absolute and passed through `check_workspace` before anything is
//!    created (QA-040), then created (`fsops::ensure_dir`), canonicalised and checked again (a
//!    junction or a short name can lead elsewhere).
//! 2. `workspace.path_invalid`: a file, the install folder or a folder inside it (ARCH-094,
//!    [`refuse_inside`]), or a folder inside another registered workspace.
//! 3. A folder that holds a workspace file (`names::workspace_file_in`: `lilith.yaml`, or the
//!    legacy one, R4) is registered only (`created: false`); nothing
//!    is written in it.
//! 4. Otherwise the hub opens it and its store writes `projects/sketchbook/project.yaml`,
//!    `projects/sketchbook/progress.md` (ARCH-098), the five `.gitignore` entries and the
//!    `.gitattributes` entry `projects/** -text` (CORE-040), each appended to a file that
//!    exists, and **last** `lilith.yaml`, whose presence makes the folder a workspace.
//! 5. It is registered (the default when `default` says so, or when none is set), the hub
//!    re-reads the default, and a workspace that isn't the default is closed again.
//!
//! Warnings, never errors (Review Focus 4): a network or cloud-synced folder, and a volume
//! that can't track files by ID (FAT, exFAT, or volume facts that can't be read). Such a
//! workspace is created and served as one without stable IDs.
//!
//! **`workspace.set_default`:** the folder must hold a workspace file (`lilith.yaml`, or the
//! legacy one). It is registered as the default, the hub re-reads the default,
//! `{kind: "default_changed", id, path}` goes out on the `workspace` topic, and the old default
//! is let go of: it closes once no call holds it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use lil_platform::fs::{PLACEHOLDER_ATTRIBUTES, Probe};
use lil_platform::names::{WORKSPACE_FILE, workspace_file_in};
use lil_platform::paths::{folder_key, strip_verbatim};
use lil_platform::{Env, check_workspace};
use lil_protocol::{EffectClass, ErrorBody, codes};
use lil_schema::SuiteToml;
use lil_schema::workspace_yaml::{
    PROGRESS_TEMPLATE, new_project_yaml, new_workspace_yaml, read_workspace_yaml,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::registry::{CallCtx, Registry, typed_tool};
use crate::store::{StoreError, WsPath, fsops, register_workspace};
use crate::workspace::{Access, WorkspaceHub, WorkspaceRef};

/// The LILITH module of every product tool: always available, never filtered (CORE-058).
pub const WORKSPACE_MODULE: &str = "workspace";

/// The `.gitignore` entries `workspace.create` writes (CORE-040, ARCH-096).
pub const GITIGNORE_ENTRIES: [&str; 5] = [
    ".lilith/",
    ".trash/",
    "inbox/*",
    "projects/*/assets/*/candidates/",
    "projects/*/assets/*/work/",
];

/// The `.gitattributes` entry `workspace.create` writes (CORE-040 clarification).
pub const GITATTRIBUTES_ENTRY: &str = "projects/** -text";

/// The first project of every workspace (ARCH-098).
const SKETCHBOOK: &str = "sketchbook";

/// `FILE_ATTRIBUTE_PINNED` and `FILE_ATTRIBUTE_UNPINNED` (winnt.h): cloud-files states a
/// synced folder carries besides the placeholder attributes.
const CLOUD_PIN_ATTRIBUTES: u32 = 0x0008_0000 | 0x0010_0000;

/// The warning for a folder on a network or cloud-synced volume.
pub const NETWORK_WARNING: &str = "This folder is on a network or cloud-synced volume. A sync \
    client can change files there behind LILITH's back, and file IDs there aren't \
    trusted, so changes are found by size and time. A local disk is safer for a workspace.";

/// The warning for a volume without stable file IDs.
pub const NO_FILE_IDS_WARNING: &str = "This volume can't track files by ID, so changes are \
    found by size and time, and a file changed in the last 2 seconds is hashed again.";

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCreateArgs {
    /// The workspace folder, as an absolute path. It is created when it doesn't exist.
    pub path: String,
    /// The name written to `lilith.yaml`; the folder's name when omitted.
    #[serde(default)]
    pub name: Option<String>,
    /// Make it the default workspace. Omitted, it becomes the default when none is set.
    #[serde(default)]
    pub default: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCreateResult {
    /// The workspace id: 16 hex characters.
    pub id: String,
    /// The canonical folder.
    pub path: String,
    /// `false` when the folder already held a workspace file and was only registered.
    pub created: bool,
    /// Whether it is the default workspace now.
    pub default: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetDefaultArgs {
    /// A workspace folder (it holds `lilith.yaml`, or the legacy workspace file), as an
    /// absolute path.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetDefaultResult {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {}

/// One workspace of `workspaces.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceListed {
    /// 16 hex characters.
    pub id: String,
    pub path: String,
    /// The workspace file's `name`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub default: bool,
    /// The workspace this core serves.
    pub served: bool,
    /// Its folder is gone.
    pub missing: bool,
    /// Why a registered path that is there isn't a usable folder: `not_a_folder` (a file is
    /// there) or `unreadable` (it can't be resolved); absent when it is a folder or gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<WorkspaceProblem>,
}

/// Why a registered workspace path isn't a usable folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceProblem {
    NotAFolder,
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListResult {
    pub workspaces: Vec<WorkspaceListed>,
}

const LIST_DESCRIPTION: &str = "The registered workspaces: the default first, each with its \
    id, folder, name, whether it is the default, whether this core serves it (only the default \
    is served) and whether its folder is missing. Takes no arguments; workspace.set_default \
    changes the default.";

/// What the workspace tools share.
struct Tools {
    hub: Arc<WorkspaceHub>,
    env: Env,
    probe: Arc<dyn Probe>,
}

const CREATE_DESCRIPTION: &str = "Create a LILITH workspace in a folder, or register a \
    folder that already is one, in suite.toml. A new workspace gets lilith.yaml, the \
    sketchbook project, .gitignore and .gitattributes entries. It becomes the default \
    workspace when `default` is true, or when no default is set; the core serves only the \
    default workspace. Warnings name volumes where files can't be tracked by ID.";

const SET_DEFAULT_DESCRIPTION: &str = "Make a workspace folder (one that holds \
    lilith.yaml) the default workspace, the one the core serves. It is registered in \
    suite.toml if it isn't yet. Sessions in the old default get workspace.unsupported.";

/// Registers `workspace.create` and `workspace.set_default` (module `workspace`).
pub fn register_workspace_tools(
    reg: &mut Registry,
    hub: Arc<WorkspaceHub>,
    env: Env,
    probe: Arc<dyn Probe>,
) {
    let tools = Arc::new(Tools { hub, env, probe });
    let t = tools.clone();
    reg.register(typed_tool(
        "workspace.create",
        CREATE_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        move |ctx, args: WorkspaceCreateArgs| {
            let t = t.clone();
            Box::pin(async move { create(&t, ctx, args).await })
        },
    ));
    let t = tools.clone();
    reg.register(typed_tool(
        "workspace.set_default",
        SET_DEFAULT_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        move |ctx, args: SetDefaultArgs| {
            let t = t.clone();
            Box::pin(async move { set_default(&t, ctx, args).await })
        },
    ));
    let t = tools;
    reg.register(typed_tool(
        "workspaces.list",
        LIST_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        move |ctx, _args: ListArgs| {
            let t = t.clone();
            let ctx_trace = ctx.trace_id.clone();
            Box::pin(async move {
                t.hub.refresh_default();
                let served = t.hub.default_id();
                let env = t.env.clone();
                let workspaces =
                    tokio::task::spawn_blocking(move || listed(&env, served.as_deref()))
                        .await
                        .map_err(|e| {
                            ErrorBody::new(
                                &codes::CORE_BUG,
                                format!(
                                    "Listing the workspaces failed: {e}. This is a bug in LILITH."
                                ),
                                &ctx_trace,
                            )
                        })?
                        .map_err(|why| ErrorBody::new(&codes::STORE_IO, why, &ctx_trace))?;
                let summary = format!("{} workspace(s)", workspaces.len());
                Ok((ListResult { workspaces }, summary))
            })
        },
    ));
}

/// `workspaces.list`'s entries (module docs); `served` is the id of the workspace this core
/// serves. No `suite.toml` registers none; one that can't be read or isn't valid is the
/// message of a `store.io` error, as the workspace tools that write it give (review M4).
fn listed(env: &Env, served: Option<&str>) -> Result<Vec<WorkspaceListed>, String> {
    let file = env.suite_toml_to_read();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!(
                "The workspace registry {} can't be read: {e}.",
                file.display()
            ));
        }
    };
    let suite = SuiteToml::parse(&text).map_err(|e| {
        format!(
            "The workspace registry {} is not valid: {e}. Fix or remove it, or run lil doctor.",
            file.display()
        )
    })?;
    let default = suite
        .default_workspace
        .clone()
        .filter(|p| !p.is_empty())
        .map(|p| folder_key(Path::new(&p)));
    let paths = suite
        .default_workspace
        .iter()
        .chain(suite.workspaces.iter().map(|w| &w.path))
        .filter(|p| !p.is_empty());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for registered in paths {
        let key = folder_key(Path::new(registered));
        if !seen.insert(key.clone()) {
            continue;
        }
        let is_default = default.as_deref() == Some(key.as_str());
        let entry = match std::fs::canonicalize(registered) {
            // A file at the registered path is the wrong type, not missing (review M5).
            Ok(canonical) if !canonical.is_dir() => {
                let path = strip_verbatim(&canonical);
                check_workspace(&path);
                WorkspaceListed {
                    id: Env::workspace_hash(&canonical),
                    path: path.display().to_string(),
                    name: None,
                    default: is_default,
                    served: false,
                    missing: false,
                    problem: Some(WorkspaceProblem::NotAFolder),
                }
            }
            Ok(canonical) => {
                let path = strip_verbatim(&canonical);
                check_workspace(&path);
                let id = Env::workspace_hash(&canonical);
                let name = workspace_file_in(&path)
                    .and_then(|file| std::fs::read_to_string(file).ok())
                    .and_then(|text| read_workspace_yaml(&text).ok())
                    .and_then(|ws| ws.name);
                WorkspaceListed {
                    served: served == Some(id.as_str()),
                    id,
                    path: path.display().to_string(),
                    name,
                    default: is_default,
                    missing: false,
                    problem: None,
                }
            }
            Err(e) => {
                let absolute =
                    std::path::absolute(registered).unwrap_or_else(|_| PathBuf::from(registered));
                let gone = e.kind() == std::io::ErrorKind::NotFound;
                WorkspaceListed {
                    id: Env::workspace_hash(&absolute),
                    path: registered.clone(),
                    name: None,
                    default: is_default,
                    served: false,
                    missing: gone,
                    problem: (!gone).then_some(WorkspaceProblem::Unreadable),
                }
            }
        };
        out.push(entry);
    }
    Ok(out)
}

/// ARCH-094: `Err` (why) when `path` is the install folder or inside it, folders compared with
/// `folder_key`; `Ok` without an install folder (dev and test channels).
///
/// # Errors
/// Why the folder is refused.
pub fn refuse_inside(path: &Path, install_dir: Option<&Path>) -> Result<(), String> {
    let Some(install) = install_dir else {
        return Ok(());
    };
    if is_inside(&folder_key(path), &folder_key(install)) {
        return Err(format!(
            "it is the LILITH install folder \"{}\" or inside it, which updates replace",
            install.display()
        ));
    }
    Ok(())
}

/// Whether the folder key `key` is `base` or below it.
fn is_inside(key: &str, base: &str) -> bool {
    if key == base {
        return true;
    }
    let prefix = if base.ends_with('\\') {
        base.to_string()
    } else {
        format!("{base}\\")
    };
    key.starts_with(&prefix)
}

/// `workspace.path_invalid` for `path`.
fn path_invalid(path: &str, why: &str, trace_id: &str) -> ErrorBody {
    let mut error = ErrorBody::new(
        &codes::WORKSPACE_PATH_INVALID,
        format!("\"{path}\" can't be used as a workspace: {why}."),
        trace_id,
    );
    error.data = Some(json!({ "path": path, "why": why }));
    error
}

/// `path` made absolute (relative paths are taken from the core's folder).
fn absolute(path: &str, trace_id: &str) -> Result<PathBuf, ErrorBody> {
    if path.trim().is_empty() {
        return Err(path_invalid(path, "the path is empty", trace_id));
    }
    std::path::absolute(path).map_err(|e| path_invalid(path, &e.to_string(), trace_id))
}

/// The workspace at the existing folder `abs`: its id and canonical path (without `\\?\`),
/// after the harness guard has seen the canonical path too.
fn resolve(abs: &Path, shown: &str, trace_id: &str) -> Result<WorkspaceRef, ErrorBody> {
    let canonical = std::fs::canonicalize(abs).map_err(|e| {
        path_invalid(
            shown,
            &format!("the folder can't be opened ({e})"),
            trace_id,
        )
    })?;
    let path = strip_verbatim(&canonical);
    check_workspace(&path);
    Ok(WorkspaceRef {
        id: Env::workspace_hash(&canonical),
        path,
    })
}

/// The folders registered in `suite.toml` (none when it is missing or doesn't parse; a write
/// then reports the file itself).
fn registered(env: &Env) -> Vec<String> {
    std::fs::read_to_string(env.suite_toml_to_read())
        .ok()
        .and_then(|text| SuiteToml::parse(&text).ok())
        .map(|suite| {
            suite
                .workspaces
                .into_iter()
                .map(|w| w.path)
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Step 2's refusals for `path`, in this order: a file, the install folder, a folder inside
/// another registered workspace.
fn refusal(path: &Path, env: &Env) -> Option<String> {
    if path.is_file() {
        return Some("it is a file, not a folder".to_string());
    }
    if let Err(why) = refuse_inside(path, env.install_dir().as_deref()) {
        return Some(why);
    }
    let key = folder_key(path);
    registered(env)
        .into_iter()
        .find(|other| {
            let other = folder_key(Path::new(other));
            key != other && is_inside(&key, &other)
        })
        .map(|other| format!("it is inside the registered workspace \"{other}\""))
}

/// The warnings for the workspace folder `path` (Review Focus 4).
pub(crate) fn volume_warnings(probe: &dyn Probe, path: &Path) -> Vec<String> {
    let cloud_attributes = probe
        .identity(path)
        .is_ok_and(|i| i.attributes & (PLACEHOLDER_ATTRIBUTES | CLOUD_PIN_ATTRIBUTES) != 0);
    let (synced, no_ids) = match probe.volume_info(path) {
        Ok(info) => (
            info.remote || info.cloud_sync_root || cloud_attributes,
            !(info.fs_name.eq_ignore_ascii_case("NTFS")
                || info.fs_name.eq_ignore_ascii_case("ReFS")),
        ),
        Err(e) => {
            tracing::info!(folder = %path.display(), error = %e, "the volume facts can't be read; it is served without stable IDs");
            (cloud_attributes, true)
        }
    };
    let mut warnings = Vec::new();
    if synced {
        warnings.push(NETWORK_WARNING.to_string());
    }
    if no_ids {
        warnings.push(NO_FILE_IDS_WARNING.to_string());
    }
    warnings
}

/// `text` with each of `lines` that it lacks appended, in the file's own line ending; the
/// text itself when it has them all.
pub(crate) fn with_lines(text: &str, lines: &[&str]) -> String {
    let have: HashSet<&str> = text.lines().map(str::trim_end).collect();
    let missing: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !have.contains(l))
        .collect();
    if missing.is_empty() {
        return text.to_string();
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }
    for line in missing {
        out.push_str(line);
        out.push_str(eol);
    }
    out
}

fn today(hub: &WorkspaceHub) -> String {
    let secs = hub
        .clock()
        .utc_now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    lil_schema::time::date(secs)
}

/// Step 4: the new workspace's files, through its store, `lilith.yaml` last. Files that
/// exist keep their content (`.gitignore` and `.gitattributes` gain the entries they lack).
async fn write_layout(
    t: &Tools,
    ws: &WorkspaceRef,
    name: &str,
    trace_id: &str,
) -> Result<(), ErrorBody> {
    let open = t.hub.open(ws).await.map_err(|mut e| {
        e.trace_id = trace_id.to_string();
        e
    })?;
    if open.access == Access::ReadOnly {
        return Err(StoreError::ReadOnly.to_error(trace_id));
    }
    let store = &open.store;
    let today = today(&t.hub);
    let rel = |p: &str| WsPath::parse(p).expect("the workspace layout paths are valid");
    let error = |e: StoreError| e.to_error(trace_id);
    let keep_or = |new: String| {
        move |current: &str| -> Result<String, StoreError> {
            Ok(if current.is_empty() {
                new.clone()
            } else {
                current.to_string()
            })
        }
    };
    let project = new_project_yaml(SKETCHBOOK, "Sketchbook", "media", &today);
    store
        .edit_text(&rel("projects/sketchbook/project.yaml"), &keep_or(project))
        .await
        .map_err(error)?;
    let progress = PROGRESS_TEMPLATE.replace("<name>", "Sketchbook");
    store
        .edit_text(&rel("projects/sketchbook/progress.md"), &keep_or(progress))
        .await
        .map_err(error)?;
    store
        .edit_text(&rel(".gitignore"), &|current| {
            Ok(with_lines(current, &GITIGNORE_ENTRIES))
        })
        .await
        .map_err(error)?;
    store
        .edit_text(&rel(".gitattributes"), &|current| {
            Ok(with_lines(current, &[GITATTRIBUTES_ENTRY]))
        })
        .await
        .map_err(error)?;
    store
        .edit_text(
            &rel(WORKSPACE_FILE),
            &keep_or(new_workspace_yaml(name, &today)),
        )
        .await
        .map_err(error)?;
    Ok(())
}

/// After `suite.toml` named `new` the default: `{kind: "default_changed"}` on the `workspace`
/// topic, and the old default is let go of (it closes once no call holds it). Nothing when
/// the default didn't change.
async fn default_switched(
    t: &Tools,
    ctx: &CallCtx,
    old: Option<&WorkspaceRef>,
    new: &WorkspaceRef,
) {
    if old.is_some_and(|o| o.id == new.id) {
        return;
    }
    ctx.events.publish(
        "workspace",
        json!({"kind": "default_changed", "id": new.id, "path": new.path.display().to_string()}),
    );
    if let Some(old) = old {
        t.hub.close(&old.id).await;
    }
}

/// After a failed create: the workspace's open (its `core.lock` and `state.db`) is let go of
/// unless it is the default workspace, which the core serves anyway.
async fn close_unless_default(t: &Tools, ws: &WorkspaceRef) {
    t.hub.refresh_default();
    if t.hub.default_id().as_deref() != Some(ws.id.as_str()) {
        t.hub.close(&ws.id).await;
    }
}

/// `workspace.create`'s body (module docs).
async fn create(
    t: &Tools,
    ctx: &CallCtx,
    args: WorkspaceCreateArgs,
) -> Result<(WorkspaceCreateResult, String), ErrorBody> {
    let trace_id = ctx.trace_id.as_str();
    // 1. Absolute, and seen by the guard before anything exists.
    let abs = absolute(&args.path, trace_id)?;
    check_workspace(&abs);
    // 2.
    if let Some(why) = refusal(&abs, &t.env) {
        return Err(path_invalid(&args.path, &why, trace_id));
    }
    let dir = abs.clone();
    tokio::task::spawn_blocking(move || fsops::ensure_dir(&dir))
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(format!("the call stopped: {e}"))))
        .map_err(|e| {
            path_invalid(
                &args.path,
                &format!("the folder can't be created ({e})"),
                trace_id,
            )
        })?;
    let ws = resolve(&abs, &args.path, trace_id)?;
    if let Some(why) = refusal(&ws.path, &t.env) {
        return Err(path_invalid(&args.path, &why, trace_id));
    }
    let warnings = volume_warnings(t.probe.as_ref(), &ws.path);
    // 3. and 4.
    let created = workspace_file_in(&ws.path).is_none();
    if created {
        let name = args.name.clone().unwrap_or_else(|| {
            ws.path.file_name().map_or_else(
                || "Workspace".to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        });
        if let Err(e) = write_layout(t, &ws, &name, trace_id).await {
            close_unless_default(t, &ws).await;
            return Err(e);
        }
    }
    // 5.
    t.hub.refresh_default();
    let old = t.hub.default_ref();
    let make_default = args.default.unwrap_or(old.is_none());
    if let Err(e) = register_workspace(&t.env, &ws.path, make_default, t.hub.failpoints()).await {
        close_unless_default(t, &ws).await;
        return Err(e.to_error(trace_id));
    }
    t.hub.refresh_default();
    let default = t.hub.default_id().as_deref() == Some(ws.id.as_str());
    if default {
        default_switched(t, ctx, old.as_ref(), &ws).await;
    } else {
        t.hub.close(&ws.id).await;
    }
    let path = ws.path.display().to_string();
    let summary = if created {
        format!("workspace created at {path}")
    } else {
        format!("workspace registered at {path}")
    };
    tracing::info!(workspace = %path, created, default, "workspace.create");
    Ok((
        WorkspaceCreateResult {
            id: ws.id,
            path,
            created,
            default,
            warnings,
        },
        summary,
    ))
}

/// `workspace.set_default`'s body (module docs).
async fn set_default(
    t: &Tools,
    ctx: &CallCtx,
    args: SetDefaultArgs,
) -> Result<(SetDefaultResult, String), ErrorBody> {
    let trace_id = ctx.trace_id.as_str();
    let abs = absolute(&args.path, trace_id)?;
    check_workspace(&abs);
    if workspace_file_in(&abs).is_none() {
        return Err(path_invalid(
            &args.path,
            &format!("it has no {WORKSPACE_FILE}; `lil workspace init` creates a workspace"),
            trace_id,
        ));
    }
    let ws = resolve(&abs, &args.path, trace_id)?;
    t.hub.refresh_default();
    let old = t.hub.default_ref();
    register_workspace(&t.env, &ws.path, true, t.hub.failpoints())
        .await
        .map_err(|e| e.to_error(trace_id))?;
    t.hub.refresh_default();
    default_switched(t, ctx, old.as_ref(), &ws).await;
    let path = ws.path.display().to_string();
    tracing::info!(workspace = %path, "workspace.set_default");
    let summary = format!("default workspace: {path}");
    Ok((SetDefaultResult { id: ws.id, path }, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-094
    fn refuse_inside_rejects_the_install_folder_and_below() {
        let install = Path::new(r"C:\Programs\LILITHSuite");
        for refused in [
            r"C:\Programs\LILITHSuite",
            r"C:\Programs\LILITHSuite\",
            r"C:\Programs\LILITHSuite\ws",
            r"C:\Programs\LILITHSuite\a\Ünïcödé space",
            r"c:\programs\lilithsuite\Ws",
        ] {
            let why = refuse_inside(Path::new(refused), Some(install))
                .expect_err(&format!("{refused} was accepted"));
            assert!(why.contains("install folder"), "{why}");
        }
        for accepted in [
            r"C:\Programs\LILITHSuite2",
            r"C:\Programs\Other",
            r"C:\Programs",
            r"D:\LILITHSuite",
        ] {
            assert_eq!(
                refuse_inside(Path::new(accepted), Some(install)),
                Ok(()),
                "{accepted}"
            );
        }
        assert_eq!(refuse_inside(install, None), Ok(()));
    }

    #[test] // spec: CORE-040
    fn with_lines_appends_only_what_is_missing_in_the_files_line_ending() {
        assert_eq!(with_lines("", &["a", "b"]), "a\nb\n");
        assert_eq!(with_lines("a\n", &["a", "b"]), "a\nb\n");
        assert_eq!(with_lines("x", &["a"]), "x\na\n");
        assert_eq!(with_lines("x\r\ny\r\n", &["a"]), "x\r\ny\r\na\r\n");
        assert_eq!(with_lines("a  \r\nb\r\n", &["a", "b"]), "a  \r\nb\r\n");
    }

    #[test] // spec: CORE-040, ARCH-096
    fn gitignore_entries_name_lilith() {
        use lil_platform::names::{LEGACY_WORKSPACE_DIR, WORKSPACE_DIR};
        assert_eq!(GITIGNORE_ENTRIES[0], ".lilith/");
        assert_eq!(GITIGNORE_ENTRIES[0], format!("{WORKSPACE_DIR}/"));
        assert!(
            !GITIGNORE_ENTRIES.contains(&format!("{LEGACY_WORKSPACE_DIR}/").as_str()),
            "the legacy folder is never written into a new .gitignore"
        );
        // A legacy workspace's `.gitignore` keeps its old line and only gains the new one.
        let legacy = format!("{LEGACY_WORKSPACE_DIR}/\n.trash/\n");
        let out = with_lines(&legacy, &GITIGNORE_ENTRIES);
        assert!(out.starts_with(&legacy), "{out}");
        assert!(out.lines().any(|l| l == ".lilith/"), "{out}");
    }
}
