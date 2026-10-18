//! [`Jobs`]: submit, the lanes' dispatch, a job's run, cancel, retry, recovery and the
//! shutdown step (module docs of [`crate::jobs`]).
//!
//! **Locks.** One `std` mutex guards the in-memory queue (the jobs this core runs, the lanes);
//! it is never held across an `await`. Each job's state changes are serialised by its own
//! async lock, held across the row update, the jobs log line and the events, so the log and the
//! topics see a job's changes in the order the row took them.
//!
//! **Which jobs a recovery touches.** Only rows that this core isn't running: a submit writes
//! its row and then adds the job to the queue, and the recovery of a scope runs once, before
//! the first job operation in it (or before the core serves, for the suite scope), so it never
//! meets a row of this core's own run.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, Weak};
use std::time::{Duration, UNIX_EPOCH};

use lil_platform::{Clock, IdSource};
use lil_protocol::codes::{self, CodeSpec};
use lil_protocol::{ErrorBody, ErrorCategory, Outcome, RetryPolicy, ToolOutput, args_hash};
use serde_json::{Value, json};
use tokio::sync::{OnceCell, watch};
use tracing::Instrument as _;

use super::queue::{self, Candidate};
use super::record::{self, Change, Inserted, JobRow};
use super::{
    Admission, Admit, AlwaysAdmit, DISK_MARGIN_BYTES, JobCompletion, JobCtx, JobDone, JobFailure,
    JobFilter, JobHandler, JobId, JobResult, JobSpec, JobStatus, JobSummary, Lane, MAX_ATTEMPTS,
    MAX_WAIT, Priority, Progress, Substatus, backoff,
};
use crate::events::{EventBus, SnapshotProvider};
use crate::lifecycle::{IdleBlocker, ShutdownPhase, ShutdownStep};
use crate::pipeline::apply_record;
use crate::state::{StateDb, StateError, StateRegistry};
use crate::store::{FailPoints, LogKind};
use crate::tools::CallCtx;
use crate::workspace::{Access, OpenWorkspace, WorkspaceHub};

/// The error a cancelled job's completions get as its outcome. Appendix D has no code for a
/// cancellation, so it never reaches the wire: a cancelled job's summary has no `error`.
const CANCELLED: CodeSpec = CodeSpec {
    code: "job.cancelled",
    category: ErrorCategory::Cancelled,
    retry: RetryPolicy::AfterResolve,
    provisional: true,
};

/// The outcome error of a cancelled job (module docs of `CANCELLED`): handlers return it when
/// they stop on `JobCtx::cancel`.
pub fn cancelled_error(trace_id: &str) -> ErrorBody {
    ErrorBody::new(&CANCELLED, "The job was cancelled.", trace_id)
}

/// The jobs of this core (module docs).
pub struct Jobs {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs")
            .field("active", &self.shared.queue().jobs.len())
            .finish_non_exhaustive()
    }
}

/// Where a job lives: the suite `state.db` or a workspace (its `state.db` and its jobs log).
#[derive(Clone)]
pub(crate) enum Scope {
    Suite(Arc<StateDb>),
    Workspace(Arc<OpenWorkspace>),
}

impl Scope {
    fn db(&self) -> &StateDb {
        match self {
            Scope::Suite(db) => db,
            Scope::Workspace(open) => &open.state,
        }
    }

    fn db_arc(&self) -> Arc<StateDb> {
        match self {
            Scope::Suite(db) => db.clone(),
            Scope::Workspace(open) => open.state.clone(),
        }
    }

    fn workspace(&self) -> Option<&Arc<OpenWorkspace>> {
        match self {
            Scope::Suite(_) => None,
            Scope::Workspace(open) => Some(open),
        }
    }

    /// The recovery key: `suite`, or the workspace id.
    fn key(&self) -> String {
        match self {
            Scope::Suite(_) => "suite".to_string(),
            Scope::Workspace(open) => format!("ws:{}", open.ws.id),
        }
    }
}

/// What a job keeps from its submit.
#[derive(Clone)]
struct Info {
    id: JobId,
    spec: JobSpec,
    scope: Scope,
    trace_id: String,
    reservation: Option<String>,
    retry_of: Option<String>,
}

/// A job this core runs.
struct Active {
    info: Info,
    /// Submission order in this core.
    order: u64,
    /// Requeued at the head of its class (preempted, CORE-125).
    head: bool,
    overtaken: u32,
    running: bool,
    cancel: watch::Sender<bool>,
    cancelling: bool,
    preempted: bool,
    /// `cancel_in_engine` was called for the current run.
    engine_cancel_sent: bool,
    /// The running attempt's context, once its handler may start.
    ctx: Option<Arc<JobCtx>>,
    progress: Option<Progress>,
    waiting_for_vram: bool,
}

#[derive(Default)]
struct LaneState {
    running: Option<String>,
    /// The group of the lane's last job (CORE-124).
    group: Option<String>,
}

#[derive(Default)]
struct Queue {
    jobs: HashMap<String, Active>,
    lanes: [LaneState; 2],
    next_order: u64,
    /// Set by the `PersistInterrupted` step: nothing starts or finishes any more.
    closed: bool,
}

