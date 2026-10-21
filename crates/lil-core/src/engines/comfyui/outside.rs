//! Outside jobs (ARCH-020, ARCH-057, ARCH-069, CORE-093; Appendix F "ComfyUI client"): prompts
//! in ComfyUI that the suite didn't submit (its web UI, the comfy MCP, scripts).
//!
//! While the engine is ready, the watch polls `GET /api/queue` and `GET /api/jobs` (its newest
//! [`JOBS_PAGE`]) every [`POLL_EVERY`] ([`OutsideWatch::poll`]). Both answers can carry an
//! outside client's JSON, so they are read with their nesting capped
//! ([`super::client::cap_depth`]): a deep value can't fail the poll.
//!
//! - **Counts.** A `/queue` entry is the suite's only when its prompt id is one the suite
//!   claimed before posting or took up from an earlier core run (`client_id` is readable by
//!   anyone from `/queue`, so it proves nothing). Every other entry is an outside job,
//!   `running` or `queued`.
//! - **What they mean.** A busy watch keeps the core from going idle ([`OutsideWatch`] is an
//!   `IdleBlocker`), but holds no drain: an outside job is not the suite's work to wait for.
//!   Suite jobs of the engine wait for it (`EngineControl::outside_busy`, the scheduler's
//!   `Admit::Wait`), and the suite never cancels one (ARCH-069).
//! - **Paid prompts** (ARCH-057, CORE-093; ruling P3-R27). An outside prompt whose graph has
//!   partner nodes gets exactly one unattributed usage draft (`route: "comfy-partner"`, no
//!   estimate). It is kept in the suite `state.db` (`outside_paid`) from the moment it is
//!   first seen, so a restart or a crash neither repeats nor loses it: the core that sees its
//!   end records it, with the status and latency `/api/jobs` reports (`completed` →
//!   `succeeded`, `failed`, `cancelled`; latency from its execution times), and one that
//!   ComfyUI no longer knows is recorded `pending` (its outcome was not seen). The open row
//!   is the usage's write-ahead record (`UsageSink::record_outside`): the meter keeps no
//!   `usage_pending` row for it, and writes no second line for a prompt whose line is
//!   already in the log, so a stop between the line and the row's mark repeats nothing. The
//!   engines stopping records nothing: the row stays open for the next core. `/api/jobs` also
//!   shows prompts that came and went between two polls: each finished one never seen before
//!   is looked up once (`/api/jobs/{id}`, for its graph); the first poll's finished prompts
//!   are taken as already known (apart from the open rows of the table).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::client::{ComfyClient, QueueEntry, RemoteJob};
use super::graph::partner_nodes;
use crate::lifecycle::IdleBlocker;
use crate::meter::{Attribution, UsageCtx, UsageDraft, UsageStatus};
use crate::state::{StateDb, StateError};
use crate::tools::stages::BillingBasis;

/// How often outside jobs are polled while the engine is ready (Appendix F).
///
/// Plan 7a carry (AI-228): a live local model turn needs this at 250 ms or less.
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// How many of the newest `/api/jobs` entries each poll reads.
pub const JOBS_PAGE: u32 = 100;
/// The `tool` of an outside prompt's usage draft (no tool call made it).
pub const OUTSIDE_TOOL: &str = "comfyui.outside_prompt";
/// The provider of partner nodes: Comfy's API (comfy.org).
pub const PARTNER_PROVIDER: &str = "comfy-org";
/// The `caller_kind` of an outside prompt's usage.
pub const OUTSIDE_CALLER: &str = "outside";

#[derive(Debug, Default)]
struct WatchState {
    running: u32,
    queued: u32,
    /// The prompt ids the watch has classified (the suite's, free outside ones, recorded ones,
    /// the first poll's finished ones), kept only while ComfyUI still lists them.
    known: HashSet<String>,
    /// Paid outside prompts not yet recorded (the open rows of `outside_paid`), by id: their
    /// partner classes.
    paid: HashMap<String, Vec<String>>,
    /// Prompts of an earlier core run that an open job's row names (`external_id`): the
    /// suite's, though no recovery has taken them up yet (re-review N1). Kept as `known` is.
    earlier: HashSet<String>,
    /// The open rows were read from the table.
    loaded: bool,
    polled: bool,
}

/// The suite's prompt ids (module docs, "Counts").
pub(crate) struct Suite {
    pub prompt_ids: HashSet<String>,
}

impl Suite {
    fn owns(&self, prompt_id: &str) -> bool {
        self.prompt_ids.contains(prompt_id)
    }
}

