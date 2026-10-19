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
//!   the row's removal never gives two lines. Reservations are Caps' (Task 16): the meter
//!   neither keeps nor releases one.
//! - **Inline calls** (stage 8, [`stage::MeterSettle`]). A tool that leaves its row pending
//!   gets it finished by [`Meter::finish_call`] at its estimate, with the call's status.
//! - **Jobs** ([`JobCompletion`]). A job that reported execution windows gets its energy
//!   attributed ([`attribution::attribute`]) and written with `Telemetry::put_job_energy`, and
//!   one local-work line (`billing_basis: "local"`, `cost: "0"`, its energy, and its energy
//!   cost at `[power] electricity` when set). A job whose handler recorded its own draft
//!   through [`UsageSink`] (the node install) owns its usage line, so it gets none from here
//!   (QA1 I5). An `interrupted_unknown` engine job you resolved gets a `cancelled` line even
//!   though its windows went with the run that left it unknown. A job that never started gets
//!   neither. The completion never settles a reservation: only Caps' does (Task 16).
//! - **Summaries** ([`summary`], `usage.summary`).

pub mod attribution;
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
pub use sink::{Attribution, PendingUsage, UsageCtx, UsageDraft, UsageSink, UsageStatus};
pub use stage::MeterSettle;
pub use summary::{MoneySettings, SummaryQuery};
pub use tools::register_meter_tools;

use self::record::PendingRow;
use self::summary::{Period, Range};
use crate::events::EventBus;
use crate::jobs::{JobCompletion, JobCtx, JobDone, JobFuture, never_started, resolved_unknown};
use crate::state::{StateDb, StateError, StateRegistry};
use crate::store::{FailPoints, LogKind, WsPath, logs};
use crate::telemetry::{JobEnergy, PowerSettings, Telemetry, format_ts, parse_ts};
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
            // As a crash there: the row stays for the next start.
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
        let open = lock(&self.open)
            .remove(id)
            .ok_or_else(|| params_invalid(format!("{id} is not an open usage record")))?;
        let mut r = open.record;
        record::finish(&mut r, cost, cost_source, status, latency_ms);
        let db = self.db(open.scope.as_ref());
        record::mark_logging(&db, id, &r, self.now_ms())
            .await
            .map_err(|e| state_error(&e))?;
        let seq = self.append(open.scope.as_ref(), r).await?;
        if let Err(e) = self.failpoints.hit("meter.after_usage_line") {
            tracing::warn!(usage_id = id, error = %e, "meter.after_usage_line: the row stays for the next start");
            return Ok(seq);
        }
        if let Err(e) = record::delete(&db, id).await {
            tracing::warn!(usage_id = id, error = %e, "a settled usage row could not be removed; the next start finds its line");
        }
        Ok(seq)
    }

    /// Finishes call `call_id`'s still-pending rows at their estimates (`cost_source:
    /// "estimated"`) with `status`: stage 8 for an inline call. `None` when it has none (the
    /// tool finished its own, or this is a second settle).
    ///
    /// # Errors
    /// As [`Meter::finish`].
    pub async fn finish_call(&self, call_id: &str, status: &str) -> Result<Option<u64>, ErrorBody> {
        let mut ids: Vec<(String, Option<(Money, Currency)>)> = lock(&self.open)
            .iter()
            .filter(|(_, o)| o.call_id == call_id)
            .map(|(id, o)| (id.clone(), record::est_cost(&o.record)))
            .collect();
        ids.sort();
        let mut last = None;
        for (id, est) in ids {
            last = Some(
                self.finish_with(&id, est, "estimated", status, None)
                    .await?,
            );
        }
        Ok(last)
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
    /// to (the month it went `logging`, or the next one).
    async fn logged(&self, scope: Option<&Arc<OpenWorkspace>>, row: &PendingRow) -> bool {
        let want = Value::Object(row.record.clone());
        let secs = row.logging_ms.unwrap_or(row.created_ms).div_euclid(1000);
        let texts: Vec<String> = match scope {
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
                for at in [secs, secs + 32 * 86_400] {
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
        };
        let machine = self.machine(scope);
        texts.iter().any(|text| {
            lil_schema::logs::read_lines(text)
                .iter()
                .any(|l| l.machine == machine && l.record == want)
        })
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
            lines.extend(workspace_lines(open).await);
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
        if let Err(err) = self.append(job.workspace.as_ref(), r).await {
            tracing::error!(job_id = %job.id, code = %err.code, error = %err.message, "a job's local-work usage line could not be written");
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
            let start = windows.iter().map(|w| w.start).min().unwrap_or_default();
            let end = windows.iter().map(|w| w.end).max().unwrap_or_default();
            e.from = format_ts(self.telemetry.wall_ms(start));
            e.to = format_ts(self.telemetry.wall_ms(end));
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

/// The start (Unix ms) of the local day or month that holds `ms`.
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
    (start - off) * 1000
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

/// Every usage line of `open`'s `registry/usage.*.jsonl` logs, of every machine.
async fn workspace_lines(open: &Arc<OpenWorkspace>) -> Vec<Map<String, Value>> {
    let dir = open.store.root().join("registry");
    let names = tokio::task::spawn_blocking(move || {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("usage.") && n.ends_with(".jsonl"))
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

    fn begin<'a>(&'a self, u: UsageDraft, ctx: UsageCtx) -> BoxFuture<'a, PendingUsage> {
        Box::pin(async move {
            let id = match Meter::begin(self, &ctx, u.clone()).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::error!(call_id = %ctx.call_id, code = %e.code, error = %e.message, "a write-ahead usage row could not be written");
                    None
                }
            };
            PendingUsage { id, draft: u, ctx }
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
