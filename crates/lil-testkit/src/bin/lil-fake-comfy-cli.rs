//! `lil-fake-comfy-cli`: a stand-in for comfy-cli's `comfy.exe` (QA-066). It appends its
//! arguments, as one JSON array per line, to `<--workspace>\fake-comfy-cli.log`, writes
//! `{path_first, names}` (its first `PATH` entry and its variables' names) to
//! `<--workspace>\fake-comfy-cli.env.json`, then exits 1
//! when `<--workspace>\FAIL` exists and 0 otherwise. Without `--workspace <dir>` it exits 2.

use std::io::Write as _;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(workspace) = args
        .iter()
        .position(|a| a == "--workspace")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
    else {
        eprintln!("lil-fake-comfy-cli: no --workspace <dir>");
        std::process::exit(2);
    };
    let log = workspace.join("fake-comfy-cli.log");
    let line = serde_json::Value::from(args.clone()).to_string();
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = written {
        eprintln!(
            "lil-fake-comfy-cli: cannot append to {}: {e}",
            log.display()
        );
        std::process::exit(2);
    }
    // What it was started with: the first `PATH` entry and every variable's name.
    let path_first = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).next())
        .map(|p| p.to_string_lossy().into_owned());
    let names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    let started = serde_json::json!({"path_first": path_first, "names": names});
    if let Err(e) = std::fs::write(
        workspace.join("fake-comfy-cli.env.json"),
        started.to_string(),
    ) {
        eprintln!("lil-fake-comfy-cli: cannot write its environment: {e}");
        std::process::exit(2);
    }
    if workspace.join("FAIL").exists() {
        eprintln!("lil-fake-comfy-cli: failing because FAIL exists");
        std::process::exit(1);
    }
}
