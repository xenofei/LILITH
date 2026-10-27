//! The app's layout file and the window clamp (APP-002, APP-003, APP-004; P4a-R16).
//!
//! `<local dir>\apps\<app>\layout.json` holds the last state of the app's windows:
//! `{"layout": 1, "windows": [{"name", "geometry": {"x", "y", "w", "h"}, "maximized",
//! "screen": {"name", "scale"}}], "kddw": <KDDockWidgets' saved layout>}`. `windows` lists the
//! main windows (each one's normal geometry, in Qt's device-independent pixels, and the screen
//! it was on); `kddw` is the blob KDDockWidgets' `LayoutSaver` writes, kept as JSON. M1 has the
//! Default layout and this last state, nothing else (APP-004).
//!
//! A file that doesn't parse, or whose blob KDDockWidgets refuses, is renamed to
//! `<name>.bad-<YYYYMMDDTHHMMSSZ>` ([`set_aside`]) and the app opens the Default layout. Every
//! write goes through `lil_platform::fs::replace_file` (P4a-R16), with the old file kept as
//! `<name>.bak`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The `layout` version this build reads and writes.
pub const LAYOUT_VERSION: u64 = 1;

/// A rectangle in Qt's device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A screen as the app sees it now: its name, its available area (without the taskbar) and its
/// scale (1.5 at 150 %).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    pub name: String,
    pub available: Rect,
    pub scale: f64,
}

/// `window`, moved and shrunk so it lies inside one screen's available area (APP-003): the screen
/// named `saved_screen` when it is still there, else the primary, `screens[0]`. A window larger
/// than the area shrinks to it; one that doesn't overlap the area at all (its screen was
/// unplugged, or the screens moved) is centred in it; any other is moved just far enough to fit.
/// With no screens, `window` is returned as it is.
pub fn clamp_to_screens(window: Rect, saved_screen: &str, screens: &[Screen]) -> Rect {
    let Some(screen) = screens
        .iter()
        .find(|s| s.name == saved_screen)
        .or_else(|| screens.first())
    else {
        return window;
    };
    let area = screen.available;
    let w = window.w.clamp(1, area.w.max(1));
    let h = window.h.clamp(1, area.h.max(1));
    let overlaps = window.x < area.x + area.w
        && area.x < window.x + window.w
        && window.y < area.y + area.h
        && area.y < window.y + window.h;
    let (x, y) = if overlaps {
        (
            window.x.clamp(area.x, area.x + area.w - w),
            window.y.clamp(area.y, area.y + area.h - h),
        )
    } else {
        (area.x + (area.w - w) / 2, area.y + (area.h - h) / 2)
    };
    Rect { x, y, w, h }
}

/// The screen a window was on when the layout was saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedScreen {
    pub name: String,
    pub scale: f64,
}

/// One main window of the layout (APP-002).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub name: String,
    /// The normal (not maximized) geometry.
    pub geometry: Rect,
    pub maximized: bool,
    pub screen: SavedScreen,
}

/// `layout.json` (APP-002).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutFile {
    pub layout: u64,
    pub windows: Vec<SavedWindow>,
    /// KDDockWidgets' `LayoutSaver` blob, as JSON.
    pub kddw: serde_json::Value,
}

/// Why a layout file can't be used.
#[derive(Debug)]
pub enum LayoutError {
    /// It can't be read (`NotFound` when there is none yet).
    Io(io::Error),
    /// It isn't a layout file: not JSON, empty, a field missing or of the wrong kind.
    Parse(String),
    /// A `layout` version this build doesn't know (a newer build wrote it).
    Version(u64),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::Io(e) => write!(f, "the layout file can't be read: {e}"),
            LayoutError::Parse(e) => write!(f, "the layout file doesn't parse: {e}"),
            LayoutError::Version(v) => write!(f, "the layout file is version {v}, not 1"),
        }
    }
}

