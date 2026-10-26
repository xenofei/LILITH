//! Caps (ARCH-056, ARCH-099, CORE-008, CORE-012, CORE-085–092, QA-047; Appendix F "Caps and
//! money in `suite.toml`"): reserve-then-settle in the suite `state.db`, local-calendar periods,
//! alerts, and the only way the caps change.
//!
//! - **What counts.** Only `payg`, `prepaid_credits` and `byok` money (CORE-090): an
//!   [`Estimate::External`] of another basis, local work and fixed costs never reserve.
//! - **Reserve** ([`Caps::reserve`], stage 6). In one `BEGIN IMMEDIATE` of the suite
//!   `state.db`, which holds every reservation of any workspace or none (CORE-087): the caps
//!   the call would pass ("settled + held + this estimate > cap") are found, and with none (or
//!   only caps a confirmation's `allow_once` named, at no more than the estimate it showed) the
//!   reservation is written, so two calls can never take the same room and nothing passes a cap
//!   you weren't shown. It records the call (its owner `c_…`), the workspace (or `none`), the estimate
//!   in its own currency, the exchange rate used, the amount in the display currency and the
//!   local month. `caps.after_reserve` fires once it is committed. Caps are optional: with none
//!   set, a reservation is still made (in its own currency when there is no rate), so a cap set
//!   later in the month sees the month's spend. A negative estimate is refused.
//! - **Currencies.** Totals are kept per currency; a part in another currency than the display
//!   one (a former display currency, or a reservation made without a rate) is converted at its
//!   newest rate dated on or before today, and without one the cap counts as exceeded.
//! - **Who owns it.** The call, until a job takes it over ([`Caps::hand_over`] to `j_…`, or to
//!   an automatic re-run's id). A call whose idempotency key gave back an existing job releases
//!   its own ([`Caps::release`]).
//! - **Settle** ([`Caps::settle`]): idempotent by reservation id. Billed, it adds the actual
//!   cost (else the reserved amount) to the month's totals (`monthly` and `provider:<id>`);
//!   unbilled, it is released and counts for nothing. An inline call is settled by stage 8
//!   after its usage line ([`super::MeterSettle`]); a job by its completion (success settles;
//!   a cancel, a job that never started and a failure release, since no plan-3 provider bills
//!   failures; an `interrupted_unknown` job you resolved settles at the reserved amount, ruling
//!   P3-R15). An `interrupted_unknown` job has no completion until then: its reservation stays
//!   held, counted at the reserved amount.
//! - **Repair** ([`Caps::repair`] at start for calls without a workspace, and
//!   [`Caps::repair_workspace`] when a workspace opens, each before the meter's recovery of the
//!   same scope). A held reservation of a dead core run's *call* is decided by the call's
//!   write-ahead usage rows (CORE-071, CORE-088): a `logging` row means its line was written,
//!   so it is settled as the line says (a crash between the line and the settle); a `pending`
//!   row means the request may have been sent, so it stays held (marked `kept`, so a later
//!   start leaves it too); no row means nothing was sent, so it is released. A dead run's *job*
//!   reservation ([`Caps::repair_jobs`], once the scope's jobs are recovered) is decided by its
//!   job: succeeded settles, gone, cancelled or failed releases, `interrupted` with external
//!   side effects stays held (`kept`; it may have been charged), and an unfinished
//!   (`interrupted_unknown`) one waits for you. A tool that finishes its own usage line under a
//!   reservation keeps the row until stage 8 settles at that line's cost, so a crash before it
//!   settles from the line, never releasing a spent reservation.
//! - **Repair fails closed** (ruling P3-R33). The repair of a scope goes on past a reservation
//!   it can't decide (logged; that one stays held) and then fails, and a scope whose repair
//!   failed skips the meter's recovery ([`Caps::repair_then_recover`],
//!   [`Caps::repair_then_recover_workspace`]), so its rows are still there at the next start.
//!   The meter's recovery never removes a call's row while its reservation is held and not
//!   `kept`, so "no row" always means no line was written. A call's reservation that a job
//!   holds (`jobs.reservation`: its hand-over failed) is left to the job repair, which decides
//!   it by that job.
//! - **Periods** are local calendar months ([`Caps::period`], CORE-012). A reservation counts
//!   in the month it was made: held ones of earlier months don't count against this month.
//! - **Alerts** at 50 %, 80 % and 100 % of the settled amount, once per level, cap and month
//!   (CORE-089), on the `caps` topic as `{kind: "alert", cap, threshold_pct, spent_pct, spent,
//!   currency, period}` (`spent_pct`: the settled total as a whole percentage of the cap,
//!   rounded down, `null` for a cap of 0; `spent`: that total in the display currency
//!   `currency`; Checkpoint A walkthrough and its review); a change
//!   is `{kind: "changed", caps: CapsView}` and invalid caps `{kind: "invalid", caps: [names]}`
//!   (empty once a hand edit made them valid again; caps are read at each use).
//! - **Who changes them** ([`Caps::set`]): only a verified `ui:*` session with `caps.v1`
//!   (`app/caps.set` or a cap confirmation's `raise`). No tool writes them (CORE-008).
//! - **Fail closed.** A cap that doesn't parse counts as exceeded; a call whose estimate can't
//!   be converted to the display currency while a cap applies is `caps.invalid`; a `state.db`
//!   failure is `state.unavailable`.
//! - **Each machine** enforces its own (CORE-092): the totals are this machine's.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use lil_platform::{Clock, Env, IdSource};
use lil_protocol::app::{CAPS_CAPABILITY, CapRow, CapsSetParams, CapsView};
use lil_protocol::{CallerKind, ErrorBody, NO_WORKSPACE, Outcome, ToolOutput, codes};
use lil_schema::money::{Currency, Money, Rate};
use lil_schema::suite_edit::CapsEdit;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use serde_json::{Map, Value, json};

use super::caps_settings::{CapsSettings, MONTHLY, PER_CALL_MAX, provider_cap};
use super::record;
use crate::events::EventBus;
use crate::jobs::{
    JobCompletion, JobCtx, JobDone, JobFuture, JobId, JobStatus, Reservations, Substatus,
    is_cancelled, never_started, resolved_unknown,
};
use crate::session::Session;
use crate::state::{StateDb, StateError, StateRegistry};
use crate::store::FailPoints;
use crate::tools::stages::{
    AllowOnce, BillingBasis, CapHit, Estimate, ReserveInput, ReserveOutcome, ReserveStage,
};
use crate::workspace::{Access, OpenWorkspace};

