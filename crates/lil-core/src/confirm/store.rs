//! The `confirmations` table (suite migration 6, workspace migration 5), through a database's actor.
//! Every state change is one `UPDATE … WHERE state = 'pending'`, so of two racing changes to
//! one confirmation exactly one happens (and only that one publishes its event).

use lil_protocol::Cover;
use rusqlite::{OptionalExtension as _, Row, TransactionBehavior, params};

use super::{ConfState, ConfirmationRow};
use crate::state::{StateDb, StateError};
use crate::tools::ConfirmedVia;

const COLUMNS: &str = "id, scope, idem_key, tool, args_hash, arguments, caller_kind, covers, \
                       cap, state, confirmed_via, created_utc, expires_utc, answered_utc, \
                       cap_action, session_id, project";

fn sql(e: &rusqlite::Error) -> StateError {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            StateError::Corrupt(e.to_string())
        }
        _ => StateError::Io(e.to_string()),
    }
}

fn text_error(column: usize, why: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        Box::<dyn std::error::Error + Send + Sync>::from(why),
    )
}

fn read_row(r: &Row<'_>) -> rusqlite::Result<ConfirmationRow> {
    let arguments: String = r.get(5)?;
    let covers: String = r.get(7)?;
    let cap: Option<String> = r.get(8)?;
    let state: String = r.get(9)?;
    let via: Option<String> = r.get(10)?;
    Ok(ConfirmationRow {
        id: r.get(0)?,
        scope: r.get(1)?,
        idem_key: r.get(2)?,
        tool: r.get(3)?,
        args_hash: r.get(4)?,
        arguments: serde_json::from_str(&arguments).map_err(|e| text_error(5, e.to_string()))?,
        caller_kind: r.get(6)?,
        covers: serde_json::from_str::<Vec<Cover>>(&covers)
            .map_err(|e| text_error(7, e.to_string()))?,
        cap: cap
            .map(|c| serde_json::from_str(&c))
            .transpose()
            .map_err(|e| text_error(8, e.to_string()))?,
        state: ConfState::parse(&state).ok_or_else(|| text_error(9, state.clone()))?,
        confirmed_via: match via.as_deref() {
            None => None,
            Some("app") => Some(ConfirmedVia::App),
            Some("claude_code") => Some(ConfirmedVia::ClaudeCode),
            Some(other) => return Err(text_error(10, other.to_string())),
        },
        created_utc: r.get(11)?,
        expires_utc: r.get(12)?,
        answered_utc: r.get(13)?,
        cap_action: r.get(14)?,
        session_id: r.get(15)?,
        project: r.get(16)?,
    })
}

fn insert(conn: &rusqlite::Connection, row: &ConfirmationRow) -> rusqlite::Result<()> {
    let json = |v: &serde_json::Value| serde_json::to_string(v).expect("JSON serialises");
    conn.execute(
        &format!(
            "INSERT INTO confirmations ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"
        ),
        params![
            row.id,
            row.scope,
            row.idem_key,
            row.tool,
            row.args_hash,
            json(&row.arguments),
            row.caller_kind,
            serde_json::to_string(&row.covers).expect("covers serialise"),
            row.cap.as_ref().map(json),
            row.state.wire(),
            row.confirmed_via.map(ConfirmedVia::wire),
            row.created_utc,
            row.expires_utc,
            row.answered_utc,
            row.cap_action,
            row.session_id,
            row.project,
        ],
    )
    .map(drop)
}

/// What [`request`] did.
pub(super) struct Requested {
    /// The pending confirmation of the key: `new`, or the one already there.
    pub row: ConfirmationRow,
    /// `new` was inserted (else the pending row with the same arguments was kept).
    pub inserted: bool,
    /// A pending row of the key that this ended first (it had expired, or other arguments).
    pub ended: Option<ConfirmationRow>,
}

/// The pending confirmation of `new`'s key: the one there with the same arguments hash that
/// has not expired at `now`, else `new`, inserted (a pending row it replaces is ended first:
/// `expired` when its time has passed, else `cancelled`). One transaction.
pub(super) async fn request(
    db: &StateDb,
    new: ConfirmationRow,
    now: i64,
) -> Result<Requested, StateError> {
    db.call(move |conn| {
        let run = || -> rusqlite::Result<Requested> {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let found = tx
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM confirmations
                         WHERE scope = ?1 AND idem_key = ?2 AND state = 'pending'"
                    ),
                    params![new.scope, new.idem_key],
                    read_row,
                )
                .optional()?;
            let mut ended = None;
            if let Some(mut row) = found {
                if row.args_hash == new.args_hash
                    && row.project == new.project
                    && row.expires_utc > now
                {
                    tx.commit()?;
                    return Ok(Requested {
                        row,
                        inserted: false,
                        ended: None,
                    });
                }
                row.state = if row.expires_utc <= now {
                    ConfState::Expired
                } else {
                    ConfState::Cancelled
                };
                tx.execute(
                    "UPDATE confirmations SET state = ?2 WHERE id = ?1 AND state = 'pending'",
                    params![row.id, row.state.wire()],
                )?;
                ended = Some(row);
            }
            insert(&tx, &new)?;
            tx.commit()?;
            Ok(Requested {
                row: new,
                inserted: true,
                ended,
            })
        };
        run().map_err(|e| sql(&e))
    })
    .await
}

