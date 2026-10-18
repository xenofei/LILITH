//! The `jobs` module (CORE-014, CORE-117, CORE-121–124, CORE-128; ARCH-020–022, ARCH-034):
//! job records in `state.db`, the two lanes and their priority classes, the suite-held queue,
//! retries, recovery after a crash, and the job tools.
//!
//! - **Records** ([`record`]). A job lives in the `state.db` of its call's scope: the session's
//!   workspace, or the suite's for a call without one (ARCH-099). The row is written as
//!   `queued` before [`Jobs::submit`] returns (ARCH-034), and every state change after it is a
//!   guarded update that never leaves a final state (`succeeded`, `failed`, `cancelled`), so a
//!   job that finished is never changed again, and cancellation happens once. Each state
//!   change is one line of the workspace's jobs log (CORE-009, Appendix F field list); a job
//!   without a workspace writes none. The `jobs` topic carries `{kind: "job", job}` and
//!   `job:<id>` carries `status`, `progress`, `retrying` and `watchdog` (snapshot: the
//!   [`JobSummary`]).
//! - **Lanes and the queue** ([`queue`]). Each lane (`gpu-heavy`, `cpu`) runs at most one job
//!   at a time; the next starts when it ends (CORE-121, CORE-123, ARCH-071). The next job is
//!   the first of the best class (`interactive` > `user` > `agent`), first in, first out
//!   within it; a job with a `group` (the lane's last group) may move ahead of older jobs of its
//!   class, but never past one that was overtaken that way 8 times (CORE-124). With
//!   `[jobs] preempt_agent_batches = true` an `interactive` job cancels the running `agent`
//!   engine job of its lane, which goes back to the head of its class (CORE-125). Before a job
//!   starts, the [`Admission`] (the GPU scheduler, [`scheduler`]; [`AlwaysAdmit`] by default)
//!   may hold it in `waiting_for_vram`, and it is told whether jobs run or wait for VRAM.
//!   [`Jobs::kick`] (on every telemetry sample) asks again, and runs the watchdog: a running job
//!   more than 3× its `expected_secs` past the moment its attempt became `executing` gets
//!   `watchdog: "slow"` (a `watchdog` event on `job:<id>`); it keeps running (ARCH-072).
//! - **Running** ([`runner`]). A free-space preflight on the staging volume (2 ×
//!   `expected_output_bytes` + 64 MiB) fails a job with `store.disk_full` before its handler
//!   runs. A [`JobFailure::Retryable`] is retried after 1 s, doubling, at most 30 s, at most 5
//!   attempts in all, with a `retrying` event per retry; anything else is final (CORE-128).
//!   When a job ends, its [`JobCompletion`]s run in registration order (Meter before Caps),
//!   and then its [`JobDone::record`] is applied in its workspace (CORE-015 steps 8–9).
//! - **Recovery** (ARCH-022). [`Jobs::recover_suite`] and [`Jobs::recover_workspace`] mark every
//!   job of a scope that this core is not running and that has no final state `interrupted`,
//!   or `interrupted_unknown` when it was `submitting` with external side effects; nothing
//!   re-runs. The core recovers the suite scope before it serves, and a workspace scope before
//!   its first job operation, once per scope. The `PersistInterrupted` shutdown step does the
//!   same for the jobs still queued or running when a drain ends (ARCH-021).
//!
//! Handlers register with [`Jobs::register`] (CORE-010: `jobs` defines [`JobHandler`], tool
//! modules register with it). `jobs` depends on no `tools` type: a tool hands
//! [`Jobs::submit`] a [`JobCaller`], built from its call context, and the job tools live in
//! `tools`.
//!
//! **Reservations** (CORE-088, for Tasks 15 and 16). A job takes over the cap reservation of the
//! call that submitted it: the row and [`JobCtx::reservation`] carry it, and
//! [`Reservations::hand_over`] tells its owner (from the call id to the job id) once the row is
//! in. Who resolves it:
//! - the job's completions, when it ends (succeeded, failed, cancelled, `interrupted` by its
//!   engine);
//! - a call whose idempotency key gave back an existing job submitted nothing new: its own
//!   reservation is [`Reservations::release`]d, with no usage line;
//! - an interrupted `agent` job re-run automatically (local work only, CORE-128) hands its
//!   reservation to the re-run: its completions get `reservation: None`, the re-run's settle
//!   it;
//! - a job that never started (`waiting`, `waiting_for_vram`) when a core stopped gets its
//!   completions at recovery or at `PersistInterrupted`, with an outcome for which
//!   [`never_started`] holds: nothing was used;
//! - a job that was running when a core stopped keeps its reservation (Appendix F's pending
//!   usage rule): Task 16's startup repair owns it;
//! - an `interrupted_unknown` job keeps it until you resolve it: `job.cancel` from the app
//!   makes it `cancelled` and runs its completions with an outcome for which
//!   [`resolved_unknown`] holds (CORE-088, AI-029). Task 16 (ruling P3-R15): that reservation
//!   is settled at its reserved amount, never released, because the request may have been
//!   charged; an ordinary cancel ([`is_cancelled`]) releases it.
//!
//! **Crash windows.** A job that worked is written `running`/`ingesting` with its result before
//! its completions run, so a core that dies during the completions or its Record step leaves
//! a row that recovery marks `interrupted` with that result kept as salvaged artifacts; a
//! retry is then an informed choice. The `PersistInterrupted` step waits a short while
//! ([`COMPLETION_GRACE`]) for jobs in their completions, and marks the rest the same way;
//! a job whose Record step has begun is never marked by it: its final state is written once
//! its run is recorded, so the row and the recorded run agree (or, when the core stops first,
//! the next start's recovery marks it `interrupted` with its result, as after a crash).