/// The state behind [`Jobs`], shared with its tasks and each [`JobCtx`].
pub(crate) struct Shared {
    states: Arc<StateRegistry>,
    hub: Arc<WorkspaceHub>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    failpoints: Arc<FailPoints>,
    handlers: RwLock<HashMap<&'static str, Arc<dyn JobHandler>>>,
    admission: RwLock<Arc<dyn Admission>>,
    completions: RwLock<Vec<Arc<dyn JobCompletion>>>,
    preempt: AtomicBool,
    queue: Mutex<Queue>,
    /// Bumped on every state change (`job.wait`).
    changed: watch::Sender<u64>,
    recovered: Mutex<HashMap<String, Arc<OnceCell<()>>>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T: Clone>(m: &RwLock<T>) -> T {
    m.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn state_unavailable(e: &StateError, trace_id: &str) -> ErrorBody {
    tracing::error!(trace_id, error = %e, "state.db failed during a job operation");
    ErrorBody::new(
        &codes::STATE_UNAVAILABLE,
        "The core's state database is not available; retry with the same idempotency key.",
        trace_id,
    )
}

fn job_unknown(id: &str, trace_id: &str) -> ErrorBody {
    ErrorBody::new(
        &codes::JOB_UNKNOWN,
        format!("There is no job {id:?} here."),
        trace_id,
    )
}

fn params_invalid(message: String, trace_id: &str) -> ErrorBody {
    ErrorBody::new(&codes::PARAMS_INVALID, message, trace_id)
}

/// How a run ends.
enum End {
    Succeeded(JobDone),
    Failed(ErrorBody),
    Cancelled,
    Interrupted {
        error: ErrorBody,
        salvaged: Vec<lil_protocol::FileRef>,
    },
    Unknown(ErrorBody),
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        lock(&self.queue)
    }

    pub(crate) fn failpoints(&self) -> &Arc<FailPoints> {
        &self.failpoints
    }

    fn now(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    fn handler(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        self.handlers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(kind)
            .cloned()
    }

    fn job_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.locks).entry(id.to_string()).or_default().clone()
    }

    fn scope_of(&self, ctx: &CallCtx) -> Scope {
        match &ctx.workspace {
            Some(open) => Scope::Workspace(open.clone()),
            None => Scope::Suite(self.states.suite_db()),
        }
    }

    fn progress_of(&self, id: &str) -> Option<Progress> {
        self.queue().jobs.get(id).and_then(|a| a.progress.clone())
    }

    fn summary(&self, row: &JobRow) -> JobSummary {
        row.summary(self.progress_of(&row.job_id))
    }

    /// The jobs log line, the events and the `job.wait` wake-up for `row`'s new state.
    async fn announce(&self, open: Option<&Arc<OpenWorkspace>>, row: &JobRow) {
        if let Some(open) = open
            && let Err(e) = open.store.append_log(LogKind::Jobs, row.log_record()).await
        {
            tracing::warn!(job_id = %row.job_id, error = %e, "a jobs log line could not be written");
        }
        let job = serde_json::to_value(self.summary(row)).expect("a job summary serialises");
        self.events
            .publish("jobs", json!({"kind": "job", "job": job.clone()}));
        self.events.publish(
            &format!("job:{}", row.job_id),
            json!({"kind": "status", "job": job}),
        );
        self.changed.send_modify(|n| *n += 1);
    }

    /// Applies `c` to the job (a guarded update: never out of a final status), then
    /// announces it. `when` limits it to a job in that status. `jobs.after_submitting` fires
    /// when the substatus becomes `submitting`.
    async fn apply(
        &self,
        id: &str,
        scope: &Scope,
        c: Change,
        when: Option<JobStatus>,
    ) -> Option<JobRow> {
        let lock = self.job_lock(id);
        let _serial = lock.lock().await;
        if let Some(when) = when {
            match record::get(scope.db(), id).await {
                Ok(Some(row)) if row.status == when => {}
                Ok(_) => return None,
                Err(e) => {
                    tracing::error!(job_id = id, error = %e, "a job's state could not be read");
                    return None;
                }
            }
        }
        let submitting = c.substatus == Some(Some(Substatus::Submitting));
        match record::update(scope.db(), id, c, self.now()).await {
            Ok(Some((before, row))) => {
                self.announce(scope.workspace(), &row).await;
                if submitting
                    && before != Some(Substatus::Submitting)
                    && let Err(e) = self.failpoints.hit("jobs.after_submitting")
                {
                    tracing::warn!(job_id = id, error = %e, "fail point jobs.after_submitting");
                }
                Some(row)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::error!(job_id = id, error = %e, "a job's state change could not be written");
                None
            }
        }
    }

    /// The recovery of one scope (ARCH-022): every row without a final status that this core
    /// isn't running becomes `interrupted`, or `interrupted_unknown` when it was `submitting`
    /// with external side effects. A row already `interrupted_unknown` stays as it is.
    async fn recover_rows(
        &self,
        db: &StateDb,
        open: Option<&Arc<OpenWorkspace>>,
    ) -> Result<Vec<JobId>, StateError> {
        let rows = record::open_rows(db).await?;
        let mine: HashSet<String> = self.queue().jobs.keys().cloned().collect();
        let mut marked = Vec::new();
        for row in rows {
            if mine.contains(&row.job_id) || row.substatus == Some(Substatus::InterruptedUnknown) {
                continue;
            }
            let c = interrupt_change(&row, self.now());
            if let Some((_, row)) = record::update(db, &row.job_id, c, self.now()).await? {
                tracing::warn!(job_id = %row.job_id, substatus = ?row.substatus, "a job of an earlier core run is interrupted; it doesn't run again");
                self.announce(open, &row).await;
                marked.push(JobId(row.job_id));
            }
        }
        Ok(marked)
    }

    /// Recovers `scope` once (module docs). A read-only workspace is left as it is.
    async fn ensure_recovered(&self, scope: &Scope) -> Result<(), StateError> {
        if let Scope::Workspace(open) = scope
            && open.access == Access::ReadOnly
        {
            return Ok(());
        }
        let cell = lock(&self.recovered)
            .entry(scope.key())
            .or_default()
            .clone();
        cell.get_or_try_init(|| async {
            self.recover_rows(scope.db(), scope.workspace())
                .await
                .map(drop)
        })
        .await
        .map(drop)
    }

