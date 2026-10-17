//! Which workspace a session works in (ARCH-030 on the core side, ARCH-090, CORE-007):
//! infrastructure, not a CORE-014 module.
//!
//! A hello names a workspace by its id, the 16-hex hash of its canonical path
//! (`Env::workspace_hash`), or says `"none"`. The core serves only the default workspace of
//! `suite.toml` (owner decision 2): another registered workspace is `workspace.unsupported`,
//! with the way to make it the default; anything else is `workspace.unknown`.
//!
//! Before a workspace is used, the harness guard sees its path (`check_workspace`), so a core
//! on a test channel never opens a workspace registered in a real `suite.toml` (QA-040).
//!
//! [`WorkspaceHub`] opens a workspace once per id (ARCH-028, CORE-005): `core.lock`, its
//! `state.db`, its [`Store`] and the store's recovery, and the orphan list. The core that
//! takes `<ws>\.worldwright\core.lock` serves the workspace [`Access::ReadWrite`]; every other
//! core serves it [`Access::ReadOnly`]: read tools work, write tools get
//! `workspace.read_only`, and its `state.db` is never written (an in-memory one stands in when
//! there is none on disk).
//!
//! When the default changes, the hub lets go of the old default ([`WorkspaceHub::close`]): it
//! closes once no call holds it, and until then an open in the same core takes it back as it
//! is, so this core's own running idempotency rows are never released by a reopen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use serde_json::{Value, json};
use tokio::sync::{OnceCell, watch};
use ww_platform::fs::{LockedFile, Probe};
use ww_platform::paths::strip_verbatim;
use ww_platform::{Clock, Env, IdSource, check_workspace};
use ww_protocol::codes::{
    CORE_DRAINING, STATE_UNAVAILABLE, STORE_IO, WORKSPACE_UNKNOWN, WORKSPACE_UNSUPPORTED,
};
use ww_protocol::{AgentHint, ErrorBody, Fix, NO_WORKSPACE, UiAction};
use ww_schema::SuiteToml;

use crate::events::EventBus;
use crate::state::{StateDb, StateError, StateRegistry};
use crate::store::{FailPoints, Store, WsPath, fsops};

/// The UI action and the tool that make another workspace the default (`ww workspace use`).
pub const SET_DEFAULT_ACTION: &str = "workspace.set_default";

/// A resolved workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    /// 16 lowercase hex: the hash of the canonical path.
    pub id: String,
    /// The canonical path, without a `\\?\` prefix.
    pub path: PathBuf,
}

/// Resolves the `workspace` of a hello: `"none"` is `None`; the id of the default workspace
/// is that workspace.
///
/// # Errors
/// - `workspace.unsupported`: the id of another registered workspace; the message names the
///   default workspace and how to switch;
/// - `workspace.unknown`: any other id, including a registered workspace whose folder is gone.
///
/// # Panics
/// When the harness guard is armed and the workspace is registered in a real `suite.toml`
/// (QA-040).
pub fn resolve_workspace(
    env: &Env,
    requested: &str,
    trace_id: &str,
) -> Result<Option<WorkspaceRef>, ErrorBody> {
    if requested == NO_WORKSPACE {
        return Ok(None);
    }
    let unknown = |why: &str| {
        ErrorBody::new(
            &WORKSPACE_UNKNOWN,
            format!("The workspace {requested} is not one this core knows: {why}."),
            trace_id,
        )
    };
    let file = env.suite_toml();
    let suite = match std::fs::read_to_string(&file) {
        Ok(text) => match SuiteToml::parse(&text) {
            Ok(suite) => suite,
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "suite.toml is not valid");
                return Err(unknown(
                    "the workspace registry (suite.toml) cannot be read",
                ));
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(unknown("no workspace is registered"));
        }
        Err(e) => {
            tracing::warn!(file = %file.display(), error = %e, "cannot read suite.toml");
            return Err(unknown(
                "the workspace registry (suite.toml) cannot be read",
            ));
        }
    };

    let default = suite.default_workspace.as_deref().filter(|p| !p.is_empty());
    if let Some(default) = default
        && let Some((id, canonical)) = id_of(Path::new(default))
        && id == requested
    {
        // The guard sees the path as registered and as resolved (it compares text).
        check_workspace(Path::new(default));
        check_workspace(&canonical);
        return Ok(Some(WorkspaceRef {
            id,
            path: canonical,
        }));
    }

    let other = suite
        .workspaces
        .iter()
        .map(|w| w.path.as_str())
        .filter(|p| !p.is_empty() && Some(*p) != default)
        .find(|p| id_of(Path::new(p)).is_some_and(|(id, _)| id == requested));
    match other {
        Some(path) => Err(unsupported(default, path, trace_id)),
        None => Err(unknown(
            "it is not a registered workspace, or its folder no longer exists",
        )),
    }
}

