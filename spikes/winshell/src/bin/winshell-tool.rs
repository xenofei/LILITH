//! `winshell-tool`: the console helper the scripts call. Each command prints one JSON line.
//!
//! - `spawn -- <exe> [args...]`: start a process detached (`DETACHED_PROCESS |
//!   CREATE_NEW_PROCESS_GROUP`), as `lil-client` starts the core; prints `{pid}`
//! - `programs-dir`: `FOLDERID_Programs`
//! - `shortcut-write --path <lnk> --target <exe> --aumid <id>` / `shortcut-read --path <lnk>`
//! - `setting`: `ToastNotifier::Setting()` for the spike's AUMID
//! - `history [--tag T]`: the Tags of the spike's toasts in the notification centre
//! - `history-clear`: removes them
//! - `shellexec <url>`: `ShellExecuteW(url)`
//! - `token`: this process's elevation and integrity level
//! - `foreground`: the foreground window, its pid and the focus window
//! - `wsb --bundle <dir> --results <dir> [--deploy <dir>] --out <file.wsb>`: the Sandbox config
//! - `b64url <text>`: base64url of the UTF-8 text, without padding

use std::path::Path;

use serde_json::{Value, json};
use winshell::args::Args;
use winshell::text::{Mapped, base64url, wsb_xml};
use winshell::win::{
    foreground_info, history_clear, history_tags, init_winrt, notifier_setting, programs_dir,
    read_shortcut, shell_execute, spawn_detached, token_info, write_shortcut,
};

/// The Sandbox side of the mapped folders and the logon command (S1.9).
const SANDBOX_IN: &str = r"C:\lil\in";
const SANDBOX_OUT: &str = r"C:\lil\out";
const LOGON: &str = r"powershell.exe -ExecutionPolicy Bypass -File C:\lil\in\inside.ps1";

fn main() {
    let args = Args::parse(std::env::args().skip(1));
    let out = match args.command.as_deref() {
        Some("spawn") => spawn(&args),
        Some("programs-dir") => match programs_dir() {
            Ok(p) => json!({ "programs": p }),
            Err(e) => json!({ "error": e.to_string() }),
        },
        Some("shortcut-write") => {
            let r = write_shortcut(
                Path::new(args.get_or("path", "")),
                Path::new(args.get_or("target", "")),
                args.get_or("aumid", winshell::AUMID),
            );
            json!({ "written": r.is_ok(), "error": r.err().map(|e| e.to_string()) })
        }
        Some("shortcut-read") => {
            let path = Path::new(args.get_or("path", ""));
            if !path.exists() {
                json!({ "exists": false })
            } else {
                match read_shortcut(path) {
                    Ok((target, aumid)) => {
                        json!({ "exists": true, "target": target, "aumid": aumid })
                    }
                    Err(e) => json!({ "exists": true, "error": e.to_string() }),
                }
            }
        }
        Some("setting") => {
            init_winrt();
            json!({ "aumid": winshell::AUMID, "setting": notifier_setting() })
        }
        Some("history") => {
            init_winrt();
            match history_tags() {
                Ok(tags) => {
                    let has = args.get("tag").map(|t| tags.iter().any(|x| x == t));
                    json!({ "setting": notifier_setting(), "tags": tags, "has_tag": has })
                }
                Err(e) => json!({ "setting": notifier_setting(), "error": e.to_string() }),
            }
        }
        Some("history-clear") => {
            init_winrt();
            let r = history_clear();
            json!({ "cleared": r.is_ok(), "error": r.err().map(|e| e.to_string()) })
        }
        Some("shellexec") => {
            let url = args.positional.first().cloned().unwrap_or_default();
            let code = shell_execute(&url);
            json!({ "url": url, "shell_execute": code, "ok": code > 32 })
        }
        Some("token") => token_info(),
        Some("foreground") => foreground_info(),
        Some("wsb") => wsb(&args),
        Some("b64url") => {
            let text = args.positional.first().cloned().unwrap_or_default();
            json!({ "text": text, "b64url": base64url(text.as_bytes()) })
        }
        other => json!({ "error": format!("unknown command {other:?}") }),
    };
    println!("{out}");
}

fn spawn(args: &Args) -> Value {
    let Some((exe, rest)) = args.rest.split_first() else {
        return json!({ "error": "spawn needs -- <exe> [args...]" });
    };
    match spawn_detached(exe, rest) {
        Ok(pid) => json!({ "pid": pid }),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

fn wsb(args: &Args) -> Value {
    let bundle = args.get_or("bundle", "");
    let results = args.get_or("results", "");
    let out = args.get_or("out", "");
    if bundle.is_empty() || results.is_empty() || out.is_empty() {
        return json!({ "error": "wsb needs --bundle, --results and --out" });
    }
    let mut mapped = vec![Mapped {
        host: bundle,
        sandbox: SANDBOX_IN,
        read_only: true,
    }];
    // The deploy copy is mapped read-only at its own host path (S1.9: a SandboxFolder outside
    // the default places).
    if let Some(deploy) = args.get("deploy").filter(|d| !d.is_empty()) {
        mapped.push(Mapped {
            host: deploy,
            sandbox: deploy,
            read_only: true,
        });
    }
    mapped.push(Mapped {
        host: results,
        sandbox: SANDBOX_OUT,
        read_only: false,
    });
    let xml = wsb_xml(&mapped, LOGON);
    match std::fs::write(out, &xml) {
        Ok(()) => json!({ "written": out, "xml": xml }),
        Err(e) => json!({ "error": e.to_string() }),
    }
}