/// A usage draft to hand the sink, with its context and the outside prompt it records.
pub(crate) struct Usage {
    pub prompt_id: String,
    pub draft: UsageDraft,
    pub ctx: UsageCtx,
}

/// Outside jobs (module docs).
#[derive(Debug, Default)]
pub struct OutsideWatch {
    state: Mutex<WatchState>,
    /// Cancelled once the core drains: the watch then holds nothing up.
    drain: Mutex<Option<CancellationToken>>,
}

impl OutsideWatch {
    fn lock(&self) -> MutexGuard<'_, WatchState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Outside prompts running in ComfyUI, as the last poll saw them.
    pub fn running(&self) -> u32 {
        self.lock().running
    }

    /// Outside prompts queued in ComfyUI, as the last poll saw them.
    pub fn queued(&self) -> u32 {
        self.lock().queued
    }

    /// Whether the last poll saw an outside prompt running or queued (never blocks).
    pub fn any(&self) -> bool {
        let s = self.lock();
        s.running + s.queued > 0
    }

    /// From the moment `drain` is cancelled (the lifecycle's drain token), the watch keeps
    /// nothing up.
    pub fn stop_on_drain(&self, drain: CancellationToken) {
        *self.drain.lock().unwrap_or_else(PoisonError::into_inner) = Some(drain);
    }

