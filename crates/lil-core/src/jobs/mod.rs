//! The `jobs` module (CORE-014, CORE-117, CORE-121–124, CORE-128, CORE-160; ARCH-020–022,
//! ARCH-034, ARCH-072): job records in `state.db`, the two lanes and their priority classes, the
//! suite-held queue and its controls, retries, recovery after a crash, and the job tools.
//!
//! - **Records** ([`record`]). A job lives in the `state.db` of its call's scope: the session's
//!   workspace, or the suite's for a call without one (ARCH-099). The row is written as
//!   `queued` before [`Jobs::submit`] returns (ARCH-034), and every state change after it is a
//!   guarded update that never leaves a final state (`succeeded`, `failed`, `cancelled`), so a
//!   job that finished is never changed again, and cancellation happens once. Each state
//!   change is one line of the workspace's jobs log (CORE-009, Appendix F field list); a job
//!   without a workspace writes none. The `jobs` topic carries `{kind: "job", job}` and
//!   `{kind: "queue", state}`, and `job:<id>` carries `status`, `progress`, `retrying`,
//!   `watchdog` and `unresponsive` (snapshot: the [`JobSummary`]). From plan 4a (Appendix G)
//!   every job has a batch and its index in it (P4a-R13; [`Jobs::submit_batch`] submits many),
//!   the typed `origin` of its caller (P4a-R12, also in its log lines), its `status_seq`, and
//!   what its run reports through [`JobCtx`] (phase and subtasks, `indeterminate`,
//!   `last_progress_at`, the cached nodes, the GPU adapter), kept in memory while it runs and
//!   written with each state change. A running job with no sign of life for 30 s is shown
//!   `unresponsive`, checked every 5 s; nothing is cancelled (ARCH-072).
//! - **Lanes and the queue** ([`queue`]). Each lane (`gpu-heavy`, `cpu`) runs at most one job
//!   at a time; the next starts when it ends (CORE-121, CORE-123, ARCH-071). The next job is
//!   the first of the best class (`interactive` > `user` > `agent`), the lowest `order_key`
//!   within it (its core-wide submission sequence, one counter for every scope, until
//!   [`Jobs::move_job`] puts it at the front or the back of its class, CORE-160); a job with
//!   a `group` (the lane's last group) may move ahead of older jobs of its class, but never
//!   past one that was overtaken that way 8 times (CORE-124). A lane's pause ([`Jobs::pause`],
//!   reasons in [`pause`]) is decided before the admission: a job it covers isn't picked and
//!   shows `queue_paused`; a suite prompt still pending in its engine is withdrawn
//!   ([`Withdraw`]), and one not posted yet (`submitting`, or in a retry's backoff) is held
//!   before it posts. With
//!   `[jobs] preempt_agent_batches = true` an `interactive` job cancels the running `agent`
//!   engine job of its lane, which goes back to the head of its class (CORE-125). Before a job
//!   starts, the [`Admission`] (the GPU scheduler, [`scheduler`]; [`AlwaysAdmit`] by default)
//!   may hold it in `waiting_for_vram`, or in `waiting` while its engine runs outside prompts
//!   ([`Admit::Wait`]), and it is told whether jobs run or wait for VRAM.
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
//!   re-runs. The exception is the restart hold (P4a-R41): a job that never reached an engine
//!   (`waiting`, `waiting_for_vram` or `queue_paused`, no engine id; never a director task)
//!   stays queued, held with the reason `restart` and its reservation, until a resume
//!   releases it. The core recovers the suite scope before it serves, and a workspace scope
//!   before its first job operation, once per scope. The `PersistInterrupted` shutdown step
//!   does the same for the jobs still queued or running when a drain ends (ARCH-021), and
//!   leaves the never-started ones queued so the next start holds them.
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
//! - a job that never started when a core stopped is held by the next start and keeps its
//!   reservation until it runs (P4a-R41); one the hold doesn't take (a director task) gets
//!   its completions at recovery or at `PersistInterrupted`, with an outcome for which
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

pub mod pause;
pub mod queue;
pub mod record;
pub mod runner;
pub mod scheduler;

