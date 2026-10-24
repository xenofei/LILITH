//! Job rows in `state.db` (CORE-117) and the jobs log line of a state change (CORE-009).
//!
//! Every change after the insert is [`update`], which never changes a row in a final status:
//! a job that succeeded, failed or was cancelled stays that way (ARCH-022, CORE-146). Each
//! change of the status or the substatus is also added to the row's `status_seq` (the first
//! entry and the newest 63, Appendix G).

use lil_protocol::ErrorBody;
use lil_protocol::estimate::Estimate;
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde_json::{Map, Value, json};

use super::{
    EngineKind, JobFilter, JobOrigin, JobResult, JobSpec, JobStatus, JobSummary, Lane, Priority,
    StartedBy, StatusStep, Subtask,
};
use super::{Progress, Substatus};
use crate::state::{StateDb, StateError};

/// The most entries a job's `status_seq` keeps: the first and the newest 63 (Appendix G).
pub const STATUS_SEQ_MAX: usize = 64;

/// What a job's run reported (Appendix G "Jobs"): kept in memory while this core runs the
/// job, and written with each of its state changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunReport {
    /// The GPU adapter of a job that held the GPU lease.
    pub engine_device: Option<String>,
    pub phase: Option<String>,
    /// Each phase reached, in order; the current one with its newest progress.
    pub subtasks: Vec<Subtask>,
    /// The executing node has sent no progress event.
    pub indeterminate: bool,
    /// The newest `executing` or `progress` event, Unix ms.
    pub last_progress_ms: Option<i64>,
    /// The nodes the engine served from its cache.
    pub cache_hit_nodes: Option<Vec<String>>,
    /// Every node of the prompt was cached: nothing changed.
    pub all_cached: bool,
}

/// What a summary shows on top of the row for a job this core runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveView {
    pub progress: Option<Progress>,
    /// The run's report as it is now (newer than the row's).
    pub report: Option<RunReport>,
    pub unresponsive: bool,
    /// The reasons holding a job in `queue_paused`.
    pub pause_reasons: Vec<String>,
}

/// One `jobs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRow {
    pub seq: i64,
    pub job_id: String,
    pub kind: String,
    pub lane: Lane,
    pub priority: Priority,
    pub module: String,
    pub args: Value,
    pub project: Option<String>,
    pub asset_id: Option<String>,
    pub doc_id: Option<String>,
    pub status: JobStatus,
    pub substatus: Option<Substatus>,
    pub attempt: u32,
    pub created_utc: i64,
    pub updated_utc: i64,
    pub started_utc: Option<i64>,
    pub finished_utc: Option<i64>,
    pub watchdog: Option<String>,
    pub vram_unchecked: bool,
    pub error: Option<ErrorBody>,
    pub result: Option<JobResult>,
    pub retry_of: Option<String>,
    pub trace_id: String,
    pub idem_scope: Option<String>,
    pub idem_key: Option<String>,
    pub args_hash: String,
    pub expected_secs: u32,
    pub expected_output_bytes: u64,
    pub vram_need_mib: Option<u64>,
    pub engine: Option<EngineKind>,
    pub group: Option<String>,
    pub external_side_effects: bool,
    pub reservation: Option<String>,
    /// The tool call that submitted the job (`job.retry` dispatches it again).
    pub origin: Option<JobOrigin>,
    /// The engine's id for the job (ComfyUI's `prompt_id`, CORE-120), written by its handler
    /// before it is sent ([`super::JobCtx::set_external_id`]).
    pub external_id: Option<String>,
    /// The job's batch, `b_<16 hex>` (P4a-R13). An insert with an empty one gets
    /// `b_<the job id's 16 hex>`.
    pub batch_id: String,
    /// Its 0-based place in the batch.
    pub batch_index: u32,
    /// Its place in its class (CORE-160): the core-wide submission sequence the runner gives
    /// it (Appendix G), or, for an insert with none (0), the row's `seq`.
    pub order_key: i64,
    /// Who started it: the `JobSummary`'s `origin` (P4a-R12).
    pub started_by: Option<StartedBy>,
    /// Its status changes, in order (the first and the newest 63).
    pub status_seq: Vec<StatusStep>,
    pub report: RunReport,
    /// The prompt id a pause withdrew (CORE-160), until its next attempt knows it gone
    /// (CORE-120).
    pub withdrawn_id: Option<String>,
    /// The estimate taken at submission (CORE-161, P4a-R40): written with the row, never
    /// changed.
    pub estimate: Option<Estimate>,
    /// The handler's estimate key for the job (Appendix G): `meter` writes the job's history
    /// under it when the job succeeds.
    pub estimate_key: Option<String>,
}

