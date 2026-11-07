//! Accepting a move and making a copy independent (CORE-154; Appendix H "Moved and copied
//! folders"; P4b-R32, P4b-R34): the two repairs of an asset folder moved or copied outside the
//! suite, which run only when you choose them or an agent calls them. What needs which repair
//! is the index's ([`crate::index::repair`]); the tools in `tools/asset_tools.rs` hold the
//! asset's version lock around these calls, compute the redirect's `by` and `actor`, and
//! publish the events.
//!
//! Both start from the folder as it is on disk: [`check_folder`] refuses a folder whose name
//! (or its project's) isn't valid, also when it differs from a valid one only by case, before
//! anything is written; the folder (and, for an accepted move, its old place; for a copy, the
//! other folders with its creation UUID) is reconciled first, so a stale index row never
//! decides, and the asset's `repair` is read from the index after that.
//!
//! - [`accept_move`] (on a `moved` asset whose old folder is not on disk, whatever the index
//!   says, else `repair.not_needed`; ruling T8 R1): (1) appends a redirect
//!   from the old place to the folder, unless the newest redirect from the old place already
//!   leads there; (2) renames the manifest file to `<asset_id>.manifest.json` when it carries
//!   another name; (3) sets the manifest's `project` and `asset_id` to the folder. Each step is
//!   skipped when done, so a retry after a crash before or during step (3) finishes the work
//!   and writes no second redirect (P4b-R34). A retry after a crash *past* step (3) (the
//!   manifest edit landed, the call failed after it) finds the asset healthy and answers
//!   `repair.not_needed`: the work is done, and no `moved` event is published again (ruling
//!   T8 M2). Dependents aren't rewritten: they resolve through the redirect. Then the index
//!   drops the old place's rows, a folder the move left behind (ruling T8 I2), also when step
//!   (3) found its work done.
//! - [`make_independent`] (on a `copy` or `duplicate_uuid` asset, else `repair.not_needed`):
//!   renames the manifest file as (2), then gives the manifest a fresh `creation_uuid` from the
//!   store's id source and sets its `project` and `asset_id` to the folder. "That copy" is the
//!   one to change (CORE-154): the one agreeing folder of a group whose other members are its
//!   copies is refused with `repair.not_needed`, naming the copies; when every folder of the
//!   group agrees, or none does, any of them may be chosen (ruling T8 I1).
//!
//! Both decide and write under the lock of the folder's creation UUID
//! ([`Store::creation_uuid_lock`]), taken after a first look and followed by a second one, so
//! two repairs of one group of folders never both act on the same view (ruling T8 M3). The
//! manifest edit re-checks that the manifest still has the creation UUID (and, for a move, the
//! place) the decision was made on; one changed meanwhile is `store.hash_conflict`.

use lil_platform::fs::Probe;
use lil_protocol::ErrorBody;
use lil_protocol::codes::{
    ASSET_UNKNOWN, PARAMS_INVALID, REPAIR_NOT_NEEDED, REPAIR_NOT_NEEDED_MESSAGE,
};
use lil_schema::{Actor, Manifest, is_asset_id, is_slug};
use serde_json::Value;

use super::redirects::{Redirect, append_redirect, parse_name};
use super::resolve::Resolver;
use crate::index::repair::{RepairKind, agrees, uuid_key};
use crate::index::{AssetRow, Index, Scope};
use crate::store::{Store, StoreError, WsPath};

/// `(project, asset_id)`.
type Name = (String, String);

/// What [`accept_move`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMove {
    /// The old place.
    pub from: Name,
    /// The folder.
    pub to: Name,
    /// Whether this call appended the redirect (a retry finds it there).
    pub redirect_written: bool,
}

fn invalid(why: String) -> ErrorBody {
    ErrorBody::new(&PARAMS_INVALID, why, "")
}

fn not_needed(why: &str) -> ErrorBody {
    ErrorBody::new(
        &REPAIR_NOT_NEEDED,
        format!("{REPAIR_NOT_NEEDED_MESSAGE} {why}"),
        "",
    )
}

fn unknown(project: &str, asset_id: &str) -> ErrorBody {
    ErrorBody::new(
        &ASSET_UNKNOWN,
        format!(
            "There is no asset `{asset_id}` in the project `{project}`; assets.query lists the \
             assets there are."
        ),
        "",
    )
}

fn store_error(e: &StoreError) -> ErrorBody {
    e.to_error("")
}

fn folder_path(project: &str, asset_id: &str) -> String {
    format!("projects/{project}/assets/{asset_id}")
}

