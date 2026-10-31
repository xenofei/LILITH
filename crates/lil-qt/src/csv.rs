//! The Usage & spend panel's CSV export (APP-117), Qt-free: [`csv`] is the RFC 4180 text of the
//! rows the panel shows, and [`export`] writes it to the file you picked through `replace_file`
//! (the bridge's `CsvExport` calls it off the GUI thread).
//!
//! - **Text.** A header line of the column names, then one line per row, each cell the row's
//!   value under that column: a string as it is, a number or `true`/`false` as JSON writes it, a
//!   missing value or `null` empty, and an array or object as compact JSON. Every line ends with
//!   CRLF. A cell holding a comma, a double quote, CR or LF is quoted, its quotes doubled.
//! - **Formulas** (OWASP's CSV-injection list). A cell starting with TAB or CR, or whose first
//!   character other than whitespace (VT, no-break and ideographic spaces included) is `=`, `+`,
//!   `-` or `@`, is prefixed with `'`, so a spreadsheet shows it as text and never runs it (a
//!   provider or model name is text the suite doesn't choose). A plain decimal number (`-1.5`,
//!   `-3`) stays a number.
//! - **The file.** [`export`] writes a UTF-8 BOM first, so Excel reads non-ASCII names right.
//!   It is written whole or not at all: the old file is swapped out under the LILITH-owned
//!   hidden name `.<name>.lil-bak` beside it (never a name of yours such as `<name>.bak`), which
//!   is deleted once the new file is in. A failed write leaves the file that was there as it was,
//!   and its error says so; in the rare case the old file is left only under its backup name,
//!   the error names it.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

/// What [`export`] writes before the text.
const BOM: &str = "\u{FEFF}";

/// The CSV text of `rows` under `columns` (module docs).
pub fn csv(rows: &[Value], columns: &[&str]) -> String {
    let mut out = String::new();
    line(&mut out, columns.iter().map(|c| (*c).to_string()));
    for row in rows {
        line(
            &mut out,
            columns
                .iter()
                .map(|c| text_of(row.get(*c).unwrap_or(&Value::Null))),
        );
    }
    out
}

/// Writes [`csv`] of `rows` to `path` through `replace_file` (module docs).
///
/// # Errors
/// When the file can't be written; the error says what became of the file at `path`.
pub fn export(path: &Path, rows: &[Value], columns: &[&str]) -> io::Result<()> {
    let text = format!("{BOM}{}", csv(rows, columns));
    write_kept(path, text.as_bytes())
}

/// How long closing the app waits for the exports under way, then how long those told to stop have
/// to do it: a stalled drive then costs a stray temporary file, not a process that stays around.
pub const CLOSE_WAIT: Duration = Duration::from_secs(10);
pub const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// How the wait for the writes ended ([`Writes::wait`]).
#[derive(Debug, PartialEq, Eq)]
pub struct WritesEnded {
    /// Some were still going at the limit, and were told to stop: each keeps the old file and
    /// removes its temporary one.
    pub stopped: bool,
    /// How many didn't stop in the grace, each with a temporary file that may stay.
    pub running: usize,
}

/// The exports' writer threads, which `main` waits for after its event loop: nothing else does, since
/// the panels that started them aren't destroyed before the process ends.
pub struct Writes {
    workers: Mutex<Vec<JoinHandle<()>>>,
    closing: AtomicBool,
}

impl Writes {
    pub const fn new() -> Writes {
        Writes {
            workers: Mutex::new(Vec::new()),
            closing: AtomicBool::new(false),
        }
    }

    pub fn add(&self, worker: JoinHandle<()>) {
        let mut workers = self.lock();
        workers.retain(|w| !w.is_finished());
        workers.push(worker);
    }

    /// Whether a write under way should stop.
    pub fn closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.workers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn running(&self) -> usize {
        self.lock().iter().filter(|w| !w.is_finished()).count()
    }