    /// A new job row for `spec`, queued (ARCH-034): written and logged before the job can
    /// start. A job that holds `idem` already is returned instead when its arguments match,
    /// else `idempotency.conflict` (CORE-016).
    async fn submit_new(
        self: &Arc<Self>,
        scope: &Scope,
        spec: JobSpec,
        trace_id: &str,
        idem: Option<(String, String)>,
        reservation: Option<String>,
        retry_of: Option<String>,
    ) -> Result<JobRow, ErrorBody> {
        if self.queue().closed {
            return Err(ErrorBody::new(
                &codes::CORE_DRAINING,
                "The core is shutting down. Retry with the same idempotency key once it runs again.",
                trace_id,
            ));
        }
        if self.handler(spec.kind).is_none() {
            return Err(ErrorBody::new(
                &codes::CORE_BUG,
                format!(
                    "No handler runs jobs of kind {:?}. This is a bug in LILITH.",
                    spec.kind
                ),
                trace_id,
            ));
        }
        let hash = args_hash(&json!({"kind": spec.kind, "args": spec.args}));
        let now = self.now();
        let id = self.ids.id("j");
        let (idem_scope, idem_key) = idem.unzip();
        let row = JobRow {
            seq: 0,
            job_id: id.clone(),
            kind: spec.kind.to_string(),
            lane: spec.lane,
            priority: spec.priority,
            module: spec.module.clone(),
            args: spec.args.clone(),
            project: spec.project.clone(),
            asset_id: spec.asset_id.clone(),
            doc_id: spec.doc_id.clone(),
            status: JobStatus::Queued,
            substatus: Some(Substatus::Waiting),
            attempt: 1,
            created_utc: now,
            updated_utc: now,
            started_utc: None,
            finished_utc: None,
            watchdog: None,
            vram_unchecked: false,
            error: None,
            result: None,
            retry_of: retry_of.clone(),
            trace_id: trace_id.to_string(),
            idem_scope,
            idem_key,
            args_hash: hash.clone(),
            expected_secs: spec.expected_secs,
            expected_output_bytes: spec.expected_output_bytes,
            vram_need_mib: spec.vram_need_mib,
            engine: spec.engine,
            group: spec.group.clone(),
            external_side_effects: spec.external_side_effects,
            reservation: reservation.clone(),
        };
        let serial = self.job_lock(&id);
        let guard = serial.lock().await;
        let inserted = record::insert(scope.db(), row).await;
        let row = match inserted {
            Err(e) => {
                drop(guard);
                lock(&self.locks).remove(&id);
                return Err(state_unavailable(&e, trace_id));
            }
            Ok(Inserted::Existing(existing)) => {
                drop(guard);
                lock(&self.locks).remove(&id);
                if existing.args_hash == hash {
                    return Ok(existing);
                }
                return Err(ErrorBody::new(
                    &codes::IDEMPOTENCY_CONFLICT,
                    "The idempotency key was used before for another job.",
                    trace_id,
                ));
            }
            Ok(Inserted::New(row)) => row,
        };
        self.announce(scope.workspace(), &row).await;
        drop(guard);
        let info = Info {
            id: JobId(id.clone()),
            spec,
            scope: scope.clone(),
            trace_id: trace_id.to_string(),
            reservation,
            retry_of,
        };
        let preempt = {
            let mut q = self.queue();
            if q.closed {
                // The shutdown step has run: the row stays queued, and the next start
                // recovers it as interrupted.
                lock(&self.locks).remove(&id);
                return Ok(row);
            }
            q.next_order += 1;
            let order = q.next_order;
            let preempt = self.preemption(&mut q, &info.spec);
            q.jobs.insert(
                id,
                Active {
                    info,
                    order,
                    head: false,
                    overtaken: 0,
                    running: false,
                    cancel: watch::Sender::new(false),
                    cancelling: false,
                    preempted: false,
                    engine_cancel_sent: false,
                    ctx: None,
                    progress: None,
                    waiting_for_vram: false,
                },
            );
            preempt
        };
        if let Some((ctx, handler)) = preempt {
            tokio::spawn(async move { handler.cancel_in_engine(&ctx).await });
        }
        self.pump();
        Ok(row)
    }

    /// CORE-125: with `preempt_agent_batches`, an `interactive` job cancels the running
    /// `agent` engine job of its lane; that job goes back to the head of its class when its
    /// run ends. A `user` job is never touched. Returns the engine cancel to run.
    fn preemption(
        &self,
        q: &mut Queue,
        incoming: &JobSpec,
    ) -> Option<(Arc<JobCtx>, Arc<dyn JobHandler>)> {
        if !self.preempt.load(Ordering::SeqCst) || incoming.priority != Priority::Interactive {
            return None;
        }
        let running = q.lanes[incoming.lane.index()].running.clone()?;
        let a = q.jobs.get_mut(&running)?;
        if a.info.spec.priority != Priority::Agent
            || a.info.spec.engine.is_none()
            || a.cancelling
            || a.preempted
        {
            return None;
        }
        tracing::info!(job_id = %running, "an interactive job preempts a running agent job");
        a.preempted = true;
        a.cancel.send_replace(true);
        if a.engine_cancel_sent {
            return None;
        }
        let ctx = a.ctx.clone()?;
        a.engine_cancel_sent = true;
        Some((ctx, self.handler(a.info.spec.kind)?))
    }