/// `b_<the job id's 16 hex>`: the batch of a job that has none (a row from before plan 4a).
pub fn own_batch(job_id: &str) -> String {
    format!("b_{}", job_id.strip_prefix("j_").unwrap_or(job_id))
}

fn time_of(secs: i64) -> String {
    lil_schema::time::rfc3339(secs)
}

impl JobRow {
    /// The row's summary, with what a job this core runs shows on top of it.
    pub fn summary(&self, live: Option<&LiveView>) -> JobSummary {
        let report = live.and_then(|l| l.report.as_ref()).unwrap_or(&self.report);
        let subtasks = report.subtasks.clone();
        JobSummary {
            job_id: self.job_id.clone(),
            kind: self.kind.clone(),
            lane: self.lane,
            priority: self.priority,
            status: self.status,
            substatus: self.substatus.map(|s| s.wire()),
            project: self.project.clone(),
            asset_id: self.asset_id.clone(),
            doc_id: self.doc_id.clone(),
            created: time_of(self.created_utc),
            updated: time_of(self.updated_utc),
            started: self.started_utc.map(time_of),
            finished: self.finished_utc.map(time_of),
            attempt: self.attempt,
            progress: live.and_then(|l| l.progress.clone()),
            watchdog: self.watchdog.clone(),
            vram_unchecked: self.vram_unchecked,
            error: self.error.clone(),
            result: self.result.clone(),
            retry_of: self.retry_of.clone(),
            trace_id: self.trace_id.clone(),
            batch_id: self.batch_id.clone(),
            index: self.batch_index,
            status_seq: self.status_seq.clone(),
            origin: self.started_by.clone(),
            engine_device: report.engine_device.clone(),
            phase: report.phase.clone(),
            subtasks,
            indeterminate: report.indeterminate && !self.status.is_final(),
            last_progress_at: report
                .last_progress_ms
                .map(|ms| time_of(ms.div_euclid(1000))),
            cache_hit_nodes: report.cache_hit_nodes.clone(),
            all_cached: report.all_cached,
            unresponsive: live.is_some_and(|l| l.unresponsive),
            pause_reasons: if self.substatus == Some(Substatus::QueuePaused) {
                live.map(|l| l.pause_reasons.clone()).unwrap_or_default()
            } else {
                Vec::new()
            },
            estimate: self.estimate.clone(),
        }
    }

    /// The spec the row was submitted with, for `kind` (the registered handler's own name).
    pub fn spec(&self, kind: &'static str) -> JobSpec {
        JobSpec {
            kind,
            lane: self.lane,
            priority: self.priority,
            module: self.module.clone(),
            args: self.args.clone(),
            project: self.project.clone(),
            asset_id: self.asset_id.clone(),
            doc_id: self.doc_id.clone(),
            expected_secs: self.expected_secs,
            expected_output_bytes: self.expected_output_bytes,
            vram_need_mib: self.vram_need_mib,
            engine: self.engine,
            group: self.group.clone(),
            external_side_effects: self.external_side_effects,
        }
    }

    /// The jobs log line of the row's current state (CORE-009; Appendix F: after `machine`
    /// and `seq`, every field, `null` when unknown; from plan 4a `origin` after `priority`,
    /// Appendix G). `usage` and `hardware` come with Tasks 15 and 9.
    pub fn log_record(&self) -> Map<String, Value> {
        let time = |secs: i64| Value::from(time_of(secs));
        let outputs = self.result.as_ref().map(|r| {
            r.artifacts
                .iter()
                .map(|a| Value::from(a.path.as_str()))
                .collect::<Vec<_>>()
        });
        let run_id = self
            .result
            .as_ref()
            .and_then(|r| r.structured.get("run_id"))
            .cloned();
        let Value::Object(map) = json!({
            "job_id": self.job_id,
            "kind": self.kind,
            "lane": self.lane.wire(),
            "priority": self.priority.wire(),
            "origin": self.started_by,
            "project": self.project,
            "asset_id": self.asset_id,
            "module": self.module,
            "run_id": run_id,
            "doc_id": self.doc_id,
            "status": self.status.wire(),
            "substatus": self.substatus.map(|s| s.wire()),
            "attempt": self.attempt,
            "submitted": time(self.created_utc),
            "updated": time(self.updated_utc),
            "outputs": outputs,
            "error": self.error,
            "idempotency_key": self.idem_key,
            "trace_id": self.trace_id,
            "usage": Value::Null,
            "hardware": Value::Null,
        }) else {
            unreachable!("a JSON object literal");
        };
        map
    }
}