pub mod queue;
pub mod record;
pub mod runner;
pub mod scheduler;

use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lil_protocol::{CallerKind, ErrorBody, FileRef, Outcome, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::events::EventBus;
use crate::pipeline::RecordPlan;
use crate::state::StateDb;
use crate::workspace::OpenWorkspace;

pub use runner::{Jobs, cancelled_error};

/// The module the job tools declare.
pub const JOBS_MODULE: &str = "jobs";
/// Attempts a job gets in all, the first included (CORE-128).
pub const MAX_ATTEMPTS: u32 = 5;
/// The longest pause between two attempts (CORE-128).
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// `job.wait` waits at most this long (AI-113).
pub const MAX_WAIT: Duration = Duration::from_secs(90);
/// How often a job may be overtaken by a job of its class with the lane's group (CORE-124).
pub const MAX_OVERTAKES: u32 = 8;
/// The staging room a job needs on top of twice its expected output (CORE-128).
pub const DISK_MARGIN_BYTES: u64 = 64 * 1024 * 1024;
/// How long the `PersistInterrupted` step waits for jobs that are in their completions.
pub const COMPLETION_GRACE: Duration = Duration::from_secs(2);

/// The internal code of a cancelled job's outcome (its completions see it). It never reaches
/// the wire: a cancelled job's summary has no `error`, and its jobs log line writes `null`.
/// Not in Appendix D, so not in `codes::ALL`.
pub const JOB_CANCELLED: &str = "job.cancelled";
/// The internal code of the outcome a job's completions get when it never started (a core
/// stopped while it was `waiting`): nothing was used. Never on the wire either.
pub const JOB_NOT_STARTED: &str = "job.not_started";

/// The internal code of the outcome an `interrupted_unknown` job's completions get when you
/// resolve it. Never on the wire.
pub const JOB_RESOLVED_UNKNOWN: &str = "job.resolved_unknown";

/// Whether `outcome` is the one of an `interrupted_unknown` job you resolved: Task 16 settles
/// its reservation at the reserved amount (ruling P3-R15), Meter records it as cancelled.
pub fn resolved_unknown(outcome: &Outcome<ToolOutput>) -> bool {
    matches!(outcome, Outcome::Error { error } if error.code == JOB_RESOLVED_UNKNOWN)
}

/// Whether `outcome` is a cancelled job's (Task 15 writes `status: "cancelled"` for it).
pub fn is_cancelled(outcome: &Outcome<ToolOutput>) -> bool {
    matches!(outcome, Outcome::Error { error } if error.code == JOB_CANCELLED)
}

/// Whether `outcome` is the one of a job that never started: Meter writes no usage line for
/// it, and Caps releases its reservation.
pub fn never_started(outcome: &Outcome<ToolOutput>) -> bool {
    matches!(outcome, Outcome::Error { error } if error.code == JOB_NOT_STARTED)
}

/// The pause before retry `n` (1-based): 1 s, doubling, at most [`MAX_BACKOFF`].
pub fn backoff(n: u32) -> Duration {
    let secs = 1u64
        .checked_shl(n.saturating_sub(1))
        .unwrap_or(u64::MAX)
        .min(MAX_BACKOFF.as_secs());
    Duration::from_secs(secs)
}

/// A job id, `j_<16 hex>`.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct JobId(pub String);

impl JobId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A lane (CORE-121): wire `gpu-heavy`, `cpu`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Lane {
    #[serde(rename = "gpu-heavy")]
    GpuHeavy,
    #[serde(rename = "cpu")]
    Cpu,
}

impl Lane {
    pub const ALL: [Lane; 2] = [Lane::GpuHeavy, Lane::Cpu];

    pub fn wire(self) -> &'static str {
        match self {
            Lane::GpuHeavy => "gpu-heavy",
            Lane::Cpu => "cpu",
        }
    }

    pub fn parse(s: &str) -> Option<Lane> {
        Lane::ALL.into_iter().find(|l| l.wire() == s)
    }

    fn index(self) -> usize {
        match self {
            Lane::GpuHeavy => 0,
            Lane::Cpu => 1,
        }
    }
}

