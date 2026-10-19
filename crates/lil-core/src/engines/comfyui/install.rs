//! Custom-node installs (CORE-131, ARCH-059; Appendix F "ComfyUI engine"): a `cpu`-lane job of
//! kind `comfy.install_node` that runs
//! `<comfy_cli> --skip-prompt --workspace <root> node registry-install <id> --version <version>`
//! in `<root>`, with the scrubbed environment (ARCH-063) and ComfyUI's Python first on `PATH`,
//! in a kill-on-close Job Object (a cancel ends it).
//!
//! The version must be an exact `major.minor.patch` and the id a registry id (never a URL or
//! anything that starts with `-`). comfy-cli asks the comfy registry, so every run that started
//! records one usage draft (provider `comfy-registry`, basis `free_tier`, `requests: 1`;
//! CORE-069), whatever its outcome. A success sets the engine's `restart_needed`; a failure is
//! `comfyui.node_install_failed` with comfy-cli's output as untrusted detail (ARCH-060).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

use lil_platform::engine_spawn::{EngineSpawn, spawn_engine};
use lil_platform::job_object::KillOnCloseJob;
use lil_protocol::{Detail, ErrorBody, codes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::engines::config::scrubbed_env;
use crate::engines::{ENGINES_MODULE, Engines};
use crate::jobs::{
    EngineKind, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobSpec, Lane, Priority,
    cancelled_error,
};
use crate::meter::{Attribution, UsageCtx, UsageDraft};
use crate::tools::stages::BillingBasis;

/// The job kind.
pub const INSTALL_KIND: &str = "comfy.install_node";
/// The tool the install runs for (its usage draft's `tool`).
pub const INSTALL_TOOL: &str = "engines.comfyui.install_node";
/// The provider comfy-cli asks.
pub const REGISTRY_PROVIDER: &str = "comfy-registry";
/// How long an install may run.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// The output lines a failure keeps.
const OUTPUT_TAIL: usize = 60;
/// How long the output readers get once comfy-cli has ended.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

/// A registry node id: `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`.
pub fn is_node_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && id.len() <= 128
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// An exact `major.minor.patch` of decimal digits (ARCH-059): no `latest`, range or suffix.
pub fn is_exact_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// A registry node id (checked when it is read).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct NodeId(
    /// `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`.
    #[schemars(regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"))]
    pub String,
);

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let id = String::deserialize(d)?;
        if is_node_id(&id) {
            Ok(NodeId(id))
        } else {
            Err(serde::de::Error::custom(format!(
                "{id:?} is not a registry node id (letters, digits, '.', '_' and '-', starting \
                 with a letter or digit)"
            )))
        }
    }
}

/// An exact `major.minor.patch` (checked when it is read).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ExactVersion(
    /// `major.minor.patch`.
    #[schemars(regex(pattern = r"^[0-9]+\.[0-9]+\.[0-9]+$"))]
    pub String,
);

impl<'de> Deserialize<'de> for ExactVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if is_exact_version(&v) {
            Ok(ExactVersion(v))
        } else {
            Err(serde::de::Error::custom(format!(
                "{v:?} is not an exact version: installs use a pinned major.minor.patch \
                 (ARCH-059)"
            )))
        }
    }
}

/// The job's arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallArgs {
    pub id: NodeId,
    pub version: ExactVersion,
    /// The caller kind of the call that submitted it (the usage draft's).
    pub caller_kind: String,
}

/// The `JobSpec` of an install of `id` at `version` for a call of `caller_kind`.
pub fn job_spec(
    id: &NodeId,
    version: &ExactVersion,
    caller_kind: &str,
    priority: Priority,
    project: Option<String>,
) -> JobSpec {
    let args = InstallArgs {
        id: id.clone(),
        version: version.clone(),
        caller_kind: caller_kind.to_string(),
    };
    JobSpec {
        kind: INSTALL_KIND,
        lane: Lane::Cpu,
        priority,
        module: ENGINES_MODULE.to_string(),
        args: serde_json::to_value(args).unwrap_or_default(),
        project,
        asset_id: None,
        doc_id: None,
        expected_secs: 0,
        expected_output_bytes: 0,
        vram_need_mib: None,
        engine: None,
        group: None,
        external_side_effects: false,
    }
}

/// What an install needs from `[engines.comfyui]`: comfy-cli, the root and ComfyUI's Python.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    pub comfy_cli: PathBuf,
    pub root: PathBuf,
    pub python: PathBuf,
}

