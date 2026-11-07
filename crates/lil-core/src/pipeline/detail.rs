//! `asset.get`'s detail and `assets.query`'s summary (Global Constraints, Product tools;
//! CORE-027, CORE-033, CORE-047, ARCH-079).
//!
//! - `AssetSummary` is `{ref, project, asset_id, type, status, version, approved_version,
//!   updated, needs_repair, tags, rating, label, runs_waiting, external, working_doc, display,
//!   last_change, repair}`; `updated` is the RFC 3339 UTC last write of the manifest as last
//!   indexed. `ref` names the approved version (`lil://<ws>/<project>/<asset_id>@<version>`),
//!   or the asset without a version when none is approved. The marks and tile fields come from
//!   the index (Appendix H "`assets.query`"; APP-147, P4b-R30): `runs_waiting` counts the open
//!   runs, `display` is `{path, sha256, media_kind}` with a workspace-relative `path`,
//!   `last_change` is the newest status-log entry's `{to, by, utc}`, and `repair` is
//!   `{kind, from?, to?, other?}`.
//! - `AssetDetail` adds `versions` (from `Store::versions`, with `legacy`, each ref carrying
//!   its content hash, computed from the manifest's `hashes` without opening a sealed file;
//!   a legacy version holding a cloud-only file has a ref without `#sha256` and
//!   `cloud_only: true`, ruling Q32), `next_versions` (bumps of `approved_version`, or `1.0.0` when there is
//!   none), `proposed_bump` (`patch` when the open run's `target_version` is the patch bump,
//!   else `minor`), the manifest's own `candidate_runs`, `human_edits`, `external`,
//!   `status_log` and `trust` (as written), the index's file states (paths relative to the
//!   asset folder), `uses` (the manifest's and its open runs' inputs, `{ref, role, resolved}`;
//!   APP-148) and `used_by` (depth 1, from the index's edges to the asset and to every name
//!   that resolves to it through `registry/redirects.yaml`: `{ref: "<project>/<asset_id>",
//!   kind: "input"}`, a pin counting as an input).
//! - `uses[].resolved` is the input resolved through `projects::Resolver` (ARCH-125): a link to
//!   an asset's version names where the asset is now, with the version's content hash when it
//!   is sealed; `null` for a library or style input, and `null` with `error: {code, message,
//!   data?}` when it doesn't resolve (a loop, a dead end, the trash). A redirects file that
//!   can't be read or parsed fails no call: the entries that would need it carry its error
//!   (`manifest.needs_repair` for one that doesn't parse, the store's read error otherwise),
//!   live inputs still resolve, and `used_by` is by the asset's own name.
//!
//! An asset whose manifest is `missing` fails with `manifest.needs_repair`, `data.missing`
//! naming the manifest; one whose manifest doesn't parse is answered with `needs_repair: true`
//! from what the index knows.

use lil_protocol::ErrorBody;
use lil_protocol::codes::{ASSET_UNKNOWN, MANIFEST_NEEDS_REPAIR};
use lil_schema::{AssetRef, Bump, Manifest, SCHEME, Version};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::index::{AssetRow, FileState, Index};
use crate::projects::{Resolved, Resolver};
use crate::store::Store;

/// One asset in an `assets.query` page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AssetSummary {
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub project: String,
    pub asset_id: String,
    #[serde(rename = "type")]
    pub asset_type: Option<String>,
    pub status: Option<String>,
    pub version: Option<String>,
    pub approved_version: Option<String>,
    /// RFC 3339 UTC.
    pub updated: String,
    pub needs_repair: bool,
    pub tags: Vec<String>,
    /// 0–5; 0 is unrated.
    pub rating: u8,
    /// `red`, `yellow`, `green`, `blue` or `purple`.
    pub label: Option<String>,
    /// Open runs.
    pub runs_waiting: u32,
    /// The asset has a link-external file. Left out of `asset.get`, whose `external` is the
    /// manifest's list (Appendix E), non-empty exactly when this is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<bool>,
    /// The asset has a working document.
    pub working_doc: bool,
    /// The file that shows the asset (P4b-R30).
    pub display: Option<DisplayEntry>,
    /// The newest status-log entry.
    pub last_change: Option<LastChange>,
    /// `{kind: "moved" | "copy" | "duplicate_uuid" | "invalid", from?, to?, other?}`.
    pub repair: Option<Value>,
}

/// `AssetSummary.display`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DisplayEntry {
    /// Workspace-relative.
    pub path: String,
    pub sha256: Option<String>,
    /// `image`, `video`, `audio`, `model_3d`, `document`, `text`, `archive`, `font` or `other`.
    pub media_kind: String,
}

/// `AssetSummary.last_change`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LastChange {
    pub to: String,
    pub by: String,
    pub utc: String,
}

/// An input the asset uses (`asset.get`'s `uses`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UseEntry {
    /// As the manifest writes it.
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub role: Option<String>,
    /// Where it resolves to (module docs); `null` for a library or style input, or when it
    /// doesn't resolve (`error` then says why).
    pub resolved: Option<Resolved>,
    /// Why it doesn't resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<UseError>,
}