/// A priority class (CORE-124), in rank order: `Interactive` runs before `User` before
/// `Agent` (the derived order).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Interactive,
    User,
    Agent,
}

impl Priority {
    /// The class of a job `caller` starts: a `ui:*` caller's job is `user` unless the tool
    /// marks it `interactive`; `assistant`, `mcp` and `cli` jobs are `agent` (Appendix F).
    pub fn for_caller(caller: &CallerKind, interactive: bool) -> Priority {
        match caller {
            CallerKind::Ui(_) if interactive => Priority::Interactive,
            CallerKind::Ui(_) => Priority::User,
            _ => Priority::Agent,
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            Priority::Interactive => "interactive",
            Priority::User => "user",
            Priority::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<Priority> {
        [Priority::Interactive, Priority::User, Priority::Agent]
            .into_iter()
            .find(|p| p.wire() == s)
    }
}

/// A job's status (CORE-117).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn wire(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<JobStatus> {
        [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Succeeded,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ]
        .into_iter()
        .find(|st| st.wire() == s)
    }

    /// `succeeded`, `failed` and `cancelled` are final: a job in one never changes again.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

/// A job's substatus (CORE-117, Appendix F). A final status other than `failed`/`interrupted`
/// has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Substatus {
    Waiting,
    WaitingForVram,
    Submitting,
    QueuedInEngine,
    Executing,
    Ingesting,
    Retrying { attempt: u32, of: u32 },
    Cancelling,
    Interrupted,
    InterruptedUnknown,
}

impl Substatus {
    /// The status it belongs to: `waiting`, `waiting_for_vram`, `submitting`,
    /// `queued_in_engine` → `queued`; `executing`, `ingesting`, `retrying_<n>_of_<m>`,
    /// `cancelling`, `interrupted_unknown` → `running`; `interrupted` → `failed`.
    pub fn status(&self) -> JobStatus {
        match self {
            Substatus::Waiting
            | Substatus::WaitingForVram
            | Substatus::Submitting
            | Substatus::QueuedInEngine => JobStatus::Queued,
            Substatus::Executing
            | Substatus::Ingesting
            | Substatus::Retrying { .. }
            | Substatus::Cancelling
            | Substatus::InterruptedUnknown => JobStatus::Running,
            Substatus::Interrupted => JobStatus::Failed,
        }
    }

    pub fn wire(&self) -> String {
        match self {
            Substatus::Waiting => "waiting".into(),
            Substatus::WaitingForVram => "waiting_for_vram".into(),
            Substatus::Submitting => "submitting".into(),
            Substatus::QueuedInEngine => "queued_in_engine".into(),
            Substatus::Executing => "executing".into(),
            Substatus::Ingesting => "ingesting".into(),
            Substatus::Retrying { attempt, of } => format!("retrying_{attempt}_of_{of}"),
            Substatus::Cancelling => "cancelling".into(),
            Substatus::Interrupted => "interrupted".into(),
            Substatus::InterruptedUnknown => "interrupted_unknown".into(),
        }
    }

    pub fn parse(s: &str) -> Option<Substatus> {
        Some(match s {
            "waiting" => Substatus::Waiting,
            "waiting_for_vram" => Substatus::WaitingForVram,
            "submitting" => Substatus::Submitting,
            "queued_in_engine" => Substatus::QueuedInEngine,
            "executing" => Substatus::Executing,
            "ingesting" => Substatus::Ingesting,
            "cancelling" => Substatus::Cancelling,
            "interrupted" => Substatus::Interrupted,
            "interrupted_unknown" => Substatus::InterruptedUnknown,
            other => {
                let rest = other.strip_prefix("retrying_")?;
                let (n, m) = rest.split_once("_of_")?;
                Substatus::Retrying {
                    attempt: n.parse().ok()?,
                    of: m.parse().ok()?,
                }
            }
        })
    }
}

