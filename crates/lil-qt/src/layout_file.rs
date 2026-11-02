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

use lil_platform::fs::RetryClass;
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

/// The smallest window (its frame) the clamp leaves, unless the screen's area is smaller: a
/// parseable file with a size of 0 doesn't restore a 1 x 1 window.
pub const MIN_WINDOW_W: i32 = 640;
pub const MIN_WINDOW_H: i32 = 400;

/// A window frame's margins around its client area: the title bar and the borders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Margins {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

fn to_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// `window` (a window's frame, title bar included), moved and shrunk so it lies inside one
/// screen's available area (APP-003): the screen named `saved_screen` when it is still there,
/// else the primary, `screens[0]`. A window larger than the area shrinks to it, and one smaller
/// than [`MIN_WINDOW_W`] x [`MIN_WINDOW_H`] grows to that (within the area); one that doesn't
/// overlap the area at all (its screen was unplugged, or the screens moved) is centred in it; any
/// other is moved just far enough to fit. With no screens, `window` is returned as it is. The
/// arithmetic is in i64, so no saved value overflows it.
pub fn clamp_to_screens(window: Rect, saved_screen: &str, screens: &[Screen]) -> Rect {
    let Some(screen) = screens
        .iter()
        .find(|s| s.name == saved_screen)
        .or_else(|| screens.first())
    else {
        return window;
    };
    let (ax, ay) = (i64::from(screen.available.x), i64::from(screen.available.y));
    let aw = i64::from(screen.available.w.max(1));
    let ah = i64::from(screen.available.h.max(1));
    let (wx, wy) = (i64::from(window.x), i64::from(window.y));
    let w = i64::from(window.w).max(i64::from(MIN_WINDOW_W)).min(aw);
    let h = i64::from(window.h).max(i64::from(MIN_WINDOW_H)).min(ah);
    let overlaps = wx < ax + aw
        && ax < wx + i64::from(window.w)
        && wy < ay + ah
        && ay < wy + i64::from(window.h);
    let (x, y) = if overlaps {
        (wx.clamp(ax, ax + aw - w), wy.clamp(ay, ay + ah - h))
    } else {
        (ax + (aw - w) / 2, ay + (ah - h) / 2)
    };
    Rect {
        x: to_i32(x),
        y: to_i32(y),
        w: to_i32(w),
        h: to_i32(h),
    }
}