/// The name of the entry of `dir` (on disk) that is `name` without regard to ASCII case.
fn entry_like(probe: &dyn Probe, dir: &std::path::Path, name: &str) -> Option<String> {
    probe
        .read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.eq_ignore_ascii_case(name))
}

/// The folders `projects/<project>/` and `projects/<project>/assets/<asset_id>/` as they are
/// spelled on disk, found without regard to ASCII case: `None` without the project folder, and
/// the asset id `None` without the asset folder.
async fn spelled_on_disk(store: &Store, name: &Name) -> Option<(String, Option<String>)> {
    let (probe, root) = (store.probe().clone(), store.root().to_path_buf());
    let (project, asset_id) = name.clone();
    tokio::task::spawn_blocking(move || {
        let projects = root.join("projects");
        let p = entry_like(probe.as_ref(), &projects, &project)?;
        let assets = projects.join(&p).join("assets");
        let a = entry_like(probe.as_ref(), &assets, &asset_id);
        Some((p, a))
    })
    .await
    .unwrap_or(None)
}

/// Refuses, before anything is written, an asset folder `projects/<project>/assets/<asset_id>`
/// whose name isn't a valid asset id or whose project's isn't a valid slug, also when the
/// folder on disk differs from the names given only by case (Windows names are
/// case-insensitive): `params.invalid` naming the folder and saying to rename it first. A
/// folder that isn't there is left to the caller (`asset.unknown`).
///
/// # Errors
/// `params.invalid`, with an empty trace id.
pub async fn check_folder(
    store: &Store,
    project: &str,
    asset_id: &str,
    tool: &str,
) -> Result<(), ErrorBody> {
    let rename = |folder: &str| {
        invalid(format!(
            "The asset folder `{folder}` isn't named as one must be: an asset id is a lowercase \
             letter, then lowercase letters, digits and '_' (at most 63 characters), inside a \
             project whose slug is lowercase letters, digits and '-'. Rename the folder to a \
             valid asset id first, then call {tool} again."
        ))
    };
    if !is_slug(project) || !is_asset_id(asset_id) {
        return Err(rename(&folder_path(project, asset_id)));
    }
    let name = (project.to_string(), asset_id.to_string());
    if let Some((p, a)) = spelled_on_disk(store, &name).await {
        let a = a.unwrap_or_else(|| asset_id.to_string());
        if p != project || a != asset_id {
            return Err(rename(&folder_path(&p, &a)));
        }
    }
    Ok(())
}

/// The asset row of the folder after reconciling `dirs`, from the index.
///
/// # Errors
/// `asset.unknown` when the index has no asset there; the index's error.
async fn fresh_row(
    index: &Index,
    project: &str,
    asset_id: &str,
    dirs: Vec<WsPath>,
) -> Result<AssetRow, ErrorBody> {
    index.ready_result().await.map_err(|e| e.to_error(""))?;
    index
        .reconcile(Scope::Paths(dirs))
        .await
        .map_err(|e| e.to_error(""))?;
    index
        .asset(project, asset_id)
        .await
        .map_err(|e| e.to_error(""))?
        .ok_or_else(|| unknown(project, asset_id))
}

/// The asset row of the folder as the disk is now (module docs): the folder is reconciled,
/// then every folder its repair depends on (the place its manifest names, as it is spelled on
/// disk, and the other folders with its creation UUID, as the index names them), so an index
/// that lags behind an outside change never decides. (A folder is reconciled under the name
/// it has on disk: a scoped reconcile names what it finds by the path it is given.)
///
/// # Errors
/// `asset.unknown` when the index has no asset there; the index's error.
async fn settled_row(index: &Index, store: &Store, here: &Name) -> Result<AssetRow, ErrorBody> {
    let row = fresh_row(index, &here.0, &here.1, vec![dir_of(here)?]).await?;
    let mut related: Vec<Name> = Vec::new();
    if row.manifest_parses {
        let place = (
            row.manifest_project
                .clone()
                .unwrap_or_else(|| here.0.clone()),
            row.manifest_asset_id
                .clone()
                .unwrap_or_else(|| here.1.clone()),
        );
        if is_slug(&place.0) && is_asset_id(&place.1) {
            related.push(match spelled_on_disk(store, &place).await {
                Some((p, Some(a))) => (p, a),
                Some((p, None)) => (p, place.1),
                None => place,
            });
        }
    }
    if let Some(uuid) = uuid_key(&row) {
        related.extend(
            index
                .repairs(&["copy", "duplicate_uuid"])
                .await
                .map_err(|e| e.to_error(""))?
                .iter()
                .filter(|r| uuid_key(r).as_ref() == Some(&uuid))
                .map(|r| (r.project.clone(), r.asset_id.clone())),
        );
    }
    related.retain(|n| !(n.0.eq_ignore_ascii_case(&here.0) && n.1.eq_ignore_ascii_case(&here.1)));
    related.sort();
    related.dedup();
    if related.is_empty() {
        return Ok(row);
    }
    let dirs = related.iter().map(dir_of).collect::<Result<_, _>>()?;
    fresh_row(index, &here.0, &here.1, dirs).await
}

