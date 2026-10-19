//! Usage records (CORE-069, CORE-071; Appendix F "Usage records"): the record of a usage line,
//! in the appendix's field order, and the write-ahead rows of `usage_pending` in the
//! `state.db` of the call's scope.
//!
//! A row is `pending` from before the request is sent until its call ends; then it becomes
//! `logging` with the finished record (the step just before the line is appended), and it is
//! removed once the line is in. So a row of a dead core that is still `pending` had no line, and
//! one that is `logging` may have one: startup looks for that line before it writes it.

use lil_schema::money::{Currency, Money};
use rusqlite::{OptionalExtension as _, params};
use serde_json::{Map, Value, json};

use super::sink::{UsageCtx, UsageDraft, UsageStatus};
use crate::state::{StateDb, StateError};

/// The units a record may carry, in the order Appendix F lists them.
pub const UNITS: &[&str] = &[
    "input_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "cache_ttl",
    "output_tokens",
    "reasoning_tokens",
    "images",
    "seconds",
    "characters",
    "requests",
    "gpu_seconds",
    "energy_wh_gross",
    "energy_wh_above_idle",
];

/// The `workspace` of a record without one.
pub const LOOSE: &str = "loose";

/// A decimal string of `m` (CORE-013): never a JSON number.
pub fn money_value(m: Money) -> Value {
    Value::String(m.to_decimal_string())
}

/// The record of `d` for `ctx` at `ts` (RFC 3339 UTC with milliseconds), `status: "pending"`,
/// without a cost: [`finish`] completes it. Fields follow Appendix F's order; a unit that
/// doesn't apply is left out, every other field is present (`null` when unknown).
pub fn draft_record(ts: &str, ctx: &UsageCtx, d: &UsageDraft) -> Map<String, Value> {
    let workspace = ctx
        .workspace
        .as_ref()
        .map_or_else(|| LOOSE.to_string(), |w| w.ws.id.clone());
    let opt = |v: &Option<String>| v.clone().map_or(Value::Null, Value::String);
    let mut r = Map::new();
    r.insert("ts".into(), json!(ts));
    r.insert("workspace".into(), json!(workspace));
    r.insert("project".into(), opt(&ctx.project));
    r.insert("asset".into(), opt(&ctx.asset));
    r.insert("job_id".into(), opt(&ctx.job_id));
    r.insert("tool".into(), json!(d.tool));
    r.insert("caller_kind".into(), json!(ctx.caller_kind));
    r.insert("account".into(), opt(&d.account));
    r.insert("provider".into(), json!(d.provider));
    r.insert("model".into(), opt(&d.model));
    r.insert("route".into(), json!(d.route));
    r.insert("service_tier".into(), Value::Null);
    r.insert("region".into(), Value::Null);
    r.insert("billing_basis".into(), json!(d.basis.wire()));
    for unit in UNITS {
        if let Some(v) = d.units.get(*unit) {
            r.insert((*unit).to_string(), v.clone());
        }
    }
    // A unit the appendix doesn't list is kept too, after the listed ones.
    for (k, v) in &d.units {
        if !UNITS.contains(&k.as_str()) && k != "idle_unknown" {
            r.insert(k.clone(), v.clone());
        }
    }
    if d.units.get("idle_unknown") == Some(&Value::Bool(true)) {
        r.insert("idle_unknown".into(), Value::Bool(true));
    }
    r.insert(
        "raw_usage".into(),
        d.raw_usage.clone().unwrap_or(Value::Null),
    );
    let (est, currency) = match &d.est_cost {
        Some((m, c)) => (money_value(*m), json!(c.as_str())),
        None => (Value::Null, Value::Null),
    };
    r.insert("est_cost".into(), est);
    r.insert("cost".into(), Value::Null);
    r.insert("energy_cost".into(), Value::Null);
    r.insert("cost_source".into(), Value::Null);
    r.insert("price_version".into(), Value::Null);
    r.insert("currency".into(), currency);
    r.insert("latency_ms".into(), Value::Null);
    r.insert("status".into(), json!(UsageStatus::Pending.wire()));
    r.insert("attribution".into(), json!(d.attribution.wire()));
    r
}

