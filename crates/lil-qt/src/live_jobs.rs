//! The Jobs panel's live rows (APP-114, APP-139, CORE-160, CORE-161; Appendix G "Queue
//! position"): the running and queued jobs, in the order they will run, each with its place,
//! its ETA and its elapsed time worked out here, Qt-free, so the panel's GUI thread only looks
//! them up. The bridge's `LiveJobsModel` (`src/qt/live_jobs_model.rs`) is a list model over it.
//!
//! - **Sources.** A read is every page of `jobs.list {status: "running"}`, of `jobs.list {order:
//!   "queue"}` and of `jobs.list {status: "queued"}` ([`LiveJobs::add_running_page`],
//!   [`LiveJobs::add_queue_page`], [`LiveJobs::add_queued_page`]: each says the next cursor to
//!   follow, at most [`MAX_PAGES`] pages, and a job listed again counts once), committed at once
//!   ([`LiveJobs::commit`]). The third is for the jobs the core has dispatched: they stay `queued`
//!   in `submitting` or `queued_in_engine` until they execute, and the queue order leaves them out
//!   (`Active::queued`), so only that read lists them; of its pages only the jobs that hold no
//!   place in a queue are taken. A status read cut at [`MAX_PAGES`] can't say that a running or
//!   dispatched job has gone, so those rows stay. Between reads each `jobs` event's job is applied
//!   as the full state it is ([`LiveJobs::apply_jobs`], any number of them in one pass, [`Op`]s
//!   for the whole): a job that ended leaves, a running or queued job (or one of a status this
//!   build doesn't know, P4a-R40) is added or updated; an event that comes while a read is out
//!   wins over that read when it is as new. A job of a status this build doesn't know is in no
//!   listing, so a read keeps it until an event ends it; a read after a snapshot or a
//!   `resync_required` ([`LiveJobs::begin_fresh_read`]) keeps only the jobs the core still lists,
//!   the reloaded history handing those back.
//! - **Places** come only from the last queue read's entries, never from a job's own
//!   `queue_position`, which is the place when its summary was made: a job the read lists without
//!   one is held by a restart; one it doesn't list has no place yet; a dispatched job has none,
//!   whatever an earlier read said, and the queue's places don't count it.
//! - **Order:** the running jobs first, then the dispatched ones, then the rest (queued, or a
//!   status this build doesn't know); within each, by lane (`gpu-heavy`, `cpu`, then any other by
//!   name), then the running ones oldest first and the rest in the read's order (jobs it doesn't
//!   list last, oldest first).
//! - **ETA** (Appendix G "Adding estimates"): a running job's time left from its `wall_s`; a queued
//!   job's from the time left of its lane's running job, the `wall_s` of its lane's dispatched job
//!   (it has run none of it yet) and of each job listed ahead of it in its lane, one job with no
//!   estimate for each place ahead the read doesn't list (another scope's) and for the earlier
//!   prompt a dispatched retry waits on (it has no estimate here), and its own. A job a lane reason
//!   holds (`queue_paused`) has no finish time while it is held, and an `interrupted_unknown` job
//!   neither an ETA nor a running clock.
//! - **Changes** come as [`Op`]s, ranges where they can, that a list model applies in order with
//!   its begin and end calls ([`apply`] is the same change on a plain list): a job starting is one
//!   move, not a reset, so a row keeps its delegate.
//! - **A read cut at [`MAX_PAGES`]** lists that many pages and says so ([`LiveJobs::capped`]): a
//!   guard against a cursor that never ends.
//! - **Cost:** every read, pass of events and tick sorts, works out and compares all the live rows,
//!   so it grows linearly with them: about 0.6 to 0.8 ms at 300, and about 15 to 18 ms at 3,000,
//!   past a frame. The panel applies the events of a frame in one pass, so a burst costs one of
//!   these, not one for each event; its bench (`bench_jobs_panel`) holds the 300-job case and a
//!   burst of 300 events over 3,000 jobs to their bounds.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lil_protocol::estimate::Spread;
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
    /// Shared with the live jobs: a row copies a pointer, never the job.
    pub job: Arc<Value>,
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