/// An engine a job runs in (ARCH-064).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum EngineKind {
    #[serde(rename = "comfyui")]
    ComfyUi,
    /// A second kind for the GPU lease's tests (no engine runs it). No input can name it: serde
    /// and the schemas skip it, and [`EngineKind::parse`] doesn't know it; only the row column
    /// reader ([`EngineKind::from_column`]) reads it back.
    #[doc(hidden)]
    #[serde(skip)]
    #[schemars(skip)]
    Test,
}

impl EngineKind {
    pub fn wire(self) -> &'static str {
        match self {
            EngineKind::ComfyUi => "comfyui",
            EngineKind::Test => "test",
        }
    }

    /// An engine named in input (a tool argument, a setting): never [`EngineKind::Test`].
    pub fn parse(s: &str) -> Option<EngineKind> {
        (s == "comfyui").then_some(EngineKind::ComfyUi)
    }

    /// The `engine` column of a jobs row, which a test may have written as `"test"`.
    pub fn from_column(s: &str) -> Option<EngineKind> {
        [EngineKind::ComfyUi, EngineKind::Test]
            .into_iter()
            .find(|e| e.wire() == s)
    }
}

/// What a tool submits.
#[derive(Debug, Clone, PartialEq)]
pub struct JobSpec {
    /// The kind of a registered [`JobHandler`].
    pub kind: &'static str,
    pub lane: Lane,
    pub priority: Priority,
    /// The submitting tool's module (the jobs log's `module`).
    pub module: String,
    /// The handler's arguments.
    pub args: serde_json::Value,
    pub project: Option<String>,
    pub asset_id: Option<String>,
    pub doc_id: Option<String>,
    /// For the watchdog (0: none).
    pub expected_secs: u32,
    /// For the disk preflight.
    pub expected_output_bytes: u64,
    pub vram_need_mib: Option<u64>,
    pub engine: Option<EngineKind>,
    /// A model profile (plan 6a): the queue may group jobs of one group (CORE-124).
    pub group: Option<String>,
    /// Hosted work or paid partner nodes: a crash in `submitting` makes it
    /// `interrupted_unknown` (CORE-128).
    pub external_side_effects: bool,
}

/// A job's progress (`job:<id>` `progress`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Progress {
    pub value: u64,
    pub max: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// A finished job's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobResult {
    pub structured: serde_json::Value,
    pub artifacts: Vec<FileRef>,
}

/// A job as tools and events show it (Appendix F `JobSummary`); times are RFC 3339 UTC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobSummary {
    pub job_id: String,
    pub kind: String,
    pub lane: Lane,
    pub priority: Priority,
    pub status: JobStatus,
    /// `null` for `succeeded`, `cancelled` and a `failed` job that wasn't interrupted.
    pub substatus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    pub created: String,
    pub updated: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// `"slow"` once the watchdog flagged the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchdog: Option<String>,
    /// Present (and `true`) only when the job ran without a VRAM check (no NVML).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vram_unchecked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JobResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    pub trace_id: String,
}

/// One stretch of engine execution, in monotonic time (telemetry attribution, Task 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecWindow {
    pub engine_pid: u32,
    pub start: Duration,
    pub end: Duration,
    pub ok: bool,
}

/// What a handler returns for a job that worked.
#[derive(Debug)]
pub struct JobDone {
    pub structured: serde_json::Value,
    pub artifacts: Vec<FileRef>,
    /// Applied through `apply_record` in the job's workspace once the completions ran.
    pub record: Option<RecordPlan>,
    pub windows: Vec<ExecWindow>,
}

/// Why a job's attempt didn't work.
#[derive(Debug, Clone, PartialEq)]
pub enum JobFailure {
    /// Transient: retried with backoff (CORE-128).
    Retryable(ErrorBody),
    /// Never retried.
    Final(ErrorBody),
    /// The engine lost the job: `failed`/`interrupted`, with what was salvaged as artifacts;
    /// an `agent` job is re-run once as a new job with `retry_of` (CORE-128).
    Interrupted {
        error: ErrorBody,
        salvaged: Vec<FileRef>,
    },
    /// It may or may not have happened: `running`/`interrupted_unknown` until you resolve it.
    Unknown(ErrorBody),
}

