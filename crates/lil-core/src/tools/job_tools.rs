//! The job tools (CORE-146, CORE-160, AI-113; Appendix F "Product tools", Appendix G "Jobs"),
//! module `jobs`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `jobs.list` | read | `{status?, kind?, project?, ids?, since?, batch_id?, limit? (1–500, default 100), cursor?}` | `{jobs, next_cursor, lanes}`, newest first |
//! | `job.wait` | read | `{job_id, timeout_s? (1–90, default 90)}` | `{job, done}` |
//! | `job.cancel` | write | exactly one of `{job_id}`, `{ids}` (1–500), `{batch_id}`, `{all_mine: true}` | `{job}` for `job_id`, else `{jobs}`; a finished job as it is |
//! | `job.retry` | write | exactly one of `{job_id}`, `{batch_id}` | `{job, retry_of}`, or `{retried: [{job, retry_of}], refused: [{job_id, error}]}` |
//! | `job.move` | write | `{job_id, to: "next" \| "last"}` | `{job}` |
//! | `job.estimate` | read | `{tool, args?}` (default `{}`) | the `Estimate` of that call (CORE-161) |
//!
//! `job.estimate` checks `args` as a call of `tool` would (`tool.unknown` for a tool the
//! caller can't call, `params.invalid` for arguments it refuses) and estimates the job that
//! call would submit from its history (Appendix G "Estimates"); a tool with no estimate key
//! has both parts unknown, `no_key`. It runs nothing.
//!
//! Each works in the call's scope: the session's workspace, or the suite without one. They live
//! in `tools`, not in `jobs`, so that `jobs` depends on no `tools` type (CORE-010): they build
//! the jobs module's [`JobCaller`](crate::jobs::JobCaller) from their call's context.
//!
//! `job.retry` re-dispatches the tool call that made the job, with the retry's caller and
//! idempotency key: the confirm, estimate and reserve stages run for it as for a fresh call
//! ([`Redispatch`](super::Redispatch)), so a retry is never a way around a confirmation or a cap.
//! `job.retry {batch_id}` does it for each job of the batch, without a key of its own per job,
//! so a job whose retry would need your confirmation is refused there (retry it alone).
//!
//! `job.wait`'s summary text for a job whose every node the engine served from its cache is
//! "Nothing changed: ComfyUI served every node from its cache." (CORE-117).

use std::time::Duration;