/// Completes `r` with how its call ended. `cost_source` is written only with a cost; a cost in
/// another currency than the estimate's names its own.
pub fn finish(
    r: &mut Map<String, Value>,
    cost: Option<(Money, Currency)>,
    cost_source: &str,
    status: &str,
    latency_ms: Option<u64>,
) {
    if let Some((m, c)) = cost {
        r.insert("cost".into(), money_value(m));
        r.insert("cost_source".into(), json!(cost_source));
        r.insert("currency".into(), json!(c.as_str()));
    }
    r.insert(
        "latency_ms".into(),
        latency_ms.map_or(Value::Null, |ms| json!(ms)),
    );
    r.insert("status".into(), json!(status));
}

/// The estimate of a record: `est_cost` in `currency`, when both read.
pub fn est_cost(r: &Map<String, Value>) -> Option<(Money, Currency)> {
    let m = Money::parse_decimal(r.get("est_cost")?.as_str()?).ok()?;
    let c = Currency::parse(r.get("currency")?.as_str()?).ok()?;
    Some((m, c))
}

/// One `usage_pending` row.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRow {
    pub id: String,
    pub run: String,
    pub call_id: String,
    pub job_id: Option<String>,
    pub reservation: Option<String>,
    pub record: Map<String, Value>,
    /// `false`: `pending`; `true`: `logging` (the line may be in the log).
    pub logging: bool,
    pub created_ms: i64,
    pub logging_ms: Option<i64>,
}

fn sql(e: &rusqlite::Error) -> StateError {
    StateError::Io(e.to_string())
}

fn text(r: &Map<String, Value>) -> String {
    serde_json::to_string(r).unwrap_or_else(|_| "{}".to_string())
}

/// Writes `row` (committed when this returns).
pub async fn insert(db: &StateDb, row: &PendingRow) -> Result<(), StateError> {
    let row = row.clone();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO usage_pending (id, run, call_id, job_id, reservation, record, state,
                 created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![
                row.id,
                row.run,
                row.call_id,
                row.job_id,
                row.reservation,
                text(&row.record),
                row.created_ms
            ],
        )
        .map(drop)
        .map_err(|e| sql(&e))
    })
    .await
}

/// Row `id` becomes `logging` with its finished `record`.
pub async fn mark_logging(
    db: &StateDb,
    id: &str,
    record: &Map<String, Value>,
    at_ms: i64,
) -> Result<(), StateError> {
    let (id, record) = (id.to_string(), text(record));
    db.call(move |conn| {
        conn.execute(
            "UPDATE usage_pending SET state = 'logging', record = ?2, logging_ms = ?3
             WHERE id = ?1",
            params![id, record, at_ms],
        )
        .map(drop)
        .map_err(|e| sql(&e))
    })
    .await
}

/// Removes row `id` (gone already is fine).
pub async fn delete(db: &StateDb, id: &str) -> Result<(), StateError> {
    let id = id.to_string();
    db.call(move |conn| {
        conn.execute("DELETE FROM usage_pending WHERE id = ?1", params![id])
            .map(drop)
            .map_err(|e| sql(&e))
    })
    .await
}

/// Row `id`, if it is there.
pub async fn get(db: &StateDb, id: &str) -> Result<Option<PendingRow>, StateError> {
    let id = id.to_string();
    db.call(move |conn| {
        conn.query_row(
            "SELECT id, run, call_id, job_id, reservation, record, state, created_ms, logging_ms
             FROM usage_pending WHERE id = ?1",
            params![id],
            read_row,
        )
        .optional()
        .map_err(|e| sql(&e))
    })
    .await
}

/// Every row another core run wrote, oldest first.
pub async fn left_by_others(db: &StateDb, run: &str) -> Result<Vec<PendingRow>, StateError> {
    let run = run.to_string();
    db.call(move |conn| {
        let read = || -> rusqlite::Result<Vec<PendingRow>> {
            conn.prepare(
                "SELECT id, run, call_id, job_id, reservation, record, state, created_ms,
                     logging_ms
                 FROM usage_pending WHERE run <> ?1 ORDER BY created_ms, id",
            )?
            .query_map(params![run], read_row)?
            .collect()
        };
        read().map_err(|e| sql(&e))
    })
    .await
}

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PendingRow> {
    let record: String = r.get(5)?;
    let state: String = r.get(6)?;
    Ok(PendingRow {
        id: r.get(0)?,
        run: r.get(1)?,
        call_id: r.get(2)?,
        job_id: r.get(3)?,
        reservation: r.get(4)?,
        record: serde_json::from_str(&record).unwrap_or_default(),
        logging: state == "logging",
        created_ms: r.get(7)?,
        logging_ms: r.get(8)?,
    })
}