    /// Starts the next job of each free lane (CORE-121, CORE-123, CORE-124).
    fn pump(self: &Arc<Self>) {
        let admission = read(&self.admission);
        let mut start = Vec::new();
        let mut wait = Vec::new();
        {
            let mut q = self.queue();
            if q.closed {
                return;
            }
            for lane in Lane::ALL {
                let li = lane.index();
                if q.lanes[li].running.is_some() {
                    continue;
                }
                let picked = {
                    let candidates: Vec<Candidate<'_>> = q
                        .jobs
                        .iter()
                        .filter(|(_, a)| !a.running && a.info.spec.lane == lane)
                        .map(|(id, a)| Candidate {
                            id,
                            priority: a.info.spec.priority,
                            head: a.head,
                            order: a.order,
                            group: a.info.spec.group.as_deref(),
                            overtaken: a.overtaken,
                        })
                        .collect();
                    queue::pick(&candidates, q.lanes[li].group.as_deref()).map(|p| {
                        let overtakes: Vec<String> =
                            p.overtakes.iter().map(|o| (*o).to_string()).collect();
                        (p.id.to_string(), overtakes)
                    })
                };
                let Some((id, overtakes)) = picked else {
                    continue;
                };
                let a = q.jobs.get_mut(&id).expect("a picked job is queued");
                match admission.admit(&a.info.spec) {
                    Admit::WaitForVram { need_mib, free_mib } => {
                        if !a.waiting_for_vram {
                            a.waiting_for_vram = true;
                            tracing::info!(job_id = %id, need_mib, free_mib, "a job waits for VRAM");
                            wait.push((id, a.info.scope.clone()));
                        }
                    }
                    admit => {
                        a.running = true;
                        a.head = false;
                        a.waiting_for_vram = false;
                        let group = a.info.spec.group.clone();
                        for o in overtakes {
                            if let Some(o) = q.jobs.get_mut(&o) {
                                o.overtaken += 1;
                            }
                        }
                        q.lanes[li] = LaneState {
                            running: Some(id.clone()),
                            group,
                        };
                        start.push((id, admit == Admit::GoUnchecked));
                    }
                }
            }
        }
        for (id, scope) in wait {
            let shared = self.clone();
            tokio::spawn(async move {
                shared
                    .apply(
                        &id,
                        &scope,
                        Change::to(Substatus::WaitingForVram),
                        Some(JobStatus::Queued),
                    )
                    .await;
            });
        }
        for (id, unchecked) in start {
            let shared = self.clone();
            tokio::spawn(async move { shared.run_job(id, unchecked).await });
        }
    }

