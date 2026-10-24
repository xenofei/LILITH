//! The `meter` module (CORE-014, CORE-064, CORE-065, CORE-069, CORE-071, CORE-081–083): usage
//! records for every external call, per-job energy and local-work records, and summaries.
//!
//! - **Write-ahead** (CORE-071). An external call is [`Meter::begin`] before its request is
//!   sent: a `pending` row in the `state.db` of the call's scope (the workspace's, or the
//!   suite's for a call without one), committed before `begin` returns. [`Meter::finish`]
//!   settles it with the outcome: the row becomes `logging` with the finished record, the line
//!   is appended (`registry/usage.<machine>.<yyyy-mm>.jsonl` through the store, or
//!   `usage-loose.jsonl` without a workspace; nothing in `meter` writes a file itself), and the
//!   row is removed. Each line is published on the `usage` topic as `{kind: "record", record}`.
//! - **Recovery.** A row of another core run was left by a dead core. At startup
//!   ([`Meter::recover`] for the suite scope, [`Meter::recover_workspace`] when a workspace
//!   opens) a `pending` row becomes a `status: "pending"` line; a `logging` row's line is looked
//!   for in the log first, and written only if it is not there, so a crash between the line and
//!   the row's removal never gives two lines. Reservations are [`caps::Caps`']: the meter
//!   neither keeps nor releases one, and the caps' repair reads a dead call's rows before the
//!   meter's recovery removes them. A call's row whose reservation is still held and not
//!   `kept` (the repair hasn't decided it) is left as it is, neither written nor removed, and
//!   a scope whose repair failed isn't recovered at all (ruling P3-R33).
//! - **Inline calls** (stage 8, [`stage::MeterSettle`]). A tool that leaves its row pending
//!   gets it finished at its estimate, with the call's status: its line first, then the call's
//!   reservation is settled, and only then is the row removed, so a crash in between leaves a
//!   `logging` row from which the next start settles the reservation once.
//! - **Jobs** ([`JobCompletion`]). A job that reported execution windows gets its energy
//!   attributed ([`attribution::attribute`]) and written with `Telemetry::put_job_energy`, and
//!   one local-work line (`billing_basis: "local"`, `cost: "0"`, its energy, and its energy
//!   cost at `[power] electricity` when set), written behind a `logging` row so a core that
//!   dies before the line is in writes it at the next start. A job whose handler recorded its
//!   own draft through [`UsageSink`] (the node install) owns its usage line, so it gets none
//!   from here (QA1 I5). An `interrupted_unknown` engine job you resolved gets a `cancelled`
//!   line, with its energy when this core run saw its windows, else without. A job that never
//!   started gets neither. The completion never settles a reservation: only Caps' does.
//!   A job with an estimate key (its row's, from its handler) also gets its history row
//!   ([`Telemetry::put_job_stats`], Appendix G) once its final state `succeeded` is in
//!   ([`JobCompletion::succeeded`]), so a job whose Record step failed, or one a shutdown
//!   interrupted and that runs again, never counts: the time of its execution windows, and
//!   its GPU-seconds and whole-system energy (0 without windows), so the estimator learns from
//!   it (CORE-161). A run with `all_cached` measured the cache, not the work, and adds none.
//!   The set of jobs that own their line is in memory: an entry goes at the job's
//!   completion (for an `interrupted_unknown` job, when you resolve it), and with the core.
//! - **Summaries** ([`summary`], `usage.summary`).

pub mod attribution;
pub mod caps;
pub mod caps_settings;
pub mod record;
pub mod sink;
pub mod stage;
pub mod summary;
pub mod tools;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::UNIX_EPOCH;

use futures_util::future::BoxFuture;
use lil_platform::{Clock, Env, IdSource};
use lil_protocol::{ErrorBody, Outcome, ToolOutput, codes};
use lil_schema::SuiteToml;
use lil_schema::money::{Currency, Money};
use serde_json::{Map, Value, json};

pub use attribution::attribute;
pub use caps::{CapCheck, Caps, Reservation, ReservationState, ReserveFor};
pub use caps_settings::CapsSettings;
pub use sink::{Attribution, PendingUsage, UsageCtx, UsageDraft, UsageSink, UsageStatus};
pub use stage::MeterSettle;
pub use summary::{MoneySettings, SummaryQuery};
pub use tools::{register_caps_tools, register_meter_tools};

use self::record::PendingRow;
use self::summary::{Period, Range};
use crate::events::EventBus;
use crate::jobs::{
    JobCompletion, JobCtx, JobDone, JobFuture, JobStatus, never_started, resolved_unknown,
};
use crate::state::{StateDb, StateError, StateRegistry};
use crate::store::{FailPoints, LogKind, WsPath, logs};
use crate::telemetry::{
    JobEnergy, JobEnergyQuery, JobStatsRow, PowerSettings, Telemetry, format_ts, parse_ts,
};
use crate::tools::stages::BillingBasis;
use crate::workspace::{Access, OpenWorkspace};

