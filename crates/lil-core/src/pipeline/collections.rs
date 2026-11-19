//! Saved searches and collections (CORE-155; P4b-R27; Appendix H "`registry/collections.yaml`"
//! and "Organising tools"): the tracked workspace file `registry/collections.yaml`,
//! `collections: [{id, name, builtin?, rule? | refs?}]`, changed with targeted YAML edits
//! (ARCH-076). This module reads and edits the file; `tools/collection_tools.rs` validates the
//! arguments (a saved search's `rule` is validated there, as an `assets.query` filter set, so
//! this module never names `tools`).
//!
//! **The file** is read strictly ([`parse_collections`]): an entry whose `id` isn't a UUID
//! (lowercase, hyphenated), whose `name` isn't a non-empty string, that has both or neither
//! of `rule` (a mapping) and `refs` (a list of `<project>/<asset_id>` names), or an `id` used
//! twice, makes the whole file refused (`yaml.unsupported_edit`: fix it by hand first), never
//! read as fewer entries. An entry's other keys are kept as they are, after its own.
//!
//! **The quick collection** (P4b-R27) is the entry with the nil UUID [`QUICK_COLLECTION_ID`],
//! named "Quick collection", `builtin: quick`: listed first whether or not the file holds it
//! (synthesised, with no refs, until its first save), never deleted, never a saved search.
//!
//! **Writes** take the store's collections lock ([`Store::collections_lock`]) around their read,
//! trash and edit, and edit through the store's hash-checked text edit, which re-applies the
//! change to the file as it is now after an outside change; so two saves never lose one.
//! A new entry is appended; a changed entry is rewritten in place (its own lines only); a
//! deleted entry's lines go, and the last one leaves `collections: []`.
//!
//! **Delete** ([`delete`]) writes the trash payload first, then the YAML edit (Appendix E):
//! the entry as a one-entry file `payload/collection-<id>.yaml`, `original_path`
//! `registry/collections.yaml`, `reason` `collection.delete`, then the fail point
//! `collections.after_payload`, then the edit. A retry after a failure between them finds the
//! earlier payload of that entry (same reason, original path and SHA-256 of the same bytes) and
//! reuses it, so one trash entry is left. When the entry changed in the file since it was read
//! (an outside edit), the delete is refused (`store.hash_conflict`), the file is left as it is,
//! and the payload this call made leaves the trash again.
//!
//! **Restore** ([`restore`]) merges the payload's entry back by `id`, appended after the
//! entries there: refused with `trash.restore_conflict` when its `id` or its name (compared
//! case-insensitively) is in use, and nothing is replaced. The merge edit is the commit point;
//! the entry leaves the trash after it, best effort (a failure there is a warning). An entry
//! already back exactly as it was (a restore cut short after its edit) just leaves the trash.

use std::sync::Mutex;

use lil_protocol::ErrorBody;
use lil_protocol::codes::{
    ASSET_TRASHED, ASSET_UNKNOWN, COLLECTION_NAME_TAKEN, COLLECTION_UNKNOWN, PARAMS_INVALID,
    REF_REDIRECT_BROKEN, TRASH_RESTORE_CONFLICT,
};
use lil_schema::workspace_yaml::read_yaml_mapping;
use lil_schema::{YamlEdit, YamlValue, yaml_edit};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::projects::Resolver;
use crate::projects::redirects::parse_name;
use crate::store::{Store, StoreError, TrashRecord, WsPath};

/// The collections file, workspace-relative.
pub const COLLECTIONS_YAML: &str = "registry/collections.yaml";
/// The quick collection's id: the nil UUID (P4b-R27).
pub const QUICK_COLLECTION_ID: &str = "00000000-0000-0000-0000-000000000000";
/// The quick collection's name until it is renamed.
pub const QUICK_COLLECTION_NAME: &str = "Quick collection";
/// The quick collection's `builtin`.
pub const QUICK_BUILTIN: &str = "quick";
/// The trash `reason` of a deleted entry.
pub const DELETE_REASON: &str = "collection.delete";
/// The fail point between a delete's trash payload and its YAML edit.
pub const AFTER_PAYLOAD: &str = "collections.after_payload";

