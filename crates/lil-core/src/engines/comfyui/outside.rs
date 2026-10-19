//! Outside jobs (ARCH-020, ARCH-057, ARCH-069, CORE-093; Appendix F "ComfyUI client"): prompts
//! in ComfyUI that the suite didn't submit (its web UI, the comfy MCP, scripts).
//!
//! While the engine is ready, the watch polls `GET /api/queue` and `GET /api/jobs` every
//! [`POLL_EVERY`] ([`OutsideWatch::poll`]):
//!
//! - **Counts.** An entry of `/queue` is the suite's when its `extra_data.client_id` is the
//!   channel's `client_id` or its prompt id is one of the suite's prompts now; every other one is
//!   an outside job, `running` or `queued`. `/queue` is the source of the counts because only
//!   it names the client.
//! - **What they mean.** A busy watch keeps the core from going idle ([`OutsideWatch`] is an
//!   `IdleBlocker`), but holds no drain: an outside job is not the suite's work to wait for.
//!   Suite jobs of the engine wait for it (`EngineControl::outside_busy`, the scheduler's
//!   `Admit::Wait`), and the suite never cancels one (ARCH-069).
//! - **Paid prompts** (ARCH-057, CORE-093). An outside prompt whose graph has partner nodes
//!   gets one unattributed usage draft (`route: "comfy-partner"`, no estimate), once, when its
//!   end is seen: its status and latency come from `/api/jobs` (`completed` → `succeeded`,
//!   `failed`, `cancelled`; latency from its execution times). One that vanishes (ComfyUI
//!   restarted) or is still open when the engines stop is recorded `pending` (its outcome was
//!   not seen). `/api/jobs` also shows prompts that came and went between two polls: each
//!   finished one never seen before is looked up once (`/api/jobs/{id}`, for its graph and
//!   client); the first poll's finished prompts are taken as already known.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::client::{ComfyClient, QueueEntry, RemoteJob};
use super::graph::partner_nodes;
use crate::lifecycle::IdleBlocker;
use crate::meter::{Attribution, UsageCtx, UsageDraft, UsageStatus};
use crate::tools::stages::BillingBasis;

/// How often outside jobs are polled while the engine is ready (Appendix F).
///
/// Plan 7a carry (AI-228): a live local model turn needs this at 250 ms or less.
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// The `tool` of an outside prompt's usage draft (no tool call made it).
pub const OUTSIDE_TOOL: &str = "comfyui.outside_prompt";
/// The provider of partner nodes: Comfy's API (comfy.org).
pub const PARTNER_PROVIDER: &str = "comfy-org";
/// The `caller_kind` of an outside prompt's usage.
pub const OUTSIDE_CALLER: &str = "outside";

/// An outside prompt with partner nodes, seen but not yet recorded.
#[derive(Debug, Clone)]
struct Paid {
    classes: Vec<String>,
}

#[derive(Debug, Default)]
struct WatchState {
    running: u32,
    queued: u32,
    /// Every prompt id the watch has classified: the suite's, free outside ones, recorded ones,
    /// and the finished ones of the first poll.
    known: HashSet<String>,
    /// Paid outside prompts not yet recorded.
    paid: HashMap<String, Paid>,
    polled: bool,
}

/// What one poll needs to know about the suite.
pub(crate) struct Suite {
    pub client_id: Option<String>,
    pub prompt_ids: HashSet<String>,
}

impl Suite {
    fn owns(&self, prompt_id: &str, extra_data: &Value) -> bool {
        self.prompt_ids.contains(prompt_id)
            || self
                .client_id
                .as_deref()
                .is_some_and(|id| extra_data.get("client_id").and_then(Value::as_str) == Some(id))
    }
}

