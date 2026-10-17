//! The asset write tools (Global Constraints, Product tools; CORE-019, CORE-020, CORE-023,
//! CORE-026, CORE-027, CORE-029, CORE-031, CORE-032, CORE-047, CORE-051, CORE-053, ARCH-042,
//! ARCH-060), module `workspace`. They work in the session's workspace (`workspace.required`
//! without one), in the call's project (`CallCtx::project` when `project` is omitted), which
//! must exist (`project.unknown`).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `asset.import` | write | `{project?, asset_id, type, files: [path], mode: "copy"\|"link_external"}` | `{ref, run_id?, handoff, files}` |
//! | `asset.collect` | write | `{project?, asset_id}` | `{run_id, handoff, files}` |
//! | `asset.signoff` | write | `{project?, asset_id, run_id, files: [name], bump?, keep_runs?}` | `{ref, version, handoff}` |
//! | `asset.request_signoff` | write | `{project?, asset_id}` | `{status}` |
//! | `asset.pin_input` | write | `{project?, asset_id, ref, reason}` | `{pinned_inputs}` |
//!
//! - **`asset.import`, `copy`**: an asset without a manifest gets `Manifest::new` with a fresh
//!   `creation_uuid` (CORE-047). Each file (an absolute path) is copied into a staging handle
//!   with `Store::stage_copy` (never over a file) and the staged copy is hashed; the call then
//!   leaves a candidate-run [`RecordPlan`] (run id from `Store::next_run_id`, manifest edit
//!   [`start_candidate_run`], each run file with that `sha256`). The Record stage commits it.
//!   The asset's version lock is held from the run id to the end of the Record stage, so two
//!   imports of one asset in the same minute get different run ids.
//!   `ref` is the first file's candidate ref, `handoff.refs` every file's.
//! - **`asset.import`, `link_external`**: each file is hashed where it is and recorded in
//!   `external: [{path, sha256}]` with a manifest edit; nothing is copied and no run is
//!   started; a `planned` asset goes `in_progress`. `ref` is the asset's ref without a
//!   version; `handoff.refs` is empty.
//! - **`asset.collect`** re-hashes each external file (a difference is `ref.hash_mismatch`),
//!   copies them in as a candidate run like `copy`, and removes `external` (the other keys
//!   keep their order).
//! - **`asset.signoff`**: an asset with `external` files is `asset.link_external` (fix
//!   "Collect into workspace", agent hint `asset.collect`). Otherwise, holding the asset's
//!   version lock from here to the end of the Record stage: run files of a pre-2b run without
//!   a recorded `sha256` are hashed, [`plan_signoff`] gives the manifest after the sign-off
//!   and the version, each chosen file is hashed again and must match its recorded `sha256`
//!   (`store.hash_conflict` with `data {path, expected, found}` otherwise, before anything
//!   moves), `Store::seal` seals it with that manifest as the snapshot, and the call
//!   leaves a sealed-version plan that carries the guard. Its manifest edit applies
//!   `plan_signoff` again to the manifest as it is then, writes `hashes` for every sealed file
//!   (derived and `source/**` files too, ruling Q17), and fails with `store.hash_conflict`
//!   unless the chosen files' hashes agree with the sealed ones and the `v<version>/` keys of
//!   `hashes` are exactly the sealed files. The sealed files (and the snapshot) reach the index
//!   through `commit.moved`, after the manifest edit. `ref` carries the version's content hash
//!   (ARCH-042). Sign-off isn't destructive, so an agent may do it (CORE-051).
//! - **`asset.request_signoff`** moves the status to `in_review` (CORE-053) with a manifest
//!   edit; an asset already `in_review` is left as it is, and an `approved`, `exported` or
//!   `released` asset with an open (kept) run takes `CandidateRun` first (ruling Q23). It is not a Record branch, so it
//!   writes no `progress.md` entry.
//! - **`asset.pin_input`** needs a ref that parses (`ref.invalid`; a candidate ref is never an
//!   input), whose target need not exist on this machine. It pushes `{ref, reason, by, date}`
//!   to `pinned_inputs` (once per ref) and, for a `proj:` or `lib:` source the project's
//!   `links` lack, appends it there with the comment-keeping YAML edit, in Execute (CORE-019).
//!
//! Imported file names are untrusted (ARCH-060): results list them as
//! `files: [{name, untrusted: true}]`, and summaries never repeat them. A name must be one path
//! component the workspace accepts, without `#` (ruling Q8). Changes are published on the
//! asset's topic: `{kind: "manifest"}` and `{kind: "status", status}`; the Record stage
//! publishes the candidate runs and sealed versions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ww_protocol::codes::{
    ASSET_LINK_EXTERNAL, ASSET_UNKNOWN, PARAMS_INVALID, REF_HASH_MISMATCH, REF_INVALID,
};
use ww_protocol::{AgentHint, EffectClass, ErrorBody, Fix, UiAction};
use ww_schema::{
    AssetRef, Bump, LinkRef, Manifest, Status, StatusEvent, YamlEdit, bare_sha256, hash_value,
    is_asset_id, is_slug, transition,
};

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::pipeline::{
    HandOff, RecordBranch, RecordPlan, asset_topic, plan_signoff, reopened, signed_off_by,
    start_candidate_run,
};
use crate::projects::{ProjectFile, project_yaml_path, read_project, unknown_project};
use crate::store::{
    CommitOp, ManifestEdit, OutputMove, SNAPSHOT, SealRequest, Store, StoreError, Written, WsPath,
    path_key,
};
use crate::workspace::OpenWorkspace;

