//! The pick log (CORE-176; Appendix H "Pick log"; P4b-R13): every pick, reject and clear you
//! make in Review, and every sign-off from Review, is one JSON line in `picks.jsonl` of the
//! workspace's local data folder (ARCH-097, `OpenWorkspace::local_dir`, the folder the
//! thumbnail cache lives in), for a future taste ranker. Nothing reads it in M1.
//!
//! - **Who is logged.** Only a `ui:*` caller whose call carries `review_view` (`grid`, `loupe`,
//!   `compare` or `survey`): the callers build a [`PickLine`] only when the call names its view,
//!   and [`log_pick`] writes nothing for any other caller. A mark or sign-off from the Inspector
//!   (no `review_view`), an agent or the cli isn't logged.
//! - **The line** ([`PickLine`]): `{utc, project, asset_id, run_id, file, sha256, take, primary,
//!   take_sha256, decision, view, recipe}`. A take's file entries are found by index through
//!   `lil_schema::file_takes`, never by bare name. `file` is the logged entry's asset-relative
//!   `path` as the run records it: the take's primary file (a mark addresses the take, whose
//!   primary holds its marks), or for a sign-off by `files` the chosen file of that take
//!   (its primary when chosen). `primary` says whether it is the take's primary. The take hash
//!   is `lil_schema::take_sha256` over the take's files, `null` when one of them has no
//!   recorded SHA-256 (a pre-2b run).
//! - **The recipe ref.** `recipe` is the run's recipe ref, `sha256:<hex>`: the SHA-256 of
//!   `serde_json::to_vec` of the run's `recipe` value as the manifest holds it (`null` for a run
//!   without one). The recipe itself is kept once, those same bytes, as
//!   `<local dir>\picks\recipes\<hex>.json` (`fsops::write_workspace_local_file`, atomic; a file
//!   already there is kept), so the log stays usable after the run is purged (CORE-032). Its
//!   write starts in its own task before the line is appended and is never waited for; a
//!   failed write is a `tracing` warning, as a failed thumbnail copy is, and the line is
//!   written all the same.
//! - **A decision is a change.** A mark is logged only when it changes the
//!   take's `flag` (`asset.mark`'s `previous` holds `flag`): none to pick or reject, pick to
//!   reject and back, or pick or reject to cleared. A repeated pick, or a clear with nothing to
//!   clear, writes nothing. A sign-off from Review always logs one line per signed take.
//! - **The append** goes through `store::append_local_line` (`fsops::append_line_locked`: one
//!   whole line under the append lock, reading only the file's last byte, into a plain file
//!   with one name, never through a link), polled under the store's backoff (10 ms doubling)
//!   while another writer holds the lock or a sharing violation stops it, [`APPEND_BUDGET`]
//!   (1 s) in all. It happens after the decision's Record step (the mark's manifest edit, the
//!   sign-off's Record stage) and never undoes it: a failed append is logged, and reported to
//!   the tools error sink as `pick_log.append_failed` (the Diagnostics' recent errors; not a
//!   protocol error code), and the call's result is unchanged.
//! - **When a line is written.** A mark's line is appended before the mark answers, while
//!   `asset.mark` still holds the asset's version lock, so marks of one asset are logged in the
//!   order the manifest took them. Under contention a mark waits at most about 1 s
//!   ([`APPEND_BUDGET`]) for the log, then records `pick_log.append_failed`, and the decision
//!   stands. A sign-off's lines are appended in their own task once its Record stage succeeded
//!   (its run is closed by then).
//! - **The thumbnail copy**: the 96 px tier at scale 1 of a file whose kind has a renderer
//!   (`lil_schema::renderable`), through `Thumbnails::ensure`, is copied to
//!   `<local dir>\picks\thumbs\<sha256>.png` (`fsops::write_workspace_local_file`, which keeps a
//!   copy already there). It runs in its own task after the line, so it never delays or fails
//!   the call; without the `thumbs` service, or when `ensure` doesn't give a ready thumbnail of
//!   that hash in time, the line stands without the copy. The service renders from the default
//!   workspace; the hash it answers must equal the line's, so a copy is always of those bytes.
//!   The copies are never cleaned (the log stays usable after candidates are purged, CORE-032).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lil_protocol::CallerKind;
use lil_schema::content_hash::sha256_hex;
use lil_schema::{
    CandidateRun, Manifest, bare_sha256, file_takes, hash_value, renderable, take_sha256, takes_of,
};
use serde::Serialize;
use serde_json::Value;