/// A usage draft to hand the sink, with its context.
pub(crate) type Usage = (UsageDraft, UsageCtx);

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

    /// The engine stopped: nothing runs in it any more (its paid prompts stay open until the
    /// next poll after it is ready again, or the engines stop). Whether the counts changed.
    pub(crate) fn clear(&self) -> bool {
        let mut s = self.lock();
        let changed = s.running + s.queued > 0;
        s.running = 0;
        s.queued = 0;
        changed
    }

    /// The paid prompts still open, recorded `pending` (the engines stop).
    pub(crate) fn flush(&self, call_id: &mut dyn FnMut() -> String) -> Vec<Usage> {
        let open: Vec<(String, Paid)> = self.lock().paid.drain().collect();
        open.into_iter()
            .map(|(id, p)| usage(&id, &p, UsageStatus::Pending, None, call_id()))
            .collect()
    }

    /// One poll (module docs): `/api/queue`, then `/api/jobs`. `object_info` is fetched only
    /// when a new outside graph needs a look. Returns whether the counts changed and the usage
    /// drafts to record. A request that fails changes nothing.
    pub(crate) async fn poll<F, Fut>(
        &self,
        client: &ComfyClient,
        suite: &Suite,
        object_info: F,
        call_id: &mut (dyn FnMut() -> String + Send),
    ) -> (bool, Vec<Usage>)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Option<Arc<Value>>>,
    {
        let queue = match client.outside_queue().await {
            Ok(q) => q,
            Err(e) => {
                tracing::debug!(error = %e, "ComfyUI's queue could not be read for outside jobs");
                return (false, Vec::new());
            }
        };
        let jobs = match client.jobs().await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(error = %e, "ComfyUI's jobs could not be read for outside jobs");
                return (false, Vec::new());
            }
        };
        let mut info: Option<Arc<Value>> = None;
        let mut new_outside: Vec<&QueueEntry> = Vec::new();
        let (mut running, mut queued) = (0u32, 0u32);
        {
            let mut s = self.lock();
            for (entries, is_running) in [(&queue.running, true), (&queue.pending, false)] {
                for e in entries {
                    if suite.owns(&e.prompt_id, &e.extra_data) {
                        s.known.insert(e.prompt_id.clone());
                        continue;
                    }
                    if is_running {
                        running += 1;
                    } else {
                        queued += 1;
                    }
                    if !s.known.contains(&e.prompt_id) {
                        new_outside.push(e);
                    }
                }
            }
        }
        // New outside graphs: are they paid?
        let mut paid_new: Vec<(String, Option<Vec<String>>)> = Vec::new();
        for e in new_outside {
            if info.is_none() {
                info = object_info().await;
            }
            // Without object info it is looked at again next time.
            let classes = info.as_ref().map(|i| partner_nodes(&e.graph, i));
            paid_new.push((e.prompt_id.clone(), classes));
        }
        let in_queue: HashSet<&str> = queue
            .running
            .iter()
            .chain(queue.pending.iter())
            .map(|e| e.prompt_id.as_str())
            .collect();
        let finished: Vec<&RemoteJob> = jobs
            .iter()
            .filter(|j| matches!(j.status.as_str(), "completed" | "failed" | "cancelled"))
            .collect();
        let mut drafts = Vec::new();
        let mut lookups: Vec<&RemoteJob> = Vec::new();
        let changed;
        {
            let mut s = self.lock();
            for (id, classes) in paid_new {
                match classes {
                    Some(c) if !c.is_empty() => {
                        s.known.insert(id.clone());
                        s.paid.insert(id, Paid { classes: c });
                    }
                    Some(_) => {
                        s.known.insert(id);
                    }
                    None => {}
                }
            }
            let first = !s.polled;
            s.polled = true;
            for j in &finished {
                if let Some(p) = s.paid.remove(&j.id) {
                    let (status, latency) = ended(j);
                    drafts.push(usage(&j.id, &p, status, latency, call_id()));
                    s.known.insert(j.id.clone());
                } else if first || suite.prompt_ids.contains(&j.id) {
                    s.known.insert(j.id.clone());
                } else if !s.known.contains(&j.id) {
                    lookups.push(j);
                }
            }
            // Paid prompts gone from both lists: ComfyUI lost them (a restart).
            let listed: HashSet<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
            let vanished: Vec<String> = s
                .paid
                .keys()
                .filter(|id| !in_queue.contains(id.as_str()) && !listed.contains(id.as_str()))
                .cloned()
                .collect();
            for id in vanished {
                if let Some(p) = s.paid.remove(&id) {
                    drafts.push(usage(&id, &p, UsageStatus::Pending, None, call_id()));
                }
            }
            changed = (s.running, s.queued) != (running, queued);
            s.running = running;
            s.queued = queued;
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
            let workflow = &detail["workflow"];
            if suite.owns(&j.id, &workflow["extra_data"]) {
                self.lock().known.insert(j.id.clone());
                continue;
            }
            if info.is_none() {
                info = object_info().await;
            }
            let Some(i) = &info else {
                continue;
            };
            let classes = partner_nodes(&workflow["prompt"], i);
            self.lock().known.insert(j.id.clone());
            if !classes.is_empty() {
                let (status, latency) = ended(j);
                drafts.push(usage(&j.id, &Paid { classes }, status, latency, call_id()));
            }
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

/// The usage status and latency of a finished `/api/jobs` entry.
fn ended(j: &RemoteJob) -> (UsageStatus, Option<u64>) {
    let status = match j.status.as_str() {
        "completed" => UsageStatus::Succeeded,
        "cancelled" => UsageStatus::Cancelled,
        _ => UsageStatus::Failed,
    };
    let time = |k: &str| j.raw.get(k).and_then(Value::as_u64);
    let latency = match (time("execution_start_time"), time("execution_end_time")) {
        (Some(start), Some(end)) if end >= start => Some(end - start),
        _ => None,
    };
    (status, latency)
}

/// The unattributed usage draft of the outside prompt `prompt_id` (module docs).
fn usage(
    prompt_id: &str,
    paid: &Paid,
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
        raw_usage: Some(json!({"prompt_id": prompt_id, "partner_nodes": paid.classes})),
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
    (draft, ctx)
}