/// A change of a row's state, applied by [`update`].
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub status: Option<JobStatus>,
    /// `Some(None)` clears it.
    pub substatus: Option<Option<Substatus>>,
    pub attempt: Option<u32>,
    pub started_utc: Option<i64>,
    pub finished_utc: Option<i64>,
    /// `Some(None)` clears it.
    pub error: Option<Option<ErrorBody>>,
    pub result: Option<JobResult>,
    pub vram_unchecked: Option<bool>,
    /// Sets the watchdog flag (`"slow"`); it is never cleared.
    pub watchdog: Option<String>,
    /// Sets (or, `Some(None)`, clears) the engine's id for the job (CORE-120).
    pub external_id: Option<Option<String>>,
    /// Sets the job's place in its class (`job.move`).
    pub order_key: Option<i64>,
    /// Sets (or, `Some(None)`, clears) the prompt id a pause withdrew.
    pub withdrawn_id: Option<Option<String>>,
    /// Writes what the job's run reported.
    pub report: Option<RunReport>,
}

impl Change {
    /// To `substatus` and its status.
    pub fn to(substatus: Substatus) -> Change {
        Change {
            status: Some(substatus.status()),
            substatus: Some(Some(substatus)),
            ..Change::default()
        }
    }

    /// To a final `status` without a substatus.
    pub fn finished(status: JobStatus) -> Change {
        Change {
            status: Some(status),
            substatus: Some(None),
            ..Change::default()
        }
    }
}

const COLUMNS: &str = "seq, job_id, kind, lane, priority, module, args, project, asset_id, \
    doc_id, status, substatus, attempt, created_utc, updated_utc, started_utc, finished_utc, \
    watchdog, vram_unchecked, error, result, retry_of, trace_id, idem_scope, idem_key, \
    args_hash, expected_secs, expected_output_bytes, vram_need_mib, engine, grp, \
    external_side_effects, reservation, tool, tool_args, external_id, batch_id, batch_index, \
    order_key, origin, status_seq, engine_device, phase, subtasks, indeterminate, \
    last_progress_ms, cache_hit_nodes, all_cached, withdrawn_id, estimate, estimate_key";

/// A JSON column, `None` when it is NULL or doesn't parse (logged).
fn json_col<T: serde::de::DeserializeOwned>(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<T>> {
    let text: Option<String> = r.get(i)?;
    Ok(text.and_then(|t| {
        serde_json::from_str(&t)
            .map_err(|e| tracing::error!(column = i, error = %e, "a jobs column doesn't parse"))
            .ok()
    }))
}

fn bad(i: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        i,
        rusqlite::types::Type::Text,
        format!("not a {what}").into(),
    )
}

fn u32_col(r: &Row<'_>, i: usize) -> rusqlite::Result<u32> {
    let v: i64 = r.get(i)?;
    u32::try_from(v).map_err(|_| bad(i, "u32"))
}