pub use pause::{PauseReason, PauseScope, RESTART_REASON};

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
    /// Held by a lane's pause or a restart (CORE-160; Appendix G).
    QueuePaused,
    Submitting,
    QueuedInEngine,
    Executing,
    Ingesting,
    Retrying {
        attempt: u32,
        of: u32,
    },
    Cancelling,
    Interrupted,
    InterruptedUnknown,
}

impl Substatus {
    /// The status it belongs to: `waiting`, `waiting_for_vram`, `queue_paused`, `submitting`,
    /// `queued_in_engine` → `queued`; `executing`, `ingesting`, `retrying_<n>_of_<m>`,
    /// `cancelling`, `interrupted_unknown` → `running`; `interrupted` → `failed`.
    pub fn status(&self) -> JobStatus {
        match self {
            Substatus::Waiting
            | Substatus::WaitingForVram
            | Substatus::QueuePaused
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
            Substatus::QueuePaused => "queue_paused".into(),
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
            "queue_paused" => Substatus::QueuePaused,
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
    /// Every kind, [`EngineKind::Test`] included.
    pub const ALL: [EngineKind; 2] = [EngineKind::ComfyUi, EngineKind::Test];

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
        EngineKind::ALL.into_iter().find(|e| e.wire() == s)
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
    /// The job's batch, `b_<16 hex>`: every job has one (P4a-R13).
    pub batch_id: String,
    /// Its 0-based place in the batch.
    pub index: u32,
    /// Its status changes in order: the first and the newest 63.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_seq: Vec<StatusStep>,
    /// Who started it (P4a-R12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<StartedBy>,
    /// The GPU adapter of a job that held the GPU lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<Subtask>,
    /// Present (and `true`) while the executing node has sent no progress.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub indeterminate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_nodes: Option<Vec<String>>,
    /// Present (and `true`) when every node of the prompt was served from the cache.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_cached: bool,
    /// Present (and `true`) while a running job has reported nothing for the stall time
    /// (ARCH-072; a display flag only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unresponsive: bool,
    /// While the job is `queue_paused`: the reasons holding it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pause_reasons: Vec<String>,
}

/// The summary text of a job whose every node the engine served from its cache (CORE-117).
pub const NOTHING_CHANGED: &str = "Nothing changed: ComfyUI served every node from its cache.";

/// One entry of a job's `status_seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusStep {
    pub status: JobStatus,
    pub substatus: Option<String>,
    /// RFC 3339 UTC.
    pub at: String,
}

/// One phase a job reached (`subtasks`): the current one carries its newest progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Subtask {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

/// The surface that started a job (P4a-R12): an open set, written snake_case; one this build
/// doesn't know (a later plan's, such as `director_task`) reads back as [`Surface::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Surface {
    /// A verified `ui:<app>` caller.
    Panel,
    /// Claude Code.
    Mcp,
    /// The in-app assistant.
    AgentSession,
    Cli,
    /// A job the core submits itself.
    Core,
    Other(String),
}

impl Surface {
    pub fn wire(&self) -> &str {
        match self {
            Surface::Panel => "panel",
            Surface::Mcp => "mcp",
            Surface::AgentSession => "agent_session",
            Surface::Cli => "cli",
            Surface::Core => "core",
            Surface::Other(name) => name,
        }
    }

    pub fn parse(s: &str) -> Surface {
        match s {
            "panel" => Surface::Panel,
            "mcp" => Surface::Mcp,
            "agent_session" => Surface::AgentSession,
            "cli" => Surface::Cli,
            "core" => Surface::Core,
            other => Surface::Other(other.to_string()),
        }
    }
}

impl Serialize for Surface {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.wire())
    }
}

impl<'de> Deserialize<'de> for Surface {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Surface, D::Error> {
        String::deserialize(d).map(|s| Surface::parse(&s))
    }
}

impl JsonSchema for Surface {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Surface".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
}

/// Who started a job: `JobSummary.origin`, `{surface, id?, step?}` (P4a-R12). Named apart
/// from [`JobOrigin`], the stored tool call `job.retry` runs again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StartedBy {
    pub surface: Surface,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

