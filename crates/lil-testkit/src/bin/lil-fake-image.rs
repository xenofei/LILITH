//! `lil-fake-image`: a stand-in for the image app `lil-image.exe` (QA-066), for `lil-mcp`'s
//! launches and `lil confirm-window`. Tests copy it into their own folder as `lil-image.exe`.
//! Each run appends one JSON line `{"args": [<argument>, …]}` to
//! `%LIL_TEST_ROOT%\fake-image-args.jsonl` and exits 0; without `LIL_TEST_ROOT` it exits 2.
//! It is a windows-subsystem program, so no console window opens when it starts.

#![windows_subsystem = "windows"]

use std::io::Write as _;
use std::path::PathBuf;

fn main() {
    let Some(root) = std::env::var_os("LIL_TEST_ROOT").filter(|r| !r.is_empty()) else {
        std::process::exit(2);
    };
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let line = serde_json::json!({ "args": args }).to_string();
    let file = PathBuf::from(root).join("fake-image-args.jsonl");
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| writeln!(f, "{line}"));
    if written.is_err() {
        std::process::exit(2);
    }
}