impl std::error::Error for LayoutError {}

impl LayoutFile {
    /// Reads `path`. `kddw` must be a JSON object; KDDockWidgets judges the rest of it at
    /// restore.
    pub fn load(path: &Path) -> Result<LayoutFile, LayoutError> {
        let text = std::fs::read_to_string(path).map_err(LayoutError::Io)?;
        LayoutFile::parse(&text)
    }

    /// Parses a layout file's text (see [`LayoutFile::load`]).
    pub fn parse(text: &str) -> Result<LayoutFile, LayoutError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| LayoutError::Parse(e.to_string()))?;
        match value.get("layout").and_then(serde_json::Value::as_u64) {
            Some(LAYOUT_VERSION) => {}
            Some(other) => return Err(LayoutError::Version(other)),
            None => return Err(LayoutError::Parse("no `layout` version".to_string())),
        }
        let file: LayoutFile =
            serde_json::from_value(value).map_err(|e| LayoutError::Parse(e.to_string()))?;
        if !file.kddw.is_object() {
            return Err(LayoutError::Parse("`kddw` isn't an object".to_string()));
        }
        Ok(file)
    }

    /// Writes the file to `path` through `replace_file` (P4a-R16), making its folder if needed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_replacing(path, &text)
    }
}

/// Writes `bytes` to `path` without ever leaving it half-written: into `.<name>.lil-tmp` beside
/// it, flushed, then swapped in with `replace_file` (the old file kept as `<name>.bak`), or
/// moved into place when there is no file yet.
pub(crate) fn write_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path has no folder or file name",
        ));
    };
    std::fs::create_dir_all(dir)?;
    let name = name.to_string_lossy();
    let temp = dir.join(format!(".{name}.lil-tmp"));
    let backup = dir.join(format!("{name}.bak"));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        lil_platform::fs::flush(&file)?;
    }
    let swapped = if path.try_exists()? {
        lil_platform::fs::replace_file(path, &temp, &backup)
    } else {
        lil_platform::fs::move_new(&temp, path)
    };
    if swapped.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    swapped
}