/// The module the meter tools declare.
pub const METER_MODULE: &str = "meter";
/// The topic of usage lines.
pub const TOPIC: &str = "usage";
/// The provider of a local-work record.
pub const LOCAL_PROVIDER: &str = "local";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each holder leaves its data usable before anything can panic.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn state_error(e: &StateError) -> ErrorBody {
    ErrorBody::new(
        &codes::STATE_UNAVAILABLE,
        format!("The usage records' state.db failed: {e}"),
        "",
    )
}

fn params_invalid(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(&codes::PARAMS_INVALID, message, "")
}

/// A usage status from a call's or job's outcome: `succeeded`, `cancelled` (a cancel, or an
/// `interrupted_unknown` job you resolved), else `failed`.
pub fn outcome_status(outcome: &Outcome<ToolOutput>) -> UsageStatus {
    match outcome {
        Outcome::Ok(_) | Outcome::PendingConfirmation(_) => UsageStatus::Succeeded,
        o if crate::jobs::is_cancelled(o) || resolved_unknown(o) => UsageStatus::Cancelled,
        Outcome::Error { .. } => UsageStatus::Failed,
    }
}

/// `mJ` as Wh, a decimal string with at most 6 decimals (rounded half to even).
fn wh(mj: u64) -> Value {
    let micro_wh = attribution::share(mj, 10, 36);
    Value::String(
        Money::from_micros(i64::try_from(micro_wh).unwrap_or(i64::MAX)).to_decimal_string(),
    )
}

/// A row this core run has begun and not finished.
struct Open {
    scope: Option<Arc<OpenWorkspace>>,
    call_id: String,
    record: Map<String, Value>,
    /// A call's row under a cap reservation: once its line is in, the row stays (`logging`)
    /// until stage 8 has settled the reservation from it.
    settles_later: bool,
}

/// A usage line written whose row is still there (`logging`).
pub(crate) struct Logged {
    id: String,
    call_id: String,
    settles_later: bool,
    db: Arc<StateDb>,
    seq: u64,
    /// The finished record, as the line has it.
    pub(crate) record: Map<String, Value>,
    /// `meter.after_usage_line` fired: as a crash right after the line, nothing more happens
    /// (no settle, the row stays for the next start).
    cut: bool,
}

/// The lines [`Meter::log_call`] wrote for one call.
#[derive(Default)]
pub(crate) struct CallLines {
    pub(crate) logged: Vec<Logged>,
    /// A fail point cut the settle short (as a crash would).
    pub(crate) cut: bool,
}

/// Every core run's own id: rows of another run were left by a dead core.
static RUNS: AtomicU64 = AtomicU64::new(0);

