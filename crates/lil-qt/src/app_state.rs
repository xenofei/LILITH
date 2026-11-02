//! The app's own state (P4a-R16): `<local dir>\apps\<app>\state.json`,
//! `{"state": 1, "workspaces": {"<workspace id>": {"last_seen_at": "<RFC 3339>"}}}`, kept per
//! app and per machine, never in the core.
//!
//! A file that doesn't parse is set aside as `state.json.bad-<YYYYMMDDTHHMMSSZ>` and the app
//! starts with no state; writes go through `replace_file` (P4a-R16), but for the first start's
//! record, which is made only if there is no file and never replaces one.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use lil_platform::Env;
use serde::{Deserialize, Serialize};

use crate::layout_file::{retry_shared, set_aside, write_replacing};

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
    /// The file is there and couldn't be read: writing now would drop what it holds.
    unread: bool,
}

impl AppState {
    /// `env`'s state of `app` (`Env::app_state_dir`). A missing file is no state; one that
    /// doesn't parse is set aside first.
    pub fn load(env: &Env, app: &str) -> AppState {
        AppState::load_from(&env.app_state_dir(app).join("state.json"))
    }

    /// The state in the app's state folder `dir` (what QML is handed, `ShellState`'s `dir`).
    pub fn load_in(dir: &Path) -> AppState {
        AppState::load_from(&dir.join("state.json"))
    }

    fn load_from(path: &Path) -> AppState {
        let mut unread = false;
        // A program holding the file for a moment is waited for (SHARE_WAIT), so a brief hold doesn't
        // make it unread, which refuses Home's write.
        let workspaces = match retry_shared(|| std::fs::read_to_string(path)) {
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
                    unread = true;
                }
                BTreeMap::new()
            }
        };
        AppState {
            path: path.to_path_buf(),
            workspaces,
            unread,
        }
    }

    /// When workspace `ws` was last seen in this app (RFC 3339), if ever.
    pub fn last_seen(&self, ws: &str) -> Option<String> {
        self.workspaces.get(ws).map(|s| s.last_seen_at.clone())
    }

    /// Records `at` (RFC 3339) as when `ws` was last seen, and writes the file.
    ///
    /// # Errors
    /// Why it wasn't written, among them that the file couldn't be read when this was loaded: the
    /// write would replace the other workspaces' times with nothing.
    pub fn set_last_seen(&mut self, ws: &str, at: &str) -> io::Result<()> {
        if self.unread {
            return Err(io::Error::other(
                "the app state couldn't be read, so it isn't written over",
            ));
        }
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

/// Whether this start is the app's first on its channel, `dir` being its state folder
/// (`Env::app_state_dir`): it only reads. A `state.json` that was set aside, or a folder that can't be
/// read, isn't a first start (Appendix G, app state).
pub fn is_first_start(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(entries) => !entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("state.json")
        }),
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}