/// What an entry holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// A saved search: an `assets.query` filter set, as written.
    Rule(Map<String, Value>),
    /// A hand-picked collection: `<project>/<asset_id>` names, in order.
    Refs(Vec<String>),
}

/// One entry of `registry/collections.yaml`.
#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub builtin: Option<String>,
    pub body: Body,
    /// Keys the suite doesn't know, as written, in order.
    pub extra: Vec<(String, Value)>,
}

impl Collection {
    /// `search` or `collection`.
    pub fn kind(&self) -> &'static str {
        match self.body {
            Body::Rule(_) => "search",
            Body::Refs(_) => "collection",
        }
    }

    pub fn is_quick(&self) -> bool {
        self.id == QUICK_COLLECTION_ID
    }

    /// The entry as an event's `previous`: `{id, name, kind, builtin?, rule? | refs?}`.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), Value::from(self.id.as_str()));
        out.insert("name".into(), Value::from(self.name.as_str()));
        out.insert("kind".into(), Value::from(self.kind()));
        if let Some(b) = &self.builtin {
            out.insert("builtin".into(), Value::from(b.as_str()));
        }
        match &self.body {
            Body::Rule(rule) => out.insert("rule".into(), Value::Object(rule.clone())),
            Body::Refs(refs) => out.insert("refs".into(), Value::from(refs.clone())),
        };
        Value::Object(out)
    }
}

/// The quick collection as it is before its first save.
pub fn quick_collection() -> Collection {
    Collection {
        id: QUICK_COLLECTION_ID.to_string(),
        name: QUICK_COLLECTION_NAME.to_string(),
        builtin: Some(QUICK_BUILTIN.to_string()),
        body: Body::Refs(Vec::new()),
        extra: Vec::new(),
    }
}

/// Whether `s` is a UUID, lowercase and hyphenated (`8-4-4-4-12` hex digits).
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

/// How a name is compared: case-insensitively.
pub fn name_key(name: &str) -> String {
    name.to_lowercase()
}

/// The collections file's path.
pub fn collections_path() -> WsPath {
    WsPath::parse(COLLECTIONS_YAML).expect("registry/collections.yaml is a workspace path")
}

/// Parses the text of `registry/collections.yaml` (module docs), entries in file order.
///
/// # Errors
/// Why the file can't be read as collections, naming the entry (1-based) at fault.
pub fn parse_collections(text: &str) -> Result<Vec<Collection>, String> {
    let root = read_yaml_mapping(text, COLLECTIONS_YAML)?;
    let list = match root.get("collections") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("`collections` is not a list".to_string()),
    };
    let mut out: Vec<Collection> = Vec::with_capacity(list.len());
    for (i, entry) in list.iter().enumerate() {
        let c = parse_entry(entry).map_err(|why| format!("entry {}: {why}", i + 1))?;
        if out.iter().any(|o| o.id == c.id) {
            return Err(format!("entry {}: the id {} is used twice", i + 1, c.id));
        }
        out.push(c);
    }
    Ok(out)
}

fn parse_entry(entry: &Value) -> Result<Collection, String> {
    let Value::Object(map) = entry else {
        return Err("not a mapping".to_string());
    };
    let id = match map.get("id") {
        Some(Value::String(s)) if is_uuid(s) => s.clone(),
        Some(v) => return Err(format!("`id` is {v}, not a lowercase UUID")),
        None => return Err("`id` is missing".to_string()),
    };
    let name = match map.get("name") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        _ => return Err("`name` is not a non-empty string".to_string()),
    };
    let builtin = match map.get("builtin") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err("`builtin` is not a string".to_string()),
    };
    let body = match (map.get("rule"), map.get("refs")) {
        (Some(Value::Object(rule)), None) => Body::Rule(rule.clone()),
        (None, Some(Value::Array(refs))) => Body::Refs(
            refs.iter()
                .map(|r| match r.as_str() {
                    Some(s) if parse_name(s).is_some() => Ok(s.to_string()),
                    _ => Err(format!(
                        "`refs` holds {r}, not a `<project>/<asset_id>` name"
                    )),
                })
                .collect::<Result<_, _>>()?,
        ),
        (Some(_), Some(_)) => return Err("it has both `rule` and `refs`".to_string()),
        (None, None) => return Err("it has neither `rule` nor `refs`".to_string()),
        (Some(_), None) => return Err("`rule` is not a mapping".to_string()),
        (None, Some(_)) => return Err("`refs` is not a list".to_string()),
    };
    let extra = map
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "id" | "name" | "builtin" | "rule" | "refs"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(Collection {
        id,
        name,
        builtin,
        body,
        extra,
    })
}

