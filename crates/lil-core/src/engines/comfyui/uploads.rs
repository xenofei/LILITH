//! A job's inputs in ComfyUI (Appendix F "ComfyUI client", CORE-130, IMG-095): each file is
//! uploaded once through `/upload/image` as `lil_<first 16 hex of its SHA-256>.<ext>`,
//! reference-counted in the suite `state.db` (`comfy_uploads`): a job takes a reference
//! before its prompt is posted and gives it back when its attempt ends. An upload whose count
//! has been 0 for [`UPLOAD_KEEP`] (24 h) is removed from `<root>\input`, when `root` is known,
//! by the supervisor's pass ([`Uploads::sweep`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use lil_platform::Clock;
use rusqlite::{OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

use super::client::{ComfyClient, ComfyError, hex};
use crate::state::{StateDb, StateError};
use crate::store::fsops;

/// How long an upload no job references stays in `<root>\input`.
pub const UPLOAD_KEEP: Duration = Duration::from_secs(24 * 60 * 60);
/// The largest input file a job uploads.
pub const MAX_UPLOAD_BYTES: u64 = 256 * 1024 * 1024;

/// The upload name of a file with `sha256` and the extension of `src` (lower case; `png`
/// when it has none): `lil_<first 16 hex>.<ext>`.
pub fn upload_name(sha256: &str, src: &Path) -> String {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map_or_else(|| "png".to_string(), str::to_ascii_lowercase);
    format!("lil_{}.{ext}", &sha256[..16.min(sha256.len())])
}

/// The reference-counted uploads (module docs).
#[derive(Debug, Clone)]
pub struct Uploads {
    db: Arc<StateDb>,
    clock: Arc<dyn Clock>,
}

fn state_err(e: &rusqlite::Error) -> StateError {
    StateError::Io(e.to_string())
}

fn io(e: &StateError) -> ComfyError {
    ComfyError::Io(format!("the uploads table: {e}"))
}

impl Uploads {
    /// The uploads kept in `db`, the suite `state.db`.
    pub fn new(db: Arc<StateDb>, clock: Arc<dyn Clock>) -> Uploads {
        Uploads { db, clock }
    }

    fn now(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    /// Takes a reference to the file `src` in ComfyUI and returns its upload name. It is read
    /// once (at most [`MAX_UPLOAD_BYTES`]); the bytes hashed are the bytes sent. It is
    /// uploaded only when no row has it yet, or (with `root` known) when it is missing from
    /// `<root>\input`. `src` is checked by the caller (the handler confines it).
    ///
    /// # Errors
    /// [`ComfyError`] of the read or the upload ([`ComfyError::Io`] for a file that can't be
    /// read or is too large); no reference is taken then.
    pub async fn acquire(
        &self,
        client: &ComfyClient,
        src: &Path,
        root: Option<&Path>,
    ) -> Result<String, ComfyError> {
        let path = src.to_path_buf();
        let (bytes, sha256) = tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            let file = std::fs::File::open(lil_platform::fs::long_path(&path))
                .map_err(|e| ComfyError::Io(format!("the input can't be read: {e}")))?;
            let mut bytes = Vec::new();
            file.take(MAX_UPLOAD_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| ComfyError::Io(format!("the input can't be read: {e}")))?;
            if bytes.len() as u64 > MAX_UPLOAD_BYTES {
                return Err(ComfyError::Io(format!(
                    "the input is larger than {MAX_UPLOAD_BYTES} bytes"
                )));
            }
            let sha256 = hex(&Sha256::digest(&bytes));
            Ok((bytes, sha256))
        })
        .await
        .map_err(|e| ComfyError::Io(e.to_string()))??;
        let name = upload_name(&sha256, src);
        let now = self.now();
        let key = name.clone();
        let taken = self
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE comfy_uploads SET refs = refs + 1, last_used_utc = ?2 WHERE name = ?1",
                    params![key, now],
                )
                .map(|n| n == 1)
                .map_err(|e| state_err(&e))
            })
            .await
            .map_err(|e| io(&e))?;
        let present = root.is_none_or(|root| root.join("input").join(&name).is_file());
        if taken && present {
            return Ok(name);
        }
        if let Err(e) = client.upload_bytes(&name, &bytes).await {
            if taken {
                self.release(&name).await;
            }
            return Err(e);
        }
        if !taken {
            let (key, hash) = (name.clone(), sha256);
            self.db
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO comfy_uploads (name, sha256, refs, last_used_utc)
                         VALUES (?1, ?2, 1, ?3)
                         ON CONFLICT (name) DO UPDATE SET refs = refs + 1, last_used_utc = ?3",
                        params![key, hash, now],
                    )
                    .map(drop)
                    .map_err(|e| state_err(&e))
                })
                .await
                .map_err(|e| io(&e))?;
        }
        Ok(name)
    }

    /// At the core's start, before any job runs: the references an earlier core took and never
    /// gave back (it stopped first) are dropped, and the 24 h start now (review M3).
    pub async fn reset(&self) {
        let now = self.now();
        let done = self
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE comfy_uploads SET refs = 0, last_used_utc = ?1 WHERE refs > 0",
                    params![now],
                )
                .map(drop)
                .map_err(|e| state_err(&e))
            })
            .await;
        if let Err(e) = done {
            tracing::warn!(error = %e, "the uploads' references could not be reset");
        }
    }

    /// Gives a reference back (never below 0); the 24 h start when the count reaches 0.
    pub async fn release(&self, name: &str) {
        let (key, now) = (name.to_string(), self.now());
        let done = self
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE comfy_uploads SET refs = MAX(refs - 1, 0), last_used_utc = ?2
                     WHERE name = ?1",
                    params![key, now],
                )
                .map(drop)
                .map_err(|e| state_err(&e))
            })
            .await;
        if let Err(e) = done {
            tracing::warn!(upload = name, error = %e, "an upload's reference could not be given back");
        }
    }

    /// The reference count of `name`; `None` when there is no such upload.
    pub async fn refs(&self, name: &str) -> Option<u32> {
        let key = name.to_string();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT refs FROM comfy_uploads WHERE name = ?1",
                    params![key],
                    |r| r.get::<_, u32>(0),
                )
                .optional()
                .map_err(|e| state_err(&e))
            })
            .await
            .ok()
            .flatten()
    }

    /// Removes from `<root>\input` (and from the table) every upload whose count has been 0
    /// for [`UPLOAD_KEEP`]; returns their names. A file that is already gone is fine; one that
    /// can't be removed keeps its row, so a later pass tries again.
    pub async fn sweep(&self, root: &Path) -> Vec<String> {
        let keep = i64::try_from(UPLOAD_KEEP.as_secs()).unwrap_or(i64::MAX);
        let cutoff = self.now().saturating_sub(keep);
        let input: PathBuf = root.join("input");
        let root = root.to_path_buf();
        let swept = self
            .db
            .call(move |conn| {
                let names: Vec<String> = conn
                    .prepare(
                        "SELECT name FROM comfy_uploads WHERE refs = 0 AND last_used_utc <= ?1",
                    )
                    .and_then(|mut st| {
                        st.query_map(params![cutoff], |r| r.get(0))?
                            .collect::<Result<_, _>>()
                    })
                    .map_err(|e| state_err(&e))?;
                let mut removed = Vec::new();
                for name in names {
                    // On the actor, so no acquire takes a reference between the file's removal
                    // and its row's.
                    match fsops::remove_engine_file(&root, &input.join(&name)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            tracing::warn!(upload = %name, error = %e, "an unused upload could not be removed");
                            continue;
                        }
                    }
                    conn.execute(
                        "DELETE FROM comfy_uploads WHERE name = ?1 AND refs = 0",
                        params![name],
                    )
                    .map_err(|e| state_err(&e))?;
                    removed.push(name);
                }
                Ok(removed)
            })
            .await;
        match swept {
            Ok(removed) => removed,
            Err(e) => {
                tracing::warn!(error = %e, "the unused uploads could not be swept");
                Vec::new()
            }
        }
    }
}
