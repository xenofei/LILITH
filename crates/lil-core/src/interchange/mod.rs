//! The `interchange` module (CORE-014; APP-022, APP-061, APP-063–066, APP-146): the app
//! handler registry, `open_in` and `ui.show`, and the notifier. `lil:` activation and
//! thumbnails are the apps'.
//!
//! - `handlers`: registrations (`apps\<app>.toml`), targets and which app handles what;
//! - `notify`: the notifier, the core's toasts and the notification path (plan 4a);
//! - `toast_xml`: the toast XML (Appendix G "Toast XML");
//! - `tools`: `handlers.for`, `open_in`, `ui.show` and `notify.claude_code`.
//!
//! An agent's `open_in` or `ui.show` that names an app posts an `agent_activity` toast
//! ([`notify::Notifier::agent_opened`], APP-065), once the server has set the notifier.
//!
//! **The core never launches an app** (APP-064): a window the core opened could not come
//! forward from a terminal's job object. [`Interchange::open_in`] answers
//! `{running, pid, launch_command}`:
//!
//! - the app is running (a `ui:<app>` connection is open): the core publishes
//!   `{target, intent, focus}` on `open_requests:<app>`, and `pid` is the app's process.
//!   `focus` is `false` for every agent caller (`assistant`, `mcp`, `cli`) and `true` for an
//!   app (APP-065), so an agent's request shows the content without activating the window;
//! - it isn't: nothing is published, and `launch_command` is the registered `exe`, its
//!   registered `args` and `lil:open/<target>`, for the requester to run.
//!
//! The target is passed on as given, as one argument. It was checked, never executed or
//! parsed beyond its type and format.

pub mod handlers;
pub mod notify;
pub mod toast_xml;
pub mod tools;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use lil_protocol::{CallerKind, ErrorBody, codes};
use lil_schema::refs::AssetRef;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

pub use handlers::{
    AppRegistration, HandlerView, Intent, OpensEntry, Target, handlers_for_target,
    load_registrations,
};
pub use notify::{Notifier, NotifierParts, NotifyOutcome};
pub use toast_xml::{ToastActions, ToastText, toast_xml};
pub use tools::register_interchange_tools;

use crate::session::Session;
use crate::tools::CallCtx;
use crate::transport::ConnObserver;

/// The `module` of this module's tools (CORE-058).
pub const INTERCHANGE_MODULE: &str = "interchange";

/// How the requester starts an app that isn't running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LaunchCommand {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

/// The answer of `open_in` and `ui.show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OpenResult {
    pub running: bool,
    pub pid: Option<u32>,
    pub launch_command: Option<LaunchCommand>,
}

/// An open `ui:<app>` connection.
struct AppConn {
    app: String,
    pid: u32,
}

/// The handler registry and the running apps.
pub struct Interchange {
    /// The folder of the core executable: installed apps' `apps\` is beside it.
    core_dir: PathBuf,
    /// `%APPDATA%\LILITH` of this channel: external programs' `apps\` is in it.
    data_dir: PathBuf,
    /// The open `ui:*` connections, by connection id.
    connected: Mutex<BTreeMap<u64, AppConn>>,
    /// The notifier, once the server set it: `notify.claude_code` and the agent-activity toasts.
    notifier: OnceLock<Arc<Notifier>>,
}