/// The meter (module docs).
pub struct Meter {
    states: Arc<StateRegistry>,
    env: Env,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    events: Arc<EventBus>,
    telemetry: Arc<Telemetry>,
    power: PowerSettings,
    failpoints: Arc<FailPoints>,
    run: String,
    open: Mutex<HashMap<String, Open>>,
    /// Lines a call's tool finished itself under a reservation, waiting for stage 8.
    awaiting: Mutex<HashMap<String, Vec<Logged>>>,
    /// Jobs whose handler recorded a draft of its own (they own their usage line).
    own: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for Meter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Meter")
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

impl Meter {
    #[allow(
        clippy::too_many_arguments,
        reason = "the core's shared parts, each from a different owner"
    )]
    pub fn new(
        states: Arc<StateRegistry>,
        env: Env,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        events: Arc<EventBus>,
        telemetry: Arc<Telemetry>,
        power: PowerSettings,
        failpoints: Arc<FailPoints>,
    ) -> Arc<Meter> {
        let started = clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let run = format!(
            "{}-{started}-{}",
            std::process::id(),
            RUNS.fetch_add(1, Ordering::Relaxed)
        );
        Arc::new(Meter {
            states,
            env,
            clock,
            ids,
            events,
            telemetry,
            power,
            failpoints,
            run,
            open: Mutex::new(HashMap::new()),
            awaiting: Mutex::new(HashMap::new()),
            own: Mutex::new(HashSet::new()),
        })
    }

    fn now_ms(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }

    fn db(&self, scope: Option<&Arc<OpenWorkspace>>) -> Arc<StateDb> {
        scope.map_or_else(|| self.states.suite_db(), |w| w.state.clone())
    }

    /// The write-ahead row of an external call, before its request is sent (CORE-071): its
    /// id, `us_<16 hex>`. `meter.after_pending` fires once the row is committed.
    ///
    /// # Errors
    /// `state.unavailable` when the row can't be written; the request must not be sent then.
    pub async fn begin(&self, ctx: &UsageCtx, d: UsageDraft) -> Result<String, ErrorBody> {
        let id = self.ids.id("us");
        let now = self.now_ms();
        let record = record::draft_record(&format_ts(now), ctx, &d);
        let row = PendingRow {
            id: id.clone(),
            run: self.run.clone(),
            call_id: ctx.call_id.clone(),
            job_id: ctx.job_id.clone(),
            reservation: ctx.reservation.clone(),
            record: record.clone(),
            logging: false,
            created_ms: now,
            logging_ms: None,
        };
        let db = self.db(ctx.workspace.as_ref());
        record::insert(&db, &row)
            .await
            .map_err(|e| state_error(&e))?;
        if let Err(e) = self.failpoints.hit("meter.after_pending") {
            // The step after the row failed: the request is not sent, so the row goes (an
            // `abort` there never returns, and leaves the row as a crash would).
            if let Err(gone) = record::delete(&db, &id).await {
                tracing::warn!(usage_id = %id, error = %gone, "an unsent call's usage row could not be removed; the next start writes it as pending");
            }
            return Err(ErrorBody::new(
                &codes::STORE_IO,
                format!("meter.after_pending: {e}"),
                "",
            ));
        }
        if let Some(job) = &ctx.job_id {
            lock(&self.own).insert(job.clone());
        }
        lock(&self.open).insert(
            id.clone(),
            Open {
                scope: ctx.workspace.clone(),
                call_id: ctx.call_id.clone(),
                record,
                settles_later: ctx.reservation.is_some() && ctx.job_id.is_none(),
            },
        );
        Ok(id)
    }

    /// Settles row `id` (begun by this core): its line, with the outcome, then the row goes.
    /// `cost_source` is written only with a cost. `meter.after_usage_line` fires after the
    /// line is appended, before the row is removed. Returns the line's `seq`.
    ///
    /// # Errors
    /// `params.invalid` for an id this core has no open row of; `state.unavailable`;
    /// the store's error when the line can't be appended (the row stays, so the next start
    /// writes it).
    pub async fn finish(
        &self,
        id: &str,
        cost: Option<(Money, Currency)>,
        cost_source: &str,
        status: &str,
        latency_ms: u64,
    ) -> Result<u64, ErrorBody> {
        self.finish_with(id, cost, cost_source, status, Some(latency_ms))
            .await
    }

    async fn finish_with(
        &self,
        id: &str,
        cost: Option<(Money, Currency)>,
        cost_source: &str,
        status: &str,
        latency_ms: Option<u64>,
    ) -> Result<u64, ErrorBody> {
        let logged = self
            .log_one(id, cost, cost_source, status, latency_ms)
            .await?;
        let seq = logged.seq;
        if logged.cut {
            return Ok(seq);
        }
        if logged.settles_later {
            // A tool that finished its own line under a reservation: stage 8 settles at this
            // line's cost, then removes the row; a crash first leaves the `logging` row, from
            // which the next start settles it (never releases it).
            lock(&self.awaiting)
                .entry(logged.call_id.clone())
                .or_default()
                .push(logged);
        } else {
            self.drop_row(&logged).await;
        }
        Ok(seq)
    }

    /// Row `id`'s line, with the outcome: the row becomes `logging`, the line is appended, and
    /// `meter.after_usage_line` fires. The row stays (see [`Logged::cut`]).
    async fn log_one(
        &self,
        id: &str,
        cost: Option<(Money, Currency)>,
        cost_source: &str,
        status: &str,
        latency_ms: Option<u64>,
    ) -> Result<Logged, ErrorBody> {
        let open = lock(&self.open)
            .remove(id)
            .ok_or_else(|| params_invalid(format!("{id} is not an open usage record")))?;
        let (call_id, settles_later) = (open.call_id.clone(), open.settles_later);
        let mut r = open.record;
        record::finish(&mut r, cost, cost_source, status, latency_ms);
        let db = self.db(open.scope.as_ref());
        record::mark_logging(&db, id, &r, self.now_ms())
            .await
            .map_err(|e| state_error(&e))?;
        let seq = self.append(open.scope.as_ref(), r.clone()).await?;
        let cut = match self.failpoints.hit("meter.after_usage_line") {
            Ok(()) => false,
            Err(e) => {
                tracing::warn!(usage_id = id, error = %e, "meter.after_usage_line: the row stays for the next start");
                true
            }
        };
        Ok(Logged {
            id: id.to_string(),
            call_id,
            settles_later,
            db,
            seq,
            record: r,
            cut,
        })
    }

    /// Removes a logged row: its line is in.
    async fn drop_row(&self, logged: &Logged) {
        if let Err(e) = record::delete(&logged.db, &logged.id).await {
            tracing::warn!(usage_id = %logged.id, error = %e, "a settled usage row could not be removed; the next start finds its line");
        }
    }

    /// The lines of call `call_id`'s still-pending rows, at their estimates with `status`
    /// (stage 8); the rows stay until [`Meter::drop_logged`].
    ///
    /// # Errors
    /// As [`Meter::finish`]; rows logged before the error stay for the next start.
    pub(crate) async fn log_call(
        &self,
        call_id: &str,
        status: &str,
    ) -> Result<CallLines, ErrorBody> {
        let mut ids: Vec<(String, Option<(Money, Currency)>)> = lock(&self.open)
            .iter()
            .filter(|(_, o)| o.call_id == call_id)
            .map(|(id, o)| (id.clone(), record::est_cost(&o.record)))
            .collect();
        ids.sort();
        let mut lines = CallLines {
            logged: lock(&self.awaiting).remove(call_id).unwrap_or_default(),
            cut: false,
        };
        for (id, est) in ids {
            let logged = self.log_one(&id, est, "estimated", status, None).await?;
            lines.cut |= logged.cut;
            lines.logged.push(logged);
        }
        Ok(lines)
    }

    /// Removes the rows of `lines`: their lines are in and their reservation is settled.
    pub(crate) async fn drop_logged(&self, lines: &CallLines) {
        for logged in &lines.logged {
            self.drop_row(logged).await;
        }
    }

    /// Finishes call `call_id`'s still-pending rows at their estimates (`cost_source:
    /// "estimated"`) with `status`: stage 8 for an inline call. `None` when it has none (the
    /// tool finished its own, or this is a second settle).
    ///
    /// # Errors
    /// As [`Meter::finish`].
    pub async fn finish_call(&self, call_id: &str, status: &str) -> Result<Option<u64>, ErrorBody> {
        let lines = self.log_call(call_id, status).await?;
        if !lines.cut {
            self.drop_logged(&lines).await;
        }
        Ok(lines.logged.last().map(|l| l.seq))
    }

    /// Drops row `id` (begun by this core) whose request was never sent: no line.
    pub async fn abandon(&self, id: &str) {
        let open = lock(&self.open).remove(id);
        if let Some(open) = open
            && let Err(e) = record::delete(&self.db(open.scope.as_ref()), id).await
        {
            tracing::warn!(usage_id = id, error = %e, "an abandoned usage row could not be removed; the next start writes it as pending");
        }
    }

    /// The suite scope's rows left by a dead core become lines (module docs, "Recovery"):
    /// how many lines were written.
    ///
    /// # Errors
    /// `state.unavailable`, or the store's error for a line that can't be written (the rest
    /// stay for the next start).
    pub async fn recover(&self) -> Result<u32, ErrorBody> {
        self.recover_scope(None).await
    }

    /// [`Meter::recover`] for workspace `open` (read-write only).
    ///
    /// # Errors
    /// As [`Meter::recover`].
    pub async fn recover_workspace(&self, open: &Arc<OpenWorkspace>) -> Result<u32, ErrorBody> {
        if open.access != Access::ReadWrite {
            return Ok(0);
        }
        self.recover_scope(Some(open)).await
    }

    async fn recover_scope(&self, scope: Option<&Arc<OpenWorkspace>>) -> Result<u32, ErrorBody> {
        let db = self.db(scope);
        let rows = record::left_by_others(&db, &self.run)
            .await
            .map_err(|e| state_error(&e))?;
        let mut written = 0;
        for row in rows {
            // A call's row stays until the caps' repair has decided its reservation, so a
            // reservation is never released after its line was written (ruling P3-R33).
            if row.job_id.is_none()
                && let Some(rv) = &row.reservation
                && caps::undecided(&self.states.suite_db(), rv).await
            {
                tracing::warn!(usage_id = %row.id, reservation = %rv, "a usage row of an earlier core run waits for its cap reservation's repair");
                continue;
            }
            if row.logging && self.logged(scope, &row).await {
                tracing::info!(usage_id = %row.id, "a usage line of an earlier core run was already in the log");
            } else {
                tracing::warn!(usage_id = %row.id, pending = !row.logging, "a usage record of an earlier core run is written");
                self.append(scope, row.record.clone()).await?;
                written += 1;
            }
            if let Err(e) = record::delete(&db, &row.id).await {
                tracing::warn!(usage_id = %row.id, error = %e, "a recovered usage row could not be removed");
            }
        }
        Ok(written)
    }

    /// Whether `row`'s finished record is a line of this machine in the log it was appended
    /// to (the month it went `logging`, or the next one when that is a minute away).
    async fn logged(&self, scope: Option<&Arc<OpenWorkspace>>, row: &PendingRow) -> bool {
        let want = Value::Object(row.record.clone());
        let secs = row.logging_ms.unwrap_or(row.created_ms).div_euclid(1000);
        let machine = self.machine(scope);
        self.log_texts(scope, secs).await.iter().any(|text| {
            lil_schema::logs::read_lines(text)
                .iter()
                .any(|l| l.machine == machine && l.record == want)
        })
    }

    /// [`UsageSink::record_outside`]: the line of paid outside prompt `prompt_id`, unless this
    /// machine's log has a line of `u.tool` for it already (`raw_usage.prompt_id`).
    async fn outside_line(
        &self,
        prompt_id: &str,
        u: UsageDraft,
        ctx: UsageCtx,
    ) -> Result<(), ErrorBody> {
        let scope = ctx.workspace.as_ref();
        let now = self.now_ms();
        let machine = self.machine(scope);
        let seen = self
            .log_texts(scope, now.div_euclid(1000))
            .await
            .iter()
            .any(|text| {
                lil_schema::logs::read_lines(text).iter().any(|l| {
                    l.machine == machine
                        && l.record.get("tool").and_then(Value::as_str) == Some(u.tool.as_str())
                        && l.record
                            .get("raw_usage")
                            .and_then(|r| r.get("prompt_id"))
                            .and_then(Value::as_str)
                            == Some(prompt_id)
                })
            });
        if seen {
            tracing::info!(
                prompt_id,
                "a paid outside prompt's usage line was already in the log"
            );
            return Ok(());
        }
        let mut r = record::draft_record(&format_ts(now), &ctx, &u);
        record::finish(&mut r, None, "", u.status.wire(), u.latency_ms);
        self.append(scope, r).await.map(|_| ())
    }

    /// The usage log texts of `scope` around `secs`: `usage-loose.jsonl`, or the workspace's
    /// logs of this machine for the month of `secs` and of a minute later.
    async fn log_texts(&self, scope: Option<&Arc<OpenWorkspace>>, secs: i64) -> Vec<String> {
        match scope {
            None => {
                let path = self.env.local_dir().join("usage-loose.jsonl");
                tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok())
                    .await
                    .ok()
                    .flatten()
                    .into_iter()
                    .collect()
            }
            Some(open) => {
                let mut texts = Vec::new();
                // The month it went `logging`, and the month a minute later: an append that
                // began just before a month's end may land in the next (its lock budget is
                // 10 s).
                for at in [secs, secs + 60] {
                    let month = lil_schema::time::year_month(at);
                    let rel = LogKind::Usage.rel(open.store.machine(), &month);
                    if let Ok(rel) = WsPath::parse(&rel)
                        && let Ok(Some(bytes)) = open.store.read(&rel).await
                    {
                        texts.push(String::from_utf8_lossy(&bytes).into_owned());
                    }
                }
                texts
            }
        }
    }

    fn machine(&self, scope: Option<&Arc<OpenWorkspace>>) -> String {
        scope.map_or_else(|| self.env.machine_id(), |w| w.store.machine().to_string())
    }

    /// Appends `record` to its scope's usage log and publishes it; its `seq`.
    async fn append(
        &self,
        scope: Option<&Arc<OpenWorkspace>>,
        record: Map<String, Value>,
    ) -> Result<u64, ErrorBody> {
        let seq = match scope {
            Some(open) => open
                .store
                .append_log(LogKind::Usage, record.clone())
                .await
                .map_err(|e| e.to_error(""))?,
            None => logs::append_loose_usage(&self.env, record.clone())
                .await
                .map_err(|e| e.to_error(""))?,
        };
        let mut line = Map::new();
        line.insert("machine".into(), json!(self.machine(scope)));
        line.insert("seq".into(), json!(seq));
        line.extend(record);
        self.events.publish(
            TOPIC,
            json!({"kind": "record", "record": Value::Object(line)}),
        );
        Ok(seq)
    }

    /// `usage.summary` over the loose usage log and, with one, `workspace`'s usage logs.
    ///
    /// # Errors
    /// `params.invalid` for a time that isn't RFC 3339 UTC or a range backwards.
    pub async fn summary(
        &self,
        q: SummaryQuery,
        workspace: Option<&Arc<OpenWorkspace>>,
    ) -> Result<Value, ErrorBody> {
        let now = self.now_ms();
        let clock = self.clock.clone();
        let offset = move |ms: i64| {
            let at = UNIX_EPOCH
                + std::time::Duration::from_millis(u64::try_from(ms.max(0)).unwrap_or(0));
            clock.local_offset_secs(at)
        };
        let time = |what: &str, t: &Option<String>| match t {
            None => Ok(None),
            Some(t) => parse_ts(t).map(Some).ok_or_else(|| {
                params_invalid(format!(
                    "{what} {t:?} is not an RFC 3339 UTC time like 2026-10-18T12:00:00Z"
                ))
            }),
        };
        let to_ms = time("to", &q.to)?.unwrap_or(now);
        let from_ms = match time("from", &q.from)? {
            Some(ms) => ms,
            None => period_start(to_ms.min(now), q.period, &offset),
        };
        if from_ms > to_ms {
            return Err(params_invalid("from is after to"));
        }
        let mut lines = self.loose_lines().await;
        if let Some(open) = workspace {
            lines.extend(workspace_lines(open, from_ms, to_ms).await);
        }
        let settings = self.money_settings().await;
        let range = Range {
            from_ms,
            to_ms,
            period: q.period,
            offset_secs: &offset,
        };
        Ok(summary::summarize(
            &lines,
            &range,
            &settings,
            q.group_by.as_deref().unwrap_or(&[]),
        ))
    }

    /// `suite.toml`'s `[money]` and `[[fixed_costs]]`, read now (a file that can't be read or
    /// parsed gives the defaults, logged).
    pub async fn money_settings(&self) -> MoneySettings {
        let path = self.env.suite_toml_to_read();
        let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
            .await
            .ok();
        match text {
            Some(Ok(text)) => match SuiteToml::parse(&text) {
                Ok(suite) => MoneySettings::from_suite_toml(&suite),
                Err(e) => {
                    tracing::warn!(error = %e, "suite.toml doesn't parse; [money] has its defaults");
                    MoneySettings::default()
                }
            },
            _ => MoneySettings::default(),
        }
    }

    async fn loose_lines(&self) -> Vec<Map<String, Value>> {
        let path = self.env.local_dir().join("usage-loose.jsonl");
        let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok())
            .await
            .ok()
            .flatten();
        text.map(|t| records(&t)).unwrap_or_default()
    }

    /// The local-work line of a job (module docs, "Jobs").
    async fn local_line(&self, job: &JobCtx, outcome: &Outcome<ToolOutput>, e: Option<&JobEnergy>) {
        let row = crate::jobs::record::get(&job.state, job.id.as_str())
            .await
            .ok()
            .flatten();
        let tool = row
            .as_ref()
            .and_then(|r| r.origin.as_ref())
            .map_or_else(|| job.spec.kind.to_string(), |o| o.tool.clone());
        let caller_kind = row
            .as_ref()
            .and_then(|r| r.idem_scope.as_deref())
            .and_then(|s| s.split_once('|'))
            .map(|(kind, _)| kind.to_string());
        let mut units = Map::new();
        let mut energy_cost = None;
        if let Some(e) = e {
            units.insert(
                "gpu_seconds".into(),
                Value::String(
                    Money::from_micros(
                        i64::try_from(e.gpu_seconds_milli.saturating_mul(1000)).unwrap_or(i64::MAX),
                    )
                    .to_decimal_string(),
                ),
            );
            let psu = u64::from(self.power.psu_efficiency_permille.clamp(1, 1000));
            let above = attribution::share(e.gpu_mj_above_idle + e.cpu_mj_above_idle, 1000, psu);
            units.insert("energy_wh_gross".into(), wh(e.system_mj));
            units.insert("energy_wh_above_idle".into(), wh(above));
            if e.idle_unknown {
                units.insert("idle_unknown".into(), Value::Bool(true));
            }
            if let Some((rate, currency)) = &self.power.electricity {
                // A kWh is 3.6e9 mJ.
                let rate = u64::try_from(rate.micros()).unwrap_or(0);
                let micros = attribution::share(rate, e.system_mj, 3_600_000_000);
                energy_cost = Some((
                    Money::from_micros(i64::try_from(micros).unwrap_or(i64::MAX)),
                    currency.clone(),
                ));
            }
        }
        let draft = UsageDraft {
            tool,
            provider: LOCAL_PROVIDER.to_string(),
            model: None,
            route: job
                .spec
                .engine
                .map_or_else(|| job.spec.kind.to_string(), |k| k.wire().to_string()),
            basis: BillingBasis::Local,
            units,
            est_cost: None,
            account: None,
            raw_usage: None,
            attribution: Attribution::Suite,
            status: UsageStatus::Pending,
            latency_ms: None,
        };
        let ctx = UsageCtx {
            workspace: job.workspace.clone(),
            project: job.spec.project.clone(),
            asset: job.spec.asset_id.clone(),
            job_id: Some(job.id.to_string()),
            call_id: job.id.to_string(),
            caller_kind: caller_kind.clone().unwrap_or_default(),
            reservation: job.reservation.clone(),
        };
        let mut r = record::draft_record(&format_ts(self.now_ms()), &ctx, &draft);
        if caller_kind.is_none() {
            r.insert("caller_kind".into(), Value::Null);
        }
        record::finish(&mut r, None, "", outcome_status(outcome).wire(), None);
        r.insert("cost".into(), json!("0"));
        if let Some((m, c)) = energy_cost {
            r.insert("energy_cost".into(), record::money_value(m));
            r.insert("currency".into(), json!(c.as_str()));
        }
        // A `logging` row first, so a core that dies before the line is in writes it at the
        // next start (the record is already final; no request is made).
        let db = self.db(job.workspace.as_ref());
        let now = self.now_ms();
        let row = PendingRow {
            id: self.ids.id("us"),
            run: self.run.clone(),
            call_id: job.id.to_string(),
            job_id: Some(job.id.to_string()),
            reservation: job.reservation.clone(),
            record: r.clone(),
            logging: true,
            created_ms: now,
            logging_ms: Some(now),
        };
        let kept = match record::insert(&db, &row).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(job_id = %job.id, error = %e, "a job's local-work row could not be written; its line is written without one");
                false
            }
        };
        match self.append(job.workspace.as_ref(), r).await {
            Ok(_) => {
                if kept && let Err(e) = record::delete(&db, &row.id).await {
                    tracing::warn!(usage_id = %row.id, error = %e, "a written local-work row could not be removed; the next start finds its line");
                }
            }
            Err(err) => {
                tracing::error!(job_id = %job.id, code = %err.code, error = %err.message, "a job's local-work usage line could not be written; the next start writes it");
            }
        }
    }

    /// The history row of a job whose final state `succeeded` is in, when its row has an
    /// estimate key and its run wasn't all cached (module docs, "Jobs"): its energy is the
    /// `JobEnergy` its completion stored. A row that can't be read or written is logged; the
    /// job is unchanged.
    async fn job_stats(&self, job: &JobCtx) {
        let row = match crate::jobs::record::get(&job.state, job.id.as_str()).await {
            Ok(row) => row,
            Err(err) => {
                tracing::warn!(job_id = %job.id, error = %err, "a job's row can't be read; it adds no history");
                None
            }
        };
        let Some(row) = row.filter(|r| r.status == JobStatus::Succeeded && !r.report.all_cached)
        else {
            return;
        };
        let Some(estimate_key) = row.estimate_key else {
            return;
        };
        let energy = self
            .telemetry
            .job_energy(JobEnergyQuery {
                job_ids: Some(vec![job.id.to_string()]),
                ..JobEnergyQuery::default()
            })
            .await
            .map_err(|err| tracing::warn!(job_id = %job.id, error = %err.message, "a job's energy can't be read for its history"))
            .ok()
            .and_then(|mut rows| rows.pop());
        let e = energy.as_ref();
        let windows = job.windows();
        let wall_ms = windows
            .iter()
            .map(|w| u64::try_from(w.end.saturating_sub(w.start).as_millis()).unwrap_or(u64::MAX))
            .fold(0_u64, u64::saturating_add);
        let row = JobStatsRow {
            job_id: job.id.to_string(),
            kind: job.spec.kind.to_string(),
            estimate_key,
            finished_ms: self.now_ms(),
            wall_ms,
            gpu_seconds_milli: e.map_or(0, |e| e.gpu_seconds_milli),
            system_mj: e.map_or(0, |e| e.system_mj),
        };
        if let Err(err) = self.telemetry.put_job_stats(&row).await {
            tracing::warn!(job_id = %job.id, error = %err.message, "a job's history row could not be stored");
        }
    }

    async fn job_completed(&self, job: &JobCtx, outcome: &Outcome<ToolOutput>) {
        let owns_its_line = lock(&self.own).remove(job.id.as_str());
        if never_started(outcome) {
            return;
        }
        let windows = job.windows();
        let mut energy = None;
        if !windows.is_empty() {
            let mut slices = Vec::with_capacity(windows.len());
            for w in &windows {
                slices.push(self.telemetry.energy_between(w.start, w.end).await);
            }
            let workspace = job
                .workspace
                .as_ref()
                .map_or_else(|| record::LOOSE.to_string(), |w| w.ws.id.clone());
            let mut e = attribute(job.id.as_str(), &workspace, &windows, &slices, &self.power);
            // `from` and `to` are the times of the windows' first and last samples, so a series
            // over them holds exactly the samples the energy came from; a window without a
            // sample counts with its own ends, mapped to wall times.
            let (mut from, mut to) = (i64::MAX, i64::MIN);
            for w in &windows {
                let (first, last) = match self.telemetry.sample_span(w.start, w.end).await {
                    Some(span) => span,
                    None => (
                        self.telemetry.wall_ms(w.start),
                        self.telemetry.wall_ms(w.end),
                    ),
                };
                (from, to) = (from.min(first), to.max(last));
            }
            e.from = format_ts(from);
            e.to = format_ts(to);
            if let Err(err) = self.telemetry.put_job_energy(&e).await {
                tracing::warn!(job_id = %job.id, error = %err.message, "a job's energy could not be stored");
            }
            energy = Some(e);
        }
        if owns_its_line {
            return;
        }
        let resolved_engine_job = resolved_unknown(outcome) && job.spec.engine.is_some();
        if windows.is_empty() && !resolved_engine_job {
            return;
        }
        self.local_line(job, outcome, energy.as_ref()).await;
    }
}

