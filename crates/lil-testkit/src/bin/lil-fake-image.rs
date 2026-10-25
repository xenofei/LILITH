//! `lil-fake-image`: a stand-in for the image app `lil-image.exe` (QA-066), for `lil-mcp`'s
//! launches and `lil confirm-window`. Tests copy it into their own folder as `lil-image.exe`.
//! Each run appends one JSON line `{"args": [<argument>, …], "use_show_window": <bool>,
//! "show_window": <n>}` to `%LIL_TEST_ROOT%\fake-image-args.jsonl` and exits 0; without
//! `LIL_TEST_ROOT` it exits 2. `use_show_window` and `show_window` are its `STARTUPINFOW`'s
//! `STARTF_USESHOWWINDOW` flag and `wShowWindow` (`SW_SHOWNOACTIVATE` is 4): how its first
//! window would be shown, so a test sees that it was started without focus.
//! It is a windows-subsystem program, so no console window opens when it starts.

#![windows_subsystem = "windows"]

use std::io::Write as _;
use std::path::PathBuf;

use windows::Win32::System::Threading::{GetStartupInfoW, STARTF_USESHOWWINDOW, STARTUPINFOW};

fn main() {
    let Some(root) = std::env::var_os("LIL_TEST_ROOT").filter(|r| !r.is_empty()) else {
        std::process::exit(2);
    };
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut startup = STARTUPINFOW::default();
    // SAFETY: `startup` is a valid, writable STARTUPINFOW.
    unsafe { GetStartupInfoW(&mut startup) };
    let line = serde_json::json!({
        "args": args,
        "use_show_window": startup.dwFlags.0 & STARTF_USESHOWWINDOW.0 != 0,
        "show_window": startup.wShowWindow,
    })
    .to_string();
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