    /// The disk preflight (CORE-128): `store.disk_full` when the staging volume has less
    /// than 2 × the expected output + 64 MiB free. A volume that can't be read is logged and
    /// passes.
    async fn preflight(&self, info: &Info) -> Option<ErrorBody> {
        let root: PathBuf = match &info.scope {
            Scope::Workspace(open) => open.store.root().to_path_buf(),
            Scope::Suite(_) => self.hub.env().local_dir(),
        };
        let need = info
            .spec
            .expected_output_bytes
            .saturating_mul(2)
            .saturating_add(DISK_MARGIN_BYTES);
        let shown = root.display().to_string();
        let free = tokio::task::spawn_blocking(move || lil_platform::volume::free_bytes(&root))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))
            .and_then(|r| r);
        match free {
            Ok(free) if free < need => {
                let mut error = ErrorBody::new(
                    &codes::STORE_DISK_FULL,
                    format!(
                        "The job needs {need} bytes free on the volume of {shown}, and it has {free}. Free some space and retry."
                    ),
                    &info.trace_id,
                );
                error.data = Some(json!({"need_bytes": need, "free_bytes": free}));
                Some(error)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(job_id = %info.id, error = %e, "the free space for a job can't be read; it runs");
                None
            }
        }
    }

    fn ctx_for(
        self: &Arc<Self>,
        info: &Info,
        attempt: u32,
        cancel: watch::Receiver<bool>,
    ) -> JobCtx {
        JobCtx {
            id: info.id.clone(),
            spec: info.spec.clone(),
            attempt,
            trace_id: info.trace_id.clone(),
            workspace: info.scope.workspace().cloned(),
            events: self.events.clone(),
            cancel,
            state: info.scope.db_arc(),
            reservation: info.reservation.clone(),
            jobs: self.clone(),
        }
    }

    /// `(cancelling, preempted)` of a job this core runs.
    fn flags(&self, id: &str) -> (bool, bool) {
        self.queue()
            .jobs
            .get(id)
            .map_or((true, false), |a| (a.cancelling, a.preempted))
    }

    /// One job's run, from its start to its end (module docs of [`crate::jobs`]).
    async fn run_job(self: Arc<Self>, id: String, unchecked: bool) {
        let Some((info, mut cancel)) = self
            .queue()
            .jobs
            .get(&id)
            .map(|a| (a.info.clone(), a.cancel.subscribe()))
        else {
            return;
        };
        let span = tracing::info_span!("job", job_id = %id, trace_id = %info.trace_id);
        async move {
            let Some(handler) = self.handler(info.spec.kind) else {
                let error = ErrorBody::new(&codes::CORE_BUG, "The job's handler is gone.", &info.trace_id);
                return self.complete(&info, 1, End::Failed(error)).await;
            };
            let first = record::get(info.scope.db(), &id).await.ok().flatten();
            let mut attempt = first.map_or(1, |r| r.attempt.max(1));
            if let Some(error) = self.preflight(&info).await {
                return self.complete(&info, attempt, End::Failed(error)).await;
            }
            let mut first_attempt = true;
            loop {
                let initial = if info.spec.engine.is_some() {
                    Substatus::Submitting
                } else {
                    Substatus::Executing
                };
                let mut c = Change::to(initial);
                c.attempt = Some(attempt);
                c.started_utc = Some(self.now());
                if first_attempt {
                    c.vram_unchecked = Some(unchecked);
                }
                self.apply(&id, &info.scope, c, None).await;
                let ctx = Arc::new(self.ctx_for(&info, attempt, cancel.clone()));
                let proceed = {
                    let mut q = self.queue();
                    match q.jobs.get_mut(&id) {
                        Some(a) if !a.cancelling && !a.preempted => {
                            a.ctx = Some(ctx.clone());
                            true
                        }
                        _ => false,
                    }
                };
                let result = if proceed {
                    let (h, c) = (handler.clone(), ctx.clone());
                    let run = tokio::spawn(async move { h.run(&c).await }.in_current_span());
                    match run.await {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!(error = %e, "a job handler panicked");
                            Err(JobFailure::Final(ErrorBody::new(
                                &codes::CORE_BUG,
                                format!("The {} job failed with an internal error. This is a bug in LILITH.", info.spec.kind),
                                &info.trace_id,
                            )))
                        }
                    }
                } else {
                    Err(JobFailure::Final(cancelled_error(&info.trace_id)))
                };
                if let Some(a) = self.queue().jobs.get_mut(&id) {
                    a.ctx = None;
                }
                let (cancelling, preempted) = self.flags(&id);
                let failure = match result {
                    Ok(done) => return self.complete(&info, attempt, End::Succeeded(done)).await,
                    Err(_) if cancelling => return self.complete(&info, attempt, End::Cancelled).await,
                    Err(_) if preempted => return self.requeue(&info).await,
                    Err(failure) => failure,
                };
                let end = match failure {
                    JobFailure::Retryable(error) if attempt < MAX_ATTEMPTS => {
                        let mut c = Change::to(Substatus::Retrying {
                            attempt,
                            of: MAX_ATTEMPTS,
                        });
                        c.error = Some(Some(error));
                        self.apply(&id, &info.scope, c, None).await;
                        self.events.publish(
                            &format!("job:{id}"),
                            json!({"kind": "retrying", "attempt": attempt, "of": MAX_ATTEMPTS}),
                        );
                        let woke = tokio::select! {
                            () = tokio::time::sleep(backoff(attempt)) => false,
                            _ = cancel.wait_for(|c| *c) => true,
                        };
                        if woke {
                            let (cancelling, preempted) = self.flags(&id);
                            if preempted && !cancelling {
                                return self.requeue(&info).await;
                            }
                            return self.complete(&info, attempt, End::Cancelled).await;
                        }
                        attempt += 1;
                        first_attempt = false;
                        continue;
                    }
                    JobFailure::Retryable(error) | JobFailure::Final(error) => End::Failed(error),
                    JobFailure::Interrupted { error, salvaged } => End::Interrupted { error, salvaged },
                    JobFailure::Unknown(error) => End::Unknown(error),
                };
                return self.complete(&info, attempt, end).await;
            }
        }
        .instrument(span)
        .await;
    }

    /// A preempted job goes back to the head of its class (CORE-125); its attempt stays.
    async fn requeue(self: &Arc<Self>, info: &Info) {
        self.apply(
            info.id.as_str(),
            &info.scope,
            Change::to(Substatus::Waiting),
            None,
        )
        .await;
        {
            let mut q = self.queue();
            let lane = info.spec.lane.index();
            if q.lanes[lane].running.as_deref() == Some(info.id.as_str()) {
                q.lanes[lane].running = None;
            }
            if let Some(a) = q.jobs.get_mut(info.id.as_str()) {
                a.running = false;
                a.head = true;
                a.preempted = false;
                a.engine_cancel_sent = false;
                a.cancel = watch::Sender::new(false);
            }
        }
        self.pump();
    }

    /// A job ends: its completions in registration order (none for `interrupted_unknown`),
    /// then the Record step of a job that worked, then its final state; it leaves the queue
    /// and the lane's next job starts. After the shutdown step nothing more is written.
    async fn complete(self: &Arc<Self>, info: &Info, attempt: u32, end: End) {
        if self.queue().closed {
            tracing::info!(job_id = %info.id, "a job ended after the shutdown step; its end is not recorded");
            return;
        }
        let ctx = self.ctx_for(info, attempt, watch::Sender::new(true).subscribe());
        let outcome = match &end {
            End::Succeeded(done) => Outcome::Ok(ToolOutput {
                structured: done.structured.clone(),
                artifacts: done.artifacts.clone(),
                summary_text: format!("{} succeeded", info.spec.kind),
            }),
            End::Failed(error) | End::Interrupted { error, .. } | End::Unknown(error) => {
                Outcome::Error {
                    error: error.clone(),
                }
            }
            End::Cancelled => Outcome::Error {
                error: cancelled_error(&info.trace_id),
            },
        };
        if !matches!(end, End::Unknown(_)) {
            let completions = read(&self.completions);
            let done = match &end {
                End::Succeeded(done) => Some(done),
                _ => None,
            };
            for c in completions {
                c.completed(&ctx, &outcome, done).await;
            }
        }
        let now = self.now();
        let mut rerun = false;
        let c = match end {
            End::Succeeded(done) => self.recorded(info, done, now).await,
            End::Failed(error) => {
                let mut c = Change::finished(JobStatus::Failed);
                c.error = Some(Some(error));
                c.finished_utc = Some(now);
                c
            }
            End::Cancelled => {
                let mut c = Change::finished(JobStatus::Cancelled);
                c.error = Some(None);
                c.finished_utc = Some(now);
                c
            }
            End::Interrupted { error, salvaged } => {
                rerun = info.spec.priority == Priority::Agent && info.retry_of.is_none();
                let mut c = Change::to(Substatus::Interrupted);
                c.error = Some(Some(error));
                c.result = Some(JobResult {
                    structured: json!({}),
                    artifacts: salvaged,
                });
                c.finished_utc = Some(now);
                c
            }
            End::Unknown(error) => {
                let mut c = Change::to(Substatus::InterruptedUnknown);
                c.error = Some(Some(error));
                c
            }
        };
        self.apply(info.id.as_str(), &info.scope, c, None).await;
        self.leave(info);
        if rerun {
            tracing::info!(job_id = %info.id, "an interrupted agent job runs once more as a new job");
            if let Err(e) = self
                .submit_new(
                    &info.scope,
                    info.spec.clone(),
                    &info.trace_id,
                    None,
                    None,
                    Some(info.id.to_string()),
                )
                .await
            {
                tracing::warn!(job_id = %info.id, code = %e.code, "the re-run of an interrupted agent job could not be queued");
            }
        }
        self.pump();
    }

    /// The final change of a job that worked: its Record step first (CORE-015 step 9), whose
    /// hand-off block and warnings go into the result, as the inline Record stage does. A
    /// Record step that fails before the manifest edit fails the job.
    async fn recorded(&self, info: &Info, mut done: JobDone, now: i64) -> Change {
        let mut structured = std::mem::take(&mut done.structured);
        if let Some(plan) = done.record.take() {
            let recorded = match info.scope.workspace() {
                None => Err(ErrorBody::new(
                    &codes::CORE_BUG,
                    format!(
                        "The {} job left a record without a workspace. This is a bug in LILITH.",
                        info.spec.kind
                    ),
                    &info.trace_id,
                )),
                Some(open) => {
                    let today = lil_schema::time::date(open.store.now_utc());
                    apply_record(
                        &open.store,
                        &open.index,
                        &self.events,
                        &open.ws.id,
                        &today,
                        plan,
                    )
                    .await
                    .map_err(|e| e.to_error(&info.trace_id))
                }
            };
            match recorded {
                Ok(recorded) => {
                    if let Value::Object(map) = &mut structured {
                        map.insert(
                            "handoff".into(),
                            serde_json::to_value(&recorded.handoff)
                                .expect("a hand-off block serialises"),
                        );
                        map.insert("warnings".into(), Value::from(recorded.warnings));
                    }
                }
                Err(error) => {
                    tracing::warn!(job_id = %info.id, code = %error.code, "a job's Record step failed");
                    let mut c = Change::finished(JobStatus::Failed);
                    c.error = Some(Some(error));
                    c.finished_utc = Some(now);
                    return c;
                }
            }
        }
        let mut c = Change::finished(JobStatus::Succeeded);
        c.error = Some(None);
        c.result = Some(JobResult {
            structured,
            artifacts: done.artifacts,
        });
        c.finished_utc = Some(now);
        c
    }

    /// The job leaves the queue and its lane.
    fn leave(&self, info: &Info) {
        let mut q = self.queue();
        let lane = info.spec.lane.index();
        if q.lanes[lane].running.as_deref() == Some(info.id.as_str()) {
            q.lanes[lane].running = None;
        }
        q.jobs.remove(info.id.as_str());
        lock(&self.locks).remove(info.id.as_str());
    }

    /// `JobCtx::set_substatus`.
    pub(crate) async fn handler_substatus(&self, id: &JobId, s: Substatus) {
        let scope = {
            let q = self.queue();
            match q.jobs.get(id.as_str()) {
                Some(a) if a.running && !a.cancelling => a.info.scope.clone(),
                _ => return,
            }
        };
        self.apply(id.as_str(), &scope, Change::to(s), None).await;
    }

    /// `JobCtx::progress`.
    pub(crate) fn progress(&self, id: &JobId, value: u64, max: u64, node: Option<&str>) {
        let progress = Progress {
            value,
            max,
            node: node.map(str::to_string),
        };
        if let Some(a) = self.queue().jobs.get_mut(id.as_str()) {
            a.progress = Some(progress.clone());
        }
        let mut data = json!({"kind": "progress", "value": value, "max": max});
        if let Some(node) = &progress.node {
            data["node"] = Value::from(node.as_str());
        }
        self.events.publish(&format!("job:{id}"), data);
    }

    /// The `PersistInterrupted` shutdown step (ARCH-021 step 3): the jobs still queued or
    /// running are marked as a recovery would mark them, and nothing starts or ends any more.
    async fn persist_interrupted(&self) {
        let left: Vec<Info> = {
            let mut q = self.queue();
            q.closed = true;
            q.lanes = Default::default();
            q.jobs
                .drain()
                .map(|(_, a)| {
                    a.cancel.send_replace(true);
                    a.info
                })
                .collect()
        };
        for info in left {
            let row = match record::get(info.scope.db(), info.id.as_str()).await {
                Ok(Some(row)) if !row.status.is_final() => row,
                Ok(_) => continue,
                Err(e) => {
                    tracing::error!(job_id = %info.id, error = %e, "a running job could not be marked interrupted");
                    continue;
                }
            };
            let c = interrupt_change(&row, self.now());
            self.apply(info.id.as_str(), &info.scope, c, None).await;
        }
    }
}