/// [`clamp_to_screens`] for a window known by its client area: its frame (`client` grown by
/// `margins`) is clamped, so the title bar stays on the screen, and the client area inside the
/// clamped frame is returned.
pub fn clamp_client(
    client: Rect,
    margins: Margins,
    saved_screen: &str,
    screens: &[Screen],
) -> Rect {
    let (l, t, r, b) = (
        i64::from(margins.left),
        i64::from(margins.top),
        i64::from(margins.right),
        i64::from(margins.bottom),
    );
    let frame = Rect {
        x: to_i32(i64::from(client.x) - l),
        y: to_i32(i64::from(client.y) - t),
        w: to_i32(i64::from(client.w) + l + r),
        h: to_i32(i64::from(client.h) + t + b),
    };
    let f = clamp_to_screens(frame, saved_screen, screens);
    Rect {
        x: to_i32(i64::from(f.x) + l),
        y: to_i32(i64::from(f.y) + t),
        w: to_i32(i64::from(f.w) - l - r),
        h: to_i32(i64::from(f.h) - t - b),
    }
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
    /// restore. A file that isn't UTF-8 is a [`LayoutError::Parse`]; any other read error stays
    /// [`LayoutError::Io`] (the file may be fine and only held by another program).
    pub fn load(path: &Path) -> Result<LayoutFile, LayoutError> {
        let text = std::fs::read_to_string(path).map_err(|e| match e.kind() {
            io::ErrorKind::InvalidData => LayoutError::Parse(e.to_string()),
            _ => LayoutError::Io(e),
        })?;
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
    write_replacing_with(path, bytes, |name| format!("{name}.bak"))
}

/// How long a read or a swap waits for another program that holds the file for a moment (a person's
/// tool, a backup program, an antivirus, a script reading it without sharing deletion), in all.
/// These run on the GUI thread too, so it stays short: a longer hold fails, and the caller's next
/// attempt tries again.
pub const SHARE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// `op`, again while it fails with a sharing violation (or a replace Windows couldn't finish for one),
/// from 10 ms doubling, for [`SHARE_WAIT`] in all; then its last error.
pub(crate) fn retry_shared<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let start = std::time::Instant::now();
    let mut pause = std::time::Duration::from_millis(10);
    loop {
        match op() {
            Err(e)
                if matches!(
                    lil_platform::fs::classify(&e),
                    RetryClass::SharingViolation | RetryClass::ReplaceRetry
                ) && start.elapsed() + pause <= SHARE_WAIT =>
            {
                std::thread::sleep(pause);
                pause = (pause * 2).min(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

/// How much [`write_replacing_stoppable`] writes between asking whether to stop.
const WRITE_CHUNK: usize = 1024 * 1024;

/// [`write_replacing`], keeping the old file under `backup(<name>)` beside it.
pub(crate) fn write_replacing_with(
    path: &Path,
    bytes: &[u8],
    backup: impl Fn(&str) -> String,
) -> io::Result<()> {
    write_replacing_stoppable(path, bytes, backup, &|| false)
}

/// [`write_replacing_with`], asking `stop` between chunks. Told to stop (`ErrorKind::Interrupted`), or
/// when the temporary file can't be written, it removes that file and leaves the old one as it was.
pub(crate) fn write_replacing_stoppable(
    path: &Path,
    bytes: &[u8],
    backup: impl Fn(&str) -> String,
    stop: &dyn Fn() -> bool,
) -> io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path has no folder or file name",
        ));
    };
    std::fs::create_dir_all(dir)?;
    let name = name.to_string_lossy();
    let temp = dir.join(format!(".{name}.lil-tmp"));
    let backup = dir.join(backup(&name));
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        for chunk in bytes.chunks(WRITE_CHUNK) {
            if stop() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "the app closed before the write ended",
                ));
            }
            file.write_all(chunk)?;
        }
        lil_platform::fs::flush(&file)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    let swapped = if path.try_exists()? {
        match retry_shared(|| lil_platform::fs::replace_file(path, &temp, &backup)) {
            // Error 1177: the old file is already under the backup name and the target name is
            // free, but the new file is still under its temp name: move it in (as lil-core's
            // ReplacementNotMoved step does) rather than lose it.
            Err(e) if lil_platform::fs::classify(&e) == RetryClass::ReplacementNotMoved => {
                lil_platform::fs::move_new(&temp, path)
            }
            other => other,
        }
    } else {
        lil_platform::fs::move_new(&temp, path)
    };
    if swapped.is_err() && temp.try_exists().unwrap_or(false) {
        let _ = std::fs::remove_file(&temp);
    }
    swapped
}

/// How many set-aside copies of a file are kept: the newest ones.
pub const KEEP_SET_ASIDE: usize = 5;

/// Renames `path` to `<name>.bad-<utc>` in its folder (`utc` as `YYYYMMDDTHHMMSSZ`) and returns
/// the new path. A name already taken gets `-2`, `-3`, ... after it; nothing is replaced. The
/// oldest copies beyond [`KEEP_SET_ASIDE`] are then removed, never the one just made.
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
        prune_set_aside(dir, &name, &to);
        return Ok(to);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many set-aside copies with this time",
    ))
}

/// The order of a set-aside copy by what follows `<name>.bad-`: its time, then the `-2`, `-3` of
/// copies made in the same second. `None` for any other name: that isn't one of ours.
fn set_aside_order(rest: &str) -> Option<(&str, u32)> {
    let (utc, n) = match rest.split_once('-') {
        Some((utc, n)) => {
            let digits = !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit());
            if !digits || n.starts_with('0') {
                return None;
            }
            (utc, n.parse().ok()?)
        }
        None => (rest, 1),
    };
    let b = utc.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let is_utc = b.len() == 16 && digits(0..8) && b[8] == b'T' && digits(9..15) && b[15] == b'Z';
    is_utc.then_some((utc, n))
}

