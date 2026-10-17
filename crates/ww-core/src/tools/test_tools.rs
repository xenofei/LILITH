//! The test-channel tools (spec Appendix D): registered only when the channel is `test-*`,
//! module `"test"`. Spec and E2E tests depend on them.
//!
//! | Tool | Effect | Arguments | Result (`structured`) |
//! |---|---|---|---|
//! | `test.echo` | read | `{text}` | `{text}` |
//! | `test.write` | write | `{label, fail?, pad?}` | `{label, executions}`; `fail: true` → `params.invalid`; `pad: n` adds `padding`, n bytes long |
//! | `test.block` | write | `{gate}` | `{gate, executions}` once a test opens the gate |
//! | `test.panic` | read | `{}` | panics |
//! | `test.state_panic` | read | `{}` | panics the suite state actor (QA-012) |
//! | `test.destructive` | destructive | `{}` | `{executed: true}` |
//! | `test.emit` | read | `{topic, data}` | `{topic, seq}`: `data` published on `test:<topic>` |
//! | `test.store_write` | write | `{path, text, expect: "new"\|"generated"\|"<sha256>"}` | `{sha256}`, through `Store::write` |
//! | `test.ingest_run` | write | `{project?, asset_id, files: [{name, text}], job_line: bool}` | `{run_id, staging_dir, files: [path]}` |
//! | `test.trash` | write | `{path}` | `{entry_id}`, through `Store::trash` |
//! | `test.watch_fence` | read | `{name}` | `{}` once the workspace's first reconcile has finished and the watcher has handled `<ws>\.worldwright\fence\<name>` |
//!
//! `test.store_write`, `test.ingest_run` and `test.trash` work in the session's workspace
//! (`workspace.required` without one). `test.ingest_run` does what an engine and the core do
//! for a candidate run: it writes the files into a staging handle with `fsops::write_temp`,
//! takes a run id from `Store::next_run_id`, and commits one candidate run in the ARCH-075
//! order (a new manifest if there is none, status by `transition(…, CandidateRun)`, each run
//! file with its `sha256`, and a jobs log line when `job_line` is true). An omitted `project`
//! is `sketchbook` (Task 12 replaces that with its resolver).
//!
//! `test.watch_fence` is the watcher fence (Global Constraints, Tests): `RawPipe::fence` writes
//! `<ws>\.worldwright\fence\<name>` (`<name>` is `<pid>-<n>`), then calls it, and it returns
//! once the workspace's index is ready and the watcher has handled that file and everything
//! before it. It also returns while git is busy (paths deferred for git count as handled), and
//! at once where the workspace has no folder watch.
//!
//! `pad` exists for the line-cap tests (ARCH-025): a result too large to send.
//!
//! `executions` counts the runs of that tool with that label or gate in this core, failed runs
//! included. `test.emit` is `read`: it changes no state, only publishes an event (so it needs
//! no idempotency key); a topic that makes no valid name is `params.invalid`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use ww_protocol::{EffectClass, ErrorBody, codes, is_valid_topic};
use ww_schema::{Manifest, Status, StatusEvent, is_asset_id, is_slug, sha256_hex, transition};

use super::registry::{CallCtx, Registry, typed_tool};
use crate::state::StateRegistry;
use crate::store::{CommitOp, Expect, ManifestEdit, OutputMove, StoreError, WsPath, fsops};

/// The module every test tool declares.
pub const TEST_MODULE: &str = "test";

/// The gates `test.block` waits on. A gate, once opened, stays open.
#[derive(Debug)]
pub struct TestGates {
    open: watch::Sender<HashSet<String>>,
}

impl Default for TestGates {
    fn default() -> Self {
        TestGates {
            open: watch::Sender::new(HashSet::new()),
        }
    }
}

impl TestGates {
    pub fn new() -> TestGates {
        TestGates::default()
    }

    /// Opens `gate`: every `test.block` waiting on it, and every later one, goes on.
    pub fn open(&self, gate: &str) {
        self.open.send_modify(|open| {
            open.insert(gate.to_string());
        });
    }

    /// Waits until `gate` is open.
    async fn wait(&self, gate: &str) {
        let mut rx = self.open.subscribe();
        // The sender lives as long as `self`, so this cannot fail while we borrow it.
        let _ = rx.wait_for(|open| open.contains(gate)).await;
    }
}