fn dir_of(name: &Name) -> Result<WsPath, ErrorBody> {
    WsPath::parse(&folder_path(&name.0, &name.1)).map_err(|e| store_error(&e))
}

fn kind_of(row: &AssetRow) -> Option<RepairKind> {
    row.repair.as_ref().and_then(RepairKind::from_json)
}

/// `<asset_id>.manifest.json` of the folder of `row`, and whether the manifest carries another
/// file name.
fn canonical_manifest(row: &AssetRow) -> Result<(WsPath, bool), ErrorBody> {
    let canonical = dir_of(&(row.project.clone(), row.asset_id.clone()))?
        .join(&format!("{}.manifest.json", row.asset_id))
        .map_err(|e| store_error(&e))?;
    let renamed = !row
        .manifest
        .file_name()
        .eq_ignore_ascii_case(canonical.file_name());
    Ok((canonical, renamed))
}

/// Step (2): the manifest file to `<asset_id>.manifest.json`, through the store.
async fn rename_manifest(store: &Store, row: &AssetRow) -> Result<WsPath, ErrorBody> {
    let (canonical, renamed) = canonical_manifest(row)?;
    if renamed {
        store
            .rename_new(&row.manifest, &canonical)
            .await
            .map_err(|e| store_error(&e))?;
    }
    Ok(canonical)
}