fn u64_col(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<u64>> {
    let v: Option<i64> = r.get(i)?;
    v.map(|v| u64::try_from(v).map_err(|_| bad(i, "u64")))
        .transpose()
}

fn row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
    let lane: String = r.get(3)?;
    let priority: String = r.get(4)?;
    let status: String = r.get(10)?;
    let substatus: Option<String> = r.get(11)?;
    let engine: Option<String> = r.get(29)?;
    let seq: i64 = r.get(0)?;
    let job_id: String = r.get(1)?;
    let batch_id: Option<String> = r.get(36)?;
    let batch_index: Option<i64> = r.get(37)?;
    let order_key: Option<i64> = r.get(38)?;
    Ok(JobRow {
        seq,
        batch_id: batch_id.unwrap_or_else(|| own_batch(&job_id)),
        job_id,
        kind: r.get(2)?,
        lane: Lane::parse(&lane).ok_or_else(|| bad(3, "lane"))?,
        priority: Priority::parse(&priority).ok_or_else(|| bad(4, "priority"))?,
        module: r.get(5)?,
        args: json_col(r, 6)?.unwrap_or(Value::Null),
        project: r.get(7)?,
        asset_id: r.get(8)?,
        doc_id: r.get(9)?,
        status: JobStatus::parse(&status).ok_or_else(|| bad(10, "status"))?,
        substatus: substatus
            .map(|s| Substatus::parse(&s).ok_or_else(|| bad(11, "substatus")))
            .transpose()?,
        attempt: u32_col(r, 12)?,
        created_utc: r.get(13)?,
        updated_utc: r.get(14)?,
        started_utc: r.get(15)?,
        finished_utc: r.get(16)?,
        watchdog: r.get(17)?,
        vram_unchecked: r.get::<_, i64>(18)? != 0,
        error: json_col(r, 19)?,
        result: json_col(r, 20)?,
        retry_of: r.get(21)?,
        trace_id: r.get(22)?,
        idem_scope: r.get(23)?,
        idem_key: r.get(24)?,
        args_hash: r.get(25)?,
        expected_secs: u32_col(r, 26)?,
        expected_output_bytes: u64_col(r, 27)?.unwrap_or(0),
        vram_need_mib: u64_col(r, 28)?,
        engine: engine
            .map(|e| EngineKind::from_column(&e).ok_or_else(|| bad(29, "engine")))
            .transpose()?,
        group: r.get(30)?,
        external_side_effects: r.get::<_, i64>(31)? != 0,
        reservation: r.get(32)?,
        origin: match r.get::<_, Option<String>>(33)? {
            None => None,
            Some(tool) => Some(JobOrigin {
                tool,
                arguments: json_col(r, 34)?.unwrap_or(Value::Null),
            }),
        },
        external_id: r.get(35)?,
        batch_index: batch_index.map_or(Ok(0), |i| u32::try_from(i).map_err(|_| bad(37, "u32")))?,
        order_key: order_key.unwrap_or(seq),
        started_by: json_col(r, 39)?,
        status_seq: json_col(r, 40)?.unwrap_or_default(),
        report: RunReport {
            engine_device: r.get(41)?,
            phase: r.get(42)?,
            subtasks: json_col(r, 43)?.unwrap_or_default(),
            indeterminate: r.get::<_, i64>(44)? != 0,
            last_progress_ms: r.get(45)?,
            cache_hit_nodes: json_col(r, 46)?,
            all_cached: r.get::<_, i64>(47)? != 0,
        },
        withdrawn_id: r.get(48)?,
        estimate: json_col(r, 49)?,
        estimate_key: r.get(50)?,
    })
}

fn sql(e: &rusqlite::Error) -> StateError {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            StateError::Corrupt(e.to_string())
        }
        _ => StateError::Io(e.to_string()),
    }
}

fn to_text<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("a job column serialises")
}

fn i64_of(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Adds a step for `status`/`substatus` at `at_utc`, keeping the first and the newest 63.
fn push_step(
    seq: &mut Vec<StatusStep>,
    status: JobStatus,
    substatus: Option<Substatus>,
    at_utc: i64,
) {
    seq.push(StatusStep {
        status,
        substatus: substatus.map(|s| s.wire()),
        at: time_of(at_utc),
    });
    while seq.len() > STATUS_SEQ_MAX {
        seq.remove(1);
    }
}

fn get_in(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<JobRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM jobs WHERE job_id = ?1"),
        params![job_id],
        row,
    )
    .optional()
}

/// The rows of `batch_id`, by index, then by `seq`.
fn batch_in(conn: &Connection, batch_id: &str) -> rusqlite::Result<Vec<JobRow>> {
    conn.prepare(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE batch_id = ?1 ORDER BY batch_index, seq"
    ))?
    .query_map(params![batch_id], row)?
    .collect()
}

/// What [`insert`] and [`insert_batch`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Inserted<T> {
    /// The rows are in, each with its `seq`.
    New(T),
    /// Another job holds the first row's idempotency scope and key already: that job (for a
    /// batch, the jobs its submit made: every job of its batch that retries none).
    Existing(T),
    /// The job it would retry was retried already, by this job (`job.retry` never retries a
    /// job twice, Appendix G).
    Retried(String),
}