/// The error of a collections file that doesn't parse.
fn unreadable(why: &str) -> StoreError {
    StoreError::YamlUnsupported(format!(
        "{COLLECTIONS_YAML} doesn't parse ({why}); fix it by hand first"
    ))
}

/// The text's entries, or the file's error.
fn entries_in(text: &str) -> Result<Vec<Collection>, StoreError> {
    parse_collections(text).map_err(|why| unreadable(&why))
}

/// Reads `registry/collections.yaml`: its entries in file order, none when there is no file.
///
/// # Errors
/// `yaml.unsupported_edit` when it isn't collections (module docs); the store's read error.
pub async fn read_collections(store: &Store) -> Result<Vec<Collection>, StoreError> {
    let Some(bytes) = store.read(&collections_path()).await? else {
        return Ok(Vec::new());
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| unreadable("it is not UTF-8"))?;
    entries_in(text)
}

/// Every entry as listed: the quick collection first (synthesised when the file doesn't hold
/// it), then the file's others in file order.
pub fn listed(file: Vec<Collection>) -> Vec<Collection> {
    let quick = file
        .iter()
        .find(|c| c.is_quick())
        .cloned()
        .unwrap_or_else(quick_collection);
    std::iter::once(quick)
        .chain(file.into_iter().filter(|c| !c.is_quick()))
        .collect()
}

/// Entry `id` as listed (the quick collection before its first save included).
///
/// # Errors
/// `collection.unknown`; the file's errors (as [`read_collections`]).
pub async fn find(store: &Store, id: &str) -> Result<Collection, ErrorBody> {
    let all = listed(read_collections(store).await.map_err(|e| e.to_error(""))?);
    all.into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| unknown(id))
}

/// `collection.unknown` for `id`.
pub fn unknown(id: &str) -> ErrorBody {
    let mut e = ErrorBody::new(
        &COLLECTION_UNKNOWN,
        format!(
            "There is no saved search or collection with the id {id}; collections.list lists them."
        ),
        "",
    );
    e.data = Some(serde_json::json!({ "id": id }));
    e
}

/// `collection.name_taken` for `name`.
fn name_taken(name: &str) -> ErrorBody {
    let mut e = ErrorBody::new(
        &COLLECTION_NAME_TAKEN,
        format!("A saved search or collection is already called {name}."),
        "",
    );
    e.data = Some(serde_json::json!({ "name": name }));
    e
}

fn invalid(why: String) -> ErrorBody {
    ErrorBody::new(&PARAMS_INVALID, why, "")
}

/// A JSON value as a value the YAML editor writes.
fn yaml_value(key: &str, v: &Value) -> Result<YamlValue, StoreError> {
    Ok(match v {
        Value::String(s) => YamlValue::Str(s.clone()),
        Value::Bool(b) => YamlValue::Bool(*b),
        Value::Number(n) if n.is_i64() => YamlValue::Int(n.as_i64().expect("an i64")),
        Value::Array(items) => YamlValue::List(
            items
                .iter()
                .map(|i| yaml_value(key, i))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => YamlValue::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), yaml_value(key, v)?)))
                .collect::<Result<_, StoreError>>()?,
        ),
        Value::Null | Value::Number(_) => {
            return Err(StoreError::YamlUnsupported(format!(
                "`{key}` holds {v}, which the entry's rewrite can't keep; edit \
                 {COLLECTIONS_YAML} by hand"
            )));
        }
    })
}