/// The manifest's own place: its `project` and `asset_id`, the folder's for one absent.
fn place_in(m: &Manifest, folder: &Name) -> Option<Name> {
    let own = |key: &str, fallback: &str| match m.get(key) {
        None | Some(Value::Null) => Some(fallback.to_string()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    };
    Some((own("project", &folder.0)?, own("asset_id", &folder.1)?))
}

fn changed(rel: &WsPath) -> StoreError {
    StoreError::HashConflict {
        path: rel.clone(),
        backup: None,
        cloud_only: false,
        kept: Vec::new(),
    }
}

/// `asset.accept_move` of the folder `projects/<project>/assets/<asset_id>` (module docs). The
/// caller holds the asset's version lock and has checked the folder ([`check_folder`]). `by`
/// and `actor` are the redirect's. The errors carry an empty trace id.
///
/// # Errors
/// `asset.unknown`; `repair.not_needed` when the asset isn't `moved`; the redirects file's
/// error when it can't be read or parsed (nothing written); the store's errors (a manifest
/// changed meanwhile is `store.hash_conflict`).
pub async fn accept_move(
    index: &Index,
    store: &Store,
    project: &str,
    asset_id: &str,
    by: &str,
    actor: Option<Actor>,
) -> Result<AcceptedMove, ErrorBody> {
    let to: Name = (project.to_string(), asset_id.to_string());
    let refuse = || {
        not_needed(
            "asset.accept_move accepts a folder moved outside the suite (project.status lists \
             them under moved).",
        )
    };
    let first = settled_row(index, store, &to).await?;
    let Some(group) = uuid_key(&first) else {
        return Err(refuse());
    };
    let lock = store.creation_uuid_lock(&group);
    let _decided = lock.lock().await;
    let row = settled_row(index, store, &to).await?;
    let Some(RepairKind::Moved { from, .. }) = kind_of(&row) else {
        return Err(refuse());
    };
    let from = parse_name(&from).ok_or_else(|| invalid(format!("{from:?} is not a name")))?;
    // Whatever the index believes, a folder still at the old place (even one that can't be
    // read) means this is no move: nothing is written (ruling T8 R1).
    if let Some((_, Some(there))) = spelled_on_disk(store, &from).await {
        return Err(not_needed(&format!(
            "Its old folder projects/{}/assets/{there} is still there, so this folder is no \
             move; project.status says what it is.",
            from.0
        )));
    }
    let uuid = uuid_key(&row);

    // (1) The redirect, unless the newest one from the old place already leads here.
    let resolver = Resolver::load(index, store).await?;
    if let Some(e) = resolver.file_error() {
        return Err(e.clone());
    }
    let present = resolver
        .newest_from(&from.0, &from.1)
        .is_some_and(|r| r.to == to);
    drop(resolver);
    if !present {
        let r = Redirect {
            from: from.clone(),
            to: to.clone(),
            moved_utc: lil_schema::time::rfc3339(store.now_utc()),
            by: by.to_string(),
            actor,
        };
        append_redirect(store, &r)
            .await
            .map_err(|e| store_error(&e))?;
    }
    // (2) The manifest's file name, then (3) its `project` and `asset_id`.
    let rel = rename_manifest(store, &row).await?;
    let (target, old) = (to.clone(), from.clone());
    let at = rel.clone();
    let edit = move |m: &mut Manifest| {
        let same_uuid = m
            .view()
            .creation_uuid
            .map(|u| u.to_ascii_lowercase())
            .filter(|u| !u.is_empty())
            == uuid;
        if !same_uuid {
            return Err(changed(&at));
        }
        match place_in(m, &target) {
            Some(p) if p == target => Ok(()),
            Some(p) if p == old => {
                m.set("project", Value::from(target.0.clone()));
                m.set("asset_id", Value::from(target.1.clone()));
                Ok(())
            }
            _ => Err(changed(&at)),
        }
    };
    store
        .edit_manifest(&rel, &edit)
        .await
        .map_err(|e| store_error(&e))?;
    // The old place is now known to be a folder the move left behind: the index drops it,
    // also when step (3) found its work done (no store event then).
    index
        .reconcile(Scope::Paths(vec![dir_of(&from)?]))
        .await
        .map_err(|e| e.to_error(""))?;
    index.refresh_repairs().await.map_err(|e| e.to_error(""))?;
    Ok(AcceptedMove {
        from,
        to,
        redirect_written: !present,
    })
}

/// `asset.make_independent` of the folder `projects/<project>/assets/<asset_id>` (module
/// docs): returns the new `creation_uuid`. The caller holds the asset's version lock and has
/// checked the folder ([`check_folder`]). The errors carry an empty trace id.
///
/// # Errors
/// `asset.unknown`; `repair.not_needed` when the asset isn't a `copy` or `duplicate_uuid`; the
/// store's errors (a manifest changed meanwhile is `store.hash_conflict`).
pub async fn make_independent(
    index: &Index,
    store: &Store,
    project: &str,
    asset_id: &str,
) -> Result<String, ErrorBody> {
    let here: Name = (project.to_string(), asset_id.to_string());
    let refuse = || {
        not_needed(
            "asset.make_independent changes a folder that shares its creation UUID with another \
             (project.status lists them under duplicates).",
        )
    };
    let first = settled_row(index, store, &here).await?;
    let Some(group) = uuid_key(&first) else {
        return Err(refuse());
    };
    let lock = store.creation_uuid_lock(&group);
    let _decided = lock.lock().await;
    let row = settled_row(index, store, &here).await?;
    if !matches!(
        kind_of(&row),
        Some(RepairKind::Copy { .. } | RepairKind::DuplicateUuid { .. })
    ) {
        return Err(refuse());
    }
    let uuid = uuid_key(&row);
    // The one agreeing folder of a group whose others are its copies: change a copy instead.
    if agrees(&row) {
        let me = format!("{}/{}", here.0, here.1);
        let copies: Vec<String> = index
            .repairs(&["copy"])
            .await
            .map_err(|e| e.to_error(""))?
            .iter()
            .filter(|r| uuid_key(r) == uuid)
            .filter(|r| matches!(kind_of(r), Some(RepairKind::Copy { other }) if other == me))
            .map(|r| format!("{}/{}", r.project, r.asset_id))
            .collect();
        if !copies.is_empty() {
            return Err(not_needed(&format!(
                "{me} is the original: its manifest names its own folder, so the copy to make \
                 independent is {}.",
                copies.join(", ")
            )));
        }
    }
    let fresh = store.ids().uuid();
    let rel = rename_manifest(store, &row).await?;
    let (target, new_uuid, at) = (here.clone(), fresh.clone(), rel.clone());
    let edit = move |m: &mut Manifest| {
        let now = m
            .view()
            .creation_uuid
            .map(|u| u.to_ascii_lowercase())
            .filter(|u| !u.is_empty());
        if now != uuid && now.as_deref() != Some(new_uuid.as_str()) {
            return Err(changed(&at));
        }
        m.set("creation_uuid", Value::from(new_uuid.clone()));
        m.set("project", Value::from(target.0.clone()));
        m.set("asset_id", Value::from(target.1.clone()));
        Ok(())
    };
    store
        .edit_manifest(&rel, &edit)
        .await
        .map_err(|e| store_error(&e))?;
    Ok(fresh)
}