/// Inserts `row` as it is (a confirmation Claude Code's prompt answered).
pub(super) async fn insert_row(db: &StateDb, row: ConfirmationRow) -> Result<(), StateError> {
    db.call(move |conn| insert(conn, &row).map_err(|e| sql(&e)))
        .await
}

/// The confirmation `id`.
pub(super) async fn get(db: &StateDb, id: &str) -> Result<Option<ConfirmationRow>, StateError> {
    let id = id.to_string();
    db.call(move |conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM confirmations WHERE id = ?1"),
            params![id],
            read_row,
        )
        .optional()
        .map_err(|e| sql(&e))
    })
    .await
}

/// The newest confirmation of the key `(scope, key)`.
pub(super) async fn latest(
    db: &StateDb,
    scope: &str,
    key: &str,
) -> Result<Option<ConfirmationRow>, StateError> {
    let (scope, key) = (scope.to_string(), key.to_string());
    db.call(move |conn| {
        conn.query_row(
            &format!(
                "SELECT {COLUMNS} FROM confirmations WHERE scope = ?1 AND idem_key = ?2
                 ORDER BY seq DESC LIMIT 1"
            ),
            params![scope, key],
            read_row,
        )
        .optional()
        .map_err(|e| sql(&e))
    })
    .await
}

/// Every pending confirmation.
pub(super) async fn pending(db: &StateDb) -> Result<Vec<ConfirmationRow>, StateError> {
    db.call(|conn| {
        let read = || -> rusqlite::Result<Vec<ConfirmationRow>> {
            conn.prepare(&format!(
                "SELECT {COLUMNS} FROM confirmations WHERE state = 'pending' ORDER BY seq"
            ))?
            .query_map([], read_row)?
            .collect()
        };
        read().map_err(|e| sql(&e))
    })
    .await
}

/// At most `limit` confirmations, newest first: the pending ones, or the others (the history).
pub(super) async fn list(
    db: &StateDb,
    pending: bool,
    limit: u32,
) -> Result<Vec<ConfirmationRow>, StateError> {
    db.call(move |conn| {
        let read = || -> rusqlite::Result<Vec<ConfirmationRow>> {
            let filter = if pending {
                "state = 'pending'"
            } else {
                "state <> 'pending'"
            };
            conn.prepare(&format!(
                "SELECT {COLUMNS} FROM confirmations WHERE {filter}
                 ORDER BY created_utc DESC, seq DESC LIMIT ?1"
            ))?
            .query_map(params![limit], read_row)?
            .collect()
        };
        read().map_err(|e| sql(&e))
    })
    .await
}

/// Ends the pending confirmation `id` as `state` (with how and when it was answered): the row
/// after the change, or `None` when it was not pending (any more), or, with `unexpired_at`,
/// when it had expired by then (checked in the same statement, so a late answer never lands).
pub(super) async fn end(
    db: &StateDb,
    id: &str,
    state: ConfState,
    via: Option<ConfirmedVia>,
    answered_utc: Option<i64>,
    cap_action: Option<String>,
    unexpired_at: Option<i64>,
) -> Result<Option<ConfirmationRow>, StateError> {
    let id = id.to_string();
    db.call(move |conn| {
        let mut run = || -> rusqlite::Result<Option<ConfirmationRow>> {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = tx.execute(
                "UPDATE confirmations
                 SET state = ?2, confirmed_via = ?3, answered_utc = ?4, cap_action = ?5
                 WHERE id = ?1 AND state = 'pending' AND (?6 IS NULL OR expires_utc > ?6)",
                params![
                    id,
                    state.wire(),
                    via.map(ConfirmedVia::wire),
                    answered_utc,
                    cap_action,
                    unexpired_at
                ],
            )?;
            let row = if changed == 1 {
                tx.query_row(
                    &format!("SELECT {COLUMNS} FROM confirmations WHERE id = ?1"),
                    params![id],
                    read_row,
                )
                .optional()?
            } else {
                None
            };
            tx.commit()?;
            Ok(row)
        };
        run().map_err(|e| sql(&e))
    })
    .await
}
