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
//! - [`accept_move`] (on a `moved` asset whose old place is confirmed empty on disk, whatever
//!   the index says: its attributes give `NotFound`, so a folder that can't be read or listed,
//!   or a file at that name, refuses too; else `repair.not_needed`, saying what to remove or
//!   rename; rulings T8 R1, R2-1, M-b): (1) appends a redirect
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
//! - [`restored_as`] (P4b-R39; CORE-037): after `trash.restore {to}` moved a whole asset's
//!   folder from the trash to `projects/<p>/assets/<to>/`, the same three steps as an accepted
//!   move, in the same order (P4b-R34): (1) appends the redirect `<p>/<old id>` →
//!   `<p>/<to>`, unless the newest redirect from the old id already leads there; (2) renames
//!   the manifest file to `<to>.manifest.json`; (3) sets the manifest's `asset_id` to `to`
//!   (only while it is still the old id). Each step is skipped when done. The folder moves
//!   first, so a crash at any point after it leaves an asset whose manifest names its old place:
//!   the index lists it `moved` (from the old id to `to`), and `asset.accept_move` finishes the
//!   work with no second redirect, as it finds the newest redirect from the old id already
//!   leading to `to`.
//!
//! Both decide and write under the lock of the folder's creation UUID
//! ([`Store::creation_uuid_lock`]), taken after a first look and followed by a second one, so
//! two repairs of one group of folders never both act on the same view (ruling T8 M3). When the
//! second look shows another UUID (an outside edit in between), the lock is released and the
//! two looks are taken again under the new UUID's lock, at most twice more; then the call is
//! refused with `store.hash_conflict` (ruling T8 A-L3).
//!
//! `accept_move` never sends one asset's refs to another (rulings T8 A-H1, A-RM1, A-RM2): a
//! folder whose old place another asset's folder (another creation UUID, or none) also names is
//! `invalid` in the index, and the refusal names the other claimants (suggesting only a valid
//! name, A-RL1); and before its redirect, holding the lock of the old name
//! ([`Store::redirect_from_lock`], taken last), it follows where old refs to that name lead now:
//! a chain that ends at a live asset folder with another creation UUID refuses, naming that
//! asset, and so does one that ends in the trash (`asset.trashed`), naming the entry; a chain
//! that ends nowhere (`target_missing`) or loops (`cycle`) is fine. The
//! manifest edit re-checks that the manifest still has the creation UUID (and, for a move, the
//! place) the decision was made on; one changed meanwhile is `store.hash_conflict`.

use lil_platform::fs::Probe;
use lil_protocol::ErrorBody;
use lil_protocol::codes::{
    ASSET_TRASHED, ASSET_UNKNOWN, PARAMS_INVALID, REPAIR_NOT_NEEDED, REPAIR_NOT_NEEDED_MESSAGE,
    STORE_HASH_CONFLICT,
};
use lil_schema::{Actor, Manifest, is_asset_id, is_slug};
use serde_json::Value;
use tokio::sync::OwnedMutexGuard;

use super::redirects::{Redirect, append_redirect, parse_name};
use super::resolve::Resolver;
use crate::index::repair::{RepairKind, agrees, claim, uuid_key};
use crate::index::{AssetRow, Index, OnDisk, Scope, asset_folder_on_disk};
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

/// How many times a repair takes the UUID lock again when the second look shows another UUID.
const LOCK_RETRIES: usize = 2;

/// The folder's row decided under the lock of its creation UUID (module docs; rulings T8 M3,
/// A-L3), with the lock's guard; `None` when the folder has no creation UUID.
///
/// # Errors
/// `store.hash_conflict` when the UUID changes between the two looks every time; the errors
/// of [`settled_row`].
async fn locked_row(
    index: &Index,
    store: &Store,
    here: &Name,
) -> Result<Option<(AssetRow, OwnedMutexGuard<()>)>, ErrorBody> {
    let mut first = settled_row(index, store, here).await?;
    for _ in 0..=LOCK_RETRIES {
        let Some(group) = uuid_key(&first) else {
            return Ok(None);
        };
        let guard = store.creation_uuid_lock(&group).lock_owned().await;
        let again = settled_row(index, store, here).await?;
        if uuid_key(&again).as_deref() == Some(group.as_str()) {
            return Ok(Some((again, guard)));
        }
        drop(guard);
        first = again;
    }
    Err(ErrorBody::new(
        &STORE_HASH_CONFLICT,
        format!(
            "The creation UUID of projects/{}/assets/{} kept changing while it was being \
             repaired (something else edits its manifest); nothing was changed. Try again when \
             it is left alone.",
            here.0, here.1
        ),
        "",
    ))
}

/// `<project>/<asset_id>` the manifest of `row` names (the folder's for a key it lacks).
fn place_of(row: &AssetRow) -> String {
    format!(
        "{}/{}",
        row.manifest_project.as_deref().unwrap_or(&row.project),
        row.manifest_asset_id.as_deref().unwrap_or(&row.asset_id)
    )
}

