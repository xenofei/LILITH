//! Document holds (APP-103–105).
//!
//! A hold is an entry in the core's memory: the first app to hold a document keeps it until
//! it drops the [`HoldGuard`] or its connection closes ([`Holds::release_connection`]);
//! everyone else, the core's own operations included, gets `doc.held_elsewhere` and may only
//! read. The core takes a [`HoldOwner::Core`] hold on a closed document for one operation and
//! drops the guard when the operation ends.
//!
//! A hold taken with [`Holds::acquire_marked`] also writes a `hold_markers` row to the
//! document's workspace `state.db`, removed again when the hold ends. The row is a record, not
//! the hold: a workspace database empties the table when a core opens it (APP-104), so a core
//! that was killed leaves no hold behind, and an app re-acquires its holds after a restart.
//! Marker writes are best effort and never fail a hold; a failed one is logged.
//!
//! One connection may take a document's hold again (a second window of the same app): the
//! hold lasts until every guard of it is dropped. Any other second taker is refused.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lil_platform::Clock;
use lil_protocol::{ErrorBody, codes};
use schemars::JsonSchema;
use serde::Serialize;

use crate::events::ClientId;
use crate::state::{HoldMarker, StateDb};
use crate::transport::ConnObserver;

/// Who holds a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldOwner {
    /// An app's connection and the app's name (`ui:<app>` → `<app>`).
    Connection(ClientId, String),
    /// The core, for one operation.
    Core,
}

impl HoldOwner {
    /// What `doc.list_open` reports as `held_by`: the app's name, or `core`.
    fn label(&self) -> &str {
        match self {
            HoldOwner::Connection(_, app) => app,
            HoldOwner::Core => "core",
        }
    }
}

/// One held document, as `doc.list_open` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HoldView {
    pub doc_id: String,
    pub path: String,
    /// The holding app's name, or `core` while the core operates on a closed document.
    pub held_by: String,
}

struct Entry {
    path: PathBuf,
    owner: HoldOwner,
    /// The id of the workspace the document belongs to; `None` for a loose document.
    workspace: Option<String>,
    /// Tells this hold from a later one on the same document, so a stale guard never ends it.
    token: u64,
    /// Guards alive for the hold (more than 1 only for one connection's repeated takes).
    count: usize,
    /// Where the marker row is, when one was written.
    marker: Option<Marker>,
}

struct Marker {
    state: Arc<StateDb>,
    runtime: tokio::runtime::Handle,
}

impl Marker {
    /// Removes the row of hold `token` in the background; if the runtime is gone the next
    /// start clears it. The row goes only if it is still this hold's: a later hold on the
    /// same document, whose row may already be written, keeps its own.
    fn clear(self, doc_id: String, token: u64) {
        let Marker { state, runtime } = self;
        runtime.spawn(async move {
            if let Err(e) = state.clear_hold_marker(&doc_id, token).await {
                tracing::warn!(doc_id, error = ?e, "a hold marker could not be removed");
            }
        });
    }
}

#[derive(Default)]
struct Inner {
    held: HashMap<String, Entry>,
    next_token: u64,
}

/// The core's holds.
pub struct Holds {
    clock: Arc<dyn Clock>,
    inner: Arc<Mutex<Inner>>,
}

impl std::fmt::Debug for Holds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Holds")
            .field("held", &lock(&self.inner).held.len())
            .finish_non_exhaustive()
    }
}

/// A hold. Dropping it releases the hold, and its marker row if it has one; a hold the
/// connection's end already released stays released.
#[must_use = "the hold ends when the guard is dropped"]
pub struct HoldGuard {
    inner: Arc<Mutex<Inner>>,
    doc_id: String,
    token: u64,
}

impl std::fmt::Debug for HoldGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HoldGuard")
            .field("doc_id", &self.doc_id)
            .finish_non_exhaustive()
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        let ended = {
            let mut inner = lock(&self.inner);
            let Some(entry) = inner.held.get_mut(&self.doc_id) else {
                return;
            };
            if entry.token != self.token {
                return;
            }
            entry.count -= 1;
            if entry.count > 0 {
                return;
            }
            inner.held.remove(&self.doc_id)
        };
        if let Some(marker) = ended.and_then(|e| e.marker) {
            marker.clear(self.doc_id.clone(), self.token);
        }
    }
}

impl Holds {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Holds> {
        Arc::new(Holds {
            clock,
            inner: Arc::new(Mutex::new(Inner::default())),
        })
    }

    /// Takes the hold on `doc_id` (the document at `path`) for `holder`.
    ///
    /// # Errors
    /// `doc.held_elsewhere` when someone else holds it; the message names the holder. The
    /// error carries no trace id: the calling tool sets it.
    pub fn acquire(
        &self,
        doc_id: &str,
        path: &Path,
        holder: HoldOwner,
    ) -> Result<HoldGuard, ErrorBody> {
        self.take(doc_id, path, holder, None, None)
    }