    fn draining(&self) -> bool {
        self.drain
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    /// The engine is gone: nothing runs in it any more (its paid prompts stay open). Whether
    /// the counts changed.
    pub(crate) fn clear(&self) -> bool {
        let mut s = self.lock();
        let changed = s.running + s.queued > 0;
        s.running = 0;
        s.queued = 0;
        changed
    }

    /// The number of prompt ids the watch remembers as classified (they are forgotten once
    /// ComfyUI no longer lists them).
    #[doc(hidden)]
    pub fn known_count(&self) -> usize {
        self.lock().known.len()
    }

    /// One poll (module docs). `suite` gives the suite's prompt ids, asked once ComfyUI's
    /// answers are in (re-review N2: a prompt is claimed before its POST, so any suite prompt
    /// they show is in it). `open_job` says whether a prompt id is the `external_id` of a job
    /// not yet final (re-review N1), asked once per new prompt. `object_info` is fetched only
    /// when a new outside graph needs a look; `db` is the suite `state.db`; `now_utc` is
    /// seconds since 1970. Returns whether the counts changed and the usage drafts to record;
    /// the caller marks each one recorded ([`mark_recorded`]) once it has handed it over. A
    /// request that fails changes nothing.
    #[allow(
        clippy::too_many_arguments,
        reason = "the poll's inputs, each a different source"
    )]
    pub(crate) async fn poll<S, G, FutG, F, Fut>(
        &self,
        client: &ComfyClient,
        suite: S,
        open_job: G,
        db: &StateDb,
        now_utc: i64,
        object_info: F,
        call_id: &mut (dyn FnMut() -> String + Send),
    ) -> (bool, Vec<Usage>)
    where
        S: FnOnce() -> Suite,
        G: Fn(String) -> FutG,
        FutG: std::future::Future<Output = bool>,
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Option<Arc<Value>>>,
    {
        if !self.lock().loaded {
            match load_open(db).await {
                Ok(open) => {
                    let mut s = self.lock();
                    s.paid.extend(open);
                    s.loaded = true;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "the open paid outside prompts can't be read; asked again next poll")
                }
            }
        }
        let queue = match client.outside_queue().await {
            Ok(q) => q,
            Err(e) => {
                tracing::debug!(error = %e, "ComfyUI's queue could not be read for outside jobs");
                return (false, Vec::new());
            }
        };
        let jobs = match client.jobs(JOBS_PAGE).await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(error = %e, "ComfyUI's jobs could not be read for outside jobs");
                return (false, Vec::new());
            }
        };
        let suite = suite();
        let in_queue: HashSet<&str> = queue
            .running
            .iter()
            .chain(queue.pending.iter())
            .map(|e| e.prompt_id.as_str())
            .collect();
        let mut info: Option<Arc<Value>> = None;
        let mut new_outside: Vec<(&QueueEntry, bool)> = Vec::new();
        let (mut running, mut queued) = (0u32, 0u32);
        {
            let mut s = self.lock();
            for (entries, is_running) in [(&queue.running, true), (&queue.pending, false)] {
                for e in entries {
                    if suite.owns(&e.prompt_id) || s.earlier.contains(&e.prompt_id) {
                        s.known.insert(e.prompt_id.clone());
                        continue;
                    }
                    if is_running {
                        running += 1;
                    } else {
                        queued += 1;
                    }
                    if !s.known.contains(&e.prompt_id) && !s.paid.contains_key(&e.prompt_id) {
                        new_outside.push((e, is_running));
                    }
                }
            }
        }
        // A new prompt an open job names is an earlier core run's: the suite's (N1).
        let mut fresh: Vec<&QueueEntry> = Vec::new();
        for (e, is_running) in new_outside {
            if open_job(e.prompt_id.clone()).await {
                let mut s = self.lock();
                s.earlier.insert(e.prompt_id.clone());
                s.known.insert(e.prompt_id.clone());
                if is_running {
                    running -= 1;
                } else {
                    queued -= 1;
                }
            } else {
                fresh.push(e);
            }
        }
        // New outside graphs: are they paid? Without object info they are looked at again
        // next time.
        for e in fresh {
            if info.is_none() {
                info = object_info().await;
            }
            let Some(i) = &info else {
                break;
            };
            let classes = partner_nodes(&e.graph, i);
            if classes.is_empty() {
                self.lock().known.insert(e.prompt_id.clone());
                continue;
            }
            match claim(db, &e.prompt_id, &classes, now_utc).await {
                // Recorded already (by an earlier core): nothing more to do.
                Ok(true) => {
                    self.lock().known.insert(e.prompt_id.clone());
                }
                Ok(false) => {
                    let mut s = self.lock();
                    s.known.insert(e.prompt_id.clone());
                    s.paid.insert(e.prompt_id.clone(), classes);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "a paid outside prompt can't be kept; it is looked at again next poll");
                }
            }
        }
        let finished: HashMap<&str, &RemoteJob> = jobs
            .iter()
            .filter(|j| matches!(j.status.as_str(), "completed" | "failed" | "cancelled"))
            .map(|j| (j.id.as_str(), j))
            .collect();
        let mut drafts = Vec::new();
        let mut lookups: Vec<&RemoteJob> = Vec::new();
        let mut open_lookups: Vec<String> = Vec::new();
        let changed;
        {
            let mut s = self.lock();
            let first = !s.polled;
            s.polled = true;
            for j in &jobs {
                if !finished.contains_key(j.id.as_str()) {
                    continue;
                }
                if let Some(classes) = s.paid.remove(&j.id) {
                    let (status, latency) = ended(&j.status, &j.raw);
                    drafts.push(usage(&j.id, &classes, status, latency, call_id()));
                    s.known.insert(j.id.clone());
                } else if first || suite.owns(&j.id) || s.earlier.contains(&j.id) {
                    s.known.insert(j.id.clone());
                } else if !s.known.contains(&j.id) {
                    lookups.push(j);
                }
            }
            // Open paid prompts neither queued nor listed: asked about one by one.
            for id in s.paid.keys() {
                if !in_queue.contains(id.as_str()) && !finished.contains_key(id.as_str()) {
                    open_lookups.push(id.clone());
                }
            }
            // `known` keeps only what ComfyUI still lists.
            let listed: HashSet<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
            s.known
                .retain(|id| listed.contains(id.as_str()) || in_queue.contains(id.as_str()));
            s.earlier
                .retain(|id| listed.contains(id.as_str()) || in_queue.contains(id.as_str()));
            changed = (s.running, s.queued) != (running, queued);
            s.running = running;
            s.queued = queued;
        }
        for id in open_lookups {
            let ended_now = match client.job_detail(&id).await {
                Ok(Some(detail)) => {
                    let status = detail["status"].as_str().unwrap_or("").to_string();
                    matches!(status.as_str(), "completed" | "failed" | "cancelled")
                        .then(|| ended(&status, &detail))
                }
                // ComfyUI no longer knows it (a restart): its outcome was not seen.
                Ok(None) => Some((UsageStatus::Pending, None)),
                Err(_) => None,
            };
            if let Some((status, latency)) = ended_now
                && let Some(classes) = self.lock().paid.remove(&id)
            {
                drafts.push(usage(&id, &classes, status, latency, call_id()));
            }
        }
        // Prompts that came and went between two polls.
        for j in lookups {
            let detail = match client.job_detail(&j.id).await {
                Ok(Some(d)) => d,
                Ok(None) => {
                    self.lock().known.insert(j.id.clone());
                    continue;
                }
                Err(_) => continue,
            };
            if info.is_none() {
                info = object_info().await;
            }
            let Some(i) = &info else {
                continue;
            };
            let classes = partner_nodes(&detail["workflow"]["prompt"], i);
            if !classes.is_empty() && !open_job(j.id.clone()).await {
                match claim(db, &j.id, &classes, now_utc).await {
                    Ok(false) => {
                        let (status, latency) = ended(&j.status, &j.raw);
                        drafts.push(usage(&j.id, &classes, status, latency, call_id()));
                    }
                    Ok(true) => {}
                    Err(_) => continue,
                }
            }
            self.lock().known.insert(j.id.clone());
        }
        (changed, drafts)
    }
}

