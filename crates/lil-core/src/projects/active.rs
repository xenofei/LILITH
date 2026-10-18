//! The active project (ARCH-088, ARCH-098): per client session, with `.worldwright\state.json`
//! `{"active_project":"<slug>"}` as the untracked last-used hint of the workspace.
//!
//! A call's project is, in this order: its `project` argument; the session's active project;
//! the hint; `sketchbook` ([`resolve_project`]). A call that names a project and ends `ok` makes
//! it the session's active project and, on a read-write workspace, the hint (the dispatcher
//! does both). A session's active project is forgotten when its connection closes.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use lil_schema::is_slug;
use serde_json::{Map, Value};

use crate::store::{Store, StoreError, WsPath};
use crate::workspace::{Access, OpenWorkspace};

/// The project of a call that names none, with no active project and no hint (ARCH-098).
pub const DEFAULT_PROJECT: &str = "sketchbook";
/// The hint file, workspace-relative.
pub const STATE_JSON: &str = ".worldwright/state.json";

/// The active project of each session, by session id.
#[derive(Debug, Default)]
pub struct ActiveProjects {
    by_session: Mutex<HashMap<String, String>>,
}

impl ActiveProjects {
    pub fn new() -> ActiveProjects {
        ActiveProjects::default()
    }

    fn map(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.by_session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The session's active project.
    pub fn get(&self, session_id: &str) -> Option<String> {
        self.map().get(session_id).cloned()
    }

    /// Makes `slug` the session's active project.
    pub fn set(&self, session_id: &str, slug: &str) {
        self.map().insert(session_id.to_string(), slug.to_string());
    }

    /// Forgets the session's active project (its connection closed).
    pub fn forget(&self, session_id: &str) {
        self.map().remove(session_id);
    }
}

/// The project of a call: `explicit`, else `active`, else `hint`, else `sketchbook`.
pub fn resolve_project(
    explicit: Option<&str>,
    active: Option<String>,
    hint: Option<String>,
) -> String {
    explicit
        .map(str::to_string)
        .or(active)
        .or(hint)
        .unwrap_or_else(|| DEFAULT_PROJECT.to_string())
}

fn state_json() -> WsPath {
    WsPath::parse(STATE_JSON).expect("the state.json path is valid")
}

/// The project `state.json` names, when it is a slug. A missing, unreadable or malformed file
/// is no hint.
pub async fn read_hint(store: &Store) -> Option<String> {
    let bytes = match store.read(&state_json()).await {
        Ok(bytes) => bytes?,
        Err(e) => {
            tracing::debug!(error = %e, "state.json can't be read; no active-project hint");
            return None;
        }
    };
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("active_project")
        .and_then(Value::as_str)
        .filter(|s| is_slug(s))
        .map(str::to_string)
}

/// Makes `slug` the hint in `state.json`, through the store's hash-checked edit. Other keys of
/// the file are kept; a file that isn't a JSON object is replaced. Nothing is written when the
/// file already names `slug`.
///
/// # Errors
/// The store's error (`workspace.read_only` on a read-only workspace).
pub async fn write_hint(store: &Store, slug: &str) -> Result<(), StoreError> {
    if read_hint(store).await.as_deref() == Some(slug) {
        return Ok(());
    }
    let slug = slug.to_string();
    store
        .edit_text(&state_json(), &move |current: &str| {
            let mut map = match serde_json::from_str::<Value>(current) {
                Ok(Value::Object(map)) => map,
                _ => Map::new(),
            };
            map.insert("active_project".to_string(), Value::from(slug.as_str()));
            Ok(Value::Object(map).to_string())
        })
        .await
        .map(drop)
}

/// What [`remember_project`] did with the hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintWrite {
    /// `state.json` names the project (written, or already so).
    Written,
    /// The workspace is served read-only (another core holds `core.lock`, ARCH-028), so the
    /// hint was left alone and no write was tried.
    ReadOnly,
}

/// After a call that named `slug` ended `ok` (ARCH-088): `slug` becomes the session's active
/// project and, on a read-write workspace, the workspace's hint. A read-only workspace's
/// `state.json` is never written to (a write would only be refused).
///
/// # Errors
/// The store's error when the hint can't be written; the active project is set anyway.
pub async fn remember_project(
    active: &ActiveProjects,
    session_id: &str,
    open: &OpenWorkspace,
    slug: &str,
) -> Result<HintWrite, StoreError> {
    active.set(session_id, slug);
    if open.access != Access::ReadWrite {
        return Ok(HintWrite::ReadOnly);
    }
    write_hint(&open.store, slug).await?;
    Ok(HintWrite::Written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-088, ARCH-098
    fn explicit_then_active_then_hint_then_sketchbook() {
        let s = |x: &str| Some(x.to_string());
        assert_eq!(resolve_project(Some("a"), s("b"), s("c")), "a");
        assert_eq!(resolve_project(None, s("b"), s("c")), "b");
        assert_eq!(resolve_project(None, None, s("c")), "c");
        assert_eq!(resolve_project(None, None, None), "sketchbook");

        let active = ActiveProjects::new();
        active.set("s_1", "harbor");
        active.set("s_2", "sketchbook");
        assert_eq!(active.get("s_1").as_deref(), Some("harbor"));
        active.forget("s_1");
        assert_eq!(active.get("s_1"), None);
        assert_eq!(active.get("s_2").as_deref(), Some("sketchbook"));
    }
}
