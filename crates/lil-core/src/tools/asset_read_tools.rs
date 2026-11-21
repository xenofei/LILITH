//! The asset read tools (Global Constraints, Product tools; APP-025), module `workspace`. They
//! parse their arguments and call the index and `pipeline::asset_detail`; both wait for the
//! workspace's first reconcile or cold rebuild ([`Index::ready`](crate::index::Index::ready)),
//! with no timeout of their own, so a fresh open never answers with an empty or partial page.
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `assets.query` | read | `{project?, status?, type?, text?, tags?, rating_min?, label?, flag?, collection?, duplicates?, deprecated?, facets?, sort?, limit?, cursor?}` | `{assets: [AssetSummary], next_cursor: string\|null, total, facets?, tags_more?}` |
//! | `asset.get` | read | `{project?, asset_id}` | `AssetDetail` |
//! | `asset.get_version` | read | `{project?, asset_id, version}` | `{version, ref, files: [{path, sha256, media_kind, primary}], companions, recipe, inputs, signed_off, restored_from?, trust}` |
//!
//! `assets.query`'s `project` is a filter: omitted, it means every project (the session's
//! active project never narrows it). `status` and `type` take a string or a list (any of);
//! `label` takes `none` for an unlabelled asset; `deprecated` defaults to `show` (Appendix H
//! "`assets.query`"; CORE-152, CORE-168, CORE-170, P4b-R10, P4b-R11). `collection` is a saved
//! search's or collection's id (CORE-155): a search adds its rule's filters, a collection its
//! refs, resolved through the redirects (`collection.unknown` for an id no entry has; the
//! quick collection is known before its first save). [`rule_query`] validates a saved search's
//! rule as this filter set. With `facets: true` the
//! result has `facets: {status, type, tags, rating, label, flag}` and, beside it, `tags_more`
//! (PR #89 audit, L3). `asset.get`'s omitted `project` is the call's resolved project
//! (`CallCtx::project`: the session's active project, else the workspace's hint, else
//! `sketchbook`).
//!
//! `asset.get_version` (CORE-177, APP-141; Appendix H) reads one sealed version from its
//! folder and its `manifest.snapshot.json`, through the store (no index wait, no lock): every
//! file in the folder but the snapshot, as `v<version>/<path>`, with the manifest's recorded
//! SHA-256 (else the file's, `null` when it can't be read) and its media kind; `primary` for a
//! file the sign-off chose that isn't a companion. `recipe` is the snapshot's, `inputs` the
//! snapshot's top-level `inputs` and then its run's `inputs`, once per `ref`, `signed_off`
//! the snapshot's run's `signed_off` as written, `restored_from` that run's, and
//! `trust` the manifest's records under `v<version>/`. A legacy version (no snapshot) gives
//! its files and hashes only. An unknown asset or version is `asset.unknown`.

use lil_protocol::codes::{ASSET_UNKNOWN, PARAMS_INVALID};
use lil_protocol::{EffectClass, ErrorBody};
use lil_schema::{AssetRef, Flag, Label, Manifest, Version, media_kind_of};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::collection_tools::collection_filter;
use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::index::{
    AssetQuery, Choice, DeprecatedFilter, FacetCount, Facets, SortDir, SortField, decode_cursor,
    encode_cursor,
};
use crate::pipeline::{AssetDetail, AssetSummary, asset_detail, asset_summary};
use crate::store::{SNAPSHOT, StoreError, WsPath};

/// The page size when `limit` is omitted.
pub const DEFAULT_LIMIT: usize = 200;
/// The largest page.
pub const MAX_LIMIT: usize = 1000;
/// The most values a list argument (`status`, `type`, `tags`, `label`, `flag`) takes.
pub const MAX_LIST: usize = 256;

const QUERY_DESCRIPTION: &str = "List the workspace's assets, a page at a time. Every argument \
    is an optional filter, and different filters combine with AND: project (omitted: every \
    project), status and type (a value or a list, any of), text (words matched as prefixes of \
    the asset id, type, status and text fields), tags (any of, case-insensitive), rating_min \
    (0-5), label (any of red, yellow, green, blue, purple, none), flag (pick or reject: assets \
    with a take so flagged in any run not trashed), collection (a saved search's or \
    collection's id from collections.list), duplicates (true: assets sharing a sealed or \
    candidate file's bytes with another asset), deprecated (show, the default, hide or only). \
    facets: true also counts status, type, tags (the 100 largest, with tags_more), rating, label \
    and flag, each over every filter but its own. sort is {field: asset_id|updated|status|type, \
    dir: asc|desc}, default asset_id asc; ties break on project, then asset_id. limit is 1-1000 \
    (default 200). Pass the previous page's next_cursor as cursor, with the same filters, to get \
    the next page; next_cursor is null on the last page. total counts every matching asset.";

