//! `lil confirm-window [<id>]` (ARCH-016, APP-049; Appendix G "The `lil` commands this plan
//! adds", P4a-R4): starts the channel's `lil-image.exe` from `lil.exe`'s folder with
//! `--confirm [<id>]`, which opens the minimal confirmation window on that confirmation, or on
//! every pending one without an id. It is for a person at a terminal: with `--non-interactive`
//! it refuses. The app is started detached, inheriting no handle of `lil`'s, and shown as
//! Windows shows any program a person starts.

use std::path::Path;

use lil_platform::process::{SpawnRequest, current_exe_dir, spawn_detached};

use super::{EXIT_OK, Exit, GlobalArgs, cannot_run, print_line};

/// The app beside `lil.exe` that shows the window.
const APP_EXE: &str = "lil-image.exe";

/// `lil confirm-window [<id>]`.
pub(super) fn run(global: &GlobalArgs, id: Option<&str>) -> Result<i32, Exit> {
    if global.non_interactive {
        return Err(cannot_run("the confirm window is for interactive use"));
    }
    if let Some(id) = id
        && !is_confirmation_id(id)
    {
        return Err(cannot_run(format!(
            "{id} isn't a confirmation id (cf_ and 16 lowercase hex digits)"
        )));
    }
    let dir = current_exe_dir()
        .map_err(|e| cannot_run(format!("Cannot find the folder lil runs from: {e}")))?;
    let app = dir.join(APP_EXE);
    if !app.is_file() {
        return Err(cannot_run("lil-image.exe isn't beside lil.exe"));
    }
    let pid = start(&app, id).map_err(|e| {
        cannot_run(format!(
            "lil-image.exe couldn't be started ({}): {e}",
            app.display()
        ))
    })?;
    let _ = print_line(&format!("confirm window started (pid {pid})"));
    Ok(EXIT_OK)
}

/// `^cf_[0-9a-f]{16}$` (Appendix G "`lil:` URLs").
fn is_confirmation_id(id: &str) -> bool {
    id.strip_prefix("cf_").is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Starts `app --confirm [<id>]`; its pid.
fn start(app: &Path, id: Option<&str>) -> std::io::Result<u32> {
    let mut args = vec!["--confirm".into()];
    args.extend(id.map(Into::into));
    let child = spawn_detached(&SpawnRequest {
        exe: app.to_path_buf(),
        args,
        extra_env: Vec::new(),
        remove_env: Vec::new(),
    })?;
    Ok(child.pid())
}