/// One row's insert in `tx`: its `seq` is assigned, and its `order_key` when it has none (0:
/// its `seq`); its `status_seq` starts with its state.
fn insert_in(tx: &rusqlite::Transaction<'_>, mut r: JobRow) -> rusqlite::Result<JobRow> {
    if r.batch_id.is_empty() {
        r.batch_id = own_batch(&r.job_id);
    }
    if r.status_seq.is_empty() {
        push_step(&mut r.status_seq, r.status, r.substatus, r.created_utc);
    }
    tx.execute(
        "INSERT INTO jobs (job_id, kind, lane, priority, module, args, project, asset_id,
             doc_id, status, substatus, attempt, created_utc, updated_utc, started_utc,
             finished_utc, watchdog, vram_unchecked, error, result, retry_of, trace_id,
             idem_scope, idem_key, args_hash, expected_secs, expected_output_bytes,
             vram_need_mib, engine, grp, external_side_effects, reservation, tool,
             tool_args, external_id, batch_id, batch_index, origin, status_seq, order_key,
             withdrawn_id, estimate, estimate_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
             ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31,
             ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43)",
        params![
            r.job_id,
            r.kind,
            r.lane.wire(),
            r.priority.wire(),
            r.module,
            to_text(&r.args),
            r.project,
            r.asset_id,
            r.doc_id,
            r.status.wire(),
            r.substatus.map(|s| s.wire()),
            r.attempt,
            r.created_utc,
            r.updated_utc,
            r.started_utc,
            r.finished_utc,
            r.watchdog,
            i64::from(r.vram_unchecked),
            r.error.as_ref().map(to_text),
            r.result.as_ref().map(to_text),
            r.retry_of,
            r.trace_id,
            r.idem_scope,
            r.idem_key,
            r.args_hash,
            r.expected_secs,
            i64_of(r.expected_output_bytes),
            r.vram_need_mib.map(i64_of),
            r.engine.map(EngineKind::wire),
            r.group,
            i64::from(r.external_side_effects),
            r.reservation,
            r.origin.as_ref().map(|o| o.tool.as_str()),
            r.origin.as_ref().map(|o| to_text(&o.arguments)),
            r.external_id,
            r.batch_id,
            r.batch_index,
            r.started_by.as_ref().map(to_text),
            to_text(&r.status_seq),
            (r.order_key != 0).then_some(r.order_key),
            r.withdrawn_id,
            r.estimate.as_ref().map(to_text),
            r.estimate_key,
        ],
    )?;
    tx.execute(
        "UPDATE jobs SET order_key = seq WHERE job_id = ?1 AND order_key IS NULL",
        params![r.job_id],
    )?;
    Ok(get_in(tx, &r.job_id)?.expect("the row was just inserted"))
}

