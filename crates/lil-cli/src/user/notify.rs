//! `lil notify <path>…` (ARCH-016, ARCH-073): tells the core's watcher that files changed, for
//! folders it can't watch or changes it may have missed. It maps each path (made absolute
//! against the current folder) to the session workspace's relative path, comparing folders as
//! `folder_key` does, and calls `watch.notify` with them.
//!
//! Output (Appendix E, "The `lil` commands this plan adds"): nothing on stdout on success. A
//! path outside the workspace, or one the watcher doesn't watch, is named on stderr and left
//! alone. With `--no-start` and no core running it is a silent no-op that exits 0 (ARCH-018).
//! An error result prints its message on stderr and exits 1.

use std::path::{Path, PathBuf};

use lil_platform::paths::relative_inside;
use lil_protocol::Outcome;
use serde_json::{Map, Value};

use super::{
    EXIT_ERROR_RESULT, EXIT_OK, EXIT_PENDING, Exit, GlobalArgs, UnknownCwdWorkspace, cannot_run,
    connect,
};

/// `lil notify`.
pub(super) fn run(global: &GlobalArgs, paths: &[PathBuf]) -> Result<i32, Exit> {
    // Made absolute before any core is involved: a bad path never starts one.
    let absolute: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            std::path::absolute(p)
                .map_err(|e| cannot_run(format!("Cannot use the path {}: {e}", p.display())))
        })
        .collect::<Result<_, _>>()?;
    let Some(client) = connect(global, UnknownCwdWorkspace::Fail)? else {
        return Ok(EXIT_OK);
    };
    let Some(ws) = client.hello().workspace.as_ref() else {
        return Err(cannot_run(
            "lil notify needs a workspace: connect with one (`lil workspace use <path>` sets the default).",
        ));
    };
    let root = PathBuf::from(&ws.path);
    let (inside, outside) = split(&root, &absolute);
    for path in outside {
        eprintln!(
            "{} is not inside the workspace {}; it was left alone.",
            path.display(),
            root.display()
        );
    }
    if inside.is_empty() {
        return Ok(EXIT_OK);
    }
    let mut args = Map::new();
    args.insert(
        "paths".into(),
        Value::Array(inside.into_iter().map(Value::from).collect()),
    );
    Ok(
        match client
            .call_tool("watch.notify", args, None)
            .map_err(cannot_run)?
        {
            Outcome::Ok(output) => {
                for ignored in output.structured["ignored"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if let Some(ignored) = ignored.as_str() {
                        eprintln!(
                            "{ignored} is not watched (a private folder of the workspace); it was left alone."
                        );
                    }
                }
                EXIT_OK
            }
            Outcome::Error { error } => {
                eprintln!("{}", error.message);
                EXIT_ERROR_RESULT
            }
            Outcome::PendingConfirmation(_) => {
                eprintln!("watch.notify is waiting for a confirmation in the Worldwright app.");
                EXIT_PENDING
            }
        },
    )
}

/// The workspace-relative form (`/` separated) of each path inside `root`, and the paths
/// outside it.
fn split<'a>(root: &Path, paths: &'a [PathBuf]) -> (Vec<String>, Vec<&'a PathBuf>) {
    let mut inside = Vec::new();
    let mut outside = Vec::new();
    for path in paths {
        match relative_inside(root, path) {
            Some(rel) => inside.push(rel),
            None => outside.push(path),
        }
    }
    (inside, outside)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-016, ARCH-073
    fn paths_inside_the_workspace_become_relative() {
        let dir = tempfile::Builder::new()
            .prefix("lil-notify-Ünïcödé space-")
            .tempdir()
            .unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("notes")).unwrap();
        std::fs::write(ws.join("notes").join("a.txt"), b"a").unwrap();
        let paths = vec![
            ws.join("notes").join("a.txt"),
            ws.join("notes").join("gone.txt"),
            dir.path().join("elsewhere.txt"),
        ];
        let (inside, outside) = split(&ws, &paths);
        assert_eq!(inside, ["notes/a.txt", "notes/gone.txt"]);
        assert_eq!(outside, [&paths[2]]);
    }
}
