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
/// key one job (CORE-016); `idem_scope` is kept without a key too (the submitting caller). `external_id` is the engine's id for the job (ComfyUI's
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

/// Workspace migration 6: the hold markers (APP-104). A hold lives in the core's memory, tied
/// to its connection; this row only records that a document was held, so one left by a killed
/// core is visible until the next start empties the table.
const HOLDS: &str = "CREATE TABLE hold_markers (
    doc_id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    holder TEXT NOT NULL,
    since_utc INTEGER NOT NULL,
    token INTEGER NOT NULL
);";

/// Suite migration 7 and workspace migration 7: the write-ahead usage rows (CORE-071;
/// Appendix F "Usage records"). A row is written to the `state.db` of its call's scope before
/// the external request is sent, and removed once its usage line is in the log. `record` is
/// the usage line's JSON record (`status: "pending"` while `state` is `pending`; the finished
/// record once `state` is `logging`, the step just before the line is appended). `run` is the
/// core run that wrote it: a row of another run was left by a dead core, and startup turns it
/// into a `status: "pending"` line. Times are Unix milliseconds.
const USAGE_PENDING: &str = "CREATE TABLE usage_pending (
    id TEXT PRIMARY KEY,
    run TEXT NOT NULL,
    call_id TEXT NOT NULL,
    job_id TEXT,
    reservation TEXT,
    record TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','logging')),
    created_ms INTEGER NOT NULL,
    logging_ms INTEGER
);
CREATE INDEX usage_pending_run ON usage_pending(run);";

/// Suite migration 8: the caps (CORE-085–092, Appendix F "Caps and money in `suite.toml`").
/// Only the suite `state.db` has them: it holds every reservation, from any workspace or none
/// (CORE-087).
/// - `cap_reservations`: one row per reservation `rv_<16 hex>`, made by core run `run` for
///   `owner` (the call id `c_…`, then the job id `j_…` that took it over) in `workspace` (an id
///   or `none`), in the local calendar month `period`. `amount_micros` is the estimate in its
///   own `currency`; `rate_micros`/`rate_date` the exchange rate used (1 unit of `currency` is
///   `rate_micros` display micro-units; `rate_date` is null for the display currency itself),
///   and `display_micros` the reserved amount in `display_currency`. `allowed_by` is the
///   confirmation that let it pass a cap once. `state` is `held` until it is `settled` (at
///   `settled_micros`, display) or `released`; `kept` marks a held reservation whose call left a
///   pending usage line at a crash (it stays held).
/// - `cap_settled`: this machine's settled totals per period, cap (`monthly` for all money,
///   `provider:<id>` per provider) and display currency.
/// - `cap_alerts`: the alert levels already raised per period and cap (CORE-089).
const CAPS: &str = "CREATE TABLE cap_reservations (
    id TEXT PRIMARY KEY,
    run TEXT NOT NULL,
    owner TEXT NOT NULL,
    workspace TEXT NOT NULL,
    provider TEXT NOT NULL,
    basis TEXT NOT NULL,
    period TEXT NOT NULL,
    currency TEXT NOT NULL,
    amount_micros INTEGER NOT NULL,
    rate_micros INTEGER NOT NULL,
    rate_date TEXT,
    display_currency TEXT NOT NULL,
    display_micros INTEGER NOT NULL,
    allowed_by TEXT,
    state TEXT NOT NULL CHECK (state IN ('held','settled','released')),
    settled_micros INTEGER,
    kept INTEGER NOT NULL DEFAULT 0,
    created_ms INTEGER NOT NULL,
    ended_ms INTEGER
);
CREATE INDEX cap_reservations_held ON cap_reservations(state, period);
CREATE TABLE cap_settled (
    period TEXT NOT NULL,
    cap TEXT NOT NULL,
    currency TEXT NOT NULL,
    settled_micros INTEGER NOT NULL,
    PRIMARY KEY (period, cap, currency)
);
CREATE TABLE cap_alerts (
    period TEXT NOT NULL,
    cap TEXT NOT NULL,
    threshold_pct INTEGER NOT NULL,
    alerted_ms INTEGER NOT NULL,
    PRIMARY KEY (period, cap, threshold_pct)
);";

