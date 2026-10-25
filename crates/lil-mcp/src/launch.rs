//! Starting a stopped suite app for an agent (ARCH-015, APP-065; Appendix G "`lil-mcp`
//! launches"). The core never launches an app (APP-064): its `open_in` and `ui.show` answer
//! `{running, pid, launch_command}`. After such a call, `lil-mcp` starts the app itself when:
//!
//! - the session is `mcp` (the core verified Claude Code as this server's parent, ARCH-047); a
//!   `cli` session, like `lil call`, only returns the launch command;
//! - the result is `ok` with `running: false` and a `launch_command`;
//! - its `exe` is a suite app registered beside the core this session reached: an
//!   `apps\<app>.toml` in the folder of the core's process (`lil_status`'s `core.pid`), which
//!   must be a folder this client trusts (ARCH-027). An external program's registration in
//!   `%APPDATA%\LILITH\apps\` never counts.
//!
//! The app is started detached and without focus (`SW_SHOWNOACTIVATE`), with `--no-activate`
//! before the URL (the last argument), and the result text gains "Started <app> in the
//! background." (or why it couldn't start). The core already posted the `agent_activity` toast
//! for the call (APP-065), so `lil-mcp` posts none. The result's structured content is the
//! core's, unchanged.

use std::path::{Path, PathBuf};

use lil_platform::paths::folder_key;
use lil_platform::process::{ProcessInspector as _, SystemInspector, spawn_no_activate};
use lil_protocol::{Outcome, ToolOutput};

use crate::core_link::Linked;

/// The tools whose results may carry a launch command.
const LAUNCHING_TOOLS: [&str; 2] = ["open_in", "ui.show"];

/// The flag that shows the app's window without activating it (Appendix G "Apps").
const NO_ACTIVATE: &str = "--no-activate";

/// What to start, once checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The registered app's name.
    pub app: String,
    pub exe: PathBuf,
    /// The core's arguments with `--no-activate` before the URL.
    pub args: Vec<String>,
}

/// After `tool` returned `outcome` on `linked`: starts the app when the module docs allow it
/// and returns the line to add to the result text; `None` when there was nothing to start.
pub async fn after_call(
    linked: &Linked,
    tool: &str,
    outcome: &Outcome<ToolOutput>,
) -> Option<String> {
    if !LAUNCHING_TOOLS.contains(&tool) || linked.hello().caller_kind != "mcp" {
        return None;
    }
    let Outcome::Ok(output) = outcome else {
        return None;
    };
    let (exe, args) = launch_command(&output.structured)?;
    let pid = linked.hello().core.pid;
    let trusted = linked.server_dirs.clone();
    tokio::task::spawn_blocking(move || {
        let core_dir = core_dir(pid, &trusted)?;
        let launch = checked(&core_dir, &exe, args)?;
        Some(match spawn_no_activate(&launch.exe, &launch.args) {
            Ok(pid) => {
                tracing::info!(app = %launch.app, pid, "started a suite app without focus for an agent");
                format!("Started {} in the background.", launch.app)
            }
            Err(e) => {
                tracing::warn!(app = %launch.app, error = %e, "a suite app could not be started");
                format!("Could not start {} in the background: {e}.", launch.app)
            }
        })
    })
    .await
    .ok()
    .flatten()
}

/// The `launch_command` of a result with `running: false`.
fn launch_command(structured: &serde_json::Value) -> Option<(PathBuf, Vec<String>)> {
    if structured["running"] != false {
        return None;
    }
    let command = &structured["launch_command"];
    let exe = PathBuf::from(command["exe"].as_str()?);
    let args = command["args"]
        .as_array()?
        .iter()
        .map(|a| a.as_str().map(str::to_string))
        .collect::<Option<Vec<String>>>()?;
    Some((exe, args))
}

/// The folder of the core process `pid`, when it is one of the folders this client trusts.
fn core_dir(pid: u32, trusted: &[PathBuf]) -> Option<PathBuf> {
    let image = match SystemInspector.inspect(pid) {
        Ok(info) => info.image,
        Err(e) => {
            tracing::warn!(pid, error = %e, "the core's process can't be read; nothing is started");
            return None;
        }
    };
    let dir = image.parent()?.to_path_buf();
    let key = folder_key(&dir);
    if trusted.iter().any(|t| folder_key(t) == key) {
        Some(dir)
    } else {
        tracing::warn!(dir = %dir.display(), "the core runs from a folder this client doesn't trust; nothing is started");
        None
    }
}

/// `exe` and `args` as a [`Launch`], when `exe` is an app registered in `<core_dir>\apps\`.
pub fn checked(core_dir: &Path, exe: &Path, mut args: Vec<String>) -> Option<Launch> {
    let wanted = folder_key(exe);
    let app = installed_apps(core_dir)
        .into_iter()
        .find(|(_, registered)| folder_key(registered) == wanted)
        .map(|(app, _)| app)?;
    let at = args.len().saturating_sub(1);
    args.insert(at, NO_ACTIVATE.to_string());
    Some(Launch {
        app,
        exe: exe.to_path_buf(),
        args,
    })
}

/// The `(app, exe)` of each `apps\*.toml` in `core_dir` that parses; a relative `exe` is beside
/// the core, as the core reads it.
fn installed_apps(core_dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(core_dir.join("apps")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
        })
        .filter_map(|file| {
            let table: toml::Table = std::fs::read_to_string(&file).ok()?.parse().ok()?;
            let app = table.get("app")?.as_str()?.to_string();
            let exe = PathBuf::from(table.get("exe")?.as_str()?);
            let exe = if exe.is_absolute() {
                exe
            } else {
                core_dir.join(exe)
            };
            Some((app, exe))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-015
    fn only_an_exe_registered_beside_the_core_is_launched() {
        let env = lil_testkit::TestEnv::new();
        let dir = env.root().join("bin");
        let apps = dir.join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("image.toml"),
            "app = \"image\"\nexe = 'lil-image.exe'\n",
        )
        .unwrap();
        std::fs::write(apps.join("broken.toml"), "app = [").unwrap();
        let url = "lil-test-x:open/YQ".to_string();
        let args = vec!["--intent".to_string(), "open".to_string(), url.clone()];

        let launch = checked(&dir, &dir.join("LIL-IMAGE.EXE"), args.clone()).unwrap();
        assert_eq!(launch.app, "image");
        assert_eq!(
            launch.args,
            ["--intent", "open", "--no-activate", url.as_str()]
        );
        assert!(checked(&dir, &dir.join("other.exe"), args.clone()).is_none());
        assert!(checked(&dir.join("nowhere"), &dir.join("lil-image.exe"), args).is_none());
    }

    #[test] // spec: ARCH-015
    fn only_a_stopped_app_with_a_launch_command_is_started() {
        let stopped = serde_json::json!({"running": false, "pid": null,
            "launch_command": {"exe": r"C:\x\lil-image.exe", "args": ["--intent", "open", "u"]}});
        let (exe, args) = launch_command(&stopped).unwrap();
        assert_eq!(exe, PathBuf::from(r"C:\x\lil-image.exe"));
        assert_eq!(args, ["--intent", "open", "u"]);
        let running = serde_json::json!({"running": true, "pid": 5, "launch_command": null});
        assert!(launch_command(&running).is_none());
        let odd =
            serde_json::json!({"running": false, "launch_command": {"exe": "x", "args": [1]}});
        assert!(launch_command(&odd).is_none());
    }
}