/// Why an input doesn't resolve: the error `ref.resolve` would give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UseError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// An asset that uses this one (`asset.get`'s `used_by`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UsedByEntry {
    /// `<project>/<asset_id>`.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// `input` (`local` is a working document's, APP-075).
    pub kind: String,
}

/// A sealed version of the asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VersionEntry {
    pub version: String,
    /// `lil://<ws>/<project>/<asset_id>@<version>#sha256=<content hash>`; without the hash
    /// when it isn't known (`cloud_only`).
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// No snapshot and no run signed off as this version (CORE-033).
    pub legacy: bool,
    /// A legacy version holds a cloud-only file, which is never downloaded, so its content
    /// hash isn't known (ruling Q32). Present only when `true`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cloud_only: bool,
}

/// The version each bump would give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct NextVersions {
    pub major: String,
    pub minor: String,
    pub patch: String,
}

/// A file of the asset and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FileEntry {
    /// Relative to the asset folder (`v1.0.0/x.png`).
    pub path: String,
    /// `ok`, `not_fetched`, `cloud_only`, `human_edit`, `needs_repair` or `missing`.
    pub state: String,
}

/// `asset.get`'s result: the summary, flattened, and the detail.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AssetDetail {
    #[serde(flatten)]
    pub summary: AssetSummary,
    pub versions: Vec<VersionEntry>,
    pub next_versions: NextVersions,
    /// `minor` or `patch`.
    pub proposed_bump: String,
    /// The manifest's `candidate_runs`, as written.
    pub candidate_runs: Value,
    /// The manifest's `human_edits`, as written.
    pub human_edits: Value,
    /// The manifest's `external`, as written.
    pub external: Value,
    /// The manifest's `status_log`, as written (newest last).
    pub status_log: Value,
    /// The manifest's `trust`, as written (`{}` when it has none).
    pub trust: Value,
    pub files: Vec<FileEntry>,
    /// The manifest's and its open runs' inputs, once each.
    pub uses: Vec<UseEntry>,
    /// Depth 1, by ref.
    pub used_by: Vec<UsedByEntry>,
}

/// The wire form of a file state.
pub fn file_state_wire(s: FileState) -> &'static str {
    s.as_str()
}

/// The summary of an indexed asset in workspace `ws_id`.
pub fn asset_summary(ws_id: &str, row: &AssetRow) -> AssetSummary {
    let approved = row
        .approved_version
        .as_deref()
        .and_then(|v| v.parse::<Version>().ok());
    let r#ref = match approved {
        Some(version) => AssetRef::Sealed {
            ws: ws_id.to_string(),
            project: row.project.clone(),
            asset: row.asset_id.clone(),
            version,
            sha256: None,
        }
        .to_string(),
        None => format!("{SCHEME}{ws_id}/{}/{}", row.project, row.asset_id),
    };
    AssetSummary {
        r#ref,
        project: row.project.clone(),
        asset_id: row.asset_id.clone(),
        asset_type: row.asset_type.clone(),
        status: row.status.clone(),
        version: row.version.clone(),
        approved_version: row.approved_version.clone(),
        updated: lil_schema::time::rfc3339(row.updated),
        needs_repair: row.needs_repair,
        tags: row.tags.clone(),
        rating: row.rating,
        label: row.label.clone(),
        runs_waiting: row.runs_waiting,
        external: Some(row.external),
        working_doc: row.working_doc,
        display: row
            .display
            .clone()
            .and_then(|v| serde_json::from_value(v).ok()),
        last_change: row
            .last_change
            .clone()
            .and_then(|v| serde_json::from_value(v).ok()),
        repair: row.repair.clone(),
    }
}