/// A future a job handler or completion returns.
pub type JobFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a running job's handler knows. A new one is made for each attempt.
pub struct JobCtx {
    pub id: JobId,
    pub spec: JobSpec,
    pub attempt: u32,
    pub trace_id: String,
    pub workspace: Option<Arc<OpenWorkspace>>,
    pub events: Arc<EventBus>,
    /// Becomes `true` when the job is cancelled (or preempted): the handler stops.
    pub cancel: tokio::sync::watch::Receiver<bool>,
    /// The `state.db` the job lives in.
    pub state: Arc<StateDb>,
    /// The cap reservation the job holds (module docs, "Reservations"): the one its call made,
    /// or its interrupted original's. `None` in the completions of a job that handed it to its
    /// re-run.
    pub reservation: Option<String>,
    pub(crate) jobs: Arc<runner::Shared>,
}

impl fmt::Debug for JobCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobCtx")
            .field("id", &self.id)
            .field("kind", &self.spec.kind)
            .field("attempt", &self.attempt)
            .field("trace_id", &self.trace_id)
            .finish_non_exhaustive()
    }
}

impl JobCtx {
    /// Records `s` (persisted, logged and published). A job that is being cancelled stays
    /// `cancelling`; `jobs.after_submitting` fires once `submitting` is persisted.
    pub async fn set_substatus(&self, s: Substatus) {
        self.jobs.handler_substatus(&self.id, s).await;
    }

    /// Publishes progress on `job:<id>`; job summaries show the latest.
    pub fn progress(&self, value: u64, max: u64, node: Option<&str>) {
        self.jobs.progress(&self.id, value, max, node);
    }

    /// Runs the fail point `name` (`jobs.after_post`, `jobs.after_outputs`): the handler
    /// treats an `Err` as the failure of the step that follows.
    ///
    /// # Errors
    /// The armed action's error.
    pub fn failpoint(&self, name: &str) -> io::Result<()> {
        self.jobs.failpoints().hit(name)
    }
}

/// Runs the jobs of one kind (CORE-010).
pub trait JobHandler: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    /// One attempt. It watches `job.cancel` and returns soon after it turns `true`.
    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>>;
    /// Cancels the job's own work in its engine (never anything else); called at most once
    /// per job, after `job.cancel` turned `true`.
    fn cancel_in_engine<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, ()>;
}

/// Whether a job may start now (the GPU scheduler, [`scheduler::GpuScheduler`]). Both methods
/// are called under the queue's lock, so neither may block or call back into [`Jobs`].
pub trait Admission: Send + Sync {
    /// Asked for the next job of a free lane, again on every [`Jobs::kick`] while it waits.
    fn admit(&self, spec: &JobSpec) -> Admit;
    /// Whether any job runs or waits for VRAM, told whenever that changes (CORE-061: the
    /// per-process GPU memory is read every second while it is `true`).
    fn busy(&self, _busy: bool) {}
}

/// Admits everything: the default (the core sets the GPU scheduler).
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysAdmit;

impl Admission for AlwaysAdmit {
    fn admit(&self, _spec: &JobSpec) -> Admit {
        Admit::Go
    }
}

/// An [`Admission`]'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    Go,
    /// Go, without a VRAM check (no NVML): the job's summary says `vram_unchecked`.
    GoUnchecked,
    /// Not enough VRAM yet: the job waits in `waiting_for_vram`.
    WaitForVram {
        need_mib: u64,
        free_mib: u64,
    },
}

/// Set by [`Jobs::submit`]: the dispatcher reads it after Execute, and a call that submitted a
/// job leaves Meter and Record to the job's completion. One call submits at most one job, so
/// the job can take over the call's one reservation.
#[derive(Debug, Default)]
pub struct SubmittedSlot(Mutex<Option<JobId>>);

impl SubmittedSlot {
    pub fn new() -> Self {
        SubmittedSlot::default()
    }