/// The [`InstallPlan`] of `engines`' configuration.
///
/// # Errors
/// `engine.not_configured`, naming what is missing.
pub fn install_plan(engines: &Engines) -> Result<InstallPlan, ErrorBody> {
    let missing = |what: &str| {
        ErrorBody::new(
            &codes::ENGINE_NOT_CONFIGURED,
            format!(
                "Node installs need [engines.comfyui] {what} in suite.toml; it is not set or \
                 doesn't exist."
            ),
            "",
        )
    };
    let config = engines
        .comfy_config()
        .ok_or_else(|| missing("(the whole table)"))?;
    let comfy_cli = config
        .comfy_cli
        .clone()
        .filter(|p| p.is_file())
        .ok_or_else(|| missing("comfy_cli"))?;
    let root = config
        .root
        .clone()
        .filter(|p| p.is_dir())
        .ok_or_else(|| missing("root"))?;
    let python = config
        .python_path()
        .filter(|p| p.is_file())
        .ok_or_else(|| missing("python"))?;
    Ok(InstallPlan {
        comfy_cli,
        root,
        python,
    })
}

/// The scrubbed environment of the current process with the folder of `python` first on
/// `PATH`.
fn install_env(python: &Path) -> Vec<(OsString, OsString)> {
    let mut env = scrubbed_env(std::env::vars_os());
    let dir = python.parent().map(Path::to_path_buf).unwrap_or_default();
    let at = env
        .iter()
        .position(|(name, _)| name.to_string_lossy().eq_ignore_ascii_case("PATH"));
    let old = at.map(|i| env.remove(i).1);
    let mut paths = vec![dir];
    if let Some(old) = &old {
        paths.extend(std::env::split_paths(old));
    }
    let path = std::env::join_paths(paths).unwrap_or_else(|_| old.unwrap_or_default());
    env.push((OsString::from("PATH"), path));
    env
}

/// Runs `comfy.install_node` jobs (module docs).
pub struct NodeInstallHandler {
    engines: Weak<Engines>,
}

impl std::fmt::Debug for NodeInstallHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeInstallHandler").finish_non_exhaustive()
    }
}

/// How comfy-cli ended.
enum Ended {
    Exited(u32),
    Cancelled,
    TimedOut,
}

impl NodeInstallHandler {
    /// A handler for `engines` (held weakly: the jobs hold their handlers).
    pub fn new(engines: Weak<Engines>) -> NodeInstallHandler {
        NodeInstallHandler { engines }
    }