    fn wait_up_to(&self, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        while self.running() > 0 {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// Waits for the writes at most `limit`, then tells those still going to stop and gives them
    /// `grace`. It never waits longer than the two.
    pub fn wait(&self, limit: Duration, grace: Duration) -> WritesEnded {
        if self.wait_up_to(limit) {
            return WritesEnded {
                stopped: false,
                running: 0,
            };
        }
        self.closing.store(true, Ordering::SeqCst);
        self.wait_up_to(grace);
        WritesEnded {
            stopped: true,
            running: self.running(),
        }
    }
}

impl Default for Writes {
    fn default() -> Self {
        Writes::new()
    }
}

/// This process's exports.
pub static WRITES: Writes = Writes::new();

/// Writes `bytes` to the file you picked, replacing what is there. A failure says why and what
/// became of the file that was there (a file of the same name is kept as a hidden backup until
/// the new one is in).
///
/// # Errors
/// The write failed.
pub fn write_kept(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_kept_unless(path, bytes, &|| WRITES.closing())
}

fn write_kept_unless(path: &Path, bytes: &[u8], stop: &dyn Fn() -> bool) -> io::Result<()> {
    let backup_name = |name: &str| format!(".{name}.lil-bak");
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let backup = path.with_file_name(backup_name(&name));
    let existed = path.try_exists().unwrap_or(false);
    match crate::layout_file::write_replacing_stoppable(path, bytes, backup_name, stop) {
        Ok(()) => {
            if existed && let Err(e) = std::fs::remove_file(&backup) {
                tracing::warn!(error = %e, "the old file's backup wasn't deleted");
            }
            Ok(())
        }
        Err(e) => {
            let left = if !existed {
                format!("Nothing was written to {name}.")
            } else if !path.try_exists().unwrap_or(true) && backup.try_exists().unwrap_or(false) {
                format!(
                    "The old {name} is kept as {} beside it.",
                    backup_name(&name)
                )
            } else {
                format!("{name} is as it was.")
            };
            Err(io::Error::new(e.kind(), format!("{e}. {left}")))
        }
    }
}

/// Whether a spreadsheet could take `cell` as a formula (module docs).
fn needs_guard(cell: &str) -> bool {
    if is_decimal(cell) {
        return false;
    }
    cell.starts_with(['\t', '\r'])
        || cell
            .trim_start_matches(char::is_whitespace)
            .starts_with(['=', '+', '-', '@'])
}

/// `-?[0-9]+(\.[0-9]+)?`.
fn is_decimal(cell: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let unsigned = cell.strip_prefix('-').unwrap_or(cell);
    match unsigned.split_once('.') {
        Some((whole, part)) => digits(whole) && digits(part),
        None => digits(unsigned),
    }
}

/// Appends one line of `cells`, each guarded and quoted, ending with CRLF.
fn line(out: &mut String, cells: impl Iterator<Item = String>) {
    for (i, cell) in cells.enumerate() {
        if i > 0 {
            out.push(',');
        }
        let guarded = if needs_guard(&cell) {
            format!("'{cell}")
        } else {
            cell
        };
        if guarded.contains([',', '"', '\r', '\n']) {
            out.push('"');
            out.push_str(&guarded.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(&guarded);
        }
    }
    out.push_str("\r\n");
}

/// A value's cell text (module docs).
fn text_of(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    #[test] // spec: ARCH-121, APP-117
    fn closing_waits_for_the_writes_then_tells_them_to_stop() {
        // A write that ends: no stop.
        let writes = Writes::new();
        writes.add(std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(30))
        }));
        assert_eq!(
            writes.wait(Duration::from_secs(5), Duration::from_secs(1)),
            WritesEnded {
                stopped: false,
                running: 0
            }
        );
        assert!(!writes.closing());

        // One that is still going at the limit is told to stop, and has the grace to do it.
        let writes = Arc::new(Writes::new());
        let seen = writes.clone();
        writes.add(std::thread::spawn(move || {
            while !seen.closing() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }));
        assert_eq!(
            writes.wait(Duration::from_millis(50), Duration::from_secs(5)),
            WritesEnded {
                stopped: true,
                running: 0
            }
        );

        // One that can't stop (a stalled drive) doesn't hold the process.
        let writes = Writes::new();
        writes.add(std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(3))
        }));
        let began = Instant::now();
        assert_eq!(
            writes.wait(Duration::from_millis(30), Duration::from_millis(30)),
            WritesEnded {
                stopped: true,
                running: 1
            }
        );
        assert!(began.elapsed() < Duration::from_secs(2));
    }
    use serde_json::json;

    #[test] // spec: APP-117
    fn csv_quotes_and_guards_formulas() {
        let rows = [
            json!({"provider": "openrouter", "records": 3, "amount": "1.25", "estimated": true}),
            json!({"provider": "a, \"b\"", "records": 1, "amount": null}),
            json!({"provider": "=HYPERLINK(\"x\")", "records": 2, "amount": "+1", "note": "-x"}),
            json!({"provider": "@SUM(A1)", "model": "line\nbreak", "note": "cr\rhere"}),
            json!({"provider": "list", "note": ["a", 1], "amount": {"USD": "1"}}),
        ];
        let text = csv(&rows, &["provider", "model", "records", "amount", "note"]);
        let lines: Vec<&str> = text.split_inclusive("\r\n").collect();
        assert_eq!(lines[0], "provider,model,records,amount,note\r\n");
        assert_eq!(
            lines[1], "openrouter,,3,1.25,\r\n",
            "missing and null are empty"
        );
        assert_eq!(
            lines[2], "\"a, \"\"b\"\"\",,1,,\r\n",
            "a comma and quotes are quoted"
        );
        assert_eq!(
            lines[3], "\"'=HYPERLINK(\"\"x\"\")\",,2,'+1,'-x\r\n",
            "a formula is text, then quoted"
        );
        assert_eq!(
            lines[4], "'@SUM(A1),\"line\nbreak\",,,\"cr\rhere\"\r\n",
            "an @ is guarded; CR and LF are quoted"
        );
        assert_eq!(
            lines[5], "list,,,\"{\"\"USD\"\":\"\"1\"\"}\",\"[\"\"a\"\",1]\"\r\n",
            "arrays and objects are compact JSON"
        );
        assert_eq!(lines.len(), 6);
        assert_eq!(csv(&[], &["a", "b"]), "a,b\r\n", "the header alone");
    }

    /// OWASP's CSV-injection list: a leading TAB or CR, and `= + - @` after leading whitespace.
    #[test] // spec: APP-117
    fn csv_guards_a_leading_tab_cr_or_whitespace_before_a_formula() {
        let cell = |v: Value| {
            let text = csv(&[json!({ "c": v })], &["c"]);
            text.strip_prefix("c\r\n")
                .unwrap()
                .strip_suffix("\r\n")
                .unwrap()
                .to_string()
        };
        assert_eq!(cell(json!("\t=1+1")), "'\t=1+1", "a leading TAB");
        assert_eq!(
            cell(json!("\tplain")),
            "'\tplain",
            "a leading TAB, whatever follows"
        );
        assert_eq!(
            cell(json!("\r=cmd|x")),
            "\"'\r=cmd|x\"",
            "a leading CR, then quoted"
        );
        assert_eq!(cell(json!(" =1+1")), "' =1+1", "a space before =");
        assert_eq!(cell(json!("  +1")), "'  +1", "spaces before +");
        assert_eq!(cell(json!(" -x")), "' -x", "a space before -");
        assert_eq!(
            cell(json!("\n@x")),
            "\"'\n@x\"",
            "a line feed before @, then quoted"
        );
        assert_eq!(cell(json!("a=b")), "a=b", "= inside text is fine");
        assert_eq!(
            cell(json!(" plain")),
            " plain",
            "leading spaces before text are fine"
        );
        assert_eq!(cell(json!("\u{b}=1")), "'\u{b}=1", "a VT before =");
        assert_eq!(cell(json!("\u{a0}=1+1")), "'\u{a0}=1+1", "a no-break space");
        assert_eq!(
            cell(json!("\u{3000}=1+1")),
            "'\u{3000}=1+1",
            "an ideographic space"
        );
    }

    #[test] // spec: APP-117
    fn csv_keeps_negative_numbers_as_numbers() {
        let cell = |v: Value| {
            let text = csv(&[json!({ "c": v })], &["c"]);
            text.strip_prefix("c\r\n")
                .unwrap()
                .strip_suffix("\r\n")
                .unwrap()
                .to_string()
        };
        assert_eq!(cell(json!("-1.5")), "-1.5", "a negative decimal string");
        assert_eq!(cell(json!("-3")), "-3");
        assert_eq!(cell(json!(-3)), "-3", "a negative JSON number");
        assert_eq!(cell(json!(-0.25)), "-0.25");
        assert_eq!(
            cell(json!("-1.5e3")),
            "'-1.5e3",
            "not a plain decimal: guarded"
        );
        assert_eq!(cell(json!("-1.")), "'-1.", "not a plain decimal: guarded");
        assert_eq!(cell(json!("--1")), "'--1");
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test] // spec: APP-117
    fn csv_export_replaces_the_file_and_a_failed_write_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.csv");
        // A file of the user's own that a backup must never take.
        let theirs = dir.path().join("usage.csv.bak");
        std::fs::write(&theirs, "mine").unwrap();
        let rows = [json!({"provider": "p", "records": 1})];
        export(&path, &rows, &["provider", "records"]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..3], b"\xEF\xBB\xBF", "a UTF-8 BOM, for Excel");
        assert_eq!(&bytes[3..], b"provider,records\r\np,1\r\n", "a new file");
        let rows = [json!({"provider": "café", "records": 2})];
        export(&path, &rows, &["provider", "records"]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "\u{FEFF}provider,records\r\ncafé,2\r\n",
            "an existing file is replaced"
        );
        assert_eq!(
            names(dir.path()),
            ["usage.csv", "usage.csv.bak"],
            "no backup or temp file is left beside it"
        );
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "mine");
        // Another program holds the file open without sharing: the write fails, the file keeps
        // what it had, and the error says so.
        let held = lil_platform::fs::open_exclusive(&path).unwrap();
        let rows = [json!({"provider": "r", "records": 3})];
        let why = export(&path, &rows, &["provider", "records"])
            .unwrap_err()
            .to_string();
        drop(held);
        assert!(why.contains("usage.csv is as it was"), "{why}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "\u{FEFF}provider,records\r\ncafé,2\r\n",
            "a failed write leaves the file untouched"
        );
        assert!(
            !names(dir.path()).iter().any(|n| n.ends_with(".lil-tmp")),
            "no temp file is left: {:?}",
            names(dir.path())
        );
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "mine");
    }

    #[test] // spec: APP-117
    fn csv_export_says_nothing_was_written_when_there_was_no_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocker"), "a file").unwrap();
        let path = dir.path().join("blocker").join("usage.csv");
        let why = export(&path, &[], &["a"]).unwrap_err().to_string();
        assert!(why.contains("Nothing was written to usage.csv"), "{why}");
    }
}