/// `c` as the YAML editor's entries: `id`, `name`, `builtin`?, `rule` or `refs`, then its
/// other keys.
fn yaml_entries(c: &Collection) -> Result<Vec<(String, YamlValue)>, StoreError> {
    let mut out = vec![
        ("id".to_string(), YamlValue::Str(c.id.clone())),
        ("name".to_string(), YamlValue::Str(c.name.clone())),
    ];
    if let Some(b) = &c.builtin {
        out.push(("builtin".to_string(), YamlValue::Str(b.clone())));
    }
    match &c.body {
        Body::Rule(rule) => out.push((
            "rule".to_string(),
            yaml_value("rule", &Value::Object(rule.clone()))?,
        )),
        Body::Refs(refs) => out.push((
            "refs".to_string(),
            YamlValue::List(refs.iter().map(|r| YamlValue::Str(r.clone())).collect()),
        )),
    }
    for (k, v) in &c.extra {
        out.push((k.clone(), yaml_value(k, v)?));
    }
    Ok(out)
}

fn path() -> Vec<String> {
    vec!["collections".to_string()]
}

/// `text` with `edit` applied, checked to read back as collections.
fn edited(text: &str, edit: YamlEdit) -> Result<String, StoreError> {
    let after =
        yaml_edit::apply(text, &[edit]).map_err(|e| StoreError::YamlUnsupported(e.to_string()))?;
    entries_in(&after)?;
    Ok(after)
}

/// The one-entry file a deleted entry goes to the trash as.
fn one_entry_file(c: &Collection) -> Result<Vec<u8>, StoreError> {
    let text = edited(
        "",
        YamlEdit::AppendMappingToList {
            path: path(),
            entries: yaml_entries(c)?,
        },
    )?;
    Ok(text.into_bytes())
}

/// Runs a store edit whose closure may refuse with an [`ErrorBody`] of its own: the closure
/// puts it in `refused` and returns a placeholder error, which this replaces.
fn refusal(refused: &Mutex<Option<ErrorBody>>, e: StoreError) -> ErrorBody {
    match refused
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        Some(body) => body,
        None => e.to_error(""),
    }
}

/// What a save made of an entry.
pub type Change<'a> =
    dyn Fn(Option<&Collection>) -> Result<Collection, ErrorBody> + Send + Sync + 'a;

/// Saves entry `id` (module docs): `change` makes the new entry from the current one (`None`
/// for a new entry: `create` must be set, and `id` must be in no entry; else the entry, the
/// quick collection before its first save included, must exist: `collection.unknown`). The
/// name is checked to be free (case-insensitively) against every other listed entry
/// (`collection.name_taken`). Returns the saved entry and the one it replaced.
///
/// # Errors
/// `collection.unknown`; `collection.name_taken`; `change`'s own errors; the file's and the
/// store's errors.
pub async fn save(
    store: &Store,
    id: &str,
    create: bool,
    change: &Change<'_>,
) -> Result<(Collection, Option<Collection>), ErrorBody> {
    let lock = store.collections_lock();
    let _held = lock.lock().await;
    // A change that changes nothing never reaches the store's edit, which would create the file
    // for the quick collection before its first save (review M-1).
    if !create && let Ok(file) = read_collections(store).await {
        let current = listed(file).into_iter().find(|c| c.id == id);
        if let Some(current) = current
            && change(Some(&current)).is_ok_and(|new| new == current)
        {
            return Ok((current.clone(), Some(current)));
        }
    }
    let refused = Mutex::new(None);
    let saved = Mutex::new(None);
    let edit = |text: &str| -> Result<String, StoreError> {
        let refuse = |body: ErrorBody| {
            *refused
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(body);
            StoreError::Invalid(String::new())
        };
        let file = entries_in(text)?;
        let at = file.iter().position(|c| c.id == id);
        let all = listed(file.clone());
        let current = all.iter().find(|c| c.id == id);
        if create && current.is_some() {
            return Err(refuse(invalid(format!("the id {id} is in use"))));
        }
        if !create && current.is_none() {
            return Err(refuse(unknown(id)));
        }
        let new = change(current).map_err(refuse)?;
        let key = name_key(&new.name);
        if all.iter().any(|c| c.id != id && name_key(&c.name) == key) {
            return Err(refuse(name_taken(&new.name)));
        }
        *saved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((new.clone(), current.cloned()));
        // Nothing changes (the quick collection before its first save included): no write.
        if current == Some(&new) {
            return Ok(text.to_string());
        }
        let entries = yaml_entries(&new)?;
        match at {
            Some(index) => edited(
                text,
                YamlEdit::SetListItem {
                    path: path(),
                    index,
                    entries,
                },
            ),
            None => edited(
                text,
                YamlEdit::AppendMappingToList {
                    path: path(),
                    entries,
                },
            ),
        }
    };
    store
        .edit_text(&collections_path(), &edit)
        .await
        .map_err(|e| refusal(&refused, e))?;
    Ok(saved
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("the edit ran"))
}

