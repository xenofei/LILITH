//! `lil test --real [--record <dir>]` (QA-063, Appendix F): the on-demand real-engine run.
//!
//! It runs `cargo nextest run --workspace --run-ignored only -E 'test(/^real_/)'` from the
//! source checkout this `lil` was built from, with `LIL_REAL_COMFYUI` set to
//! `http://127.0.0.1:<port>`, the port from `[engines.comfyui]` in the channel's `suite.toml`
//! (default 8188). With `--record <dir>` it also sets `LIL_REAL_RECORD` to that folder (read
//! from the current directory), where `real_comfyui_behaviours_match_the_fake` records the
//! ComfyUI shape fixtures (QA-067). The `real_*` tests attach to the ComfyUI that is running;
//! nothing here starts, stops or installs anything. The run builds into its own target folder,
//! `target\real` (`CARGO_TARGET_DIR`), because Windows can't replace the `lil.exe` that runs it
//! (`cargo run -p lil-cli -- test --real` runs `target\debug\lil.exe`).
//!
//! Outside the checkout (or in another repository) it says so and exits 2. Otherwise its exit
//! code is 0 when every test passed, 1 when a test failed, and 2 when nextest couldn't run.

use std::path::{Path, PathBuf};
use std::process::Command;

use lil_platform::Env;

use super::{EXIT_CANNOT_RUN, EXIT_ERROR_RESULT, EXIT_OK, Exit, cannot_run};

/// The URL of the ComfyUI the real tests attach to.
pub const ENV_REAL_COMFYUI: &str = "LIL_REAL_COMFYUI";
/// The folder the shape fixtures are recorded into, when asked.
pub const ENV_REAL_RECORD: &str = "LIL_REAL_RECORD";
/// ComfyUI's default port (ARCH-070).
const DEFAULT_PORT: u16 = 8188;

/// The target folder of the run, under the checkout's `target`.
const TARGET_DIR: [&str; 2] = ["target", "real"];

/// The nextest filter of the real tests.
const FILTER: &str = "test(/^real_/)";

/// Runs the command (module docs).
pub fn run(record: Option<&Path>) -> Result<i32, Exit> {
    let root = crate::dev::root::source_root().map_err(|why| {
        cannot_run(format!(
            "lil test --real runs from the source checkout it was built from: {why}"
        ))
    })?;
    let env = Env::from_process()
        .map_err(|e| cannot_run(format!("Cannot tell which LILITH folders to use: {e}")))?;
    let text = match std::fs::read_to_string(env.suite_toml_to_read()) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(cannot_run(format!("suite.toml can't be read: {e}"))),
    };
    let url = comfyui_url(&text).map_err(cannot_run)?;
    let record = match record {
        Some(dir) => Some(absolute(dir).map_err(cannot_run)?),
        None => None,
    };
    let mut cmd = command(&root, &url, record.as_deref());
    eprintln!(
        "Running the real-engine tests against the ComfyUI at {url}{}.",
        record
            .as_ref()
            .map(|d| format!(", recording into {}", d.display()))
            .unwrap_or_default()
    );
    let status = cmd
        .status()
        .map_err(|e| cannot_run(format!("cargo nextest could not be started: {e}")))?;
    Ok(match status.code() {
        Some(0) => EXIT_OK,
        Some(_) => EXIT_ERROR_RESULT,
        None => EXIT_CANNOT_RUN,
    })
}

/// `http://127.0.0.1:<port>` from `suite.toml`'s `[engines.comfyui] port` (default 8188).
fn comfyui_url(suite_toml: &str) -> Result<String, String> {
    let table: toml::Table = suite_toml
        .parse()
        .map_err(|e| format!("suite.toml can't be parsed: {e}"))?;
    let port = match table
        .get("engines")
        .and_then(|e| e.get("comfyui"))
        .and_then(|c| c.get("port"))
    {
        None => DEFAULT_PORT,
        Some(toml::Value::Integer(p)) => u16::try_from(*p)
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| format!("[engines.comfyui] port = {p} is not a port"))?,
        Some(other) => return Err(format!("[engines.comfyui] port = {other} is not a port")),
    };
    Ok(format!("http://127.0.0.1:{port}"))
}

/// `dir`, made absolute from the current directory.
fn absolute(dir: &Path) -> Result<PathBuf, String> {
    if dir.is_absolute() {
        return Ok(dir.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(dir))
        .map_err(|e| format!("couldn't read the current directory: {e}"))
}

/// The nextest run, in `root`, with the real tests' variables.
fn command(root: &Path, url: &str, record: Option<&Path>) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "nextest",
        "run",
        "--workspace",
        "--run-ignored",
        "only",
        "-E",
        FILTER,
    ])
    .current_dir(root)
    .env(
        "CARGO_TARGET_DIR",
        TARGET_DIR.iter().fold(root.to_path_buf(), |p, c| p.join(c)),
    )
    .env(ENV_REAL_COMFYUI, url)
    .env_remove(ENV_REAL_RECORD);
    if let Some(dir) = record {
        cmd.env(ENV_REAL_RECORD, dir);
    }
    crate::dev::root::scrub_git_env(&mut cmd);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-063, ARCH-070
    fn the_url_comes_from_the_configured_port() {
        assert_eq!(comfyui_url("").unwrap(), "http://127.0.0.1:8188");
        assert_eq!(
            comfyui_url("[engines.comfyui]\nport = 8190\n").unwrap(),
            "http://127.0.0.1:8190"
        );
        assert_eq!(
            comfyui_url("[engines.comfyui]\nroot = 'x'\n").unwrap(),
            "http://127.0.0.1:8188"
        );
        for bad in [
            "[engines.comfyui]\nport = 0\n",
            "[engines.comfyui]\nport = 70000\n",
            "[engines.comfyui]\nport = '8188'\n",
            "not toml = = 1",
        ] {
            assert!(comfyui_url(bad).is_err(), "{bad}");
        }
    }

    #[test] // spec: QA-063, QA-067
    fn the_run_is_the_real_filter_with_the_engine_and_record_variables() {
        let cmd = command(
            Path::new(r"C:\src"),
            "http://127.0.0.1:8190",
            Some(Path::new(r"C:\rec")),
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "nextest",
                "run",
                "--workspace",
                "--run-ignored",
                "only",
                "-E",
                "test(/^real_/)"
            ]
        );
        assert_eq!(cmd.get_current_dir(), Some(Path::new(r"C:\src")));
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&(
            ENV_REAL_COMFYUI.into(),
            Some("http://127.0.0.1:8190".into())
        )));
        assert!(envs.contains(&(ENV_REAL_RECORD.into(), Some(r"C:\rec".into()))));
        assert!(
            envs.contains(&(
                "CARGO_TARGET_DIR".into(),
                Some(r"C:\src\target\real".into())
            )),
            "never the target folder of the running lil.exe: {envs:?}"
        );
        let plain = command(Path::new(r"C:\src"), "http://127.0.0.1:8188", None);
        assert!(
            plain
                .get_envs()
                .any(|(k, v)| k == ENV_REAL_RECORD && v.is_none()),
            "an inherited LIL_REAL_RECORD is removed"
        );
    }
}