/// `workspace.unsupported` for the registered workspace at `requested` (owner decision 2,
/// QA-003): the message names the default workspace and says how to switch, with the path
/// quoted (`ww workspace use "<path>"`); the fix is the UI action and the agent hint
/// `workspace.set_default {path}`.
pub(crate) fn unsupported(default: Option<&str>, requested: &str, trace_id: &str) -> ErrorBody {
    let serves = match default {
        Some(default) => format!("This core serves only the default workspace, \"{default}\"."),
        None => "This core serves only the default workspace, and none is set.".to_string(),
    };
    ErrorBody {
        fix: Some(Fix {
            ui_action: UiAction {
                id: SET_DEFAULT_ACTION.to_string(),
                label: format!("Use {requested} as the default workspace"),
            },
            agent_hint: Some(AgentHint {
                tool: SET_DEFAULT_ACTION.to_string(),
                arguments: json!({ "path": requested }),
            }),
        }),
        ..ErrorBody::new(
            &WORKSPACE_UNSUPPORTED,
            format!(
                "{serves} To use \"{requested}\" instead, make it the default with `ww workspace use \"{requested}\"`."
            ),
            trace_id,
        )
    }
}

/// The id of the workspace at `path` and its canonical path without `\\?\`, or `None` if its
/// folder does not exist.
fn id_of(path: &Path) -> Option<(String, PathBuf)> {
    let canonical = std::fs::canonicalize(path).ok()?;
    if !canonical.is_dir() {
        return None;
    }
    let id = Env::workspace_hash(&canonical);
    // Shown as `hello.workspace.path`: the prefix goes, the case stays (never `folder_key`).
    Some((id, strip_verbatim(&canonical)))
}

// ---------------------------------------------------------------------------------------------
// Open workspaces (ARCH-028).
// ---------------------------------------------------------------------------------------------

/// How this core serves a workspace (ARCH-028).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// This core holds the workspace's `core.lock`.
    ReadWrite,
    /// Another core holds it: reads only, and the `state.db` is never written.
    ReadOnly,
}

/// A workspace this core serves: what [`WorkspaceHub::open`] set up, shared by every call.
/// `core.lock` (read-write only) is held until the hub closes it, or until the last reference
/// goes: once the hub has let go of it ([`WorkspaceHub::close`]), the last call that holds it
/// closes its `state.db` and releases `core.lock` as it drops it.
pub struct OpenWorkspace {
    pub ws: WorkspaceRef,
    pub access: Access,
    pub state: Arc<StateDb>,
    pub store: Arc<Store>,
    /// The orphans listed at open (CORE-038).
    pub orphans: Vec<WsPath>,
    /// `core.lock`, held while the workspace is open read-write.
    lock: Mutex<Option<LockedFile>>,
    /// Closes the workspace's databases when it is dropped.
    states: Arc<StateRegistry>,
    /// Set once the workspace is closed; a drop then does nothing more.
    shut: AtomicBool,
    /// Dropped last, after the drop has closed everything: the hub waits for it before it
    /// opens the workspace anew ([`WorkspaceHub::open`]).
    gone: watch::Sender<()>,
}

impl std::fmt::Debug for OpenWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenWorkspace")
            .field("ws", &self.ws)
            .field("access", &self.access)
            .field("orphans", &self.orphans)
            .finish_non_exhaustive()
    }
}

impl OpenWorkspace {
    /// Closes the workspace now: its `state.db` (its queued jobs run first), then `core.lock`.
    /// A blocking call; closing again does nothing.
    fn close_now(&self) {
        if self.shut.swap(true, Ordering::SeqCst) {
            return;
        }
        self.states.close_workspace(&self.ws.id);
        drop(lock(&self.lock).take());
        tracing::info!(workspace = %self.ws.path.display(), "workspace closed");
    }
}

