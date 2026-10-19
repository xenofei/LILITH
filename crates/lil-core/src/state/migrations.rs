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

/// Suite migration 3 and workspace migration 4: the job records (ARCH-022, ARCH-034, CORE-117).
/// A job lives in the `state.db` of its call's scope (ARCH-099). `seq` is the submission order.
/// `idem_scope`/`idem_key` (caller kind + workspace, and the call's idempotency key) make one
/// key one job (CORE-016). `external_id` is the engine's id for the job (ComfyUI's
/// `prompt_id`, CORE-120), written by the engine's handler. `tool`/`tool_args` are the tool
/// call that submitted the job, which `job.retry` dispatches again.
const JOBS: &str = "CREATE TABLE jobs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    lane TEXT NOT NULL CHECK (lane IN ('gpu-heavy','cpu')),
    priority TEXT NOT NULL CHECK (priority IN ('interactive','user','agent')),
    module TEXT NOT NULL,
    args TEXT NOT NULL,
    project TEXT,
    asset_id TEXT,
    doc_id TEXT,
    status TEXT NOT NULL
        CHECK (status IN ('queued','running','succeeded','failed','cancelled')),
    substatus TEXT,
    attempt INTEGER NOT NULL,
    created_utc INTEGER NOT NULL,
    updated_utc INTEGER NOT NULL,
    started_utc INTEGER,
    finished_utc INTEGER,
    watchdog TEXT,
    vram_unchecked INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    result TEXT,
    retry_of TEXT,
    trace_id TEXT NOT NULL,
    idem_scope TEXT,
    idem_key TEXT,
    args_hash TEXT NOT NULL,
    expected_secs INTEGER NOT NULL,
    expected_output_bytes INTEGER NOT NULL,
    vram_need_mib INTEGER,
    engine TEXT,
    grp TEXT,
    external_side_effects INTEGER NOT NULL,
    external_id TEXT,
    reservation TEXT,
    tool TEXT,
    tool_args TEXT
);
CREATE UNIQUE INDEX jobs_idem ON jobs(idem_scope, idem_key) WHERE idem_key IS NOT NULL;
CREATE INDEX jobs_open ON jobs(status) WHERE status IN ('queued','running');";

/// Suite migration 4: the ComfyUI client's caches (Appendix F "ComfyUI client", CORE-130). The
/// one `client_id` of the channel (one row); the hash-named uploads in `<root>\input` with
/// their reference counts (`last_used_utc` is when the count last changed: an upload whose
/// count is 0 is removed 24 h after it); and the model hashes, cached by path, size, mtime
/// and FileId.
const COMFY_CACHE: &str = "CREATE TABLE comfy_client (
    id TEXT PRIMARY KEY
);
CREATE TABLE comfy_uploads (
    name TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL,
    refs INTEGER NOT NULL CHECK (refs >= 0),
    last_used_utc INTEGER NOT NULL
);
CREATE TABLE model_hashes (
    path TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    file_id TEXT NOT NULL,
    sha256 TEXT NOT NULL
);";

/// Suite migration 5 (ruling P3-R27): the paid outside ComfyUI prompts the suite has seen
/// (ARCH-057, CORE-093), by prompt id, so each gets exactly one usage line across restarts and
/// crashes. `classes` is the JSON list of partner classes; `first_seen_utc` is when the suite
/// first saw it; `recorded_utc` is when its usage line was written (`NULL`: still open).
const OUTSIDE_PAID: &str = "CREATE TABLE outside_paid (
    prompt_id TEXT PRIMARY KEY,
    classes TEXT NOT NULL,
    first_seen_utc INTEGER NOT NULL,
    recorded_utc INTEGER
);
CREATE INDEX outside_paid_open ON outside_paid(prompt_id) WHERE recorded_utc IS NULL;";