/// Removes `<name>`'s set-aside copies beyond the newest [`KEEP_SET_ASIDE`], counting `made`, which
/// stays whatever its time says (the clock can have been set back). Only files named exactly as
/// [`set_aside`] names them count or go. A copy that can't be removed stays, and the log says so.
fn prune_set_aside(dir: &Path, name: &str, made: &Path) {
    let prefix = format!("{name}.bad-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut copies: Vec<(String, u32, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter(|entry| entry.path() != made)
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let (utc, n) = set_aside_order(file.strip_prefix(&prefix)?)?;
            Some((utc.to_string(), n, entry.path()))
        })
        .collect();
    copies.sort();
    let old = copies.len().saturating_sub(KEEP_SET_ASIDE - 1);
    for (_, _, path) in copies.drain(..old) {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(error = %e, "an old set-aside copy couldn't be removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-121
    fn a_write_told_to_stop_leaves_no_temporary_file_and_the_old_file_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kept.zip");
        std::fs::write(&path, b"old").unwrap();
        let backup = |name: &str| format!(".{name}.bak");
        let asked = std::cell::Cell::new(0);
        let stop = || {
            asked.set(asked.get() + 1);
            asked.get() > 1
        };
        let bytes = vec![7u8; 3 * WRITE_CHUNK];
        let e = write_replacing_stoppable(&path, &bytes, backup, &stop).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["kept.zip"], "nothing beside it");
        // Told nothing, it writes the whole file.
        write_replacing_stoppable(&path, &bytes, backup, &|| false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap().len(), bytes.len());
    }

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
        let left = rect(-1920, 0, 1920, 1040);
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

        // A layout saved at 100 % (a 1800 x 1000 window on a 1920 x 1040 area) restored when the
        // screen is at 150 %, whose area is then 1280 x 693 device-independent pixels: the window
        // shrinks into it.
        let at_150 = [screen(r"\\.\DISPLAY1", rect(0, 0, 1280, 693), 1.5)];
        let r = clamp_to_screens(rect(60, 20, 1800, 1000), r"\\.\DISPLAY1", &at_150);
        assert_eq!(r, rect(0, 0, 1280, 693));
        // And the other way: saved at 150 %, restored at 100 %, it fits and stays where it was.
        let r = clamp_to_screens(rect(40, 30, 1200, 650), r"\\.\DISPLAY1", &one);
        assert_eq!(r, rect(40, 30, 1200, 650));

        // A window larger than its area shrinks to it.
        let r = clamp_to_screens(rect(-50, -50, 4000, 3000), r"\\.\DISPLAY1", &one);
        assert_eq!(r, primary);

        // A maximized window's normal geometry restores onto its own screen, not the primary,
        // even when it reaches over the edge.
        let r = clamp_to_screens(rect(1800, 50, 1600, 900), r"\\.\DISPLAY2", &two);
        assert!(inside(r, right), "{r:?}");

        // A screen left of the primary, at negative coordinates, keeps its window there.
        let three = [
            screen(r"\\.\DISPLAY1", primary, 1.0),
            screen(r"\\.\DISPLAY3", left, 1.0),
        ];
        let r = clamp_to_screens(rect(-1800, 100, 1000, 700), r"\\.\DISPLAY3", &three);
        assert_eq!(r, rect(-1800, 100, 1000, 700));
        let r = clamp_to_screens(rect(-2500, -40, 1000, 700), r"\\.\DISPLAY3", &three);
        assert_eq!(r, rect(-1920, 0, 1000, 700));

        // One pixel of overlap counts as on the screen: it is moved in, not centred.
        let r = clamp_to_screens(rect(1919, 100, 800, 600), r"\\.\DISPLAY1", &one);
        assert_eq!(r, rect(1120, 100, 800, 600));

        // A window that fits is left where it is.
        let fits = rect(100, 100, 800, 600);
        assert_eq!(clamp_to_screens(fits, r"\\.\DISPLAY1", &two), fits);

        // A size of 0 grows to the minimum; values near i32's limits don't overflow.
        let r = clamp_to_screens(rect(10, 10, 0, 0), r"\\.\DISPLAY1", &one);
        assert_eq!((r.w, r.h), (MIN_WINDOW_W, MIN_WINDOW_H));
        let r = clamp_to_screens(rect(2_147_483_000, 10, 1280, 800), r"\\.\DISPLAY1", &one);
        assert!(inside(r, primary), "{r:?}");

        // No screens: unchanged.
        assert_eq!(clamp_to_screens(fits, "x", &[]), fits);
    }

    #[test] // spec: APP-003
    fn layout_clamps_the_frame_so_the_title_bar_stays_on_screen() {
        let laptop = [screen(r"\\.\DISPLAY1", rect(0, 0, 1366, 728), 1.0)];
        let margins = Margins {
            left: 8,
            top: 31,
            right: 8,
            bottom: 8,
        };
        // A 1280 x 800 client area is too tall: the frame shrinks to the area's height, and the
        // client area sits inside it, below the title bar.
        let client = clamp_client(rect(40, 0, 1280, 800), margins, r"\\.\DISPLAY1", &laptop);
        assert_eq!(client, rect(40, 31, 1280, 728 - 39));
        // A client area whose top is at the area's top moves down by the title bar.
        let client = clamp_client(rect(100, 0, 800, 600), margins, r"\\.\DISPLAY1", &laptop);
        assert_eq!(client, rect(100, 31, 800, 600));
        // No margins: the client area is the frame.
        let client = clamp_client(
            rect(100, 0, 800, 600),
            Margins::default(),
            r"\\.\DISPLAY1",
            &laptop,
        );
        assert_eq!(client, rect(100, 0, 800, 600));
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
        // Bytes that aren't UTF-8 are damage too, not a read error.
        let path = dir.path().join("binary.json");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x7b]).unwrap();
        assert!(matches!(
            LayoutFile::load(&path),
            Err(LayoutError::Parse(_))
        ));
        assert!(matches!(
            LayoutFile::load(&dir.path().join("layout.json")),
            Err(LayoutError::Io(e)) if e.kind() == io::ErrorKind::NotFound
        ));
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test] // spec: APP-002
    fn set_aside_counts_and_removes_only_its_own_copies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        // Names that only start alike: a person's files, and a folder.
        let others = [
            "layout.json.bad-",
            "layout.json.bad-0",
            "layout.json.bad-20261001T000000Z.keep",
            "layout.json.bad-20261001T000000Z-",
            "layout.json.bad-20261001T000000Z-x",
            "layout.json.bad-20261001T000000Z-0",
            "layout.json.bad-2026100T000000Z",
            "layout.json.bad-20261001X000000Z",
            "layout.json.bad-my-notes.txt",
            "layout.json.bad-z1",
            "layout.json.bad-z2",
            "layout.json.bad-z3",
            "layout.json.bad-z4",
            "layout.json.bad-z5",
        ];
        for name in others {
            std::fs::write(dir.path().join(name), "mine").unwrap();
        }
        let folder = "layout.json.bad-20260101T000000Z";
        std::fs::create_dir(dir.path().join(folder)).unwrap();
        let mut made = Vec::new();
        for utc in [
            "20261101T000000Z",
            "20261102T000000Z",
            "20261103T000000Z",
            "20261104T000000Z",
            "20261105T000000Z",
        ] {
            std::fs::write(&path, utc).unwrap();
            let moved = set_aside(&path, utc).unwrap();
            assert!(moved.exists(), "{moved:?}");
            made.push(format!("layout.json.bad-{utc}"));
        }
        // Five real copies: neither the folder nor the others count, and all of them stay.
        let mut want: Vec<String> = others.iter().map(|s| (*s).to_string()).collect();
        want.push(folder.to_string());
        want.extend(made);
        want.sort();
        assert_eq!(names_in(dir.path()), want);
        assert!(dir.path().join(folder).is_dir());
    }

    #[test] // spec: APP-002
    fn set_aside_never_removes_the_copy_it_just_made() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        for utc in [
            "20270101T000000Z",
            "20270102T000000Z",
            "20270103T000000Z",
            "20270104T000000Z",
            "20270105T000000Z",
        ] {
            std::fs::write(&path, utc).unwrap();
            set_aside(&path, utc).unwrap();
        }
        // The clock was set back: the new copy has the oldest name, and the banner names it.
        std::fs::write(&path, "now").unwrap();
        let moved = set_aside(&path, "20261031T000000Z").unwrap();
        assert!(moved.exists(), "{moved:?}");
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "now");
        assert_eq!(
            names_in(dir.path()),
            [
                "layout.json.bad-20261031T000000Z",
                "layout.json.bad-20270102T000000Z",
                "layout.json.bad-20270103T000000Z",
                "layout.json.bad-20270104T000000Z",
                "layout.json.bad-20270105T000000Z",
            ]
        );
    }

    #[test] // spec: APP-002
    fn set_aside_keeps_only_the_newest_copies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        // Another file's copies, and files that only look alike, are left alone.
        std::fs::write(dir.path().join("state.json.bad-20260101T000000Z"), "s").unwrap();
        std::fs::write(dir.path().join("layout.json.bak"), "b").unwrap();
        // Three in one second (the -10 sorts after the -2 by its number), then later ones.
        for utc in [
            "20261001T000000Z",
            "20261001T000000Z",
            "20261002T000000Z",
            "20261003T000000Z",
            "20261004T000000Z",
            "20261005T000000Z",
            "20261006T000000Z",
        ] {
            std::fs::write(&path, utc).unwrap();
            set_aside(&path, utc).unwrap();
        }
        std::fs::write(dir.path().join("layout.json.bad-20261001T000000Z-10"), "x").unwrap();
        std::fs::write(&path, "last").unwrap();
        set_aside(&path, "20261007T000000Z").unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "layout.json.bad-20261003T000000Z",
                "layout.json.bad-20261004T000000Z",
                "layout.json.bad-20261005T000000Z",
                "layout.json.bad-20261006T000000Z",
                "layout.json.bad-20261007T000000Z",
                "layout.json.bak",
                "state.json.bad-20260101T000000Z",
            ]
        );
    }
}
