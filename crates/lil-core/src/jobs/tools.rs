//! The job tools (CORE-146, AI-113; Appendix F "Product tools"), module `jobs`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `jobs.list` | read | `{status?, kind?, project?, ids?, limit? (1–500, default 100), cursor?}` | `{jobs, next_cursor}`, newest first |
//! | `job.wait` | read | `{job_id, timeout_s? (1–90, default 90)}` | `{job, done}` |
//! | `job.cancel` | write | `{job_id}` | `{job}`; a finished job as it is |
//! | `job.retry` | write | `{job_id}` | `{job, retry_of}`; only `failed` or `cancelled` |
//!
//! Each works in the call's scope: the session's workspace, or the suite without one.

use std::time::Duration;

use lil_protocol::EffectClass;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{JOBS_MODULE, JobFilter, JobId, JobSummary, MAX_WAIT};
use crate::tools::{Registry, typed_tool};

#[derive(Debug, Serialize, JsonSchema)]
struct ListResult {
    jobs: Vec<JobSummary>,
    /// Pass it as `cursor` for the next page; `null` on the last.
    next_cursor: Option<String>,
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
    /// The job is `succeeded`, `failed` or `cancelled`.
    done: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JobArgs {
    job_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct JobResultOut {
    job: JobSummary,
}

#[derive(Debug, Serialize, JsonSchema)]
struct RetryResult {
    job: JobSummary,
    retry_of: String,
}

/// Registers `jobs.list`, `job.wait`, `job.cancel` and `job.retry` on `reg`.
pub fn register_job_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "jobs.list",
        "Lists jobs, newest first: `status`, `kind`, `project` and `ids` filter them; `limit` (1–500, default 100) and `cursor` (the previous page's `next_cursor`) page through them.",
        EffectClass::Read,
        JOBS_MODULE,
        |ctx, f: JobFilter| {
            Box::pin(async move {
                let (jobs, next_cursor) = ctx.jobs.list(ctx, f).await?;
                let summary = format!("{} job(s)", jobs.len());
                Ok((ListResult { jobs, next_cursor }, summary))
            })
        },
    ));

    reg.register(typed_tool(
        "job.wait",
        "Waits until a job is succeeded, failed or cancelled, or for `timeout_s` seconds (1–90, default 90), and returns it with `done`. Call it again while `done` is false.",
        EffectClass::Read,
        JOBS_MODULE,
        |ctx, args: WaitArgs| {
            Box::pin(async move {
                let timeout = args
                    .timeout_s
                    .map_or(MAX_WAIT, Duration::from_secs);
                let (job, done) = ctx.jobs.wait(ctx, &JobId(args.job_id), timeout).await?;
                let summary = format!("job {} is {}", job.job_id, job.status.wire());
                Ok((WaitResult { job, done }, summary))
            })
        },
    ));

    reg.register(typed_tool(
        "job.cancel",
        "Cancels a job: a queued job at once, a running one through its engine (it is cancelled when it stops). A finished job is returned as it is.",
        EffectClass::Write,
        JOBS_MODULE,
        |ctx, args: JobArgs| {
            Box::pin(async move {
                let job = ctx.jobs.cancel(ctx, &JobId(args.job_id)).await?;
                let summary = format!("job {} is {}", job.job_id, job.status.wire());
                Ok((JobResultOut { job }, summary))
            })
        },
    ));

    reg.register(typed_tool(
        "job.retry",
        "Runs a failed or cancelled job again as a new job, whose `retry_of` is the old one.",
        EffectClass::Write,
        JOBS_MODULE,
        |ctx, args: JobArgs| {
            Box::pin(async move {
                let job = ctx.jobs.retry(ctx, &JobId(args.job_id.clone())).await?;
                let summary = format!("job {} retries {}", job.job_id, args.job_id);
                Ok((
                    RetryResult {
                        job,
                        retry_of: args.job_id,
                    },
                    summary,
                ))
            })
        },
    ));
}
