//! The one log setup every process uses (ARCH-117, ARCH-119; P4a-R20, Appendix G
//! "Diagnostics"): `tracing` JSON lines to `<logs dir>\<process>.<YYYY-MM-DD>.log` (the UTC
//! date), a new file each day, 14 kept per process, and a day's file stopped at 64 MiB with one
//! `log_capped` line; a panic hook that logs `panic` with its location before the previous
//! hook runs; and a first `logging started` line with the process name and id.
//!
//! Several processes of one name (one `lil-mcp` per Claude Code session) append to the same
//! day's file, so the cap counts the file: each line is written under the file's append lock
//! (`LockFileEx` on `[2⁶⁴−2, +1)`), after reading the file's length and whether it already
//! ends with the `log_capped` line. A panic raised while a line is being written logs nothing
//! (the writer's lock is held), so the hook can't deadlock on it.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::env::Env;

/// A day's file stops at this size (ARCH-117).
pub const LOG_CAP_BYTES: u64 = 64 * 1024 * 1024;
/// Daily files kept per process (ARCH-117).
pub const LOG_FILES_KEPT: usize = 14;

/// Returned by [`init`] and [`init_at`]: logging lasts for the process. Each line is written
/// to its file as it is logged, so there is nothing to flush when it is dropped.
#[derive(Debug)]
pub struct LogGuard;

/// Logs `process` to [`Env::logs_dir`] with the 64 MiB cap, and installs the panic hook.
///
/// # Errors
/// If the folder can't be made, or the process already has a global `tracing` subscriber.
pub fn init(env: &Env, process: &str) -> io::Result<LogGuard> {
    init_at(&env.logs_dir(), process, LOG_CAP_BYTES)
}

/// [`init`] in `dir` with a cap of `cap_bytes` (tests).
///
/// # Errors
/// As [`init`].
pub fn init_at(dir: &Path, process: &str, cap_bytes: u64) -> io::Result<LogGuard> {
    let writer = DailyWriter::new(dir, process, cap_bytes, utc_today)?;
    init_with(writer, process)
}

/// [`init_at`] with a writer of its own (tests: a writer whose date fails).
///
/// # Errors
/// If the process already has a global `tracing` subscriber.
pub fn init_with(writer: DailyWriter, process: &str) -> io::Result<LogGuard> {
    tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_writer(writer)
        .try_init()
        .map_err(|e| io::Error::other(format!("cannot start logging: {e}")))?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let text = info.payload_as_str().unwrap_or("(no message)");
        // A panic inside a line's write would wait on the writer's own lock.
        if !IN_WRITE.with(Cell::get) {
            tracing::error!(%location, panic = text, "panic");
        }
        previous(info);
    }));
    // Every process's log starts with a line, so its file is there from the start.
    tracing::info!(process, pid = std::process::id(), "logging started");
    Ok(LogGuard)
}

/// The file writer behind [`init_at`]: one `<process>.<date>.log` per date `today` returns,
/// at most [`LOG_FILES_KEPT`] of them, each stopped at `cap_bytes` with one `log_capped` line.
/// Each `write` is one whole line (the JSON formatter writes a line at once). Clones share the
/// file.
#[derive(Clone)]
pub struct DailyWriter {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    dir: PathBuf,
    process: String,
    cap: u64,
    today: Box<dyn Fn() -> String + Send + Sync>,
    open: Option<Day>,
}

/// The open file of one day.
struct Day {
    date: String,
    file: File,
    capped: bool,
    /// The file's length after this writer's last line: another length means another
    /// process wrote meanwhile, maybe its `log_capped` line.
    seen: u64,
}

thread_local! {
    /// Set while this thread writes a line (the panic hook then logs nothing).
    static IN_WRITE: Cell<bool> = const { Cell::new(false) };
}

/// [`IN_WRITE`] for as long as it is held.
struct Writing;

impl Writing {
    fn enter() -> Writing {
        IN_WRITE.with(|w| w.set(true));
        Writing
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        IN_WRITE.with(|w| w.set(false));
    }
}

/// How long a line waits for another process's append lock before it is written without it.
const APPEND_LOCK_WAIT: Duration = Duration::from_millis(200);

impl DailyWriter {
    /// A writer for `process` in `dir` (made if missing), with the date from `today`
    /// (`YYYY-MM-DD`).
    ///
    /// # Errors
    /// If `process` isn't 1 to 64 ASCII letters, digits, `-` or `_` (so its files stay in
    /// `dir`), or `dir` can't be made.
    pub fn new(
        dir: &Path,
        process: &str,
        cap_bytes: u64,
        today: impl Fn() -> String + Send + Sync + 'static,
    ) -> io::Result<DailyWriter> {
        let valid = (1..=64).contains(&process.len())
            && process
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a log's process name is 1 to 64 ASCII letters, digits, `-` or `_`: {process:?}"
                ),
            ));
        }
        std::fs::create_dir_all(dir)?;
        Ok(DailyWriter {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.to_path_buf(),
                process: process.to_string(),
                cap: cap_bytes,
                today: Box::new(today),
                open: None,
            })),
        })
    }
}

impl Inner {
    /// The day's file, opened (and the old files pruned) when the date changed.
    fn day(&mut self) -> io::Result<&mut Day> {
        let date = (self.today)();
        if self.open.as_ref().is_none_or(|d| d.date != date) {
            self.open = None;
            let path = self.dir.join(format!("{}.{date}.log", self.process));
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&path)?;
            let len = file.metadata()?.len();
            let capped = ends_capped(&file, len);
            prune(&self.dir, &self.process, &date);
            self.open = Some(Day {
                date,
                file,
                capped,
                seen: len,
            });
        }
        Ok(self.open.as_mut().expect("opened above"))
    }
}

