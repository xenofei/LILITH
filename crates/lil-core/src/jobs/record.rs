//! Job rows in `state.db` (CORE-117) and the jobs log line of a state change (CORE-009).
//!
//! Every change after the insert is [`update`], which never changes a row in a final status:
//! a job that succeeded, failed or was cancelled stays that way (ARCH-022, CORE-146).

use lil_protocol::ErrorBody;
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde_json::{Map, Value, json};

use super::{
    EngineKind, JobFilter, JobOrigin, JobResult, JobSpec, JobStatus, JobSummary, Lane, Priority,
};
use super::{Progress, Substatus};
use crate::state::{StateDb, StateError};

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
}

impl JobRow {
    /// The row's summary; `progress` comes from the running job, when there is one.
    pub fn summary(&self, progress: Option<Progress>) -> JobSummary {
        let time = |secs: i64| lil_schema::time::rfc3339(secs);
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
            created: time(self.created_utc),
            updated: time(self.updated_utc),
            started: self.started_utc.map(time),
            finished: self.finished_utc.map(time),
            attempt: self.attempt,
            progress,
            watchdog: self.watchdog.clone(),
            vram_unchecked: self.vram_unchecked,
            error: self.error.clone(),
            result: self.result.clone(),
            retry_of: self.retry_of.clone(),
            trace_id: self.trace_id.clone(),
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
    /// and `seq`, every field, `null` when unknown). `usage` and `hardware` come with Tasks 15
    /// and 9.
    pub fn log_record(&self) -> Map<String, Value> {
        let time = |secs: i64| Value::from(lil_schema::time::rfc3339(secs));
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
    /// Sets the engine's id for the job (CORE-120).
    pub external_id: Option<String>,
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
    external_side_effects, reservation, tool, tool_args, external_id";

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
    Ok(JobRow {
        seq: r.get(0)?,
        job_id: r.get(1)?,
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

fn get_in(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<JobRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM jobs WHERE job_id = ?1"),
        params![job_id],
        row,
    )
    .optional()
}

/// What [`insert`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Inserted {
    /// The row is in, with its `seq`.
    New(JobRow),
    /// Another job holds the row's idempotency scope and key already.
    Existing(JobRow),
}

/// Inserts `r` (its `seq` is assigned). A job that already holds `r`'s idempotency scope and
/// key is returned instead, untouched.
pub async fn insert(db: &StateDb, r: JobRow) -> Result<Inserted, StateError> {
    db.call(move |conn| {
        let mut run = || -> rusqlite::Result<Inserted> {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if let (Some(scope), Some(key)) = (&r.idem_scope, &r.idem_key) {
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
                    return Ok(Inserted::Existing(existing));
                }
            }
            tx.execute(
                "INSERT INTO jobs (job_id, kind, lane, priority, module, args, project, asset_id,
                     doc_id, status, substatus, attempt, created_utc, updated_utc, started_utc,
                     finished_utc, watchdog, vram_unchecked, error, result, retry_of, trace_id,
                     idem_scope, idem_key, args_hash, expected_secs, expected_output_bytes,
                     vram_need_mib, engine, grp, external_side_effects, reservation, tool,
                     tool_args, external_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31,
                     ?32, ?33, ?34, ?35)",
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
                ],
            )?;
            let inserted = get_in(&tx, &r.job_id)?.expect("the row was just inserted");
            tx.commit()?;
            Ok(Inserted::New(inserted))
        };
        run().map_err(|e| sql(&e))
    })
    .await
}

/// The row of `job_id`.
pub async fn get(db: &StateDb, job_id: &str) -> Result<Option<JobRow>, StateError> {
    let job_id = job_id.to_string();
    db.call(move |conn| get_in(conn, &job_id).map_err(|e| sql(&e)))
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
                r.external_id = Some(external_id);
            }
            r.updated_utc = now_utc;
            tx.execute(
                "UPDATE jobs SET status = ?2, substatus = ?3, attempt = ?4, updated_utc = ?5,
                     started_utc = ?6, finished_utc = ?7, error = ?8, result = ?9,
                     vram_unchecked = ?10, watchdog = ?11, external_id = ?12
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
                ],
            )?;
            tx.commit()?;
            Ok(Some((before, r)))
        };
        run().map_err(|e| sql(&e))
    })
    .await
}

/// The automatic re-run of `job_id` (CORE-128): the first job whose `retry_of` it is and that
/// has no idempotency key (a `job.retry` always has one).
pub async fn auto_rerun_of(db: &StateDb, job_id: &str) -> Result<Option<String>, StateError> {
    let job_id = job_id.to_string();
    db.call(move |conn| {
        conn.query_row(
            "SELECT job_id FROM jobs WHERE retry_of = ?1 AND idem_key IS NULL ORDER BY seq LIMIT 1",
            params![job_id],
            |r| r.get(0),
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
pub async fn list(
    db: &StateDb,
    f: &JobFilter,
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