impl IdleBlocker for OutsideWatch {
    fn name(&self) -> &'static str {
        "comfyui.outside_jobs"
    }

    fn busy(&self) -> bool {
        !self.draining() && self.any()
    }
}

/// The open rows of `outside_paid`: prompt id → partner classes.
async fn load_open(db: &StateDb) -> Result<Vec<(String, Vec<String>)>, StateError> {
    db.call(|conn| {
        let run = || -> rusqlite::Result<Vec<(String, String)>> {
            let mut st = conn.prepare(
                "SELECT prompt_id, classes FROM outside_paid WHERE recorded_utc IS NULL",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        };
        run().map_err(|e| StateError::Io(e.to_string()))
    })
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|(id, classes)| {
                let classes = serde_json::from_str(&classes).unwrap_or_default();
                (id, classes)
            })
            .collect()
    })
}

/// Keeps the paid outside prompt `prompt_id` (once: a row that exists is left as it is).
/// Whether its usage was recorded already.
async fn claim(
    db: &StateDb,
    prompt_id: &str,
    classes: &[String],
    now_utc: i64,
) -> Result<bool, StateError> {
    let (id, classes) = (
        prompt_id.to_string(),
        serde_json::to_string(classes).unwrap_or_else(|_| "[]".to_string()),
    );
    db.call(move |conn| {
        let run = || -> rusqlite::Result<bool> {
            conn.execute(
                "INSERT OR IGNORE INTO outside_paid (prompt_id, classes, first_seen_utc) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![id, classes, now_utc],
            )?;
            conn.query_row(
                "SELECT recorded_utc IS NOT NULL FROM outside_paid WHERE prompt_id = ?1",
                [&id],
                |r| r.get(0),
            )
        };
        run().map_err(|e| StateError::Io(e.to_string()))
    })
    .await
}

/// Marks the paid outside prompt `prompt_id` recorded (its usage line was handed over).
///
/// # Errors
/// When the row can't be written: the next core records the prompt again.
pub(crate) async fn mark_recorded(
    db: &StateDb,
    prompt_id: &str,
    now_utc: i64,
) -> Result<(), StateError> {
    let id = prompt_id.to_string();
    db.call(move |conn| {
        conn.execute(
            "UPDATE outside_paid SET recorded_utc = ?2 WHERE prompt_id = ?1",
            rusqlite::params![id, now_utc],
        )
        .map(drop)
        .map_err(|e| StateError::Io(e.to_string()))
    })
    .await
}

/// The usage status and latency of a finished `/api/jobs` entry of `status`.
fn ended(status: &str, entry: &Value) -> (UsageStatus, Option<u64>) {
    let status = match status {
        "completed" => UsageStatus::Succeeded,
        "cancelled" => UsageStatus::Cancelled,
        _ => UsageStatus::Failed,
    };
    let time = |k: &str| entry.get(k).and_then(Value::as_u64);
    let latency = match (time("execution_start_time"), time("execution_end_time")) {
        (Some(start), Some(end)) if end >= start => Some(end - start),
        _ => None,
    };
    (status, latency)
}

/// The unattributed usage draft of the outside prompt `prompt_id` (module docs). Its
/// `raw_usage` holds the prompt id and the classes as ComfyUI showed them: an outside client's
/// text, untrusted like every `raw_usage`.
fn usage(
    prompt_id: &str,
    classes: &[String],
    status: UsageStatus,
    latency_ms: Option<u64>,
    call_id: String,
) -> Usage {
    let mut units = serde_json::Map::new();
    units.insert("requests".to_string(), json!(1));
    let draft = UsageDraft {
        tool: OUTSIDE_TOOL.to_string(),
        provider: PARTNER_PROVIDER.to_string(),
        model: None,
        route: "comfy-partner".to_string(),
        basis: BillingBasis::PrepaidCredits,
        units,
        est_cost: None,
        account: None,
        raw_usage: Some(json!({"prompt_id": prompt_id, "partner_nodes": classes})),
        attribution: Attribution::Unattributed,
        status,
        latency_ms,
    };
    let ctx = UsageCtx {
        workspace: None,
        project: None,
        asset: None,
        job_id: None,
        call_id,
        caller_kind: OUTSIDE_CALLER.to_string(),
        reservation: None,
    };
    Usage {
        prompt_id: prompt_id.to_string(),
        draft,
        ctx,
    }
}