/// Run counters per tool and label.
#[derive(Debug, Default)]
struct Counters(Mutex<HashMap<(&'static str, String), u64>>);

impl Counters {
    /// Counts one more run of `tool` with `label`; returns the new count.
    fn bump(&self, tool: &'static str, label: &str) -> u64 {
        let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let n = counts.entry((tool, label.to_string())).or_insert(0);
        *n += 1;
        *n
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct EchoResult {
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    label: String,
    #[serde(default)]
    fail: bool,
    /// Adds a `padding` string of this many bytes to the result.
    #[serde(default)]
    pad: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
struct WriteResult {
    label: String,
    executions: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    padding: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BlockArgs {
    gate: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct BlockResult {
    gate: String,
    executions: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmitArgs {
    /// Published on `test:<topic>`.
    topic: String,
    data: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
struct EmitResult {
    topic: String,
    seq: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StoreWriteArgs {
    /// Workspace-relative, `/` separators.
    path: String,
    text: String,
    /// `new`, `generated` or a SHA-256 (64 lowercase hex).
    expect: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct StoreWriteResult {
    sha256: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IngestFile {
    name: String,
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IngestArgs {
    #[serde(default)]
    project: Option<String>,
    asset_id: String,
    files: Vec<IngestFile>,
    job_line: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct IngestResult {
    run_id: String,
    /// The staging handle's folder (removed once the run is committed).
    staging_dir: String,
    /// The run's files, workspace-relative.
    files: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrashArgs {
    /// Workspace-relative, `/` separators: a file or a folder.
    path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TrashResult {
    entry_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FenceArgs {
    /// The file `<ws>\.worldwright\fence\<name>` the caller wrote first.
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// What a tool that never returns normally would have returned.
#[derive(Debug, Serialize, JsonSchema)]
struct Nothing {}

#[derive(Debug, Serialize, JsonSchema)]
struct StatePanicResult {
    requested: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DestructiveResult {
    executed: bool,
}

/// `test.panic`'s body.
fn boom() -> Result<(Nothing, String), ErrorBody> {
    panic!("test.panic was called")
}

fn params_invalid(ctx: &CallCtx, message: String) -> ErrorBody {
    ErrorBody::new(&codes::PARAMS_INVALID, message, ctx.trace_id.clone())
}

/// `test.store_write`'s body.
async fn store_write(
    ctx: &CallCtx,
    args: StoreWriteArgs,
) -> Result<(StoreWriteResult, String), ErrorBody> {
    let store = &ctx.workspace()?.store;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let rel = WsPath::parse(&args.path).map_err(error)?;
    let expect = match args.expect.as_str() {
        "new" => Expect::New,
        "generated" => Expect::Generated,
        hex if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) => {
            Expect::Hash(hex.to_string())
        }
        other => {
            return Err(params_invalid(
                ctx,
                format!("expect must be new, generated or a SHA-256, not {other:?}"),
            ));
        }
    };
    let written = store
        .write(&rel, args.text.into_bytes(), expect)
        .await
        .map_err(error)?;
    let summary = format!("wrote {rel}");
    Ok((
        StoreWriteResult {
            sha256: written.sha256,
        },
        summary,
    ))
}

/// `test.trash`'s body: `Store::trash` with the reason `test.trash`.
async fn trash(ctx: &CallCtx, args: TrashArgs) -> Result<(TrashResult, String), ErrorBody> {
    let store = &ctx.workspace()?.store;
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let rel = WsPath::parse(&args.path).map_err(error)?;
    let record = store.trash(&rel, "test.trash").await.map_err(error)?;
    let summary = format!("moved {rel} to the trash as {}", record.entry_id);
    Ok((
        TrashResult {
            entry_id: record.entry_id,
        },
        summary,
    ))
}

/// `test.ingest_run`'s body (module docs).
async fn ingest_run(ctx: &CallCtx, args: IngestArgs) -> Result<(IngestResult, String), ErrorBody> {
    let store = ctx.workspace()?.store.clone();
    let error = |e: StoreError| e.to_error(&ctx.trace_id);
    let project = args.project.unwrap_or_else(|| "sketchbook".to_string());
    if !is_slug(&project) {
        return Err(params_invalid(
            ctx,
            format!("{project:?} is not a project slug"),
        ));
    }
    if !is_asset_id(&args.asset_id) {
        return Err(params_invalid(
            ctx,
            format!("{:?} is not an asset id", args.asset_id),
        ));
    }
    if args.files.is_empty() {
        return Err(params_invalid(
            ctx,
            "a run needs at least one file".to_string(),
        ));
    }
    let asset_id = args.asset_id;
    let asset_dir =
        WsPath::parse(&format!("projects/{project}/assets/{asset_id}")).map_err(error)?;
    let manifest_rel = asset_dir
        .join(&format!("{asset_id}.manifest.json"))
        .map_err(error)?;
    let current = match store.read(&manifest_rel).await.map_err(error)? {
        None => None,
        Some(bytes) => std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| Manifest::parse(t).ok())
            .map(|(m, _)| m),
    };
    let now = store
        .clock()
        .utc_now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let run_id = store
        .next_run_id(&asset_dir, current.as_ref(), now)
        .await
        .map_err(error)?;

    // Every name is checked before anything is written.
    let mut planned = Vec::with_capacity(args.files.len());
    let mut run_files = Vec::with_capacity(args.files.len());
    for file in args.files {
        if file.name.contains('/') || WsPath::parse(&file.name).is_err() {
            return Err(params_invalid(
                ctx,
                format!("{:?} is not a file name", file.name),
            ));
        }
        let to = asset_dir
            .join(&format!("candidates/{run_id}/{}", file.name))
            .map_err(error)?;
        let bytes = file.text.into_bytes();
        run_files.push(json!({
            "path": format!("candidates/{run_id}/{}", file.name),
            "role": "candidate",
            "sha256": sha256_hex(&bytes),
        }));
        planned.push((file.name, bytes, to));
    }

    let ids = store.ids().clone();
    let (edit_project, edit_asset, edit_run) = (project.clone(), asset_id.clone(), run_id.clone());
    let (edit_rel, created) = (manifest_rel.clone(), ww_schema::time::rfc3339(now));
    let manifest: ManifestEdit = Arc::new(move |m: &mut Manifest| {
        if m.get("asset_id").is_none() {
            *m = Manifest::new(&edit_project, &edit_asset, "image", &ids.uuid());
        }
        let view = m.view();
        let parse = |key: &str| {
            m.get(key)
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Status>().ok())
        };
        let from = view.status.unwrap_or(Status::Planned);
        let moved = transition(
            from,
            parse("status_before_stale"),
            parse("status_before_blocked"),
            &StatusEvent::CandidateRun,
        )
        .map_err(StoreError::InvalidTransition)?;
        let target = view
            .version
            .map_or_else(|| "1.0.0".to_string(), |v| v.to_string());
        m.set("status", Value::from(moved.to.as_str()));
        m.push(
            "candidate_runs",
            json!({
                "run_id": edit_run,
                "path": format!("candidates/{edit_run}/"),
                "status": "open",
                "created": created,
                "target_version": target,
                "files": run_files,
                "pinned": false,
            }),
        )
        .map_err(|e| StoreError::manifest_edit(&edit_rel, &e))
    });
    let job_line = args.job_line.then(|| {
        let mut record = serde_json::Map::new();
        record.insert("job_id".into(), Value::from(store.ids().id("j")));
        record.insert("kind".into(), Value::from("ingest"));
        record.insert("project".into(), Value::from(project.as_str()));
        record.insert("asset_id".into(), Value::from(asset_id.as_str()));
        record.insert("run_id".into(), Value::from(run_id.as_str()));
        record
    });

    // As an engine would: the files go into a staging handle, flushed; then the commit.
    let handle = store.staging_handle().await.map_err(error)?;
    let committed = async {
        let mut outputs = Vec::with_capacity(planned.len());
        for (name, bytes, to) in planned {
            let (dir, file, fp) = (handle.dir.clone(), name.clone(), store.failpoints().clone());
            let staged =
                tokio::task::spawn_blocking(move || fsops::write_temp(&dir, &file, &bytes, &fp))
                    .await
                    .unwrap_or_else(|e| {
                        Err(std::io::Error::other(format!("the write stopped: {e}")))
                    })
                    .map_err(|e| {
                        ErrorBody::new(
                            &codes::STORE_IO,
                            format!("the staged file {name} can't be written: {e}"),
                            ctx.trace_id.clone(),
                        )
                    })?;
            outputs.push(OutputMove { from: staged, to });
        }
        store
            .commit(CommitOp {
                outputs,
                moved: Vec::new(),
                manifest: Some((manifest_rel, manifest)),
                job_line,
                progress: None,
            })
            .await
            .map_err(error)
    }
    .await;
    let staging_dir = handle.dir.display().to_string();
    // Given back either way: an emptied handle goes, one that still holds files stays as an
    // orphan.
    let released = store.release_handle(handle).await;
    let report = committed?;
    released.map_err(error)?;
    let files: Vec<String> = report
        .outputs
        .iter()
        .map(|w| w.path.as_str().to_string())
        .collect();
    let summary = format!("ingested {} file(s) as {run_id}", files.len());
    Ok((
        IngestResult {
            run_id,
            staging_dir,
            files,
        },
        summary,
    ))
}

/// Registers the test-channel tools on `reg`. `test.block` waits on `gates`;
/// `test.state_panic` panics the actor of `states`' suite database.
pub fn register_test_tools(reg: &mut Registry, gates: Arc<TestGates>, states: Arc<StateRegistry>) {
    let counters = Arc::new(Counters::default());

    reg.register(typed_tool(
        "test.echo",
        "Test channels only: returns its text.",
        EffectClass::Read,
        TEST_MODULE,
        |_ctx, args: EchoArgs| {
            Box::pin(async move {
                let summary = format!("echoed {} characters", args.text.chars().count());
                Ok((EchoResult { text: args.text }, summary))
            })
        },
    ));

    let write_counters = counters.clone();
    reg.register(typed_tool(
        "test.write",
        "Test channels only: a write that counts its runs per label. `fail: true` returns params.invalid; `pad: n` adds n bytes of padding to the result.",
        EffectClass::Write,
        TEST_MODULE,
        move |ctx, args: WriteArgs| {
            let executions = write_counters.bump("test.write", &args.label);
            Box::pin(async move {
                if args.fail {
                    return Err(ErrorBody::new(
                        &codes::PARAMS_INVALID,
                        "test.write was asked to fail",
                        ctx.trace_id.clone(),
                    ));
                }
                let summary = format!("run {executions}");
                Ok((
                    WriteResult {
                        label: args.label,
                        executions,
                        padding: "p".repeat(args.pad),
                    },
                    summary,
                ))
            })
        },
    ));

    let block_counters = counters;
    reg.register(typed_tool(
        "test.block",
        "Test channels only: a write that waits until an in-process test opens its gate.",
        EffectClass::Write,
        TEST_MODULE,
        move |_ctx, args: BlockArgs| {
            let gates = gates.clone();
            let counters = block_counters.clone();
            Box::pin(async move {
                gates.wait(&args.gate).await;
                let executions = counters.bump("test.block", &args.gate);
                Ok((
                    BlockResult {
                        gate: args.gate,
                        executions,
                    },
                    format!("run {executions}"),
                ))
            })
        },
    ));

    reg.register(typed_tool(
        "test.panic",
        "Test channels only: panics (QA-012).",
        EffectClass::Read,
        TEST_MODULE,
        |_ctx, _args: NoArgs| Box::pin(async move { boom() }),
    ));

    reg.register(typed_tool(
        "test.state_panic",
        "Test channels only: makes the suite state actor panic, which stops the core (QA-012).",
        EffectClass::Read,
        TEST_MODULE,
        move |_ctx, _args: NoArgs| {
            let states = states.clone();
            Box::pin(async move {
                states.suite().panic_actor().await;
                Ok((
                    StatePanicResult { requested: true },
                    "the suite state actor was asked to panic".to_string(),
                ))
            })
        },
    ));

    reg.register(typed_tool(
        "test.destructive",
        "Test channels only: a destructive tool that does nothing.",
        EffectClass::Destructive,
        TEST_MODULE,
        |_ctx, _args: NoArgs| {
            Box::pin(async move { Ok((DestructiveResult { executed: true }, "executed".into())) })
        },
    ));

    reg.register(typed_tool(
        "test.store_write",
        "Test channels only: writes `text` to `path` in the session's workspace through the store, checked against `expect` (new, generated or a SHA-256).",
        EffectClass::Write,
        TEST_MODULE,
        |ctx, args: StoreWriteArgs| Box::pin(store_write(ctx, args)),
    ));

    reg.register(typed_tool(
        "test.ingest_run",
        "Test channels only: writes `files` into a staging handle as an engine would, then commits them as one candidate run of the asset.",
        EffectClass::Write,
        TEST_MODULE,
        |ctx, args: IngestArgs| Box::pin(ingest_run(ctx, args)),
    ));

    reg.register(typed_tool(
        "test.trash",
        "Test channels only: moves `path` (a file or folder in the session's workspace) to the workspace trash through the store.",
        EffectClass::Write,
        TEST_MODULE,
        |ctx, args: TrashArgs| Box::pin(trash(ctx, args)),
    ));

    reg.register(typed_tool(
        "test.watch_fence",
        "Test channels only: returns once the workspace's first reconcile has finished and the watcher has handled the file `.worldwright/fence/<name>` and everything before it.",
        EffectClass::Read,
        TEST_MODULE,
        |ctx, args: FenceArgs| {
            Box::pin(async move {
                let open = ctx.workspace()?;
                open.index.ready().await;
                if let Some(watch) = &open.watch {
                    watch.fence(&args.name).await;
                }
                Ok((Nothing {}, format!("fence {} passed", args.name)))
            })
        },
    ));

    reg.register(typed_tool(
        "test.emit",
        "Test channels only: publishes `data` as an event on the topic `test:<topic>`.",
        EffectClass::Read,
        TEST_MODULE,
        |ctx, args: EmitArgs| {
            Box::pin(async move {
                let topic = format!("{TEST_MODULE}:{}", args.topic);
                if !is_valid_topic(&topic) {
                    return Err(ErrorBody::new(
                        &codes::PARAMS_INVALID,
                        format!("{topic:?} is not a valid topic name"),
                        ctx.trace_id.clone(),
                    ));
                }
                let seq = ctx.events.publish(&topic, args.data);
                let summary = format!("published {topic} seq {seq}");
                Ok((EmitResult { topic, seq }, summary))
            })
        },
    ));
}