impl Drop for OpenWorkspace {
    /// The last reference closes the workspace (a short blocking call: the `state.db` actor
    /// finishes its queued jobs), so a call that held a workspace the hub let go of releases
    /// `core.lock` before its response is sent.
    fn drop(&mut self) {
        self.close_now();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A workspace the hub let go of while calls still held it.
struct Closing {
    open: Weak<OpenWorkspace>,
    /// Closed once the workspace's drop has finished closing it.
    gone: watch::Receiver<()>,
}

/// The open workspaces, one once-cell per id, those let go of while still in use, and whether
/// the hub has closed.
#[derive(Default)]
struct Opens {
    cells: HashMap<String, Arc<OnceCell<Arc<OpenWorkspace>>>>,
    closing: HashMap<String, Closing>,
    closed: bool,
}

/// Opens and closes the workspaces this core serves (ARCH-028), and knows the default one.
pub struct WorkspaceHub {
    env: Env,
    states: Arc<StateRegistry>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    failpoints: Arc<FailPoints>,
    probe: Arc<dyn Probe>,
    /// The default workspace of `suite.toml`, as last read.
    default: Mutex<Option<WorkspaceRef>>,
    opens: Mutex<Opens>,
}

impl std::fmt::Debug for WorkspaceHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceHub")
            .field("default", &*lock(&self.default))
            .finish_non_exhaustive()
    }
}

impl WorkspaceHub {
    /// A hub that has opened nothing yet; it reads the default workspace from `suite.toml`.
    pub fn new(
        env: Env,
        states: Arc<StateRegistry>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        failpoints: Arc<FailPoints>,
        probe: Arc<dyn Probe>,
    ) -> Arc<WorkspaceHub> {
        let hub = Arc::new(WorkspaceHub {
            env,
            states,
            events,
            clock,
            ids,
            failpoints,
            probe,
            default: Mutex::new(None),
            opens: Mutex::new(Opens::default()),
        });
        hub.refresh_default();
        hub
    }

    /// The fail points of the workspace stores and of `suite.toml` writes.
    pub fn failpoints(&self) -> &Arc<FailPoints> {
        &self.failpoints
    }

    /// The core's clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The id of the default workspace, as last read from `suite.toml`.
    pub fn default_id(&self) -> Option<String> {
        lock(&self.default).as_ref().map(|w| w.id.clone())
    }

    /// The default workspace, as last read from `suite.toml`.
    pub fn default_ref(&self) -> Option<WorkspaceRef> {
        lock(&self.default).clone()
    }

    /// Re-reads the default workspace from `suite.toml`. No file, a file that doesn't parse
    /// or a default whose folder is gone is no default.
    pub fn refresh_default(&self) {
        let found = default_of(&self.env);
        *lock(&self.default) = found;
    }

    /// `workspace.unsupported` for a session in `ws`, which is not the default workspace
    /// (owner decision 2), with the way to make it the default.
    pub fn unsupported(&self, ws: &WorkspaceRef, trace_id: &str) -> ErrorBody {
        let default = self.default_ref().map(|d| d.path.display().to_string());
        unsupported(default.as_deref(), &ws.path.display().to_string(), trace_id)
    }