/// Deletes entry `id` (module docs): the trash payload first, then the YAML edit. Returns the
/// trash record and the deleted entry.
///
/// # Errors
/// `params.invalid` for the quick collection; `collection.unknown`; the file's and the store's
/// errors (`store.io` for the fail point).
pub async fn delete(store: &Store, id: &str) -> Result<(TrashRecord, Collection), ErrorBody> {
    if id == QUICK_COLLECTION_ID {
        return Err(invalid(
            "The quick collection is never deleted; remove its refs with collection.save \
             {id, remove} instead."
                .to_string(),
        ));
    }
    let lock = store.collections_lock();
    let _held = lock.lock().await;
    let error = |e: StoreError| e.to_error("");
    let file = read_collections(store).await.map_err(error)?;
    let entry = file
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| unknown(id))?;
    let bytes = one_entry_file(&entry).map_err(error)?;
    let sha256 = lil_schema::sha256_hex(&bytes);
    // A retry after a failure past the payload reuses it (module docs).
    let earlier = store
        .trash_list()
        .await
        .map_err(error)?
        .into_iter()
        .find(|r| {
            r.reason == DELETE_REASON
                && r.original_path == COLLECTIONS_YAML
                && r.sha256.as_deref() == Some(sha256.as_str())
        });
    let made_now = earlier.is_none();
    let record = match earlier {
        Some(record) => record,
        None => store
            .trash_bytes(
                &format!("collection-{id}.yaml"),
                &bytes,
                &collections_path(),
                DELETE_REASON,
            )
            .await
            .map_err(error)?,
    };
    store
        .failpoints()
        .hit(AFTER_PAYLOAD)
        .map_err(|e| StoreError::Io {
            path: COLLECTIONS_YAML.to_string(),
            why: e.to_string(),
        })
        .map_err(error)?;
    let edit = |text: &str| -> Result<String, StoreError> {
        let file = entries_in(text)?;
        match file.iter().position(|c| c.id == id) {
            // Gone meanwhile (an outside edit): nothing left to take out.
            None => Ok(text.to_string()),
            // Changed meanwhile by an outside edit: the trash holds the entry as it was read,
            // so the newer one is never taken out (review M-3).
            Some(index) if file[index] != entry => Err(StoreError::HashConflict {
                path: collections_path(),
                backup: None,
                cloud_only: false,
                kept: Vec::new(),
            }),
            Some(index) => edited(
                text,
                YamlEdit::RemoveListItem {
                    path: path(),
                    index,
                },
            ),
        }
    };
    if let Err(e) = store.edit_text(&collections_path(), &edit).await {
        // Nothing changed in the file: the payload this call put in the trash goes again (one
        // reused from an earlier attempt stays, as that attempt left it).
        if made_now && let Err(dropped) = store.trash_drop_merged(&record.entry_id, &sha256).await {
            tracing::warn!(entry = %record.entry_id, error = %dropped, "a refused delete's trash entry stays");
        }
        return Err(error(e));
    }
    Ok((record, entry))
}

