//! The Jobs panel's live rows (APP-114, APP-139, CORE-160, CORE-161; Appendix G "Queue
//! position"): the running and queued jobs, in the order they will run, each with its place,
//! its ETA and its elapsed time worked out here, Qt-free, so the panel's GUI thread only looks
//! them up. The bridge's `LiveJobsModel` (`src/qt/live_jobs_model.rs`) is a list model over it.
//!
//! - **Sources.** A read is a `jobs.list {status: "running"}` page and every page of
//!   `jobs.list {order: "queue"}` ([`LiveJobs::add_queue_page`] follows `next_cursor`, at most
//!   [`MAX_PAGES`] pages, dropping a job a page lists again), committed at once
//!   ([`LiveJobs::commit`]). Between reads each `jobs` event's job is applied as the full state it
//!   is ([`LiveJobs::apply_job`]): a job that ended leaves, a running or queued job (or one of a
//!   status this build doesn't know, P4a-R40) is added or updated; an event that comes while a
//!   read is out wins over that read when it is as new.
//! - **Places** come only from the last queue read's entries, never from a job's own
//!   `queue_position`, which is the place when its summary was made: a job the read lists without
//!   one is held by a restart; one it doesn't list has no place yet.
//! - **Order:** the running jobs first, then the rest (queued, or a status this build doesn't
//!   know); within each, by lane (`gpu-heavy`, `cpu`, then any other by name), then the running
//!   ones oldest first and the rest in the read's order (jobs it doesn't list last, oldest
//!   first).
//! - **ETA** (Appendix G "Adding estimates"): a running job's time left from its `wall_s`; a queued
//!   job's from the time left of its lane's running job, the `wall_s` of each job listed ahead of
//!   it in its lane, one job with no estimate for each place ahead the read doesn't list (another
//!   scope's), and its own. A job a lane reason holds (`queue_paused`) has no finish time while
//!   it is held, and an `interrupted_unknown` job neither an ETA nor a running clock.
//! - **Changes** come as [`Op`]s, ranges where they can, that a list model applies in order with
//!   its begin and end calls ([`apply`] is the same change on a plain list): a job starting is one
//!   move, not a reset, so a row keeps its delegate.

use std::collections::{HashMap, HashSet};

use lil_protocol::estimate::{Estimate, Spread};
use serde_json::Value;

use crate::format;

/// The queue read's page size.
pub const QUEUE_PAGE: u32 = 100;
/// The most pages one read follows.
pub const MAX_PAGES: u32 = 100;

/// One live row: the job, its place from the last queue read, and what it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveRow {
    pub id: String,
    pub job: Value,
    /// `None`: the last queue read didn't list it; `Some(None)`: listed with no place (a
    /// restart holds it); `Some(Some(n))`: its 0-based place in its lane.
    pub place: Option<Option<u32>>,
    pub eta_text: String,
    pub elapsed_text: String,
    /// The batch's jobs among the live rows.
    pub batch_size: u32,
}

/// A change to the shown list, applied in order. `Move`'s `to` is the row it goes before, in
/// the list as it is before the move (Qt's `beginMoveRows`).
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Remove { first: usize, last: usize },
    Insert { at: usize, rows: Vec<LiveRow> },
    Move { from: usize, to: usize },
    Update { at: usize, row: LiveRow },
}

/// Applies `op` to `shown`, as a list model does between its begin and end calls.
pub fn apply(shown: &mut Vec<LiveRow>, op: &Op) {
    match op {
        Op::Remove { first, last } => {
            shown.drain(*first..=*last);
        }
        Op::Insert { at, rows } => {
            shown.splice(*at..*at, rows.iter().cloned());
        }
        Op::Move { from, to } => {
            let row = shown.remove(*from);
            let at = if to > from { to - 1 } else { *to };
            shown.insert(at, row);
        }
        Op::Update { at, row } => shown[*at] = row.clone(),
    }
}

#[derive(Debug, Default)]
struct Read {
    queue: Vec<Value>,
    seen: HashSet<String>,
    running: Vec<Value>,
    events: HashMap<String, Value>,
    lanes: Option<Value>,
    pages: u32,
}

