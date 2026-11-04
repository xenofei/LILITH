//! `ShellState`, a QML singleton of module `Lilith.Core`: the shell's layout file and shortcut
//! map for QML (APP-002, APP-003, APP-011; P4a-R16). Every call names the folder it works in
//! (`dir`): the app's state folder, which `main.cpp` hands the shell (`app_state_dir`), or a
//! test's own temporary folder, so a QML test never touches real state.
//!
//! - `loadLayout(dir)`: `{"status": "ok", "layout": {...}}`, `{"status": "missing"}` (no
//!   `layout.json`; its `.bak` is only `replace_file`'s backup and is never read), for a file
//!   that doesn't parse `{"status": "bad", "setAside": "<new file name>", "why"}` (the file is set
//!   aside first), or for one that can't be read (another program holds it, say)
//!   `{"status": "unreadable", "why"}`: the file is left alone and the shell doesn't save over it.
//! - `setLayoutAside(dir)`: sets `layout.json` aside (KDDockWidgets refused its blob); the new
//!   file name, or empty when it couldn't.
//! - `saveLayout(dir, layoutJson)`: writes `layout.json` through `replace_file`; empty, or why it
//!   couldn't.
//! - `clamp(windowJson, savedScreen, screensJson, marginsJson)`: `clamp_client` over JSON (the
//!   window's client area and its frame margins).
//! - `shortcuts()`: the plan 4a map as `[{"keys", "action", "scope"}]`.
//! - `lastSeen(dir, workspace)`: when Home was last seen in that workspace (RFC 3339), empty when
//!   never or when what is kept doesn't read; `setLastSeen(dir, workspace, at)`: writes it through
//!   `AppState`; empty, or why it couldn't (P4a-R16).
//! - `stateUnreadable(dir)`: whether that state file is there and couldn't be read, so `lastSeen`'s
//!   empty answer may be a lost time rather than a first run ([`AppState::unread`]).
//! - `timeMs(text)`: [`crate::format::parse_utc_ms`], NaN when it doesn't read.
//! - `awayItems(pagesJson, pendingJson, lastSeen)`: [`crate::away::away_items_json`] as JSON;
//!   `nextCursor(resultJson)`: a `jobs.list` result's `next_cursor`, empty on the last page, so
//!   QML needn't parse the page.
//! - `isFirstStart(dir)`: whether this is the app's first start in `dir`, read only
//!   ([`crate::app_state::is_first_start`]); false with no `dir`.
//! - `recordFirstStart(dir)`: from now on it isn't ([`crate::app_state::record_first_start`]): never
//!   replaces a file. A record that can't be created is logged, and the next start offers again; one
//!   created but cut short leaves a `state.json`, so it counts as a start that has happened.

use core::pin::Pin;
use std::path::{Path, PathBuf};

use cxx_qt_lib::QString;
use serde_json::json;

use crate::app_state::AppState;
use crate::away::away_items_json;
use crate::format::parse_utc_ms;
use crate::layout_file::{LayoutError, LayoutFile, Margins, Rect, Screen, clamp_client, set_aside};
use crate::shortcuts::ShortcutMap;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        type ShellState = super::ShellStateRust;

        #[qinvokable]
        fn load_layout(self: Pin<&mut ShellState>, dir: &QString) -> QString;

        #[qinvokable]
        fn set_layout_aside(self: Pin<&mut ShellState>, dir: &QString) -> QString;

        #[qinvokable]
        fn save_layout(self: Pin<&mut ShellState>, dir: &QString, layout_json: &QString)
        -> QString;

        #[qinvokable]
        fn clamp(
            self: &ShellState,
            window_json: &QString,
            saved_screen: &QString,
            screens_json: &QString,
            margins_json: &QString,
        ) -> QString;

        #[qinvokable]
        fn shortcuts(self: &ShellState) -> QString;

        #[qinvokable]
        fn last_seen(self: Pin<&mut ShellState>, dir: &QString, workspace: &QString) -> QString;

        #[qinvokable]
        fn set_last_seen(
            self: Pin<&mut ShellState>,
            dir: &QString,
            workspace: &QString,
            at: &QString,
        ) -> QString;

        #[qinvokable]
        fn state_unreadable(self: &ShellState, dir: &QString) -> bool;

        #[qinvokable]
        fn time_ms(self: &ShellState, text: &QString) -> f64;

        #[qinvokable]
        fn next_cursor(self: &ShellState, result_json: &QString) -> QString;

        #[qinvokable]
        fn away_items(
            self: &ShellState,
            pages_json: &QString,
            pending_json: &QString,
            last_seen: &QString,
        ) -> QString;

        #[qinvokable]
        fn is_first_start(self: &ShellState, dir: &QString) -> bool;

        #[qinvokable]
        fn record_first_start(self: &ShellState, dir: &QString);
    }

    #[namespace = "lil_qt"]
    extern "Rust" {
        /// The app's state folder, `<local dir>\apps\image` (P4a-R16); empty when this process
        /// can't tell its LILITH folders.
        fn app_state_dir() -> String;
    }
}

#[derive(Default)]
pub struct ShellStateRust;

fn layout_path(dir: &QString) -> Option<PathBuf> {
    let dir = dir.to_string();
    (!dir.is_empty()).then(|| Path::new(&dir).join("layout.json"))
}

fn qs(value: serde_json::Value) -> QString {
    QString::from(&value.to_string())
}