/// Inserts the rows of one batch in one transaction, in order (their `seq` is assigned). When
/// a job holds the first row's idempotency scope and key already, every job of its batch is
/// returned instead, untouched; when a row retries a job that another job already retried,
/// nothing is inserted.
pub async fn insert_batch(
    db: &StateDb,
    rows: Vec<JobRow>,
) -> Result<Inserted<Vec<JobRow>>, StateError> {
    db.call(move |conn| {
        let mut run = || -> rusqlite::Result<Inserted<Vec<JobRow>>> {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if let Some(first) = rows.first()
                && let (Some(scope), Some(key)) = (&first.idem_scope, &first.idem_key)
            {
                let existing = tx
                    .query_row(
                        &format!(
                            "SELECT {COLUMNS} FROM jobs WHERE idem_scope = ?1 AND idem_key = ?2"
                        ),
                        params![scope, key],
                        row,
                    )
                    .optional()?;
                if let Some(existing) = existing {
                    // One job for one: the key's own (a retry's key names its job alone, in
                    // the batch of the job it retried). A batch: the jobs its submit made.
                    if rows.len() == 1 {
                        return Ok(Inserted::Existing(vec![existing]));
                    }
                    let batch: Vec<JobRow> = batch_in(&tx, &existing.batch_id)?
                        .into_iter()
                        .filter(|r| r.retry_of.is_none())
                        .collect();
                    return Ok(Inserted::Existing(if batch.is_empty() {
                        vec![existing]
                    } else {
                        batch
                    }));
                }
            }
            for r in &rows {
                if let Some(of) = &r.retry_of {
                    let retried: Option<String> = tx
                        .query_row(
                            "SELECT job_id FROM jobs WHERE retry_of = ?1 ORDER BY seq DESC LIMIT 1",
                            params![of],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(by) = retried {
                        return Ok(Inserted::Retried(by));
                    }
                }
            }
            let mut inserted = Vec::with_capacity(rows.len());
            for r in rows.clone() {
                inserted.push(insert_in(&tx, r)?);
            }
            tx.commit()?;
            Ok(Inserted::New(inserted))
        };
        run().map_err(|e| sql(&e))
    })
    .await
}

/// Inserts `r` (its `seq` is assigned) as a batch of one. A job that already holds `r`'s
/// idempotency scope and key is returned instead, untouched.
pub async fn insert(db: &StateDb, r: JobRow) -> Result<Inserted<JobRow>, StateError> {
    Ok(match insert_batch(db, vec![r]).await? {
        Inserted::New(mut rows) => Inserted::New(rows.remove(0)),
        Inserted::Existing(mut rows) => Inserted::Existing(rows.remove(0)),
        Inserted::Retried(by) => Inserted::Retried(by),
    })
}

/// The row of `job_id`.
pub async fn get(db: &StateDb, job_id: &str) -> Result<Option<JobRow>, StateError> {
    let job_id = job_id.to_string();
    db.call(move |conn| get_in(conn, &job_id).map_err(|e| sql(&e)))
        .await
}

/// Every row of batch `batch_id`, by index, then by `seq` (a retry after the job it retries).
pub async fn batch(db: &StateDb, batch_id: &str) -> Result<Vec<JobRow>, StateError> {
    let batch_id = batch_id.to_string();
    db.call(move |conn| batch_in(conn, &batch_id).map_err(|e| sql(&e)))
        .await
}

/// Applies `c` to `job_id` at `now_utc`, unless the row is gone or already in a final status:
/// the substatus before and the row after the change, or `None` when nothing changed.
pub async fn update(
    db: &StateDb,
    job_id: &str,
    c: Change,
    now_utc: i64,
) -> Result<Option<(Option<Substatus>, JobRow)>, StateError> {
    let job_id = job_id.to_string();
    db.call(move |conn| {
        let run = || -> rusqlite::Result<Option<(Option<Substatus>, JobRow)>> {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let Some(mut r) = get_in(&tx, &job_id)? else {
                return Ok(None);
            };
            if r.status.is_final() {
                return Ok(None);
            }
            let before = r.substatus;
            let before_status = r.status;
            if let Some(status) = c.status {
                r.status = status;
            }
            if let Some(substatus) = c.substatus {
                r.substatus = substatus;
            }
            if let Some(attempt) = c.attempt {
                r.attempt = attempt;
            }
            if c.started_utc.is_some() && r.started_utc.is_none() {
                r.started_utc = c.started_utc;
            }
            if c.finished_utc.is_some() {
                r.finished_utc = c.finished_utc;
            }
            if let Some(error) = c.error {
                r.error = error;
            }
            if let Some(result) = c.result {
                r.result = Some(result);
            }
            if let Some(unchecked) = c.vram_unchecked {
                r.vram_unchecked = unchecked;
            }
            if let Some(watchdog) = c.watchdog {
                r.watchdog = Some(watchdog);
            }
            if let Some(external_id) = c.external_id {
                r.external_id = external_id;
            }
            if let Some(key) = c.order_key {
                r.order_key = key;
            }
            if let Some(withdrawn_id) = c.withdrawn_id {
                r.withdrawn_id = withdrawn_id;
            }
            if let Some(report) = c.report {
                r.report = report;
            }
            if r.status != before_status || r.substatus != before {
                push_step(&mut r.status_seq, r.status, r.substatus, now_utc);
            }
            r.updated_utc = now_utc;
            let report = &r.report;
            tx.execute(
                "UPDATE jobs SET status = ?2, substatus = ?3, attempt = ?4, updated_utc = ?5,
                     started_utc = ?6, finished_utc = ?7, error = ?8, result = ?9,
                     vram_unchecked = ?10, watchdog = ?11, external_id = ?12, order_key = ?13,
                     status_seq = ?14, engine_device = ?15, phase = ?16, subtasks = ?17,
                     indeterminate = ?18, last_progress_ms = ?19, cache_hit_nodes = ?20,
                     all_cached = ?21, withdrawn_id = ?22
                 WHERE job_id = ?1",
                params![
                    job_id,
                    r.status.wire(),
                    r.substatus.map(|s| s.wire()),
                    r.attempt,
                    r.updated_utc,
                    r.started_utc,
                    r.finished_utc,
                    r.error.as_ref().map(to_text),
                    r.result.as_ref().map(to_text),
                    i64::from(r.vram_unchecked),
                    r.watchdog,
                    r.external_id,
                    r.order_key,
                    to_text(&r.status_seq),
                    report.engine_device,
                    report.phase,
                    (!report.subtasks.is_empty()).then(|| to_text(&report.subtasks)),
                    i64::from(report.indeterminate),
                    report.last_progress_ms,
                    report.cache_hit_nodes.as_ref().map(to_text),
                    i64::from(report.all_cached),
                    r.withdrawn_id,
                ],
            )?;
            tx.commit()?;
            Ok(Some((before, r)))
        };
        run().map_err(|e| sql(&e))
    })
    .await
}

/// The core-wide order counter's high-water mark (Appendix G "Queue order"), if any was
/// kept (the suite `state.db`).
pub async fn order_mark(db: &StateDb) -> Result<Option<i64>, StateError> {
    db.call(|conn| {
        conn.query_row("SELECT last_order FROM job_order WHERE k = 0", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| sql(&e))
    })
    .await
}

/// Raises the order counter's high-water mark to `key` (never lowers it).
pub async fn raise_order_mark(db: &StateDb, key: i64) -> Result<(), StateError> {
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO job_order (k, last_order) VALUES (0, ?1)
             ON CONFLICT(k) DO UPDATE SET last_order = max(last_order, excluded.last_order)",
            params![key],
        )
        .map(|_| ())
        .map_err(|e| sql(&e))
    })
    .await
}

