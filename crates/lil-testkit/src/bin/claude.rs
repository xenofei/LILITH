//! The fake `claude` (Claude Code's CLI) for tests (QA-066). It never talks to a network and
//! never reads a real profile: the plugin commands need `CLAUDE_CONFIG_DIR`.
//!
//! - `claude --version` prints `9.9.9 (Claude Code)`;
//! - `claude plugin list --json` prints `<config>\fake-plugins.json` (`[]` when missing);
//! - `claude plugin marketplace add …`, `claude plugin install …` and `claude plugin update …`
//!   append their arguments, as one JSON array per line, to `<config>\fake-claude.log`;
//! - `claude --spawn-stdio <exe> [args…]` starts `<exe>` as its direct child with stdin, stdout
//!   and stderr passed through, waits for it and exits with its exit code.
//!
//! Anything else exits 2. So that a `claude.exe` left in a build folder can never stand in for
//! Claude Code outside a test (a dev core trusts `mcp` under a `claude.exe` parent, ARCH-047),
//! it does nothing and exits 2 unless `LIL_CHANNEL` is a `test-*` channel and `LIL_TEST_ROOT`
//! is set.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;

fn usage(message: &str) -> ! {
    eprintln!("fake claude: {message}");
    std::process::exit(2);
}

fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| usage("CLAUDE_CONFIG_DIR is not set"))
}

fn main() {
    let on_test_channel = std::env::var("LIL_CHANNEL").is_ok_and(|c| c.starts_with("test-"))
        && std::env::var_os("LIL_TEST_ROOT").is_some_and(|r| !r.is_empty());
    if !on_test_channel {
        usage("this is the test kit's fake claude; it runs only on a test channel");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["--version"] => println!("9.9.9 (Claude Code)"),
        ["--spawn-stdio", exe, rest @ ..] => {
            let status = Command::new(exe)
                .args(rest)
                .status()
                .unwrap_or_else(|e| usage(&format!("cannot start {exe}: {e}")));
            std::process::exit(status.code().unwrap_or(1));
        }
        ["plugin", "list", "--json"] => {
            let file = config_dir().join("fake-plugins.json");
            match std::fs::read_to_string(&file) {
                Ok(text) => println!("{}", text.trim_end()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("[]"),
                Err(e) => usage(&format!("cannot read {}: {e}", file.display())),
            }
        }
        ["plugin", "marketplace", "add", ..]
        | ["plugin", "install", ..]
        | ["plugin", "update", ..] => {
            let log = config_dir().join("fake-claude.log");
            let line = serde_json::Value::from(args.clone()).to_string();
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                usage(&format!("cannot append to {}: {e}", log.display()));
            }
        }
        _ => usage(&format!("unknown arguments {args:?}")),
    }
}