/// `asset.get` of `projects/<project>/assets/<asset_id>/` in workspace `ws_id`. The errors
/// carry an empty trace id; the caller sets its own.
///
/// # Errors
/// `asset.unknown` for an asset the index doesn't have; `manifest.needs_repair` (with
/// `data.missing`) when its manifest is missing; `store.io` when a read fails.
pub async fn asset_detail(
    store: &Store,
    index: &Index,
    ws_id: &str,
    project: &str,
    asset_id: &str,
) -> Result<AssetDetail, ErrorBody> {
    let row = index
        .asset(project, asset_id)
        .await
        .map_err(|e| e.to_error(""))?
        .ok_or_else(|| {
            ErrorBody::new(
                &ASSET_UNKNOWN,
                format!(
                    "There is no asset `{asset_id}` in the project `{project}`; assets.query lists \
                     the assets there are."
                ),
                "",
            )
        })?;
    let files = index
        .files_of(project, asset_id)
        .await
        .map_err(|e| e.to_error(""))?;
    let missing = || ErrorBody {
        data: Some(json!({ "missing": row.manifest.as_str() })),
        ..ErrorBody::new(
            &MANIFEST_NEEDS_REPAIR,
            format!(
                "The manifest `{}` is missing: it disappeared outside LILITH. Put it back \
                 (from git or a backup); the asset stays listed until then.",
                row.manifest
            ),
            "",
        )
    };
    if files
        .iter()
        .any(|f| f.path == row.manifest && f.state == FileState::Missing)
    {
        return Err(missing());
    }
    let bytes = match store.read(&row.manifest).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(missing()),
        Err(e) => return Err(e.to_error("")),
    };
    let manifest = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| Manifest::parse(text).ok())
        .map(|(m, _)| m);
    let list = |key: &str| {
        manifest
            .as_ref()
            .and_then(|m| m.get(key))
            .filter(|v| v.is_array())
            .cloned()
            .unwrap_or_else(|| json!([]))
    };
    let view = manifest.as_ref().map(Manifest::view).unwrap_or_default();
    let resolver = Resolver::load(index, store).await?;

    let versions = store
        .versions(project, asset_id)
        .await
        .map_err(|e| e.to_error(""))?
        .into_iter()
        .map(|v| VersionEntry {
            version: v.version.to_string(),
            r#ref: AssetRef::Sealed {
                ws: ws_id.to_string(),
                project: project.to_string(),
                asset: asset_id.to_string(),
                version: v.version,
                sha256: v.content_hash,
            }
            .to_string(),
            legacy: v.legacy,
            cloud_only: v.cloud_only,
        })
        .collect();

    let approved = view
        .approved_version
        .or_else(|| row.approved_version.as_deref().and_then(|v| v.parse().ok()));
    let next_versions = match approved {
        Some(v) => NextVersions {
            major: v.bump(Bump::Major).to_string(),
            minor: v.bump(Bump::Minor).to_string(),
            patch: v.bump(Bump::Patch).to_string(),
        },
        None => {
            let first = Version::new(1, 0, 0).to_string();
            NextVersions {
                major: first.clone(),
                minor: first.clone(),
                patch: first,
            }
        }
    };
    let patch = approved.map(|v| v.bump(Bump::Patch));
    let open_run_is_patch = view.candidate_runs.iter().any(|run| {
        run.status.as_deref() == Some("open")
            && run.trashed_utc.is_none()
            && run.target_version.is_some()
            && run.target_version == patch
    });
    let proposed_bump = if open_run_is_patch {
        Bump::Patch
    } else {
        Bump::Minor
    };

    let base = format!("projects/{project}/assets/{asset_id}/");
    let files = files
        .iter()
        .map(|f| FileEntry {
            path: f
                .path
                .as_str()
                .strip_prefix(&base)
                .unwrap_or(f.path.as_str())
                .to_string(),
            state: file_state_wire(f.state).to_string(),
        })
        .collect();

    let mut listed: Vec<(String, Option<String>)> = Vec::new();
    let run_inputs = view
        .candidate_runs
        .iter()
        .filter(|run| run.status.as_deref() == Some("open") && run.trashed_utc.is_none())
        .flat_map(|run| run.inputs.iter().map(|i| (&i.r#ref, &i.role)));
    for (r#ref, role) in view
        .inputs
        .iter()
        .map(|i| (&i.r#ref, &i.role))
        .chain(run_inputs)
    {
        let entry = (r#ref.clone(), role.clone());
        if !listed.contains(&entry) {
            listed.push(entry);
        }
    }
    let mut uses: Vec<UseEntry> = Vec::with_capacity(listed.len());
    for (r#ref, role) in listed {
        let (resolved, error) = match resolver.resolve_input(ws_id, project, &r#ref).await {
            None => (None, None),
            Some(Ok(resolved)) => (Some(resolved), None),
            Some(Err(e)) => (
                None,
                Some(UseError {
                    code: e.code,
                    message: e.message,
                    data: e.data,
                }),
            ),
        };
        uses.push(UseEntry {
            r#ref,
            role,
            resolved,
            error,
        });
    }
    let mut used_by: Vec<UsedByEntry> = Vec::new();
    for edge in resolver.edges_to(project, asset_id).await? {
        let entry = UsedByEntry {
            r#ref: format!("{}/{}", edge.from_project, edge.from_asset),
            kind: "input".to_string(),
        };
        if !used_by.contains(&entry) {
            used_by.push(entry);
        }
    }
    used_by.sort_by(|a, b| (&a.r#ref, &a.kind).cmp(&(&b.r#ref, &b.kind)));
    let trust = manifest
        .as_ref()
        .and_then(|m| m.get("trust"))
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut summary = asset_summary(ws_id, &row);
    summary.needs_repair = row.needs_repair || manifest.is_none();
    // `asset.get`'s `external` is the manifest's list.
    summary.external = None;
    Ok(AssetDetail {
        summary,
        versions,
        next_versions,
        proposed_bump: proposed_bump.as_str().to_string(),
        candidate_runs: list("candidate_runs"),
        human_edits: list("human_edits"),
        external: list("external"),
        status_log: list("status_log"),
        trust,
        files,
        uses,
        used_by,
    })
}