    /// The open workspace `ws`, opened on first use: one open per id, behind a once-cell, so
    /// concurrent first calls share it. A failed open is retried by the next call. The errors
    /// carry an empty trace id; the caller sets its own.
    ///
    /// A workspace the hub let go of ([`WorkspaceHub::close`]) while calls still hold it is
    /// taken back as it is: the same `core.lock`, `state.db` and running idempotency rows, so a
    /// reopen in the same core never releases this core's own in-flight calls. Once its last
    /// holder has closed it, a new open waits for that close to finish before it starts.
    ///
    /// The open, in order:
    /// 1. `check_workspace` (the harness guard, QA-040);
    /// 2. `<ws>\.worldwright\` and its `core.lock`: taken → [`Access::ReadWrite`], held by
    ///    another core → [`Access::ReadOnly`];
    /// 3. the workspace `state.db` for that access;
    /// 4. the store, and its `recover()` (read-write only);
    /// 5. `orphans()`, published once on the `workspace` topic as
    ///    `{kind: "orphans", paths}` when there are any.
    ///
    /// # Errors
    /// `store.io` when `core.lock` can't be opened, `state.unavailable` when the `state.db`
    /// can't, and `core.draining` once the hub has closed.
    ///
    /// # Panics
    /// When the harness guard is armed and `ws` is registered in a real `suite.toml`.
    pub async fn open(&self, ws: &WorkspaceRef) -> Result<Arc<OpenWorkspace>, ErrorBody> {
        let (cell, wait) = {
            let mut opens = lock(&self.opens);
            if opens.closed {
                return Err(ErrorBody::new(
                    &CORE_DRAINING,
                    "The core is shutting down. Retry with the same idempotency key once it runs \
                     again.",
                    "",
                ));
            }
            let mut wait = None;
            if !opens.cells.contains_key(&ws.id)
                && let Some(closing) = opens.closing.remove(&ws.id)
            {
                if let Some(live) = closing.open.upgrade() {
                    tracing::info!(workspace = %ws.path.display(), "a workspace still in use is taken back");
                    let cell = Arc::new(OnceCell::new_with(Some(live.clone())));
                    opens.cells.insert(ws.id.clone(), cell);
                    return Ok(live);
                }
                wait = Some(closing.gone);
            }
            (opens.cells.entry(ws.id.clone()).or_default().clone(), wait)
        };
        cell.get_or_try_init(|| async move {
            if let Some(mut gone) = wait {
                // Ends (with an error) once the last holder's drop has closed everything.
                let _ = gone.changed().await;
            }
            self.open_now(ws).await
        })
        .await
        .cloned()
    }

    async fn open_now(&self, ws: &WorkspaceRef) -> Result<Arc<OpenWorkspace>, ErrorBody> {
        // 1.
        check_workspace(&ws.path);
        // 2.
        let root = ws.path.clone();
        let taken = tokio::task::spawn_blocking(move || {
            let dir = root.join(".worldwright");
            fsops::ensure_dir(&dir)?;
            fsops::lock_exclusive(&dir.join("core.lock"))
        })
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(format!("the open stopped: {e}"))))
        .map_err(|e| {
            tracing::error!(workspace = %ws.path.display(), error = %e, "core.lock can't be opened");
            ErrorBody::new(
                &STORE_IO,
                format!(
                    "The workspace {} can't be opened: its .worldwright\\core.lock can't be \
                     opened ({e}).",
                    ws.path.display()
                ),
                "",
            )
        })?;
        let access = if taken.is_some() {
            Access::ReadWrite
        } else {
            Access::ReadOnly
        };
        // 3.
        let state = self
            .states
            .for_workspace(ws, access)
            .await
            .map_err(|e| state_unavailable(ws, &e))?;
        // 4.
        let store = Arc::new(
            Store::new(
                ws.path.clone(),
                state.clone(),
                self.failpoints.clone(),
                self.clock.clone(),
                self.ids.clone(),
                self.probe.clone(),
            )
            .with_access(access)
            .with_machine(&self.env.machine_id()),
        );
        if access == Access::ReadWrite {
            match store.recover().await {
                Ok(report) => {
                    tracing::info!(workspace = %ws.path.display(), ?report, "workspace recovered")
                }
                Err(e) => {
                    tracing::warn!(workspace = %ws.path.display(), error = %e, "the workspace recovery stopped; it runs again at the next open")
                }
            }
        }
        // 5.
        let orphans = store.orphans().await.unwrap_or_else(|e| {
            tracing::warn!(workspace = %ws.path.display(), error = %e, "the orphans can't be listed");
            Vec::new()
        });
        if !orphans.is_empty() {
            let paths: Vec<Value> = orphans.iter().map(|p| Value::from(p.as_str())).collect();
            self.events
                .publish("workspace", json!({"kind": "orphans", "paths": paths}));
        }
        tracing::info!(
            workspace = %ws.path.display(),
            ?access,
            orphans = orphans.len(),
            "workspace open"
        );
        Ok(Arc::new(OpenWorkspace {
            ws: ws.clone(),
            access,
            state,
            store,
            orphans,
            lock: Mutex::new(taken),
            states: self.states.clone(),
            shut: AtomicBool::new(false),
            gone: watch::Sender::new(()),
        }))
    }

    /// Lets go of the workspace `id` if it is open. Unless a call still holds it, it is closed
    /// now: its `state.db` is closed (its queued jobs run first), then `core.lock` is released.
    /// Otherwise the last call that holds it closes it as it ends, and until then a new
    /// [`WorkspaceHub::open`] takes it back as it is (its running idempotency rows stay this
    /// core's). A later open opens it anew, once the close has finished.
    ///
    /// The workspace is entered in the closing list (and its "gone" signal subscribed to)
    /// under the same lock that takes it out of the open cells, before this hub's reference is
    /// dropped. So an open racing the close always sees it: while it lives, the open takes it
    /// back; once its drop has begun, the open waits for that drop to release `core.lock` and
    /// close its `state.db` (both keyed by the workspace id) before it opens anew. A workspace
    /// whose first open is still running is left as it is.
    pub async fn close(&self, id: &str) {
        let open = {
            let mut opens = lock(&self.opens);
            let Some(open) = opens.cells.get(id).and_then(|c| c.get().cloned()) else {
                return;
            };
            opens.cells.remove(id);
            opens.closing.retain(|_, c| c.open.strong_count() > 0);
            opens.closing.insert(
                id.to_string(),
                Closing {
                    open: Arc::downgrade(&open),
                    gone: open.gone.subscribe(),
                },
            );
            open
        };
        if Arc::strong_count(&open) > 1 {
            tracing::info!(workspace = %open.ws.path.display(), "workspace let go of; it closes when its last call ends");
        }
        // The last reference closes it as it drops: on a blocking thread when that is this one.
        if let Err(e) = tokio::task::spawn_blocking(move || drop(open)).await {
            tracing::error!(error = %e, "closing a workspace failed");
        }
    }

    /// Closes every open workspace now, also those still in use, and opens none after. The
    /// core runs it at shutdown, after its calls have drained and before its state databases
    /// close.
    pub async fn close_all(&self) {
        let opens: Vec<Arc<OpenWorkspace>> = {
            let mut opens = lock(&self.opens);
            opens.closed = true;
            let cells: Vec<_> = opens.cells.drain().map(|(_, cell)| cell).collect();
            let closing: Vec<_> = opens.closing.drain().map(|(_, c)| c.open).collect();
            cells
                .into_iter()
                .filter_map(|c| c.get().cloned())
                .chain(closing.into_iter().filter_map(|w| w.upgrade()))
                .collect()
        };
        for open in opens {
            if let Err(e) = tokio::task::spawn_blocking(move || open.close_now()).await {
                tracing::error!(error = %e, "closing a workspace failed");
            }
        }
    }
}