/// The newest job whose `retry_of` is `job_id`: the attempt that retried it (a `job.retry`, a
/// batch retry or the automatic re-run), with its idempotency scope and key.
pub async fn retried_by(
    db: &StateDb,
    job_id: &str,
) -> Result<Option<(String, Option<String>, Option<String>)>, StateError> {
    let job_id = job_id.to_string();
    db.call(move |conn| {
        conn.query_row(
            "SELECT job_id, idem_scope, idem_key FROM jobs WHERE retry_of = ?1
             ORDER BY seq DESC LIMIT 1",
            params![job_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| sql(&e))
    })
    .await
}

/// The newest row whose `reservation` is `rv` (a job that holds a call's cap reservation, also
/// when the hand-over to it failed; an automatic re-run is newer than the job it re-runs).
pub async fn holding(db: &StateDb, rv: &str) -> Result<Option<JobRow>, StateError> {
    let rv = rv.to_string();
    db.call(move |conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM jobs WHERE reservation = ?1 ORDER BY seq DESC LIMIT 1"),
            params![rv],
            row,
        )
        .optional()
        .map_err(|e| sql(&e))
    })
    .await
}

/// The rows without a final status (recovery), by `seq`.
pub async fn open_rows(db: &StateDb) -> Result<Vec<JobRow>, StateError> {
    db.call(|conn| {
        let run = || -> rusqlite::Result<Vec<JobRow>> {
            conn.prepare(&format!(
                "SELECT {COLUMNS} FROM jobs WHERE status IN ('queued', 'running') ORDER BY seq"
            ))?
            .query_map([], row)?
            .collect()
        };
        run().map_err(|e| sql(&e))
    })
    .await
}

