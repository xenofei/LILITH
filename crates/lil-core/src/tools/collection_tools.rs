//! The saved search and collection tools (CORE-155; P4b-R27; Appendix H "Organising tools"),
//! module `workspace`, over `registry/collections.yaml` (`pipeline::collections`).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `collections.list` | read | `{}` | `{collections: [{id, name, kind: "search" \| "collection", builtin?: "quick", rule?, refs?: [{ref, state: "ok" \| "redirected" \| "missing" \| "trashed", to?}]}]}` |
//! | `collection.save` | write | `{id?, name?, rule? \| refs? \| add?/remove?}` | `{collection}` |
//! | `collection.delete` | destructive | `{id}` | `{entry_id}` |
//!
//! `collection.save` without `id` makes a new entry (a fresh UUID v4) and needs `name` and
//! exactly one of `rule` (a saved search: an `assets.query` filter set without `sort`, `limit`,
//! `cursor` and `facets`, validated as one by [`rule_query`]) or `refs` (a collection of
//! `<project>/<asset_id>` names). With `id` it changes the parts given: `name`, or one of
//! `rule` (the entry becomes a saved search), `refs` (a collection) or `add`/`remove` (a
//! collection's refs; a ref is never listed twice, removing one that isn't there changes
//! nothing). Names are unique case-insensitively (`collection.name_taken`). The quick
//! collection (the nil UUID) takes refs only, can be renamed, and is never deleted
//! (`collection.delete` of it is `params.invalid`, P4b-R27). `collection.delete` is
//! `destructive`, as `resource.delete` is: an agent's call waits for a confirmation, which lists
//! `registry/collections.yaml` (`write`). The deleted entry goes to the trash and comes back with
//! `trash.restore` (`tools/trash_tools.rs`).
//!
//! Every change is published on the topic `workspace` as `{kind: "collection", id, previous?}`
//! (`previous`: the entry before the change, `{id, name, kind, builtin?, rule? | refs?}`; absent
//! for a new entry).

