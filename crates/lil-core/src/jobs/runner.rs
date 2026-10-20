//! [`Jobs`]: submit, the lanes' dispatch, a job's run, cancel, the retry check, recovery and
//! the shutdown step (module docs of [`crate::jobs`]).
//!
//! **Locks.** One `std` mutex guards the in-memory queue (the jobs this core runs, the lanes);
//! it is never held across an `await`. Each job's state changes are serialised by its own
//! async lock, held across the row update, the jobs log line and the events, so the log and the
//! topics see a job's changes in the order the row took them. A guarded change looks at the
//! queue under that lock (job lock, then queue mutex; never the other way round).
//!
//! **Which jobs a recovery touches.** Only rows that this core isn't running: a submit writes
//! its row and then adds the job to the queue, and the recovery of a scope runs once, before
//! the first job operation in it (or before the core serves, for the suite scope), so it never
//! meets a row of this core's own run.
//!
//! **Draining** (ARCH-021 step 1). Once the lifecycle drains ([`Jobs::stop_on_drain`]), no
//! queued job starts and no interrupted job is re-run; the `jobs` blocker then counts only the
//! jobs that are running or in their completions, so a drain waits for those alone, and the
//! `PersistInterrupted` step marks the queued rest.

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
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use super::queue::{self, Candidate};
use super::record::{self, Change, Inserted, JobRow};
use super::scheduler::watchdog_slow;
use super::{
    Admission, Admit, AlwaysAdmit, COMPLETION_GRACE, DISK_MARGIN_BYTES, ExecWindow, JOB_CANCELLED,
    JOB_NOT_STARTED, JOB_RESOLVED_UNKNOWN, JobCaller, JobCompletion, JobCtx, JobDone, JobFailure,
    JobFilter, JobHandler, JobId, JobOrigin, JobResult, JobSpec, JobStatus, JobSummary, Lane,
    MAX_ATTEMPTS, MAX_WAIT, NoReservations, Priority, Progress, Reservations, RetryPlan, Substatus,
    backoff,
};
use crate::events::{EventBus, SnapshotProvider};
use crate::lifecycle::{IdleBlocker, ShutdownPhase, ShutdownStep};
use crate::pipeline::apply_record;
use crate::state::{StateDb, StateError, StateRegistry};
use crate::store::{FailPoints, LogKind};
use crate::workspace::{Access, OpenWorkspace, WorkspaceHub};

/// The error a cancelled job's completions get as its outcome ([`super::is_cancelled`]).
const CANCELLED: CodeSpec = CodeSpec {
    code: JOB_CANCELLED,
    category: ErrorCategory::Cancelled,
    retry: RetryPolicy::AfterResolve,
    provisional: true,
};

/// The error a resolved `interrupted_unknown` job's completions get
/// ([`super::resolved_unknown`]).
const RESOLVED_UNKNOWN: CodeSpec = CodeSpec {
    code: JOB_RESOLVED_UNKNOWN,
    category: ErrorCategory::Cancelled,
    retry: RetryPolicy::AfterResolve,
    provisional: true,
};

/// The error the completions of a job that never started get ([`super::never_started`]).
const NOT_STARTED: CodeSpec = CodeSpec {
    code: JOB_NOT_STARTED,
    category: ErrorCategory::Cancelled,
    retry: RetryPolicy::AfterResolve,
    provisional: true,
};

/// The outcome error of a cancelled job (module docs of `CANCELLED`): handlers return it when
/// they stop on `JobCtx::cancel`.
pub fn cancelled_error(trace_id: &str) -> ErrorBody {
    ErrorBody::new(&CANCELLED, "The job was cancelled.", trace_id)
}

fn not_started(trace_id: &str) -> Outcome<ToolOutput> {
    Outcome::Error {
        error: ErrorBody::new(
            &NOT_STARTED,
            "The job never started: the core stopped first.",
            trace_id,
        ),
    }
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
    origin: Option<JobOrigin>,
    /// The row's `idem_scope` (an automatic re-run keeps it).
    idem_scope: Option<String>,
    /// The execution windows its attempts reported (`JobCtx::add_window`), shared by every
    /// attempt and its completions.
    windows: Arc<Mutex<Vec<ExecWindow>>>,
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
    /// Its end is being recorded: the completions, the Record step, the final state.
    completing: bool,
    /// Its Record step has begun: the shutdown step leaves it to write its final state.
    recording: bool,
    cancel: watch::Sender<bool>,
    cancelling: bool,
    preempted: bool,
    /// `cancel_in_engine` was called for the current run.
    engine_cancel_sent: bool,
    /// The running attempt's context, once its handler may start.
    ctx: Option<Arc<JobCtx>>,
    progress: Option<Progress>,
    waiting_for_vram: bool,
    /// When the current attempt became `executing` (monotonic): the watchdog's start.
    exec_start: Option<Duration>,
    /// The watchdog flagged it.
    slow: bool,
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
    /// What the admission was last told: a job runs or waits for VRAM.
    busy: bool,
}

/// A state change applies only while the job, as the queue holds it, passes this check.
type Guard = fn(&Active) -> bool;

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
    /// The execution windows of the jobs that ended `interrupted_unknown` in this core run,
    /// for their completions when you resolve them (after a restart they are gone).
    unknown_windows: Mutex<HashMap<String, Arc<Mutex<Vec<ExecWindow>>>>>,
    reservations: RwLock<Arc<dyn Reservations>>,
    /// The lifecycle's drain token ([`Jobs::stop_on_drain`]).
    drain: RwLock<Option<CancellationToken>>,
    preempt: AtomicBool,
    queue: Mutex<Queue>,
    /// Bumped on every state change and whenever a job leaves the queue (`job.wait`, the
    /// shutdown step's wait).
    changed: watch::Sender<u64>,
    recovered: Mutex<HashMap<String, Arc<OnceCell<()>>>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The engine ids of the jobs taken up again from an earlier core run (ruling P3-R21):
    /// their prompts are the suite's, not outside ones.
    adopted: Mutex<HashSet<String>>,
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

/// The tool an agent retries a finished job with (CORE-146; `jobs` names no `tools` item).
const JOB_RETRY_TOOL: &str = "job.retry";

/// How a finished job's error is retried ([`job_error`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryHow {
    /// The core re-runs it itself (an interrupted `agent` job's one re-run).
    Rerun,
    /// After the fix, with `job.retry`.
    ByJobRetry,
    /// After you resolve it (or only you can retry it): no `job.retry` hint.
    Resolve,
}

/// The error a finished job keeps (Appendix F "Retries and recovery"; QA-004, QA-007). Its
/// `retry` is about the job, never about sending the call that made it again: that call's
/// idempotency key gives back this same job (CORE-016). `auto` only while the core re-runs it
/// itself; `never` stays `never`; anything else is `after_resolve`, and with
/// [`RetryHow::ByJobRetry`] `fix.agent_hint` is `job.retry {job_id}` unless the error names a
/// fix of its own.
fn job_error(mut error: ErrorBody, job_id: &str, how: RetryHow) -> ErrorBody {
    if error.retry == RetryPolicy::Never {
        return error;
    }
    if how == RetryHow::Rerun {
        error.retry = RetryPolicy::Auto;
        return error;
    }
    error.retry = RetryPolicy::AfterResolve;
    if how == RetryHow::ByJobRetry {
        let hint = lil_protocol::AgentHint {
            tool: JOB_RETRY_TOOL.to_string(),
            arguments: json!({ "job_id": job_id }),
        };
        match &mut error.fix {
            Some(fix) => {
                fix.agent_hint.get_or_insert(hint);
            }
            None => {
                error.fix = Some(lil_protocol::Fix {
                    ui_action: lil_protocol::UiAction {
                        id: "retry_job".to_string(),
                        label: "Retry the job".to_string(),
                    },
                    agent_hint: Some(hint),
                });
            }
        }
    }
    error
}