/// The other live disagreeing folders, in a group or alone, whose manifests name the same
/// place as `row` (without regard to case) with another creation UUID or none: the other
/// claimants of an `invalid` folder (rulings T8 A-H1, A-RM1). Every disagreeing folder needs a
/// repair, so the index's repair rows hold them all (a gone folder has none).
async fn claimants(index: &Index, row: &AssetRow) -> Result<Vec<String>, ErrorBody> {
    if !row.manifest_parses || agrees(row) || uuid_key(row).is_none() {
        return Ok(Vec::new());
    }
    let mine = place_of(row).to_ascii_lowercase();
    Ok(index
        .repairs(&["moved", "copy", "duplicate_uuid", "invalid"])
        .await
        .map_err(|e| e.to_error(""))?
        .iter()
        .filter(|r| {
            (r.project.as_str(), r.asset_id.as_str())
                != (row.project.as_str(), row.asset_id.as_str())
        })
        .filter(|r| {
            claim(r).is_some_and(|((p, a), uuid)| {
                format!("{p}/{a}").to_ascii_lowercase() == mine && uuid != uuid_key(row)
            })
        })
        .map(|r| format!("{}/{}", r.project, r.asset_id))
        .collect())
}

/// `a`, `a and b`, `a, b and c`.
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The refusal of a folder whose old place other assets' folders (`others`) also name (ruling
/// T8 A-RL1): it reads right for one or several, and suggests only a valid name: the place's
/// canonical lowercase spelling, or, when even that isn't a valid `<project>/<asset_id>`, that
/// a valid name comes first.
fn claimants_refusal(place: &str, others: &[String]) -> String {
    let canonical = place.to_ascii_lowercase();
    let valid = canonical
        .split_once('/')
        .is_some_and(|(p, a)| is_slug(p) && is_asset_id(a));
    let who = listed(others);
    let (verb, pronoun, are) = if others.len() == 1 {
        ("names", "it", "it is another asset")
    } else {
        ("name", "they", "they are other assets")
    };
    let way_out = if valid {
        format!(
            "Rename the folder of the asset that had {canonical} back to {canonical}; then make \
             each other one independent (asset.make_independent), or, for one without a \
             creation UUID, set its manifest's asset_id to its folder by hand."
        )
    } else {
        "That place isn't a valid asset name, so none of them can move back there: give the \
         right folder's manifest a valid asset_id and project by hand first."
            .to_string()
    };
    format!(
        "{who} also {verb} {canonical} as the folder {pronoun} moved from, and {are} (another \
         creation UUID, or none), so this folder is no move from there. {way_out}"
    )
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
    let Some((row, _decided)) = locked_row(index, store, &to).await? else {
        return Err(refuse());
    };
    let Some(RepairKind::Moved { from, .. }) = kind_of(&row) else {
        let others = claimants(index, &row).await?;
        if others.is_empty() {
            return Err(refuse());
        }
        return Err(not_needed(&claimants_refusal(&place_of(&row), &others)));
    };
    let from = parse_name(&from).ok_or_else(|| invalid(format!("{from:?} is not a name")))?;
    // Whatever the index believes, the old place must be confirmed empty on disk (its
    // attributes give `NotFound`, the ghost clean-up's predicate); anything else, a folder (even
    // an empty one or one that can't be read), a file or an error, means this is no move, and
    // nothing is written (rulings T8 R1, R2-1). A listing only spells the name in the message.
    let (root, (p, a)) = (store.root().to_path_buf(), from.clone());
    let there = tokio::task::spawn_blocking(move || asset_folder_on_disk(&root, &p, &a))
        .await
        .unwrap_or(OnDisk::Unknown);
    if there != OnDisk::Absent {
        let spelled = match spelled_on_disk(store, &from).await {
            Some((p, Some(a))) => (p, a),
            _ => from.clone(),
        };
        let what = if there == OnDisk::File {
            "file"
        } else {
            "folder"
        };
        return Err(not_needed(&format!(
            "Something is still at this asset's old place: the {what} projects/{}/assets/{} \
             (even an empty folder, or one that can't be read, counts), so this folder is no \
             move. If what is there is left over, remove or rename it, then call \
             asset.accept_move again; otherwise project.status says what this folder is.",
            spelled.0, spelled.1
        )));
    }
    let uuid = uuid_key(&row);

    // The old name's lock, last: where old refs to it lead is checked, and the redirect
    // appended, with no other move from it in between.
    let from_lock = store.redirect_from_lock(&from.0, &from.1);
    let _from_held = from_lock.lock().await;
    let resolver = Resolver::load(index, store).await?;
    if let Some(e) = resolver.file_error() {
        return Err(e.clone());
    }
    let newest_to = resolver.newest_from(&from.0, &from.1).map(|r| r.to.clone());
    let present = newest_to.as_ref() == Some(&to);
    // Old refs to the old name already lead somewhere else: never to another live asset, nor
    // over an asset in the trash (ruling T8 A-RM2); a chain that ends nowhere (`target_missing`)
    // or loops (`cycle`) is fine.
    // Resolved also with no redirect from the old name: an asset trashed there through the
    // suite answers `asset.trashed` too (one rule for both trashed cases, ruling T8 A-R2M1).
    let end = if !present {
        Some(resolver.resolve_name(&from.0, &from.1).await)
    } else {
        None
    };
    if let Some(Err(e)) = &end
        && e.code == ASSET_TRASHED.code
    {
        let entry = e
            .data
            .as_ref()
            .and_then(|d| d.get("entry_id"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        return Err(not_needed(&format!(
            "Old refs to {}/{} lead to an asset in the trash (entry {entry}), so this folder \
             can't take them over by a move. To accept this move, empty trash entry {entry} \
             first, then call asset.accept_move again. Restoring that entry with trash.restore \
             instead puts that older asset back at {}/{}, and this folder is then a copy of it, \
             not a move.",
            from.0, from.1, from.0, from.1
        )));
    }
    if let Some(Ok(end)) = end {
        let same_folder =
            end.project.eq_ignore_ascii_case(&to.0) && end.asset_id.eq_ignore_ascii_case(&to.1);
        let end_uuid = index
            .asset(&end.project, &end.asset_id)
            .await
            .map_err(|e| e.to_error(""))?
            .and_then(|r| uuid_key(&r));
        if !same_folder && end_uuid != uuid {
            return Err(not_needed(&format!(
                "Old refs to {}/{} already lead to {}/{}, another asset (its creation UUID \
                 differs), so this folder can't take them over by a move. If this folder is the \
                 asset that name had, rename this folder back to {}/{}'s name ({}); \
                 asset.make_independent doesn't apply, as it isn't a copy.",
                from.0, from.1, end.project, end.asset_id, from.0, from.1, from.1
            )));
        }
    }
    drop(resolver);

    // (1) The redirect, unless the newest one from the old place already leads here.
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

/// The rest of a whole asset's `trash.restore {to}` (module docs): the folder is at
/// `projects/<project>/assets/<new_id>/` now, with the manifest it had as `old_id`. `by` and
/// `actor` are the redirect's. Returns whether this call appended the redirect. The caller
/// holds the version lock of `<project>/<new_id>`. The errors carry an empty trace id.
///
/// # Errors
/// The redirects file's error when it can't be read or parsed (the folder stays where it was
/// restored, and `asset.accept_move` finishes once the file is fixed); the store's errors.
pub async fn restored_as(
    index: &Index,
    store: &Store,
    project: &str,
    old_id: &str,
    new_id: &str,
    by: &str,
    actor: Option<Actor>,
) -> Result<bool, ErrorBody> {
    let from: Name = (project.to_string(), old_id.to_string());
    let to: Name = (project.to_string(), new_id.to_string());
    // (1) The redirect, under the old name's lock, as an accepted move appends it.
    let written = {
        let from_lock = store.redirect_from_lock(&from.0, &from.1);
        let _from_held = from_lock.lock().await;
        let resolver = Resolver::load(index, store).await?;
        if let Some(e) = resolver.file_error() {
            return Err(e.clone());
        }
        let present = resolver.newest_from(&from.0, &from.1).map(|r| &r.to) == Some(&to);
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
        !present
    };
    // (2) The manifest's file name, then (3) its `asset_id`.
    let dir = dir_of(&to)?;
    let canonical = dir
        .join(&format!("{new_id}.manifest.json"))
        .map_err(|e| store_error(&e))?;
    let old_file = dir
        .join(&format!("{old_id}.manifest.json"))
        .map_err(|e| store_error(&e))?;
    let has = |rel: &WsPath| {
        let abs = rel.abs(store.root());
        let probe = store.probe().clone();
        async move {
            tokio::task::spawn_blocking(move || probe.identity(&abs).is_ok())
                .await
                .unwrap_or(false)
        }
    };
    if !has(&canonical).await && has(&old_file).await {
        store
            .rename_new(&old_file, &canonical)
            .await
            .map_err(|e| store_error(&e))?;
    }
    let (old, new) = (old_id.to_string(), new_id.to_string());
    let edit = move |m: &mut Manifest| {
        if m.get("asset_id").and_then(Value::as_str) == Some(old.as_str()) {
            m.set("asset_id", Value::from(new.clone()));
        }
        Ok(())
    };
    store
        .edit_manifest(&canonical, &edit)
        .await
        .map_err(|e| store_error(&e))?;
    index
        .reconcile(Scope::Paths(vec![dir_of(&from)?, dir]))
        .await
        .map_err(|e| e.to_error(""))?;
    index.refresh_repairs().await.map_err(|e| e.to_error(""))?;
    Ok(written)
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
    let Some((row, _decided)) = locked_row(index, store, &here).await? else {
        return Err(refuse());
    };
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