use super::registry::CallCtx;
use super::services::Thumbnails;
use crate::pipeline::Chosen;
use crate::store::{WsPath, append_local_line, fsops};

/// The log's file name in the workspace's local data folder.
pub const PICKS_FILE: &str = "picks.jsonl";
/// The Diagnostics entry of a failed append (Appendix H "Pick log").
pub const APPEND_FAILED: &str = "pick_log.append_failed";
/// The thumbnail tier the copy keeps, at scale 1 (P4b-R13).
pub const PICK_THUMB_PX: u32 = 96;
/// How long the copy waits for its thumbnail.
const COPY_WAIT: Duration = Duration::from_secs(60);
/// How long an append polls for the log's append lock before it counts as failed: a mark waits
/// for its line (module docs "When a line is written"). The store's other logs keep 10 s.
pub const APPEND_BUDGET: Duration = Duration::from_secs(1);

/// A logged decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Pick,
    Reject,
    Cleared,
    SignedOff,
}

/// One line of `picks.jsonl` (Appendix H "Pick log"; module docs). A mark's line is written
/// before the mark answers, in the order the manifest took the marks; when the log stays
/// locked past [`APPEND_BUDGET`] (about 1 s) the mark records `pick_log.append_failed` instead,
/// and the decision stands.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PickLine {
    /// RFC 3339 UTC, on the store's clock.
    pub utc: String,
    pub project: String,
    pub asset_id: String,
    pub run_id: String,
    /// The file entry's asset-relative path, as the run records it.
    pub file: String,
    /// Bare hex; `None` when the run recorded none.
    pub sha256: Option<String>,
    /// 1-based.
    pub take: u32,
    /// Whether `file` is its take's primary file.
    pub primary: bool,
    /// `lil_schema::take_sha256` of the take's files; `None` when one has no `sha256`.
    pub take_sha256: Option<String>,
    pub decision: Decision,
    /// `grid`, `loupe`, `compare` or `survey`.
    pub view: String,
    /// The run's recipe ref, `sha256:<hex>` of `serde_json::to_vec` of its `recipe`; `None`
    /// for a run without one.
    pub recipe: Option<String>,
    /// The recipe `recipe` names: [`log_pick`] keeps it once as
    /// `<local dir>\picks\recipes\<hex>.json`. Never part of the line.
    #[serde(skip)]
    pub recipe_body: Option<Value>,
}

/// A run's recipe: its ref and the value it names.
type Recipe = Option<(String, Value)>;

/// What every line of one decision shares.
#[derive(Debug, Clone)]
pub(crate) struct Decided {
    pub utc: String,
    pub project: String,
    pub asset_id: String,
    pub decision: Decision,
    pub view: &'static str,
}

/// Whether `ctx`'s caller is one whose Review decisions are logged: an app (`ui:*`).
pub fn logs_caller(ctx: &CallCtx) -> bool {
    matches!(ctx.session.caller, CallerKind::Ui(_))
}

