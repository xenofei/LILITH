//! The app's own state (P4a-R16): `<local dir>\apps\<app>\state.json`,
//! `{"state": 1, "workspaces": {"<workspace id>": {"last_seen_at": "<RFC 3339>"}}}`, kept per
//! app and per machine, never in the core.
//!
//! A file that doesn't parse is set aside as `state.json.bad-<YYYYMMDDTHHMMSSZ>` and the app
//! starts with no state; writes go through `replace_file` (P4a-R16).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use lil_platform::Env;
use serde::{Deserialize, Serialize};

use crate::layout_file::{set_aside, write_replacing};

/// The `state` version this build reads and writes.
pub const STATE_VERSION: u64 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StateFile {
    state: u64,
    workspaces: BTreeMap<String, Seen>,
}

/// One app's state, read from and written to its `state.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    path: PathBuf,
    workspaces: BTreeMap<String, Seen>,
}

impl AppState {
    /// `env`'s state of `app` (`Env::app_state_dir`). A missing file is no state; one that
    /// doesn't parse is set aside first.
    pub fn load(env: &Env, app: &str) -> AppState {
        AppState::load_from(&env.app_state_dir(app).join("state.json"))
    }

    fn load_from(path: &Path) -> AppState {
        let workspaces = match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<StateFile>(&text) {
                Ok(file) if file.state == STATE_VERSION => file.workspaces,
                Ok(_) | Err(_) => {
                    if let Err(e) = set_aside(path, &utc_now()) {
                        tracing::warn!(error = %e, "the app state couldn't be set aside");
                    }
                    BTreeMap::new()
                }
            },
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, "the app state can't be read");
                }
                BTreeMap::new()
            }
        };
        AppState {
            path: path.to_path_buf(),
            workspaces,
        }
    }

    /// When workspace `ws` was last seen in this app (RFC 3339), if ever.
    pub fn last_seen(&self, ws: &str) -> Option<String> {
        self.workspaces.get(ws).map(|s| s.last_seen_at.clone())
    }

    /// Records `at` (RFC 3339) as when `ws` was last seen, and writes the file.
    pub fn set_last_seen(&mut self, ws: &str, at: &str) -> io::Result<()> {
        self.workspaces.insert(
            ws.to_string(),
            Seen {
                last_seen_at: at.to_string(),
            },
        );
        let file = StateFile {
            state: STATE_VERSION,
            workspaces: self.workspaces.clone(),
        };
        let text = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        write_replacing(&self.path, &text)
    }
}

/// Now, as `YYYYMMDDTHHMMSSZ`.
pub(crate) fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    lil_schema::time::compact(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::{Channel, Roots};

    fn env(root: &Path) -> Env {
        let roots = Roots {
            roaming: root.join("roaming"),
            local: root.join("local"),
            user_profile: root.join("profile"),
        };
        Env::with_roots(
            "test-appstate".parse::<Channel>().unwrap(),
            roots,
            "S-1-5-21-1-2-3-1001".to_string(),
        )
    }

    #[test] // spec: APP-111
    fn app_state_round_trips_last_seen_per_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let mut state = AppState::load(&env, "image");
        assert_eq!(state.last_seen("ws_a"), None);
        state.set_last_seen("ws_a", "2026-10-27T10:00:00Z").unwrap();
        state.set_last_seen("ws_b", "2026-10-26T09:00:00Z").unwrap();
        state.set_last_seen("ws_a", "2026-10-27T11:30:00Z").unwrap();

        let path = env.app_state_dir("image").join("state.json");
        let text: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            text,
            serde_json::json!({"state": 1, "workspaces": {
                "ws_a": {"last_seen_at": "2026-10-27T11:30:00Z"},
                "ws_b": {"last_seen_at": "2026-10-26T09:00:00Z"}}})
        );

        let again = AppState::load(&env, "image");
        assert_eq!(
            again.last_seen("ws_a").as_deref(),
            Some("2026-10-27T11:30:00Z")
        );
        assert_eq!(
            again.last_seen("ws_b").as_deref(),
            Some("2026-10-26T09:00:00Z")
        );
        assert_eq!(again, state);
        // Each app keeps its own.
        assert_eq!(AppState::load(&env, "other").last_seen("ws_a"), None);
    }

    #[test] // spec: APP-111
    fn a_bad_app_state_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let app_dir = env.app_state_dir("image");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("state.json"), "{nope").unwrap();
        let state = AppState::load(&env, "image");
        assert_eq!(state.last_seen("ws_a"), None);
        let names: Vec<String> = std::fs::read_dir(&app_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names[0].starts_with("state.json.bad-"), "{names:?}");
    }
}