/// The start (Unix ms) of the local day or month that holds `ms` (CORE-012), daylight saving
/// time included.
fn period_start(ms: i64, period: Period, offset: &dyn Fn(i64) -> i32) -> i64 {
    let off = i64::from(offset(ms));
    let local = ms.div_euclid(1000) + off;
    let day_start = local - local.rem_euclid(86_400);
    let start = match period {
        Period::Day => day_start,
        Period::Month => {
            let date = lil_schema::time::date(local);
            let day: i64 = date[8..].parse().unwrap_or(1);
            day_start - (day - 1) * 86_400
        }
    };
    // The local start in UTC with the offset that holds at the start itself, not the one of
    // `ms`: a month or day that began before a daylight-saving change began an hour apart.
    let at_start = i64::from(offset((start - off) * 1000));
    (start - at_start) * 1000
}

/// The records of a usage log's text (`machine` and `seq` removed).
fn records(text: &str) -> Vec<Map<String, Value>> {
    lil_schema::logs::read_lines(text)
        .into_iter()
        .filter_map(|l| match l.record {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .collect()
}

/// The usage lines of `open`'s `registry/usage.<machine>.<yyyy-mm>.jsonl` logs of every machine,
/// for the UTC months a range of `from_ms` to `to_ms` touches (a line is in the log of the
/// month it was appended, at most a minute after its `ts`).
async fn workspace_lines(
    open: &Arc<OpenWorkspace>,
    from_ms: i64,
    to_ms: i64,
) -> Vec<Map<String, Value>> {
    let lo = lil_schema::time::year_month(from_ms.div_euclid(1000));
    let hi = lil_schema::time::year_month(to_ms.div_euclid(1000).saturating_add(60));
    let in_range = move |name: &str| {
        name.strip_suffix(".jsonl")
            .and_then(|n| n.rsplit_once('.'))
            .is_some_and(|(_, month)| month >= lo.as_str() && month <= hi.as_str())
    };
    let dir = open.store.root().join("registry");
    let names = tokio::task::spawn_blocking(move || {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("usage.") && in_range(n))
            .collect();
        names.sort();
        names
    })
    .await
    .unwrap_or_default();
    let mut out = Vec::new();
    for name in names {
        let Ok(rel) = WsPath::parse(&format!("registry/{name}")) else {
            continue;
        };
        match open.store.read(&rel).await {
            Ok(Some(bytes)) => out.extend(records(&String::from_utf8_lossy(&bytes))),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(log = %rel, error = %e, "a usage log can't be read; the summary leaves it out")
            }
        }
    }
    out
}

