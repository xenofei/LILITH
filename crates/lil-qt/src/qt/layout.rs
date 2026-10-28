//! `ShellState`, a QML singleton of module `Lilith.Core`: the shell's layout file and shortcut
//! map for QML (APP-002, APP-003, APP-011; P4a-R16). Every call names the folder it works in
//! (`dir`): the app's state folder, which `main.cpp` hands the shell (`app_state_dir`), or a
//! test's own temporary folder, so a QML test never touches real state.
//!
//! - `loadLayout(dir)`: `{"status": "ok", "layout": {...}}` (with `"fromBackup": true` when
//!   `layout.json` is missing and its `.bak` is read instead), `{"status": "missing"}`, for a file
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

use core::pin::Pin;
use std::path::{Path, PathBuf};

use cxx_qt_lib::QString;
use serde_json::json;

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
                // A save that stopped between the swap's two steps leaves only the backup.
                match LayoutFile::load(&path.with_file_name("layout.json.bak")) {
                    Ok(layout) => qs(json!({"status": "ok", "layout": layout, "fromBackup": true})),
                    Err(_) => qs(json!({"status": "missing"})),
                }
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