/// How a job without a final state is interrupted (ARCH-022, CORE-128).
fn interrupt_change(row: &JobRow, now: i64) -> Change {
    if row.substatus == Some(Substatus::Submitting) && row.external_side_effects {
        Change::to(Substatus::InterruptedUnknown)
    } else {
        let mut c = Change::to(Substatus::Interrupted);
        c.finished_utc = Some(now);
        c
    }
}

impl Jobs {
    /// The jobs of a core. Registers the `job:` topic snapshots on `events`.
    pub fn new(
        states: Arc<StateRegistry>,
        hub: Arc<WorkspaceHub>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        failpoints: Arc<FailPoints>,
    ) -> Arc<Jobs> {
        let shared = Arc::new(Shared {
            states,
            hub,
            events: events.clone(),
            clock,
            ids,
            failpoints,
            handlers: RwLock::new(HashMap::new()),
            admission: RwLock::new(Arc::new(AlwaysAdmit)),
            completions: RwLock::new(Vec::new()),
            preempt: AtomicBool::new(false),
            queue: Mutex::new(Queue::default()),
            changed: watch::Sender::new(0),
            recovered: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
        });
        events.register_snapshot("job:", Arc::new(JobSnapshots(Arc::downgrade(&shared))));
        Arc::new(Jobs { shared })
    }

    /// Adds `h` for its kind (a later one for the same kind replaces it).
    pub fn register(&self, h: Arc<dyn JobHandler>) {
        self.shared
            .handlers
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(h.kind(), h);
    }

    /// Replaces the admission (Task 10's GPU scheduler).
    pub fn set_admission(&self, a: Arc<dyn Admission>) {
        *self
            .shared
            .admission
            .write()
            .unwrap_or_else(PoisonError::into_inner) = a;
    }

    /// Adds a completion; they run in the order they were added (Meter before Caps).
    pub fn add_completion(&self, c: Arc<dyn JobCompletion>) {
        self.shared
            .completions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(c);
    }

