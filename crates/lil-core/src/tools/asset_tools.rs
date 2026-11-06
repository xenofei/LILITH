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
//! | `asset.signoff` | write | `{project?, asset_id, run_id, files?: [name], takes?: [n], bump?, keep_runs?, review_view?}` | `{ref, version, handoff}` |
//! | `asset.request_signoff` | write | `{project?, asset_id}` | `{status}` |
//! | `asset.pin_input` | write | `{project?, asset_id, ref, reason}` | `{pinned_inputs}` |
//! | `asset.deprecate` | write | `{project?, asset_id, deprecated}` | `{status}` |
//! | `asset.mark` | write | `{project?, asset_id, run_id?, take?, file?, tags?, add_tags?, remove_tags?, rating?, label?, flag?, review_view?}` | `{tags, rating, label, take?: {run_id, take, flag, rating}}` |
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
//!   (ARCH-042). Sign-off isn't destructive, so an agent may do it (CORE-051). It takes
//!   exactly one of `files` and `takes` (`params.invalid` otherwise; Appendix H "Takes"):
//!   with `takes`, `pipeline::chosen_files` chooses each take's primary, then the takes'
//!   companions, and `signed_off` records `takes` and `companions`. `review_view` is validated
//!   and stored nowhere until the pick log.
//! - **`asset.request_signoff`** moves the status to `in_review` (CORE-053) with a manifest
//!   edit; an asset already `in_review` is left as it is, and an `approved`, `exported` or
//!   `released` asset with an open (kept) run takes `CandidateRun` first (ruling Q23). It is not a Record branch, so it
//!   writes no `progress.md` entry.
//! - **`asset.pin_input`** needs a ref that parses (`ref.invalid`; a candidate ref is never an
//!   input), whose target need not exist on this machine. It pushes `{ref, reason, by, date}`
//!   to `pinned_inputs` (once per ref) and, for a `proj:` or `lib:` source the project's
//!   `links` lack, appends it there with the comment-keeping YAML edit, in Execute (CORE-019).
//! - **`asset.deprecate`** (CORE-170; P4b-R4) sets the status `deprecated` with a manifest
//!   edit (`status_before_deprecated` = the status it had), or with `deprecated: false` returns
//!   it to the status `lil_schema::status::transition`'s fallback order gives. An asset already
//!   deprecated (or, for `false`, not deprecated) is left as it is. The asset's files and
//!   dependents are untouched; no `progress.md` entry. The edit holds the asset's version lock,
//!   as a sign-off does. A change publishes `{kind: "deprecated", deprecated, status,
//!   previous}` and `{kind: "status", status}` on the asset's topic, and
//!   `{kind: "deprecated", asset_id, deprecated, status, previous}` on `project:<slug>`.
//! - **`asset.mark`** (CORE-152; Appendix H "Organising tools"; P4b-R3, P4b-R26) sets the
//!   asset's `tags` (`tags` replaces; `add_tags`/`remove_tags` edit, compared by
//!   `lil_schema::fold_tag`; either way normalised when written), `rating` and `label`, and,
//!   with `run_id` and one of `take` or `file` ([`resolve_take`]: a companion addresses its
//!   take; a path picks its entry, a bare name must lead to one take), the take's `flag` and
//!   `rating` on its primary file entry. `null` clears `label` and `flag`. Like `set_marks`, a
//!   take mark is written only when its view value changes. A take's marks need an open run,
//!   else `status.invalid_transition`. Holding the
//!   asset's version lock, it edits the manifest (the store's change sink refreshes the index),
//!   then publishes `{kind: "mark", asset, run_id?, take?, previous}` on the asset's topic and
//!   `project:<slug>` (`asset` is the bare asset id), `previous` holding the view's value
//!   before (a hand-written rating of 7 reads as 5) of each mark that changed: `tags` and
//!   `label` are the asset's, `flag` is the take's, and `rating` is the take's when the call
//!   names a run, else the asset's. Nothing changed writes and publishes nothing. No jobs log
//!   line, no `progress.md` entry, no status change. `review_view` is validated and stored
//!   nowhere until the pick log.
//!
//! Every status change goes through `lil_schema::change_status`, which logs one status-log
//! entry in the same manifest edit, `by` the caller (`pipeline::status_by`).
//!
//! Imported file names are untrusted (ARCH-060): results list them as
//! `files: [{name, untrusted: true}]`, and summaries never repeat them. A name must be one path
//! component the workspace accepts, without `#` (ruling Q8). Changes are published on the
//! asset's topic: `{kind: "manifest"}` and `{kind: "status", status}`; the Record stage
//! publishes the candidate runs and sealed versions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lil_protocol::codes::{
    ASSET_LINK_EXTERNAL, ASSET_UNKNOWN, MANIFEST_NEEDS_REPAIR, PARAMS_INVALID, REF_HASH_MISMATCH,
    REF_INVALID, STATUS_INVALID_TRANSITION,
};
use lil_protocol::{AgentHint, EffectClass, ErrorBody, Fix, UiAction};
use lil_schema::{
    AssetRef, Bump, CandidateRun, Flag, Label, LinkRef, Manifest, SCHEME, Status, StatusEvent,
    Take, YamlEdit, bare_sha256, change_status, file_takes, fold_tag, hash_value, is_asset_id,
    is_slug, normalise_tags, takes_of,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::pipeline::{
    Chosen, HandOff, RecordBranch, RecordPlan, asset_topic, chosen_files, plan_signoff,
    signed_off_by, start_candidate_run, status_by,
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

const SIGNOFF_DESCRIPTION: &str = "Sign off files of an open candidate run, named by files \
    or chosen by takes (each take's primary file and its companions; exactly one of the two): \
    they (and the files derived from them) are sealed read-only as a new version folder \
    v<version>/ with a manifest snapshot, the asset becomes approved, and the other open runs \
    for that version are rejected unless listed in keep_runs (kept open and pinned). bump \
    (major, minor, patch) confirms the version bump; without it the version proposed when the \
    work started is used. The result's ref carries the version's content hash; handoff lists \
    what this unlocks and what it made stale.";

const MARK_DESCRIPTION: &str = "Set an asset's marks: tags (replaces them) or add_tags and \
    remove_tags (compared without regard to case), rating (0-5, 0 unrated) and label (red, \
    yellow, green, blue, purple; null clears). With run_id and one of take or file, flag (pick \
    or reject; null clears) and rating are the take's instead; a file addresses its take. A \
    run's marks change only while it is open. No status change.";

const REQUEST_SIGNOFF_DESCRIPTION: &str = "Flag an asset for sign-off: its status becomes \
    in_review.";

const DEPRECATE_DESCRIPTION: &str = "Deprecate an asset (deprecated: true): its status \
    becomes deprecated, every file stays and its dependents keep working. deprecated: false \
    returns it to the status it had before. Reversible; the change is in its status log.";

const PIN_INPUT_DESCRIPTION: &str = "Pin an input of an asset, so a newer version of it \
    doesn't make the asset stale: ref is a link (<asset_id>@<v>, proj:<slug>/<asset_id>@<v>, \
    lib:<item>@<v>, style@<n>) or a lil:// ref to a sealed version; its target need not exist \
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
    /// The chosen file names in the run; exactly one of files and takes.
    files: Option<Vec<String>>,
    /// The chosen takes (1-based): each take's primary file and its companions.
    takes: Option<Vec<u32>>,
    /// The confirmed bump; omitted: the proposed version.
    bump: Option<BumpArg>,
    /// Other open runs to keep open (pinned) instead of rejecting them.
    #[serde(default)]
    keep_runs: Vec<String>,
    /// The Review view the sign-off came from.
    #[expect(dead_code, reason = "validated now; the pick log (Task 13) records it")]
    review_view: Option<ReviewView>,
}

/// A Review view (Appendix H "Pick log"): validated on `asset.mark` and `asset.signoff`, stored
/// nowhere until the pick log records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReviewView {
    Grid,
    Loupe,
    Compare,
    Survey,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LabelArg {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl From<LabelArg> for Label {
    fn from(l: LabelArg) -> Label {
        match l {
            LabelArg::Red => Label::Red,
            LabelArg::Yellow => Label::Yellow,
            LabelArg::Green => Label::Green,
            LabelArg::Blue => Label::Blue,
            LabelArg::Purple => Label::Purple,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FlagArg {
    Pick,
    Reject,
}

impl From<FlagArg> for Flag {
    fn from(f: FlagArg) -> Flag {
        match f {
            FlagArg::Pick => Flag::Pick,
            FlagArg::Reject => Flag::Reject,
        }
    }
}

/// A present field, `null` included: `Some(None)` for `null`, `Some(Some(v))` for a value (an
/// absent field is `None` through `#[serde(default)]`).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MarkArgs {
    /// A project slug; omitted: the call's project.
    project: Option<String>,
    asset_id: String,
    /// The run of the take to mark; needs take or file.
    run_id: Option<String>,
    /// The take (1-based).
    take: Option<u32>,
    /// A file of the take: it addresses the take's primary file.
    file: Option<String>,
    /// Replaces the asset's tags.
    tags: Option<Vec<String>>,
    #[serde(default)]
    add_tags: Vec<String>,
    #[serde(default)]
    remove_tags: Vec<String>,
    /// 0-5, 0 unrated: the take's with run_id, else the asset's.
    rating: Option<u8>,
    /// null clears.
    #[serde(default, deserialize_with = "present")]
    label: Option<Option<LabelArg>>,
    /// The take's flag; null clears.
    #[serde(default, deserialize_with = "present")]
    flag: Option<Option<FlagArg>>,
    /// The Review view the decision came from.
    #[expect(dead_code, reason = "validated now; the pick log (Task 13) records it")]
    review_view: Option<ReviewView>,
}

/// A take's marks in `asset.mark`'s result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
struct TakeMarks {
    run_id: String,
    take: u32,
    flag: Option<String>,
    rating: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
struct MarkResult {
    tags: Vec<String>,
    rating: u8,
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    take: Option<TakeMarks>,
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
struct DeprecateArgs {
    /// A project slug; omitted: the call's project.
    project: Option<String>,
    asset_id: String,
    /// true deprecates; false returns to the status before.
    deprecated: bool,
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
        "created": lil_schema::time::rfc3339(now),
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
    let by = status_by(&ctx.session.caller);
    let edit: ManifestEdit = Arc::new(move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            *m = Manifest::new(&project, &asset_id, &asset_type, &uuid);
        }
        let Value::Object(run) = run.clone() else {
            unreachable!("the run is an object");
        };
        start_candidate_run(m, run, &by, now)?;
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
    let by = status_by(&ctx.session.caller);
    let utc = lil_schema::time::rfc3339(store.now_utc());
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
            change_status(m, &StatusEvent::StartWork, by.clone(), None, None, &utc)
                .map_err(StoreError::InvalidTransition)?;
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
            r#ref: format!("{SCHEME}{}/{}/{}", a.open.ws.id, a.project, a.asset_id),
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
        user_hint: None,
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
    // Appendix H "Takes": exactly one of `files` and `takes`.
    let chosen = match (args.files, args.takes) {
        (Some(files), None) => Chosen::Files(files),
        (None, Some(takes)) => Chosen::Takes(takes),
        _ => {
            return Err(invalid(
                ctx,
                "a sign-off names exactly one of files and takes".to_string(),
            ));
        }
    };
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
    // The names to hash when a pre-2b run recorded none: a bad take is `plan_signoff`'s to
    // report.
    let names = m
        .view()
        .candidate_runs
        .iter()
        .find(|r| r.run_id.eq_ignore_ascii_case(&args.run_id))
        .and_then(|run| chosen_files(run, &chosen).ok())
        .map(|c| c.names)
        .unwrap_or_default();
    let filled = hash_unrecorded(&a, &m, &args.run_id, &names)
        .await
        .map_err(error)?;
    let by = signed_off_by(&ctx.session.caller);
    let now = store.now_utc();
    let bump = args.bump.map(Bump::from);
    let (planned, version) = plan_signoff(
        &with_hashes(&m, &args.run_id, &filled),
        &args.run_id,
        &chosen,
        bump,
        &args.keep_runs,
        &by,
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
    let (rel, run_id, keep) = (
        a.manifest.clone(),
        run.run_id.clone(),
        args.keep_runs.clone(),
    );
    let edit: ManifestEdit = Arc::new(move |m: &mut Manifest| {
        let (mut out, again) = plan_signoff(
            &with_hashes(m, &run_id, &filled),
            &run_id,
            &chosen,
            bump,
            &keep,
            &by,
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
    let by = status_by(&ctx.session.caller);
    let utc = lil_schema::time::rfc3339(a.store().now_utc());
    let edit = move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            return Err(StoreError::NeedsRepair(rel.clone()));
        }
        if m.view().status == Some(Status::InReview) {
            return Ok(());
        }
        // Ruling Q23 (an approved asset with an open, kept run is new work) is
        // `change_status`'s.
        change_status(
            m,
            &StatusEvent::RequestSignoff,
            by.clone(),
            None,
            None,
            &utc,
        )
        .map_err(StoreError::InvalidTransition)?;
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

async fn deprecate(
    ctx: &CallCtx,
    args: DeprecateArgs,
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
    let by = status_by(&ctx.session.caller);
    let utc = lil_schema::time::rfc3339(a.store().now_utc());
    let ev = if args.deprecated {
        StatusEvent::Deprecate
    } else {
        StatusEvent::Undeprecate
    };
    // The status before this call's change, when it made one.
    let previous: Arc<std::sync::Mutex<Option<Option<Status>>>> = Arc::default();
    let seen = previous.clone();
    let edit = move |m: &mut Manifest| {
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if m.get("asset_id").is_none() {
            return Err(StoreError::NeedsRepair(rel.clone()));
        }
        let from = m.view().status;
        if (from == Some(Status::Deprecated)) == args.deprecated {
            return Ok(());
        }
        change_status(m, &ev, by.clone(), None, None, &utc)
            .map_err(StoreError::InvalidTransition)?;
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(from);
        Ok(())
    };
    // The asset's version lock, as `asset.signoff` holds it from its seal to its manifest
    // edit: a deprecation never lands between the two.
    let guard = a
        .store()
        .version_lock(&a.project, &a.asset_id)
        .lock_owned()
        .await;
    let (after, _) = a
        .store()
        .edit_manifest(&a.manifest, &edit)
        .await
        .map_err(error)?;
    drop(guard);
    let status = after.view().status.unwrap_or(Status::Planned).as_str();
    let previous = *previous
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(from) = previous {
        let previous = from.map(Status::as_str);
        ctx.events.publish(
            &a.topic(),
            json!({"kind": "deprecated", "deprecated": args.deprecated, "status": status,
                   "previous": previous}),
        );
        ctx.events
            .publish(&a.topic(), json!({"kind": "status", "status": status}));
        ctx.events.publish(
            &format!("project:{}", a.project),
            json!({"kind": "deprecated", "asset_id": a.asset_id, "deprecated": args.deprecated,
                   "status": status, "previous": previous}),
        );
    }
    let status = status.to_string();
    let summary = format!("{} is {status}", a.asset_id);
    Ok((StatusResult { status }, summary))
}

/// How an `asset.mark` call changes the asset's tags.
#[derive(Debug, Clone)]
enum TagsChange {
    Keep,
    Replace(Vec<String>),
    Edit {
        add: Vec<String>,
        remove: Vec<String>,
    },
}

/// The take an `asset.mark` call addresses: `run_id` and one of `take` or `file`.
#[derive(Debug, Clone)]
struct TakeAddress {
    run_id: String,
    take: Option<u32>,
    file: Option<String>,
}

/// An `asset.mark` call, its arguments checked.
#[derive(Debug, Clone)]
struct MarkRequest {
    tags: TagsChange,
    /// The take's with `take`, else the asset's.
    rating: Option<u8>,
    label: Option<Option<Label>>,
    flag: Option<Option<Flag>>,
    take: Option<TakeAddress>,
}

/// What [`apply_mark`] did: the result, and the view's values before for each mark it
/// changed (the `mark` event's `previous`).
#[derive(Debug, Clone)]
struct Applied {
    result: MarkResult,
    previous: Map<String, Value>,
}

/// The checks that need no manifest (params.invalid otherwise).
fn mark_request(ctx: &CallCtx, args: MarkArgs) -> Result<MarkRequest, ErrorBody> {
    if args.tags.is_some() && !(args.add_tags.is_empty() && args.remove_tags.is_empty()) {
        return Err(invalid(
            ctx,
            "tags replaces the tags; give it or add_tags/remove_tags, not both".to_string(),
        ));
    }
    if args
        .rating
        .is_some_and(|r| r > lil_schema::marks::MAX_RATING)
    {
        return Err(invalid(ctx, "a rating is 0 to 5".to_string()));
    }
    let take = match args.run_id {
        None if args.take.is_some() || args.file.is_some() || args.flag.is_some() => {
            return Err(invalid(
                ctx,
                "a take's flag and rating need run_id and one of take or file".to_string(),
            ));
        }
        None => None,
        Some(run_id) => {
            if args.take.is_some() == args.file.is_some() {
                return Err(invalid(
                    ctx,
                    "with run_id, name exactly one of take or file".to_string(),
                ));
            }
            if args.take == Some(0) {
                return Err(invalid(ctx, "takes are numbered from 1".to_string()));
            }
            Some(TakeAddress {
                run_id,
                take: args.take,
                file: args.file,
            })
        }
    };
    let tags = match args.tags {
        Some(tags) => TagsChange::Replace(tags),
        None if args.add_tags.is_empty() && args.remove_tags.is_empty() => TagsChange::Keep,
        None => TagsChange::Edit {
            add: args.add_tags,
            remove: args.remove_tags,
        },
    };
    Ok(MarkRequest {
        tags,
        rating: args.rating,
        label: args.label.map(|l| l.map(Label::from)),
        flag: args.flag.map(|f| f.map(Flag::from)),
        take,
    })
}

/// The take of `run` that `take` (its number) or `file` (one of its files) addresses (P4b-R3):
/// a companion addresses its take, whose primary holds the marks. Names are compared after
/// `to_lowercase`, as `lil_schema::takes_of` compares them. A `file` with a `/` (or `\`) is a
/// path: the file entry's asset-relative `path`, or that path inside the run folder. A bare
/// name matches any entry of that name, and must lead to exactly one take. The errors carry an
/// empty trace id.
///
/// # Errors
/// `params.invalid` unless exactly one of `take` and `file` is given, or for a `file` whose
/// entries are in more than one take; `asset.unknown` for a take the run doesn't have or a
/// file in none of its takes.
pub(crate) fn resolve_take(
    run: &CandidateRun,
    take: Option<u32>,
    file: Option<&str>,
) -> Result<Take, ErrorBody> {
    let takes = takes_of(run);
    let unknown = |what: String| {
        ErrorBody::new(
            &ASSET_UNKNOWN,
            format!("The run `{}` has no {what}.", run.run_id),
            "",
        )
    };
    match (take, file) {
        (Some(n), None) => takes
            .into_iter()
            .find(|t| t.n == n)
            .ok_or_else(|| unknown(format!("take {n}"))),
        (None, Some(file)) => {
            let given = file.replace('\\', "/").to_lowercase();
            let mut run_dir = run
                .path
                .clone()
                .unwrap_or_else(|| format!("candidates/{}/", run.run_id))
                .to_lowercase();
            if !run_dir.ends_with('/') {
                run_dir.push('/');
            }
            let bare = !given.contains('/');
            let mut found = BTreeSet::new();
            for (f, n) in run.files.iter().zip(file_takes(run)) {
                let path = f.path.to_lowercase();
                let hit = if bare {
                    path.rsplit('/').next() == Some(given.as_str())
                } else {
                    path == given || path.strip_prefix(&run_dir) == Some(given.as_str())
                };
                if let (true, Some(n)) = (hit, n) {
                    found.insert(n);
                }
            }
            let mut found = found.into_iter();
            match (found.next(), found.next()) {
                (None, _) => Err(unknown(format!("file `{file}` in a take"))),
                (Some(n), None) => takes
                    .into_iter()
                    .find(|t| t.n == n)
                    .ok_or_else(|| unknown(format!("take {n}"))),
                (Some(_), Some(_)) => Err(ErrorBody::new(
                    &PARAMS_INVALID,
                    format!(
                        "`{file}` names files in more than one take of the run `{}`: name the \
                         take, or the file's path in the run folder.",
                        run.run_id
                    ),
                    "",
                )),
            }
        }
        _ => Err(ErrorBody::new(
            &PARAMS_INVALID,
            "A take is named by exactly one of take or file.",
            "",
        )),
    }
}

/// Applies `req` to `m` (Appendix H "Organising tools"): the asset's marks through
/// `Manifest::set_marks` (tags normalised when this call writes them, P4b-R26), then the
/// take's flag and rating through `Manifest::set_take_mark` on an open run. The errors carry
/// an empty trace id; on an error `m` may be half-changed, so callers apply it to a copy or
/// inside a manifest edit that is then not written.
fn apply_mark(m: &mut Manifest, req: &MarkRequest) -> Result<Applied, ErrorBody> {
    let bad = |why: String| ErrorBody::new(&PARAMS_INVALID, why, "");
    let before = m.marks();
    let mut marks = before.clone();
    match &req.tags {
        TagsChange::Keep => {}
        TagsChange::Replace(tags) => marks.tags = normalise_tags(tags).map_err(bad)?,
        TagsChange::Edit { add, remove } => {
            let gone: BTreeSet<String> = remove.iter().map(|t| fold_tag(t.trim())).collect();
            let mut tags: Vec<String> = before
                .tags
                .iter()
                .filter(|t| !gone.contains(&fold_tag(t.trim())))
                .cloned()
                .collect();
            tags.extend(add.iter().cloned());
            marks.tags = normalise_tags(&tags).map_err(bad)?;
        }
    }
    if req.take.is_none()
        && let Some(rating) = req.rating
    {
        marks.rating = rating;
    }
    if let Some(label) = req.label {
        marks.label = label;
    }
    m.set_marks(&marks).map_err(bad)?;
    let after = m.marks();
    let label_value = |l: Option<Label>| l.map_or(Value::Null, |l| Value::from(l.as_str()));
    let mut previous = Map::new();
    if after.tags != before.tags {
        previous.insert("tags".into(), json!(before.tags));
    }
    if after.rating != before.rating {
        previous.insert("rating".into(), json!(before.rating));
    }
    if after.label != before.label {
        previous.insert("label".into(), label_value(before.label));
    }

    let take = match &req.take {
        None => None,
        Some(addr) => {
            let view = m.view();
            let Some(run) = view
                .candidate_runs
                .iter()
                .find(|r| r.run_id.eq_ignore_ascii_case(&addr.run_id))
            else {
                return Err(ErrorBody::new(
                    &ASSET_UNKNOWN,
                    format!(
                        "The asset `{}` has no candidate run `{}`; asset.get lists its runs.",
                        view.asset_id, addr.run_id
                    ),
                    "",
                ));
            };
            if !matches!(run.status.as_deref(), None | Some("open")) || run.trashed_utc.is_some() {
                return Err(ErrorBody::new(
                    &STATUS_INVALID_TRANSITION,
                    "This run is closed: its marks don't change after sign-off.",
                    "",
                ));
            }
            let t = resolve_take(run, addr.take, addr.file.as_deref())?;
            // Only what changes the view is written, as `set_marks` does: a hand-written value
            // the view already shows (a rating of 7 read as 5) stays until that mark changes.
            let flag = req.flag.filter(|f| *f != t.flag);
            let rating = req.rating.filter(|r| *r != t.rating);
            if flag.is_some() || rating.is_some() {
                m.set_take_mark(&run.run_id, t.n, flag, rating)
                    .map_err(|why| ErrorBody::new(&MANIFEST_NEEDS_REPAIR, why, ""))?;
            }
            let run_id = run.run_id.clone();
            let now = m
                .view()
                .candidate_runs
                .iter()
                .find(|r| r.run_id == run_id)
                .and_then(|r| takes_of(r).into_iter().find(|n| n.n == t.n))
                .unwrap_or_else(|| t.clone());
            if now.flag != t.flag {
                previous.insert("flag".into(), json!(t.flag.map(Flag::as_str)));
            }
            if now.rating != t.rating {
                previous.insert("rating".into(), json!(t.rating));
            }
            Some(TakeMarks {
                run_id,
                take: t.n,
                flag: now.flag.map(|f| f.as_str().to_string()),
                rating: now.rating,
            })
        }
    };
    Ok(Applied {
        result: MarkResult {
            tags: after.tags,
            rating: after.rating,
            label: after.label.map(|l| l.as_str().to_string()),
            take,
        },
        previous,
    })
}

async fn mark(ctx: &CallCtx, args: MarkArgs) -> Result<(MarkResult, String), ErrorBody> {
    let project = args.project.clone();
    let asset_id = args.asset_id.clone();
    let req = mark_request(ctx, args)?;
    let a = asset_of(ctx, project, &asset_id).await?;
    let store = a.store().clone();
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    // The asset's version lock, as `asset.signoff` holds it from its seal to its manifest
    // edit: a mark never lands between the two.
    let guard = store
        .version_lock(&a.project, &a.asset_id)
        .lock_owned()
        .await;
    let Some(m) = read_manifest(&store, &a.manifest).await.map_err(error)? else {
        return Err(unknown_asset(ctx, &a));
    };
    // The refusals, with their own codes, on a copy; the edit applies the same again.
    apply_mark(&mut m.clone(), &req).map_err(|e| traced(ctx, e))?;
    let applied: Arc<std::sync::Mutex<Option<Applied>>> = Arc::default();
    let seen = applied.clone();
    let rel = a.manifest.clone();
    let edit = move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            return Err(StoreError::NeedsRepair(rel.clone()));
        }
        // Refused now though the copy passed: the manifest changed meanwhile.
        let out = apply_mark(m, &req).map_err(|e| {
            if e.code == STATUS_INVALID_TRANSITION.code {
                return StoreError::InvalidTransition(e.message);
            }
            tracing::warn!(path = %rel, code = %e.code, why = %e.message, "the manifest changed during a mark");
            conflict(&rel)
        })?;
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(out);
        Ok(())
    };
    // The manifest, then the index (the store's change sink refreshes it), then the events.
    store
        .edit_manifest(&a.manifest, &edit)
        .await
        .map_err(error)?;
    drop(guard);
    let Applied { result, previous } = applied
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .expect("the manifest edit ran");
    if !previous.is_empty() {
        let mut event = json!({"kind": "mark", "asset": a.asset_id});
        if let Some(t) = &result.take {
            event["run_id"] = json!(t.run_id);
            event["take"] = json!(t.take);
        }
        event["previous"] = Value::Object(previous);
        ctx.events.publish(&a.topic(), event.clone());
        ctx.events.publish(&format!("project:{}", a.project), event);
    }
    let summary = match &result.take {
        Some(t) => format!("marked {} take {} of run {}", a.asset_id, t.take, t.run_id),
        None => format!("marked {}", a.asset_id),
    };
    Ok((result, summary))
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
             style@<n>) or a lil:// ref to a sealed version",
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
        "date": lil_schema::time::date(a.store().now_utc()),
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

/// Registers `asset.import`, `asset.collect`, `asset.signoff`, `asset.request_signoff`,
/// `asset.pin_input`, `asset.deprecate` and `asset.mark` (module `workspace`).
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
    reg.register(typed_tool(
        "asset.deprecate",
        DEPRECATE_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: DeprecateArgs| Box::pin(deprecate(ctx, args)),
    ));
    reg.register(typed_tool(
        "asset.mark",
        MARK_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: MarkArgs| Box::pin(mark(ctx, args)),
    ));
}