impl StartedBy {
    /// The origin of a job `caller` submits (P4a-R12): `{surface: "panel", id: <app>}` for a
    /// verified `ui:<app>`, `mcp`, `agent_session` for the assistant, `cli`; and `core` for a
    /// job the core submits itself (a `cli` caller with no tool call, Appendix F).
    pub fn of(caller: &JobCaller) -> StartedBy {
        let (surface, id) = match &caller.caller {
            CallerKind::Ui(app) => (Surface::Panel, Some(app.clone())),
            CallerKind::Mcp => (Surface::Mcp, None),
            CallerKind::Assistant => (Surface::AgentSession, None),
            CallerKind::Cli if caller.origin.is_none() => (Surface::Core, None),
            CallerKind::Cli => (Surface::Cli, None),
        };
        StartedBy {
            surface,
            id,
            step: None,
        }
    }
}

/// Where `job.move` puts a queued job in its class (CORE-160).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MoveTo {
    /// The front of its class.
    Next,
    /// The back of its class.
    Last,
}

/// Which jobs `job.cancel` cancels (Appendix G): exactly one selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelSelector {
    One(JobId),
    /// 1–500 jobs.
    Ids(Vec<JobId>),
    /// Every job of the batch.
    Batch(String),
    /// Every unfinished job the caller's kind submitted (for a `ui:*` caller, the same app's).
    AllMine,
}

/// The most ids one `job.cancel {ids}` takes.
pub const MAX_CANCEL_IDS: usize = 500;

/// `job.retry {batch_id}`'s result (Appendix G).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetryBatch {
    pub retried: Vec<RetriedJob>,
    pub refused: Vec<RefusedJob>,
}

/// A job a batch retry made, and the job it retries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetriedJob {
    pub job: JobSummary,
    pub retry_of: String,
}

/// A job of the batch it didn't retry, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RefusedJob {
    pub job_id: String,
    pub error: ErrorBody,
}

/// What [`Withdraw::withdraw`] did with a job's prompt (CORE-160).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawOutcome {
    /// The engine still listed it pending: it was cancelled there, and it never ran.
    Withdrawn,
    /// It started meanwhile: it runs on.
    AlreadyStarted,
    /// The engine doesn't list it pending (or couldn't be asked): nothing was done.
    NotPending,
}

/// Withdraws a suite prompt still pending in its engine (CORE-160; Appendix G "Withdrawal"):
/// the engines implement it for ComfyUI. Only `Withdrawn` means the prompt was cancelled
/// before it ran; the job's next attempt checks the old prompt is gone before it posts a new
/// one (CORE-120).
pub trait Withdraw: Send + Sync {
    fn withdraw<'a>(
        &'a self,
        job: &'a JobId,
        external_id: &'a str,
    ) -> JobFuture<'a, WithdrawOutcome>;
}