/// The live rows (module docs).
#[derive(Debug, Default)]
pub struct LiveJobs {
    rows: Vec<LiveRow>,
    jobs: HashMap<String, Value>,
    rank: HashMap<String, usize>,
    places: HashMap<String, Option<u32>>,
    read: Option<Read>,
    lanes: Vec<Value>,
}

fn text(job: &Value, key: &str) -> String {
    job.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn job_id(job: &Value) -> Option<String> {
    job.get("job_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn ended(job: &Value) -> bool {
    matches!(
        job.get("status").and_then(Value::as_str),
        Some("succeeded" | "failed" | "cancelled")
    )
}

/// `new` is at least as new as `old` (by `updated`): each event is the whole state.
fn as_new(new: &Value, old: Option<&Value>) -> bool {
    old.is_none_or(|old| text(new, "updated") >= text(old, "updated"))
}

fn structured(result: &Value) -> &Value {
    result.get("structured").unwrap_or(result)
}

fn estimate(job: &Value) -> Estimate {
    job.get("estimate")
        .and_then(|e| serde_json::from_value(e.clone()).ok())
        .unwrap_or_default()
}

fn wall(job: &Value) -> Option<Spread> {
    estimate(job).wall_s.as_ref()?.spread()
}

/// The seconds the job's current run has executed: from its newest `running`/`executing` step,
/// else from `started`.
fn elapsed_s(job: &Value, now_ms: i64) -> f64 {
    let step = job
        .get("status_seq")
        .and_then(Value::as_array)
        .and_then(|seq| {
            seq.iter().rev().find(|s| {
                s.get("status").and_then(Value::as_str) == Some("running")
                    && s.get("substatus").and_then(Value::as_str) == Some("executing")
            })
        })
        .and_then(|s| s.get("at").and_then(Value::as_str))
        .map(str::to_string);
    let since = step
        .or_else(|| {
            job.get("started")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .and_then(|at| format::parse_utc_ms(&at));
    since.map_or(0.0, |ms| ((now_ms - ms) as f64 / 1000.0).max(0.0))
}

fn lane_rank(lane: &str) -> u8 {
    match lane {
        "gpu-heavy" => 0,
        "cpu" => 1,
        _ => 2,
    }
}

impl LiveJobs {
    pub fn new() -> LiveJobs {
        LiveJobs::default()
    }

    /// The rows, in order.
    pub fn rows(&self) -> &[LiveRow] {
        &self.rows
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    /// The lanes' `QueueState`s from the last committed read.
    pub fn lanes(&self) -> &[Value] {
        &self.lanes
    }

    /// The lanes the rows are in, `gpu-heavy` and `cpu` first.
    pub fn row_lanes(&self) -> Vec<String> {
        let mut lanes: Vec<String> = Vec::new();
        for r in &self.rows {
            let lane = text(&r.job, "lane");
            if !lane.is_empty() && !lanes.contains(&lane) {
                lanes.push(lane);
            }
        }
        lanes.sort_by_key(|l| (lane_rank(l), l.clone()));
        lanes
    }

    /// Starts a read: its pages and its running jobs come next, then [`LiveJobs::commit`].
    pub fn begin_read(&mut self) {
        self.read = Some(Read::default());
    }

    /// One page of `jobs.list {order: "queue"}` (the call's result): the cursor of the next page
    /// to ask for, or `None` once the read has them all (or [`MAX_PAGES`] of them).
    pub fn add_queue_page(&mut self, result: &Value) -> Option<String> {
        let read = self.read.as_mut()?;
        read.pages += 1;
        let page = structured(result);
        if read.lanes.is_none() {
            read.lanes = page.get("lanes").cloned();
        }
        for job in page
            .get("jobs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = job_id(job)
                && read.seen.insert(id)
            {
                read.queue.push(job.clone());
            }
        }
        page.get("next_cursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty() && read.pages < MAX_PAGES)
            .map(str::to_string)
    }

    /// The read's `jobs.list {status: "running"}` result.
    pub fn set_running(&mut self, result: &Value) {
        if let Some(read) = self.read.as_mut() {
            read.running = structured(result)
                .get("jobs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
        }
    }

    /// The read is in: the rows become what it says, with the events that came meanwhile.
    pub fn commit(&mut self, now_ms: i64) -> Vec<Op> {
        let Some(read) = self.read.take() else {
            return Vec::new();
        };
        let mut jobs: HashMap<String, Value> = HashMap::new();
        for job in read.running.iter().chain(read.queue.iter()) {
            if let Some(id) = job_id(job)
                && !ended(job)
                && as_new(job, jobs.get(&id))
            {
                jobs.insert(id, job.clone());
            }
        }
        for (id, job) in read.events {
            if ended(&job) {
                jobs.remove(&id);
            } else if as_new(&job, jobs.get(&id)) {
                jobs.insert(id, job);
            }
        }
        self.rank.clear();
        self.places.clear();
        for (i, job) in read.queue.iter().enumerate() {
            if let Some(id) = job_id(job) {
                self.rank.insert(id.clone(), i);
                let place = job
                    .get("queue_position")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok());
                self.places.insert(id, place);
            }
        }
        if let Some(Value::Array(lanes)) = read.lanes {
            self.lanes = lanes;
        }
        self.jobs = jobs;
        self.refresh(now_ms)
    }

    /// A `jobs` event's job (module docs).
    pub fn apply_job(&mut self, job: Value, now_ms: i64) -> Vec<Op> {
        let Some(id) = job_id(&job) else {
            return Vec::new();
        };
        if let Some(read) = self.read.as_mut() {
            read.events.insert(id.clone(), job.clone());
        }
        if ended(&job) {
            if self.jobs.remove(&id).is_none() {
                return Vec::new();
            }
        } else if as_new(&job, self.jobs.get(&id)) {
            self.jobs.insert(id, job);
        } else {
            return Vec::new();
        }
        self.refresh(now_ms)
    }

    /// Whether `id` is a live row.
    pub fn contains(&self, id: &str) -> bool {
        self.jobs.contains_key(id)
    }

    /// The clock moved on: the rows whose times read differently.
    pub fn tick(&mut self, now_ms: i64) -> Vec<Op> {
        let mut rows = self.rows.clone();
        derive(&mut rows, now_ms);
        let ops = rows
            .iter()
            .enumerate()
            .filter(|(i, r)| self.rows[*i] != **r)
            .map(|(at, row)| Op::Update {
                at,
                row: row.clone(),
            })
            .collect();
        self.rows = rows;
        ops
    }

    fn refresh(&mut self, now_ms: i64) -> Vec<Op> {
        let mut rows: Vec<LiveRow> = self
            .jobs
            .iter()
            .map(|(id, job)| LiveRow {
                id: id.clone(),
                job: job.clone(),
                place: self.places.get(id).copied(),
                eta_text: String::new(),
                elapsed_text: String::new(),
                batch_size: 0,
            })
            .collect();
        let rank = &self.rank;
        rows.sort_by_cached_key(|r| {
            let running = text(&r.job, "status") == "running";
            let lane = text(&r.job, "lane");
            (
                u8::from(!running),
                lane_rank(&lane),
                lane,
                if running {
                    0
                } else {
                    rank.get(&r.id).copied().unwrap_or(usize::MAX)
                },
                text(&r.job, "created"),
                r.id.clone(),
            )
        });
        derive(&mut rows, now_ms);
        let ops = diff(&self.rows, &rows);
        self.rows = rows;
        ops
    }
}

/// Works out each row's ETA, elapsed time and batch size, the rows being in order.
fn derive(rows: &mut [LiveRow], now_ms: i64) {
    let mut batches: HashMap<String, u32> = HashMap::new();
    for r in rows.iter() {
        *batches.entry(text(&r.job, "batch_id")).or_default() += 1;
    }
    // Each lane's running job: its time left, or `None` without a `wall_s`.
    let mut running: HashMap<String, Option<f64>> = HashMap::new();
    for r in rows.iter() {
        if text(&r.job, "status") == "running" && text(&r.job, "substatus") != "interrupted_unknown"
        {
            let lane = text(&r.job, "lane");
            running.entry(lane).or_insert_with(|| {
                format::running_left(&estimate(&r.job), elapsed_s(&r.job, now_ms))
            });
        }
    }
    // The `wall_s` of the jobs listed so far in each lane, in order.
    let mut ahead: HashMap<String, Vec<Option<Spread>>> = HashMap::new();
    for r in rows.iter_mut() {
        let status = text(&r.job, "status");
        let substatus = text(&r.job, "substatus");
        r.batch_size = batches.get(&text(&r.job, "batch_id")).copied().unwrap_or(1);
        r.eta_text.clear();
        r.elapsed_text.clear();
        match status.as_str() {
            "running" if substatus != "interrupted_unknown" => {
                let elapsed = elapsed_s(&r.job, now_ms);
                r.elapsed_text = format::elapsed_text(elapsed);
                let eta = format::eta_text(&estimate(&r.job), elapsed);
                if eta != format::NO_TIME {
                    r.eta_text = eta;
                }
            }
            "queued" => {
                let Some(Some(place)) = r.place else {
                    continue;
                };
                let lane = text(&r.job, "lane");
                let own = wall(&r.job);
                let listed = ahead.entry(lane.clone()).or_default();
                if substatus != "queue_paused" {
                    let mut before: Vec<Option<&Spread>> = Vec::with_capacity(listed.len() + 1);
                    let left = match running.get(&lane) {
                        Some(Some(left)) => Some(*left),
                        Some(None) => {
                            before.push(None);
                            None
                        }
                        None => None,
                    };
                    before.extend(listed.iter().map(Option::as_ref));
                    let unseen = (place as usize).saturating_sub(listed.len());
                    before.extend(std::iter::repeat_n(None, unseen));
                    r.eta_text = format::queued_eta_text(left, &before, own.as_ref());
                }
                listed.push(own);
            }
            _ => {}
        }
    }
}

/// The changes that turn `old` into `new` (module docs).
fn diff(old: &[LiveRow], new: &[LiveRow]) -> Vec<Op> {
    let mut ops = Vec::new();
    let keep: HashSet<&str> = new.iter().map(|r| r.id.as_str()).collect();
    let mut cur: Vec<&str> = old.iter().map(|r| r.id.as_str()).collect();
    let mut i = cur.len();
    while i > 0 {
        i -= 1;
        if keep.contains(cur[i]) {
            continue;
        }
        let last = i;
        while i > 0 && !keep.contains(cur[i - 1]) {
            i -= 1;
        }
        ops.push(Op::Remove { first: i, last });
        cur.drain(i..=last);
    }
    let had: HashSet<&str> = cur.iter().copied().collect();
    let target: HashMap<&str, usize> = new
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let mut i = 0;
    while i < new.len() {
        let id = new[i].id.as_str();
        if cur.get(i) == Some(&id) {
            i += 1;
            continue;
        }
        if !had.contains(id) {
            let mut k = i;
            while k < new.len() && !had.contains(new[k].id.as_str()) {
                k += 1;
            }
            ops.push(Op::Insert {
                at: i,
                rows: new[i..k].to_vec(),
            });
            cur.splice(i..i, new[i..k].iter().map(|r| r.id.as_str()));
            i = k;
            continue;
        }
        // The row here belongs further down and the next one is the one wanted: move this one
        // down after the row that comes before it in `new` (one move for "Run last").
        let here = cur[i];
        if cur.get(i + 1) == Some(&id) {
            let before = new[target[here] - 1].id.as_str();
            if let Some(p) = cur.iter().position(|c| *c == before) {
                ops.push(Op::Move { from: i, to: p + 1 });
                let row = cur.remove(i);
                cur.insert(p, row);
                continue;
            }
        }
        let j = cur
            .iter()
            .position(|c| *c == id)
            .expect("a row kept is in the list");
        ops.push(Op::Move { from: j, to: i });
        let row = cur.remove(j);
        cur.insert(i, row);
        i += 1;
    }
    let before: HashMap<&str, &LiveRow> = old.iter().map(|r| (r.id.as_str(), r)).collect();
    for (at, row) in new.iter().enumerate() {
        if before.get(row.id.as_str()).is_some_and(|b| *b != row) {
            ops.push(Op::Update {
                at,
                row: row.clone(),
            });
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_793_239_205_000; // 2026-10-29T02:00:05Z

    fn at(secs_ago: i64) -> String {
        lil_schema::time::rfc3339(NOW / 1000 - secs_ago)
    }

    fn wall(p50: f64) -> Value {
        json!({"wall_s": {"source": "measured_history", "samples": 3, "p50": format!("{p50:.1}"),
                          "p90": format!("{:.1}", p50 * 1.5)}, "unknown": []})
    }

    fn job(id: &str, status: &str, substatus: &str, lane: &str) -> Value {
        json!({"job_id": id, "kind": "comfy.prompt", "lane": lane, "priority": "user",
               "status": status, "substatus": substatus, "created": at(100), "updated": at(10),
               "attempt": 1, "trace_id": "t", "batch_id": format!("b_{id}"), "index": 0})
    }

    fn queued(id: &str, place: Option<u32>, est: Option<f64>) -> Value {
        let mut j = job(id, "queued", "waiting", "gpu-heavy");
        if let Some(p) = place {
            j["queue_position"] = json!(p);
        }
        if let Some(e) = est {
            j["estimate"] = wall(e);
        }
        j
    }

    fn running(id: &str, lane: &str, est: Option<f64>, started_ago: i64) -> Value {
        let mut j = job(id, "running", "executing", lane);
        j["started"] = json!(at(started_ago));
        j["status_seq"] =
            json!([{"status": "running", "substatus": "executing", "at": at(started_ago)}]);
        if let Some(e) = est {
            j["estimate"] = wall(e);
        }
        j
    }

    fn page(jobs: &[Value], next: Option<&str>) -> Value {
        json!({"structured": {"jobs": jobs, "next_cursor": next,
                              "lanes": [{"lane": "gpu-heavy", "paused": false, "reasons": [], "held": 0,
                                         "restart_held": 0, "withdrawn": 0}]}})
    }

    fn read(live: &mut LiveJobs, run: &[Value], pages: &[Vec<Value>]) -> Vec<Op> {
        live.begin_read();
        live.set_running(&page(run, None));
        for (i, p) in pages.iter().enumerate() {
            let next = (i + 1 < pages.len()).then(|| format!("c{i}"));
            let cursor = live.add_queue_page(&page(p, next.as_deref()));
            assert_eq!(cursor, next);
        }
        live.commit(NOW)
    }

    fn ids(live: &LiveJobs) -> Vec<&str> {
        live.rows().iter().map(|r| r.id.as_str()).collect()
    }

    fn row<'a>(live: &'a LiveJobs, id: &str) -> &'a LiveRow {
        &live.rows()[live.index_of(id).unwrap()]
    }

    /// Applying the ops to the old rows gives the new ones.
    fn replay(before: &[LiveRow], ops: &[Op], after: &[LiveRow]) {
        let mut shown = before.to_vec();
        for op in ops {
            apply(&mut shown, op);
        }
        assert_eq!(shown, after);
    }

    #[test] // spec: APP-114, CORE-160
    fn a_queue_longer_than_a_page_keeps_its_front_and_its_running_job() {
        let mut live = LiveJobs::new();
        let all: Vec<Value> = (0..250)
            .map(|i| queued(&format!("q{i:03}"), Some(i), Some(10.0)))
            .collect();
        let mut cpu = queued("c0", Some(0), None);
        cpu["lane"] = json!("cpu");
        let pages = vec![all[..100].to_vec(), all[100..200].to_vec(), {
            let mut last = all[200..].to_vec();
            // A job a page lists again is dropped.
            last.push(all[150].clone());
            last.push(cpu);
            last
        }];
        let ops = read(
            &mut live,
            &[running("r1", "gpu-heavy", Some(30.0), 10)],
            &pages,
        );
        replay(&[], &ops, live.rows());
        assert_eq!(live.rows().len(), 252);
        assert_eq!(&ids(&live)[..3], ["r1", "q000", "q001"]);
        assert_eq!(
            *ids(&live).last().unwrap(),
            "c0",
            "the CPU lane after the GPU lane"
        );
        assert_eq!(row(&live, "q000").place, Some(Some(0)));
        assert_eq!(row(&live, "q249").place, Some(Some(249)));
        // 20 s left of r1, then 10 s each.
        assert_eq!(row(&live, "q000").eta_text, "about 30 s");
        assert_eq!(row(&live, "q005").eta_text, "about 1 min");
        assert_eq!(row(&live, "r1").elapsed_text, "10 s");
        assert_eq!(row(&live, "r1").eta_text, "about 20 s left");
        assert_eq!(
            row(&live, "c0").eta_text,
            format::NO_ESTIMATE,
            "first in an idle lane, unestimated"
        );
    }

    #[test] // spec: APP-114, CORE-160
    fn places_come_from_the_read_never_from_a_summary() {
        let mut live = LiveJobs::new();
        read(
            &mut live,
            &[],
            &[vec![
                queued("a", Some(0), Some(10.0)),
                queued("b", Some(1), Some(10.0)),
            ]],
        );
        assert_eq!(
            row(&live, "b").eta_text,
            "about 20 s",
            "no phantom jobs ahead"
        );
        // An event of b with its old summary place changes nothing about its place.
        let mut stale = queued("b", Some(7), Some(10.0));
        stale["updated"] = json!(at(0));
        live.apply_job(stale, NOW);
        assert_eq!(row(&live, "b").place, Some(Some(1)));
        assert_eq!(row(&live, "b").eta_text, "about 20 s");
        // A place ahead the read doesn't list is another scope's job: one with no estimate.
        let mut live = LiveJobs::new();
        read(&mut live, &[], &[vec![queued("x", Some(2), Some(10.0))]]);
        assert_eq!(
            row(&live, "x").eta_text,
            "about 10 s, after 2 jobs with no estimate"
        );
        // Listed with no place: a restart holds it, and it counts ahead of nothing.
        let mut held = queued("h", None, Some(10.0));
        held["substatus"] = json!("queue_paused");
        held["pause_reasons"] = json!(["restart"]);
        let mut live = LiveJobs::new();
        read(
            &mut live,
            &[],
            &[vec![queued("y", Some(0), Some(10.0)), held]],
        );
        assert_eq!(row(&live, "h").place, Some(None));
        assert_eq!(row(&live, "h").eta_text, "");
    }

    #[test] // spec: APP-114, CORE-160
    fn running_jobs_come_first_in_every_lane_and_unknown_statuses_with_the_queue() {
        let mut live = LiveJobs::new();
        let mut odd = job("odd", "paused_by_director", "planning", "gpu-heavy");
        odd["created"] = json!(at(500));
        let net = job("net", "queued", "waiting", "network");
        read(
            &mut live,
            &[
                running("cpu1", "cpu", None, 5),
                running("g1", "gpu-heavy", None, 5),
            ],
            &[vec![queued("q1", Some(0), None)]],
        );
        live.apply_job(odd, NOW);
        live.apply_job(net, NOW);
        assert_eq!(ids(&live), ["g1", "cpu1", "q1", "odd", "net"]);
        assert_eq!(live.row_lanes(), ["gpu-heavy", "cpu", "network"]);
        // A job that ends leaves.
        let mut done = running("g1", "gpu-heavy", None, 5);
        done["status"] = json!("succeeded");
        done["updated"] = json!(at(0));
        let before = live.rows().to_vec();
        let ops = live.apply_job(done, NOW);
        replay(&before, &ops, live.rows());
        assert_eq!(ids(&live), ["cpu1", "q1", "odd", "net"]);
    }

    #[test] // spec: APP-114
    fn held_and_unknown_jobs_have_no_clock() {
        let mut live = LiveJobs::new();
        let mut unknown = running("u", "gpu-heavy", Some(10.0), 7200);
        unknown["substatus"] = json!("interrupted_unknown");
        let mut paused = queued("p", Some(0), Some(30.0));
        paused["substatus"] = json!("queue_paused");
        paused["pause_reasons"] = json!(["manual"]);
        read(
            &mut live,
            &[unknown],
            &[vec![paused, queued("n", Some(1), Some(30.0))]],
        );
        assert_eq!(
            (
                row(&live, "u").eta_text.as_str(),
                row(&live, "u").elapsed_text.as_str()
            ),
            ("", "")
        );
        assert_eq!(
            row(&live, "p").eta_text,
            "",
            "no finish time while your pause holds it"
        );
        assert_eq!(
            row(&live, "n").eta_text,
            "about 1 min",
            "but it still counts ahead"
        );
    }

    #[test] // spec: APP-114, APP-139
    fn a_job_starting_is_one_move_and_the_clock_updates_only_what_changed() {
        let mut live = LiveJobs::new();
        let q: Vec<Value> = (0..5)
            .map(|i| queued(&format!("q{i}"), Some(i), Some(100.0)))
            .collect();
        read(&mut live, &[], &[q]);
        let before = live.rows().to_vec();
        let mut started = running("q2", "gpu-heavy", Some(100.0), 0);
        started["updated"] = json!(at(0));
        let ops = live.apply_job(started, NOW);
        assert_eq!(
            ops.iter().filter(|o| matches!(o, Op::Move { .. })).count(),
            1,
            "{ops:?}"
        );
        assert!(
            !ops.iter()
                .any(|o| matches!(o, Op::Insert { .. } | Op::Remove { .. }))
        );
        replay(&before, &ops, live.rows());
        assert_eq!(ids(&live)[0], "q2");
        // One second on: the running row and the ETAs that read differently.
        let before = live.rows().to_vec();
        let ops = live.tick(NOW + 1000);
        assert!(ops.iter().all(|o| matches!(o, Op::Update { .. })));
        assert!(
            ops.iter().any(|o| matches!(o, Op::Update { at: 0, .. })),
            "the running row"
        );
        replay(&before, &ops, live.rows());
        assert!(
            live.tick(NOW + 1000).is_empty(),
            "nothing changed, nothing sent"
        );
    }

    #[test] // spec: APP-114
    fn the_changes_replay_to_the_rows_for_any_reorder() {
        // A small deterministic generator.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for round in 0..300 {
            let n = 1 + next(30);
            let mut pool: Vec<usize> = (0..40).collect();
            let mut order = Vec::new();
            for _ in 0..n {
                order.push(pool.remove(next(pool.len())));
            }
            let mut live = LiveJobs::new();
            let first: Vec<Value> = order
                .iter()
                .enumerate()
                .map(|(i, id)| queued(&format!("j{id}"), Some(i as u32), Some(5.0)))
                .collect();
            read(&mut live, &[], &[first]);
            // Some leave, some come, the rest reorder.
            let mut second: Vec<usize> = order.iter().copied().filter(|_| next(4) != 0).collect();
            for _ in 0..next(5) {
                second.insert(next(second.len() + 1), pool.remove(next(pool.len())));
            }
            for _ in 0..next(6) {
                let len = second.len();
                if len > 1 {
                    let x = second.remove(next(len));
                    second.insert(next(len), x);
                }
            }
            let jobs: Vec<Value> = second
                .iter()
                .enumerate()
                .map(|(i, id)| queued(&format!("j{id}"), Some(i as u32), Some(5.0)))
                .collect();
            let before = live.rows().to_vec();
            let ops = read(&mut live, &[], &[jobs]);
            replay(&before, &ops, live.rows());
            let want: Vec<String> = second.iter().map(|id| format!("j{id}")).collect();
            assert_eq!(ids(&live), want, "round {round}");
        }
        // "Run last" on the first of many is one move.
        let mut live = LiveJobs::new();
        let q: Vec<Value> = (0..50)
            .map(|i| queued(&format!("q{i:02}"), Some(i), None))
            .collect();
        read(&mut live, &[], std::slice::from_ref(&q));
        let mut moved = q[1..].to_vec();
        moved.push(q[0].clone());
        let ops = read(&mut live, &[], &[moved]);
        assert_eq!(
            ops.iter().filter(|o| matches!(o, Op::Move { .. })).count(),
            1,
            "{:?}",
            ops.iter()
                .filter(|o| matches!(o, Op::Move { .. }))
                .collect::<Vec<_>>()
        );
    }

    #[test] // spec: APP-114
    fn an_event_during_a_read_wins_when_as_new() {
        let mut live = LiveJobs::new();
        read(&mut live, &[], &[vec![queued("a", Some(0), None)]]);
        live.begin_read();
        let mut done = queued("a", Some(0), None);
        done["status"] = json!("cancelled");
        done["updated"] = json!(at(0));
        live.apply_job(done, NOW);
        // The read was made before the cancel and still lists it.
        live.set_running(&page(&[], None));
        assert_eq!(
            live.add_queue_page(&page(&[queued("a", Some(0), None)], None)),
            None
        );
        live.commit(NOW);
        assert!(live.rows().is_empty(), "the cancel stands");
    }
}
