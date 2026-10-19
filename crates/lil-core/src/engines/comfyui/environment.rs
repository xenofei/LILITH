//! Environment capture (Appendix F "ComfyUI client"; CORE-130): `/system_stats`, the NVML
//! driver version, and the revision of ComfyUI and of each `custom_nodes\*` folder: its git
//! commit (`HEAD`, as `git rev-parse HEAD` gives it) and its `pyproject.toml` `[project]
//! version`.
//!
//! The commit is read from the repository's files, not by running git: `.git` (a folder, or a
//! file `gitdir: <path>` as a worktree or submodule has), its `HEAD` (a commit id, or `ref:
//! <name>`), then that ref's loose file or its `packed-refs` line. A repository it can't read
//! has no commit; nothing here fails the capture.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// What the environment was when the engine became ready.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvCapture {
    pub system_stats: Value,
    /// The NVML driver version, when there is a GPU reading.
    pub driver: Option<String>,
    /// ComfyUI's commit, else its `pyproject.toml` version.
    pub comfyui: Option<String>,
    /// `(folder name, git commit, pyproject version)` of each `custom_nodes\*` folder, sorted
    /// by name.
    #[serde(serialize_with = "nodes_as_objects")]
    pub nodes: Vec<(String, Option<String>, Option<String>)>,
    /// RFC 3339 UTC.
    pub captured: String,
}

fn nodes_as_objects<S: serde::Serializer>(
    nodes: &[(String, Option<String>, Option<String>)],
    s: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq as _;
    let mut seq = s.serialize_seq(Some(nodes.len()))?;
    for (name, git_rev, version) in nodes {
        seq.serialize_element(&serde_json::json!({
            "name": name, "git_rev": git_rev, "version": version,
        }))?;
    }
    seq.end()
}

/// The capture for an engine whose folder is `root` (none known: no revisions). Reads files
/// only: call it off the runtime's threads.
pub fn capture(
    mut system_stats: Value,
    driver: Option<String>,
    root: Option<&Path>,
    captured: String,
) -> EnvCapture {
    if let Some(argv) = system_stats
        .get_mut("system")
        .and_then(|s| s.get_mut("argv"))
        .and_then(Value::as_array_mut)
    {
        blank_secret_args(argv);
    }
    let comfyui = root.and_then(|r| {
        let (rev, version) = revision(r);
        rev.or(version)
    });
    let mut nodes: Vec<(String, Option<String>, Option<String>)> = root
        .and_then(|r| std::fs::read_dir(r.join("custom_nodes")).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let (rev, version) = revision(&e.path());
            (e.file_name().to_string_lossy().into_owned(), rev, version)
        })
        .filter(|(name, _, _)| name != "__pycache__")
        .collect();
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    EnvCapture {
        system_stats,
        driver,
        comfyui,
        nodes,
        captured,
    }
}

/// The value of every flag whose name looks like a secret's (the scrubbed environment's
/// words, ruling P3-R19: `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`,
/// `AUTH`) becomes `"…"`, in either form (`--api-key x`, `--api-key=x`): a launcher may pass
/// one, and the capture lands in a job's result.
pub fn blank_secret_args(argv: &mut [Value]) {
    const WORDS: [&str; 7] = [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ];
    let secret = |flag: &str| {
        let name = flag.trim_start_matches('-').to_ascii_uppercase();
        flag.starts_with('-') && WORDS.iter().any(|w| name.contains(w))
    };
    let mut blank_next = false;
    for arg in argv.iter_mut() {
        let Some(text) = arg.as_str().map(str::to_string) else {
            blank_next = false;
            continue;
        };
        // A flag that takes no value is followed by another flag, which is kept.
        if blank_next && !text.starts_with('-') {
            *arg = Value::from("…");
            blank_next = false;
            continue;
        }
        blank_next = false;
        match text.split_once('=') {
            Some((flag, _)) if secret(flag) => *arg = Value::from(format!("{flag}=…")),
            None if secret(&text) => blank_next = true,
            _ => {}
        }
    }
}

/// `(git commit, pyproject version)` of the folder `dir`.
pub fn revision(dir: &Path) -> (Option<String>, Option<String>) {
    let version = std::fs::read_to_string(dir.join("pyproject.toml"))
        .ok()
        .and_then(|t| lil_schema::engine_files::pyproject_version(&t));
    (git_head(dir), version)
}

/// Whether `s` is a full commit id (40 hex, or 64 for SHA-256 repositories).
fn is_commit(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit `HEAD` names in the repository at `dir` (module docs).
fn git_head(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if target.is_absolute() {
            target
        } else {
            dir.join(target)
        }
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(name) = head.strip_prefix("ref:").map(str::trim) else {
        return is_commit(head).then(|| head.to_ascii_lowercase());
    };
    // A worktree keeps its refs in the main repository (`commondir`).
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| git_dir.join(c.trim()))
        .unwrap_or_else(|| git_dir.clone());
    for base in [&git_dir, &common] {
        if let Ok(id) = std::fs::read_to_string(base.join(name)) {
            let id = id.trim();
            if is_commit(id) {
                return Some(id.to_ascii_lowercase());
            }
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (id, r) = line.split_once(' ')?;
        (r.trim() == name && is_commit(id)).then(|| id.to_ascii_lowercase())
    })
}