const GET_DESCRIPTION: &str = "Get one asset by project and asset_id: its summary, sealed \
    versions (with legacy and content-hash refs), the next version of each bump and the \
    proposed bump, the manifest's candidate runs, human edits, external files, status log and \
    trust records, the state of each file (ok, not_fetched, cloud_only, human_edit, \
    needs_repair, missing), the inputs it uses and the assets that use it.";

const GET_VERSION_DESCRIPTION: &str = "Get one sealed version of an asset: its files (path, \
    SHA-256, media kind, and whether the sign-off chose it as a take's primary), the chosen \
    takes' companions, the recipe and inputs, the sign-off record, restored_from when it was \
    restored from an older version, and the trust records of its files. A legacy version (no \
    snapshot) gives its files and hashes only.";

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FieldArg {
    AssetId,
    Updated,
    Status,
    Type,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DirArg {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SortArg {
    field: FieldArg,
    dir: DirArg,
}

/// A value, or a list of values (any of).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum OneOrList {
    One(String),
    List(#[schemars(length(max = 256))] Vec<String>),
}

impl OneOrList {
    fn len(&self) -> usize {
        match self {
            OneOrList::One(_) => 1,
            OneOrList::List(l) => l.len(),
        }
    }
}

impl From<OneOrList> for Choice {
    fn from(v: OneOrList) -> Choice {
        match v {
            OneOrList::One(s) => Choice::One(s),
            OneOrList::List(l) => Choice::AnyOf(l),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LabelArg {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
    None,
}

impl LabelArg {
    fn label(self) -> Option<Label> {
        match self {
            LabelArg::Red => Some(Label::Red),
            LabelArg::Yellow => Some(Label::Yellow),
            LabelArg::Green => Some(Label::Green),
            LabelArg::Blue => Some(Label::Blue),
            LabelArg::Purple => Some(Label::Purple),
            LabelArg::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FlagArg {
    Pick,
    Reject,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DeprecatedArg {
    Show,
    Hide,
    Only,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    /// A project slug; omitted means every project.
    project: Option<String>,
    /// A status, or a list (any of).
    status: Option<OneOrList>,
    /// A type, or a list (any of).
    #[serde(rename = "type")]
    asset_type: Option<OneOrList>,
    text: Option<String>,
    /// Any of these tags, case-insensitive.
    #[schemars(length(max = 256))]
    tags: Option<Vec<String>>,
    /// 0-5.
    #[schemars(range(max = 5))]
    rating_min: Option<u8>,
    /// Any of these labels; `none` matches an unlabelled asset.
    #[schemars(length(max = 256))]
    label: Option<Vec<LabelArg>>,
    /// Assets with a take so flagged in any run not trashed.
    #[schemars(length(max = 256))]
    flag: Option<Vec<FlagArg>>,
    /// A saved search's or collection's id (`collections.list`).
    collection: Option<String>,
    /// `true`: assets sharing a sealed or candidate file's bytes with another asset.
    duplicates: Option<bool>,
    /// `show` (default), `hide` or `only`.
    deprecated: Option<DeprecatedArg>,
    /// Count the facets too.
    facets: Option<bool>,
    sort: Option<SortArg>,
    /// 1-1000; default 200.
    limit: Option<usize>,
    /// The previous page's `next_cursor`.
    cursor: Option<String>,
}

/// A facet value and its count; `value` is `null` for none.
#[derive(Debug, Serialize, JsonSchema)]
struct FacetEntry {
    value: Value,
    count: u64,
}

/// `assets.query`'s `facets`.
#[derive(Debug, Serialize, JsonSchema)]
struct FacetsResult {
    status: Vec<FacetEntry>,
    #[serde(rename = "type")]
    asset_type: Vec<FacetEntry>,
    /// The 100 largest counts.
    tags: Vec<FacetEntry>,
    rating: Vec<FacetEntry>,
    label: Vec<FacetEntry>,
    flag: Vec<FacetEntry>,
}

fn entries(counts: Vec<FacetCount>) -> Vec<FacetEntry> {
    counts
        .into_iter()
        .map(|c| FacetEntry {
            value: c.value,
            count: c.count,
        })
        .collect()
}

impl From<Facets> for FacetsResult {
    fn from(f: Facets) -> FacetsResult {
        FacetsResult {
            status: entries(f.status),
            asset_type: entries(f.asset_type),
            tags: entries(f.tags),
            rating: entries(f.rating),
            label: entries(f.label),
            flag: entries(f.flag),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct QueryResult {
    assets: Vec<AssetSummary>,
    next_cursor: Option<String>,
    total: u64,
    /// With `facets: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    facets: Option<FacetsResult>,
    /// With `facets: true`: the tags facet lists only the 100 largest counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    tags_more: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    /// A project slug; omitted: the session's active project.
    project: Option<String>,
    asset_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetVersionArgs {
    /// A project slug; omitted: the session's active project.
    project: Option<String>,
    asset_id: String,
    /// The sealed version.
    version: String,
}

/// A file of a sealed version.
#[derive(Debug, Serialize, JsonSchema)]
struct VersionFile {
    /// Asset-relative: `v<version>/<path in the version>`.
    path: String,
    /// Bare hex; `null` when it isn't recorded and the file can't be read.
    sha256: Option<String>,
    media_kind: String,
    /// The sign-off chose the file, and not as a companion.
    primary: bool,
}

/// `asset.get_version`'s result (Appendix H).
#[derive(Debug, Serialize, JsonSchema)]
struct VersionDetail {
    version: String,
    #[serde(rename = "ref")]
    r#ref: String,
    files: Vec<VersionFile>,
    companions: Vec<String>,
    recipe: Value,
    inputs: Value,
    signed_off: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    restored_from: Option<String>,
    trust: Value,
}

fn invalid(ctx: &CallCtx, why: String) -> ErrorBody {
    ErrorBody::new(&PARAMS_INVALID, why, ctx.trace_id.clone())
}

/// The filters of `args` (`project` to `deprecated`, `collection` left out) as a query, checked
/// as `assets.query` checks them: `rating_min` 0-5, at most [`MAX_LIST`] values per list.
fn filters(args: &mut QueryArgs) -> Result<AssetQuery, String> {
    if let Some(r) = args.rating_min
        && r > lil_schema::marks::MAX_RATING
    {
        return Err(format!("rating_min is 0 to 5; {r} is outside that"));
    }
    for (name, len) in [
        ("status", args.status.as_ref().map(OneOrList::len)),
        ("type", args.asset_type.as_ref().map(OneOrList::len)),
        ("tags", args.tags.as_ref().map(Vec::len)),
        ("label", args.label.as_ref().map(Vec::len)),
        ("flag", args.flag.as_ref().map(Vec::len)),
    ] {
        if let Some(len) = len
            && len > MAX_LIST
        {
            return Err(format!(
                "{name} takes at most {MAX_LIST} values; {len} were given"
            ));
        }
    }
    Ok(AssetQuery {
        project: args.project.take(),
        status: args.status.take().map(Choice::from),
        asset_type: args.asset_type.take().map(Choice::from),
        text: args.text.take(),
        tags: args.tags.take(),
        rating_min: args.rating_min,
        label: args
            .label
            .take()
            .map(|l| l.into_iter().map(LabelArg::label).collect()),
        flag: args.flag.take().map(|f| {
            f.into_iter()
                .map(|f| match f {
                    FlagArg::Pick => Flag::Pick,
                    FlagArg::Reject => Flag::Reject,
                })
                .collect()
        }),
        collection: None,
        duplicates: args.duplicates,
        deprecated: args.deprecated.map(|d| match d {
            DeprecatedArg::Show => DeprecatedFilter::Show,
            DeprecatedArg::Hide => DeprecatedFilter::Hide,
            DeprecatedArg::Only => DeprecatedFilter::Only,
        }),
        ..AssetQuery::default()
    })
}

/// A saved search's `rule` validated as an `assets.query` filter set (Appendix H "Organising
/// tools"): `assets.query`'s arguments without `sort`, `limit`, `cursor` and `facets`, and
/// without `collection` (a search is never defined by another entry, so no rule can loop),
/// each checked as `assets.query` checks it. Returns the filters as a query.
///
/// # Errors
/// Why the rule isn't such a filter set (an unknown key, a value of the wrong type, a rating
/// outside 0-5, …).
pub fn rule_query(rule: &serde_json::Map<String, Value>) -> Result<AssetQuery, String> {
    for key in ["sort", "limit", "cursor", "facets"] {
        if rule.contains_key(key) {
            return Err(format!(
                "a rule is a filter set, so {key} doesn't apply (nor do sort, limit, cursor and \
                 facets); pass it to assets.query with the collection instead"
            ));
        }
    }
    if rule.contains_key("collection") {
        return Err("a rule can't name a collection; use its filters instead".to_string());
    }
    let mut args: QueryArgs = serde_json::from_value(Value::Object(rule.clone()))
        .map_err(|e| format!("the rule isn't an assets.query filter set: {e}"))?;
    filters(&mut args)
}

async fn query(ctx: &CallCtx, mut args: QueryArgs) -> Result<(QueryResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(invalid(
            ctx,
            format!("limit is 1 to {MAX_LIMIT}; {limit} is outside that"),
        ));
    }
    let mut q = filters(&mut args).map_err(|why| invalid(ctx, why))?;
    q.sort = args.sort.map_or((SortField::AssetId, SortDir::Asc), |s| {
        let field = match s.field {
            FieldArg::AssetId => SortField::AssetId,
            FieldArg::Updated => SortField::Updated,
            FieldArg::Status => SortField::Status,
            FieldArg::Type => SortField::Type,
        };
        let dir = match s.dir {
            DirArg::Asc => SortDir::Asc,
            DirArg::Desc => SortDir::Desc,
        };
        (field, dir)
    });
    q.limit = limit;
    q.facets = args.facets == Some(true);
    if let Some(id) = &args.collection {
        open.index
            .ready_result()
            .await
            .map_err(|e| e.to_error(&ctx.trace_id))?;
        q.collection = Some(collection_filter(open, id).await.map_err(|mut e| {
            e.trace_id.clone_from(&ctx.trace_id);
            e
        })?);
        // The cursor hashes the id as given (Appendix H), never what it resolved to.
        q.collection_id = Some(id.clone());
    }
    if let Some(cursor) = &args.cursor {
        q.after = Some(decode_cursor(&q, cursor).map_err(|why| invalid(ctx, why))?);
    }
    open.index
        .ready_result()
        .await
        .map_err(|e| e.to_error(&ctx.trace_id))?;
    let page = open
        .index
        .assets(q.clone())
        .await
        .map_err(|e| e.to_error(&ctx.trace_id))?;
    let assets: Vec<AssetSummary> = page
        .rows
        .iter()
        .map(|row| asset_summary(&open.ws.id, row))
        .collect();
    let summary = format!("{} of {} assets", assets.len(), page.total);
    let tags_more = page.facets.as_ref().map(|f| f.tags_more);
    Ok((
        QueryResult {
            next_cursor: page.next.as_ref().map(|k| encode_cursor(&q, k)),
            assets,
            total: page.total,
            facets: page.facets.map(FacetsResult::from),
            tags_more,
        },
        summary,
    ))
}

async fn get(ctx: &CallCtx, args: GetArgs) -> Result<(AssetDetail, String), ErrorBody> {
    let open = ctx.workspace()?;
    let project = args.project.unwrap_or_else(|| ctx.project_or_default());
    open.index
        .ready_result()
        .await
        .map_err(|e| e.to_error(&ctx.trace_id))?;
    let detail = asset_detail(
        &open.store,
        &open.index,
        &open.ws.id,
        &project,
        &args.asset_id,
    )
    .await
    .map_err(|mut e| {
        e.trace_id.clone_from(&ctx.trace_id);
        e
    })?;
    let summary = format!("{}/{}", detail.summary.project, detail.summary.asset_id);
    Ok((detail, summary))
}

async fn get_version(
    ctx: &CallCtx,
    args: GetVersionArgs,
) -> Result<(VersionDetail, String), ErrorBody> {
    let open = ctx.workspace()?;
    let store = &open.store;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let project = args.project.unwrap_or_else(|| ctx.project_or_default());
    let asset_id = args.asset_id;
    let unknown = |what: String| ErrorBody::new(&ASSET_UNKNOWN, what, ctx.trace_id.clone());
    let Ok(version) = args.version.parse::<Version>() else {
        return Err(invalid(
            ctx,
            "version names a sealed version, as major.minor.patch".to_string(),
        ));
    };
    let asset = WsPath::parse(&format!("projects/{project}/assets/{asset_id}")).map_err(error)?;
    let manifest = asset
        .join(&format!("{asset_id}.manifest.json"))
        .map_err(error)?;
    let Some(bytes) = store.read(&manifest).await.map_err(error)? else {
        return Err(unknown(format!(
            "There is no asset `{asset_id}` in the project `{project}`; assets.query lists the \
             assets there are."
        )));
    };
    let live = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|t| Manifest::parse(t).ok())
        .map(|(m, _)| m);
    let Some(found) = store
        .versions(&project, &asset_id)
        .await
        .map_err(error)?
        .into_iter()
        .find(|v| v.version == version)
    else {
        return Err(unknown(format!(
            "`{asset_id}` has no version {version}; asset.get lists its versions."
        )));
    };
    let snapshot = if found.legacy {
        None
    } else {
        let rel = found.dir.join(SNAPSHOT).map_err(error)?;
        store
            .read(&rel)
            .await
            .map_err(error)?
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| Manifest::parse(&t).ok())
            .map(|(m, _)| m)
    };
    // The snapshot's run signed off as this version: its view, and its `signed_off` as written.
    let run = snapshot.as_ref().and_then(|s| {
        s.view()
            .candidate_runs
            .into_iter()
            .find(|r| r.signed_off.as_ref().is_some_and(|o| o.version == version))
    });
    let wire = version.to_string();
    let raw_run = snapshot
        .as_ref()
        .and_then(|s| s.get("candidate_runs"))
        .and_then(Value::as_array)
        .and_then(|runs| {
            runs.iter()
                .find(|r| r["signed_off"]["version"].as_str() == Some(wire.as_str()))
        });
    let signed_off = raw_run.map_or(Value::Null, |r| r["signed_off"].clone());
    // The snapshot's inputs, then its run's, once per `ref` (the first entry kept).
    fn listed(v: Option<&Value>) -> impl Iterator<Item = &Value> {
        v.and_then(Value::as_array).into_iter().flatten()
    }
    let mut refs = std::collections::BTreeSet::new();
    let inputs: Vec<Value> = listed(snapshot.as_ref().and_then(|s| s.get("inputs")))
        .chain(listed(raw_run.and_then(|r| r.get("inputs"))))
        .filter(|i| match i["ref"].as_str() {
            Some(r) => refs.insert(r.to_string()),
            None => true,
        })
        .cloned()
        .collect();
    let companions = run
        .as_ref()
        .and_then(|r| r.signed_off.as_ref())
        .map(|o| o.companions.clone())
        .unwrap_or_default();
    let primary = |inside: &str| {
        run.as_ref().is_some_and(|r| {
            r.selected.iter().any(|s| s.eq_ignore_ascii_case(inside))
                && !companions.iter().any(|c| c.eq_ignore_ascii_case(inside))
        })
    };
    let hashes = live.as_ref().map(|m| m.view().hashes).unwrap_or_default();
    let prefix = format!("v{version}/");
    let mut files = Vec::new();
    for (inside, _) in store.version_files(&found.dir).await.map_err(error)? {
        if inside == SNAPSHOT {
            continue;
        }
        let path = format!("{prefix}{inside}");
        let sha256 = match hashes.get(&path) {
            Some(hex) => Some(hex.clone()),
            None => match found.dir.join(&inside) {
                Ok(rel) => store.hash(&rel).await.ok().flatten(),
                Err(_) => None,
            },
        };
        files.push(VersionFile {
            media_kind: media_kind_of(&inside).as_str().to_string(),
            primary: primary(&inside),
            path,
            sha256,
        });
    }
    let trust: Map<String, Value> = match live.as_ref().and_then(|m| m.get("trust")) {
        Some(Value::Object(t)) => t
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        _ => Map::new(),
    };
    let from_snapshot = |key: &str, empty: Value| {
        snapshot
            .as_ref()
            .and_then(|s| s.get(key))
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(empty)
    };
    let r#ref = AssetRef::Sealed {
        ws: open.ws.id.clone(),
        project: project.clone(),
        asset: asset_id.clone(),
        version,
        sha256: found.content_hash.clone(),
    }
    .to_string();
    let summary = format!("{project}/{asset_id}@{version}");
    Ok((
        VersionDetail {
            version: wire,
            r#ref,
            files,
            companions,
            recipe: from_snapshot("recipe", Value::Null),
            inputs: Value::Array(inputs),
            signed_off,
            restored_from: run
                .as_ref()
                .and_then(|r| r.restored_from)
                .map(|v| v.to_string()),
            trust: Value::Object(trust),
        },
        summary,
    ))
}

/// Registers `assets.query`, `asset.get` and `asset.get_version` (module `workspace`).
pub fn register_asset_read_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "assets.query",
        QUERY_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        |ctx, args: QueryArgs| Box::pin(query(ctx, args)),
    ));
    reg.register(typed_tool(
        "asset.get",
        GET_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        |ctx, args: GetArgs| Box::pin(get(ctx, args)),
    ));
    reg.register(typed_tool(
        "asset.get_version",
        GET_VERSION_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        |ctx, args: GetVersionArgs| Box::pin(get_version(ctx, args)),
    ));
}
