//! The asset read tools (Global Constraints, Product tools; APP-025), module `workspace`. They
//! parse their arguments and call the index and `pipeline::asset_detail`; both wait for the
//! workspace's first reconcile or cold rebuild ([`Index::ready`](crate::index::Index::ready)),
//! with no timeout of their own, so a fresh open never answers with an empty or partial page.
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `assets.query` | read | `{project?, status?, type?, text?, sort?, limit?, cursor?}` | `{assets: [AssetSummary], next_cursor: string\|null, total}` |
//! | `asset.get` | read | `{project?, asset_id}` | `AssetDetail` |
//!
//! `assets.query`'s `project` is a filter: omitted, it means every project (the session's
//! active project never narrows it). `asset.get`'s omitted `project` is the call's resolved
//! project (`CallCtx::project`: the session's active project, else the workspace's hint, else
//! `sketchbook`).

use lil_protocol::codes::PARAMS_INVALID;
use lil_protocol::{EffectClass, ErrorBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::index::{AssetQuery, SortDir, SortField, decode_cursor, encode_cursor};
use crate::pipeline::{AssetDetail, AssetSummary, asset_detail, asset_summary};

/// The page size when `limit` is omitted.
pub const DEFAULT_LIMIT: usize = 200;
/// The largest page.
pub const MAX_LIMIT: usize = 1000;

const QUERY_DESCRIPTION: &str = "List the workspace's assets, a page at a time. Every argument \
    is an optional filter: project (omitted: every project), status, type, and text (words \
    matched as prefixes of the asset id, type, status and text fields). sort is {field: \
    asset_id|updated|status|type, dir: asc|desc}, default asset_id asc; ties break on project, \
    then asset_id. limit is 1-1000 (default 200). Pass the previous page's next_cursor as \
    cursor to get the next page; next_cursor is null on the last page. total counts every \
    matching asset.";

const GET_DESCRIPTION: &str = "Get one asset by project and asset_id: its summary, sealed \
    versions (with legacy and content-hash refs), the next version of each bump and the \
    proposed bump, the manifest's candidate runs, human edits and external files, and the \
    state of each file (ok, not_fetched, cloud_only, human_edit, needs_repair, missing).";

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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    /// A project slug; omitted means every project.
    project: Option<String>,
    status: Option<String>,
    #[serde(rename = "type")]
    asset_type: Option<String>,
    text: Option<String>,
    sort: Option<SortArg>,
    /// 1-1000; default 200.
    limit: Option<usize>,
    /// The previous page's `next_cursor`.
    cursor: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct QueryResult {
    assets: Vec<AssetSummary>,
    next_cursor: Option<String>,
    total: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    /// A project slug; omitted: the session's active project.
    project: Option<String>,
    asset_id: String,
}

fn invalid(ctx: &CallCtx, why: String) -> ErrorBody {
    ErrorBody::new(&PARAMS_INVALID, why, ctx.trace_id.clone())
}

async fn query(ctx: &CallCtx, args: QueryArgs) -> Result<(QueryResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(invalid(
            ctx,
            format!("limit is 1 to {MAX_LIMIT}; {limit} is outside that"),
        ));
    }
    let sort = args.sort.map_or((SortField::AssetId, SortDir::Asc), |s| {
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
    let mut q = AssetQuery {
        project: args.project,
        status: args.status,
        asset_type: args.asset_type,
        text: args.text,
        sort,
        limit,
        after: None,
    };
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
    Ok((
        QueryResult {
            next_cursor: page.next.as_ref().map(|k| encode_cursor(&q, k)),
            assets,
            total: page.total,
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

/// Registers `assets.query` and `asset.get` (module `workspace`).
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
}
