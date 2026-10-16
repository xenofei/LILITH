//! The schema migrations of the two kinds of `state.db` (CORE-004). Migration `n` (counting
//! from 1) is `MIGRATIONS[n - 1]`; a database at `PRAGMA user_version = n` has had the first
//! `n` applied. Entries are only ever appended, never edited.

/// Migration 1 of both scopes: the idempotency rows (CORE-016).
const IDEMPOTENCY_V1: &str = "CREATE TABLE idempotency (
    scope TEXT,
    key TEXT,
    args_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running','done')),
    outcome TEXT,
    created_utc INTEGER NOT NULL,
    PRIMARY KEY (scope, key)
);";

/// The suite `state.db` (`Env::local_dir()\state.db`).
pub const SUITE_MIGRATIONS: &[&str] = &[IDEMPOTENCY_V1];

/// A workspace's `state.db` (`Env::workspace_local_dir(ws)\state.db`).
pub const WORKSPACE_MIGRATIONS: &[&str] = &[IDEMPOTENCY_V1];

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-004
    fn migrations_apply_in_order_to_an_empty_database() {
        for list in [SUITE_MIGRATIONS, WORKSPACE_MIGRATIONS] {
            let db = rusqlite::Connection::open_in_memory().unwrap();
            for m in list {
                db.execute_batch(m).unwrap();
            }
            let columns: Vec<String> = db
                .prepare("SELECT name FROM pragma_table_info('idempotency') ORDER BY cid")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(
                columns,
                [
                    "scope",
                    "key",
                    "args_hash",
                    "state",
                    "outcome",
                    "created_utc"
                ]
            );
            let bad = db.execute(
                "INSERT INTO idempotency VALUES ('s', 'k', 'h', 'lost', NULL, 0)",
                [],
            );
            assert!(bad.is_err(), "the state CHECK constraint is missing");
        }
    }
}