/// The default workspace of `env`'s `suite.toml`, resolved; `None` for no file, a file that
/// doesn't parse, no default, or a default whose folder is gone.
fn default_of(env: &Env) -> Option<WorkspaceRef> {
    let text = std::fs::read_to_string(env.suite_toml()).ok()?;
    let suite = SuiteToml::parse(&text).ok()?;
    let default = suite.default_workspace.filter(|p| !p.is_empty())?;
    let (id, path) = id_of(Path::new(&default))?;
    Some(WorkspaceRef { id, path })
}

/// `state.unavailable` for a workspace `state.db` that can't be opened.
fn state_unavailable(ws: &WorkspaceRef, e: &StateError) -> ErrorBody {
    tracing::error!(workspace = %ws.path.display(), error = %e, "the workspace state.db can't be opened");
    ErrorBody::new(
        &STATE_UNAVAILABLE,
        format!(
            "The state database of the workspace {} is not available ({e}); retry with the same \
             idempotency key.",
            ws.path.display()
        ),
        "",
    )
}

#[cfg(test)]
mod tests {
    use ww_protocol::{ErrorCategory, RetryPolicy};
    use ww_testkit::TestEnv;

    use super::*;

    fn id(path: &Path) -> String {
        Env::workspace_hash(&std::fs::canonicalize(path).expect("canonicalize"))
    }

    #[test] // spec: ARCH-030
    fn none_means_no_workspace() {
        let env = TestEnv::new();
        // No suite.toml at all.
        assert_eq!(resolve_workspace(&env.env, "none", "t"), Ok(None));
        let ws = env.make_workspace("alpha");
        env.register_workspace(&ws, true);
        assert_eq!(resolve_workspace(&env.env, "none", "t"), Ok(None));
    }