/// Restores trash entry `entry_id`, a deleted entry, by merging it back by id (module docs).
/// Returns the restored entry and the warnings of the steps after the commit (the merge edit):
/// an entry that couldn't leave the trash is logged and named there, never an error.
///
/// # Errors
/// `trash.unknown`; `trash.restore_conflict` when its id or name is in use; `params.invalid`
/// when the payload isn't one entry; the file's and the store's errors.
pub async fn restore(
    store: &Store,
    entry_id: &str,
) -> Result<(Collection, Vec<String>), ErrorBody> {
    let error = |e: StoreError| e.to_error("");
    let lock = store.collections_lock();
    let _held = lock.lock().await;
    let (_record, bytes) = store.trash_payload(entry_id).await.map_err(error)?;
    let back = std::str::from_utf8(&bytes)
        .map_err(|_| "it is not UTF-8".to_string())
        .and_then(parse_collections)
        .map_err(|why| {
            invalid(format!(
                "The trash entry {entry_id} isn't a saved search or collection ({why})."
            ))
        })?;
    let [back] = <[Collection; 1]>::try_from(back).map_err(|_| {
        invalid(format!(
            "The trash entry {entry_id} doesn't hold exactly one saved search or collection."
        ))
    })?;
    let refused = Mutex::new(None);
    let edit = |text: &str| -> Result<String, StoreError> {
        let refuse = |why: String| {
            let mut e = ErrorBody::new(&TRASH_RESTORE_CONFLICT, why, "");
            e.data = Some(serde_json::json!({ "path": COLLECTIONS_YAML, "id": back.id }));
            *refused
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
            StoreError::RestoreConflict(collections_path())
        };
        let file = entries_in(text)?;
        // Back exactly as it was: a restore cut short after its edit.
        if file.contains(&back) {
            return Ok(text.to_string());
        }
        if file.iter().any(|c| c.id == back.id) {
            return Err(refuse(format!(
                "A saved search or collection with the id {} is in {COLLECTIONS_YAML} already, \
                 so the entry wasn't restored; nothing was replaced.",
                back.id
            )));
        }
        let key = name_key(&back.name);
        if let Some(c) = listed(file).iter().find(|c| name_key(&c.name) == key) {
            return Err(refuse(format!(
                "A saved search or collection is already called {}, so {} wasn't restored; \
                 rename that one first (collection.save {{id: {}, name}}), then restore again.",
                c.name, back.name, c.id
            )));
        }
        edited(
            text,
            YamlEdit::AppendMappingToList {
                path: path(),
                entries: yaml_entries(&back)?,
            },
        )
    };
    store
        .edit_text(&collections_path(), &edit)
        .await
        .map_err(|e| refusal(&refused, e))?;
    // The merge edit was the commit point (Appendix E): the entry leaves the trash after it,
    // best effort.
    let mut warnings = Vec::new();
    if let Err(e) = store
        .trash_drop_merged(entry_id, &lil_schema::sha256_hex(&bytes))
        .await
    {
        tracing::warn!(entry = %entry_id, error = %e, "a restored collection's trash entry stays");
        warnings.push(format!(
            "{} is back in {COLLECTIONS_YAML}, but its trash entry {entry_id} couldn't be \
             removed ({e}); it is purged with the trash.",
            back.name
        ));
    }
    Ok((back, warnings))
}

/// A ref's state in `collections.list` (Appendix H).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RefState {
    /// `<project>/<asset_id>`, as saved.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// `ok`, `redirected`, `missing` or `trashed`.
    pub state: &'static str,
    /// Where a redirected ref leads now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// Where `r` leads: `Some((state, live name))`, the live name for `ok` and `redirected`.
async fn resolve_one(
    resolver: &Resolver<'_>,
    r: &str,
) -> Result<(&'static str, Option<(String, String)>), ErrorBody> {
    let Some((project, asset_id)) = parse_name(r) else {
        return Ok(("missing", None));
    };
    match resolver.resolve_name(&project, &asset_id).await {
        Ok(res) => {
            let state = if res.redirected_from.is_empty() {
                "ok"
            } else {
                "redirected"
            };
            Ok((state, Some((res.project, res.asset_id))))
        }
        Err(e) if e.code == ASSET_TRASHED.code => Ok(("trashed", None)),
        Err(e) if e.code == ASSET_UNKNOWN.code || e.code == REF_REDIRECT_BROKEN.code => {
            Ok(("missing", None))
        }
        Err(e) => Err(e),
    }
}

/// Checks the liveness of every ref's name in one batch (review M-5).
async fn preload(resolver: &Resolver<'_>, refs: &[String]) {
    let names: Vec<(String, String)> = refs.iter().filter_map(|r| parse_name(r)).collect();
    resolver.preload_live(&names).await;
}