    async fn attempt(&self, job: &JobCtx) -> Result<JobDone, JobFailure> {
        let trace = |mut e: ErrorBody| {
            e.trace_id.clone_from(&job.trace_id);
            e
        };
        let args: InstallArgs = serde_json::from_value(job.spec.args.clone()).map_err(|e| {
            JobFailure::Final(trace(ErrorBody::new(
                &codes::PARAMS_INVALID,
                format!("{INSTALL_KIND} arguments: {e}"),
                "",
            )))
        })?;
        let Some(engines) = self.engines.upgrade() else {
            return Err(JobFailure::Final(trace(ErrorBody::new(
                &codes::CORE_DRAINING,
                "The core is shutting down.",
                "",
            ))));
        };
        let plan = install_plan(&engines).map_err(|e| JobFailure::Final(trace(e)))?;
        let (id, version) = (&args.id.0, &args.version.0);
        let kill_on_close = KillOnCloseJob::new().map_err(|e| {
            JobFailure::Final(trace(failed(
                format!("no Job Object for comfy-cli: {e}"),
                &[],
            )))
        })?;
        // Each line goes to the log and down the channel, which closes once both output
        // readers have ended.
        let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let on_line = Box::new(move |stream: &str, line: &str| {
            tracing::info!(target: "engine.comfyui", stream, run = "install_node", "{line}");
            let _ = line_tx.send(line.to_string());
        });
        let request = EngineSpawn {
            exe: plan.comfy_cli.clone(),
            args: vec![
                "--skip-prompt".into(),
                "--workspace".into(),
                plan.root.clone().into_os_string(),
                "node".into(),
                "registry-install".into(),
                id.into(),
                "--version".into(),
                version.into(),
            ],
            cwd: plan.root.clone(),
            env: install_env(&plan.python),
        };
        let cli = spawn_engine(&request, &kill_on_close, on_line).map_err(|e| {
            JobFailure::Final(trace(failed(
                format!("{} could not be started: {e}", plan.comfy_cli.display()),
                &[],
            )))
        })?;
        tracing::info!(id, version, pid = cli.pid(), "a node install runs");
        let mut cancel = job.cancel.clone();
        let ended = tokio::select! {
            code = cli.wait() => match code {
                Ok(code) => Ended::Exited(code),
                Err(_) => Ended::Exited(u32::MAX),
            },
            _ = cancel.wait_for(|c| *c) => Ended::Cancelled,
            () = tokio::time::sleep(INSTALL_TIMEOUT) => Ended::TimedOut,
        };
        // Whatever comfy-cli started ends with it; then its output is read to the end.
        let _ = kill_on_close.terminate();
        drop(kill_on_close);
        let mut tail: VecDeque<String> = VecDeque::new();
        let deadline = tokio::time::Instant::now() + OUTPUT_GRACE;
        while let Ok(Some(line)) = tokio::time::timeout_at(deadline, line_rx.recv()).await {
            if tail.len() == OUTPUT_TAIL {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        let lines: Vec<String> = tail.into();

        // comfy-cli ran, so the registry was asked (CORE-069).
        engines
            .comfy()
            .usage
            .record(draft(), usage_ctx(job, &args))
            .await;

        match ended {
            Ended::Exited(0) => {
                engines.set_restart_needed(EngineKind::ComfyUi);
                tracing::info!(
                    id,
                    version,
                    "the node pack was installed; ComfyUI needs a restart"
                );
                Ok(JobDone {
                    structured: json!({"id": id, "version": version, "restart_needed": true}),
                    artifacts: Vec::new(),
                    record: None,
                    windows: Vec::new(),
                })
            }
            Ended::Exited(code) => Err(JobFailure::Final(trace(failed(
                format!("comfy-cli exited with code {code} while installing {id} {version}."),
                &lines,
            )))),
            Ended::Cancelled => Err(JobFailure::Final(cancelled_error(&job.trace_id))),
            Ended::TimedOut => Err(JobFailure::Final(trace(failed(
                format!(
                    "comfy-cli was still installing {id} {version} after {} minutes; it was \
                     stopped.",
                    INSTALL_TIMEOUT.as_secs() / 60
                ),
                &lines,
            )))),
        }
    }
}

/// The usage draft of one registry install.
fn draft() -> UsageDraft {
    let mut units = serde_json::Map::new();
    units.insert("requests".to_string(), json!(1));
    UsageDraft {
        tool: INSTALL_TOOL.to_string(),
        provider: REGISTRY_PROVIDER.to_string(),
        model: None,
        route: "comfy-cli".to_string(),
        basis: BillingBasis::FreeTier,
        units,
        est_cost: None,
        account: None,
        raw_usage: None,
        attribution: Attribution::Suite,
    }
}

fn usage_ctx(job: &JobCtx, args: &InstallArgs) -> UsageCtx {
    UsageCtx {
        workspace: job.workspace.clone(),
        project: job.spec.project.clone(),
        asset: job.spec.asset_id.clone(),
        job_id: Some(job.id.to_string()),
        call_id: job.id.to_string(),
        caller_kind: args.caller_kind.clone(),
        reservation: job.reservation.clone(),
    }
}

/// `comfyui.node_install_failed`, with comfy-cli's output as untrusted detail.
fn failed(message: String, lines: &[String]) -> ErrorBody {
    let mut error = ErrorBody::new(&codes::COMFYUI_NODE_INSTALL_FAILED, message, "");
    if !lines.is_empty() {
        error.detail = Some(Detail {
            text: lines.join("\n"),
            untrusted: true,
        });
    }
    error
}

impl JobHandler for NodeInstallHandler {
    fn kind(&self) -> &'static str {
        INSTALL_KIND
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(self.attempt(job))
    }

    /// The install watches `job.cancel` itself and ends comfy-cli; nothing runs in ComfyUI.
    fn cancel_in_engine<'a>(&'a self, _job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-059
    fn only_exact_versions_and_registry_ids_pass() {
        for ok in ["1.2.3", "0.0.10", "10.20.30"] {
            assert!(is_exact_version(ok), "{ok}");
        }
        for bad in [
            "latest",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.3-rc1",
            "1.2.x",
            "",
            "1..3",
            " 1.2.3",
            "^1.2.3",
        ] {
            assert!(!is_exact_version(bad), "{bad}");
        }
        for ok in ["comfyui-impact-pack", "ComfyUI_Node", "a", "node.v2"] {
            assert!(is_node_id(ok), "{ok}");
        }
        for bad in [
            "",
            "-rf",
            "--version",
            "https://example.com/x",
            "a b",
            "a/b",
            "a\\b",
        ] {
            assert!(!is_node_id(bad), "{bad}");
        }
    }
}