    /// `[jobs] preempt_agent_batches` (CORE-125; default `false`).
    pub fn set_preempt_agent_batches(&self, on: bool) {
        self.shared.preempt.store(on, Ordering::SeqCst);
    }

    /// Looks at the lanes again: a job waiting for VRAM may start now (Task 10 calls it on each
    /// telemetry sample).
    pub fn kick(&self) {
        self.shared.pump();
    }

    /// Submits `spec` in the scope of `ctx` and returns it `queued` (ARCH-034): its row and
    /// its jobs log line are written first. With an idempotency key, the job that key made
    /// before is returned (CORE-016). Sets `ctx.submitted`.
    ///
    /// # Errors
    /// `idempotency.conflict`, `core.draining` after the shutdown step, `state.unavailable`,
    /// `core.bug` for a kind no handler runs.
    pub async fn submit(&self, ctx: &CallCtx, spec: JobSpec) -> Result<JobSummary, ErrorBody> {
        let s = &self.shared;
        let scope = s.scope_of(ctx);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        let idem = ctx.idem_key.as_ref().map(|key| {
            (
                format!("{}|{}", ctx.session.caller, ctx.session.workspace_id()),
                key.clone(),
            )
        });
        let row = s
            .submit_new(
                &scope,
                spec,
                &ctx.trace_id,
                idem,
                ctx.reservation.clone(),
                None,
            )
            .await?;
        ctx.submitted.set(JobId(row.job_id.clone()));
        Ok(s.summary(&row))
    }

    /// The jobs of `ctx`'s scope that `f` selects, newest first, and the cursor of the next
    /// page (`None` on the last).
    ///
    /// # Errors
    /// `params.invalid` for a `limit` outside 1–500 or a cursor this didn't give;
    /// `state.unavailable`.
    pub async fn list(
        &self,
        ctx: &CallCtx,
        f: JobFilter,
    ) -> Result<(Vec<JobSummary>, Option<String>), ErrorBody> {
        let s = &self.shared;
        let limit = f.limit.unwrap_or(100);
        if !(1..=500).contains(&limit) {
            return Err(params_invalid(
                format!("limit must be 1–500, not {limit}"),
                &ctx.trace_id,
            ));
        }
        let before = match &f.cursor {
            None => None,
            Some(c) => Some(c.parse::<i64>().map_err(|_| {
                params_invalid(format!("{c:?} is not a jobs.list cursor"), &ctx.trace_id)
            })?),
        };
        let scope = s.scope_of(ctx);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        let rows = record::list(scope.db(), &f, limit, before)
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        let next = (rows.len() == limit as usize)
            .then(|| rows.last().map(|r| r.seq.to_string()))
            .flatten();
        Ok((rows.iter().map(|r| s.summary(r)).collect(), next))
    }