/// The state of each of `refs`, in order (Appendix H).
///
/// # Errors
/// What the resolver can't answer for a ref: the redirects file's `manifest.needs_repair`,
/// `store.io`.
pub async fn ref_states(
    resolver: &Resolver<'_>,
    refs: &[String],
) -> Result<Vec<RefState>, ErrorBody> {
    preload(resolver, refs).await;
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let (state, live) = resolve_one(resolver, r).await?;
        let to = (state == "redirected")
            .then(|| live.map(|(p, a)| format!("{p}/{a}")))
            .flatten();
        out.push(RefState {
            r#ref: r.clone(),
            state,
            to,
        });
    }
    Ok(out)
}

/// The live assets `refs` lead to, through the redirects, each once, in order; a ref that
/// leads nowhere (missing or trashed) is left out.
///
/// # Errors
/// As [`ref_states`].
pub async fn live_refs(
    resolver: &Resolver<'_>,
    refs: &[String],
) -> Result<Vec<(String, String)>, ErrorBody> {
    preload(resolver, refs).await;
    let mut out: Vec<(String, String)> = Vec::new();
    for r in refs {
        if let (_, Some(live)) = resolve_one(resolver, r).await?
            && !out.contains(&live)
        {
            out.push(live);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-155
    fn the_file_is_read_strictly() {
        let ok = "# c\ncollections:\n  - id: 11111111-2222-4333-8444-555555555555\n    name: A\n    refs: [s/a]\n    note: x\n  - id: 00000000-0000-0000-0000-000000000000\n    name: Quick collection\n    builtin: quick\n    refs: []\n  - {id: 11111111-2222-4333-8444-666666666666, name: B, rule: {type: image}}\n";
        let read = parse_collections(ok).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].body, Body::Refs(vec!["s/a".into()]));
        assert_eq!(read[0].extra, [("note".to_string(), Value::from("x"))]);
        assert!(read[1].is_quick());
        assert_eq!(read[2].kind(), "search");
        let ids: Vec<String> = listed(read).into_iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                QUICK_COLLECTION_ID,
                "11111111-2222-4333-8444-555555555555",
                "11111111-2222-4333-8444-666666666666"
            ]
        );
        for empty in ["", "# only\n", "collections:\n", "collections: []\n"] {
            assert_eq!(parse_collections(empty).unwrap(), Vec::new(), "{empty:?}");
        }
        let id = "abcdef11-2222-4333-8444-555555555555";
        for bad in [
            "collections: x\n".to_string(),
            "collections:\n  - x\n".to_string(),
            "collections:\n  - {id: nope, name: A, refs: []}\n".to_string(),
            format!(
                "collections:\n  - {{id: {}, name: A, refs: []}}\n",
                id.to_uppercase()
            ),
            format!("collections:\n  - {{id: {id}, name: '', refs: []}}\n"),
            format!("collections:\n  - {{id: {id}, name: A}}\n"),
            format!("collections:\n  - {{id: {id}, name: A, refs: [], rule: {{}}}}\n"),
            format!("collections:\n  - {{id: {id}, name: A, refs: [Bad/ref]}}\n"),
            format!("collections:\n  - {{id: {id}, name: A, rule: [x]}}\n"),
            format!(
                "collections:\n  - {{id: {id}, name: A, refs: []}}\n  - {{id: {id}, name: B, refs: []}}\n"
            ),
        ] {
            assert!(parse_collections(&bad).is_err(), "{bad:?}");
        }
    }

    #[test] // spec: CORE-155
    fn a_deleted_entry_is_one_entry_file_that_reads_back() {
        let c = Collection {
            id: "11111111-2222-4333-8444-555555555555".into(),
            name: "Props, all".into(),
            builtin: None,
            body: Body::Rule(
                serde_json::json!({"type": ["prop_2d"], "rating_min": 3, "duplicates": true})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            extra: vec![("note".into(), Value::from("kept"))],
        };
        let bytes = one_entry_file(&c).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with("collections:\n  - id: 11111111"), "{text}");
        assert_eq!(parse_collections(&text).unwrap(), std::slice::from_ref(&c));
        assert_eq!(
            one_entry_file(&c).unwrap(),
            bytes,
            "the same bytes every time"
        );
        let mut odd = c;
        odd.extra = vec![("weight".into(), serde_json::json!(1.5))];
        assert!(one_entry_file(&odd).is_err());
    }
}