/// The topic of the caps' events.
pub const TOPIC: &str = "caps";
/// The fail point after a reservation is committed.
pub const AFTER_RESERVE: &str = "caps.after_reserve";
/// The alert levels, in percent of a cap (CORE-089).
pub const ALERT_LEVELS: [u32; 3] = [50, 80, 100];

/// One micro-unit per micro-unit: the rate of the display currency itself.
const ONE: i64 = 1_000_000;

/// What the caps say about an estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapCheck {
    Within,
    Exceeds(Vec<CapHit>),
}

/// A reservation's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationState {
    Held,
    Settled,
    Released,
}

impl ReservationState {
    pub fn wire(self) -> &'static str {
        match self {
            ReservationState::Held => "held",
            ReservationState::Settled => "settled",
            ReservationState::Released => "released",
        }
    }

    fn parse(s: &str) -> ReservationState {
        match s {
            "settled" => ReservationState::Settled,
            "released" => ReservationState::Released,
            _ => ReservationState::Held,
        }
    }
}

/// One reservation, as `cap_reservations` keeps it (amounts in micro-units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    /// `rv_<16 hex>`.
    pub id: String,
    /// The core run that made it.
    pub run: String,
    /// The call `c_…` it was made for, or the job `j_…` that took it over.
    pub owner: String,
    /// The workspace id, or `none`.
    pub workspace: String,
    pub provider: String,
    pub basis: String,
    /// The local month, `YYYY-MM`.
    pub period: String,
    /// The estimate, in its own currency.
    pub currency: String,
    pub amount: Money,
    /// 1 unit of `currency` is `rate_micros` display micro-units.
    pub rate_micros: i64,
    /// The date of the rate used; `None` for the display currency itself.
    pub rate_date: Option<String>,
    pub display_currency: String,
    /// The reserved amount in the display currency.
    pub display: Money,
    /// The confirmation that let it pass a cap once.
    pub allowed_by: Option<String>,
    pub state: ReservationState,
    /// What it settled at, in the display currency.
    pub settled: Option<Money>,
    /// It stays held: its call left a pending usage line at a crash.
    pub kept: bool,
}

/// Whether reservation `id` in the suite `state.db` `suite` is still held and not `kept`: the
/// caps' repair hasn't decided it, so the meter's recovery leaves its call's rows (ruling
/// P3-R33). An error counts as undecided (fail closed).
pub(crate) async fn undecided(suite: &StateDb, id: &str) -> bool {
    let id = id.to_string();
    let row = suite
        .call(move |conn| {
            conn.query_row(
                "SELECT state, kept FROM cap_reservations WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|e| sql(&e))
        })
        .await;
    match row {
        Ok(Some((state, kept))) => state == "held" && kept == 0,
        Ok(None) => false,
        Err(_) => true,
    }
}

/// What a scope's start did: the caps' repair, then the meter's recovery.
#[derive(Debug)]
pub struct ScopeStart {
    pub repaired: Result<u32, ErrorBody>,
    pub recovered: Option<Result<u32, ErrorBody>>,
}

/// Who a reservation is for: its owner (the call id `c_…`) and workspace (an id or `none`).
#[derive(Debug, Clone, Copy)]
pub struct ReserveFor<'a> {
    pub owner: &'a str,
    pub workspace: &'a str,
}

/// The part of an estimate that counts against money caps.
struct Counted<'a> {
    provider: &'a str,
    amount: Money,
    currency: &'a Currency,
    basis: BillingBasis,
}

fn counted(est: &Estimate) -> Option<Counted<'_>> {
    match est {
        Estimate::External {
            provider,
            amount,
            currency,
            basis: basis @ (BillingBasis::Payg | BillingBasis::PrepaidCredits | BillingBasis::Byok),
        } => Some(Counted {
            provider,
            amount: *amount,
            currency,
            basis: *basis,
        }),
        _ => None,
    }
}

/// An estimate in the display currency, with the rate used.
#[derive(Debug, Clone)]
struct Priced {
    display: Money,
    rate_micros: i64,
    rate_date: Option<String>,
}

impl Priced {
    /// The rate used, `None` for the display currency itself.
    fn rate(&self) -> Option<Rate> {
        self.rate_date.as_ref().map(|d| Rate {
            in_display_micros: self.rate_micros,
            date: d.clone(),
        })
    }
}

fn sql(e: &rusqlite::Error) -> StateError {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            StateError::Corrupt(e.to_string())
        }
        _ => StateError::Io(e.to_string()),
    }
}

fn state_unavailable(e: &StateError) -> ErrorBody {
    tracing::error!(error = %e, "the caps' state.db failed; the call is refused (caps fail closed)");
    ErrorBody::new(
        &codes::STATE_UNAVAILABLE,
        "The core's state database is not available, so the caps can't be checked; retry with the same idempotency key.",
        "",
    )
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every reservation column, in [`read_row`]'s order.
const COLUMNS: &str = "id, run, owner, workspace, provider, basis, period, currency, amount_micros,
    rate_micros, rate_date, display_currency, display_micros, allowed_by, state, settled_micros,
    kept";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Reservation> {
    let state: String = r.get(14)?;
    Ok(Reservation {
        id: r.get(0)?,
        run: r.get(1)?,
        owner: r.get(2)?,
        workspace: r.get(3)?,
        provider: r.get(4)?,
        basis: r.get(5)?,
        period: r.get(6)?,
        currency: r.get(7)?,
        amount: Money::from_micros(r.get(8)?),
        rate_micros: r.get(9)?,
        rate_date: r.get(10)?,
        display_currency: r.get(11)?,
        display: Money::from_micros(r.get(12)?),
        allowed_by: r.get(13)?,
        state: ReservationState::parse(&state),
        settled: r.get::<_, Option<i64>>(15)?.map(Money::from_micros),
        kept: r.get::<_, i64>(16)? != 0,
    })
}

/// An amount in the display currency, and whether every part of it could be converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Total {
    amount: Money,
    /// `false`: a part in another currency has no rate, so the cap counts as exceeded.
    complete: bool,
}

/// `(currency, micros)` parts as one amount in `s`'s display currency (module docs,
/// "Currencies"): a part in another currency (a former display currency, or a reservation made
/// with no rate) is converted at its newest rate dated on or before the local `date`.
fn in_display(parts: Vec<(String, i64)>, s: &CapsSettings, date: &str) -> Total {
    let mut total = Total {
        amount: Money::ZERO,
        complete: true,
    };
    for (currency, micros) in parts {
        let m = Money::from_micros(micros);
        let converted = if currency == s.display.as_str() {
            Some(m)
        } else {
            Currency::parse(&currency)
                .ok()
                .and_then(|c| s.rate(&c, date))
                .map(|r| m.convert(&r))
        };
        match converted.and_then(|c| total.amount.checked_add(c)) {
            Some(sum) => total.amount = sum,
            None => total.complete = false,
        }
    }
    total
}