/// The record that the first start's offer has been made or wasn't needed: `state.json` with no
/// workspaces, unless there is one by now. It never replaces a file: the app's other writer (Home's
/// last seen time) may have got there first.
pub fn record_first_start(dir: &Path) -> io::Result<()> {
    let file = StateFile {
        state: STATE_VERSION,
        workspaces: BTreeMap::new(),
    };
    let text = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
    std::fs::create_dir_all(dir)?;
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("state.json"));
    match created {
        Ok(mut new) => new.write_all(&text),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
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

    #[test] // spec: P4a-R16
    fn a_read_error_never_writes_over_the_other_workspaces() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let mut state = AppState::load(&env, "image");
        state.set_last_seen("ws_a", "2026-10-27T10:00:00Z").unwrap();
        state.set_last_seen("ws_b", "2026-10-26T09:00:00Z").unwrap();
        let path = env.app_state_dir("image").join("state.json");
        // Another program holds the file for a moment: no sharing, so a read fails.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let mut unread = AppState::load(&env, "image");
        drop(held);
        assert_eq!(unread.last_seen("ws_b"), None);
        // The write is refused, not made with only this workspace in it.
        assert!(
            unread
                .set_last_seen("ws_a", "2026-10-28T10:00:00Z")
                .is_err()
        );
        let again = AppState::load(&env, "image");
        assert_eq!(
            again.last_seen("ws_a").as_deref(),
            Some("2026-10-27T10:00:00Z")
        );
        assert_eq!(
            again.last_seen("ws_b").as_deref(),
            Some("2026-10-26T09:00:00Z")
        );
    }

    /// Holds `path` open from another thread, with `share` (FILE_SHARE_* bits), for `ms`, as a person's
    /// tool, a backup program or an antivirus can; returns once it is held.
    fn hold(path: &Path, share: u32, ms: u64) -> std::thread::JoinHandle<()> {
        use std::os::windows::fs::OpenOptionsExt;
        let (held, path) = (std::sync::mpsc::channel(), path.to_path_buf());
        let worker = std::thread::spawn(move || {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(share)
                .open(&path)
                .unwrap();
            held.0.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(ms));
            drop(file);
        });
        held.1.recv().unwrap();
        worker
    }

    /// A reader that doesn't share deletion (PowerShell's Get-Content) holds the file for a moment:
    /// the write waits for it rather than failing (P4a-R16).
    #[test] // spec: P4a-R16
    fn a_brief_hold_doesnt_fail_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let mut state = AppState::load(&env, "image");
        state.set_last_seen("ws_a", "2026-10-27T10:00:00Z").unwrap();
        let path = env.app_state_dir("image").join("state.json");
        // FILE_SHARE_READ | FILE_SHARE_WRITE, no FILE_SHARE_DELETE.
        let holder = hold(&path, 0x1 | 0x2, 200);
        let mut again = AppState::load(&env, "image");
        again.set_last_seen("ws_a", "2026-10-28T10:00:00Z").unwrap();
        holder.join().unwrap();
        assert_eq!(
            AppState::load(&env, "image").last_seen("ws_a").as_deref(),
            Some("2026-10-28T10:00:00Z")
        );
    }

    /// One that shares nothing, for a moment: the read waits for it, so the state isn't taken for
    /// unreadable and Home's time isn't refused.
    #[test] // spec: P4a-R16
    fn a_brief_exclusive_hold_doesnt_fail_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let mut state = AppState::load(&env, "image");
        state.set_last_seen("ws_a", "2026-10-27T10:00:00Z").unwrap();
        let path = env.app_state_dir("image").join("state.json");
        let holder = hold(&path, 0, 200);
        let mut read = AppState::load(&env, "image");
        holder.join().unwrap();
        assert_eq!(
            read.last_seen("ws_a").as_deref(),
            Some("2026-10-27T10:00:00Z")
        );
        read.set_last_seen("ws_b", "2026-10-28T10:00:00Z").unwrap();
    }

    #[test] // spec: APP-022
    fn only_a_start_with_no_state_is_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let app_dir = env.app_state_dir("image");
        assert!(is_first_start(&app_dir), "no folder yet");
        assert!(!app_dir.exists(), "asking writes nothing");
        record_first_start(&app_dir).unwrap();
        assert!(!is_first_start(&app_dir), "recorded");
        // The record is a state like any other, and takes what comes later.
        let mut state = AppState::load(&env, "image");
        assert_eq!(state.last_seen("ws_a"), None);
        assert_eq!(
            std::fs::read_dir(&app_dir).unwrap().count(),
            1,
            "nothing was set aside"
        );
        state.set_last_seen("ws_a", "2026-10-27T10:00:00Z").unwrap();
        record_first_start(&app_dir).unwrap();
        assert_eq!(
            AppState::load(&env, "image").last_seen("ws_a").as_deref(),
            Some("2026-10-27T10:00:00Z"),
            "a record never writes over a state"
        );

        // A state set aside is an app that has run before.
        let other = env.app_state_dir("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("state.json"), "{nope").unwrap();
        let _ = AppState::load(&env, "other");
        assert!(!other.join("state.json").exists());
        assert!(!is_first_start(&other), "set aside, not new");

        // Another file in the folder (the layout) says nothing of it.
        let third = env.app_state_dir("third");
        std::fs::create_dir_all(&third).unwrap();
        std::fs::write(third.join("layout.json"), "{}").unwrap();
        assert!(is_first_start(&third));
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