/// What `jobs` events mean to the panel, the most of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobChange {
    /// None of them is or was a live row.
    None,
    /// A live row changed, its place in the queue as it was (a running job's update).
    Shown,
    /// The queue may have changed (Appendix G "Queue position"): a job is queued now or was shown
    /// as queued, or a job came, went or changed status. Read it again, coalesced.
    ReadAgain,
}

/// What [`LiveJobs::apply_jobs`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub ops: Vec<Op>,
    pub change: JobChange,
}

#[derive(Debug, Default)]
struct Read {
    queue: Vec<Value>,
    seen: HashSet<String>,
    running: Vec<Value>,
    /// The `{status: "queued"}` pages' jobs that hold no place in a queue.
    dispatched: Vec<Value>,
    events: HashMap<String, Value>,
    lanes: Option<Value>,
    queue_pages: u32,
    running_pages: u32,
    queued_pages: u32,
    /// A read after a resync: nothing carries over ([`LiveJobs::begin_fresh_read`]).
    fresh: bool,
    /// The queue read stopped at [`MAX_PAGES`] with a cursor still to follow.
    capped: bool,
    /// A status read did.
    status_cut: bool,
}

/// The live rows (module docs).
#[derive(Debug, Default)]
pub struct LiveJobs {
    rows: Vec<LiveRow>,
    jobs: HashMap<String, Arc<Value>>,
    rank: HashMap<String, usize>,
    places: HashMap<String, Option<u32>>,
    read: Option<Read>,
    lanes: Vec<Value>,
    capped: Option<usize>,
}

fn text<'a>(job: &'a Value, key: &str) -> &'a str {
    job.get(key).and_then(Value::as_str).unwrap_or_default()
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

/// The core has dispatched it (module docs): `queued`, on its way to the engine or waiting there.
fn dispatched(job: &Value) -> bool {
    text(job, "status") == "queued"
        && matches!(text(job, "substatus"), "submitting" | "queued_in_engine")
}

/// A substatus of a job the core holds in a queue, which the queue read lists (Appendix G "Queue
/// position").
fn holds_a_place(job: &Value) -> bool {
    matches!(
        text(job, "substatus"),
        "waiting" | "waiting_for_vram" | "queue_paused"
    )
}

/// The page's next cursor, or `None` at its end or at [`MAX_PAGES`] (`cut` then says so).
fn next_cursor(page: &Value, pages: &mut u32, cut: &mut bool) -> Option<String> {
    *pages += 1;
    let next = page
        .get("next_cursor")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty());
    if next.is_some() && *pages >= MAX_PAGES {
        *cut = true;
        return None;
    }
    next.map(str::to_string)
}