impl UsageSink for Meter {
    /// A call that is over: its row and its line at once, with the draft's status and latency
    /// (a `pending` status stays `pending`: an outside prompt whose end was not seen).
    fn record<'a>(&'a self, u: UsageDraft, ctx: UsageCtx) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let (status, latency) = (u.status, u.latency_ms);
            let done = match self.begin(&ctx, u).await {
                Ok(id) => {
                    self.finish_with(&id, None, "", status.wire(), latency)
                        .await
                }
                Err(e) => Err(e),
            };
            if let Err(e) = done {
                tracing::error!(call_id = %ctx.call_id, code = %e.code, error = %e.message, "a usage record could not be written");
            }
        })
    }

    /// The row, or the error the call fails with (the request must not be sent).
    fn begin<'a>(
        &'a self,
        u: UsageDraft,
        ctx: UsageCtx,
    ) -> BoxFuture<'a, Result<PendingUsage, ErrorBody>> {
        Box::pin(async move {
            match Meter::begin(self, &ctx, u.clone()).await {
                Ok(id) => Ok(PendingUsage {
                    id: Some(id),
                    draft: u,
                    ctx,
                }),
                Err(e) => {
                    tracing::error!(call_id = %ctx.call_id, code = %e.code, error = %e.message, "a write-ahead usage row could not be written; the call is not sent");
                    Err(e)
                }
            }
        })
    }

    fn settle<'a>(
        &'a self,
        p: PendingUsage,
        status: UsageStatus,
        latency_ms: Option<u64>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            match &p.id {
                Some(id) => {
                    if let Err(e) = self
                        .finish_with(id, None, "", status.wire(), latency_ms)
                        .await
                    {
                        tracing::error!(usage_id = %id, code = %e.code, error = %e.message, "a usage record could not be settled");
                    }
                }
                None => {
                    let mut draft = p.draft;
                    draft.status = status;
                    draft.latency_ms = latency_ms;
                    UsageSink::record(self, draft, p.ctx).await;
                }
            }
        })
    }

    fn abandon<'a>(&'a self, p: PendingUsage) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(id) = &p.id {
                Meter::abandon(self, id).await;
            }
        })
    }

    fn record_outside<'a>(
        &'a self,
        prompt_id: &'a str,
        u: UsageDraft,
        ctx: UsageCtx,
    ) -> BoxFuture<'a, Result<(), ErrorBody>> {
        Box::pin(self.outside_line(prompt_id, u, ctx))
    }
}