/// Suite migration 9 and workspace migration 8: plan 4a's job fields (CORE-117, CORE-160;
/// Appendix G "Jobs"). `batch_id`/`batch_index` are the job's batch and its place in it (every
/// job has one, P4a-R13); `order_key` orders a class in its lane (a new job's is its `seq`;
/// `job.move` changes it); `origin` is the typed `{surface, id?, step?}` JSON of who started
/// it (P4a-R12); `status_seq` the JSON list of its status changes (the first and the newest
/// 63); `engine_device`, `phase`, `subtasks` (JSON), `indeterminate`, `last_progress_ms`
/// (Unix ms), `cache_hit_nodes` (JSON) and `all_cached` what its run reported; `estimate` and
/// `estimate_key` the estimate taken at submission (Task 7 fills them); `withdrawn_id` the
/// prompt id a pause withdrew, kept until it is known gone (CORE-160, CORE-120). `job_order`
/// holds the high-water mark of the core-wide order counter (Appendix G "Queue order"),
/// one row; only the suite `state.db`'s is used. A row from
/// before this migration gets the batch of its own job id (`b_<its 16 hex>`), index 0, its
/// `seq` as its order key, and the origin its idempotency scope names (a scope of another
/// form, or none, leaves it null). A `cli|…` scope gives `{surface: "cli"}`, also for a job
/// the core submitted itself, which a new row records as `core` (`StartedBy::of`): the old
/// row doesn't tell the two apart.
const JOBS_V2: &str = "ALTER TABLE jobs ADD COLUMN batch_id TEXT;
ALTER TABLE jobs ADD COLUMN batch_index INTEGER;
ALTER TABLE jobs ADD COLUMN order_key INTEGER;
ALTER TABLE jobs ADD COLUMN origin TEXT;
ALTER TABLE jobs ADD COLUMN status_seq TEXT;
ALTER TABLE jobs ADD COLUMN engine_device TEXT;
ALTER TABLE jobs ADD COLUMN phase TEXT;
ALTER TABLE jobs ADD COLUMN subtasks TEXT;
ALTER TABLE jobs ADD COLUMN indeterminate INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN last_progress_ms INTEGER;
ALTER TABLE jobs ADD COLUMN cache_hit_nodes TEXT;
ALTER TABLE jobs ADD COLUMN all_cached INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN estimate TEXT;
ALTER TABLE jobs ADD COLUMN estimate_key TEXT;
ALTER TABLE jobs ADD COLUMN withdrawn_id TEXT;
UPDATE jobs SET
    batch_id = 'b_' || substr(job_id, 3, 16),
    batch_index = 0,
    order_key = seq,
    origin = CASE
        WHEN idem_scope LIKE 'ui:%|%' THEN json_object('surface', 'panel', 'id',
            substr(idem_scope, 4, instr(idem_scope, '|') - 4))
        WHEN idem_scope LIKE 'mcp|%' THEN json_object('surface', 'mcp')
        WHEN idem_scope LIKE 'assistant|%' THEN json_object('surface', 'agent_session')
        WHEN idem_scope LIKE 'cli|%' THEN json_object('surface', 'cli')
    END;
CREATE INDEX jobs_batch ON jobs(batch_id) WHERE batch_id IS NOT NULL;
CREATE TABLE job_order (
    k INTEGER PRIMARY KEY CHECK (k = 0),
    last_order INTEGER NOT NULL
);";

/// Suite migration 10: plan 4a's engine records (CORE-130, CORE-164; Appendix G "Model
/// inventory report", "Engine environments"). `model_hashes.family` is the model family a
/// file's header names (untrusted, ARCH-060; `NULL`: none), and `family_read` is 1 once the
/// header gave its answer for the cached size, mtime and FileId, so a row from before this
/// migration (0) has its header read once by the next inventory. `engine_captures` keeps each
/// engine's environment captures (`capture` is the `EnvCapture` JSON), the newest 10 per
/// engine; `captured_utc` is Unix milliseconds.
const ENGINE_RECORDS: &str = "ALTER TABLE model_hashes ADD COLUMN family TEXT;
ALTER TABLE model_hashes ADD COLUMN family_read INTEGER NOT NULL DEFAULT 0;
CREATE TABLE engine_captures (
    engine TEXT NOT NULL,
    captured_utc INTEGER NOT NULL,
    capture TEXT NOT NULL,
    PRIMARY KEY (engine, captured_utc)
);";

/// Suite migration 11: the restart-hold toast once per held batch (owner decision 2026-10-26,
/// Checkpoint A walkthrough; Appendix G "Restart hold"). Each row is a job counted in a
/// posted restart-hold toast and when (Unix milliseconds); the newest 4,096 are kept.
const RESTART_HOLD_TOASTS: &str = "CREATE TABLE restart_hold_toasts (
    job_id TEXT PRIMARY KEY,
    toasted_ms INTEGER NOT NULL
);";