fn jobs_of(page: &Value) -> impl Iterator<Item = &Value> {
    page.get("jobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// `new` is at least as new as `old` (by `updated`): each event is the whole state.
fn as_new(new: &Value, old: Option<&Value>) -> bool {
    old.is_none_or(|old| text(new, "updated") >= text(old, "updated"))
}

fn structured(result: &Value) -> &Value {
    result.get("structured").unwrap_or(result)
}

/// The job's `estimate.wall_s` as a [`Spread`], read in place.
fn wall(job: &Value) -> Option<Spread> {
    let w = job.get("estimate")?.get("wall_s")?;
    let figure = |key: &str| w.get(key)?.as_str()?.parse::<f64>().ok();
    Spread::new(figure("p50")?, figure("p90")?)
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
        .and_then(|s| s.get("at").and_then(Value::as_str));
    let since = step
        .or_else(|| job.get("started").and_then(Value::as_str))
        .and_then(format::parse_utc_ms);
    since.map_or(0.0, |ms| ((now_ms - ms) as f64 / 1000.0).max(0.0))
}

/// A row's place in the order (module docs): running first, then dispatched, then by lane, then
/// the running ones oldest first and the rest by the read's order.
fn order_key<'a>(
    rank: &HashMap<String, usize>,
    id: &str,
    job: &'a Value,
) -> (u8, u8, &'a str, usize, &'a str) {
    let running = text(job, "status") == "running";
    let lane = text(job, "lane");
    let place = if running {
        0
    } else {
        rank.get(id).copied().unwrap_or(usize::MAX)
    };
    let class = if running {
        0
    } else if dispatched(job) {
        1
    } else {
        2
    };
    (class, lane_rank(lane), lane, place, text(job, "created"))
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
            if !lane.is_empty() && !lanes.iter().any(|l| l == lane) {
                lanes.push(lane.to_string());
            }
        }
        lanes.sort_by_key(|l| (lane_rank(l), l.clone()));
        lanes
    }

    /// Starts a read: its pages come next, then [`LiveJobs::commit`].
    pub fn begin_read(&mut self) {
        self.read = Some(Read::default());
    }

    /// A read after a snapshot or a `resync_required` (a core restart, a workspace switch): it
    /// carries nothing over, so a job the core no longer lists leaves; the reloaded history
    /// hands back, as events, the jobs of a status this build doesn't know that it still lists.
    pub fn begin_fresh_read(&mut self) {
        self.read = Some(Read {
            fresh: true,
            ..Read::default()
        });
    }

    /// One page of `jobs.list {order: "queue"}` (the call's result): the cursor of the next page
    /// to ask for, or `None` once the read has them all (or [`MAX_PAGES`] of them).
    pub fn add_queue_page(&mut self, result: &Value) -> Option<String> {
        let read = self.read.as_mut()?;
        let page = structured(result);
        if read.lanes.is_none() {
            read.lanes = page.get("lanes").cloned();
        }
        for job in jobs_of(page) {
            if let Some(id) = job_id(job)
                && read.seen.insert(id)
            {
                read.queue.push(job.clone());
            }
        }
        next_cursor(page, &mut read.queue_pages, &mut read.capped)
    }

    /// One page of `jobs.list {status: "running"}`: as [`LiveJobs::add_queue_page`].
    pub fn add_running_page(&mut self, result: &Value) -> Option<String> {
        let read = self.read.as_mut()?;
        let page = structured(result);
        read.running.extend(jobs_of(page).cloned());
        next_cursor(page, &mut read.running_pages, &mut read.status_cut)
    }

    /// One page of `jobs.list {status: "queued"}`: as [`LiveJobs::add_queue_page`], taking only
    /// the jobs that hold no place in a queue, which the queue read doesn't list.
    pub fn add_queued_page(&mut self, result: &Value) -> Option<String> {
        let read = self.read.as_mut()?;
        let page = structured(result);
        read.dispatched
            .extend(jobs_of(page).filter(|j| !holds_a_place(j)).cloned());
        next_cursor(page, &mut read.queued_pages, &mut read.status_cut)
    }

    /// The read is in: the rows become what it says, with the events that came meanwhile.
    pub fn commit(&mut self, now_ms: i64) -> Vec<Op> {
        let Some(read) = self.read.take() else {
            return Vec::new();
        };
        let mut jobs: HashMap<String, Arc<Value>> = HashMap::new();
        for job in read
            .dispatched
            .into_iter()
            .chain(read.running)
            .chain(read.queue.iter().cloned())
        {
            if let Some(id) = job_id(&job)
                && !ended(&job)
                && as_new(&job, jobs.get(&id).map(Arc::as_ref))
            {
                // A job the rows show as it is keeps its pointer: its row isn't sent again.
                let job = match self.jobs.get(&id) {
                    Some(held) if **held == job => held.clone(),
                    _ => Arc::new(job),
                };
                jobs.insert(id, job);
            }
        }
        // A job of a status this build doesn't know is in no listing: it stays until an event
        // ends it (P4a-R40), never dropped by a read, except a fresh one. So does a job a status
        // read cut at its cap may not have reached.
        for (id, job) in self.jobs.iter().filter(|_| !read.fresh) {
            let status = text(job, "status");
            let unknown = status != "queued" && status != "running";
            let unreached = read.status_cut && (status == "running" || dispatched(job));
            if (unknown || unreached) && !jobs.contains_key(id) {
                jobs.insert(id.clone(), job.clone());
            }
        }
        for (id, job) in read.events {
            if ended(&job) {
                jobs.remove(&id);
            } else if as_new(&job, jobs.get(&id).map(Arc::as_ref)) {
                jobs.insert(id, Arc::new(job));
            }
        }
        self.capped = read.capped.then_some(read.queue.len());
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

    /// `jobs` events' jobs, in the order they came, in one pass (module docs): one sort and one
    /// set of changes for them all.
    pub fn apply_jobs(&mut self, jobs: Vec<Value>, now_ms: i64) -> Applied {
        let mut change = JobChange::None;
        let mut touched = false;
        for job in jobs {
            let Some(id) = job_id(&job) else {
                continue;
            };
            let before = self.status_of(&id);
            if let Some(read) = self.read.as_mut() {
                read.events.insert(id.clone(), job.clone());
            }
            if ended(&job) {
                touched |= self.jobs.remove(&id).is_some();
            } else if as_new(&job, self.jobs.get(&id).map(Arc::as_ref)) {
                self.jobs.insert(id.clone(), Arc::new(job));
                touched = true;
            }
            let after = self.status_of(&id);
            let queued = |s: &Option<String>| s.as_deref() == Some("queued");
            change = change.max(if before.is_none() && after.is_none() {
                JobChange::None
            } else if queued(&before) || queued(&after) || before != after {
                JobChange::ReadAgain
            } else {
                JobChange::Shown
            });
        }
        let ops = if touched {
            self.refresh(now_ms)
        } else {
            Vec::new()
        };
        Applied { ops, change }
    }

    /// One `jobs` event's job: [`LiveJobs::apply_jobs`]' changes.
    pub fn apply_job(&mut self, job: Value, now_ms: i64) -> Vec<Op> {
        self.apply_jobs(vec![job], now_ms).ops
    }

    /// How many queued jobs the last read listed when it stopped at [`MAX_PAGES`] with more to
    /// come; `None` when it listed them all.
    pub fn capped(&self) -> Option<usize> {
        self.capped
    }

    /// Whether `id` is a live row.
    pub fn contains(&self, id: &str) -> bool {
        self.jobs.contains_key(id)
    }

    /// A live job's status, `None` for one that isn't live.
    pub fn status_of(&self, id: &str) -> Option<String> {
        self.jobs.get(id).map(|j| text(j, "status").to_string())
    }

    /// What the lane headers and the panel's own controls need: `{lanes}` (as
    /// [`LiveJobs::row_lanes`]), `{waiting_for_vram}` (the lanes with a queued job waiting for
    /// GPU memory), `{mine}` (the live jobs `app`'s panels started) and `{capped}` (as
    /// [`LiveJobs::capped`], or null).
    pub fn overview(&self, app: &str) -> Value {
        let mut vram: Vec<String> = Vec::new();
        let mut mine = 0_u32;
        for r in &self.rows {
            if text(&r.job, "substatus") == "waiting_for_vram" {
                let lane = text(&r.job, "lane");
                if !vram.iter().any(|l| l == lane) {
                    vram.push(lane.to_string());
                }
            }
            let origin = r.job.get("origin");
            let surface = origin
                .and_then(|o| o.get("surface"))
                .and_then(Value::as_str);
            let id = origin.and_then(|o| o.get("id")).and_then(Value::as_str);
            if surface == Some("panel") && id == Some(app) {
                mine += 1;
            }
        }
        serde_json::json!({"lanes": self.row_lanes(), "waiting_for_vram": vram, "mine": mine,
                           "capped": self.capped})
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
        let rank = &self.rank;
        let mut order: Vec<(&String, &Arc<Value>)> = self.jobs.iter().collect();
        order.sort_by(|a, b| {
            order_key(rank, a.0, a.1)
                .cmp(&order_key(rank, b.0, b.1))
                .then_with(|| a.0.cmp(b.0))
        });
        let mut rows: Vec<LiveRow> = order
            .into_iter()
            .map(|(id, job)| LiveRow {
                id: id.clone(),
                job: job.clone(),
                // A dispatched job has none, whatever the read before it said.
                place: self.places.get(id).copied().filter(|_| !dispatched(job)),
                eta_text: String::new(),
                elapsed_text: String::new(),
                batch_size: 0,
            })
            .collect();
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
        *batches
            .entry(text(&r.job, "batch_id").to_string())
            .or_default() += 1;
    }
    // Each lane's running job: its time left, or `None` without a `wall_s`.
    let mut running: HashMap<String, Option<f64>> = HashMap::new();
    for r in rows.iter() {
        if text(&r.job, "status") == "running" && text(&r.job, "substatus") != "interrupted_unknown"
        {
            running
                .entry(text(&r.job, "lane").to_string())
                .or_insert_with(|| {
                    wall(&r.job).map(|w| format::left_of(&w, elapsed_s(&r.job, now_ms)))
                });
        }
    }
    // The `wall_s` of the dispatched jobs so far in each lane, the earlier prompts they wait on
    // as jobs with no estimate.
    let mut sent: HashMap<String, Vec<Option<Spread>>> = HashMap::new();
    // The `wall_s` of the jobs listed so far in each lane, in order.
    let mut ahead: HashMap<String, Vec<Option<Spread>>> = HashMap::new();
    // The time left of `lane`'s running job, or one more job with no estimate in `before`.
    let running_left = |lane: &str, before: &mut Vec<Option<&Spread>>| match running.get(lane) {
        Some(Some(left)) => Some(*left),
        Some(None) => {
            before.push(None);
            None
        }
        None => None,
    };
    for r in rows.iter_mut() {
        let job = r.job.clone();
        let (status, substatus) = (text(&job, "status"), text(&job, "substatus"));
        r.batch_size = batches.get(text(&job, "batch_id")).copied().unwrap_or(1);
        r.eta_text.clear();
        r.elapsed_text.clear();
        match status {
            "running" if substatus != "interrupted_unknown" => {
                let elapsed = elapsed_s(&job, now_ms);
                r.elapsed_text = format::elapsed_text(elapsed);
                if let Some(w) = wall(&job) {
                    r.eta_text = format::left_text(Some(format::left_of(&w, elapsed)));
                }
            }
            "queued" if dispatched(&job) => {
                let lane = text(&job, "lane");
                let own = wall(&job);
                let lane_sent = sent.entry(lane.to_string()).or_default();
                if job
                    .get("waiting_for_earlier_prompt")
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    lane_sent.push(None);
                }
                let mut before: Vec<Option<&Spread>> = Vec::with_capacity(lane_sent.len() + 1);
                let left = running_left(lane, &mut before);
                before.extend(lane_sent.iter().map(Option::as_ref));
                r.eta_text = format::queued_eta_text(left, &before, own.as_ref());
                lane_sent.push(own);
            }
            "queued" => {
                let Some(Some(place)) = r.place else {
                    continue;
                };
                let lane = text(&job, "lane");
                let own = wall(&job);
                let listed = ahead.entry(lane.to_string()).or_default();
                if substatus != "queue_paused" {
                    let lane_sent = sent.get(lane).map_or(&[][..], Vec::as_slice);
                    let mut before: Vec<Option<&Spread>> =
                        Vec::with_capacity(lane_sent.len() + listed.len() + 1);
                    let left = running_left(lane, &mut before);
                    before.extend(lane_sent.iter().map(Option::as_ref));
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
        live.add_running_page(&page(run, None));
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
        let overview = live.overview("image");
        assert_eq!(overview["lanes"], json!(["gpu-heavy", "cpu", "network"]));
        assert_eq!(overview["waiting_for_vram"], json!([]));
        assert_eq!(
            overview["mine"],
            json!(0),
            "none of them came from the image app's panels"
        );
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
    fn a_job_of_an_unknown_status_stays_through_reads_until_it_ends() {
        let mut live = LiveJobs::new();
        read(&mut live, &[], &[vec![queued("q", Some(0), None)]]);
        let mut odd = job("odd", "paused_by_director", "waiting_quota", "gpu-heavy");
        live.apply_job(odd.clone(), NOW);
        // No read can list it: several reads keep it.
        for _ in 0..3 {
            read(&mut live, &[], &[vec![queued("q", Some(0), None)]]);
            assert_eq!(ids(&live), ["q", "odd"]);
        }
        // An event that ends it does.
        odd["status"] = json!("cancelled");
        odd["updated"] = json!(at(0));
        live.apply_job(odd, NOW);
        read(&mut live, &[], &[vec![queued("q", Some(0), None)]]);
        assert_eq!(ids(&live), ["q"]);
        // A queued or running job the reads no longer list goes (it ended or moved scope).
        read(&mut live, &[], &[vec![]]);
        assert!(live.rows().is_empty());
    }

    #[test] // spec: APP-114
    fn a_read_cut_at_the_page_cap_says_how_many_it_lists() {
        let mut live = LiveJobs::new();
        live.begin_read();
        live.add_running_page(&page(&[], None));
        for i in 0..MAX_PAGES {
            let next = live.add_queue_page(&page(
                &[queued(&format!("q{i:04}"), Some(i), None)],
                Some("more"),
            ));
            assert_eq!(next.is_some(), i + 1 < MAX_PAGES, "page {i}");
        }
        live.commit(NOW);
        assert_eq!(live.rows().len(), MAX_PAGES as usize);
        assert_eq!(live.capped(), Some(MAX_PAGES as usize));
        assert_eq!(live.overview("image")["capped"], json!(MAX_PAGES));
        // A read that ends on its own isn't cut.
        read(&mut live, &[], &[vec![queued("a", Some(0), None)]]);
        assert_eq!(live.capped(), None);
        assert_eq!(live.overview("image")["capped"], Value::Null);
    }

    #[test] // spec: APP-114
    fn a_fresh_read_keeps_only_what_the_core_still_lists() {
        let mut live = LiveJobs::new();
        read(&mut live, &[], &[vec![queued("q", Some(0), None)]]);
        live.apply_job(job("gone", "paused_by_director", "x", "gpu-heavy"), NOW);
        live.apply_job(job("kept", "paused_by_director", "x", "gpu-heavy"), NOW);
        assert_eq!(ids(&live), ["q", "gone", "kept"]);
        // A resync: the reloaded history lists only `kept`, handed over during the read.
        live.begin_fresh_read();
        live.apply_job(job("kept", "paused_by_director", "x", "gpu-heavy"), NOW);
        live.add_running_page(&page(&[], None));
        assert_eq!(
            live.add_queue_page(&page(&[queued("q", Some(0), None)], None)),
            None
        );
        live.commit(NOW);
        assert_eq!(
            ids(&live),
            ["q", "kept"],
            "the job the core no longer lists is gone"
        );
    }

    /// A job the core has dispatched (Appendix G "Queue position"): status `queued`, no place.
    fn dispatched_job(id: &str, substatus: &str, est: Option<f64>) -> Value {
        let mut j = job(id, "queued", substatus, "gpu-heavy");
        if let Some(e) = est {
            j["estimate"] = wall(e);
        }
        j
    }

    /// A read as the real core answers it: `queued` holds every queued job, the dispatched ones
    /// among them, which only the `{status: "queued"}` read lists.
    fn read_as_the_core(live: &mut LiveJobs, run: &[Value], queued: &[Value]) -> Vec<Op> {
        live.begin_read();
        live.add_running_page(&page(run, None));
        let held: Vec<Value> = queued
            .iter()
            .filter(|j| !matches!(text(j, "substatus"), "submitting" | "queued_in_engine"))
            .cloned()
            .collect();
        assert_eq!(live.add_queue_page(&page(&held, None)), None);
        assert_eq!(live.add_queued_page(&page(queued, None)), None);
        live.commit(NOW)
    }

    #[test] // spec: APP-114, CORE-160
    fn a_dispatched_job_stays_listed_after_the_running_and_before_the_queue() {
        let mut live = LiveJobs::new();
        let cpu = running("c", "cpu", None, 5);
        let a = dispatched_job("a", "queued_in_engine", Some(30.0));
        let b = queued("b", Some(0), Some(20.0));
        let c = queued("c2", Some(1), Some(10.0));
        let queue = [b, a, c];
        read_as_the_core(&mut live, std::slice::from_ref(&cpu), &queue);
        assert_eq!(ids(&live), ["c", "a", "b", "c2"]);
        // Reads after it keep it, and nobody's place moves for it.
        read_as_the_core(&mut live, std::slice::from_ref(&cpu), &queue);
        assert_eq!(ids(&live), ["c", "a", "b", "c2"]);
        assert_eq!(row(&live, "a").place, None, "it has no place");
        assert_eq!(row(&live, "b").place, Some(Some(0)));
        assert_eq!(row(&live, "c2").place, Some(Some(1)));
        // Its time counts ahead of the queue, and no phantom job does.
        assert_eq!(row(&live, "a").eta_text, "about 30 s");
        assert_eq!(row(&live, "b").eta_text, "about 50 s");
        assert_eq!(row(&live, "c2").eta_text, "about 1 min");
        // An event ends it.
        let mut done = dispatched_job("a", "queued_in_engine", None);
        done["status"] = json!("cancelled");
        done["updated"] = json!(at(0));
        live.apply_job(done, NOW);
        assert_eq!(ids(&live), ["c", "b", "c2"]);
    }

    #[test] // spec: APP-114, CORE-160
    fn a_dispatched_job_without_an_estimate_is_named_not_taken_as_zero() {
        let mut live = LiveJobs::new();
        let a = dispatched_job("a", "submitting", None);
        let b = queued("b", Some(0), Some(20.0));
        read_as_the_core(&mut live, &[], &[a, b]);
        assert_eq!(
            row(&live, "b").eta_text,
            "about 20 s, after 1 job with no estimate"
        );
        // A retry waiting on an earlier prompt: that prompt has no estimate, whoever's it is.
        let mut live = LiveJobs::new();
        let mut waits = dispatched_job("a", "submitting", Some(30.0));
        waits["waiting_for_earlier_prompt"] = json!(true);
        let b = queued("b", Some(0), Some(20.0));
        read_as_the_core(&mut live, &[], &[waits, b]);
        assert_eq!(
            row(&live, "a").eta_text,
            "about 30 s, after 1 job with no estimate"
        );
        assert_eq!(
            row(&live, "b").eta_text,
            "about 50 s, after 1 job with no estimate"
        );
    }

    #[test] // spec: APP-114, CORE-160
    fn a_job_that_leaves_the_queue_for_the_engine_loses_its_place_at_once() {
        let mut live = LiveJobs::new();
        let (a, b) = (
            queued("a", Some(0), Some(30.0)),
            queued("b", Some(1), Some(20.0)),
        );
        read(&mut live, &[], &[vec![a, b]]);
        let mut sent = dispatched_job("a", "submitting", Some(30.0));
        sent["updated"] = json!(at(0));
        live.apply_job(sent, NOW);
        assert_eq!(row(&live, "a").place, None, "its old place is gone");
        assert_eq!(ids(&live), ["a", "b"]);
    }

    #[test] // spec: APP-114, CORE-160
    fn every_page_of_the_status_reads_is_followed_and_repeats_dropped() {
        let mut live = LiveJobs::new();
        live.begin_read();
        // Two running pages.
        let first = page(&[running("r1", "gpu-heavy", None, 5)], Some("r2"));
        assert_eq!(live.add_running_page(&first).as_deref(), Some("r2"));
        let second = page(&[running("r2", "cpu", None, 5)], None);
        assert_eq!(live.add_running_page(&second), None);
        assert_eq!(live.add_queue_page(&page(&[], None)), None);
        // Two `{status: "queued"}` pages, the second repeating a job of the first; a job the
        // queue read holds a place for isn't taken from them.
        let a = dispatched_job("a", "queued_in_engine", None);
        let first = page(&[queued("w", Some(0), None), a.clone()], Some("q2"));
        assert_eq!(live.add_queued_page(&first).as_deref(), Some("q2"));
        let second = page(&[a, dispatched_job("b", "submitting", None)], None);
        assert_eq!(live.add_queued_page(&second), None);
        live.commit(NOW);
        assert_eq!(ids(&live), ["r1", "r2", "a", "b"]);
    }

    #[test] // spec: APP-114, CORE-160
    fn a_status_read_cut_at_the_page_cap_keeps_the_rows_it_cant_vouch_for() {
        let mut live = LiveJobs::new();
        read_as_the_core(&mut live, &[], &[dispatched_job("a", "submitting", None)]);
        live.begin_read();
        live.add_running_page(&page(&[], None));
        assert_eq!(live.add_queue_page(&page(&[], None)), None);
        for i in 0..MAX_PAGES {
            let next = live.add_queued_page(&page(&[], Some("more")));
            assert_eq!(next.is_some(), i + 1 < MAX_PAGES, "page {i}");
        }
        live.commit(NOW);
        assert_eq!(ids(&live), ["a"]);
        // The next read that ends on its own says it has gone.
        read_as_the_core(&mut live, &[], &[]);
        assert!(live.rows().is_empty());
    }

    #[test] // spec: APP-114, CORE-160
    fn a_queued_job_of_an_unknown_substatus_shows_by_the_status_read() {
        let mut live = LiveJobs::new();
        live.begin_read();
        live.add_running_page(&page(&[], None));
        live.add_queue_page(&page(&[], None));
        live.add_queued_page(&page(
            &[job("a", "queued", "from_later", "gpu-heavy")],
            None,
        ));
        live.commit(NOW);
        assert_eq!(ids(&live), ["a"]);
    }

    /// Appendix G "Queue position": a read on every `jobs` event of a job that is queued now or
    /// that the panel shows as queued; a running job's event that changes no status asks none.
    #[test] // spec: APP-114, CORE-160
    fn an_event_asks_for_a_read_as_appendix_g_says() {
        let mut live = LiveJobs::new();
        read(
            &mut live,
            &[running("r", "gpu-heavy", None, 5)],
            &[vec![queued("q", Some(0), None)]],
        );
        let change = |live: &mut LiveJobs, j: Value| live.apply_jobs(vec![j], NOW).change;
        // A queued job's event, its status the same (released, moved from another surface).
        let mut moved = queued("q", Some(3), None);
        moved["updated"] = json!(at(0));
        assert_eq!(change(&mut live, moved), JobChange::ReadAgain);
        // A running job's event, its status the same.
        let mut on = running("r", "gpu-heavy", None, 5);
        on["updated"] = json!(at(0));
        on["phase"] = json!("sampling");
        assert_eq!(change(&mut live, on), JobChange::Shown);
        // A job the panel shows as queued starts; a new one comes; one ends.
        let mut started = running("q", "gpu-heavy", None, 0);
        started["updated"] = json!(at(0));
        assert_eq!(change(&mut live, started), JobChange::ReadAgain);
        assert_eq!(
            change(&mut live, running("n", "cpu", None, 0)),
            JobChange::ReadAgain
        );
        let mut done = running("n", "cpu", None, 0);
        done["status"] = json!("succeeded");
        done["updated"] = json!(at(0));
        assert_eq!(change(&mut live, done.clone()), JobChange::ReadAgain);
        // An end of a job that isn't live changes nothing.
        assert_eq!(change(&mut live, done), JobChange::None);
    }

    #[test] // spec: APP-114, APP-139
    fn a_batch_of_events_is_one_set_of_changes() {
        let mut live = LiveJobs::new();
        let q: Vec<Value> = (0..30)
            .map(|i| queued(&format!("q{i:02}"), Some(i), Some(10.0)))
            .collect();
        read(&mut live, &[], std::slice::from_ref(&q));
        let before = live.rows().to_vec();
        // Every other job is cancelled, one job twice; a new one comes.
        let mut events: Vec<Value> = q
            .iter()
            .step_by(2)
            .map(|j| {
                let mut gone = j.clone();
                gone["status"] = json!("cancelled");
                gone["updated"] = json!(at(0));
                gone
            })
            .collect();
        events.push(events[0].clone());
        events.push(queued("new", None, None));
        let applied = live.apply_jobs(events, NOW);
        assert_eq!(applied.change, JobChange::ReadAgain);
        replay(&before, &applied.ops, live.rows());
        assert_eq!(live.rows().len(), 16);
        let removes = applied
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Remove { .. }))
            .count();
        assert_eq!(removes, 15, "one range for each run of rows");
        // Nothing to apply, nothing to do.
        assert!(live.apply_jobs(Vec::new(), NOW).ops.is_empty());
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
        live.add_running_page(&page(&[], None));
        assert_eq!(
            live.add_queue_page(&page(&[queued("a", Some(0), None)], None)),
            None
        );
        live.commit(NOW);
        assert!(live.rows().is_empty(), "the cancel stands");
    }
}
