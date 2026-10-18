//! Single file-write allowance test (ARCH-112, QA-024).
//!
//! `lil-core` forbids file-mutating std and tokio calls through `crates/lil-core/clippy.toml`.
//! The exceptions are the functions of `src/store/fsops.rs` (`write_file_atomic`,
//! `ensure_dir`, ...), each carrying its own `#[allow(clippy::disallowed_methods)]`.
//! This test scans every `*.rs` file under `crates/lil-core/src` and asserts that the allowance
//! (or an `expect` of the same lint) appears only in `src/store/fsops.rs`, that the file exists,
//! and that each occurrence attaches to a single function (no module-level or crate-level
//! blanket allowance, and at most one allowance per function).

use std::fs;
use std::path::{Path, PathBuf};

const LINT: &str = "disallowed_methods";

fn core_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("lil-core")
        .join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// True for a line that allows or expects the lint (any attribute form).
fn is_allowance(line: &str) -> bool {
    !is_comment(line)
        && line.contains(LINT)
        && (line.contains("allow(") || line.contains("expect("))
}

#[test]
fn disallowed_methods_allowance_only_in_store_on_single_functions() {
    let src = core_src();
    let fsops = src.join("store").join("fsops.rs");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty(), "no sources found under lil-core/src");

    let mut problems = Vec::new();
    let mut store_seen = false;

    for file in &files {
        let in_store = *file == fsops;
        store_seen |= in_store;
        let text = fs::read_to_string(file).expect("read source");
        let lines: Vec<&str> = text.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            if !is_allowance(line) {
                continue;
            }
            if !in_store {
                problems.push(format!(
                    "{}:{}: allowance outside store/fsops.rs",
                    file.display(),
                    i + 1
                ));
                continue;
            }
            // Inner attributes (`#![...]`) would blanket a whole module.
            if line.trim_start().starts_with("#![") {
                problems.push(format!(
                    "{}:{}: module-level allowance",
                    file.display(),
                    i + 1
                ));
                continue;
            }
            // The attribute must attach to a function: skip other attributes, comments, blanks.
            let target = lines[i + 1..]
                .iter()
                .find(|l| {
                    let t = l.trim_start();
                    !(t.is_empty() || t.starts_with("#[") || t.starts_with("//"))
                })
                .copied()
                .unwrap_or("");
            if !target.contains("fn ") {
                problems.push(format!(
                    "{}:{}: allowance does not attach to a function (next item: `{}`)",
                    file.display(),
                    i + 1,
                    target.trim()
                ));
            }
        }

        // At most one allowance per function: two allowances need a `fn` line between them.
        let allowance_lines: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| is_allowance(l))
            .map(|(i, _)| i)
            .collect();
        for pair in allowance_lines.windows(2) {
            let between = &lines[pair[0] + 1..pair[1]];
            if in_store && !between.iter().any(|l| l.contains("fn ")) {
                problems.push(format!(
                    "{}:{}: more than one allowance on the same function",
                    file.display(),
                    pair[1] + 1
                ));
            }
        }
    }

    assert!(store_seen, "lil-core/src/store/fsops.rs is missing");
    assert!(
        problems.is_empty(),
        "single-write allowance violated:\n{}",
        problems.join("\n")
    );
}
