//! Where a `comfy.prompt` job's outputs land (Appendix F "ComfyUI client", ARCH-080): a staging
//! handle of the job's workspace, or `%LOCALAPPDATA%\LILITH\cache\jobs\<job_id>\` without
//! one; and the salvage of a lost prompt's outputs by the job's filename prefix (CORE-128,
//! QA-009). A ComfyUI-side copy is removed (through `fsops::remove_engine_file`, only below
//! `<root>\output`) only once its staged copy has been verified: the same size, and a SHA-256
//! read back from the staged file equal to the one taken from the source.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use lil_protocol::{ErrorBody, FileRef, codes};
use sha2::{Digest as _, Sha256};

use super::client::hex;
use crate::jobs::JobCtx;
use crate::store::{STAGING_DIR, fsops};

/// The folder a job's outputs are staged in.
#[derive(Debug, Clone)]
pub(crate) struct Staging {
    pub(crate) dir: PathBuf,
    /// `.lilith/staging/<handle>` for a workspace handle (artifact paths are workspace
    /// relative); `None` for the job cache (absolute paths).
    rel: Option<String>,
}

impl Staging {
    /// A new staging handle in the job's workspace, or the job's cache folder under
    /// `local_dir`.
    ///
    /// # Errors
    /// The store's (`workspace.read_only`, `store.io`, `store.disk_full`), or `store.io` for
    /// a cache folder that can't be made.
    pub(crate) async fn for_job(job: &JobCtx, local_dir: &Path) -> Result<Staging, ErrorBody> {
        match &job.workspace {
            Some(open) => {
                let handle = open
                    .store
                    .staging_handle()
                    .await
                    .map_err(|e| e.to_error(&job.trace_id))?;
                Ok(Staging {
                    rel: Some(format!("{STAGING_DIR}/{}", handle.id)),
                    dir: handle.dir,
                })
            }
            None => {
                let dir = local_dir.join("cache").join("jobs").join(job.id.as_str());
                let made = dir.clone();
                tokio::task::spawn_blocking(move || fsops::ensure_dir(&made))
                    .await
                    .map_err(std::io::Error::other)
                    .and_then(|r| r)
                    .map_err(|e| {
                        ErrorBody::new(
                            &codes::STORE_IO,
                            format!(
                                "The job's output folder {} can't be made: {e}",
                                dir.display()
                            ),
                            job.trace_id.clone(),
                        )
                    })?;
                Ok(Staging { dir, rel: None })
            }
        }
    }

    /// The artifact of the staged file `name`.
    pub(crate) fn file_ref(&self, name: &str, sha256: &str) -> FileRef {
        let path = match &self.rel {
            Some(rel) => format!("{rel}/{name}"),
            None => self.dir.join(name).display().to_string(),
        };
        FileRef {
            path,
            sha256: Some(sha256.to_string()),
        }
    }
}

/// `(size, sha256)` of the file at `path`, read from disk.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut file = std::fs::File::open(lil_platform::fs::long_path(path))?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, hex(&hash.finalize())))
}

/// Copies every file directly in `<root>\output` whose name starts with `prefix` into
/// `staging`, verifies each copy against its source, and removes the source once it matches.
/// Returns the staged files; one that can't be copied or doesn't match is left where it is
/// (and its staged copy removed).
pub(crate) async fn salvage(root: &Path, prefix: &str, staging: &Staging) -> Vec<FileRef> {
    let (root, prefix, dir) = (root.to_path_buf(), prefix.to_string(), staging.dir.clone());
    let found = tokio::task::spawn_blocking(move || salvage_blocking(&root, &prefix, &dir))
        .await
        .unwrap_or_default();
    found
        .into_iter()
        .map(|(name, sha256)| staging.file_ref(&name, &sha256))
        .collect()
}

/// The names (lower-cased) already in the staging folder `dir`: a job's cache folder is
/// reused when a later core run takes the job up (ruling P3-R21), and a copy a dead run left
/// there keeps its name. None when it can't be listed.
pub(crate) fn names_in(dir: &Path) -> HashSet<String> {
    std::fs::read_dir(lil_platform::fs::long_path(dir))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// `name`, or `<n>_<name>` with the first `n` from 2 that `taken` doesn't hold (names compare
/// without case, as Windows does); the name returned is added to `taken`.
pub(crate) fn free_name(taken: &mut HashSet<String>, name: &str) -> String {
    let mut chosen = name.to_string();
    let mut n = 1;
    while !taken.insert(chosen.to_ascii_lowercase()) {
        n += 1;
        chosen = format!("{n}_{name}");
    }
    chosen
}

fn salvage_blocking(root: &Path, prefix: &str, dir: &Path) -> Vec<(String, String)> {
    let output = root.join("output");
    let Ok(entries) = std::fs::read_dir(lil_platform::fs::long_path(&output)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix))
        .collect();
    names.sort();
    // A dead run's copy in a reused cache folder stays; the salvaged one goes beside it.
    let mut taken = names_in(dir);
    let mut salvaged = Vec::new();
    for name in names {
        let staged = free_name(&mut taken, &name);
        let (src, dst) = (output.join(&name), dir.join(&staged));
        if let Err(e) = fsops::copy_new(&src, &dst) {
            tracing::warn!(file = %src.display(), error = %e, "an output could not be salvaged");
            continue;
        }
        match (sha256_file(&src), sha256_file(&dst)) {
            (Ok(from), Ok(to)) if from == to => {
                if let Err(e) = fsops::remove_engine_file(root, &src) {
                    tracing::warn!(file = %src.display(), error = %e, "a salvaged output's ComfyUI copy could not be removed");
                }
                salvaged.push((staged, to.1));
            }
            _ => {
                tracing::warn!(file = %src.display(), "a salvaged output's copy doesn't match; it stays in ComfyUI's folder");
                let _ = fsops::discard_unverified(dir, &dst);
            }
        }
    }
    salvaged
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A salvage into a reused job cache folder (a P3-R21 take-up) puts the copy beside the
    /// dead run's (`2_<name>`), leaves that copy as it was, and removes ComfyUI's verified
    /// source.
    #[test] // spec: QA-009, CORE-128
    fn salvage_goes_beside_a_dead_runs_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, dir) = (tmp.path().join("comfy"), tmp.path().join("cache"));
        let name = "lil_0123456789abcdef_00001_.png";
        for (at, bytes) in [
            (root.join("output").join(name), b"engine".as_slice()),
            (dir.join(name), b"dead run".as_slice()),
        ] {
            fsops::ensure_dir(at.parent().unwrap()).unwrap();
            fsops::write_file_atomic(&at, bytes).unwrap();
        }
        let got = salvage_blocking(&root, "lil_0123456789abcdef_", &dir);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, format!("2_{name}"));
        assert_eq!(std::fs::read(dir.join(name)).unwrap(), b"dead run");
        assert_eq!(
            std::fs::read(dir.join(format!("2_{name}"))).unwrap(),
            b"engine"
        );
        assert!(!root.join("output").join(name).exists());
    }
}