/// This machine's settled total of `cap` in `period`, in the display currency.
fn settled_total(
    conn: &Connection,
    s: &CapsSettings,
    date: &str,
    period: &str,
    cap: &str,
) -> rusqlite::Result<Total> {
    let parts = conn
        .prepare("SELECT currency, settled_micros FROM cap_settled WHERE period = ?1 AND cap = ?2")?
        .query_map(params![period, cap], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    Ok(in_display(parts, s, date))
}

/// The amount held in `period`, in the display currency, of `provider` only when given.
fn held_total(
    conn: &Connection,
    s: &CapsSettings,
    date: &str,
    period: &str,
    provider: Option<&str>,
) -> rusqlite::Result<Total> {
    let parts = conn
        .prepare(
            "SELECT display_currency, SUM(display_micros) FROM cap_reservations
             WHERE state = 'held' AND period = ?1 AND (?2 IS NULL OR provider = ?2)
             GROUP BY display_currency",
        )?
        .query_map(params![period, provider], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    Ok(in_display(parts, s, date))
}

/// `a + b + c > limit`, an overflow (or a part without a rate) counting as over.
fn over(a: Total, b: Total, c: Money, limit: Money) -> bool {
    !a.complete
        || !b.complete
        || a.amount
            .checked_add(b.amount)
            .and_then(|s| s.checked_add(c))
            .is_none_or(|sum| sum > limit)
}

/// The caps an estimate of `est` (display currency) for `provider` would pass in `period` on
/// the local `date`: `monthly`, `per_call_max`, then the provider's, each with its limit (0 for
/// an invalid one), settled and held amounts.
fn hits(
    conn: &Connection,
    s: &CapsSettings,
    date: &str,
    period: &str,
    provider: &str,
    est: Money,
) -> rusqlite::Result<Vec<CapHit>> {
    let mut out = Vec::new();
    if s.monthly.is_some() || s.is_invalid(MONTHLY) {
        let settled = settled_total(conn, s, date, period, MONTHLY)?;
        let held = held_total(conn, s, date, period, None)?;
        let limit = if s.is_invalid(MONTHLY) {
            None
        } else {
            s.monthly
        };
        if limit.is_none_or(|l| over(settled, held, est, l)) {
            out.push(CapHit {
                cap: MONTHLY.to_string(),
                limit: limit.unwrap_or(Money::ZERO),
                settled: settled.amount,
                held: held.amount,
                estimate: est,
                currency: s.display.as_str().to_string(),
                rate: None,
            });
        }
    }
    if s.per_call_max.is_some() || s.is_invalid(PER_CALL_MAX) {
        let limit = if s.is_invalid(PER_CALL_MAX) {
            None
        } else {
            s.per_call_max
        };
        if limit.is_none_or(|l| est > l) {
            out.push(CapHit {
                cap: PER_CALL_MAX.to_string(),
                limit: limit.unwrap_or(Money::ZERO),
                settled: Money::ZERO,
                held: Money::ZERO,
                estimate: est,
                currency: s.display.as_str().to_string(),
                rate: None,
            });
        }
    }
    let name = provider_cap(provider);
    let limit = s.provider_monthly.get(provider).copied();
    if limit.is_some() || s.is_invalid(&name) {
        let settled = settled_total(conn, s, date, period, &name)?;
        let held = held_total(conn, s, date, period, Some(provider))?;
        let limit = if s.is_invalid(&name) { None } else { limit };
        if limit.is_none_or(|l| over(settled, held, est, l)) {
            out.push(CapHit {
                cap: name,
                limit: limit.unwrap_or(Money::ZERO),
                settled: settled.amount,
                held: held.amount,
                estimate: est,
                currency: s.display.as_str().to_string(),
                rate: None,
            });
        }
    }
    Ok(out)
}

/// Every core run's own id (as the meter's): reservations of another run were left by a dead
/// core.
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The caps (module docs).
pub struct Caps {
    states: Arc<StateRegistry>,
    env: Env,
    clock: Arc<dyn Clock>,
    events: Arc<EventBus>,
    failpoints: Arc<FailPoints>,
    ids: Arc<dyn IdSource>,
    run: String,
    /// The invalid caps last reported on the topic.
    reported_invalid: Mutex<Vec<String>>,
    /// One `app/caps.set` at a time.
    setting: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Caps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caps")
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

/// An alert raised: its cap, its level, the spend as a whole percentage of the cap (`None` for
/// a cap of 0) and the settled total.
type RaisedAlert = (String, u32, Option<i128>, Money);

impl Caps {
    pub fn new(
        states: Arc<StateRegistry>,
        env: Env,
        clock: Arc<dyn Clock>,
        events: Arc<EventBus>,
        failpoints: Arc<FailPoints>,
        ids: Arc<dyn IdSource>,
    ) -> Arc<Caps> {
        let started = clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let run = format!(
            "{}-{started}-{}",
            std::process::id(),
            RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        Arc::new(Caps {
            states,
            env,
            clock,
            events,
            failpoints,
            ids,
            run,
            reported_invalid: Mutex::new(Vec::new()),
            setting: tokio::sync::Mutex::new(()),
        })
    }

    fn now_ms(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }

    /// `utc` in local seconds since the epoch.
    fn local_secs(&self, utc: SystemTime) -> i64 {
        let secs = utc
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        secs + i64::from(self.clock.local_offset_secs(utc))
    }

    /// The local calendar month of `utc`, `YYYY-MM` (CORE-012).
    pub fn period(&self, utc: SystemTime) -> String {
        lil_schema::time::year_month(self.local_secs(utc))
    }

    fn db(&self) -> Arc<StateDb> {
        self.states.suite_db()
    }

    /// `suite.toml`'s caps, read now. A missing file is no caps; one that can't be read fails
    /// closed. A changed set of invalid caps is published as `invalid`, an empty one too once
    /// the set cleared.
    pub fn settings(&self) -> CapsSettings {
        let path = self.env.suite_toml_to_read();
        let s = match std::fs::read_to_string(&path) {
            Ok(text) => CapsSettings::from_suite_toml(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => CapsSettings::default(),
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "suite.toml can't be read; the caps fail closed");
                CapsSettings {
                    invalid: vec![MONTHLY.to_string(), PER_CALL_MAX.to_string()],
                    ..CapsSettings::default()
                }
            }
        };
        let changed = {
            let mut last = lock(&self.reported_invalid);
            let changed = *last != s.invalid;
            last.clone_from(&s.invalid);
            changed
        };
        // A set that cleared is published too (empty), so a gauge stops showing "invalid"
        // (Appendix G, Checkpoint A M4).
        if changed {
            self.events
                .publish(TOPIC, json!({"kind": "invalid", "caps": s.invalid}));
        }
        s
    }

    /// The display currency of `suite.toml` now (the estimate stage's default currency).
    pub fn display_currency(&self) -> Currency {
        self.settings().display
    }

    /// `c` in the display currency at the rate of the local day, or `None` when there is no
    /// rate.
    fn price(&self, s: &CapsSettings, c: &Counted<'_>) -> Option<Priced> {
        if *c.currency == s.display {
            return Some(Priced {
                display: c.amount,
                rate_micros: ONE,
                rate_date: None,
            });
        }
        let rate = s.rate(c.currency, &self.today())?;
        Some(Priced {
            display: c.amount.convert(&rate),
            rate_micros: rate.in_display_micros,
            rate_date: Some(rate.date),
        })
    }

    /// Today's local date, `YYYY-MM-DD`: the day whose rates convert.
    fn today(&self) -> String {
        lil_schema::time::date(self.local_secs(self.clock.utc_now()))
    }

    /// `caps.invalid` for a negative estimate (M1): it would credit the caps, so it fails
    /// closed.
    fn negative(c: &Counted<'_>) -> ErrorBody {
        ErrorBody::new(
            &codes::CAPS_INVALID,
            format!(
                "The call's estimate {} {} is negative, so the caps can't count it.",
                c.amount, c.currency
            ),
            "",
        )
    }

    fn no_rate(s: &CapsSettings, c: &Counted<'_>) -> ErrorBody {
        ErrorBody::new(
            &codes::CAPS_INVALID,
            format!(
                "The call costs {} {}, and suite.toml has no [[money.rates]] entry from {} to the display currency {} dated on or before today, so the caps can't be checked. Add the rate in the LILITH app.",
                c.amount, c.currency, c.currency, s.display
            ),
            "",
        )
    }

    /// The caps `est` would pass now (stage 5's cap half): never for an estimate that doesn't
    /// count (CORE-090) or while no cap applies.
    ///
    /// # Errors
    /// `caps.invalid` when the estimate can't be converted; `state.unavailable`.
    pub async fn check(&self, est: &Estimate) -> Result<CapCheck, ErrorBody> {
        let Some(c) = counted(est) else {
            return Ok(CapCheck::Within);
        };
        if c.amount < Money::ZERO {
            return Err(Caps::negative(&c));
        }
        let s = self.settings();
        if !s.applies_to(c.provider) {
            return Ok(CapCheck::Within);
        }
        let priced = self.price(&s, &c).ok_or_else(|| Caps::no_rate(&s, &c))?;
        let period = self.period(self.clock.utc_now());
        let (provider, date) = (c.provider.to_string(), self.today());
        let rate = priced.rate();
        let found = self
            .db()
            .call(move |conn| {
                let mut found = hits(conn, &s, &date, &period, &provider, priced.display)
                    .map_err(|e| sql(&e))?;
                for h in &mut found {
                    h.rate.clone_from(&rate);
                }
                Ok(found)
            })
            .await
            .map_err(|e| state_unavailable(&e))?;
        Ok(if found.is_empty() {
            CapCheck::Within
        } else {
            CapCheck::Exceeds(found)
        })
    }

    /// Reserves `est` for `who` (module docs, "Reserve"): one `BEGIN IMMEDIATE`. `allow_once`
    /// lets it pass the caps that confirmation named, up to the estimate it showed, this once
    /// (it still counts); any other cap passed is `NeedsCap`, so the call asks again.
    ///
    /// # Errors
    /// `caps.invalid` for a negative estimate, or one that can't be converted while a cap
    /// applies;
    /// `state.unavailable`; `store.io` from the `caps.after_reserve` fail point (as a crash
    /// there: the reservation stays for the next start).
    pub async fn reserve(
        &self,
        est: &Estimate,
        who: ReserveFor<'_>,
        allow_once: Option<&AllowOnce>,
    ) -> Result<ReserveOutcome, ErrorBody> {
        let Some(c) = counted(est) else {
            return Ok(ReserveOutcome::None);
        };
        if c.amount < Money::ZERO {
            return Err(Caps::negative(&c));
        }
        let s = self.settings();
        let applies = s.applies_to(c.provider);
        // With no rate and no cap that applies, the reservation is kept in the estimate's own
        // currency (CORE-086: every call reserves); a cap set later converts it, or counts as
        // exceeded without a rate.
        let (priced, display_currency) = match self.price(&s, &c) {
            Some(p) => (p, s.display.as_str().to_string()),
            None if applies => return Err(Caps::no_rate(&s, &c)),
            None => (
                Priced {
                    display: c.amount,
                    rate_micros: ONE,
                    rate_date: None,
                },
                c.currency.as_str().to_string(),
            ),
        };
        let now = self.clock.utc_now();
        let row = Reservation {
            id: self.ids.id("rv"),
            run: self.run.clone(),
            owner: who.owner.to_string(),
            workspace: who.workspace.to_string(),
            provider: c.provider.to_string(),
            basis: c.basis.wire().to_string(),
            period: self.period(now),
            currency: c.currency.as_str().to_string(),
            amount: c.amount,
            rate_micros: priced.rate_micros,
            rate_date: priced.rate_date.clone(),
            display_currency,
            display: priced.display,
            allowed_by: allow_once.map(|a| a.confirmation_id.clone()),
            state: ReservationState::Held,
            settled: None,
            kept: false,
        };
        let created = self.now_ms();
        let allowed = allow_once.cloned();
        let date = self.today();
        let rate = priced.rate();
        let id = row.id.clone();
        let done = self
            .db()
            .call(move |conn| {
                let mut run = || -> rusqlite::Result<Result<(), Vec<CapHit>>> {
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    if applies {
                        let mut found =
                            hits(&tx, &s, &date, &row.period, &row.provider, row.display)?;
                        for h in &mut found {
                            h.rate.clone_from(&rate);
                        }
                        // A confirmation's allow-once passes only the caps it named, up to
                        // the estimate it showed (CORE-091); anything else asks again.
                        let passes = allowed
                            .as_ref()
                            .is_some_and(|a| a.covers(&found, row.display));
                        if !found.is_empty() && !passes {
                            return Ok(Err(found));
                        }
                    }
                    tx.execute(
                        &format!(
                            "INSERT INTO cap_reservations ({COLUMNS}, created_ms)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                                     'held', NULL, 0, ?15)"
                        ),
                        params![
                            row.id,
                            row.run,
                            row.owner,
                            row.workspace,
                            row.provider,
                            row.basis,
                            row.period,
                            row.currency,
                            row.amount.micros(),
                            row.rate_micros,
                            row.rate_date,
                            row.display_currency,
                            row.display.micros(),
                            row.allowed_by,
                            created,
                        ],
                    )?;
                    tx.commit()?;
                    Ok(Ok(()))
                };
                run().map_err(|e| sql(&e))
            })
            .await
            .map_err(|e| state_unavailable(&e))?;
        if let Err(found) = done {
            return Ok(ReserveOutcome::NeedsCap(found));
        }
        tracing::info!(reservation = %id, owner = who.owner, workspace = who.workspace, allowed_once = allow_once.is_some(), "a cap reservation is held");
        if let Err(e) = self.failpoints.hit(AFTER_RESERVE) {
            return Err(ErrorBody::new(
                &codes::STORE_IO,
                format!("{AFTER_RESERVE}: {e}"),
                "",
            ));
        }
        Ok(ReserveOutcome::Reserved(id))
    }

    /// Reservation `id`, if there is one.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn reservation(&self, id: &str) -> Result<Option<Reservation>, ErrorBody> {
        let id = id.to_string();
        self.db()
            .call(move |conn| {
                conn.query_row(
                    &format!("SELECT {COLUMNS} FROM cap_reservations WHERE id = ?1"),
                    params![id],
                    read_row,
                )
                .optional()
                .map_err(|e| sql(&e))
            })
            .await
            .map_err(|e| state_unavailable(&e))
    }

    /// The reservations made in `period`, oldest first.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn list(&self, period: &str) -> Result<Vec<Reservation>, ErrorBody> {
        let period = period.to_string();
        self.db()
            .call(move |conn| {
                let read = || -> rusqlite::Result<Vec<Reservation>> {
                    conn.prepare(&format!(
                        "SELECT {COLUMNS} FROM cap_reservations WHERE period = ?1
                         ORDER BY created_ms, id"
                    ))?
                    .query_map(params![period], read_row)?
                    .collect()
                };
                read().map_err(|e| sql(&e))
            })
            .await
            .map_err(|e| state_unavailable(&e))
    }

    /// Settles reservation `reservation` once (module docs, "Settle"): `billed` adds `actual`
    /// (in the reservation's own currency, converted at its recorded rate; `None`, or a
    /// negative amount, is the reserved amount, ruling P3-R15) to the month's totals; unbilled
    /// releases it. A reservation that is settled or released already is left as it is.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn settle(
        &self,
        reservation: &str,
        actual: Option<Money>,
        billed: bool,
    ) -> Result<(), ErrorBody> {
        let (id, ended) = (reservation.to_string(), self.now_ms());
        let settled = self
            .db()
            .call(move |conn| {
                let mut run = || -> rusqlite::Result<Option<String>> {
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let row = tx
                        .query_row(
                            &format!("SELECT {COLUMNS} FROM cap_reservations WHERE id = ?1"),
                            params![id],
                            read_row,
                        )
                        .optional()?;
                    let Some(row) = row.filter(|r| r.state == ReservationState::Held) else {
                        return Ok(None);
                    };
                    if !billed {
                        tx.execute(
                            "UPDATE cap_reservations SET state = 'released', ended_ms = ?2
                             WHERE id = ?1 AND state = 'held'",
                            params![row.id, ended],
                        )?;
                        tx.commit()?;
                        return Ok(None);
                    }
                    let amount = actual.filter(|a| *a >= Money::ZERO).map_or(row.display, |a| {
                        a.convert(&Rate {
                            in_display_micros: row.rate_micros,
                            date: row.rate_date.clone().unwrap_or_default(),
                        })
                    });
                    tx.execute(
                        "UPDATE cap_reservations SET state = 'settled', settled_micros = ?2,
                             ended_ms = ?3
                         WHERE id = ?1 AND state = 'held'",
                        params![row.id, amount.micros(), ended],
                    )?;
                    for cap in [MONTHLY.to_string(), provider_cap(&row.provider)] {
                        tx.execute(
                            "INSERT INTO cap_settled (period, cap, currency, settled_micros)
                             VALUES (?1, ?2, ?3, ?4)
                             ON CONFLICT (period, cap, currency)
                             DO UPDATE SET settled_micros = settled_micros + excluded.settled_micros",
                            params![row.period, cap, row.display_currency, amount.micros()],
                        )?;
                    }
                    tx.commit()?;
                    Ok(Some(row.period))
                };
                run().map_err(|e| sql(&e))
            })
            .await
            .map_err(|e| state_unavailable(&e))?;
        tracing::debug!(reservation, billed, "a cap reservation is settled");
        if let Some(period) = settled {
            self.alerts(&period).await;
        }
        Ok(())
    }

    /// Settles `rv` from the finished usage records of its call: billed when they say
    /// `succeeded`, at their summed cost when each is in the reservation's currency (else at the
    /// reserved amount).
    pub(crate) async fn settle_from_records(
        &self,
        rv: &str,
        records: &[Map<String, Value>],
        billed: bool,
    ) -> Result<(), ErrorBody> {
        let actual = match (billed, records.is_empty()) {
            (true, false) => match self.reservation(rv).await? {
                Some(r) => records.iter().try_fold(Money::ZERO, |sum, rec| {
                    record::amount(rec)
                        .filter(|(_, c)| c.as_str() == r.currency)
                        .and_then(|(m, _)| sum.checked_add(m))
                }),
                None => None,
            },
            _ => None,
        };
        self.settle(rv, actual, billed).await
    }

    /// Raises the alerts of `period` that the settled totals reached and that weren't raised
    /// yet (CORE-089).
    async fn alerts(&self, period: &str) {
        let s = self.settings();
        let mut caps: Vec<(String, Money)> = Vec::new();
        if let Some(l) = s.monthly {
            caps.push((MONTHLY.to_string(), l));
        }
        for (p, l) in &s.provider_monthly {
            caps.push((provider_cap(p), *l));
        }
        if caps.is_empty() {
            return;
        }
        let named = period.to_string();
        let currency = s.display.to_string();
        let (period, date, now) = (period.to_string(), self.today(), self.now_ms());
        let raised = self
            .db()
            .call(move |conn| {
                let mut run = || -> rusqlite::Result<Vec<RaisedAlert>> {
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let mut raised = Vec::new();
                    for (cap, limit) in &caps {
                        let total = settled_total(&tx, &s, &date, &period, cap)?;
                        // A part without a rate can't be measured: no alert is guessed.
                        let settled = if total.complete {
                            total.amount
                        } else {
                            Money::ZERO
                        };
                        // The spend each alert shows (owner decision 2026-10-26).
                        let spent = (limit.micros() > 0).then(|| {
                            i128::from(settled.micros()) * 100 / i128::from(limit.micros())
                        });
                        for level in ALERT_LEVELS {
                            let reached = settled > Money::ZERO
                                && i128::from(settled.micros()) * 100
                                    >= i128::from(limit.micros()) * i128::from(level);
                            if reached
                                && tx.execute(
                                    "INSERT OR IGNORE INTO cap_alerts
                                         (period, cap, threshold_pct, alerted_ms)
                                     VALUES (?1, ?2, ?3, ?4)",
                                    params![period, cap, level, now],
                                )? == 1
                            {
                                raised.push((cap.clone(), level, spent, settled));
                            }
                        }
                    }
                    tx.commit()?;
                    Ok(raised)
                };
                run().map_err(|e| sql(&e))
            })
            .await;
        match raised {
            Ok(raised) => {
                for (cap, level, spent, settled) in raised {
                    let spent = spent.map(|p| u64::try_from(p).unwrap_or(u64::MAX));
                    tracing::info!(cap = %cap, threshold_pct = level, spent_pct = ?spent, "a cap alert");
                    self.events.publish(
                        TOPIC,
                        json!({"kind": "alert", "cap": cap, "threshold_pct": level,
                               "spent_pct": spent, "spent": settled.to_decimal_string(),
                               "currency": currency, "period": named}),
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "the cap alerts could not be checked"),
        }
    }

    /// Releases reservation `reservation` of `owner`, unused (no usage line). Idempotent; one
    /// that is settled, released or another's is left alone.
    pub async fn release(&self, reservation: &str, owner: &str) {
        let (id, owner_id, ended) = (reservation.to_string(), owner.to_string(), self.now_ms());
        let changed = self
            .db()
            .call(move |conn| {
                conn.execute(
                    "UPDATE cap_reservations SET state = 'released', ended_ms = ?3
                     WHERE id = ?1 AND owner = ?2 AND state = 'held'",
                    params![id, owner_id, ended],
                )
                .map_err(|e| sql(&e))
            })
            .await;
        match changed {
            Ok(1) => tracing::info!(reservation, owner, "a cap reservation is released unused"),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(reservation, error = %e, "a cap reservation could not be released; it stays held (fail closed)");
            }
        }
    }

    /// `reservation`, made for `from`, now belongs to job `to`.
    pub async fn hand_over(&self, reservation: &str, from: &str, to: &JobId) {
        let (id, from_id, to_id) = (reservation.to_string(), from.to_string(), to.to_string());
        let changed = self
            .db()
            .call(move |conn| {
                conn.execute(
                    "UPDATE cap_reservations SET owner = ?3
                     WHERE id = ?1 AND owner = ?2 AND state = 'held'",
                    params![id, from_id, to_id],
                )
                .map_err(|e| sql(&e))
            })
            .await;
        match changed {
            Ok(1) => {
                tracing::debug!(reservation, from, to = %to, "a cap reservation is handed over")
            }
            Ok(_) => {
                tracing::warn!(reservation, from, to = %to, "a cap reservation to hand over was not held by its owner")
            }
            Err(e) => {
                tracing::warn!(reservation, error = %e, "a cap reservation could not be handed over")
            }
        }
    }

    /// The suite scope's repair at start (module docs, "Repair"): how many reservations were
    /// settled, kept or released.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn repair(&self) -> Result<u32, ErrorBody> {
        self.repair_scope(NO_WORKSPACE, &self.states.suite_db())
            .await
    }

    /// The suite scope's start (ruling P3-R33): [`Caps::repair`], then the meter's recovery
    /// ([`super::Meter::recover`]) only when the repair succeeded. After a failed repair the
    /// scope's usage rows stay, so the next start decides their reservations from them.
    pub async fn repair_then_recover(&self, meter: &super::Meter) -> ScopeStart {
        let repaired = self.repair().await;
        let recovered = match repaired {
            Ok(_) => Some(meter.recover().await),
            Err(_) => None,
        };
        ScopeStart {
            repaired,
            recovered,
        }
    }

    /// [`Caps::repair_then_recover`] for workspace `open`: [`Caps::repair_workspace`], then
    /// [`super::Meter::recover_workspace`] only when it succeeded.
    pub async fn repair_then_recover_workspace(
        &self,
        meter: &super::Meter,
        open: &Arc<OpenWorkspace>,
    ) -> ScopeStart {
        let repaired = self.repair_workspace(open).await;
        let recovered = match repaired {
            Ok(_) => Some(meter.recover_workspace(open).await),
            Err(_) => None,
        };
        ScopeStart {
            repaired,
            recovered,
        }
    }

    /// The suite scope's job reservations at start, once its jobs are recovered (module docs,
    /// "Repair").
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn repair_jobs(&self) -> Result<u32, ErrorBody> {
        self.repair_job_scope(NO_WORKSPACE, &self.states.suite_db())
            .await
    }

    /// [`Caps::repair`] and [`Caps::repair_jobs`] for workspace `open` (read-write only), once
    /// its jobs are recovered and before its usage rows are.
    ///
    /// # Errors
    /// As [`Caps::repair`].
    pub async fn repair_workspace(&self, open: &OpenWorkspace) -> Result<u32, ErrorBody> {
        if open.access != Access::ReadWrite {
            return Ok(0);
        }
        let calls = self.repair_scope(&open.ws.id, &open.state).await?;
        Ok(calls + self.repair_job_scope(&open.ws.id, &open.state).await?)
    }

    /// The held reservations of a dead run whose owner starts with `prefix`, in `workspace`.
    async fn left_held(
        &self,
        workspace: &str,
        prefix: &str,
    ) -> Result<Vec<Reservation>, ErrorBody> {
        let (run, ws, like) = (
            self.run.clone(),
            workspace.to_string(),
            format!("{prefix}\\_%"),
        );
        self.db()
            .call(move |conn| {
                let read = || -> rusqlite::Result<Vec<Reservation>> {
                    conn.prepare(&format!(
                        "SELECT {COLUMNS} FROM cap_reservations
                         WHERE state = 'held' AND kept = 0 AND run <> ?1 AND workspace = ?2
                           AND owner LIKE ?3 ESCAPE '\\'
                         ORDER BY created_ms, id"
                    ))?
                    .query_map(params![run, ws, like], read_row)?
                    .collect()
                };
                read().map_err(|e| sql(&e))
            })
            .await
            .map_err(|e| state_unavailable(&e))
    }

    /// A dead run's held job reservations (`j_…`), decided by the job's row in `jobs` once the
    /// scope's jobs are recovered: a job that is gone, cancelled or failed releases it, unless it
    /// was `interrupted` with external side effects (it may have been charged, so it stays held,
    /// `kept`); one that succeeded settles it at the reserved amount; one still unfinished (an
    /// `interrupted_unknown` job) keeps it until you resolve it.
    ///
    /// A call's reservation (`c_…`) whose job holds it (`jobs.reservation`, a hand-over that
    /// failed) is decided here the same way, by that job (ruling P3-R33).
    async fn repair_job_scope(&self, workspace: &str, jobs: &StateDb) -> Result<u32, ErrorBody> {
        let mut repaired = 0;
        let mut held = self.left_held(workspace, "j").await?;
        held.extend(self.left_held(workspace, "c").await?);
        for r in held {
            let row = if r.owner.starts_with("j_") {
                crate::jobs::record::get(jobs, &r.owner).await
            } else {
                crate::jobs::record::holding(jobs, &r.id).await
            }
            .map_err(|e| state_unavailable(&e))?;
            if row.is_none() && !r.owner.starts_with("j_") {
                // No job took this call's reservation: the calls' repair decides it.
                continue;
            }
            match row {
                Some(job) if !job.status.is_final() => continue,
                Some(job) if job.status == JobStatus::Succeeded => {
                    self.settle(&r.id, None, true).await?;
                }
                Some(job)
                    if job.external_side_effects
                        && job.substatus == Some(Substatus::Interrupted) =>
                {
                    tracing::warn!(reservation = %r.id, job_id = %job.job_id, "an interrupted job with external side effects may have been charged; its reservation stays held");
                    self.keep(&r.id).await?;
                }
                _ => self.settle(&r.id, None, false).await?,
            }
            repaired += 1;
        }
        Ok(repaired)
    }

    /// A dead run's held call reservations (`c_…`) in `workspace`, decided by their calls'
    /// usage rows in `usage` (module docs, "Repair"). It goes on past a reservation it can't
    /// decide (that one stays held and is logged) and then returns the first error, so the
    /// caller skips the meter's recovery of the scope (ruling P3-R33).
    async fn repair_scope(&self, workspace: &str, usage: &StateDb) -> Result<u32, ErrorBody> {
        let held = self.left_held(workspace, "c").await?;
        let mut repaired = 0;
        let mut first_error = None;
        for r in held {
            match self.repair_call(&r, usage).await {
                Ok(true) => repaired += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(reservation = %r.id, code = %e.code, error = %e.message, "a dead call's cap reservation could not be repaired; it stays held until the next start");
                    first_error.get_or_insert(e);
                }
            }
        }
        first_error.map_or(Ok(repaired), Err)
    }

    /// Decides one dead call's reservation `r`; `false` when it is left alone (a job holds it).
    async fn repair_call(&self, r: &Reservation, usage: &StateDb) -> Result<bool, ErrorBody> {
        let job = crate::jobs::record::holding(usage, &r.id)
            .await
            .map_err(|e| state_unavailable(&e))?;
        if let Some(job) = job {
            tracing::warn!(reservation = %r.id, job_id = %job.job_id, "a dead call's reservation was never handed over to its job; the jobs' repair decides it");
            return Ok(false);
        }
        let rows = record::of_reservation(usage, &r.id)
            .await
            .map_err(|e| state_unavailable(&e))?;
        if rows.iter().any(|row| !row.logging) {
            tracing::warn!(reservation = %r.id, "a dead call's usage was pending; its reservation stays held");
            self.keep(&r.id).await?;
        } else if rows.is_empty() {
            // The meter never removes a call's row while its reservation is undecided
            // (`Meter::recover`), so no row means no line: nothing was sent.
            tracing::info!(reservation = %r.id, "a dead call sent nothing; its reservation is released");
            self.settle(&r.id, None, false).await?;
        } else {
            let records: Vec<Map<String, Value>> = rows.into_iter().map(|row| row.record).collect();
            let billed = records
                .iter()
                .all(|rec| rec.get("status").and_then(Value::as_str) == Some("succeeded"));
            tracing::warn!(reservation = %r.id, billed, "a dead call's usage line was written; its reservation is settled");
            self.settle_from_records(&r.id, &records, billed).await?;
        }
        Ok(true)
    }

    async fn keep(&self, id: &str) -> Result<(), ErrorBody> {
        let id = id.to_string();
        self.db()
            .call(move |conn| {
                conn.execute(
                    "UPDATE cap_reservations SET kept = 1 WHERE id = ?1",
                    params![id],
                )
                .map(drop)
                .map_err(|e| sql(&e))
            })
            .await
            .map_err(|e| state_unavailable(&e))
    }

    /// The caps as `caps.get` shows them (`CapsView`): each cap set (or invalid) with this
    /// period's settled and held amounts.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn view(&self) -> Result<CapsView, ErrorBody> {
        let s = self.settings();
        let period = self.period(self.clock.utc_now());
        let mut caps: Vec<(String, Option<Money>, Option<String>)> = Vec::new();
        if s.monthly.is_some() || s.is_invalid(MONTHLY) {
            caps.push((MONTHLY.to_string(), s.monthly, None));
        }
        if s.per_call_max.is_some() || s.is_invalid(PER_CALL_MAX) {
            caps.push((PER_CALL_MAX.to_string(), s.per_call_max, None));
        }
        let mut providers: Vec<String> = s.provider_monthly.keys().cloned().collect();
        providers.extend(
            s.invalid
                .iter()
                .filter_map(|n| n.strip_prefix("provider:"))
                .map(str::to_string),
        );
        providers.sort();
        providers.dedup();
        for p in providers {
            caps.push((
                provider_cap(&p),
                s.provider_monthly.get(&p).copied(),
                Some(p),
            ));
        }
        let (settings, date, at) = (s.clone(), self.today(), period.clone());
        let rows = self
            .db()
            .call(move |conn| {
                let mut out = Vec::new();
                let zero = Total {
                    amount: Money::ZERO,
                    complete: true,
                };
                for (cap, limit, provider) in caps {
                    let (settled, held) = if cap == PER_CALL_MAX {
                        (zero, zero)
                    } else {
                        (
                            settled_total(conn, &settings, &date, &at, &cap)
                                .map_err(|e| sql(&e))?,
                            held_total(conn, &settings, &date, &at, provider.as_deref())
                                .map_err(|e| sql(&e))?,
                        )
                    };
                    out.push((cap, limit, settled, held));
                }
                Ok(out)
            })
            .await
            .map_err(|e| state_unavailable(&e))?;
        let caps = rows
            .into_iter()
            .map(|(cap, limit, settled, held)| {
                // A part kept in another currency without a rate counts as exceeded too.
                let invalid = s.is_invalid(&cap) || !settled.complete || !held.complete;
                let (settled, held) = (settled.amount, held.amount);
                let limit = if invalid {
                    Money::ZERO
                } else {
                    limit.unwrap_or(Money::ZERO)
                };
                let used = i128::from(settled.micros()) + i128::from(held.micros());
                let ratio_permille = if limit == Money::ZERO {
                    if used > 0 || invalid { 1000 } else { 0 }
                } else {
                    u64::try_from(used * 1000 / i128::from(limit.micros())).unwrap_or(0)
                };
                CapRow {
                    cap,
                    limit: limit.to_decimal_string(),
                    settled: settled.to_decimal_string(),
                    held: held.to_decimal_string(),
                    ratio_permille,
                    invalid: invalid.then_some(true),
                }
            })
            .collect();
        Ok(CapsView {
            display_currency: s.display.as_str().to_string(),
            caps,
            period,
        })
    }

    /// Changes the caps for `by` (CORE-008): only a verified `ui:*` session whose shared
    /// capabilities hold `caps.v1`. The new keys are written to `suite.toml` and the legacy ones
    /// removed, keeping comments; `changed` is published. Returns the new view.
    ///
    /// # Errors
    /// `caller.not_allowed` for any other session; `caps.invalid` for an amount that isn't a
    /// non-negative decimal string or an empty provider id; the store's error when `suite.toml`
    /// can't be written; `state.unavailable`.
    pub async fn set(&self, by: &Session, p: CapsSetParams) -> Result<CapsView, ErrorBody> {
        let trace_id = self.ids.id("t");
        if !matches!(by.caller, CallerKind::Ui(_))
            || !by.capabilities.iter().any(|c| c == CAPS_CAPABILITY)
        {
            tracing::warn!(caller = %by.caller, session = %by.id, "a caps change from a session that may not make one; refused");
            return Err(ErrorBody::new(
                &codes::CALLER_NOT_ALLOWED,
                "Only the LILITH app can change the caps.",
                trace_id,
            ));
        }
        let invalid = |what: &str, why: String| {
            ErrorBody::new(
                &codes::CAPS_INVALID,
                format!(
                    "The {what} cap is not valid: {why}. Give a decimal amount like \"20\" or \"7.5\", or null to clear it."
                ),
                trace_id.clone(),
            )
        };
        let norm = |what: &str, v: Option<String>| -> Result<Option<String>, ErrorBody> {
            v.map(|text| {
                let m = Money::parse_decimal(text.trim()).map_err(|why| invalid(what, why))?;
                if m < Money::ZERO {
                    return Err(invalid(what, format!("{text:?} is negative")));
                }
                Ok(m.to_decimal_string())
            })
            .transpose()
        };
        let mut edit = CapsEdit {
            monthly: p.monthly.map(|v| norm(MONTHLY, v)).transpose()?,
            per_call_max: p.per_call_max.map(|v| norm(PER_CALL_MAX, v)).transpose()?,
            ..CapsEdit::default()
        };
        for (provider, v) in p.provider_monthly.unwrap_or_default() {
            if provider.trim().is_empty() {
                return Err(invalid("provider", "the provider id is empty".into()));
            }
            let name = provider_cap(&provider);
            let v = norm(&name, v)?;
            edit.provider_monthly.push((provider, v));
        }
        {
            let _one_at_a_time = self.setting.lock().await;
            // A `suite.toml` whose `caps` the editor can't change is your config to fix by
            // hand: a clear error, not the store's.
            let text = std::fs::read_to_string(self.env.suite_toml_to_read()).unwrap_or_default();
            if let Some(why) = super::caps_settings::shape_problem(&text) {
                return Err(ErrorBody::new(
                    &codes::CAPS_INVALID,
                    format!(
                        "The caps can't be changed: {why}. Fix that line in suite.toml, then set the caps again."
                    ),
                    trace_id,
                ));
            }
            edit.carry_legacy = self.settings().display.as_str() == "USD";
            crate::store::set_caps(&self.env, &edit, &self.failpoints)
                .await
                .map_err(|e| e.to_error(&trace_id))?;
        }
        let view = self.view().await?;
        tracing::info!(caller = %by.caller, "the caps changed");
        self.events
            .publish(TOPIC, json!({"kind": "changed", "caps": view}));
        // A lowered cap may have reached a level already.
        self.alerts(&view.period).await;
        Ok(view)
    }

    /// A job's reservation, by its outcome (module docs, "Settle").
    async fn job_completed(&self, job: &JobCtx, outcome: &Outcome<ToolOutput>) {
        let Some(rv) = &job.reservation else {
            return;
        };
        let billed = match outcome {
            Outcome::Ok(_) | Outcome::PendingConfirmation(_) => true,
            o if resolved_unknown(o) => true,
            o if is_cancelled(o) || never_started(o) => false,
            Outcome::Error { .. } => false,
        };
        if let Err(e) = self.settle(rv, None, billed).await {
            tracing::error!(job_id = %job.id, reservation = %rv, code = %e.code, "a job's cap reservation could not be settled; it stays held");
        }
    }
}

impl ReserveStage for Caps {
    fn reserve<'a>(
        &'a self,
        i: ReserveInput<'a>,
    ) -> BoxFuture<'a, Result<ReserveOutcome, ErrorBody>> {
        Box::pin(Caps::reserve(
            self,
            i.estimate,
            ReserveFor {
                owner: i.call_id,
                workspace: i.workspace_id,
            },
            i.allow_once,
        ))
    }
}

impl Reservations for Caps {
    fn hand_over<'a>(
        &'a self,
        reservation: &'a str,
        from: &'a str,
        to: &'a JobId,
    ) -> JobFuture<'a, ()> {
        Box::pin(Caps::hand_over(self, reservation, from, to))
    }

    fn release<'a>(&'a self, reservation: &'a str, owner: &'a str) -> JobFuture<'a, ()> {
        Box::pin(Caps::release(self, reservation, owner))
    }
}

impl JobCompletion for Caps {
    fn completed<'a>(
        &'a self,
        job: &'a JobCtx,
        outcome: &'a Outcome<ToolOutput>,
        _done: Option<&'a JobDone>,
    ) -> JobFuture<'a, ()> {
        Box::pin(self.job_completed(job, outcome))
    }
}