impl std::fmt::Debug for Interchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interchange")
            .field("core_dir", &self.core_dir)
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl Interchange {
    pub fn new(core_dir: PathBuf, data_dir: PathBuf) -> Arc<Interchange> {
        Arc::new(Interchange {
            core_dir,
            data_dir,
            connected: Mutex::new(BTreeMap::new()),
            notifier: OnceLock::new(),
        })
    }

    /// Sets the notifier (once; a second call is ignored).
    pub fn set_notifier(&self, notifier: Arc<Notifier>) {
        let _ = self.notifier.set(notifier);
    }

    /// The notifier, once set.
    pub fn notifier(&self) -> Option<&Arc<Notifier>> {
        self.notifier.get()
    }

    /// An agent caller's request (APP-065): its `agent_activity` toast, posted in the
    /// background so the call doesn't wait for the desktop.
    fn agent_activity(&self, ctx: &CallCtx, target: &Target) {
        if !ctx.session.caller.is_agent() {
            return;
        }
        if let Some(notifier) = self.notifier().cloned() {
            let (title, text) = (target_title(&target.text), target.text.clone());
            tokio::spawn(async move {
                notifier.agent_opened(&title, &text).await;
            });
        }
    }

    fn connected(&self) -> MutexGuard<'_, BTreeMap<u64, AppConn>> {
        self.connected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The registrations on disk now: an app installed or registered after the core started
    /// is found by the next call.
    pub fn registrations(&self) -> Vec<AppRegistration> {
        load_registrations(&self.core_dir, &self.data_dir)
    }

    /// The pid of the running `app` (the oldest of its connections); `Some(0)` when Windows
    /// didn't report one.
    fn running(&self, app: &str) -> Option<u32> {
        self.connected()
            .values()
            .find(|c| c.app == app)
            .map(|c| c.pid)
    }

    /// The apps that can open `target`, with their intents (APP-061). A target that isn't
    /// valid has none.
    pub fn handlers_for(&self, target: &str) -> Vec<HandlerView> {
        Target::parse(target)
            .map(|t| handlers_for_target(&self.registrations(), &t))
            .unwrap_or_default()
    }

    /// Asks `app` to open `target` for `intent` (APP-063): see the module docs.
    ///
    /// # Errors
    /// `params.invalid` for a target that isn't one; `handler.none` when no app of that name
    /// is registered or it doesn't handle the target for the intent.
    pub async fn open_in(
        &self,
        ctx: &CallCtx,
        app: &str,
        target: &str,
        intent: Intent,
    ) -> Result<OpenResult, ErrorBody> {
        let target = resolve(ctx, target).await?;
        let regs = self.load_registrations().await;
        let Some(reg) = regs.iter().find(|r| r.app == app) else {
            return Err(ErrorBody::new(
                &codes::HANDLER_NONE,
                format!("No app is registered as `{app}`."),
                ctx.trace_id.clone(),
            ));
        };
        if !reg.intents_for(&target).contains(&intent) {
            return Err(ErrorBody::new(
                &codes::HANDLER_NONE,
                format!(
                    "`{app}` does not handle this target for intent {}. `handlers.for` lists \
                     what can.",
                    intent_name(intent)
                ),
                ctx.trace_id.clone(),
            ));
        }
        let result = self.deliver(ctx, reg, &target, intent);
        self.agent_activity(ctx, &target);
        Ok(result)
    }

    /// Shows `target` in the app that handles it, without taking focus when an agent asks
    /// (APP-063, APP-065): a running app that opens it, else the first registered one.
    ///
    /// # Errors
    /// `params.invalid` for a target that isn't one; `handler.none` when no app opens it.
    pub async fn ui_show(&self, ctx: &CallCtx, target: &str) -> Result<OpenResult, ErrorBody> {
        let target = resolve(ctx, target).await?;
        let regs = self.load_registrations().await;
        let handlers = handlers_for_target(&regs, &target);
        let openers = handlers
            .iter()
            .filter(|h| h.intents.contains(&Intent::Open));
        let chosen = openers
            .clone()
            .find(|h| self.running(&h.app).is_some())
            .or_else(|| openers.clone().next());
        let Some(reg) = chosen.and_then(|h| regs.iter().find(|r| r.app == h.app)) else {
            return Err(ErrorBody::new(
                &codes::HANDLER_NONE,
                "No registered app opens this target.",
                ctx.trace_id.clone(),
            ));
        };
        let result = self.deliver(ctx, reg, &target, Intent::Open);
        self.agent_activity(ctx, &target);
        Ok(result)
    }

    /// Tells a running app, or says how to start it. Never starts anything.
    fn deliver(
        &self,
        ctx: &CallCtx,
        reg: &AppRegistration,
        target: &Target,
        intent: Intent,
    ) -> OpenResult {
        if let Some(pid) = self.running(&reg.app) {
            // Agents never take focus (APP-065).
            let focus = !ctx.session.caller.is_agent();
            ctx.events.publish(
                &format!("open_requests:{}", reg.app),
                json!({"target": target.text, "intent": intent, "focus": focus}),
            );
            return OpenResult {
                running: true,
                pid: (pid != 0).then_some(pid),
                launch_command: None,
            };
        }
        let mut args = reg.args.clone();
        args.push(format!("lil:open/{}", target.text));
        OpenResult {
            running: false,
            pid: None,
            launch_command: Some(LaunchCommand {
                exe: reg.exe.clone(),
                args,
            }),
        }
    }

    /// [`Interchange::registrations`] on a blocking thread.
    async fn load_registrations(&self) -> Vec<AppRegistration> {
        let (core_dir, data_dir) = (self.core_dir.clone(), self.data_dir.clone());
        tokio::task::spawn_blocking(move || load_registrations(&core_dir, &data_dir))
            .await
            .unwrap_or_default()
    }
}

impl ConnObserver for Interchange {
    fn ready(&self, conn_id: u64, session: &Session) {
        if let CallerKind::Ui(app) = &session.caller {
            self.connected().insert(
                conn_id,
                AppConn {
                    app: app.clone(),
                    pid: session.client_pid,
                },
            );
        }
    }

    fn closed(&self, conn_id: u64) {
        self.connected().remove(&conn_id);
    }
}

/// `target` as a [`Target`], its asset type looked up in the call's workspace when it is a
/// ref (the type isn't in the ref).
async fn resolve(ctx: &CallCtx, text: &str) -> Result<Target, ErrorBody> {
    let mut target = Target::parse(text)
        .map_err(|why| ErrorBody::new(&codes::PARAMS_INVALID, why, ctx.trace_id.clone()))?;
    if target.kind.is_none()
        && let Ok(
            AssetRef::Sealed {
                ws, project, asset, ..
            }
            | AssetRef::Candidate {
                ws, project, asset, ..
            },
        ) = text.parse::<AssetRef>()
        && let Some(open) = &ctx.workspace
        && open.ws.id == ws
        && let Ok(Some(row)) = open.index.asset(&project, &asset).await
    {
        target.kind = row.asset_type;
    }
    Ok(target)
}

/// What a toast calls `target`: a path's or candidate's file name, a ref's asset (without its
/// `#…` part), else the text as given.
fn target_title(target: &str) -> String {
    let text = target.split(['#', '?']).next().unwrap_or(target);
    text.rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or(target)
        .to_string()
}

fn intent_name(intent: Intent) -> &'static str {
    match intent {
        Intent::Open => "open",
        Intent::Edit => "edit",
        Intent::UseAsInput => "use_as_input",
    }
}