/// Renames `path` to `<name>.bad-<utc>` in its folder (`utc` as `YYYYMMDDTHHMMSSZ`) and returns
/// the new path. A name already taken gets `-2`, `-3`, ... after it; nothing is replaced.
pub fn set_aside(path: &Path, utc: &str) -> io::Result<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path has no folder or file name",
        ));
    };
    let name = name.to_string_lossy();
    for n in 1..=100 {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let to = dir.join(format!("{name}.bad-{utc}{suffix}"));
        if to.try_exists()? {
            continue;
        }
        lil_platform::fs::move_new(path, &to)?;
        return Ok(to);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many set-aside copies with this time",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    fn screen(name: &str, available: Rect, scale: f64) -> Screen {
        Screen {
            name: name.to_string(),
            available,
            scale,
        }
    }

    fn inside(r: Rect, area: Rect) -> bool {
        r.x >= area.x
            && r.y >= area.y
            && r.x + r.w <= area.x + area.w
            && r.y + r.h <= area.y + area.h
    }

    #[test] // spec: APP-003
    fn layout_clamps_windows_to_the_available_area() {
        let primary = rect(0, 0, 1920, 1040);
        let right = rect(1920, 0, 2560, 1400);
        let two = [
            screen(r"\\.\DISPLAY1", primary, 1.0),
            screen(r"\\.\DISPLAY2", right, 1.5),
        ];
        let one = [screen(r"\\.\DISPLAY1", primary, 1.0)];

        // A window on an unplugged screen moves onto the primary's available area.
        let on_gone = rect(2200, 100, 1280, 800);
        let r = clamp_to_screens(on_gone, r"\\.\DISPLAY2", &one);
        assert!(inside(r, primary), "{r:?}");
        assert_eq!((r.w, r.h), (1280, 800), "it keeps its size");

        // A layout saved at 150 % restored on a 100 % screen stays inside the area.
        let saved_at_150 = rect(150, 90, 1800, 1100);
        let at_100 = [screen(r"\\.\DISPLAY1", primary, 1.0)];
        let r = clamp_to_screens(saved_at_150, r"\\.\DISPLAY1", &at_100);
        assert!(inside(r, primary), "{r:?}");

        // A window larger than its area shrinks to it.
        let r = clamp_to_screens(rect(-50, -50, 4000, 3000), r"\\.\DISPLAY1", &one);
        assert_eq!(r, primary);

        // A maximized window's normal geometry restores onto its own screen, not the primary,
        // even when it reaches over the edge.
        let r = clamp_to_screens(rect(1800, 50, 1600, 900), r"\\.\DISPLAY2", &two);
        assert!(inside(r, right), "{r:?}");

        // A window that fits is left where it is.
        let fits = rect(100, 100, 800, 600);
        assert_eq!(clamp_to_screens(fits, r"\\.\DISPLAY1", &two), fits);

        // No screens: unchanged.
        assert_eq!(clamp_to_screens(fits, "x", &[]), fits);
    }

    fn sample() -> LayoutFile {
        LayoutFile {
            layout: LAYOUT_VERSION,
            windows: vec![SavedWindow {
                name: "image-main".to_string(),
                geometry: rect(10, 20, 1280, 800),
                maximized: true,
                screen: SavedScreen {
                    name: r"\\.\DISPLAY1".to_string(),
                    scale: 1.25,
                },
            }],
            kddw: serde_json::json!({"serializationVersion": 3, "mainWindows": []}),
        }
    }

    #[test] // spec: APP-002
    fn layout_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apps").join("image").join("layout.json");
        let mut file = sample();
        file.save(&path).unwrap();
        assert_eq!(LayoutFile::load(&path).unwrap(), file);

        // A second save replaces the first, keeping it as the backup.
        file.windows[0].maximized = false;
        file.save(&path).unwrap();
        assert_eq!(LayoutFile::load(&path).unwrap(), file);
        assert!(path.with_file_name("layout.json.bak").exists());
        assert!(!path.with_file_name(".layout.json.lil-tmp").exists());

        let text: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(text["layout"], 1);
        assert_eq!(text["windows"][0]["geometry"]["w"], 1280);
        assert_eq!(text["windows"][0]["screen"]["scale"], 1.25);
    }

    #[test] // spec: APP-002
    fn a_bad_layout_file_is_set_aside() {
        for bad in [
            "",
            "{not json",
            r#"{"layout": 2, "windows": [], "kddw": {}}"#,
            r#"{"windows": [], "kddw": {}}"#,
            r#"{"layout": 1, "windows": [], "kddw": "blob"}"#,
            r#"{"layout": 1, "windows": [{"name": "m"}], "kddw": {}}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("layout.json");
            std::fs::write(&path, bad).unwrap();
            assert!(
                matches!(
                    LayoutFile::load(&path),
                    Err(LayoutError::Parse(_) | LayoutError::Version(_))
                ),
                "{bad:?}"
            );
            let moved = set_aside(&path, "20261027T101500Z").unwrap();
            assert_eq!(
                moved.file_name().unwrap(),
                "layout.json.bad-20261027T101500Z"
            );
            assert!(!path.exists());
            assert_eq!(std::fs::read_to_string(&moved).unwrap(), bad);

            // A second one in the same second keeps both.
            std::fs::write(&path, bad).unwrap();
            let again = set_aside(&path, "20261027T101500Z").unwrap();
            assert_eq!(
                again.file_name().unwrap(),
                "layout.json.bad-20261027T101500Z-2"
            );
            assert!(moved.exists());
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            LayoutFile::load(&dir.path().join("layout.json")),
            Err(LayoutError::Io(e)) if e.kind() == io::ErrorKind::NotFound
        ));
    }
}