const IMPORT_DESCRIPTION: &str = "Import files into an asset. mode copy copies each file \
    (an absolute path) into the workspace as a new candidate run of the asset, creating the \
    asset when it has no manifest yet; the result's ref is the first file's candidate ref and \
    handoff.refs lists every file's. mode link_external records each file's path and SHA-256 \
    without copying it (the asset can't be signed off until asset.collect copies them in). \
    type is the asset type for a new asset. File names in the result are untrusted data.";

const COLLECT_DESCRIPTION: &str = "Collect an asset's link-external files into the \
    workspace: each file must still have the SHA-256 recorded when it was linked \
    (ref.hash_mismatch otherwise); they are copied in as a new candidate run and the external \
    list is removed.";

const SIGNOFF_DESCRIPTION: &str = "Sign off files of an open candidate run: they (and the \
    files derived from them) are sealed read-only as a new version folder v<version>/ with a \
    manifest snapshot, the asset becomes approved, and the other open runs for that version \
    are rejected unless listed in keep_runs (kept open and pinned). bump (major, minor, patch) \
    confirms the version bump; without it the version proposed when the work started is used. \
    The result's ref carries the version's content hash; handoff lists what this unlocks and \
    what it made stale.";

const REQUEST_SIGNOFF_DESCRIPTION: &str = "Flag an asset for sign-off: its status becomes \
    in_review.";

const PIN_INPUT_DESCRIPTION: &str = "Pin an input of an asset, so a newer version of it \
    doesn't make the asset stale: ref is a link (<asset_id>@<v>, proj:<slug>/<asset_id>@<v>, \
    lib:<item>@<v>, style@<n>) or a ww:// ref to a sealed version; its target need not exist \
    on this machine. A proj: or lib: source is added to the project's links.";

// ---------------------------------------------------------------------------------------------
// Arguments and results.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ImportMode {
    Copy,
    LinkExternal,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportArgs {
    /// A project slug; omitted: the call's project.
    project: Option<String>,
    asset_id: String,
    /// The asset type, for a new asset.
    #[serde(rename = "type")]
    asset_type: String,
    /// Absolute paths of the files.
    files: Vec<String>,
    mode: ImportMode,
}