use lil_protocol::app::QueueState;
use lil_protocol::estimate::Estimate;
use lil_protocol::{EffectClass, ErrorBody, codes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::registry::{Registry, typed_tool};
use crate::jobs::{
    CancelSelector, JOBS_MODULE, JobFilter, JobId, JobSummary, Lane, MAX_WAIT, MoveTo,
    NOTHING_CHANGED, RetryBatch,
};

/// The retry tool's name: the dispatcher re-dispatches the call it retries.
pub const JOB_RETRY: &str = "job.retry";

#[derive(Debug, Serialize, JsonSchema)]
struct ListResult {
    jobs: Vec<JobSummary>,
    /// Pass it as `cursor` for the next page; `null` on the last.
    next_cursor: Option<String>,
    /// Each lane's pause (Appendix G `QueueState`).
    lanes: Vec<QueueState>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    job_id: String,
    /// 1–90 seconds; default 90.
    #[serde(default)]
    #[schemars(range(min = 1, max = 90))]
    timeout_s: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct WaitResult {
    job: JobSummary,
    /// The job is `succeeded`, `failed` or `cancelled`, or `interrupted_unknown` (it waits for
    /// you to resolve it).
    done: bool,
}

/// `job.cancel`'s selectors: exactly one.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    #[serde(default)]
    job_id: Option<String>,
    /// 1–500 job ids.
    #[serde(default)]
    ids: Option<Vec<String>>,
    #[serde(default)]
    batch_id: Option<String>,
    /// `true`: every unfinished job your caller kind submitted (for an app, that app's).
    #[serde(default)]
    all_mine: Option<bool>,
}

impl CancelArgs {
    /// The one selector, or why there isn't exactly one.
    fn selector(self) -> Result<CancelSelector, String> {
        let mut found = Vec::new();
        if let Some(id) = self.job_id {
            found.push(CancelSelector::One(JobId(id)));
        }
        if let Some(ids) = self.ids {
            found.push(CancelSelector::Ids(ids.into_iter().map(JobId).collect()));
        }
        if let Some(batch) = self.batch_id {
            found.push(CancelSelector::Batch(batch));
        }
        match self.all_mine {
            Some(true) => found.push(CancelSelector::AllMine),
            Some(false) => return Err("all_mine is true when it is given".to_string()),
            None => {}
        }
        if found.len() != 1 {
            return Err(
                "job.cancel takes exactly one of job_id, ids, batch_id or all_mine".to_string(),
            );
        }
        Ok(found.remove(0))
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
enum CancelResult {
    One { job: Box<JobSummary> },
    Many { jobs: Vec<JobSummary> },
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RetryArgs {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    batch_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
enum RetryResult {
    One {
        job: Box<JobSummary>,
        retry_of: String,
    },
    Batch(RetryBatch),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    job_id: String,
    to: MoveTo,
}

#[derive(Debug, Serialize, JsonSchema)]
struct JobResultOut {
    job: JobSummary,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EstimateArgs {
    /// The tool whose call to estimate.
    tool: String,
    /// The call's arguments; default `{}`.
    #[serde(default)]
    args: serde_json::Map<String, serde_json::Value>,
}

/// `job.wait`'s summary text. A job a reason holds says so, and for `restart` or `manual`
/// that it waits until you resume it in Jobs (Appendix G "Restart hold"), so an agent knows
/// that polling it won't help.
fn wait_summary(job: &JobSummary, done: bool) -> String {
    if done && job.all_cached {
        return NOTHING_CHANGED.to_string();
    }
    let mut text = format!("job {} is {}", job.job_id, job.status.wire());
    if !job.pause_reasons.is_empty() {
        text.push_str(&format!(", held ({})", job.pause_reasons.join(", ")));
        if job
            .pause_reasons
            .iter()
            .any(|r| r == "restart" || r == "manual")
        {
            text.push_str(": it waits until you resume it in Jobs");
        }
    }
    text
}

fn params_invalid(message: String, trace_id: &str) -> ErrorBody {
    ErrorBody::new(&codes::PARAMS_INVALID, message, trace_id)
}

/// Registers `jobs.list`, `job.wait`, `job.cancel`, `job.retry`, `job.move` and
/// `job.estimate` on `reg`.
pub fn register_job_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "jobs.list",
        "Lists jobs, newest first, or with `order: \"queue\"` the queued jobs in queue order (by lane, then `queue_position`): `status`, `kind`, `project`, `ids`, `since` (RFC 3339: updated later) and `batch_id` filter them; `limit` (1–500, default 100; a page may end sooner, under 3 MiB) and `cursor` (the previous page's `next_cursor`) page through them. `lanes` gives each lane's pause.",
        EffectClass::Read,
        JOBS_MODULE,
        |ctx, f: JobFilter| {
            Box::pin(async move {
                let (jobs, next_cursor) = ctx.jobs.list(&ctx.job_caller(), f).await?;
                let lanes = Lane::ALL
                    .into_iter()
                    .map(|lane| ctx.jobs.queue_state(lane))
                    .collect();
                let summary = format!("{} job(s)", jobs.len());
                Ok((
                    ListResult {
                        jobs,
                        next_cursor,
                        lanes,
                    },
                    summary,
                ))
            })
        },
    ));

    reg.register(typed_tool(
        "job.wait",
        "Waits until a job is succeeded, failed or cancelled (or interrupted_unknown, which only the person can resolve), or for `timeout_s` seconds (1–90, default 90), and returns it with `done`. Call it again while `done` is false.",
        EffectClass::Read,
        JOBS_MODULE,
        |ctx, args: WaitArgs| {
            Box::pin(async move {
                let timeout = args
                    .timeout_s
                    .map_or(MAX_WAIT, Duration::from_secs);
                let (job, done) = ctx
                    .jobs
                    .wait(&ctx.job_caller(), &JobId(args.job_id), timeout)
                    .await?;
                let summary = wait_summary(&job, done);
                Ok((WaitResult { job, done }, summary))
            })
        },
    ));

    reg.register(typed_tool(
        "job.cancel",
        "Cancels jobs: exactly one of `job_id`, `ids` (1–500 job ids), `batch_id` or `all_mine: true` (every unfinished job your caller kind submitted). A queued job is cancelled at once, a running one through its engine (it is cancelled when it stops). A finished job is returned as it is.",
        EffectClass::Write,
        JOBS_MODULE,
        |ctx, args: CancelArgs| {
            Box::pin(async move {
                let selector = args
                    .selector()
                    .map_err(|why| params_invalid(why, &ctx.trace_id))?;
                let one = matches!(selector, CancelSelector::One(_));
                let mut jobs = ctx
                    .jobs
                    .cancel_selected(&ctx.job_caller(), selector)
                    .await?;
                if one && let Some(job) = jobs.pop() {
                    let summary = format!("job {} is {}", job.job_id, job.status.wire());
                    return Ok((CancelResult::One { job: Box::new(job) }, summary));
                }
                let summary = format!("{} job(s) cancelled or ended", jobs.len());
                Ok((CancelResult::Many { jobs }, summary))
            })
        },
    ));

    reg.register(typed_tool(
        JOB_RETRY,
        "Runs a failed or cancelled job again as a new job, whose `retry_of` is the old one and which keeps its batch and index: `job_id`, or `batch_id` for the newest attempt at each index of a batch that failed or was cancelled (the others are listed in `refused`). A job already retried is never retried again. The tool call that made the job runs again for you, so it asks for the same confirmations and counts against the same caps.",
        EffectClass::Write,
        JOBS_MODULE,
        |ctx, args: RetryArgs| {
            Box::pin(async move {
                let Some(redispatch) = &ctx.redispatch else {
                    return Err(ErrorBody::new(
                        &codes::CORE_BUG,
                        "job.retry ran outside the dispatcher. This is a bug in LILITH.",
                        ctx.trace_id.clone(),
                    ));
                };
                match (args.job_id, args.batch_id) {
                    (Some(job_id), None) => {
                        let job = redispatch.retry(ctx, &JobId(job_id.clone())).await?;
                        let summary = format!("job {} retries {job_id}", job.job_id);
                        Ok((
                            RetryResult::One {
                                job: Box::new(job),
                                retry_of: job_id,
                            },
                            summary,
                        ))
                    }
                    (None, Some(batch_id)) => {
                        let caller = ctx.job_caller();
                        let batch = ctx
                            .jobs
                            .retry_batch(&caller, &batch_id, |id| {
                                Box::pin(async move { redispatch.retry_in_batch(ctx, &id).await })
                            })
                            .await?;
                        let summary = format!(
                            "{} job(s) of batch {batch_id} retried, {} not",
                            batch.retried.len(),
                            batch.refused.len()
                        );
                        Ok((RetryResult::Batch(batch), summary))
                    }
                    _ => Err(params_invalid(
                        "job.retry takes exactly one of job_id or batch_id".to_string(),
                        &ctx.trace_id,
                    )),
                }
            })
        },
    ));

    reg.register(typed_tool(
        "job.move",
        "Moves a job that waits its turn to the front (`next`) or the back (`last`) of its own class in its lane. It never moves a job ahead of a higher class, nor touches a prompt in the engine; a job with the engine, running or finished gets job.not_queued.",
        EffectClass::Write,
        JOBS_MODULE,
        |ctx, args: MoveArgs| {
            Box::pin(async move {
                let job = ctx
                    .jobs
                    .move_job(&ctx.job_caller(), &JobId(args.job_id), args.to)
                    .await?;
                let summary = format!("job {} moved", job.job_id);
                Ok((JobResultOut { job }, summary))
            })
        },
    ));

    reg.register(typed_tool(
        "job.estimate",
        "Estimates a tool call before it runs, from the measured history of similar jobs (the newest 20 that succeeded, at least 3): `local` (p50 and p90 GPU-seconds and watt-hours, and their energy cost when an electricity rate is set) and `wall_s` (p50 and p90 seconds), each with its `source` and `samples`. `unknown` lists the parts it couldn't estimate and why (no_history, no_key, no_price). `args` is checked as the call would check it. Nothing runs.",
        EffectClass::Read,
        JOBS_MODULE,
        |ctx, args: EstimateArgs| {
            Box::pin(async move {
                let Some(redispatch) = &ctx.redispatch else {
                    return Err(ErrorBody::new(
                        &codes::CORE_BUG,
                        "job.estimate ran outside the dispatcher. This is a bug in LILITH.",
                        ctx.trace_id.clone(),
                    ));
                };
                let tool = redispatch
                    .tool(&args.tool, &ctx.session.caller)
                    .ok_or_else(|| {
                        ErrorBody::new(
                            &codes::TOOL_UNKNOWN,
                            format!("There is no tool called {:?}.", args.tool),
                            ctx.trace_id.clone(),
                        )
                    })?;
                let call = serde_json::Value::Object(args.args);
                tool.validate(&call).map_err(|why| {
                    params_invalid(
                        format!("Invalid arguments for {}: {why}", args.tool),
                        &ctx.trace_id,
                    )
                })?;
                let estimate: Estimate = match tool.estimate_key(&call) {
                    Some(k) => ctx.jobs.estimate(k.kind, Some(&k.key)).await,
                    None => ctx.jobs.estimate(&args.tool, None).await,
                };
                let summary = match &estimate.wall_s {
                    Some(w) => format!("{}: about {} s", args.tool, w.p50),
                    None => format!("{}: no estimate yet", args.tool),
                };
                Ok((estimate, summary))
            })
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(substatus: &str, reasons: &[&str]) -> JobSummary {
        serde_json::from_value(serde_json::json!({
            "job_id": "j_1", "kind": "t.job", "lane": "cpu", "priority": "agent",
            "status": "queued", "substatus": substatus, "created": "2026-10-26T00:00:00Z",
            "updated": "2026-10-26T00:00:00Z", "attempt": 1, "trace_id": "t_1",
            "batch_id": "b_1", "index": 0, "pause_reasons": reasons,
        }))
        .expect("a summary")
    }

    /// Checkpoint A M6: `job.wait` on a held job says what it waits for, so an agent stops
    /// polling a job only you can release.
    #[test] // spec: AI-113, CORE-160
    fn a_held_jobs_wait_says_who_it_waits_for() {
        assert_eq!(
            wait_summary(&job("queue_paused", &["restart"]), false),
            "job j_1 is queued, held (restart): it waits until you resume it in Jobs"
        );
        assert_eq!(
            wait_summary(&job("queue_paused", &["fullscreen", "manual"]), false),
            "job j_1 is queued, held (fullscreen, manual): it waits until you resume it in Jobs"
        );
        assert_eq!(
            wait_summary(&job("queue_paused", &["fullscreen"]), false),
            "job j_1 is queued, held (fullscreen)"
        );
        assert_eq!(
            wait_summary(&job("waiting", &[]), false),
            "job j_1 is queued"
        );
    }
}