/// The rows `f` selects, newest first, at most `limit`, before the cursor `before` (a `seq`).
/// `since` is Unix seconds: the rows whose `updated` is later.
pub async fn list(
    db: &StateDb,
    f: &JobFilter,
    since: Option<i64>,
    limit: u32,
    before: Option<i64>,
) -> Result<Vec<JobRow>, StateError> {
    let f = f.clone();
    db.call(move |conn| {
        let run = || -> rusqlite::Result<Vec<JobRow>> {
            let mut sql_text = format!("SELECT {COLUMNS} FROM jobs WHERE 1 = 1");
            let mut values: Vec<rusqlite::types::Value> = Vec::new();
            let mut bind = |sql_text: &mut String, clause: &str, v: rusqlite::types::Value| {
                values.push(v);
                sql_text.push_str(&format!(" AND {clause} ?{}", values.len()));
            };
            if let Some(status) = f.status {
                bind(&mut sql_text, "status =", status.wire().to_string().into());
            }
            if let Some(kind) = &f.kind {
                bind(&mut sql_text, "kind =", kind.clone().into());
            }
            if let Some(project) = &f.project {
                bind(&mut sql_text, "project =", project.clone().into());
            }
            if let Some(batch_id) = &f.batch_id {
                bind(&mut sql_text, "batch_id =", batch_id.clone().into());
            }
            if let Some(since) = since {
                bind(&mut sql_text, "updated_utc >", since.into());
            }
            if let Some(before) = before {
                bind(&mut sql_text, "seq <", before.into());
            }
            if let Some(ids) = &f.ids {
                let ids = serde_json::to_string(ids).expect("ids serialise");
                bind(
                    &mut sql_text,
                    "job_id IN (SELECT value FROM json_each(",
                    ids.into(),
                );
                sql_text.push_str("))");
            }
            values.push(i64::from(limit).into());
            sql_text.push_str(&format!(" ORDER BY seq DESC LIMIT ?{}", values.len()));
            conn.prepare(&sql_text)?
                .query_map(rusqlite::params_from_iter(values), row)?
                .collect()
        };
        run().map_err(|e| sql(&e))
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::state::{OnFatal, StateScope};

    fn row(id: &str) -> JobRow {
        JobRow {
            seq: 0,
            job_id: id.to_string(),
            kind: "k".into(),
            lane: Lane::Cpu,
            priority: Priority::Agent,
            module: "m".into(),
            args: json!({}),
            project: None,
            asset_id: None,
            doc_id: None,
            status: JobStatus::Queued,
            substatus: Some(Substatus::Waiting),
            attempt: 1,
            created_utc: 0,
            updated_utc: 0,
            started_utc: None,
            finished_utc: None,
            watchdog: None,
            vram_unchecked: false,
            error: None,
            result: None,
            retry_of: None,
            trace_id: "t".into(),
            idem_scope: Some("cli|none".into()),
            idem_key: Some("key".into()),
            args_hash: "h".into(),
            expected_secs: 1,
            expected_output_bytes: u64::MAX,
            vram_need_mib: None,
            engine: None,
            group: None,
            external_side_effects: false,
            reservation: None,
            origin: None,
            external_id: None,
            batch_id: String::new(),
            batch_index: 0,
            order_key: 0,
            started_by: None,
            status_seq: Vec::new(),
            report: RunReport::default(),
            withdrawn_id: None,
            estimate: None,
            estimate_key: None,
        }
    }

    /// A job in a final status never changes again: no update reaches it (ARCH-022: a job
    /// that succeeded is never re-run or rewritten; CORE-146: a cancellation happens once).
    #[tokio::test] // spec: ARCH-022, CORE-146
    async fn a_final_row_is_never_updated() {
        let dir = tempfile::tempdir().unwrap();
        let on_fatal: OnFatal = Arc::new(|_: &str| {});
        let db = StateDb::open(&dir.path().join("state.db"), StateScope::Suite, on_fatal).unwrap();
        let Inserted::New(inserted) = insert(&db, row("j_1")).await.unwrap() else {
            panic!("a new row");
        };
        assert_eq!(inserted.expected_output_bytes, i64::MAX as u64, "clamped");
        assert_eq!(
            inserted.order_key, inserted.seq,
            "a new job's key is its seq"
        );
        assert_eq!(inserted.batch_id, "b_1", "a batch of its own");
        let (before, running) = update(&db, "j_1", Change::to(Substatus::Executing), 5)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before, Some(Substatus::Waiting));
        assert_eq!(
            (running.status, running.updated_utc),
            (JobStatus::Running, 5)
        );
        update(&db, "j_1", Change::finished(JobStatus::Succeeded), 6)
            .await
            .unwrap()
            .unwrap();
        for c in [
            Change::to(Substatus::Interrupted),
            Change::finished(JobStatus::Cancelled),
            Change::to(Substatus::Waiting),
        ] {
            assert_eq!(update(&db, "j_1", c, 7).await.unwrap(), None);
        }
        let kept = get(&db, "j_1").await.unwrap().unwrap();
        assert_eq!((kept.status, kept.updated_utc), (JobStatus::Succeeded, 6));
        let steps: Vec<(JobStatus, Option<String>)> = kept
            .status_seq
            .iter()
            .map(|s| (s.status, s.substatus.clone()))
            .collect();
        assert_eq!(
            steps,
            [
                (JobStatus::Queued, Some("waiting".to_string())),
                (JobStatus::Running, Some("executing".to_string())),
                (JobStatus::Succeeded, None),
            ]
        );
        assert!(open_rows(&db).await.unwrap().is_empty());
        // The same idempotency key gives back the row it made, untouched.
        let Inserted::Existing(existing) = insert(&db, row("j_2")).await.unwrap() else {
            panic!("the key's job");
        };
        assert_eq!(existing.job_id, "j_1");
        assert_eq!(
            update(&db, "j_404", Change::to(Substatus::Executing), 8)
                .await
                .unwrap(),
            None
        );
        db.close();
    }
}
