//! `ref.resolve` (Appendix H "`ref.resolve`"; ARCH-125, APP-019, SP-08), module `workspace`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `ref.resolve` | read | `{ref} \| {path}` | `Resolved {kind, ref?, project?, asset_id?, version?, file?, read_only, redirected_from, sha256_ok?}` |
//!
//! - `{ref}`: a `lil://` ref of the session's workspace (a sealed version, with or without
//!   `#sha256=`, or a candidate file) resolved through `projects::Resolver`: redirects
//!   followed, a trashed or missing end and a loop are errors as Resolution says, and a sealed
//!   ref's hash is checked at the end. The ref a `.lilref` file carries is resolved this way.
//!   Any other text, or a ref of another workspace, is `ref.invalid`.
//! - `{path}`: an absolute or workspace-relative file path (APP-019), mapped by
//!   `Resolver::resolve_path` without reading the redirects (a path names a place): a sealed
//!   file is `read_only`; a path whose real location is outside the workspace (through `..`,
//!   a junction or a symbolic link) is `outside`.
//!
//! The arguments are checked first; a redirects file that can't be read or parsed fails only
//! a `{ref}` whose name isn't live (with its `manifest.needs_repair` or read error).
//!
//! Exactly one of the two (`params.invalid` otherwise).

use lil_protocol::codes::{PARAMS_INVALID, REF_INVALID};
use lil_protocol::{EffectClass, ErrorBody};
use lil_schema::AssetRef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::projects::{Resolved, ResolvedKind, Resolver};

const RESOLVE_DESCRIPTION: &str = "Resolve a lil:// ref or a file path to the asset it names. \
    ref: a sealed ref (lil://<ws>/<project>/<asset>@<version>, with or without #sha256=) or a \
    candidate ref, as asset.get or a .lilref file gives it; old names are followed through \
    registry/redirects.yaml to where the asset is now (redirected_from lists them), and a \
    sealed ref's hash is checked against the version (ref.hash_mismatch). path: an absolute or \
    workspace-relative path; kind says what it is: sealed (read_only), candidate, asset, inbox \
    or outside. Errors: asset.trashed (with entry_id), ref.redirect_broken (a loop or a dead \
    end: chain, reason, to), asset.unknown. Give exactly one of ref and path.";

/// The longest path accepted, in bytes of UTF-8.
const MAX_PATH: usize = 4096;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolveArgs {
    /// A `lil://` ref: a sealed version (optionally with `#sha256=<hex>`) or a candidate file.
    #[serde(rename = "ref")]
    r#ref: Option<String>,
    /// An absolute or workspace-relative file path.
    path: Option<String>,
}

fn traced(ctx: &CallCtx, mut e: ErrorBody) -> ErrorBody {
    e.trace_id.clone_from(&ctx.trace_id);
    e
}

fn summary(r: &Resolved) -> String {
    let kind = match r.kind {
        ResolvedKind::Sealed => "sealed",
        ResolvedKind::Candidate => "candidate",
        ResolvedKind::Asset => "asset",
        ResolvedKind::Inbox => "inbox",
        ResolvedKind::Outside => "outside",
    };
    let mut out = match (&r.project, &r.asset_id) {
        (Some(p), Some(a)) => format!("{kind} {p}/{a}"),
        _ => kind.to_string(),
    };
    if let Some(v) = &r.version {
        out.push_str(&format!("@{v}"));
    }
    if let Some(first) = r.redirected_from.first() {
        out.push_str(&format!(" (redirected from {first})"));
    }
    out
}

async fn resolve(ctx: &CallCtx, args: ResolveArgs) -> Result<(Resolved, String), ErrorBody> {
    let open = ctx.workspace()?;
    let invalid = |why: &str| ErrorBody::new(&PARAMS_INVALID, why, ctx.trace_id.clone());
    // The arguments are checked before anything is read.
    let resolved = match (args.r#ref, args.path) {
        (Some(text), None) => {
            let refused = |why: String| {
                let mut e = ErrorBody::new(&REF_INVALID, why, ctx.trace_id.clone());
                e.data = Some(json!({ "ref": text }));
                e
            };
            let parsed: AssetRef = text.parse().map_err(refused)?;
            let (AssetRef::Sealed { ws, .. } | AssetRef::Candidate { ws, .. }) = &parsed;
            if *ws != open.ws.id {
                return Err(refused(format!(
                    "`{text}` is a ref of the workspace {ws}; this session's workspace is {}",
                    open.ws.id
                )));
            }
            let resolver = Resolver::load(&open.index, &open.store)
                .await
                .map_err(|e| traced(ctx, e))?;
            resolver.resolve_ref(&parsed).await
        }
        (None, Some(path)) => {
            if path.is_empty() || path.len() > MAX_PATH || path.chars().any(char::is_control) {
                return Err(invalid(
                    "path is 1 to 4096 bytes of UTF-8 with no control character",
                ));
            }
            // A path names a place: the redirects aren't read.
            Resolver::without_redirects(&open.index, &open.store)
                .resolve_path(&open.ws.id, &path)
                .await
        }
        _ => return Err(invalid("give exactly one of ref and path")),
    }
    .map_err(|e| traced(ctx, e))?;
    let summary = summary(&resolved);
    Ok((resolved, summary))
}

/// Registers `ref.resolve` (module `workspace`).
pub fn register_ref_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "ref.resolve",
        RESOLVE_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        |ctx, args: ResolveArgs| Box::pin(resolve(ctx, args)),
    ));
}