impl qobject::ShellState {
    fn load_layout(self: Pin<&mut Self>, dir: &QString) -> QString {
        let Some(path) = layout_path(dir) else {
            return qs(json!({"status": "missing"}));
        };
        match LayoutFile::load(&path) {
            Ok(layout) => qs(json!({"status": "ok", "layout": layout})),
            Err(LayoutError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                qs(json!({"status": "missing"}))
            }
            Err(LayoutError::Io(e)) => {
                tracing::warn!(error = %e, "the layout file can't be read: the Default layout opens, and the file is kept");
                qs(json!({"status": "unreadable", "why": e.to_string()}))
            }
            Err(e) => {
                let why = e.to_string();
                tracing::warn!(error = %why, "the layout file can't be used: the Default layout opens");
                let moved = set_aside(&path, &crate::app_state::utc_now());
                let name = match moved {
                    Ok(to) => to
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    Err(e) => {
                        tracing::warn!(error = %e, "the layout file couldn't be set aside");
                        String::new()
                    }
                };
                qs(json!({"status": "bad", "setAside": name, "why": why}))
            }
        }
    }

    fn set_layout_aside(self: Pin<&mut Self>, dir: &QString) -> QString {
        let Some(path) = layout_path(dir) else {
            return QString::default();
        };
        match set_aside(&path, &crate::app_state::utc_now()) {
            Ok(to) => QString::from(
                &to.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            Err(e) => {
                tracing::warn!(error = %e, "the layout file couldn't be set aside");
                QString::default()
            }
        }
    }

    fn save_layout(self: Pin<&mut Self>, dir: &QString, layout_json: &QString) -> QString {
        let Some(path) = layout_path(dir) else {
            return QString::from("no state folder");
        };
        let saved = LayoutFile::parse(&layout_json.to_string())
            .map_err(|e| e.to_string())
            .and_then(|layout| layout.save(&path).map_err(|e| e.to_string()));
        match saved {
            Ok(()) => QString::default(),
            Err(e) => {
                tracing::warn!(error = %e, "the layout wasn't saved");
                QString::from(&e)
            }
        }
    }

    fn clamp(
        &self,
        window_json: &QString,
        saved_screen: &QString,
        screens_json: &QString,
        margins_json: &QString,
    ) -> QString {
        let window: Result<Rect, _> = serde_json::from_str(&window_json.to_string());
        let screens: Result<Vec<Screen>, _> = serde_json::from_str(&screens_json.to_string());
        let margins: Margins = serde_json::from_str(&margins_json.to_string()).unwrap_or_default();
        match (window, screens) {
            (Ok(window), Ok(screens)) => qs(json!(clamp_client(
                window,
                margins,
                &saved_screen.to_string(),
                &screens
            ))),
            _ => qs(json!(null)),
        }
    }

    fn shortcuts(&self) -> QString {
        qs(json!(ShortcutMap::plan_4a().bindings()))
    }

    fn last_seen(self: Pin<&mut Self>, dir: &QString, workspace: &QString) -> QString {
        let dir = dir.to_string();
        if dir.is_empty() {
            return QString::default();
        }
        AppState::load_in(Path::new(&dir))
            .last_seen(&workspace.to_string())
            .filter(|at| parse_utc_ms(at).is_some())
            .map_or_else(QString::default, |at| QString::from(&at))
    }

    fn set_last_seen(
        self: Pin<&mut Self>,
        dir: &QString,
        workspace: &QString,
        at: &QString,
    ) -> QString {
        let dir = dir.to_string();
        if dir.is_empty() {
            return QString::from("no state folder");
        }
        let mut state = AppState::load_in(Path::new(&dir));
        match state.set_last_seen(&workspace.to_string(), &at.to_string()) {
            Ok(()) => QString::default(),
            Err(e) => {
                tracing::warn!(error = %e, "when Home was last seen wasn't saved");
                QString::from(&e.to_string())
            }
        }
    }

    fn state_unreadable(&self, dir: &QString) -> bool {
        let dir = dir.to_string();
        !dir.is_empty() && AppState::load_in(Path::new(&dir)).unread()
    }

    fn time_ms(&self, text: &QString) -> f64 {
        parse_utc_ms(&text.to_string()).map_or(f64::NAN, |ms| ms as f64)
    }

    fn next_cursor(&self, result_json: &QString) -> QString {
        let result: serde_json::Value =
            serde_json::from_str(&result_json.to_string()).unwrap_or_default();
        result["structured"]["next_cursor"]
            .as_str()
            .map_or_else(QString::default, QString::from)
    }

    fn away_items(
        &self,
        pages_json: &QString,
        pending_json: &QString,
        last_seen: &QString,
    ) -> QString {
        qs(away_items_json(
            &pages_json.to_string(),
            &pending_json.to_string(),
            &last_seen.to_string(),
        ))
    }

    fn is_first_start(&self, dir: &QString) -> bool {
        let dir = dir.to_string();
        !dir.is_empty() && crate::app_state::is_first_start(Path::new(&dir))
    }

    fn record_first_start(&self, dir: &QString) {
        let dir = dir.to_string();
        if dir.is_empty() {
            return;
        }
        if let Err(e) = crate::app_state::record_first_start(Path::new(&dir)) {
            tracing::warn!(kind = ?e.kind(), "the app's first start couldn't be recorded");
        }
    }
}

fn app_state_dir() -> String {
    match lil_platform::Env::from_process() {
        Ok(env) => env.app_state_dir(crate::APP).display().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "no LILITH folders: the layout isn't kept");
            String::new()
        }
    }
}
