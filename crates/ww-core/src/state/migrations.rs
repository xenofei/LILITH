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

/// Migration 2 of both scopes: the index the prune in `idem_begin` uses, so it no longer scans
/// the table inside `BEGIN IMMEDIATE`.
const IDEMPOTENCY_PRUNE_V2: &str =
    "CREATE INDEX idempotency_prune ON idempotency(state, created_utc);";

/// Migration 3 of a workspace database: the store's last generated hash per file (ARCH-078)
/// and its pending replaces (write step 5, ARCH-074). Rows are keyed on the case-folded path
/// (`store::paths::path_key`, the folding of the store's path locks; ruling Q14), so every
/// spelling of one file finds them; `path` keeps the spelling last written.
const GENERATED_V3: &str = "CREATE TABLE generated (
    key TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    written_utc INTEGER NOT NULL
);
CREATE TABLE generated_pending (
    key TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    old_sha256 TEXT,
    new_sha256 TEXT NOT NULL,
    started_utc INTEGER NOT NULL
);";

/// The suite `state.db` (`Env::local_dir()\state.db`).
pub const SUITE_MIGRATIONS: &[&str] = &[IDEMPOTENCY_V1, IDEMPOTENCY_PRUNE_V2];

/// A workspace's `state.db` (`Env::workspace_local_dir(ws)\state.db`).
pub const WORKSPACE_MIGRATIONS: &[&str] = &[IDEMPOTENCY_V1, IDEMPOTENCY_PRUNE_V2, GENERATED_V3];

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
            let index: String = db
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'idempotency'
                     AND name = 'idempotency_prune'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(index, "idempotency_prune");
        }
    }

    #[test] // spec: ARCH-074, ARCH-078
    fn workspace_scope_has_generated_and_pending_tables() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for m in WORKSPACE_MIGRATIONS {
            db.execute_batch(m).unwrap();
        }
        let columns = |table: &str| -> Vec<String> {
            db.prepare(&format!(
                "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
        };
        assert_eq!(
            columns("generated"),
            ["key", "path", "sha256", "written_utc"]
        );
        assert_eq!(
            columns("generated_pending"),
            ["key", "path", "old_sha256", "new_sha256", "started_utc"]
        );
        assert_eq!(SUITE_MIGRATIONS.len(), 2);
        assert_eq!(WORKSPACE_MIGRATIONS.len(), 3);
    }
}