    /// Waits until job `id` of `ctx`'s scope is final, or for `timeout`: the job, and whether
    /// it is final (AI-113).
    ///
    /// # Errors
    /// `params.invalid` for a timeout outside 1–90 s; `job.unknown`; `state.unavailable`.
    pub async fn wait(
        &self,
        ctx: &CallCtx,
        id: &JobId,
        timeout: Duration,
    ) -> Result<(JobSummary, bool), ErrorBody> {
        if timeout < Duration::from_secs(1) || timeout > MAX_WAIT {
            return Err(params_invalid(
                format!(
                    "timeout_s must be 1–{}, not {}",
                    MAX_WAIT.as_secs(),
                    timeout.as_secs()
                ),
                &ctx.trace_id,
            ));
        }
        let s = &self.shared;
        let scope = s.scope_of(ctx);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Subscribed before the read, so a change in between wakes the wait.
            let mut changed = s.changed.subscribe();
            let row = record::get(scope.db(), id.as_str())
                .await
                .map_err(|e| state_unavailable(&e, &ctx.trace_id))?
                .ok_or_else(|| job_unknown(id.as_str(), &ctx.trace_id))?;
            if row.status.is_final() {
                return Ok((s.summary(&row), true));
            }
            tokio::select! {
                _ = changed.changed() => {}
                () = tokio::time::sleep_until(deadline) => return Ok((s.summary(&row), false)),
            }
        }
    }

    /// Cancels job `id` of `ctx`'s scope (CORE-146): a queued job is `cancelled` at once; a
    /// running one becomes `cancelling`, its handler is told, and its engine work is
    /// cancelled once; it is `cancelled` when its run ends. A finished job is returned as it
    /// is.
    ///
    /// # Errors
    /// `job.unknown`; `state.unavailable`.
    pub async fn cancel(&self, ctx: &CallCtx, id: &JobId) -> Result<JobSummary, ErrorBody> {
        let s = &self.shared;
        let scope = s.scope_of(ctx);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        let current = |row: Option<JobRow>| {
            row.map(|r| s.summary(&r))
                .ok_or_else(|| job_unknown(id.as_str(), &ctx.trace_id))
        };
        let row = record::get(scope.db(), id.as_str())
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        let Some(row) = row else {
            return current(None);
        };
        if row.status.is_final() {
            return current(Some(row));
        }
        enum Act {
            Nothing,
            Queued(Box<Info>),
            Running(Option<(Arc<JobCtx>, Arc<dyn JobHandler>)>),
        }
        let act = {
            let mut q = s.queue();
            match q.jobs.get_mut(id.as_str()) {
                None => Act::Nothing,
                Some(a) if !a.running => {
                    let a = q.jobs.remove(id.as_str()).expect("the job is queued");
                    Act::Queued(Box::new(a.info))
                }
                Some(a) if a.cancelling => Act::Nothing,
                Some(a) => {
                    a.cancelling = true;
                    a.cancel.send_replace(true);
                    let engine = if a.engine_cancel_sent {
                        None
                    } else {
                        a.ctx.clone().and_then(|c| {
                            a.engine_cancel_sent = true;
                            s.handler(a.info.spec.kind).map(|h| (c, h))
                        })
                    };
                    Act::Running(engine)
                }
            }
        };
        match act {
            Act::Nothing => {}
            Act::Queued(info) => {
                // Out of the queue already, so it never starts; its completions release
                // whatever its call reserved.
                s.complete(&info, row.attempt, End::Cancelled).await;
            }
            Act::Running(engine) => {
                s.apply(id.as_str(), &scope, Change::to(Substatus::Cancelling), None)
                    .await;
                if let Some((ctx, handler)) = engine {
                    handler.cancel_in_engine(&ctx).await;
                }
            }
        }
        let row = record::get(scope.db(), id.as_str())
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        current(row)
    }

    /// Runs failed or cancelled job `id` of `ctx`'s scope again as a new job whose
    /// `retry_of` is `id` (CORE-146). Never an `interrupted_unknown` one.
    ///
    /// # Errors
    /// `job.unknown`; `job.not_retryable` for any other state, or a kind no handler runs;
    /// as [`Jobs::submit`].
    pub async fn retry(&self, ctx: &CallCtx, id: &JobId) -> Result<JobSummary, ErrorBody> {
        let s = &self.shared;
        let scope = s.scope_of(ctx);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?;
        let row = record::get(scope.db(), id.as_str())
            .await
            .map_err(|e| state_unavailable(&e, &ctx.trace_id))?
            .ok_or_else(|| job_unknown(id.as_str(), &ctx.trace_id))?;
        let not_retryable =
            |why: String| ErrorBody::new(&codes::JOB_NOT_RETRYABLE, why, ctx.trace_id.clone());
        if !matches!(row.status, JobStatus::Failed | JobStatus::Cancelled) {
            return Err(not_retryable(format!(
                "Job {id} is {}; only a failed or cancelled job can be retried.",
                row.substatus
                    .map_or_else(|| row.status.wire().to_string(), |s| s.wire())
            )));
        }
        let Some(handler) = s.handler(&row.kind) else {
            return Err(not_retryable(format!(
                "No handler runs jobs of kind {:?} any more.",
                row.kind
            )));
        };
        let idem = ctx.idem_key.as_ref().map(|key| {
            (
                format!("{}|{}", ctx.session.caller, ctx.session.workspace_id()),
                key.clone(),
            )
        });
        let new = s
            .submit_new(
                &scope,
                row.spec(handler.kind()),
                &ctx.trace_id,
                idem,
                ctx.reservation.clone(),
                Some(row.job_id.clone()),
            )
            .await?;
        ctx.submitted.set(JobId(new.job_id.clone()));
        Ok(s.summary(&new))
    }

    /// The startup recovery of `scope` (ARCH-022): the jobs it marked. Every job of the scope
    /// without a final state that this core isn't running becomes `interrupted` (or
    /// `interrupted_unknown`, CORE-128); nothing re-runs. Writes no log line: the core calls it
    /// for the suite `state.db`, whose jobs have none; a workspace is recovered with
    /// [`Jobs::recover_workspace`].
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn recover(&self, scope: &StateDb) -> Result<Vec<JobId>, ErrorBody> {
        self.shared
            .recover_rows(scope, None)
            .await
            .map_err(|e| state_unavailable(&e, ""))
    }

    /// Recovers the workspace `open` once per core (as [`Jobs::recover`], with its jobs log
    /// lines); later calls do nothing. Every job operation in a workspace runs it first.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn recover_workspace(&self, open: &Arc<OpenWorkspace>) -> Result<(), ErrorBody> {
        self.shared
            .ensure_recovered(&Scope::Workspace(open.clone()))
            .await
            .map_err(|e| state_unavailable(&e, ""))
    }

    /// Whether a job is queued or running in this core (ARCH-020: the core isn't idle).
    pub fn busy(&self) -> bool {
        !self.shared.queue().jobs.is_empty()
    }

    /// The `PersistInterrupted` shutdown step (ARCH-021 step 3).
    pub fn persist_interrupted_step(&self) -> Arc<dyn ShutdownStep> {
        Arc::new(PersistInterrupted(self.shared.clone()))
    }
}

impl IdleBlocker for Jobs {
    fn name(&self) -> &'static str {
        "jobs"
    }

    fn busy(&self) -> bool {
        Jobs::busy(self)
    }
}

/// [`Jobs::persist_interrupted_step`].
struct PersistInterrupted(Arc<Shared>);

impl ShutdownStep for PersistInterrupted {
    fn phase(&self) -> ShutdownPhase {
        ShutdownPhase::PersistInterrupted
    }

    fn name(&self) -> &'static str {
        "jobs.persist_interrupted"
    }

    fn run(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(self.0.persist_interrupted())
    }
}

/// The `job:<id>` snapshot: the job's summary, from the scope that holds it (a job this core
/// runs, else the suite `state.db`, else the default workspace's), or `{"error": …}`.
struct JobSnapshots(Weak<Shared>);

impl SnapshotProvider for JobSnapshots {
    fn snapshot<'a>(
        &'a self,
        topic: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            let id = topic.strip_prefix("job:").unwrap_or(topic);
            let unknown = || json!({"error": job_unknown(id, "")});
            let Some(s) = self.0.upgrade() else {
                return unknown();
            };
            let mine = s.queue().jobs.get(id).map(|a| a.info.scope.clone());
            let mut scopes = Vec::new();
            if let Some(scope) = mine {
                scopes.push(scope);
            }
            scopes.push(Scope::Suite(s.states.suite_db()));
            if let Some(ws) = s.hub.default_ref()
                && let Ok(open) = s.hub.open(&ws).await
            {
                scopes.push(Scope::Workspace(open));
            }
            for scope in scopes {
                if let Ok(Some(row)) = record::get(scope.db(), id).await {
                    return serde_json::to_value(s.summary(&row))
                        .expect("a job summary serialises");
                }
            }
            unknown()
        })
    }
}
