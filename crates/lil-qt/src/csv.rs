//! The Usage & spend panel's CSV export (APP-117), Qt-free: [`csv`] is the RFC 4180 text of the
//! rows the panel shows, and [`export`] writes it to the file you picked through `replace_file`
//! (the bridge's `CsvExport` calls it off the GUI thread).
//!
//! - **Text.** A header line of the column names, then one line per row, each cell the row's
//!   value under that column: a string as it is, a number or `true`/`false` as JSON writes it, a
//!   missing value or `null` empty, and an array or object as compact JSON. Every line ends with
//!   CRLF. A cell holding a comma, a double quote, CR or LF is quoted, its quotes doubled.
//! - **Formulas.** A cell starting with `=`, `+`, `-` or `@` is prefixed with `'`, so a
//!   spreadsheet shows it as text and never runs it (a provider or model name is text the suite
//!   doesn't choose).
//! - **The file.** Written whole or not at all: a failed write leaves the file that was there as
//!   it was.

use std::path::Path;

use serde_json::Value;

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
/// When the file can't be written; the file at `path`, if any, is then unchanged.
pub fn export(path: &Path, rows: &[Value], columns: &[&str]) -> std::io::Result<()> {
    crate::layout_file::write_replacing(path, csv(rows, columns).as_bytes())
}

/// Appends one line of `cells`, each guarded and quoted, ending with CRLF.
fn line(out: &mut String, cells: impl Iterator<Item = String>) {
    for (i, cell) in cells.enumerate() {
        if i > 0 {
            out.push(',');
        }
        let guarded = if cell.starts_with(['=', '+', '-', '@']) {
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
    use serde_json::json;

    #[test] // spec: APP-117
    fn csv_quotes_and_guards_formulas() {
        let rows = [
            json!({"provider": "openrouter", "records": 3, "amount": "1.25", "estimated": true}),
            json!({"provider": "a, \"b\"", "records": 1, "amount": null}),
            json!({"provider": "=HYPERLINK(\"x\")", "records": 2, "amount": "+1", "note": "-2"}),
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
            lines[3], "\"'=HYPERLINK(\"\"x\"\")\",,2,'+1,'-2\r\n",
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

    #[test] // spec: APP-117
    fn csv_export_replaces_the_file_and_a_failed_write_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.csv");
        let rows = [json!({"provider": "p", "records": 1})];
        export(&path, &rows, &["provider", "records"]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "provider,records\r\np,1\r\n",
            "a new file"
        );
        let rows = [json!({"provider": "q", "records": 2})];
        export(&path, &rows, &["provider", "records"]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "provider,records\r\nq,2\r\n",
            "an existing file is replaced"
        );
        // Another program holds the file open without sharing: the write fails and the file
        // keeps what it had.
        let held = lil_platform::fs::open_exclusive(&path).unwrap();
        let rows = [json!({"provider": "r", "records": 3})];
        assert!(export(&path, &rows, &["provider", "records"]).is_err());
        drop(held);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "provider,records\r\nq,2\r\n",
            "a failed write leaves the file untouched"
        );
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.ends_with(".lil-tmp")),
            "no temp file is left: {names:?}"
        );
    }
}
