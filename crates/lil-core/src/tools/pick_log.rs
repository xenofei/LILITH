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
//!   recorded SHA-256 (a pre-2b run); `recipe` is the run's `recipe` as the manifest holds it
//!   (`null` when it has none).
//! - **The append** goes through `fsops::append_locked`, one whole line under the append lock,
//!   tried again while another writer holds the lock. It happens after the decision's Record
//!   step (the mark's manifest edit, the sign-off's Record stage) and never undoes it: a failed
//!   append is logged, and reported to the tools error sink as `pick_log.append_failed` (the
//!   Diagnostics' recent errors; not a protocol error code), and the call's result is unchanged.
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
use std::time::{Duration, Instant};

use lil_protocol::CallerKind;
use lil_schema::{CandidateRun, Manifest, file_takes, renderable, take_sha256, takes_of};
use serde::Serialize;
use serde_json::Value;

use super::registry::CallCtx;
use super::services::Thumbnails;
use crate::pipeline::Chosen;
use crate::store::{WsPath, fsops};

/// The log's file name in the workspace's local data folder.
pub const PICKS_FILE: &str = "picks.jsonl";
/// The Diagnostics entry of a failed append (Appendix H "Pick log").
pub const APPEND_FAILED: &str = "pick_log.append_failed";
/// The thumbnail tier the copy keeps, at scale 1 (P4b-R13).
pub const PICK_THUMB_PX: u32 = 96;
/// How long an append waits for the append lock before it counts as failed.
const APPEND_WAIT: Duration = Duration::from_secs(10);
/// How long the copy waits for its thumbnail.
const COPY_WAIT: Duration = Duration::from_secs(60);

/// A logged decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Pick,
    Reject,
    Cleared,
    SignedOff,
}

/// One line of `picks.jsonl` (Appendix H "Pick log"; module docs).
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
    /// The run's `recipe` as the manifest holds it.
    pub recipe: Value,
}

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

/// The run `run_id` of `m` and its raw `recipe`.
fn run_of(m: &Manifest, run_id: &str) -> Option<(CandidateRun, Value)> {
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
        .cloned()
        .unwrap_or(Value::Null);
    Some((run, recipe))
}

/// The line of take `n` of `run` (module docs): its entries found by index; with `chosen` (a
/// sign-off by files) the logged entry is the chosen one of the take, its primary when chosen.
/// `None` for a take the run doesn't have.
fn take_line(
    d: &Decided,
    run: &CandidateRun,
    recipe: &Value,
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
        recipe: recipe.clone(),
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

/// Appends `line` to the pick log of `ctx`'s workspace, and copies the 96 px thumbnail of the
/// file at `thumb` (its workspace path and SHA-256) through `thumbs` in the background (module
/// docs). Only for a `ui:*` caller; never returns an error: a failed append is reported to the
/// tools error sink as `pick_log.append_failed`.
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
    let mut text = match serde_json::to_string(&line) {
        Ok(text) => text,
        Err(e) => {
            report(ctx, &format!("the line can't be written as JSON: {e}"));
            return;
        }
    };
    text.push('\n');
    let log = local.join(PICKS_FILE);
    let appended = tokio::task::spawn_blocking(move || append(&log, &text))
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(format!("the append stopped: {e}"))));
    if let Err(e) = appended {
        report(
            ctx,
            &format!("A Review decision couldn't be added to {PICKS_FILE}: {e}"),
        );
        return;
    }
    let (Some(thumbs), Some((path, sha))) = (thumbs, thumb) else {
        return;
    };
    if !renderable(path.as_str()) || !is_sha256(sha) {
        return;
    }
    let (path, sha) = (path.clone(), sha.to_string());
    tokio::spawn(async move { copy_thumb(thumbs.as_ref(), &local, &path, &sha).await });
}

/// Appends `text` (one line) to `log` under the append lock, waiting up to [`APPEND_WAIT`]
/// while another writer holds it.
fn append(log: &Path, text: &str) -> std::io::Result<()> {
    let until = Instant::now() + APPEND_WAIT;
    loop {
        if fsops::append_locked(log, &|_| text.to_string())?.is_some() {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "another writer held the log's append lock",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Tells the tools error sink (Diagnostics) of a failed append.
fn report(ctx: &CallCtx, message: &str) {
    tracing::warn!(tool = %ctx.tool, trace_id = %ctx.trace_id, why = %message, "the pick log couldn't take a line");
    if let Some(dispatcher) = &ctx.redispatch {
        dispatcher.report_error(APPEND_FAILED, &ctx.tool, message);
    }
}

/// Whether `s` is a SHA-256 in bare lowercase hex, as a cache name takes it.
fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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