    /// [`Holds::acquire`] for a document of workspace `workspace` (its id), and a marker row
    /// in `state`, that workspace's database, for as long as the hold lasts. The hold is
    /// listed only to sessions of that workspace ([`Holds::list_for`]). Call it inside the
    /// runtime: the row is removed from a task on it.
    ///
    /// # Errors
    /// As [`Holds::acquire`]; a marker that can't be written is only logged.
    pub async fn acquire_marked(
        &self,
        state: &Arc<StateDb>,
        workspace: &str,
        doc_id: &str,
        path: &Path,
        holder: HoldOwner,
    ) -> Result<HoldGuard, ErrorBody> {
        let marker = Marker {
            state: state.clone(),
            runtime: tokio::runtime::Handle::current(),
        };
        let holder_label = match &holder {
            HoldOwner::Connection(_, app) => format!("ui:{app}"),
            HoldOwner::Core => "core".to_string(),
        };
        let guard = self.take(doc_id, path, holder, Some(workspace), Some(marker))?;
        let row = HoldMarker {
            doc_id: doc_id.to_string(),
            path: path.to_string_lossy().into_owned(),
            holder: holder_label,
            since_utc: self
                .clock
                .utc_now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
            token: guard.token,
        };
        if let Err(e) = state.set_hold_marker(&row).await {
            tracing::warn!(doc_id, error = ?e, "a hold marker could not be written");
        } else if !self.is_current(doc_id, guard.token) {
            // The hold ended while the row was being written, and its clear may have run
            // first: take the row away (only this hold's row goes).
            if let Err(e) = state.clear_hold_marker(doc_id, guard.token).await {
                tracing::warn!(doc_id, error = ?e, "a hold marker could not be removed");
            }
        }
        Ok(guard)
    }

    /// Whether hold `token` is still the hold of `doc_id`.
    fn is_current(&self, doc_id: &str, token: u64) -> bool {
        lock(&self.inner)
            .held
            .get(doc_id)
            .is_some_and(|e| e.token == token)
    }

    fn take(
        &self,
        doc_id: &str,
        path: &Path,
        holder: HoldOwner,
        workspace: Option<&str>,
        marker: Option<Marker>,
    ) -> Result<HoldGuard, ErrorBody> {
        let mut inner = lock(&self.inner);
        if let Some(entry) = inner.held.get_mut(doc_id) {
            return match (&entry.owner, &holder) {
                (HoldOwner::Connection(held, _), HoldOwner::Connection(conn, _))
                    if held == conn =>
                {
                    entry.count += 1;
                    Ok(HoldGuard {
                        inner: self.inner.clone(),
                        doc_id: doc_id.to_string(),
                        token: entry.token,
                    })
                }
                _ => Err(held_elsewhere(doc_id, entry.owner.label())),
            };
        }
        inner.next_token += 1;
        let token = inner.next_token;
        inner.held.insert(
            doc_id.to_string(),
            Entry {
                path: path.to_path_buf(),
                owner: holder,
                workspace: workspace.map(str::to_string),
                token,
                count: 1,
                marker,
            },
        );
        Ok(HoldGuard {
            inner: self.inner.clone(),
            doc_id: doc_id.to_string(),
            token,
        })
    }

    /// Ends every hold of connection `conn` (it closed): a crashed app never leaves a hold
    /// behind (APP-104).
    pub fn release_connection(&self, conn: ClientId) {
        let ended: Vec<(String, Entry)> = {
            let mut inner = lock(&self.inner);
            let docs: Vec<String> = inner
                .held
                .iter()
                .filter(|(_, e)| matches!(&e.owner, HoldOwner::Connection(c, _) if *c == conn))
                .map(|(doc, _)| doc.clone())
                .collect();
            docs.into_iter()
                .filter_map(|doc| inner.held.remove(&doc).map(|e| (doc, e)))
                .collect()
        };
        for (doc_id, entry) in ended {
            if let Some(marker) = entry.marker {
                marker.clear(doc_id, entry.token);
            }
        }
    }

    /// Every held document, by id.
    pub fn list(&self) -> Vec<HoldView> {
        self.views(|_| true)
    }

    /// The held documents a session of workspace `workspace` (its id; `None` for a session
    /// without one) may see: those of its own workspace and loose ones, which belong to no
    /// workspace. A document of another workspace is not listed, nor is its path.
    pub fn list_for(&self, workspace: Option<&str>) -> Vec<HoldView> {
        self.views(|e| e.workspace.is_none() || e.workspace.as_deref() == workspace)
    }

    fn views(&self, keep: impl Fn(&Entry) -> bool) -> Vec<HoldView> {
        let inner = lock(&self.inner);
        let mut views: Vec<HoldView> = inner
            .held
            .iter()
            .filter(|(_, e)| keep(e))
            .map(|(doc_id, e)| HoldView {
                doc_id: doc_id.clone(),
                path: e.path.to_string_lossy().into_owned(),
                held_by: e.owner.label().to_string(),
            })
            .collect();
        views.sort_by(|a, b| a.doc_id.cmp(&b.doc_id));
        views
    }
}

impl ConnObserver for Holds {
    fn closed(&self, conn_id: u64) {
        self.release_connection(conn_id);
    }
}

fn held_elsewhere(doc_id: &str, holder: &str) -> ErrorBody {
    ErrorBody::new(
        &codes::DOC_HELD_ELSEWHERE,
        format!("The document {doc_id} is held by {holder}. Open it read-only, or make a copy."),
        "",
    )
}

/// The holds stay consistent if a holder panicked: every update is one statement.
fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}