/// The suite `state.db` (`Env::local_dir()\state.db`).
pub const SUITE_MIGRATIONS: &[&str] = &[
    IDEMPOTENCY_V1,
    IDEMPOTENCY_PRUNE_V2,
    JOBS,
    COMFY_CACHE,
    OUTSIDE_PAID,
    CONFIRMATIONS,
    USAGE_PENDING,
    CAPS,
    JOBS_V2,
    ENGINE_RECORDS,
    RESTART_HOLD_TOASTS,
];

/// A workspace's `state.db` (`Env::workspace_local_dir(ws)\state.db`).
pub const WORKSPACE_MIGRATIONS: &[&str] = &[
    IDEMPOTENCY_V1,
    IDEMPOTENCY_PRUNE_V2,
    GENERATED_V3,
    JOBS,
    CONFIRMATIONS,
    HOLDS,
    USAGE_PENDING,
    JOBS_V2,
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
        assert_eq!(SUITE_MIGRATIONS.len(), 11);
        assert_eq!(
            SUITE_MIGRATIONS[10], RESTART_HOLD_TOASTS,
            "appended, never inserted"
        );
        assert_eq!(
            SUITE_MIGRATIONS[9], ENGINE_RECORDS,
            "appended, never inserted"
        );
        assert_eq!(WORKSPACE_MIGRATIONS.len(), 8);
        assert_eq!(SUITE_MIGRATIONS[8], JOBS_V2, "appended, never inserted");
        assert_eq!(WORKSPACE_MIGRATIONS[7], JOBS_V2, "appended, never inserted");
        assert_eq!(WORKSPACE_MIGRATIONS[5], HOLDS, "appended, never inserted");
        assert_eq!(
            SUITE_MIGRATIONS[6], USAGE_PENDING,
            "appended, never inserted"
        );
        assert_eq!(
            WORKSPACE_MIGRATIONS[6], USAGE_PENDING,
            "appended, never inserted"
        );
        assert_eq!(SUITE_MIGRATIONS[7], CAPS, "appended, never inserted");
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

    /// JOBS_V2 gives a row from before it the batch of its own job id, index 0, its `seq` as
    /// its order key and the origin its idempotency scope names (Task 6).
    #[test] // spec: CORE-117, CORE-160
    fn jobs_v2_derives_batch_order_and_origin_for_old_rows() {
        for list in [SUITE_MIGRATIONS, WORKSPACE_MIGRATIONS] {
            let at = list.iter().position(|m| *m == JOBS_V2).unwrap();
            let db = rusqlite::Connection::open_in_memory().unwrap();
            for m in &list[..at] {
                db.execute_batch(m).unwrap();
            }
            for (id, scope) in [
                ("j_00000000000000a1", Some("ui:image|ws1")),
                ("j_00000000000000a2", Some("cli|none")),
                ("j_00000000000000a3", Some("mcp|none")),
                ("j_00000000000000a4", Some("assistant|ws1")),
                ("j_00000000000000a5", None),
            ] {
                db.execute(
                    "INSERT INTO jobs (job_id, kind, lane, priority, module, args, status,
                         substatus, attempt, created_utc, updated_utc, trace_id, idem_scope,
                         args_hash, expected_secs, expected_output_bytes, external_side_effects)
                     VALUES (?1, 'k', 'cpu', 'agent', 'm', '{}', 'queued', 'waiting', 1, 0, 0,
                         't', ?2, 'h', 0, 0, 0)",
                    rusqlite::params![id, scope],
                )
                .unwrap();
            }
            db.execute_batch(JOBS_V2).unwrap();
            let rows: Vec<(String, String, i64, bool, Option<String>)> = db
                .prepare(
                    "SELECT batch_id, job_id, batch_index, order_key = seq, origin FROM jobs
                     ORDER BY seq",
                )
                .unwrap()
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let origins: Vec<Option<&str>> = rows.iter().map(|r| r.4.as_deref()).collect();
            assert_eq!(
                origins,
                [
                    Some(r#"{"surface":"panel","id":"image"}"#),
                    Some(r#"{"surface":"cli"}"#),
                    Some(r#"{"surface":"mcp"}"#),
                    Some(r#"{"surface":"agent_session"}"#),
                    None,
                ]
            );
            for (batch, job, index, ordered, _) in &rows {
                assert_eq!(batch, &format!("b_{}", &job[2..]));
                assert_eq!((*index, *ordered), (0, true));
            }
        }
    }
}