/// How the error of a job that ends `failed` or `interrupted` is retried: by `job.retry` when
/// a tool call made it and an agent may retry it (ruling P3-R21: never one with external
/// side effects that was interrupted), else only after you resolve it.
fn retry_of_end(origin: bool, external_side_effects: bool, interrupted: bool) -> RetryHow {
    if origin && !(interrupted && external_side_effects) {
        RetryHow::ByJobRetry
    } else {
        RetryHow::Resolve
    }
}

/// The error a recovery or the shutdown step gives a job it marks `interrupted` or
/// `interrupted_unknown` (Appendix F): any earlier attempt's transient error is replaced.
fn stopped_error(row: &JobRow, how: RetryHow) -> ErrorBody {
    let error = ErrorBody::new(
        &codes::CORE_DRAINING,
        "The core stopped while this job ran.",
        &row.trace_id,
    );
    job_error(error, &row.job_id, how)
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

/// A new job for [`Shared::submit_new`].
struct NewJob {
    spec: JobSpec,
    trace_id: String,
    /// The idempotency scope and key.
    idem: Option<(String, String)>,
    /// The submitting caller's `<caller kind>|<workspace id or none>`, kept on the row even
    /// without a key (the meter's `caller_kind`, CORE-069).
    caller_scope: Option<String>,
    reservation: Option<String>,
    /// Whose reservation it is: the call id, or the job a re-run replaces.
    owner: String,
    retry_of: Option<String>,
    origin: Option<JobOrigin>,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        lock(&self.queue)
    }

    /// Tells the admission, under the queue's lock (so the calls keep the queue's order),
    /// when whether any job runs or waits for VRAM changed.
    fn tell_busy(&self, q: &mut Queue) {
        let busy = q.jobs.values().any(|a| a.running || a.waiting_for_vram);
        if busy != q.busy {
            q.busy = busy;
            read(&self.admission).busy(busy);
        }
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

    /// The registered name of `kind` (a [`JobSpec`] holds a `&'static str`).
    fn static_kind(&self, kind: &str) -> Option<&'static str> {
        self.handlers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get_key_value(kind)
            .map(|(k, _)| *k)
    }

    fn reservations(&self) -> Arc<dyn Reservations> {
        read(&self.reservations)
    }

    /// Whether the lifecycle drains (module docs, "Draining").
    fn draining(&self) -> bool {
        self.drain
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    fn job_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.locks).entry(id.to_string()).or_default().clone()
    }

    /// Returns once a state change of the job that is being announced is done (its jobs log
    /// line and events): they happen under the job's lock, after the row's update.
    async fn announced(&self, id: &str) {
        let held = lock(&self.locks).get(id).cloned();
        if let Some(held) = held {
            drop(held.lock().await);
        }
    }

    fn scope_of(&self, caller: &JobCaller) -> Scope {
        match &caller.workspace {
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
    /// announces it: the row after it, `None` when nothing changed (the row is final or gone,
    /// or `guard` refused it; a guard refuses a job this core doesn't run).
    /// `jobs.after_submitting` fires when the substatus becomes `submitting`.
    async fn apply_checked(
        &self,
        id: &str,
        scope: &Scope,
        c: Change,
        guard: Option<Guard>,
    ) -> Result<Option<JobRow>, StateError> {
        let lock = self.job_lock(id);
        let _serial = lock.lock().await;
        if let Some(guard) = guard
            && !self.queue().jobs.get(id).is_some_and(guard)
        {
            return Ok(None);
        }
        let submitting = c.substatus == Some(Some(Substatus::Submitting));
        let Some((before, row)) = record::update(scope.db(), id, c, self.now()).await? else {
            return Ok(None);
        };
        // The watchdog times only executing: any other substatus (retrying, cancelling,
        // ingesting) stops its clock, and the next `executing` starts it again.
        if let Some(a) = self.queue().jobs.get_mut(id) {
            if row.substatus != Some(Substatus::Executing) {
                a.exec_start = None;
            } else if a.exec_start.is_none() {
                a.exec_start = Some(self.clock.monotonic());
            }
        }
        self.announce(scope.workspace(), &row).await;
        if submitting
            && before != Some(Substatus::Submitting)
            && let Err(e) = self.failpoints.hit("jobs.after_submitting")
        {
            tracing::warn!(job_id = id, error = %e, "fail point jobs.after_submitting");
        }
        Ok(Some(row))
    }

    /// [`Shared::apply_checked`], with a write error logged.
    async fn apply(
        &self,
        id: &str,
        scope: &Scope,
        c: Change,
        guard: Option<Guard>,
    ) -> Option<JobRow> {
        match self.apply_checked(id, scope, c, guard).await {
            Ok(row) => row,
            Err(e) => {
                tracing::error!(job_id = id, error = %e, "a job's state change could not be written");
                None
            }
        }
    }

    /// The context a job's completions get for `row`, outside a run (recovery, the shutdown
    /// step, resolving an `interrupted_unknown` job). `None` (logged) when no handler runs its
    /// kind any more.
    fn ctx_from_row(self: &Arc<Self>, row: &JobRow, scope: &Scope) -> Option<JobCtx> {
        let Some(kind) = self.static_kind(&row.kind) else {
            tracing::error!(job_id = %row.job_id, kind = %row.kind, "no handler runs this kind any more; the job's completions can't run");
            return None;
        };
        Some(JobCtx {
            id: JobId(row.job_id.clone()),
            spec: row.spec(kind),
            attempt: row.attempt,
            trace_id: row.trace_id.clone(),
            workspace: scope.workspace().cloned(),
            events: self.events.clone(),
            cancel: watch::Sender::new(false).subscribe(),
            state: scope.db_arc(),
            reservation: row.reservation.clone(),
            windows: Arc::default(),
            jobs: self.clone(),
        })
    }

    /// Runs the completions, in registration order.
    async fn run_completions(
        &self,
        ctx: &JobCtx,
        outcome: &Outcome<ToolOutput>,
        done: Option<&JobDone>,
    ) {
        for c in read(&self.completions) {
            c.completed(ctx, outcome, done).await;
        }
    }

    /// The recovery of one scope (ARCH-022): every row without a final status that this core
    /// isn't running becomes `interrupted`, or `interrupted_unknown` when it was `submitting`
    /// with external side effects. A row already `interrupted_unknown` stays as it is. The
    /// completions of a job that never started run with a [`super::never_started`] outcome
    /// (its reservation is released); one that had started keeps its reservation.
    async fn recover_rows(self: &Arc<Self>, scope: &Scope) -> Result<Vec<JobId>, StateError> {
        let rows = record::open_rows(scope.db()).await?;
        let mine: HashSet<String> = self.queue().jobs.keys().cloned().collect();
        let mut marked = Vec::new();
        for row in rows {
            if mine.contains(&row.job_id) || row.substatus == Some(Substatus::InterruptedUnknown) {
                continue;
            }
            if self.adopt(&row, scope) {
                continue;
            }
            let c = interrupt_change(&row, self.now());
            if let Some((before, after)) =
                record::update(scope.db(), &row.job_id, c, self.now()).await?
            {
                tracing::warn!(job_id = %after.job_id, substatus = ?after.substatus, "a job of an earlier core run is interrupted; it doesn't run again");
                self.announce(scope.workspace(), &after).await;
                if never_ran(before)
                    && let Some(ctx) = self.ctx_from_row(&after, scope)
                {
                    self.run_completions(&ctx, &not_started(&after.trace_id), None)
                        .await;
                }
                marked.push(JobId(after.job_id));
            }
        }
        Ok(marked)
    }

    /// Recovers `scope` once (module docs): the jobs it marked, empty on every later call. A
    /// read-only workspace is left as it is.
    /// Ruling P3-R21: a job with external side effects that an earlier core left in its
    /// engine (`queued_in_engine`, `executing` or `ingesting`, with an `external_id`) is
    /// taken up again rather than marked: its handler asks the engine about that id first
    /// (CORE-120), follows a prompt that is still there, ingests one that finished, and ends
    /// any other `interrupted_unknown`; it never posts it again. `true` when it was queued.
    fn adopt(self: &Arc<Self>, row: &JobRow, scope: &Scope) -> bool {
        let in_engine = matches!(
            row.substatus,
            Some(Substatus::QueuedInEngine | Substatus::Executing | Substatus::Ingesting)
        );
        if !(row.external_side_effects
            && in_engine
            && row.external_id.is_some()
            && row.engine.is_some()
            && !self.draining())
        {
            return false;
        }
        let Some(kind) = self.static_kind(&row.kind) else {
            return false;
        };
        tracing::warn!(job_id = %row.job_id, external_id = ?row.external_id, "a job with external side effects of an earlier core run is taken up again in its engine");
        if let Some(id) = &row.external_id {
            lock(&self.adopted).insert(id.clone());
        }
        let info = Info {
            id: JobId(row.job_id.clone()),
            spec: row.spec(kind),
            scope: scope.clone(),
            trace_id: row.trace_id.clone(),
            reservation: row.reservation.clone(),
            retry_of: row.retry_of.clone(),
            origin: row.origin.clone(),
            idem_scope: row.idem_scope.clone(),
            windows: Arc::default(),
        };
        self.enqueue(info, false)
    }

    async fn ensure_recovered(self: &Arc<Self>, scope: &Scope) -> Result<Vec<JobId>, StateError> {
        if let Scope::Workspace(open) = scope
            && open.access == Access::ReadOnly
        {
            return Ok(Vec::new());
        }
        let cell = lock(&self.recovered)
            .entry(scope.key())
            .or_default()
            .clone();
        let mut marked = Vec::new();
        cell.get_or_try_init(|| async {
            marked = self.recover_rows(scope).await?;
            Ok::<(), StateError>(())
        })
        .await?;
        Ok(marked)
    }

    /// A new job row, queued (ARCH-034): written and logged before the job can start, and its
    /// reservation handed over to it. A job that holds `idem` already is returned instead when
    /// its arguments match (and the new call's reservation is released: it submitted nothing),
    /// else `idempotency.conflict` (CORE-016).
    async fn submit_new(self: &Arc<Self>, scope: &Scope, new: NewJob) -> Result<JobRow, ErrorBody> {
        let NewJob {
            spec,
            trace_id,
            idem,
            caller_scope,
            reservation,
            owner,
            retry_of,
            origin,
        } = new;
        let trace_id = trace_id.as_str();
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
        let (key_scope, idem_key) = idem.unzip();
        let idem_scope = key_scope.or(caller_scope);
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
            origin: origin.clone(),
            external_id: None,
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
                if existing.args_hash != hash {
                    return Err(ErrorBody::new(
                        &codes::IDEMPOTENCY_CONFLICT,
                        "The idempotency key was used before for another job.",
                        trace_id,
                    ));
                }
                if let Some(rv) = &reservation {
                    tracing::info!(job_id = %existing.job_id, reservation = %rv, "the key's job exists; the call's own reservation is released");
                    self.reservations().release(rv, &owner).await;
                }
                return Ok(existing);
            }
            Ok(Inserted::New(row)) => row,
        };
        self.announce(scope.workspace(), &row).await;
        let job_id = JobId(id.clone());
        if let Some(rv) = &reservation {
            self.reservations().hand_over(rv, &owner, &job_id).await;
        }
        drop(guard);
        let info = Info {
            id: job_id,
            spec,
            scope: scope.clone(),
            trace_id: trace_id.to_string(),
            reservation,
            retry_of,
            origin,
            idem_scope: row.idem_scope.clone(),
            windows: Arc::default(),
        };
        self.enqueue(info, true);
        Ok(row)
    }

    /// Adds `info`'s job to the queue (with the CORE-125 preemption when `preempt` allows it)
    /// and starts what can start. After the shutdown step nothing is added: the row stays
    /// queued, and the next start recovers it. `false` then.
    fn enqueue(self: &Arc<Self>, info: Info, preempt: bool) -> bool {
        let id = info.id.to_string();
        let preempt = {
            let mut q = self.queue();
            if q.closed {
                // The shutdown step has run: the row stays queued, and the next start
                // recovers it as interrupted (and releases its reservation).
                lock(&self.locks).remove(&id);
                return false;
            }
            q.next_order += 1;
            let order = q.next_order;
            let preempt = if preempt {
                self.preemption(&mut q, &info.spec)
            } else {
                None
            };
            q.jobs.insert(
                id,
                Active {
                    info,
                    order,
                    head: false,
                    overtaken: 0,
                    running: false,
                    completing: false,
                    recording: false,
                    cancel: watch::Sender::new(false),
                    cancelling: false,
                    preempted: false,
                    engine_cancel_sent: false,
                    ctx: None,
                    progress: None,
                    waiting_for_vram: false,
                    exec_start: None,
                    slow: false,
                },
            );
            preempt
        };
        if let Some((ctx, handler)) = preempt {
            tokio::spawn(async move { handler.cancel_in_engine(&ctx).await });
        }
        self.pump();
        true
    }

    /// CORE-125: with `preempt_agent_batches`, an `interactive` job cancels the running
    /// `agent` engine job of its lane; that job goes back to the head of its class when its
    /// run ends. A `user` job is never touched, nor a job with external side effects (it is
    /// never re-run automatically, CORE-128, ARCH-022). Returns the engine cancel to run.
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
            || a.info.spec.external_side_effects
            || a.cancelling
            || a.preempted
            || a.completing
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

    /// Starts the next job of each free lane (CORE-121, CORE-123, CORE-124); none while the
    /// core drains.
    fn pump(self: &Arc<Self>) {
        if self.draining() {
            return;
        }
        let admission = read(&self.admission);
        let mut start = Vec::new();
        let mut wait = Vec::new();
        let mut rewait = Vec::new();
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
                        .filter(|(_, a)| !a.running && !a.completing && a.info.spec.lane == lane)
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
                // A job that waited for VRAM but is no longer the lane's pick waits for its
                // turn again (`waiting`).
                for (other, a) in &mut q.jobs {
                    if a.waiting_for_vram && a.info.spec.lane == lane && *other != id {
                        a.waiting_for_vram = false;
                        rewait.push((other.clone(), a.info.scope.clone()));
                    }
                }
                let a = q.jobs.get_mut(&id).expect("a picked job is queued");
                match admission.admit(&a.info.spec) {
                    Admit::Wait => {
                        // It waits for its engine, not for VRAM: `waiting` again.
                        if a.waiting_for_vram {
                            a.waiting_for_vram = false;
                            rewait.push((id, a.info.scope.clone()));
                        }
                    }
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
            self.tell_busy(&mut q);
        }
        for (id, scope) in wait {
            let shared = self.clone();
            tokio::spawn(async move {
                // Only while it still waits: a job admitted meanwhile is `submitting` already.
                let still_waiting: Guard = |a| a.waiting_for_vram && !a.running && !a.completing;
                shared
                    .apply(
                        &id,
                        &scope,
                        Change::to(Substatus::WaitingForVram),
                        Some(still_waiting),
                    )
                    .await;
            });
        }
        for (id, scope) in rewait {
            let shared = self.clone();
            tokio::spawn(async move {
                // Only while it still waits for its turn: not picked and waiting again since.
                let overtaken: Guard = |a| !a.waiting_for_vram && !a.running && !a.completing;
                shared
                    .apply(&id, &scope, Change::to(Substatus::Waiting), Some(overtaken))
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
            windows: info.windows.clone(),
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
            loop {
                // The watchdog measures each attempt from its own `execution_start`.
                if let Some(a) = self.queue().jobs.get_mut(&id) {
                    a.exec_start = None;
                }
                let initial = if info.spec.engine.is_some() {
                    Substatus::Submitting
                } else {
                    Substatus::Executing
                };
                let mut c = Change::to(initial);
                c.attempt = Some(attempt);
                c.started_utc = Some(self.now());
                // Only ever set: a job that ran unchecked once (a run before a preemption) stays
                // marked.
                if unchecked {
                    c.vram_unchecked = Some(true);
                }
                // CORE-120: the attempt's state (`submitting` for an engine job) is persisted
                // before the handler runs; a job being cancelled keeps `cancelling`.
                let live: Guard = |a| !a.cancelling && !a.preempted;
                match self.apply_checked(&id, &info.scope, c, Some(live)).await {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        let (cancelling, preempted) = self.flags(&id);
                        if cancelling {
                            return self.complete(&info, attempt, End::Cancelled).await;
                        }
                        if preempted {
                            return self.requeue(&info).await;
                        }
                        tracing::warn!("a job's row is final or gone before its attempt; it doesn't run");
                        self.leave(&info);
                        return self.pump();
                    }
                    Err(e) => {
                        let error = state_unavailable(&e, &info.trace_id);
                        return self.fail_unstarted(&info, error).await;
                    }
                }
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
                    // The attempt is over: neither its backoff nor its end counts (watchdog).
                    a.exec_start = None;
                }
                let (cancelling, preempted) = self.flags(&id);
                // An unknown outcome stays unknown, and an engine that lost the job is an
                // interruption, whatever cancel or preemption came meanwhile: a cancel never
                // hides a charge that may have happened, the outputs salvaged are kept, and
                // neither is ever requeued (nor re-run, when it was being cancelled).
                let failure = match result {
                    Ok(done) => return self.complete(&info, attempt, End::Succeeded(done)).await,
                    Err(JobFailure::Unknown(error)) => {
                        return self.complete(&info, attempt, End::Unknown(error)).await;
                    }
                    Err(JobFailure::Interrupted { error, salvaged }) => {
                        return self.complete(&info, attempt, End::Interrupted { error, salvaged }).await;
                    }
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
                        self.apply(&id, &info.scope, c, Some(live)).await;
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

    /// A job whose attempt couldn't be persisted (I8, CORE-120): its handler never ran. It is
    /// written `failed` with `error`, and only once that write is in do its completions run
    /// (with a [`super::never_started`] outcome); when the database refuses that too, the row
    /// is left for the next start's recovery, which runs them.
    async fn fail_unstarted(self: &Arc<Self>, info: &Info, error: ErrorBody) {
        if let Some(a) = self.queue().jobs.get_mut(info.id.as_str()) {
            a.completing = true;
        }
        let mut c = Change::finished(JobStatus::Failed);
        let how = retry_of_end(
            info.origin.is_some(),
            info.spec.external_side_effects,
            false,
        );
        c.error = Some(Some(job_error(error, info.id.as_str(), how)));
        c.finished_utc = Some(self.now());
        if let Some(row) = self.apply(info.id.as_str(), &info.scope, c, None).await
            && let Some(ctx) = self.ctx_from_row(&row, &info.scope)
        {
            self.run_completions(&ctx, &not_started(&info.trace_id), None)
                .await;
        }
        self.leave(info);
        self.pump();
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
                a.exec_start = None;
                a.cancel = watch::Sender::new(false);
            }
            self.tell_busy(&mut q);
        }
        self.pump();
    }

    /// Whether the job is still in its completions in this core: the shutdown step gives up on
    /// a job that took longer than [`COMPLETION_GRACE`], and then nothing more is written.
    fn still_completing(&self, id: &str) -> bool {
        self.queue().jobs.get(id).is_some_and(|a| a.completing)
    }

    /// A job ends: a job that worked is written `ingesting` with its result first (module
    /// docs, "Crash windows"); then its completions in registration order (none for
    /// `interrupted_unknown`), then the Record step of a job that worked, then its final
    /// state; it leaves the queue and the lane's next job starts. A job that ends after the
    /// shutdown step records nothing.
    async fn complete(self: &Arc<Self>, info: &Info, attempt: u32, end: End) {
        {
            let mut q = self.queue();
            // A job whose end began before the shutdown step (a queued job being cancelled)
            // still ends: the step waits for it.
            let begun = q.jobs.get(info.id.as_str()).is_some_and(|a| a.completing);
            if q.closed && !begun {
                tracing::info!(job_id = %info.id, "a job ended after the shutdown step; its end is not recorded");
                return;
            }
            if let Some(a) = q.jobs.get_mut(info.id.as_str()) {
                a.completing = true;
            }
        }
        let id = info.id.as_str();
        // CORE-128, ARCH-022: only local work is re-run automatically, and never while the
        // core drains. The re-run takes the reservation over.
        // A job you were cancelling when its engine lost it is not run again.
        let rerun = matches!(end, End::Interrupted { .. })
            && !self.flags(id).0
            && info.spec.priority == Priority::Agent
            && info.retry_of.is_none()
            && !info.spec.external_side_effects
            && !self.draining();
        if let End::Succeeded(done) = &end {
            lock(&info.windows).extend(done.windows.iter().cloned());
            let mut c = Change::to(Substatus::Ingesting);
            c.result = Some(JobResult {
                structured: done.structured.clone(),
                artifacts: done.artifacts.clone(),
            });
            self.apply(id, &info.scope, c, None).await;
        }
        let mut ctx = self.ctx_for(info, attempt, watch::Sender::new(false).subscribe());
        if rerun {
            ctx.reservation = None;
        }
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
        if matches!(end, End::Unknown(_)) {
            lock(&self.unknown_windows).insert(id.to_string(), info.windows.clone());
        } else {
            let done = match &end {
                End::Succeeded(done) => Some(done),
                _ => None,
            };
            self.run_completions(&ctx, &outcome, done).await;
        }
        if !self.still_completing(id) {
            tracing::warn!(
                job_id = id,
                "the shutdown step marked the job interrupted during its completions; its end is not recorded"
            );
            return;
        }
        let now = self.now();
        let c = match end {
            End::Succeeded(done) => match self.recorded(info, done, now).await {
                Some(c) => c,
                None => {
                    tracing::warn!(
                        job_id = id,
                        "the shutdown step marked the job interrupted before its Record step; its end is not recorded"
                    );
                    return;
                }
            },
            End::Failed(error) => {
                let mut c = Change::finished(JobStatus::Failed);
                let how = retry_of_end(
                    info.origin.is_some(),
                    info.spec.external_side_effects,
                    false,
                );
                c.error = Some(Some(job_error(error, id, how)));
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
                let mut c = Change::to(Substatus::Interrupted);
                let how = if rerun {
                    RetryHow::Rerun
                } else {
                    retry_of_end(info.origin.is_some(), info.spec.external_side_effects, true)
                };
                c.error = Some(Some(job_error(error, id, how)));
                c.result = Some(JobResult {
                    structured: json!({}),
                    artifacts: salvaged,
                });
                c.finished_utc = Some(now);
                c
            }
            End::Unknown(error) => {
                // QA-007: `after_resolve`; it is retried by no one until you resolve it.
                let mut c = Change::to(Substatus::InterruptedUnknown);
                c.error = Some(Some(job_error(error, id, RetryHow::Resolve)));
                c
            }
        };
        self.apply(id, &info.scope, c, None).await;
        self.leave(info);
        if rerun {
            tracing::info!(
                job_id = id,
                "an interrupted agent job runs once more as a new job"
            );
            let new = NewJob {
                spec: info.spec.clone(),
                trace_id: info.trace_id.clone(),
                idem: None,
                caller_scope: info.idem_scope.clone(),
                reservation: info.reservation.clone(),
                owner: id.to_string(),
                retry_of: Some(id.to_string()),
                origin: info.origin.clone(),
            };
            if let Err(e) = self.submit_new(&info.scope, new).await {
                tracing::warn!(job_id = id, code = %e.code, "the re-run of an interrupted agent job could not be queued");
                if let Some(rv) = &info.reservation {
                    self.reservations().release(rv, id).await;
                }
            }
        }
        self.pump();
    }

    /// The final change of a job that worked: its Record step first (CORE-015 step 9), whose
    /// hand-off block and warnings go into the result, as the inline Record stage does. A
    /// Record step that fails before the manifest edit fails the job. `None`: the shutdown
    /// step gave up on the job before its Record step began, so nothing is recorded.
    async fn recorded(&self, info: &Info, mut done: JobDone, now: i64) -> Option<Change> {
        let mut structured = std::mem::take(&mut done.structured);
        if let Some(plan) = done.record.take() {
            // Claimed under the queue's lock: from here on the shutdown step leaves the job to
            // write its own final state, so the row agrees with the recorded run.
            {
                let mut q = self.queue();
                match q.jobs.get_mut(info.id.as_str()) {
                    Some(a) if a.completing => a.recording = true,
                    _ => return None,
                }
            }
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
                    return Some(c);
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
        Some(c)
    }

    /// The job leaves the queue and its lane.
    fn leave(&self, info: &Info) {
        {
            let mut q = self.queue();
            let lane = info.spec.lane.index();
            if q.lanes[lane].running.as_deref() == Some(info.id.as_str()) {
                q.lanes[lane].running = None;
            }
            q.jobs.remove(info.id.as_str());
            self.tell_busy(&mut q);
        }
        lock(&self.locks).remove(info.id.as_str());
        self.changed.send_modify(|n| *n += 1);
    }

    /// `JobCtx::set_substatus`.
    pub(crate) async fn handler_substatus(&self, id: &JobId, s: Substatus) {
        let scope = {
            let q = self.queue();
            match q.jobs.get(id.as_str()) {
                Some(a) if a.running => a.info.scope.clone(),
                _ => return,
            }
        };
        let live: Guard = |a| a.running && !a.cancelling && !a.completing;
        self.apply(id.as_str(), &scope, Change::to(s), Some(live))
            .await;
    }

    /// `JobCtx::set_external_id`: a guarded update of the running job's row (never a final
    /// one), without an announcement.
    pub(crate) async fn handler_external_id(
        &self,
        id: &JobId,
        external_id: Option<&str>,
        trace_id: &str,
    ) -> Result<(), ErrorBody> {
        let scope = {
            let q = self.queue();
            match q.jobs.get(id.as_str()) {
                Some(a) if a.running && !a.completing => a.info.scope.clone(),
                _ => return Err(job_unknown(id.as_str(), trace_id)),
            }
        };
        let lock = self.job_lock(id.as_str());
        let _serial = lock.lock().await;
        let c = Change {
            external_id: Some(external_id.map(str::to_string)),
            ..Change::default()
        };
        match record::update(scope.db(), id.as_str(), c, self.now()).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(job_unknown(id.as_str(), trace_id)),
            Err(e) => Err(state_unavailable(&e, trace_id)),
        }
    }

    /// `JobCtx::uuid`.
    pub(crate) fn uuid(&self) -> String {
        self.ids.uuid()
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

    /// The watchdog (ARCH-072, CORE-126): a running job past [`watchdog_slow`] since its
    /// attempt became `executing` is flagged `watchdog: "slow"` once, with a `watchdog` event
    /// on `job:<id>`. It never cancels.
    fn watchdog(self: &Arc<Self>) {
        let now = self.clock.monotonic();
        let slow: Vec<(String, Scope)> = {
            let mut q = self.queue();
            q.jobs
                .iter_mut()
                .filter(|(_, a)| a.running && !a.completing && !a.slow)
                .filter(|(_, a)| {
                    a.exec_start.is_some_and(|start| {
                        watchdog_slow(a.info.spec.expected_secs, now.saturating_sub(start))
                    })
                })
                .map(|(id, a)| {
                    a.slow = true;
                    (id.clone(), a.info.scope.clone())
                })
                .collect()
        };
        for (id, scope) in slow {
            let shared = self.clone();
            tokio::spawn(async move {
                tracing::warn!(job_id = %id, "a job runs more than 3 times its expected time; it keeps running");
                let c = Change {
                    watchdog: Some("slow".to_string()),
                    ..Change::default()
                };
                // Still in the attempt that was slow (not in a backoff, not ending).
                let live: Guard = |a| a.running && !a.completing && a.exec_start.is_some();
                if shared.apply(&id, &scope, c, Some(live)).await.is_some() {
                    shared.events.publish(
                        &format!("job:{id}"),
                        json!({"kind": "watchdog", "watchdog": "slow"}),
                    );
                }
            });
        }
    }

    /// Marks a job of this core that a shutdown leaves behind, as a recovery would; the
    /// completions of one that never started run (module docs, "Reservations").
    async fn persist_one(self: &Arc<Self>, info: &Info, started: bool) {
        let row = match record::get(info.scope.db(), info.id.as_str()).await {
            Ok(Some(row)) if !row.status.is_final() => row,
            Ok(_) => return,
            Err(e) => {
                tracing::error!(job_id = %info.id, error = %e, "a running job could not be marked interrupted");
                return;
            }
        };
        let never = !started && never_ran(row.substatus);
        let c = interrupt_change(&row, self.now());
        if let Some(after) = self.apply(info.id.as_str(), &info.scope, c, None).await
            && never
            && let Some(ctx) = self.ctx_from_row(&after, &info.scope)
        {
            self.run_completions(&ctx, &not_started(&info.trace_id), None)
                .await;
        }
    }

    /// The `PersistInterrupted` shutdown step (ARCH-021 step 3): nothing starts or ends any
    /// more; the jobs still queued or running are marked as a recovery would mark them; a job
    /// in its completions gets [`COMPLETION_GRACE`] to finish, and is then marked the same
    /// way (its result, written before its completions, is kept as salvaged artifacts).
    async fn persist_interrupted(self: &Arc<Self>) {
        let left: Vec<(Info, bool)> = {
            let mut q = self.queue();
            q.closed = true;
            q.lanes = Default::default();
            let ids: Vec<String> = q
                .jobs
                .iter()
                .filter(|(_, a)| !a.completing)
                .map(|(id, _)| id.clone())
                .collect();
            let left = ids
                .into_iter()
                .filter_map(|id| q.jobs.remove(&id))
                .map(|a| {
                    a.cancel.send_replace(true);
                    (a.info, a.running)
                })
                .collect();
            self.tell_busy(&mut q);
            left
        };
        for (info, started) in &left {
            self.persist_one(info, *started).await;
        }
        let deadline = tokio::time::Instant::now() + COMPLETION_GRACE;
        loop {
            let mut changed = self.changed.subscribe();
            if !self.queue().jobs.values().any(|a| a.completing) {
                return;
            }
            tokio::select! {
                _ = changed.changed() => {}
                () = tokio::time::sleep_until(deadline) => break,
            }
        }
        // A job in its Record step writes its own final state (module docs, "Crash windows").
        let late: Vec<Info> = {
            let mut q = self.queue();
            let ids: Vec<String> = q
                .jobs
                .iter()
                .filter(|(_, a)| !a.recording)
                .map(|(id, _)| id.clone())
                .collect();
            let late = ids
                .into_iter()
                .filter_map(|id| q.jobs.remove(&id))
                .map(|a| a.info)
                .collect();
            self.tell_busy(&mut q);
            late
        };
        for info in &late {
            tracing::warn!(job_id = %info.id, "a job's completions did not finish before the shutdown; it is marked interrupted");
            self.persist_one(info, true).await;
        }
    }
}

/// Whether a job in `substatus` never started (it was waiting for its turn or for VRAM).
fn never_ran(substatus: Option<Substatus>) -> bool {
    matches!(
        substatus,
        Some(Substatus::Waiting | Substatus::WaitingForVram)
    )
}

/// How a job without a final state is interrupted (ARCH-022, CORE-128).
/// A job with external side effects whose work may have happened is `interrupted_unknown`,
/// never plain `interrupted`: it was `submitting`, `executing` or `ingesting` (an engine job or
/// not, ruling P3-R29), or its row holds an engine id (a recovery that couldn't take it up, an
/// adopted job waiting for its turn; ruling P3-R21, Task 12 re-review N4). Either way the row
/// gets [`stopped_error`] in place of any earlier attempt's error.
fn interrupt_change(row: &JobRow, now: i64) -> Change {
    let maybe_sent = matches!(
        row.substatus,
        Some(Substatus::Submitting | Substatus::Executing | Substatus::Ingesting)
    ) || row.external_id.is_some();
    if row.external_side_effects && maybe_sent {
        let mut c = Change::to(Substatus::InterruptedUnknown);
        c.error = Some(Some(stopped_error(row, RetryHow::Resolve)));
        c
    } else {
        let mut c = Change::to(Substatus::Interrupted);
        let how = retry_of_end(row.origin.is_some(), row.external_side_effects, true);
        c.error = Some(Some(stopped_error(row, how)));
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
            unknown_windows: Mutex::new(HashMap::new()),
            reservations: RwLock::new(Arc::new(NoReservations)),
            drain: RwLock::new(None),
            preempt: AtomicBool::new(false),
            queue: Mutex::new(Queue::default()),
            changed: watch::Sender::new(0),
            recovered: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            adopted: Mutex::new(HashSet::new()),
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

    /// Replaces the admission (the core sets the GPU scheduler).
    pub fn set_admission(&self, a: Arc<dyn Admission>) {
        *self
            .shared
            .admission
            .write()
            .unwrap_or_else(PoisonError::into_inner) = a;
    }

    /// Adds a completion; they run in the order they were added (Meter before Caps). Add them
    /// before the core's first recovery, which runs them for jobs that never started.
    pub fn add_completion(&self, c: Arc<dyn JobCompletion>) {
        self.shared
            .completions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(c);
    }

    /// Replaces the reservation hand-over (Task 16; [`NoReservations`] until then).
    pub fn set_reservations(&self, r: Arc<dyn Reservations>) {
        *self
            .shared
            .reservations
            .write()
            .unwrap_or_else(PoisonError::into_inner) = r;
    }

    /// From the moment `drain` is cancelled (the lifecycle's drain token), no queued job
    /// starts and no interrupted job is re-run (module docs, "Draining").
    pub fn stop_on_drain(&self, drain: CancellationToken) {
        *self
            .shared
            .drain
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(drain);
    }

    /// `[jobs] preempt_agent_batches` (CORE-125; default `false`).
    pub fn set_preempt_agent_batches(&self, on: bool) {
        self.shared.preempt.store(on, Ordering::SeqCst);
    }

    /// Looks at the lanes again (a job waiting for VRAM may start now) and runs the watchdog.
    /// The core calls it on every telemetry sample; it never blocks.
    pub fn kick(&self) {
        self.shared.pump();
        self.shared.watchdog();
    }

    /// Submits `spec` in `caller`'s scope and returns it `queued` (ARCH-034): its row and its
    /// jobs log line are written first, and the call's reservation passes to it. With an
    /// idempotency key, the job that key made before is returned (CORE-016). A retry's job
    /// (`caller.retry_of`) is in the retrying caller's class (Appendix F). Sets
    /// `caller.submitted`.
    ///
    /// # Errors
    /// `idempotency.conflict`, `core.draining` after the shutdown step, `state.unavailable`,
    /// `core.bug` for a kind no handler runs or a second job from one call.
    pub async fn submit(
        &self,
        caller: &JobCaller,
        mut spec: JobSpec,
    ) -> Result<JobSummary, ErrorBody> {
        let s = &self.shared;
        if let Some(first) = caller.submitted.get() {
            return Err(ErrorBody::new(
                &codes::CORE_BUG,
                format!(
                    "A tool call submitted a second job after {first}; one call submits at most one. This is a bug in LILITH."
                ),
                caller.trace_id.clone(),
            ));
        }
        let scope = s.scope_of(caller);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, &caller.trace_id))?;
        if caller.retry_of.is_some() {
            spec.priority =
                Priority::for_caller(&caller.caller, spec.priority == Priority::Interactive);
        }
        let new = NewJob {
            spec,
            trace_id: caller.trace_id.clone(),
            idem: caller
                .idem_key
                .as_ref()
                .map(|key| (caller.idem_scope.clone(), key.clone())),
            caller_scope: Some(caller.idem_scope.clone()),
            reservation: caller.reservation.clone(),
            owner: caller.call_id.clone(),
            retry_of: caller.retry_of.as_ref().map(|j| j.0.clone()),
            origin: caller.origin.clone(),
        };
        let row = s.submit_new(&scope, new).await?;
        caller.submitted.set(JobId(row.job_id.clone()));
        Ok(s.summary(&row))
    }

    /// Job `id` of `caller`'s scope.
    ///
    /// # Errors
    /// `job.unknown`; `state.unavailable`.
    pub async fn get(&self, caller: &JobCaller, id: &JobId) -> Result<JobSummary, ErrorBody> {
        let s = &self.shared;
        let scope = s.scope_of(caller);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, &caller.trace_id))?;
        let row = record::get(scope.db(), id.as_str())
            .await
            .map_err(|e| state_unavailable(&e, &caller.trace_id))?
            .ok_or_else(|| job_unknown(id.as_str(), &caller.trace_id))?;
        Ok(s.summary(&row))
    }

    /// The jobs of `caller`'s scope that `f` selects, newest first, and the cursor of the next
    /// page (`None` on the last).
    ///
    /// # Errors
    /// `params.invalid` for a `limit` outside 1–500 or a cursor this didn't give;
    /// `state.unavailable`.
    pub async fn list(
        &self,
        caller: &JobCaller,
        f: JobFilter,
    ) -> Result<(Vec<JobSummary>, Option<String>), ErrorBody> {
        let s = &self.shared;
        let trace_id = &caller.trace_id;
        let limit = f.limit.unwrap_or(100);
        if !(1..=500).contains(&limit) {
            return Err(params_invalid(
                format!("limit must be 1–500, not {limit}"),
                trace_id,
            ));
        }
        let before = match &f.cursor {
            None => None,
            Some(c) => Some(c.parse::<i64>().map_err(|_| {
                params_invalid(format!("{c:?} is not a jobs.list cursor"), trace_id)
            })?),
        };
        let scope = s.scope_of(caller);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?;
        // One row more than the page: there is a next page only when it exists.
        let mut rows = record::list(scope.db(), &f, limit + 1, before)
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next = more
            .then(|| rows.last().map(|r| r.seq.to_string()))
            .flatten();
        Ok((rows.iter().map(|r| s.summary(r)).collect(), next))
    }

    /// Waits until job `id` of `caller`'s scope is done, or for `timeout`: the job, and whether
    /// it is done (AI-113). Done is final, or `interrupted_unknown`: such a job changes only
    /// when you resolve it, so waiting on it would never end (Appendix F, `job.wait`).
    ///
    /// # Errors
    /// `params.invalid` for a timeout outside 1–90 s; `job.unknown`; `state.unavailable`.
    pub async fn wait(
        &self,
        caller: &JobCaller,
        id: &JobId,
        timeout: Duration,
    ) -> Result<(JobSummary, bool), ErrorBody> {
        let trace_id = &caller.trace_id;
        if timeout < Duration::from_secs(1) || timeout > MAX_WAIT {
            return Err(params_invalid(
                format!(
                    "timeout_s must be 1–{}, not {}",
                    MAX_WAIT.as_secs(),
                    timeout.as_secs()
                ),
                trace_id,
            ));
        }
        let s = &self.shared;
        let scope = s.scope_of(caller);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Subscribed before the read, so a change in between wakes the wait.
            let mut changed = s.changed.subscribe();
            let row = record::get(scope.db(), id.as_str())
                .await
                .map_err(|e| state_unavailable(&e, trace_id))?
                .ok_or_else(|| job_unknown(id.as_str(), trace_id))?;
            if row.status.is_final() || row.substatus == Some(Substatus::InterruptedUnknown) {
                // Its jobs log line is written once `job.wait` says it is done.
                s.announced(id.as_str()).await;
                return Ok((s.summary(&row), true));
            }
            tokio::select! {
                _ = changed.changed() => {}
                () = tokio::time::sleep_until(deadline) => return Ok((s.summary(&row), false)),
            }
        }
    }

    /// Cancels job `id` of `caller`'s scope (CORE-146): a queued job is `cancelled` at once; a
    /// running one becomes `cancelling`, its handler is told, and its engine work is
    /// cancelled once; it is `cancelled` when its run ends. A job in its completions, or
    /// finished, is returned as it is. An `interrupted_unknown` job is yours to resolve
    /// (CORE-088, AI-029): from the app this makes it `cancelled` and runs its completions,
    /// with its reservation; an agent gets `confirm.unavailable`.
    ///
    /// # Errors
    /// `job.unknown`; `confirm.unavailable`; `state.unavailable`.
    pub async fn cancel(&self, caller: &JobCaller, id: &JobId) -> Result<JobSummary, ErrorBody> {
        let s = &self.shared;
        let trace_id = &caller.trace_id;
        let scope = s.scope_of(caller);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?;
        let current = |row: Option<JobRow>| {
            row.map(|r| s.summary(&r))
                .ok_or_else(|| job_unknown(id.as_str(), trace_id))
        };
        let row = record::get(scope.db(), id.as_str())
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?;
        let Some(row) = row else {
            return current(None);
        };
        if row.status.is_final() {
            return current(Some(row));
        }
        let mine = s.queue().jobs.contains_key(id.as_str());
        if !mine && row.substatus == Some(Substatus::InterruptedUnknown) {
            return self.resolve_unknown(caller, &scope, row).await;
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
                Some(a) if a.completing || a.cancelling => Act::Nothing,
                Some(a) if !a.running => {
                    // Its end begins now, so it never starts; its completions release
                    // whatever its call reserved.
                    a.completing = true;
                    Act::Queued(Box::new(a.info.clone()))
                }
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
                s.complete(&info, row.attempt, End::Cancelled).await;
            }
            Act::Running(engine) => {
                let not_ending: Guard = |a| !a.completing;
                s.apply(
                    id.as_str(),
                    &scope,
                    Change::to(Substatus::Cancelling),
                    Some(not_ending),
                )
                .await;
                if let Some((ctx, handler)) = engine {
                    handler.cancel_in_engine(&ctx).await;
                }
            }
        }
        let row = record::get(scope.db(), id.as_str())
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?;
        current(row)
    }

    /// `job.cancel` of an `interrupted_unknown` job ([`Jobs::cancel`]): the final state is
    /// written first, so of two cancels only one runs the completions.
    async fn resolve_unknown(
        &self,
        caller: &JobCaller,
        scope: &Scope,
        row: JobRow,
    ) -> Result<JobSummary, ErrorBody> {
        let s = &self.shared;
        if caller.caller.is_agent() {
            return Err(ErrorBody::new(
                &codes::CONFIRM_UNAVAILABLE,
                format!(
                    "Job {} may or may not have run (interrupted_unknown); only you can resolve it. Cancel it from the LILITH app.",
                    row.job_id
                ),
                caller.trace_id.clone(),
            ));
        }
        let mut c = Change::finished(JobStatus::Cancelled);
        c.error = Some(None);
        c.finished_utc = Some(s.now());
        let resolved = s.apply(&row.job_id, scope, c, None).await;
        if resolved.is_some()
            && let Some(mut ctx) = s.ctx_from_row(&row, scope)
        {
            tracing::info!(job_id = %row.job_id, "an interrupted_unknown job is resolved: cancelled");
            // The windows it reported, when this core run saw them.
            if let Some(windows) = lock(&s.unknown_windows).remove(&row.job_id) {
                ctx.windows = windows;
            }
            let error = ErrorBody::new(
                &RESOLVED_UNKNOWN,
                "You resolved the job, which may or may not have run.",
                &row.trace_id,
            );
            s.run_completions(&ctx, &Outcome::Error { error }, None)
                .await;
        }
        let after = record::get(scope.db(), &row.job_id)
            .await
            .map_err(|e| state_unavailable(&e, &caller.trace_id))?;
        after
            .map(|r| s.summary(&r))
            .ok_or_else(|| job_unknown(&row.job_id, &caller.trace_id))
    }

    /// The tool call that made failed or cancelled job `id` of `caller`'s scope, and the job's
    /// project, for `job.retry` to dispatch again (CORE-146). Never an `interrupted_unknown`
    /// job; for an agent, never a job that was re-run automatically already (CORE-128: the
    /// re-run is the retry).
    ///
    /// # Errors
    /// `job.unknown`; `job.not_retryable` for any other state, a job no tool call made, or an
    /// agent's retry of a re-run job (its `data.rerun` names the re-run); `state.unavailable`.
    pub async fn retry_origin(
        &self,
        caller: &JobCaller,
        id: &JobId,
    ) -> Result<RetryPlan, ErrorBody> {
        let s = &self.shared;
        let trace_id = &caller.trace_id;
        let scope = s.scope_of(caller);
        s.ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?;
        let row = record::get(scope.db(), id.as_str())
            .await
            .map_err(|e| state_unavailable(&e, trace_id))?
            .ok_or_else(|| job_unknown(id.as_str(), trace_id))?;
        let not_retryable =
            |why: String| ErrorBody::new(&codes::JOB_NOT_RETRYABLE, why, trace_id.clone());
        if !matches!(row.status, JobStatus::Failed | JobStatus::Cancelled) {
            return Err(not_retryable(format!(
                "Job {id} is {}; only a failed or cancelled job can be retried.",
                row.substatus
                    .map_or_else(|| row.status.wire().to_string(), |s| s.wire())
            )));
        }
        // Ruling P3-R21: an agent's retry of paid or hosted work that may have run
        // (`interrupted_with_side_effects` below) needs your confirmation; the dispatcher makes
        // it `pending_confirmation`. An `interrupted_unknown` job is retried by no one (above):
        // it is resolved first.
        if caller.caller.is_agent()
            && let Some(rerun) = record::auto_rerun_of(scope.db(), id.as_str())
                .await
                .map_err(|e| state_unavailable(&e, trace_id))?
        {
            let mut error = not_retryable(format!(
                "Job {id} was re-run automatically as job {rerun}; follow that job instead."
            ));
            error.data = Some(json!({ "rerun": rerun }));
            return Err(error);
        }
        let origin = row.origin.ok_or_else(|| {
            not_retryable(format!(
                "Job {id} was not started by a tool call, so there is nothing to run again."
            ))
        })?;
        Ok(RetryPlan {
            origin,
            project: row.project,
            interrupted_with_side_effects: row.external_side_effects
                && row.substatus == Some(Substatus::Interrupted),
        })
    }

    /// The startup recovery of the suite scope (ARCH-022), once per core: the jobs it marked
    /// (none on a later call). Every job of the scope without a final state that this core
    /// isn't running becomes `interrupted` (or `interrupted_unknown`, CORE-128); nothing
    /// re-runs.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn recover_suite(&self) -> Result<Vec<JobId>, ErrorBody> {
        let scope = Scope::Suite(self.shared.states.suite_db());
        self.shared
            .ensure_recovered(&scope)
            .await
            .map_err(|e| state_unavailable(&e, ""))
    }

    /// Recovers the workspace `open` once per core (as [`Jobs::recover_suite`], with its jobs
    /// log lines); later calls do nothing. Every job operation in a workspace runs it first.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn recover_workspace(&self, open: &Arc<OpenWorkspace>) -> Result<(), ErrorBody> {
        self.shared
            .ensure_recovered(&Scope::Workspace(open.clone()))
            .await
            .map(drop)
            .map_err(|e| state_unavailable(&e, ""))
    }

    /// Whether the core isn't idle for its jobs (ARCH-020): a job is queued, running or in its
    /// completions; once the core drains, only running jobs and completions count (ARCH-021).
    pub fn busy(&self) -> bool {
        let draining = self.shared.draining();
        let q = self.shared.queue();
        if draining {
            q.jobs.values().any(|a| a.running || a.completing)
        } else {
            !q.jobs.is_empty()
        }
    }

    /// The suite `state.db`: the engines keep their caches in it (the ComfyUI client's id,
    /// its uploads and model hashes, Appendix F).
    pub fn suite_state(&self) -> Arc<StateDb> {
        self.shared.states.suite_db()
    }

    /// The engine ids of the jobs taken up again from an earlier core run (ruling P3-R21): the
    /// engines count their prompts as the suite's.
    pub fn adopted_engine_ids(&self) -> HashSet<String> {
        lock(&self.shared.adopted).clone()
    }

    /// Whether `engine_id` is the `external_id` of a job not yet final, in the suite's scope
    /// or an open workspace's: a prompt of an earlier core run that no recovery has taken up
    /// yet is still the suite's. A database that can't be read answers `false`.
    pub async fn is_open_job_engine_id(&self, engine_id: &str) -> bool {
        let mut dbs = vec![self.shared.states.suite_db()];
        dbs.extend(
            self.shared
                .hub
                .opened()
                .into_iter()
                .map(|w| w.state.clone()),
        );
        for db in dbs {
            let id = engine_id.to_string();
            let found = db
                .call(move |conn| {
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM jobs WHERE external_id = ?1 \
                         AND status IN ('queued', 'running'))",
                        [&id],
                        |r| r.get::<_, bool>(0),
                    )
                    .map_err(|e| StateError::Io(e.to_string()))
                })
                .await;
            if found.unwrap_or(false) {
                return true;
            }
        }
        false
    }

    /// A new id `<prefix>_<16 hex>` from the core's id source: the call and trace ids of a job
    /// the core submits itself (`comfy.inventory`), with no tool call.
    pub fn mint_id(&self, prefix: &str) -> String {
        self.shared.ids.id(prefix)
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
/// runs, else the suite `state.db`, else a workspace the hub has open; a snapshot never opens
/// one), or `{"error": …}`.
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
            scopes.extend(s.hub.opened().into_iter().map(Scope::Workspace));
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

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::fs::SystemProbe;
    use lil_testkit::{FakeClock, SeqIds, TestEnv};

    /// Runs `t.hold` jobs until they are cancelled.
    struct Hold;

    impl JobHandler for Hold {
        fn kind(&self) -> &'static str {
            "t.hold"
        }

        fn run<'a>(
            &'a self,
            job: &'a JobCtx,
        ) -> super::super::JobFuture<'a, Result<JobDone, JobFailure>> {
            Box::pin(async move {
                let mut cancel = job.cancel.clone();
                let _ = cancel.wait_for(|c| *c).await;
                Err(JobFailure::Final(cancelled_error(&job.trace_id)))
            })
        }

        fn cancel_in_engine<'a>(&'a self, _job: &'a JobCtx) -> super::super::JobFuture<'a, ()> {
            Box::pin(std::future::ready(()))
        }
    }

    /// Logs `<job> <outcome code> <reservation>` for each completion.
    struct Ends(Arc<Mutex<Vec<String>>>);

    impl JobCompletion for Ends {
        fn completed<'a>(
            &'a self,
            job: &'a JobCtx,
            outcome: &'a Outcome<ToolOutput>,
            _done: Option<&'a JobDone>,
        ) -> super::super::JobFuture<'a, ()> {
            let code = match outcome {
                Outcome::Error { error } => error.code.clone(),
                _ => "ok".to_string(),
            };
            lock(&self.0).push(format!(
                "{} {code} {}",
                job.id,
                job.reservation.as_deref().unwrap_or("-")
            ));
            Box::pin(std::future::ready(()))
        }
    }

    fn caller(reservation: Option<&str>) -> JobCaller {
        JobCaller {
            caller: lil_protocol::CallerKind::Cli,
            idem_scope: "cli|none".into(),
            workspace: None,
            trace_id: "t_unit".into(),
            idem_key: None,
            call_id: "c_0000000000000001".into(),
            reservation: reservation.map(str::to_string),
            submitted: Arc::new(super::super::SubmittedSlot::new()),
            origin: None,
            retry_of: None,
        }
    }

    fn hold() -> JobSpec {
        JobSpec {
            kind: "t.hold",
            lane: Lane::Cpu,
            priority: Priority::Agent,
            module: "test".into(),
            args: json!({}),
            project: None,
            asset_id: None,
            doc_id: None,
            expected_secs: 10,
            expected_output_bytes: 0,
            vram_need_mib: None,
            engine: None,
            group: None,
            external_side_effects: false,
        }
    }

    /// N2: a cancel of a queued job that races the shutdown step (its end has begun when the
    /// step closes the queue) still ends the job at once as cancelled, with its completions
    /// releasing the reservation; the step doesn't wait out its grace for it.
    #[tokio::test(start_paused = true)] // spec: ARCH-021, CORE-088, CORE-146
    async fn a_queued_cancel_that_races_the_shutdown_step_ends_at_once() {
        let env = TestEnv::new();
        let states = Arc::new(StateRegistry::open(&env.env, Arc::new(|_: &str| {})).unwrap());
        let clock: Arc<dyn Clock> = FakeClock::new();
        let events = Arc::new(EventBus::new(clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            FailPoints::none(),
            Arc::new(SystemProbe),
            Default::default(),
        );
        let jobs = Jobs::new(states.clone(), hub, events, clock, ids, FailPoints::none());
        jobs.register(Arc::new(Hold));
        let log = Arc::new(Mutex::new(Vec::new()));
        jobs.add_completion(Arc::new(Ends(log.clone())));
        let running = jobs.submit(&caller(None), hold()).await.unwrap();
        let queued = jobs.submit(&caller(Some("rv_q")), hold()).await.unwrap();
        let shared = jobs.shared.clone();
        while !shared
            .queue()
            .jobs
            .get(&running.job_id)
            .is_some_and(|a| a.running)
        {
            tokio::task::yield_now().await;
        }
        // `job.cancel` has begun the queued job's end (under the queue's lock) ...
        let info = {
            let mut q = shared.queue();
            let a = q.jobs.get_mut(&queued.job_id).unwrap();
            assert!(!a.running);
            a.completing = true;
            a.info.clone()
        };
        // ... the shutdown step closes the queue meanwhile ...
        let step = {
            let shared = shared.clone();
            tokio::spawn(async move { shared.persist_interrupted().await })
        };
        while !shared.queue().closed {
            tokio::task::yield_now().await;
        }
        // ... and the cancel goes on. The state database is a real thread, so while this task
        // waits on it the runtime would idle, and a paused clock auto-advances when it idles:
        // the step's 2 s grace would then expire at once. A task that never idles keeps the
        // clock where it is, so `begun.elapsed()` below can only move if the step really waits.
        let keep_clock = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let begun = tokio::time::Instant::now();
        shared.complete(&info, 1, End::Cancelled).await;
        step.await.unwrap();
        keep_clock.abort();
        assert_eq!(
            begun.elapsed(),
            Duration::ZERO,
            "the step didn't wait out its grace"
        );
        let row = record::get(&states.suite_db(), &queued.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, JobStatus::Cancelled);
        assert!(
            lock(&log).contains(&format!("{} job.cancelled rv_q", queued.job_id)),
            "{:?}",
            lock(&log)
        );
        drop(jobs);
        tokio::task::spawn_blocking(move || states.close())
            .await
            .unwrap();
    }
}