/// One stretch of engine execution, in monotonic time (telemetry attribution, Task 15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecWindow {
    /// Every process of the engine when the window closed (an owned engine's Job Object, an
    /// attached one's listener and its descendants): never empty, and never the telemetry's
    /// pid 0 (every untracked process); a window without a known process is not reported.
    pub engine_pids: Vec<u32>,
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
    /// The execution windows the job's attempts reported ([`JobCtx::add_window`]).
    pub(crate) windows: Arc<Mutex<Vec<ExecWindow>>>,
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

    /// Persists the engine's id for the job (the `jobs` row's `external_id`: ComfyUI's
    /// `prompt_id`, CORE-120) before the handler sends it, so a later attempt reads it back
    /// with [`record::get`] and asks the engine about it first. Nothing is shown or logged: it
    /// is no state change.
    ///
    /// # Errors
    /// `state.unavailable` when it could not be written; `job.unknown` when this core no
    /// longer runs the job or its row is final. The handler sends nothing then.
    pub async fn set_external_id(&self, external_id: &str) -> Result<(), ErrorBody> {
        self.jobs
            .handler_external_id(&self.id, Some(external_id), &self.trace_id)
            .await
    }

    /// Clears the row's `external_id`: what it named provably never reached the engine (no
    /// connection was made, or the engine refused it with 429 or 503), so a later attempt may
    /// send a new one (CORE-128).
    ///
    /// # Errors
    /// As [`JobCtx::set_external_id`].
    pub async fn clear_external_id(&self) -> Result<(), ErrorBody> {
        self.jobs
            .handler_external_id(&self.id, None, &self.trace_id)
            .await
    }

    /// Called under the handler's POST lock just before it posts to its engine (CORE-160): when
    /// a reason of the job's lane covers it now, nothing may be posted. The runner then holds
    /// the job (`queue_paused`, its unposted engine id cleared) and `true` comes back: the
    /// handler returns any error, and its attempt ends the way a withdrawn one does.
    pub async fn hold_if_paused(&self) -> bool {
        self.jobs.hold_if_paused(self.id.as_str(), None, true).await
    }

    /// Settles the row's `withdrawn_id`, the prompt a pause withdrew (CORE-160), once the
    /// engine said where it is (CORE-120): gone (`follow` is `None`), or still the job's
    /// (`follow` names it, and it becomes the row's `external_id` again).
    ///
    /// # Errors
    /// As [`JobCtx::set_external_id`].
    pub async fn settle_withdrawn(&self, follow: Option<&str>) -> Result<(), ErrorBody> {
        self.jobs
            .handler_settle_withdrawn(&self.id, follow, &self.trace_id)
            .await
    }

    /// Reports an execution window of the job's engine work, whatever the attempt's outcome
    /// (CORE-065: a run that fails used the GPU too). A success's [`JobDone::windows`] are
    /// added by the runner.
    pub fn add_window(&self, w: ExecWindow) {
        self.windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(w);
    }

    /// Every execution window the job's attempts reported so far, a success's included: what
    /// the completions (Task 15's meter) attribute energy to.
    pub fn windows(&self) -> Vec<ExecWindow> {
        self.windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A new UUID version 4 from the core's id source (an engine id the core mints, such as
    /// ComfyUI's `prompt_id`).
    pub fn uuid(&self) -> String {
        self.jobs.uuid()
    }

    /// Publishes progress on `job:<id>`; job summaries show the latest. It is the job's newest
    /// sign of life (`last_progress_at`), and the current phase's `value` and `max`.
    pub fn progress(&self, value: u64, max: u64, node: Option<&str>) {
        self.jobs.progress(&self.id, value, max, node);
    }

    /// The job's run is in `phase` now (Appendix G "Phase"): a phase other than the current
    /// one is added to its `subtasks`. Its summary is published on `jobs` and `job:<id>`; the
    /// row gets it with the job's next state change.
    pub fn set_phase(&self, phase: &str) {
        self.jobs.set_phase(&self.id, phase);
    }

    /// The engine served `nodes` from its cache (`cache_hit_nodes`, CORE-117); none leaves
    /// the list out.
    pub fn mark_cached(&self, nodes: &[String]) {
        if nodes.is_empty() {
            return;
        }
        self.jobs.report(&self.id, |r| {
            let hits = r.cache_hit_nodes.get_or_insert_with(Vec::new);
            for node in nodes {
                if !hits.contains(node) {
                    hits.push(node.clone());
                }
            }
        });
    }

    /// Every node of the job's prompt was served from the cache: it succeeds saying nothing
    /// changed (`all_cached`, [`NOTHING_CHANGED`]).
    pub fn set_all_cached(&self) {
        self.jobs.report(&self.id, |r| r.all_cached = true);
    }

    /// The GPU adapter the job runs on (`engine_device`).
    pub fn set_engine_device(&self, name: &str) {
        self.jobs
            .report(&self.id, |r| r.engine_device = Some(name.to_string()));
    }

    /// A node of the job began executing: a sign of life, and `indeterminate` until it
    /// reports measurable progress ([`JobCtx::measured`]).
    pub fn node_started(&self) {
        self.jobs.node_started(&self.id);
    }

    /// The executing node reports measurable progress: no longer `indeterminate`.
    pub fn measured(&self) {
        self.jobs.report(&self.id, |r| r.indeterminate = false);
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
    /// Not now, for another reason (its engine runs prompts from outside the suite, ARCH-069):
    /// the job stays `waiting` and is asked again on every [`Jobs::kick`].
    Wait,
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
    /// The job had external side effects and was `interrupted`: it may have run, so an
    /// agent's retry needs your confirmation whatever its tool (ruling P3-R21).
    pub interrupted_with_side_effects: bool,
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
    /// RFC 3339: the jobs whose `updated` is later.
    #[serde(default)]
    pub since: Option<String>,
    /// The jobs of this batch.
    #[serde(default)]
    pub batch_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-117
    fn substatus_wire_names_round_trip() {
        for s in [
            Substatus::Waiting,
            Substatus::WaitingForVram,
            Substatus::QueuePaused,
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