impl Write for DailyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _writing = Writing::enter();
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let cap = inner.cap;
        let day = inner.day()?;
        if day.capped {
            return Ok(buf.len());
        }
        // The file, not this writer, is what the cap counts: other processes append too.
        let _lock = append_lock(&day.file);
        let len = day.file.metadata()?.len();
        let capped_by_another = len != day.seen && ends_capped(&day.file, len);
        if capped_by_another || len + buf.len() as u64 > cap {
            day.capped = true;
            if !capped_by_another && !ends_capped(&day.file, len) {
                (&day.file).write_all(capped_line(cap).as_bytes())?;
            }
            return Ok(buf.len());
        }
        (&day.file).write_all(buf)?;
        day.seen = len + buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DailyWriter {
    type Writer = DailyWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The file's append lock, waited for up to [`APPEND_LOCK_WAIT`]; `None` past that (the line
/// is then written without it) or when the lock can't be taken at all.
fn append_lock(file: &File) -> Option<crate::fs::AppendLock<'_>> {
    let until = Instant::now() + APPEND_LOCK_WAIT;
    loop {
        match crate::fs::try_lock_append(file) {
            Ok(Some(lock)) => return Some(lock),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(1)),
            _ => return None,
        }
    }
}

/// Removes `<process>.<YYYY-MM-DD>.log` files in `dir` dated more than [`LOG_FILES_KEPT`]
/// days before today (UTC): the files of a process name no longer used, which [`prune`]
/// never reaches (the core's `core.<date>.log` from before plan 4a). Other files are never
/// touched; a file that can't be deleted is left.
pub fn prune_retired(dir: &Path, process: &str) {
    prune_retired_at(dir, process, &utc_today());
}

/// [`prune_retired`] with `today` given (`YYYY-MM-DD`, tests).
pub fn prune_retired_at(dir: &Path, process: &str, today: &str) {
    let Some(today) = days_of(today) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{process}.");
    for name in entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
    {
        let old = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".log"))
            .and_then(days_of)
            .is_some_and(|day| today - day > LOG_FILES_KEPT as i64);
        if old {
            let _ = std::fs::remove_file(dir.join(&name));
        }
    }
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date (Howard Hinnant's days-from-civil).
fn days_of(date: &str) -> Option<i64> {
    if !is_date(date) {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (
        date[..4].parse().ok()?,
        date[5..7].parse().ok()?,
        date[8..].parse().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Whether the file's last line is the `log_capped` line (a process that starts again on a
/// capped day writes no second one).
fn ends_capped(mut file: &File, len: u64) -> bool {
    let from = len.saturating_sub(512);
    let mut tail = Vec::new();
    file.seek(SeekFrom::Start(from)).is_ok()
        && file.read_to_end(&mut tail).is_ok()
        && String::from_utf8_lossy(&tail)
            .trim_end()
            .rsplit('\n')
            .next()
            .is_some_and(|last| last.contains("\"log_capped\""))
}

/// The one line that ends a capped day, in the formatter's JSON shape.
fn capped_line(cap: u64) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"level\":\"WARN\",\"fields\":{{\"message\":\"log_capped\",\
         \"cap_bytes\":{cap}}},\"target\":\"lil_platform::logs\"}}\n",
        utc_timestamp(SystemTime::now())
    )
}

/// Deletes all but the newest [`LOG_FILES_KEPT`] `<process>.<YYYY-MM-DD>.log` files in `dir`,
/// never the day `today`'s, which the writer has just opened: with the clock behind newer
/// files it is kept with the newest others. Other files are never touched; a file that can't
/// be deleted is left.
fn prune(dir: &Path, process: &str, today: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{process}.");
    let own = format!("{process}.{today}.log");
    let mut dated: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".log"))
                .is_some_and(is_date)
                && *name != own
        })
        .collect();
    dated.sort();
    let excess = dated.len().saturating_sub(LOG_FILES_KEPT - 1);
    for name in &dated[..excess] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// `YYYY-MM-DD` with digits.
fn is_date(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// Today's UTC date, `YYYY-MM-DD`.
fn utc_today() -> String {
    utc_timestamp(SystemTime::now())[..10].to_string()
}

/// `t` as RFC 3339 UTC with microseconds, `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
fn utc_timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil-from-days, for days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_micros()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test] // spec: ARCH-117
    fn utc_timestamps_are_rfc3339() {
        let at = |secs: u64| utc_timestamp(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "1970-01-01T00:00:00.000000Z");
        // 2024-02-29 (a leap day) 23:59:59 UTC.
        assert_eq!(at(1_709_251_199), "2024-02-29T23:59:59.000000Z");
        assert_eq!(at(1_709_251_200), "2024-03-01T00:00:00.000000Z");
        assert!(is_date("2026-10-24"));
        assert_eq!(days_of("1970-01-01"), Some(0));
        assert_eq!(days_of("2024-03-01"), Some(1_709_251_200 / 86_400));
        assert_eq!(days_of("2026-13-01"), None);
        for bad in ["2026-1-24", "2026/10/24", "2026-10-2x", "notes"] {
            assert!(!is_date(bad), "{bad}");
        }
    }
}
