//! The append logs (ARCH-081, CORE-009; Appendix E "Append logs"):
//! `queue/jobs.<machine>.<yyyy-mm>.jsonl`, `registry/usage.<machine>.<yyyy-mm>.jsonl` and the
//! loose usage log `%LOCALAPPDATA%\LILITH\usage-loose.jsonl`.
//!
//! Each line is one write under the append lock of the log file itself (`LockFileEx` on
//! `[2⁶⁴−2, +1)`, which `lil` can take too), requested with `LOCKFILE_FAIL_IMMEDIATELY` by
//! `fsops::append_locked` on a blocking thread. While another holder has it, the store polls
//! with `tokio::time::sleep` from 10 ms doubling to 500 ms, 10 s in all, then `store.busy`
//! (ruling R13). With the lock held the file is read, a torn last line is closed with `\n`,
//! and the line's `seq` is the last `seq` of this machine in that text plus one, so two
//! appenders never write the same `(machine, seq)`. Readers (`lil_schema::logs::read_lines`)
//! skip lines that don't parse and dedupe by `(machine, seq)`.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;

use lil_platform::Env;
use lil_platform::fs::{RetryClass, classify};
use lil_schema::logs::{encode_line, last_seq};
use serde_json::{Map, Value};

use super::fsops;
use super::paths::WsPath;
use super::write::{Backoff, Store, StoreError, blocking};

/// Which workspace log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    /// `queue/jobs.<machine>.<yyyy-mm>.jsonl`.
    Jobs,
    /// `registry/usage.<machine>.<yyyy-mm>.jsonl`.
    Usage,
}

impl LogKind {
    /// The workspace-relative log of `machine` for the UTC month `yyyy_mm`.
    pub fn rel(self, machine: &str, yyyy_mm: &str) -> String {
        match self {
            LogKind::Jobs => format!("queue/jobs.{machine}.{yyyy_mm}.jsonl"),
            LogKind::Usage => format!("registry/usage.{machine}.{yyyy_mm}.jsonl"),
        }
    }
}

impl Store {
    /// Appends `record` as one line of this machine's `kind` log for the current UTC month
    /// (`{"machine","seq",…record}`); returns its `seq`.
    ///
    /// # Errors
    /// `workspace.read_only` on a read-only store; `store.busy` when another holder keeps the
    /// lock past the 10 s budget; `store.io` or `store.disk_full`.
    pub async fn append_log(
        &self,
        kind: LogKind,
        record: Map<String, Value>,
    ) -> Result<u64, StoreError> {
        self.append_log_at(kind, record).await.map(|(seq, _)| seq)
    }

    /// [`Store::append_log`], with the log's path.
    pub(super) async fn append_log_at(
        &self,
        kind: LogKind,
        record: Map<String, Value>,
    ) -> Result<(u64, WsPath), StoreError> {
        self.writable_store()?;
        let month = lil_schema::time::year_month(self.now_utc());
        let rel = WsPath::parse(&kind.rel(self.machine(), &month))?;
        let seq = append_line(
            rel.abs(self.root()),
            self.machine().to_string(),
            record,
            rel.clone(),
        )
        .await?;
        Ok((seq, rel))
    }
}

/// Appends `record` to the loose usage log `Env::local_dir()\usage-loose.jsonl` (usage that
/// belongs to no workspace, ARCH-081), as `env.machine_id()`; returns its `seq`.
///
/// # Errors
/// As [`Store::append_log`].
pub async fn append_loose_usage(env: &Env, record: Map<String, Value>) -> Result<u64, StoreError> {
    let path = env.local_dir().join("usage-loose.jsonl");
    let shown = WsPath::from_disk(path.display().to_string());
    append_line(path, env.machine_id(), record, shown).await
}

/// One locked append of `record` to `path` as `machine`, polled under the retry budget.
async fn append_line(
    path: PathBuf,
    machine: String,
    record: Map<String, Value>,
    shown: WsPath,
) -> Result<u64, StoreError> {
    let (path, machine, record) = (Arc::new(path), Arc::new(machine), Arc::new(record));
    let mut backoff = Backoff::new();
    loop {
        let (path, machine, record) = (path.clone(), machine.clone(), record.clone());
        let attempt = blocking(move || {
            if let Some(dir) = path.parent() {
                fsops::ensure_dir(dir)?;
            }
            let seq = Cell::new(0_u64);
            let line = fsops::append_locked(&path, &|text| {
                let next = last_seq(text, &machine) + 1;
                seq.set(next);
                encode_line(&machine, next, &record)
            })?;
            Ok(line.map(|_| seq.get()))
        })
        .await;
        let held = match attempt {
            Ok(Some(seq)) => return Ok(seq),
            Ok(None) => true,
            Err(e) if classify(&e) == RetryClass::SharingViolation => true,
            Err(e) => return Err(StoreError::io(&shown, &e)),
        };
        if held && !backoff.wait().await {
            tracing::warn!(log = %shown, "the append lock stayed held past the retry budget");
            return Err(StoreError::Busy(shown));
        }
    }
}