/// The bare name of a run file's `path`.
fn bare(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The run `run_id` of `m` and its recipe (module docs "The recipe ref").
fn run_of(m: &Manifest, run_id: &str) -> Option<(CandidateRun, Recipe)> {
    let run = m
        .view()
        .candidate_runs
        .into_iter()
        .find(|r| r.run_id.eq_ignore_ascii_case(run_id))?;
    let recipe = m
        .get("candidate_runs")
        .and_then(Value::as_array)
        .and_then(|runs| {
            runs.iter().find(|r| {
                r.get("run_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.eq_ignore_ascii_case(run_id))
            })
        })
        .and_then(|r| r.get("recipe"))
        .filter(|r| !r.is_null())
        .and_then(|r| {
            let bytes = serde_json::to_vec(r).ok()?;
            Some((hash_value(&sha256_hex(&bytes)), r.clone()))
        });
    Some((run, recipe))
}

/// The line of take `n` of `run` (module docs): its entries found by index; with `chosen` (a
/// sign-off by files) the logged entry is the chosen one of the take, its primary when chosen.
/// `None` for a take the run doesn't have.
fn take_line(
    d: &Decided,
    run: &CandidateRun,
    recipe: &Recipe,
    n: u32,
    chosen: Option<&[String]>,
) -> Option<PickLine> {
    let numbers = file_takes(run);
    let members: Vec<usize> = (0..run.files.len())
        .filter(|i| numbers.get(*i).copied().flatten() == Some(n))
        .collect();
    let take = takes_of(run).into_iter().find(|t| t.n == n)?;
    let first = *members.first()?;
    let primary = members
        .iter()
        .find(|i| run.files[**i].primary)
        .or_else(|| {
            members
                .iter()
                .find(|i| bare(&run.files[**i].path) == take.primary)
        })
        .copied()
        .unwrap_or(first);
    let is_chosen = |i: usize| {
        chosen.is_none_or(|names| {
            names
                .iter()
                .any(|c| c.eq_ignore_ascii_case(bare(&run.files[i].path)))
        })
    };
    let logged = if is_chosen(primary) {
        primary
    } else {
        members
            .iter()
            .copied()
            .find(|i| is_chosen(*i))
            .unwrap_or(primary)
    };
    let take_sha = members
        .iter()
        .map(|i| {
            let f = &run.files[*i];
            f.sha256.clone().map(|sha| (bare(&f.path).to_string(), sha))
        })
        .collect::<Option<Vec<_>>>()
        .map(|files| take_sha256(&files));
    let entry = &run.files[logged];
    Some(PickLine {
        utc: d.utc.clone(),
        project: d.project.clone(),
        asset_id: d.asset_id.clone(),
        run_id: run.run_id.clone(),
        file: entry.path.clone(),
        sha256: entry.sha256.clone(),
        take: n,
        primary: logged == primary,
        take_sha256: take_sha,
        decision: d.decision,
        view: d.view.to_string(),
        recipe: recipe.as_ref().map(|(r, _)| r.clone()),
        recipe_body: recipe.as_ref().map(|(_, body)| body.clone()),
    })
}

/// The line of a mark of take `n` of run `run_id` in `m` (the manifest after the mark).
pub(crate) fn mark_line(d: &Decided, m: &Manifest, run_id: &str, n: u32) -> Option<PickLine> {
    let (run, recipe) = run_of(m, run_id)?;
    take_line(d, &run, &recipe, n, None)
}

/// The lines of a sign-off of run `run_id` in `m` (the manifest the sign-off records), one per
/// signed take, in take order: the takes `chosen` names, or the takes of the files it names
/// (each name the first entry of that bare name, as the sign-off itself finds it; a file in no
/// take is in no line).
pub(crate) fn signoff_lines(
    d: &Decided,
    m: &Manifest,
    run_id: &str,
    chosen: &Chosen,
) -> Vec<PickLine> {
    let Some((run, recipe)) = run_of(m, run_id) else {
        return Vec::new();
    };
    let (mut takes, names) = match chosen {
        Chosen::Takes(takes) => (takes.clone(), None),
        Chosen::Files(names) => {
            let numbers = file_takes(&run);
            let takes = names
                .iter()
                .filter_map(|name| {
                    let i = run
                        .files
                        .iter()
                        .position(|f| bare(&f.path).eq_ignore_ascii_case(name))?;
                    numbers.get(i).copied().flatten()
                })
                .collect();
            (takes, Some(names.as_slice()))
        }
    };
    takes.sort_unstable();
    takes.dedup();
    takes
        .into_iter()
        .filter_map(|n| take_line(d, &run, &recipe, n, names))
        .collect()
}

/// Appends `line` to the pick log of `ctx`'s workspace, and keeps `line`'s recipe and copies
/// the 96 px thumbnail of the file at `thumb` (its workspace path and SHA-256) through `thumbs`
/// in tasks of their own, never waited for (module docs). Only the append is awaited, for at
/// most [`APPEND_BUDGET`]. Only for a `ui:*` caller; never returns an error: a failed append is
/// reported to the tools error sink as `pick_log.append_failed`.
pub async fn log_pick(
    ctx: &CallCtx,
    thumbs: Option<Arc<dyn Thumbnails>>,
    line: PickLine,
    thumb: Option<(&WsPath, &str)>,
) {
    if !logs_caller(ctx) {
        return;
    }
    let Some(open) = ctx.workspace.as_ref() else {
        return;
    };
    let Some(local) = open.local_dir.clone() else {
        report(ctx, "the workspace has no local data folder");
        return;
    };
    if let (Some(r), Some(body)) = (line.recipe.as_deref(), line.recipe_body.as_ref())
        && let Some(hex) = bare_sha256(r)
    {
        let (local, hex, body) = (local.clone(), hex.to_string(), body.clone());
        tokio::spawn(async move { keep_recipe(&local, &hex, &body).await });
    }
    let mut text = match serde_json::to_string(&line) {
        Ok(text) => text,
        Err(e) => {
            report(ctx, &format!("the line can't be written as JSON: {e}"));
            return;
        }
    };
    text.push('\n');
    if let Err(e) = append_local_line(local.join(PICKS_FILE), text, APPEND_BUDGET).await {
        report(
            ctx,
            &format!("A Review decision couldn't be added to {PICKS_FILE}: {e}"),
        );
        return;
    }
    let (Some(thumbs), Some((path, sha))) = (thumbs, thumb) else {
        return;
    };
    if !renderable(path.as_str()) || bare_sha256(sha) != Some(sha) {
        return;
    }
    let (path, sha) = (path.clone(), sha.to_string());
    tokio::spawn(async move { copy_thumb(thumbs.as_ref(), &local, &path, &sha).await });
}

/// Tells the tools error sink (Diagnostics) of a failed append.
fn report(ctx: &CallCtx, message: &str) {
    tracing::warn!(tool = %ctx.tool, trace_id = %ctx.trace_id, why = %message, "the pick log couldn't take a line");
    if let Some(dispatcher) = &ctx.redispatch {
        dispatcher.report_error(APPEND_FAILED, &ctx.tool, message);
    }
}

/// Keeps the recipe `body` (ref hash `hex`) as `<local dir>\picks\recipes\<hex>.json`, the
/// bytes its ref hashes, unless it is there; a failure is a warning only.
async fn keep_recipe(local: &Path, hex: &str, body: &Value) {
    let target = local
        .join("picks")
        .join("recipes")
        .join(format!("{hex}.json"));
    if target.exists() {
        return;
    }
    let (local, bytes) = (local.to_path_buf(), serde_json::to_vec(body));
    let kept = tokio::task::spawn_blocking(move || {
        let bytes = bytes.map_err(std::io::Error::other)?;
        fsops::write_workspace_local_file(&local, &target, &bytes)
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(format!("the write stopped: {e}"))));
    if let Err(e) = kept {
        tracing::warn!(recipe = %hex, error = %e, "the pick log's recipe couldn't be kept");
    }
}

/// The copy's place: `<local dir>\picks\thumbs\<sha256>.png`.
fn copy_path(local: &Path, sha: &str) -> PathBuf {
    local
        .join("picks")
        .join("thumbs")
        .join(format!("{sha}.png"))
}

/// Copies the 96 px thumbnail of `path` (bytes of hash `sha`) to [`copy_path`], unless one is
/// there; anything short of a ready thumbnail of that hash leaves it out.
async fn copy_thumb(thumbs: &dyn Thumbnails, local: &Path, path: &WsPath, sha: &str) {
    let target = copy_path(local, sha);
    if target.exists() {
        return;
    }
    let Ok(state) =
        tokio::time::timeout(COPY_WAIT, thumbs.ensure(path, Some(sha), PICK_THUMB_PX)).await
    else {
        tracing::debug!(path = %path, "the pick log's thumbnail wasn't ready in time");
        return;
    };
    let Some(file) = state.file.filter(|_| {
        state.state == "ready" && state.px == PICK_THUMB_PX && state.sha256.as_deref() == Some(sha)
    }) else {
        tracing::debug!(path = %path, state = %state.state, "no thumbnail to keep for the pick log");
        return;
    };
    let local = local.to_path_buf();
    let copied = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&file)?;
        fsops::write_workspace_local_file(&local, &target, &bytes)
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(format!("the copy stopped: {e}"))));
    if let Err(e) = copied {
        tracing::warn!(path = %path, error = %e, "the pick log's thumbnail couldn't be kept");
    }
}