/// Suite migration 6 and workspace migration 5: the confirmations (ARCH-052–055, CORE-109–115;
/// Appendix F "Confirmations"). A confirmation lives in the `state.db` of its call's scope and
/// belongs to the call's idempotency row `(scope, idem_key)`, bound to the tool and the
/// arguments' hash. At most one per key is `pending` (`confirmations_pending`); the answered,
/// expired and cancelled ones stay as the history, so a key whose earlier confirmation was
/// denied can be asked about again. `covers` is the JSON array (`confirm` before `cap`), `cap`
/// the JSON of the caps the call would pass; `project` is the call's resolved project (none
/// without a workspace), which an approval is bound to. Times are Unix seconds.
const CONFIRMATIONS: &str = "CREATE TABLE confirmations (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    scope TEXT NOT NULL,
    idem_key TEXT NOT NULL,
    tool TEXT NOT NULL,
    args_hash TEXT NOT NULL,
    arguments TEXT NOT NULL,
    caller_kind TEXT NOT NULL,
    covers TEXT NOT NULL,
    cap TEXT,
    state TEXT NOT NULL
        CHECK (state IN ('pending','approved','denied','expired','cancelled')),
    confirmed_via TEXT CHECK (confirmed_via IN ('app','claude_code')),
    created_utc INTEGER NOT NULL,
    expires_utc INTEGER NOT NULL,
    answered_utc INTEGER,
    cap_action TEXT,
    session_id TEXT NOT NULL,
    project TEXT
);
CREATE UNIQUE INDEX confirmations_pending ON confirmations(scope, idem_key)
    WHERE state = 'pending';
CREATE INDEX confirmations_key ON confirmations(scope, idem_key);
CREATE INDEX confirmations_state ON confirmations(state, created_utc);";

/// The suite `state.db` (`Env::local_dir()\state.db`).
pub const SUITE_MIGRATIONS: &[&str] = &[
    IDEMPOTENCY_V1,
    IDEMPOTENCY_PRUNE_V2,
    JOBS,
    COMFY_CACHE,
    OUTSIDE_PAID,
    CONFIRMATIONS,
];

/// A workspace's `state.db` (`Env::workspace_local_dir(ws)\state.db`).
pub const WORKSPACE_MIGRATIONS: &[&str] = &[
    IDEMPOTENCY_V1,
    IDEMPOTENCY_PRUNE_V2,
    GENERATED_V3,
    JOBS,
    CONFIRMATIONS,
];

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
        assert_eq!(SUITE_MIGRATIONS.len(), 6);
        assert_eq!(WORKSPACE_MIGRATIONS.len(), 5);
    }

    #[test] // spec: CORE-115, ARCH-053
    fn both_scopes_hold_confirmations_with_one_pending_per_key() {
        for (list, at) in [(SUITE_MIGRATIONS, 5), (WORKSPACE_MIGRATIONS, 4)] {
            assert_eq!(list[at], CONFIRMATIONS, "appended, never inserted");
            let db = rusqlite::Connection::open_in_memory().unwrap();
            for m in list {
                db.execute_batch(m).unwrap();
            }
            let insert = |id: &str, state: &str| {
                db.execute(
                    "INSERT INTO confirmations (id, scope, idem_key, tool, args_hash, arguments,
                         caller_kind, covers, state, created_utc, expires_utc, session_id)
                     VALUES (?1, 'cli|none', 'k', 't', 'h', '{}', 'cli', '[\"confirm\"]', ?2,
                         0, 1800, 's_1')",
                    rusqlite::params![id, state],
                )
            };
            insert("cf_1", "pending").unwrap();
            assert!(insert("cf_2", "pending").is_err(), "one pending per key");
            insert("cf_3", "denied").unwrap();
            insert("cf_4", "expired").unwrap();
            assert!(insert("cf_5", "maybe").is_err(), "the state CHECK");
        }
    }

    #[test] // spec: CORE-117, ARCH-099
    fn both_scopes_hold_job_records_after_their_earlier_migrations() {
        for (list, at) in [(SUITE_MIGRATIONS, 2), (WORKSPACE_MIGRATIONS, 3)] {
            assert_eq!(list[at], JOBS, "JOBS is appended, never inserted");
            let db = rusqlite::Connection::open_in_memory().unwrap();
            for m in list {
                db.execute_batch(m).unwrap();
            }
            let insert = |id: &str, status: &str, key: Option<&str>| {
                db.execute(
                    "INSERT INTO jobs (job_id, kind, lane, priority, module, args, status,
                         substatus, attempt, created_utc, updated_utc, trace_id, idem_scope,
                         idem_key, args_hash, expected_secs, expected_output_bytes,
                         external_side_effects)
                     VALUES (?1, 'k', 'cpu', 'agent', 'm', '{}', ?2, 'waiting', 1, 0, 0, 't',
                         'cli|none', ?3, 'h', 0, 0, 0)",
                    rusqlite::params![id, status, key],
                )
            };
            insert("j_1", "queued", Some("k")).unwrap();
            assert!(
                insert("j_2", "queued", Some("k")).is_err(),
                "one key, one job"
            );
            insert("j_3", "queued", None).unwrap();
            insert("j_4", "queued", None).unwrap();
            assert!(insert("j_5", "lost", None).is_err(), "the status CHECK");
        }
    }
}