use lil_protocol::codes::PARAMS_INVALID;
use lil_protocol::{EffectClass, ErrorBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::asset_read_tools::rule_query;
use super::registry::{CallCtx, Registry, ToolMeta, TouchDecl, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::index::CollectionFilter;
use crate::pipeline::collections::{self, Body, COLLECTIONS_YAML, Collection, RefState, is_uuid};
use crate::projects::Resolver;
use crate::projects::redirects::parse_name;
use crate::workspace::OpenWorkspace;

/// The most refs one collection holds (each is one query parameter pair).
pub const MAX_REFS: usize = 10_000;

const LIST_DESCRIPTION: &str = "List the workspace's saved searches and collections \
    (registry/collections.yaml), the quick collection first: each with its id, name and kind \
    (search or collection); a search with its rule (an assets.query filter set), a collection \
    with its refs and each ref's state (ok, redirected with to, missing, trashed). Pass an id to \
    assets.query as collection to list its assets.";

const SAVE_DESCRIPTION: &str = "Save a saved search or a collection. Without id: a new entry, \
    with name and exactly one of rule (assets.query filters without sort, limit, cursor and \
    facets) or refs (<project>/<asset_id> names). With id: change its name, or set its rule or \
    refs, or add/remove refs of a collection. Names are unique whatever their case. The quick \
    collection's id is 00000000-0000-0000-0000-000000000000.";

const DELETE_DESCRIPTION: &str = "Delete a saved search or collection by id: it goes to the \
    trash as a one-entry file and comes back with trash.restore. The quick collection is never \
    deleted.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {}

/// An entry as `collections.list` gives it.
#[derive(Debug, Serialize, JsonSchema)]
struct CollectionView {
    id: String,
    name: String,
    /// `search` or `collection`.
    kind: &'static str,
    /// `quick` for the quick collection.
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin: Option<String>,
    /// A saved search's `assets.query` filters.
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refs: Option<Vec<RefState>>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ListResult {
    collections: Vec<CollectionView>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    /// The entry to change; omitted: a new entry.
    id: Option<String>,
    name: Option<String>,
    /// A saved search's `assets.query` filters (no `sort`, `limit`, `cursor`, `facets`).
    rule: Option<Map<String, Value>>,
    /// A collection's `<project>/<asset_id>` names, replacing its refs.
    #[schemars(length(max = MAX_REFS))]
    refs: Option<Vec<String>>,
    /// Refs to add to a collection.
    #[schemars(length(max = MAX_REFS))]
    add: Option<Vec<String>>,
    /// Refs to take out of a collection.
    #[schemars(length(max = MAX_REFS))]
    remove: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SaveResult {
    collection: CollectionView,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    /// The saved search's or collection's id.
    id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DeleteResult {
    /// The trash entry it went to.
    entry_id: String,
}

fn invalid(why: String) -> ErrorBody {
    ErrorBody::new(&PARAMS_INVALID, why, "")
}

fn traced(ctx: &CallCtx, mut e: ErrorBody) -> ErrorBody {
    e.trace_id.clone_from(&ctx.trace_id);
    e
}

/// `c` as listed, its refs' states read through `resolver`.
async fn view(resolver: &Resolver<'_>, c: Collection) -> Result<CollectionView, ErrorBody> {
    let kind = c.kind();
    let (rule, refs) = match c.body {
        Body::Rule(rule) => (Some(rule), None),
        Body::Refs(refs) => (None, Some(collections::ref_states(resolver, &refs).await?)),
    };
    Ok(CollectionView {
        id: c.id,
        name: c.name,
        kind,
        builtin: c.builtin,
        rule,
        refs,
    })
}

async fn list(ctx: &CallCtx, _args: ListArgs) -> Result<(ListResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let file = collections::read_collections(&open.store)
        .await
        .map_err(|e| e.to_error(&ctx.trace_id))?;
    let resolver = Resolver::load(&open.index, &open.store)
        .await
        .map_err(|e| traced(ctx, e))?;
    let mut out = Vec::new();
    for c in collections::listed(file) {
        out.push(view(&resolver, c).await.map_err(|e| traced(ctx, e))?);
    }
    let summary = format!(
        "{} saved search{} and collection{}",
        out.len(),
        if out.len() == 1 { "" } else { "es" },
        if out.len() == 1 { "" } else { "s" }
    );
    Ok((ListResult { collections: out }, summary))
}

/// `refs` checked as `<project>/<asset_id>` names, each once, in order.
fn checked_refs(what: &str, refs: &[String]) -> Result<Vec<String>, ErrorBody> {
    let mut out: Vec<String> = Vec::with_capacity(refs.len());
    for r in refs {
        if parse_name(r).is_none() {
            return Err(invalid(format!(
                "{what} holds {r:?}, not a `<project>/<asset_id>` ref (lowercase, as on disk)"
            )));
        }
        if !out.contains(r) {
            out.push(r.clone());
        }
    }
    Ok(out)
}

/// The arguments of a save, checked: what the entry becomes.
struct Plan {
    name: Option<String>,
    rule: Option<Map<String, Value>>,
    refs: Option<Vec<String>>,
    add: Vec<String>,
    remove: Vec<String>,
}

fn plan(args: &SaveArgs) -> Result<Plan, ErrorBody> {
    let name = match &args.name {
        Some(n) if n.trim().is_empty() => {
            return Err(invalid("name is empty".to_string()));
        }
        Some(n) => Some(n.clone()),
        None => None,
    };
    let changes = usize::from(args.rule.is_some())
        + usize::from(args.refs.is_some())
        + usize::from(args.add.is_some() || args.remove.is_some());
    if changes > 1 {
        return Err(invalid(
            "give at most one of rule, refs, or add/remove".to_string(),
        ));
    }
    if let Some(rule) = &args.rule {
        rule_query(rule).map_err(|why| invalid(format!("rule: {why}")))?;
    }
    let refs = args
        .refs
        .as_deref()
        .map(|r| checked_refs("refs", r))
        .transpose()?;
    let add = checked_refs("add", args.add.as_deref().unwrap_or_default())?;
    let remove = checked_refs("remove", args.remove.as_deref().unwrap_or_default())?;
    if args.id.is_none() {
        if name.is_none() || (args.rule.is_none() && refs.is_none()) {
            return Err(invalid(
                "a new saved search or collection needs name and one of rule or refs".to_string(),
            ));
        }
    } else if name.is_none() && changes == 0 {
        return Err(invalid(
            "nothing to change: give name, rule, refs, or add/remove".to_string(),
        ));
    }
    Ok(Plan {
        name,
        rule: args.rule.clone(),
        refs,
        add,
        remove,
    })
}

/// The entry `current` (`None`: a new one with `id`) after `plan`.
fn apply(id: &str, plan: &Plan, current: Option<&Collection>) -> Result<Collection, ErrorBody> {
    let mut c = match current {
        Some(c) => c.clone(),
        None => Collection {
            id: id.to_string(),
            name: String::new(),
            builtin: None,
            body: Body::Refs(Vec::new()),
            extra: Vec::new(),
        },
    };
    if let Some(name) = &plan.name {
        c.name.clone_from(name);
    }
    if let Some(rule) = &plan.rule {
        if c.is_quick() {
            return Err(invalid(
                "the quick collection holds refs; it is never a saved search".to_string(),
            ));
        }
        c.body = Body::Rule(rule.clone());
    }
    if let Some(refs) = &plan.refs {
        c.body = Body::Refs(refs.clone());
    }
    if !plan.add.is_empty() || !plan.remove.is_empty() {
        let Body::Refs(refs) = &mut c.body else {
            return Err(invalid(format!(
                "{} is a saved search: add and remove change a collection's refs",
                c.name
            )));
        };
        for r in &plan.add {
            if !refs.contains(r) {
                refs.push(r.clone());
            }
        }
        refs.retain(|r| !plan.remove.contains(r));
    }
    if let Body::Refs(refs) = &c.body
        && refs.len() > MAX_REFS
    {
        return Err(invalid(format!(
            "a collection holds at most {MAX_REFS} refs; this one would hold {}",
            refs.len()
        )));
    }
    Ok(c)
}

async fn save(ctx: &CallCtx, args: SaveArgs) -> Result<(SaveResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let plan = plan(&args).map_err(|e| traced(ctx, e))?;
    let (id, create) = match &args.id {
        Some(id) if !is_uuid(id) => {
            return Err(traced(
                ctx,
                invalid(format!("id is {id:?}, not a lowercase UUID")),
            ));
        }
        Some(id) => (id.clone(), false),
        None => (open.store.ids().uuid(), true),
    };
    let change = |current: Option<&Collection>| apply(&id, &plan, current);
    let (saved, previous) = collections::save(&open.store, &id, create, &change)
        .await
        .map_err(|e| traced(ctx, e))?;
    if previous.as_ref() != Some(&saved) {
        let mut event = json!({"kind": "collection", "id": saved.id});
        if let Some(previous) = &previous {
            event["previous"] = previous.to_json();
        }
        ctx.events.publish("workspace", event);
    }
    let resolver = Resolver::load(&open.index, &open.store)
        .await
        .map_err(|e| traced(ctx, e))?;
    let summary = format!("saved {} ({})", saved.name, saved.id);
    let collection = view(&resolver, saved).await.map_err(|e| traced(ctx, e))?;
    Ok((SaveResult { collection }, summary))
}

async fn delete(ctx: &CallCtx, args: DeleteArgs) -> Result<(DeleteResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    if !is_uuid(&args.id) {
        return Err(traced(
            ctx,
            invalid(format!("id is {:?}, not a lowercase UUID", args.id)),
        ));
    }
    let (record, deleted) = collections::delete(&open.store, &args.id)
        .await
        .map_err(|e| traced(ctx, e))?;
    ctx.events.publish(
        "workspace",
        json!({"kind": "collection", "id": deleted.id, "previous": deleted.to_json()}),
    );
    let summary = format!(
        "moved {} to the trash (entry {})",
        deleted.name, record.entry_id
    );
    Ok((
        DeleteResult {
            entry_id: record.entry_id,
        },
        summary,
    ))
}

/// `assets.query`'s `collection` filter for entry `id` (CORE-155): a saved search's rule as its
/// filters, a collection's refs resolved through the redirects to the live assets they reach.
///
/// # Errors
/// `collection.unknown`; `params.invalid` for a saved search whose rule (edited by hand) isn't
/// a filter set; the file's and the resolver's errors.
pub async fn collection_filter(
    open: &OpenWorkspace,
    id: &str,
) -> Result<CollectionFilter, ErrorBody> {
    if !is_uuid(id) {
        return Err(collections::unknown(id));
    }
    let c = collections::find(&open.store, id).await?;
    match c.body {
        Body::Rule(rule) => rule_query(&rule)
            .map(|q| CollectionFilter::Rule(Box::new(q)))
            .map_err(|why| {
                invalid(format!(
                    "The saved search {} ({COLLECTIONS_YAML}) has a rule assets.query doesn't \
                     take: {why}",
                    c.name
                ))
            }),
        Body::Refs(refs) => {
            let resolver = Resolver::load(&open.index, &open.store).await?;
            Ok(CollectionFilter::Refs(
                collections::live_refs(&resolver, &refs).await?,
            ))
        }
    }
}

/// `collection.delete`'s meta: it writes the collections file (P4b-R33).
fn delete_meta() -> ToolMeta {
    ToolMeta {
        touches: vec![TouchDecl {
            template: COLLECTIONS_YAML,
            access: lil_protocol::app::TouchAccess::Write,
        }],
        ..ToolMeta::default()
    }
}

/// Registers `collections.list`, `collection.save` and `collection.delete` (module
/// `workspace`).
pub fn register_collection_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "collections.list",
        LIST_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        |ctx, args: ListArgs| Box::pin(list(ctx, args)),
    ));
    reg.register(typed_tool(
        "collection.save",
        SAVE_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: SaveArgs| Box::pin(save(ctx, args)),
    ));
    reg.register_with(
        typed_tool(
            "collection.delete",
            DELETE_DESCRIPTION,
            EffectClass::Destructive,
            WORKSPACE_MODULE,
            |ctx, args: DeleteArgs| Box::pin(delete(ctx, args)),
        ),
        delete_meta(),
    );
}