impl JobCompletion for Meter {
    fn completed<'a>(
        &'a self,
        job: &'a JobCtx,
        outcome: &'a Outcome<ToolOutput>,
        _done: Option<&'a JobDone>,
    ) -> JobFuture<'a, ()> {
        Box::pin(self.job_completed(job, outcome))
    }

    fn succeeded<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(self.job_stats(job))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-012
    fn periods_start_at_local_midnight_and_the_first_of_the_month() {
        // 2026-10-19T05:30:00Z.
        let ms = 1_792_387_800_000;
        let utc = |_: i64| 0;
        assert_eq!(
            format_ts(period_start(ms, Period::Day, &utc)),
            "2026-10-19T00:00:00.000Z"
        );
        assert_eq!(
            format_ts(period_start(ms, Period::Month, &utc)),
            "2026-10-01T00:00:00.000Z"
        );
        // Seven hours behind UTC, it is still 18 October locally.
        let behind = |_: i64| -7 * 3600;
        assert_eq!(
            format_ts(period_start(ms, Period::Day, &behind)),
            "2026-10-18T07:00:00.000Z"
        );
    }

    #[test] // spec: CORE-012
    fn a_month_that_began_before_a_dst_change_starts_at_its_own_midnight() {
        // US Pacific: PDT (-7 h) until 2026-11-01T09:00Z, then PST (-8 h).
        let change = 1_793_523_600_000; // 2026-11-01T09:00:00Z
        assert_eq!(format_ts(change), "2026-11-01T09:00:00.000Z");
        let pacific = move |at: i64| if at < change { -7 * 3600 } else { -8 * 3600 };
        let nov_5 = 1_793_880_000_000; // 2026-11-05T12:00:00Z
        assert_eq!(format_ts(nov_5), "2026-11-05T12:00:00.000Z");
        assert_eq!(
            format_ts(period_start(nov_5, Period::Month, &pacific)),
            "2026-11-01T07:00:00.000Z",
            "local midnight of 1 November, in PDT"
        );
        assert_eq!(
            format_ts(period_start(nov_5, Period::Day, &pacific)),
            "2026-11-05T08:00:00.000Z"
        );
    }

    #[test] // spec: CORE-069
    fn energy_is_written_in_watt_hours() {
        assert_eq!(wh(3_600_000), json!("1"));
        // Six decimals of a Wh are 3.6 mJ: 1 mJ rounds to 0, 10 mJ to 0.000003.
        assert_eq!(wh(3_600), json!("0.001"));
        assert_eq!(wh(10), json!("0.000003"));
        assert_eq!(wh(1), json!("0"));
        assert_eq!(wh(0), json!("0"));
    }
}