    #[test] // spec: ARCH-030, ARCH-090
    fn default_workspace_id_resolves() {
        let env = TestEnv::with_label("Ünïcödé space");
        let other = env.make_workspace("other");
        let ws = env.make_workspace("Ünïcödé space");
        env.register_workspace(&other, false);
        env.register_workspace(&ws, true);
        let found = resolve_workspace(&env.env, &id(&ws), "t")
            .expect("the default workspace resolves")
            .expect("a workspace");
        assert_eq!(found.id, id(&ws));
        assert_eq!(found.id.len(), 16);
        assert!(
            !found.path.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            found.path.display()
        );
        assert_eq!(
            std::fs::canonicalize(&found.path).unwrap(),
            std::fs::canonicalize(&ws).unwrap()
        );
    }

    #[test] // spec: ARCH-030
    fn unregistered_id_is_unknown() {
        let env = TestEnv::new();
        let ws = env.make_workspace("alpha");
        let check = |requested: &str| {
            let err = resolve_workspace(&env.env, requested, "t_9").unwrap_err();
            assert_eq!(err.code, "workspace.unknown", "{requested}");
            assert_eq!(err.category, ErrorCategory::UserFixable);
            assert_eq!(err.retry, RetryPolicy::AfterResolve);
            assert_eq!(err.trace_id, "t_9");
        };
        // No suite.toml yet.
        check(&id(&ws));
        // Registered: a workspace that exists but is not listed, and ids that match nothing.
        let listed = env.make_workspace("listed");
        env.register_workspace(&listed, true);
        check(&id(&ws));
        check("0123456789abcdef");
        check("not-an-id");
        check("");
    }

    #[test] // spec: ARCH-090
    fn second_workspace_is_refused_naming_the_default() {
        let env = TestEnv::with_label("Ünïcödé space");
        let default = env.make_workspace("default");
        let second = env.make_workspace("second");
        env.register_workspace(&default, true);
        env.register_workspace(&second, false);
        let err = resolve_workspace(&env.env, &id(&second), "t").unwrap_err();
        assert_eq!(err.code, "workspace.unsupported");
        assert_eq!(err.category, ErrorCategory::UserFixable);
        let (default, second) = (default.display(), second.display());
        assert!(
            err.message.contains(&default.to_string()),
            "{}",
            err.message
        );
        assert!(err.message.contains("ww workspace use"), "{}", err.message);
        assert!(
            err.message
                .contains(&format!("`ww workspace use \"{second}\"`")),
            "{}",
            err.message
        );
        let fix = err.fix.expect("a fix");
        assert_eq!(fix.ui_action.id, "workspace.set_default");
        assert_eq!(
            fix.ui_action.label,
            format!("Use {second} as the default workspace")
        );
        let hint = fix.agent_hint.expect("an agent hint (QA-003)");
        assert_eq!(hint.tool, "workspace.set_default");
        assert_eq!(hint.arguments, json!({"path": second.to_string()}));
    }

    #[test] // spec: ARCH-090
    fn second_workspace_without_a_default_is_refused() {
        let env = TestEnv::new();
        let second = env.make_workspace("second");
        env.register_workspace(&second, false);
        let err = resolve_workspace(&env.env, &id(&second), "t").unwrap_err();
        assert_eq!(err.code, "workspace.unsupported");
        assert!(err.message.contains("ww workspace use"), "{}", err.message);
    }

    #[test] // spec: ARCH-030
    fn vanished_workspace_is_unknown() {
        let env = TestEnv::new();
        // Folders removed by dropping their `TempDir` (ww-core code itself never deletes).
        let default = tempfile::tempdir_in(env.root()).unwrap();
        let second = tempfile::tempdir_in(env.root()).unwrap();
        env.register_workspace(default.path(), true);
        env.register_workspace(second.path(), false);
        let (default_id, second_id) = (id(default.path()), id(second.path()));
        assert!(resolve_workspace(&env.env, &default_id, "t").is_ok());
        drop(default);
        drop(second);
        for gone in [default_id, second_id] {
            let err = resolve_workspace(&env.env, &gone, "t").unwrap_err();
            assert_eq!(err.code, "workspace.unknown");
        }
    }
}