    pub fn set(&self, id: JobId) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(id);
    }

    pub fn get(&self) -> Option<JobId> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The tool call that submitted a job: `job.retry` dispatches it again (with the retry's
/// caller and key), so its confirm, estimate and reserve stages run again.
#[derive(Debug, Clone, PartialEq)]
pub struct JobOrigin {
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// What `job.retry` runs again: the tool call that made the job, in the job's own project
/// (the row's; `None` without a workspace or a project).
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPlan {
    pub origin: JobOrigin,
    pub project: Option<String>,
}

/// Who submits a job or asks about one, for which call (CORE-010: `tools` builds it from its
/// call context, so `jobs` takes no `tools` type).
#[derive(Debug, Clone)]
pub struct JobCaller {
    pub caller: CallerKind,
    /// `<caller kind>|<workspace id or none>`: the idempotency scope (CORE-016).
    pub idem_scope: String,
    /// The call's workspace: the job's scope (ARCH-099); `None` is the suite's.
    pub workspace: Option<Arc<OpenWorkspace>>,
    pub trace_id: String,
    /// `None` for a call without one.
    pub idem_key: Option<String>,
    /// The call's id (`c_<16 hex>`), whose reservation a job takes over.
    pub call_id: String,
    pub reservation: Option<String>,
    pub submitted: Arc<SubmittedSlot>,
    /// The tool call, kept with the job for `job.retry`; `None` outside a tool call.
    pub origin: Option<JobOrigin>,
    /// Set when the call is a retry of that job: the new job's `retry_of`, and its class is
    /// the retrying caller's (Appendix F).
    pub retry_of: Option<JobId>,
}

/// What happens to cap reservations as jobs take them over (module docs, "Reservations"):
/// Task 16 implements it; [`NoReservations`] does nothing.
pub trait Reservations: Send + Sync {
    /// `reservation`, made for `from` (a call id `c_…`, or the job a re-run replaces), now
    /// belongs to job `to` (CORE-088: a reservation records its job or call id).
    fn hand_over<'a>(
        &'a self,
        reservation: &'a str,
        from: &'a str,
        to: &'a JobId,
    ) -> JobFuture<'a, ()>;
    /// `reservation`, made for `owner`, is released unused: nothing ran for it.
    fn release<'a>(&'a self, reservation: &'a str, owner: &'a str) -> JobFuture<'a, ()>;
}

/// No caps: nothing to hand over or release.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoReservations;

impl Reservations for NoReservations {
    fn hand_over<'a>(
        &'a self,
        _reservation: &'a str,
        _from: &'a str,
        _to: &'a JobId,
    ) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }

    fn release<'a>(&'a self, _reservation: &'a str, _owner: &'a str) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// Runs when a job ends (CORE-015 steps 8–9 for jobs): Tasks 15 and 16 implement it.
/// `outcome` is the job's outcome; `done` is the handler's result when it worked.
pub trait JobCompletion: Send + Sync {
    fn completed<'a>(
        &'a self,
        job: &'a JobCtx,
        outcome: &'a Outcome<ToolOutput>,
        done: Option<&'a JobDone>,
    ) -> JobFuture<'a, ()>;
}

/// `jobs.list`'s filter (Appendix F).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobFilter {
    #[serde(default)]
    pub status: Option<JobStatus>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// 1–500, default 100.
    #[serde(default)]
    #[schemars(range(min = 1, max = 500))]
    pub limit: Option<u32>,
    /// The previous page's `next_cursor`.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-117
    fn substatus_wire_names_round_trip() {
        for s in [
            Substatus::Waiting,
            Substatus::WaitingForVram,
            Substatus::Submitting,
            Substatus::QueuedInEngine,
            Substatus::Executing,
            Substatus::Ingesting,
            Substatus::Retrying { attempt: 2, of: 5 },
            Substatus::Cancelling,
            Substatus::Interrupted,
            Substatus::InterruptedUnknown,
        ] {
            assert_eq!(Substatus::parse(&s.wire()), Some(s), "{}", s.wire());
        }
        assert_eq!(
            Substatus::Retrying { attempt: 2, of: 5 }.wire(),
            "retrying_2_of_5"
        );
        for bad in ["", "retrying_x_of_5", "retrying_1", "done"] {
            assert_eq!(Substatus::parse(bad), None, "{bad}");
        }
    }

    #[test] // spec: CORE-128
    fn backoff_doubles_from_one_second_to_thirty() {
        let secs: Vec<u64> = (1..=8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(backoff(200), MAX_BACKOFF);
    }

    #[test] // spec: CORE-124
    fn caller_kinds_give_their_priority_class() {
        let ui = CallerKind::Ui("image".into());
        assert_eq!(Priority::for_caller(&ui, false), Priority::User);
        assert_eq!(Priority::for_caller(&ui, true), Priority::Interactive);
        for agent in [CallerKind::Cli, CallerKind::Mcp, CallerKind::Assistant] {
            assert_eq!(Priority::for_caller(&agent, true), Priority::Agent);
        }
        assert!(Priority::Interactive < Priority::User && Priority::User < Priority::Agent);
    }
}