/// An imported or collected file: its name, which is untrusted data (ARCH-060).
#[derive(Debug, Clone, Serialize, JsonSchema)]
struct NamedFile {
    name: String,
    untrusted: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ImportResult {
    #[serde(rename = "ref")]
    r#ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    handoff: HandOff,
    files: Vec<NamedFile>,
    /// What failed after the manifest recorded the operation (ruling Q31).
    warnings: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssetArgs {
    /// A project slug; omitted: the call's project.
    project: Option<String>,
    asset_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CollectResult {
    run_id: String,
    handoff: HandOff,
    files: Vec<NamedFile>,
    /// What failed after the manifest recorded the operation (ruling Q31).
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum BumpArg {
    Major,
    Minor,
    Patch,
}

impl From<BumpArg> for Bump {
    fn from(b: BumpArg) -> Bump {
        match b {
            BumpArg::Major => Bump::Major,
            BumpArg::Minor => Bump::Minor,
            BumpArg::Patch => Bump::Patch,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SignoffArgs {
    /// A project slug; omitted: the call's project.
    project: Option<String>,
    asset_id: String,
    run_id: String,
    /// The chosen file names in the run.
    files: Vec<String>,
    /// The confirmed bump; omitted: the proposed version.
    bump: Option<BumpArg>,
    /// Other open runs to keep open (pinned) instead of rejecting them.
    #[serde(default)]
    keep_runs: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SignoffResult {
    #[serde(rename = "ref")]
    r#ref: String,
    version: String,
    handoff: HandOff,
    /// What failed after the manifest recorded the operation (ruling Q31).
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct StatusResult {
    status: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PinArgs {
    /// A project slug; omitted: the call's project.
    project: Option<String>,
    asset_id: String,
    #[serde(rename = "ref")]
    r#ref: String,
    reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct PinResult {
    pinned_inputs: Value,
}

// ---------------------------------------------------------------------------------------------
// Shared steps.
// ---------------------------------------------------------------------------------------------

/// The asset a call works on.
struct Asset {
    open: Arc<OpenWorkspace>,
    project: String,
    asset_id: String,
    /// `projects/<project>/assets/<asset_id>`.
    dir: WsPath,
    /// `<dir>/<asset_id>.manifest.json`.
    manifest: WsPath,
}

impl Asset {
    fn store(&self) -> &Arc<Store> {
        &self.open.store
    }

    fn topic(&self) -> String {
        asset_topic(&self.open.ws.id, &self.project, &self.asset_id)
    }
}

fn invalid(ctx: &CallCtx, why: String) -> ErrorBody {
    ErrorBody::new(&PARAMS_INVALID, why, ctx.trace_id.clone())
}

fn traced(ctx: &CallCtx, mut e: ErrorBody) -> ErrorBody {
    e.trace_id.clone_from(&ctx.trace_id);
    e
}

fn unknown_asset(ctx: &CallCtx, a: &Asset) -> ErrorBody {
    ErrorBody::new(
        &ASSET_UNKNOWN,
        format!(
            "There is no asset `{}` in the project `{}`; assets.query lists the assets there \
             are.",
            a.asset_id, a.project
        ),
        ctx.trace_id.clone(),
    )
}

/// The call's asset: names checked, the project must exist, and no folder on the way to the
/// asset may be a link.
async fn asset_of(
    ctx: &CallCtx,
    project: Option<String>,
    asset_id: &str,
) -> Result<Asset, ErrorBody> {
    let open = ctx.workspace()?.clone();
    let project = project.unwrap_or_else(|| ctx.project_or_default());
    if !is_slug(&project) {
        return Err(invalid(ctx, format!("{project:?} is not a project slug")));
    }
    if !is_asset_id(asset_id) {
        return Err(invalid(
            ctx,
            format!(
                "{asset_id:?} is not an asset id: a lowercase letter, then lowercase letters, \
                 digits and '_', at most 63 characters"
            ),
        ));
    }
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    if read_project(&open.store, &project).await.map_err(error)? == ProjectFile::Missing {
        return Err(traced(ctx, unknown_project(&project)));
    }
    let dir = WsPath::parse(&format!("projects/{project}/assets/{asset_id}")).map_err(error)?;
    let manifest = dir
        .join(&format!("{asset_id}.manifest.json"))
        .map_err(error)?;
    open.store.refuse_links(&dir).await.map_err(error)?;
    Ok(Asset {
        open,
        project,
        asset_id: asset_id.to_string(),
        dir,
        manifest,
    })
}

/// The manifest at `rel`, parsed; `None` when there is none. One that doesn't parse is
/// `manifest.needs_repair`.
async fn read_manifest(store: &Store, rel: &WsPath) -> Result<Option<Manifest>, StoreError> {
    let Some(bytes) = store.read(rel).await? else {
        return Ok(None);
    };
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|t| Manifest::parse(t).ok())
        .map(|(m, _)| Some(m))
        .ok_or_else(|| StoreError::NeedsRepair(rel.clone()))
}

/// The name a file at `path` gets in the workspace: its last component, which must be one the
/// workspace accepts and hold no `#` (ruling Q8: candidate refs can't name it otherwise).
fn workspace_name(ctx: &CallCtx, path: &Path) -> Result<String, ErrorBody> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .ok_or_else(|| invalid(ctx, "a file to import has no usable file name".to_string()))?;
    if name.contains('#') || WsPath::parse(&name).is_err() {
        return Err(invalid(
            ctx,
            "a file name to import holds a character or form the workspace refuses ('#', \
             a reserved name, a trailing dot or space, …); rename the file first"
                .to_string(),
        ));
    }
    Ok(name)
}

/// The sources named by `paths` (absolute), with their workspace names, which must differ
/// (without regard to case).
fn sources(ctx: &CallCtx, paths: &[String]) -> Result<Vec<(PathBuf, String)>, ErrorBody> {
    if paths.is_empty() {
        return Err(invalid(ctx, "files lists at least one file".to_string()));
    }
    let mut seen = BTreeSet::new();
    let mut found = Vec::with_capacity(paths.len());
    for p in paths {
        let path = PathBuf::from(p);
        if !path.is_absolute() {
            return Err(invalid(
                ctx,
                "every file is named by its absolute path".to_string(),
            ));
        }
        let name = workspace_name(ctx, &path)?;
        if !seen.insert(path_key(&name)) {
            return Err(invalid(
                ctx,
                "two files to import have the same name; a run holds each name once".to_string(),
            ));
        }
        found.push((path, name));
    }
    Ok(found)
}

fn untrusted(names: &[String]) -> Vec<NamedFile> {
    names
        .iter()
        .map(|n| NamedFile {
            name: n.clone(),
            untrusted: true,
        })
        .collect()
}

/// Copies `files` into a staging handle and leaves a candidate-run plan for the Record stage.
/// `extra` runs on the manifest after the run has started (`asset.collect` removes `external`).
/// Returns the run id, the files' candidate refs and their names.
async fn stage_candidate_run(
    ctx: &CallCtx,
    a: &Asset,
    asset_type: &str,
    files: &[(PathBuf, String)],
    expected: Option<&HashMap<String, String>>,
    remove_external: bool,
) -> Result<(String, Vec<String>, Vec<String>), ErrorBody> {
    let store = a.store().clone();
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    // The asset's version lock from the run id to the end of the Record stage: two runs of
    // one asset started in the same minute never take the same id.
    let guard = store
        .version_lock(&a.project, &a.asset_id)
        .lock_owned()
        .await;
    let current = read_manifest(&store, &a.manifest).await.map_err(error)?;
    let now = store.now_utc();
    let run_id = store
        .next_run_id(&a.dir, current.as_ref(), now)
        .await
        .map_err(error)?;
    let uuid = store.ids().uuid();
    let handle = store.staging_handle().await.map_err(error)?;
    let mut outputs = Vec::with_capacity(files.len());
    let mut records = Vec::with_capacity(files.len());
    let mut refs = Vec::with_capacity(files.len());
    let mut names = Vec::with_capacity(files.len());
    for (source, name) in files {
        let staged = store.stage_copy(&handle, source, name).await;
        let (staged, sha256) = match staged {
            Ok(staged) => staged,
            Err(e) => {
                // What was staged stays in the handle as an orphan; nothing is deleted.
                let _ = store.release_handle(handle).await;
                return Err(error(e));
            }
        };
        if let Some(expected) = expected
            && expected.get(name) != Some(&sha256)
        {
            let _ = store.release_handle(handle).await;
            let mut e = ErrorBody::new(
                &REF_HASH_MISMATCH,
                "A link-external file changed while it was being collected; check it and \
                 link it again.",
                ctx.trace_id.clone(),
            );
            e.data = Some(json!({ "path": source.display().to_string() }));
            return Err(e);
        }
        let to = a
            .dir
            .join(&format!("candidates/{run_id}/{name}"))
            .map_err(error)?;
        outputs.push(OutputMove { from: staged, to });
        records.push(json!({
            "path": format!("candidates/{run_id}/{name}"),
            "role": "candidate",
            "sha256": sha256,
        }));
        refs.push(
            AssetRef::Candidate {
                ws: a.open.ws.id.clone(),
                project: a.project.clone(),
                asset: a.asset_id.clone(),
                run: run_id.clone(),
                file: name.clone(),
            }
            .to_string(),
        );
        names.push(name.clone());
    }
    let run = json!({
        "run_id": run_id,
        "path": format!("candidates/{run_id}/"),
        "status": "open",
        "created": ww_schema::time::rfc3339(now),
        "target_version": null,
        "files": records,
        "selected": null,
        "signed_off": null,
        "pinned": false,
    });
    let (project, asset_id, asset_type) = (
        a.project.clone(),
        a.asset_id.clone(),
        asset_type.to_string(),
    );
    let edit: ManifestEdit = Arc::new(move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            *m = Manifest::new(&project, &asset_id, &asset_type, &uuid);
        }
        let Value::Object(run) = run.clone() else {
            unreachable!("the run is an object");
        };
        start_candidate_run(m, run, now)?;
        if remove_external {
            m.remove("external");
        }
        Ok(())
    });
    let entry = if remove_external {
        format!(
            "collected {} link-external file(s) into {} as candidate run {run_id}",
            files.len(),
            a.asset_id
        )
    } else {
        format!(
            "imported {} file(s) into {} as candidate run {run_id}",
            files.len(),
            a.asset_id
        )
    };
    ctx.record.set(RecordPlan {
        branch: RecordBranch::CandidateRun {
            run_id: run_id.clone(),
        },
        project: a.project.clone(),
        asset_id: a.asset_id.clone(),
        commit: CommitOp {
            outputs,
            moved: Vec::new(),
            removed: Vec::new(),
            manifest: Some((a.manifest.clone(), edit)),
            job_line: None,
            progress: None,
        },
        progress_entry: entry,
        refs: refs.clone(),
        version_guard: Some(guard),
    });
    Ok((run_id, refs, names))
}

// ---------------------------------------------------------------------------------------------
// The tools.
// ---------------------------------------------------------------------------------------------

async fn import(ctx: &CallCtx, args: ImportArgs) -> Result<(ImportResult, String), ErrorBody> {
    let a = asset_of(ctx, args.project, &args.asset_id).await?;
    let asset_type = args.asset_type.trim();
    if asset_type.is_empty() || asset_type.chars().any(char::is_control) {
        return Err(invalid(ctx, "type is one non-empty line".to_string()));
    }
    let files = sources(ctx, &args.files)?;
    match args.mode {
        ImportMode::Copy => {
            let (run_id, refs, names) =
                stage_candidate_run(ctx, &a, asset_type, &files, None, false).await?;
            let summary = format!(
                "importing {} file(s) into {} as candidate run {run_id}",
                files.len(),
                a.asset_id
            );
            Ok((
                ImportResult {
                    r#ref: refs[0].clone(),
                    run_id: Some(run_id),
                    handoff: HandOff {
                        refs,
                        ..HandOff::default()
                    },
                    files: untrusted(&names),
                    warnings: Vec::new(),
                },
                summary,
            ))
        }
        ImportMode::LinkExternal => link_external(ctx, &a, asset_type, &files).await,
    }
}

/// `asset.import` with `mode: link_external` (module docs).
async fn link_external(
    ctx: &CallCtx,
    a: &Asset,
    asset_type: &str,
    files: &[(PathBuf, String)],
) -> Result<(ImportResult, String), ErrorBody> {
    let store = a.store();
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let mut linked = Vec::with_capacity(files.len());
    for (source, _) in files {
        let sha256 = store.hash_outside(source).await.map_err(error)?;
        linked.push((source.display().to_string(), sha256));
    }
    let uuid = store.ids().uuid();
    let (project, asset_id, asset_type) = (
        a.project.clone(),
        a.asset_id.clone(),
        asset_type.to_string(),
    );
    let edit = move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            *m = Manifest::new(&project, &asset_id, &asset_type, &uuid);
        }
        let mut list = match m.get("external") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(list)) => list.clone(),
            Some(_) => {
                return Err(StoreError::NeedsRepair(WsPath::from_disk(format!(
                    "projects/{project}/assets/{asset_id}/{asset_id}.manifest.json"
                ))));
            }
        };
        for (path, sha256) in &linked {
            let entry = json!({"path": path, "sha256": sha256});
            match list.iter_mut().find(|e| {
                e.get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|p| p.eq_ignore_ascii_case(path))
            }) {
                Some(existing) => *existing = entry,
                None => list.push(entry),
            }
        }
        m.set("external", Value::Array(list));
        if m.view().status.unwrap_or(Status::Planned) == Status::Planned {
            let moved = transition(Status::Planned, None, None, &StatusEvent::StartWork)
                .map_err(StoreError::InvalidTransition)?;
            m.set("status", Value::from(moved.to.as_str()));
        }
        Ok(())
    };
    store
        .edit_manifest(&a.manifest, &edit)
        .await
        .map_err(error)?;
    ctx.events.publish(&a.topic(), json!({"kind": "manifest"}));
    let names: Vec<String> = files.iter().map(|(_, n)| n.clone()).collect();
    let summary = format!(
        "linked {} external file(s) to {}; collect them before sign-off",
        files.len(),
        a.asset_id
    );
    Ok((
        ImportResult {
            r#ref: format!("ww://{}/{}/{}", a.open.ws.id, a.project, a.asset_id),
            run_id: None,
            handoff: HandOff::default(),
            files: untrusted(&names),
            warnings: Vec::new(),
        },
        summary,
    ))
}

async fn collect(ctx: &CallCtx, args: AssetArgs) -> Result<(CollectResult, String), ErrorBody> {
    let a = asset_of(ctx, args.project, &args.asset_id).await?;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let Some(m) = read_manifest(a.store(), &a.manifest).await.map_err(error)? else {
        return Err(unknown_asset(ctx, &a));
    };
    let view = m.view();
    if view.external.is_empty() {
        return Err(invalid(
            ctx,
            format!("`{}` has no link-external files to collect", a.asset_id),
        ));
    }
    let paths: Vec<String> = view.external.iter().map(|e| e.path.clone()).collect();
    let files = sources(ctx, &paths)?;
    let mut expected = HashMap::new();
    for ((source, name), recorded) in files.iter().zip(&view.external) {
        let found = a.store().hash_outside(source).await.map_err(error)?;
        if found != recorded.sha256 {
            let mut e = ErrorBody::new(
                &REF_HASH_MISMATCH,
                "A link-external file changed since it was linked: its SHA-256 differs from \
                 the recorded one. Check the file, then link it again.",
                ctx.trace_id.clone(),
            );
            e.data = Some(json!({
                "path": recorded.path,
                "expected": recorded.sha256,
                "found": found,
            }));
            return Err(e);
        }
        expected.insert(name.clone(), found);
    }
    let asset_type = view.asset_type.unwrap_or_else(|| "file".to_string());
    let (run_id, refs, names) =
        stage_candidate_run(ctx, &a, &asset_type, &files, Some(&expected), true).await?;
    let summary = format!(
        "collecting {} file(s) into {} as candidate run {run_id}",
        files.len(),
        a.asset_id
    );
    Ok((
        CollectResult {
            run_id,
            handoff: HandOff {
                refs,
                ..HandOff::default()
            },
            files: untrusted(&names),
            warnings: Vec::new(),
        },
        summary,
    ))
}

/// `asset.link_external` for an asset that still links files outside the workspace.
fn link_external_refusal(ctx: &CallCtx, a: &Asset) -> ErrorBody {
    let mut e = ErrorBody::new(
        &ASSET_LINK_EXTERNAL,
        format!(
            "`{}` links files outside the workspace; collect them into the workspace before \
             signing it off.",
            a.asset_id
        ),
        ctx.trace_id.clone(),
    );
    e.fix = Some(Fix {
        ui_action: UiAction {
            id: "asset.collect".to_string(),
            label: "Collect into workspace".to_string(),
        },
        agent_hint: Some(AgentHint {
            tool: "asset.collect".to_string(),
            arguments: json!({"project": a.project, "asset_id": a.asset_id}),
        }),
    });
    e
}

/// `m` with each listed run file record that has no `sha256` given the hash in `filled` (by
/// bare name).
fn with_hashes(m: &Manifest, run_id: &str, filled: &HashMap<String, String>) -> Manifest {
    let mut out = m.clone();
    if filled.is_empty() {
        return out;
    }
    for run in out.runs_mut() {
        let id = run
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !id.eq_ignore_ascii_case(run_id) {
            continue;
        }
        let Some(Value::Array(files)) = run.get_mut("files") else {
            continue;
        };
        for file in files.iter_mut().filter_map(Value::as_object_mut) {
            let name = file
                .get("path")
                .and_then(Value::as_str)
                .map(|p| p.rsplit('/').next().unwrap_or(p).to_string());
            let missing = file.get("sha256").is_none_or(Value::is_null);
            if let (Some(name), true) = (name, missing)
                && let Some(hex) = filled.get(&path_key(&name))
            {
                file.insert("sha256".into(), Value::from(hex.as_str()));
            }
        }
    }
    out
}

/// The hashes of the chosen files of run `run_id` whose records have none (a pre-2b run),
/// keyed by the bare name's `path_key`.
async fn hash_unrecorded(
    a: &Asset,
    m: &Manifest,
    run_id: &str,
    files: &[String],
) -> Result<HashMap<String, String>, StoreError> {
    let mut filled = HashMap::new();
    let view = m.view();
    let Some(run) = view
        .candidate_runs
        .iter()
        .find(|r| r.run_id.eq_ignore_ascii_case(run_id))
    else {
        return Ok(filled);
    };
    for record in run.files.iter().filter(|f| f.sha256.is_none()) {
        let name = record.path.rsplit('/').next().unwrap_or(&record.path);
        if !files.iter().any(|f| f.eq_ignore_ascii_case(name)) {
            continue;
        }
        let Ok(rel) = a.dir.join(&format!("candidates/{}/{name}", run.run_id)) else {
            continue;
        };
        if let Some(hex) = a.store().hash(&rel).await? {
            filled.insert(path_key(name), hex);
        }
    }
    Ok(filled)
}

/// The store error a sign-off's re-applied manifest edit fails with.
fn replanned_error(rel: &WsPath, e: &ErrorBody) -> StoreError {
    match e.code.as_str() {
        "status.invalid_transition" => StoreError::InvalidTransition(e.message.clone()),
        "version.exists" => StoreError::VersionExists(
            e.data
                .as_ref()
                .and_then(|d| d["version"].as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        _ => {
            tracing::warn!(path = %rel, code = %e.code, why = %e.message, "the manifest changed during a sign-off");
            conflict(rel)
        }
    }
}

fn conflict(rel: &WsPath) -> StoreError {
    StoreError::HashConflict {
        path: rel.clone(),
        backup: None,
        cloud_only: false,
        kept: Vec::new(),
    }
}

async fn signoff(ctx: &CallCtx, args: SignoffArgs) -> Result<(SignoffResult, String), ErrorBody> {
    let a = asset_of(ctx, args.project, &args.asset_id).await?;
    let store = a.store().clone();
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let Some(m) = read_manifest(&store, &a.manifest).await.map_err(error)? else {
        return Err(unknown_asset(ctx, &a));
    };
    if !m.view().external.is_empty() {
        return Err(link_external_refusal(ctx, &a));
    }
    // From here to the end of the Record stage (CORE-026).
    let guard = store
        .version_lock(&a.project, &a.asset_id)
        .lock_owned()
        .await;
    // Read again under the lock: a sign-off that held it before has recorded its version.
    let Some(m) = read_manifest(&store, &a.manifest).await.map_err(error)? else {
        return Err(unknown_asset(ctx, &a));
    };
    if !m.view().external.is_empty() {
        return Err(link_external_refusal(ctx, &a));
    }
    // The seal moves files out of the run folder: no folder on the way may be a link.
    if let Ok(run_dir) = a.dir.join(&format!("candidates/{}", args.run_id)) {
        store.refuse_links(&run_dir).await.map_err(error)?;
    }
    let filled = hash_unrecorded(&a, &m, &args.run_id, &args.files)
        .await
        .map_err(error)?;
    let by = signed_off_by(&ctx.session.caller);
    let now = store.now_utc();
    let bump = args.bump.map(Bump::from);
    let (planned, version) = plan_signoff(
        &with_hashes(&m, &args.run_id, &filled),
        &args.run_id,
        &args.files,
        bump,
        &args.keep_runs,
        by,
        now,
    )
    .map_err(|e| traced(ctx, e))?;
    let run = planned
        .view()
        .candidate_runs
        .into_iter()
        .find(|r| r.run_id.eq_ignore_ascii_case(&args.run_id))
        .expect("plan_signoff found the run");
    // Before anything moves: each chosen file must still be what the run recorded. A file
    // edited outside the suite is refused here (the fast path), and the seal checks again in
    // its sealing folder, so a save that lands in between is never sealed either.
    let mut expected = BTreeMap::new();
    for name in &run.selected {
        let Some(hex) = run
            .files
            .iter()
            .find(|f| f.path.rsplit('/').next() == Some(name.as_str()))
            .and_then(|f| f.sha256.clone())
        else {
            continue;
        };
        let rel = a
            .dir
            .join(&format!("candidates/{}/{name}", run.run_id))
            .map_err(error)?;
        // A missing file is the seal's to report.
        if let Some(found) = store.hash(&rel).await.map_err(error)?
            && found != hex
        {
            return Err(error(StoreError::Changed {
                path: rel,
                expected: hex,
                found,
            }));
        }
        expected.insert(name.clone(), hex);
    }
    let sealed = store
        .seal(
            &guard,
            SealRequest {
                project: a.project.clone(),
                asset_id: a.asset_id.clone(),
                version,
                run_id: run.run_id.clone(),
                files: run.selected.clone(),
                snapshot: planned.clone(),
                expected,
            },
        )
        .await
        .map_err(error)?;

    // The sealed files (and the snapshot) reach the index after the manifest edit (ARCH-075).
    let identities: HashMap<&str, _> = sealed
        .identities
        .iter()
        .map(|(p, id)| (p.as_str(), *id))
        .collect();
    let mut moved = Vec::with_capacity(sealed.files.len() + 1);
    // The snapshot as it is in the version (an adopted seal's is its first attempt's).
    let snapshot_sha = sealed.snapshot_sha256.clone();
    let snapshot_key = format!("v{version}/{SNAPSHOT}");
    for (key, sha256) in sealed
        .files
        .iter()
        .map(|(k, h)| (k.as_str(), h.as_str()))
        .chain([(snapshot_key.as_str(), snapshot_sha.as_str())])
    {
        let path = WsPath::from_disk(format!("{}/{key}", a.dir));
        if let Some(identity) = identities.get(path.as_str()) {
            moved.push(Written {
                path,
                sha256: sha256.to_string(),
                identity: *identity,
            });
        }
    }

    let prefix = format!("v{version}/");
    let sealed_files: Vec<(String, String)> = sealed.files.clone();
    // An adopted seal was signed off when it was sealed: the manifest takes the snapshot's
    // date (ruling Q34).
    let sealed_on = sealed.signed_off_date.clone().filter(|_| sealed.adopted);
    let (rel, run_id, files, keep) = (
        a.manifest.clone(),
        run.run_id.clone(),
        args.files.clone(),
        args.keep_runs.clone(),
    );
    let edit: ManifestEdit = Arc::new(move |m: &mut Manifest| {
        let (mut out, again) = plan_signoff(
            &with_hashes(m, &run_id, &filled),
            &run_id,
            &files,
            bump,
            &keep,
            by,
            now,
        )
        .map_err(|e| replanned_error(&rel, &e))?;
        if again != version {
            return Err(conflict(&rel));
        }
        // Ruling Q17: the chosen files are a subset of the sealed ones, with the same hashes;
        // then every sealed file goes into `hashes`, and the `v<version>/` keys must be
        // exactly the sealed files (a stray key there is a conflict).
        let mut hashes: Map<String, Value> = match out.get("hashes") {
            Some(Value::Object(h)) => h.clone(),
            _ => Map::new(),
        };
        let chosen: Vec<String> = out
            .view()
            .candidate_runs
            .into_iter()
            .find(|r| r.run_id.eq_ignore_ascii_case(&run_id))
            .map(|r| r.selected)
            .unwrap_or_default();
        for name in &chosen {
            let key = format!("{prefix}{name}");
            let recorded = hashes
                .get(&key)
                .and_then(Value::as_str)
                .and_then(bare_sha256);
            let agrees = sealed_files
                .iter()
                .any(|(path, hex)| *path == key && recorded == Some(hex.as_str()));
            if !agrees {
                tracing::warn!(path = %rel, file = %key, "a chosen file's hash differs from the sealed file");
                return Err(conflict(&rel));
            }
        }
        for (path, hex) in &sealed_files {
            hashes.insert(path.clone(), Value::from(hash_value(hex)));
        }
        let recorded: BTreeSet<&str> = hashes
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<&str> = sealed_files.iter().map(|(p, _)| p.as_str()).collect();
        if recorded != expected {
            tracing::warn!(path = %rel, "the sealed version's keys in `hashes` aren't exactly its files");
            return Err(conflict(&rel));
        }
        out.set("hashes", Value::Object(hashes));
        if let Some(date) = &sealed_on {
            for run in out.runs_mut() {
                let ours = run
                    .get("run_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.eq_ignore_ascii_case(&run_id));
                if let (true, Some(Value::Object(signed))) = (ours, run.get_mut("signed_off")) {
                    signed.insert("date".into(), Value::from(date.as_str()));
                }
            }
        }
        *m = out;
        Ok(())
    });
    let r#ref = AssetRef::Sealed {
        ws: a.open.ws.id.clone(),
        project: a.project.clone(),
        asset: a.asset_id.clone(),
        version,
        sha256: Some(sealed.content_hash.clone()),
    }
    .to_string();
    ctx.record.set(RecordPlan {
        branch: RecordBranch::SealedVersion { version },
        project: a.project.clone(),
        asset_id: a.asset_id.clone(),
        commit: CommitOp {
            outputs: Vec::new(),
            moved,
            removed: sealed.left_run.clone(),
            manifest: Some((a.manifest.clone(), edit)),
            job_line: None,
            progress: None,
        },
        progress_entry: format!("signed off {} {version}", a.asset_id),
        refs: vec![r#ref.clone()],
        version_guard: Some(guard),
    });
    let summary = format!("signed off {} {version}", a.asset_id);
    Ok((
        SignoffResult {
            handoff: HandOff {
                refs: vec![r#ref.clone()],
                ..HandOff::default()
            },
            r#ref,
            version: version.to_string(),
            warnings: Vec::new(),
        },
        summary,
    ))
}

async fn request_signoff(
    ctx: &CallCtx,
    args: AssetArgs,
) -> Result<(StatusResult, String), ErrorBody> {
    let a = asset_of(ctx, args.project, &args.asset_id).await?;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    if read_manifest(a.store(), &a.manifest)
        .await
        .map_err(error)?
        .is_none()
    {
        return Err(unknown_asset(ctx, &a));
    }
    let rel = a.manifest.clone();
    let edit = move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            return Err(StoreError::NeedsRepair(rel.clone()));
        }
        let parse = |key: &str| {
            m.get(key)
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Status>().ok())
        };
        let view = m.view();
        let mut from = view.status.unwrap_or(Status::Planned);
        if from == Status::InReview {
            return Ok(());
        }
        let (before_stale, before_blocked) =
            (parse("status_before_stale"), parse("status_before_blocked"));
        // Ruling Q23: an approved asset with an open (kept) run is new work.
        let open_run = view
            .candidate_runs
            .iter()
            .any(|r| matches!(r.status.as_deref(), None | Some("open")) && r.trashed_utc.is_none());
        if open_run {
            from = reopened(from, before_stale, before_blocked)
                .map_err(StoreError::InvalidTransition)?;
        }
        let moved = transition(
            from,
            before_stale,
            before_blocked,
            &StatusEvent::RequestSignoff,
        )
        .map_err(StoreError::InvalidTransition)?;
        m.set("status", Value::from(moved.to.as_str()));
        Ok(())
    };
    let (after, _) = a
        .store()
        .edit_manifest(&a.manifest, &edit)
        .await
        .map_err(error)?;
    let status = after
        .view()
        .status
        .unwrap_or(Status::InReview)
        .as_str()
        .to_string();
    ctx.events
        .publish(&a.topic(), json!({"kind": "status", "status": status}));
    let summary = format!("{} is {status}", a.asset_id);
    Ok((StatusResult { status }, summary))
}

/// The `links` source a pinned ref names from `project`: `proj:<slug>` for another project's
/// asset, `lib:<item>` for a library item; `None` otherwise.
fn link_source(project: &str, r: &str) -> Result<Option<String>, ()> {
    if let Ok(link) = r.parse::<LinkRef>() {
        return Ok(match link {
            LinkRef::Project { slug, .. } if slug != project => Some(format!("proj:{slug}")),
            LinkRef::Library { item, .. } => Some(format!("lib:{item}")),
            _ => None,
        });
    }
    match r.parse::<AssetRef>() {
        Ok(AssetRef::Sealed { project: p, .. }) if p != project => Ok(Some(format!("proj:{p}"))),
        Ok(AssetRef::Sealed { .. }) => Ok(None),
        // A candidate ref is for viewing only, never an input (ARCH-042).
        Ok(AssetRef::Candidate { .. }) | Err(_) => Err(()),
    }
}

async fn pin_input(ctx: &CallCtx, args: PinArgs) -> Result<(PinResult, String), ErrorBody> {
    let a = asset_of(ctx, args.project, &args.asset_id).await?;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let Ok(source) = link_source(&a.project, &args.r#ref) else {
        let mut e = ErrorBody::new(
            &REF_INVALID,
            "ref is not a link (<asset_id>@<v>, proj:<slug>/<asset_id>@<v>, lib:<item>@<v>, \
             style@<n>) or a ww:// ref to a sealed version",
            ctx.trace_id.clone(),
        );
        e.data = Some(json!({ "ref": args.r#ref }));
        return Err(e);
    };
    if read_manifest(a.store(), &a.manifest)
        .await
        .map_err(error)?
        .is_none()
    {
        return Err(unknown_asset(ctx, &a));
    }
    // CORE-019: the source goes into the project's `links`, in Execute.
    if let Some(source) = source {
        let yaml = project_yaml_path(&a.project).map_err(|e| traced(ctx, e))?;
        match read_project(a.store(), &a.project).await.map_err(error)? {
            ProjectFile::Missing => return Err(traced(ctx, unknown_project(&a.project))),
            ProjectFile::Unreadable(why) => {
                return Err(error(StoreError::YamlUnsupported(format!(
                    "{yaml} doesn't parse ({why}); fix it first"
                ))));
            }
            ProjectFile::Parsed(p) if p.links.contains(&source) => {}
            ProjectFile::Parsed(_) => {
                a.store()
                    .edit_yaml(
                        &yaml,
                        &[YamlEdit::AppendToList {
                            path: vec!["links".to_string()],
                            item: source,
                        }],
                    )
                    .await
                    .map_err(error)?;
                ctx.events
                    .publish(&format!("project:{}", a.project), json!({"kind": "status"}));
            }
        }
    }
    let entry = json!({
        "ref": args.r#ref,
        "reason": args.reason,
        "by": signed_off_by(&ctx.session.caller),
        "date": ww_schema::time::date(a.store().now_utc()),
    });
    let (rel, r) = (a.manifest.clone(), args.r#ref.clone());
    let edit = move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            return Err(StoreError::NeedsRepair(rel.clone()));
        }
        if m.view().pinned_inputs.iter().any(|p| p.r#ref == r) {
            return Ok(());
        }
        m.push("pinned_inputs", entry.clone())
            .map_err(|e| StoreError::manifest_edit(&rel, &e))
    };
    let (after, _) = a
        .store()
        .edit_manifest(&a.manifest, &edit)
        .await
        .map_err(error)?;
    ctx.events.publish(&a.topic(), json!({"kind": "manifest"}));
    let pinned_inputs = after
        .get("pinned_inputs")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let summary = format!("pinned an input of {}", a.asset_id);
    Ok((PinResult { pinned_inputs }, summary))
}

/// Registers `asset.import`, `asset.collect`, `asset.signoff`, `asset.request_signoff` and
/// `asset.pin_input` (module `workspace`).
pub fn register_asset_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "asset.import",
        IMPORT_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: ImportArgs| Box::pin(import(ctx, args)),
    ));
    reg.register(typed_tool(
        "asset.collect",
        COLLECT_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: AssetArgs| Box::pin(collect(ctx, args)),
    ));
    reg.register(typed_tool(
        "asset.signoff",
        SIGNOFF_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: SignoffArgs| Box::pin(signoff(ctx, args)),
    ));
    reg.register(typed_tool(
        "asset.request_signoff",
        REQUEST_SIGNOFF_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: AssetArgs| Box::pin(request_signoff(ctx, args)),
    ));
    reg.register(typed_tool(
        "asset.pin_input",
        PIN_INPUT_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: PinArgs| Box::pin(pin_input(ctx, args)),
    ));
}
