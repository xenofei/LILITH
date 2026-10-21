//! `telemetry.db` (CORE-004, CORE-005, CORE-062): the machine-level, disposable store of
//! hardware samples.
//!
//! - **Tables.** `s1` is one row per sample (1 s apart; 24 h kept), `m1` and `h1` are the
//!   1-minute rows (30 days) and the hourly rows (forever): per metric the count, minimum,
//!   maximum and sum of the samples in the bucket, upserted as each sample is written, so the
//!   rolled-up rows are always current and the energy sums stay exact. `proc_s1` holds the
//!   per-process figures of the processes that matter (tracked ones and any using the GPU),
//!   24 h. `job_energy` holds the per-job rows `meter` writes when a job completes.
//! - **Metrics.** The twelve series metrics are columns of `s1`; a column is `NULL` when its
//!   figure was unknown at that sample, and a bucket only counts the samples that had it. For
//!   the two `*.energy` metrics a sample's value is that interval's energy in mJ, so a bucket's
//!   sum is the exact energy of the bucket.
//! - **Estimates.** A sample's `est` bits say which of its figures were estimated, not measured
//!   (a lost source's gap, an integrated power, a TDP estimate).
//! - **Open.** `quick_check` must say `ok` and `PRAGMA user_version` must be one this core
//!   knows: the current [`USER_VERSION`], or an older one, which is migrated forward in place
//!   ([`MIGRATIONS`], one transaction per step), so an upgrade keeps the history. Anything else
//!   (a newer version, version 0, a failed migration, a file that isn't a database) means the
//!   old file and its side files are renamed to `telemetry.db.damaged-<utc>` through the
//!   store's one door (`fsops::set_aside_db`) and a new one is created. Nothing is deleted. If
//!   even that fails, the telemetry runs on an in-memory database, so a broken disk never stops
//!   the core.
//! - **Access.** One connection behind a mutex; every call is blocking and runs on a blocking
//!   thread.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, params};

use crate::store::fsops;

use super::{EnergySlice, JobEnergy, JobEnergyQuery, Measured};

/// `PRAGMA user_version` of `telemetry.db`.
pub(crate) const USER_VERSION: i64 = 1;

/// The forward migrations: entry `i` takes a file from version `i + 1` to `i + 2`, so a file of
/// any older version reaches [`USER_VERSION`]. A schema change appends its step here, edits
/// [`SCHEMA`] (what a new file gets) to match, and bumps [`USER_VERSION`].
const MIGRATIONS: &[&str] = &[];

/// What [`open_file`] expects: the schema of a new file, its version, and the steps that bring
/// an older file to it (production: [`SCHEMA`], [`USER_VERSION`], [`MIGRATIONS`]).
#[derive(Clone, Copy)]
struct Layout<'a> {
    schema: &'a str,
    version: i64,
    migrations: &'a [&'a str],
}

const LAYOUT: Layout<'static> = Layout {
    schema: SCHEMA,
    version: USER_VERSION,
    migrations: MIGRATIONS,
};
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
/// 1-second samples are kept 24 h (CORE-062).
pub(crate) const KEEP_S1_MS: i64 = 24 * HOUR_MS;
/// 1-minute rows are kept 30 days (CORE-062).
pub(crate) const KEEP_M1_MS: i64 = 30 * 24 * HOUR_MS;

/// `est` bits: which figures of a sample were estimated rather than measured.
pub(crate) const EST_GPU_POWER: i64 = 1;
pub(crate) const EST_GPU_ENERGY: i64 = 2;
pub(crate) const EST_CPU_POWER: i64 = 4;
pub(crate) const EST_CPU_ENERGY: i64 = 8;

/// The series metrics (Appendix F).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Metric {
    GpuUtil,
    GpuPower,
    GpuEnergy,
    GpuTemp,
    GpuVramUsed,
    CpuUtil,
    CpuPower,
    CpuEnergy,
    RamUsed,
    SystemPower,
    NetRx,
    NetTx,
}

impl Metric {
    pub(crate) const ALL: [Metric; 12] = [
        Metric::GpuUtil,
        Metric::GpuPower,
        Metric::GpuEnergy,
        Metric::GpuTemp,
        Metric::GpuVramUsed,
        Metric::CpuUtil,
        Metric::CpuPower,
        Metric::CpuEnergy,
        Metric::RamUsed,
        Metric::SystemPower,
        Metric::NetRx,
        Metric::NetTx,
    ];

    /// The name a caller uses.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Metric::GpuUtil => "gpu.util",
            Metric::GpuPower => "gpu.power",
            Metric::GpuEnergy => "gpu.energy",
            Metric::GpuTemp => "gpu.temp",
            Metric::GpuVramUsed => "gpu.vram_used",
            Metric::CpuUtil => "cpu.util",
            Metric::CpuPower => "cpu.power",
            Metric::CpuEnergy => "cpu.energy",
            Metric::RamUsed => "ram.used",
            Metric::SystemPower => "system.power",
            Metric::NetRx => "net.rx",
            Metric::NetTx => "net.tx",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The column of `s1`.
    fn column(self) -> &'static str {
        match self {
            Metric::GpuUtil => "gpu_util",
            Metric::GpuPower => "gpu_power",
            Metric::GpuEnergy => "gpu_energy",
            Metric::GpuTemp => "gpu_temp",
            Metric::GpuVramUsed => "gpu_vram_used",
            Metric::CpuUtil => "cpu_util",
            Metric::CpuPower => "cpu_power",
            Metric::CpuEnergy => "cpu_energy",
            Metric::RamUsed => "ram_used",
            Metric::SystemPower => "system_power",
            Metric::NetRx => "net_rx",
            Metric::NetTx => "net_tx",
        }
    }

    /// The two energy metrics: a point's min, average and max are the interval's sum.
    pub(crate) fn is_energy(self) -> bool {
        matches!(self, Metric::GpuEnergy | Metric::CpuEnergy)
    }

    fn index(self) -> usize {
        Metric::ALL
            .iter()
            .position(|m| *m == self)
            .expect("every metric is in ALL")
    }
}

/// How fine a series is (`telemetry.series` `resolution`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    Second,
    Minute,
    Hour,
}

/// One process's figures at one sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcRow {
    pub pid: u32,
    /// SM utilisation, permille.
    pub sm: u32,
    /// CPU time used since the previous sample, ms.
    pub cpu_ms: u64,
    pub ram_mib: Option<u64>,
    pub vram_local: Option<u64>,
    pub vram_committed: Option<u64>,
}

/// One sample, as stored.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    /// Unix ms, unique and increasing.
    pub ts: i64,
    /// The core run that wrote the sample (its start, Unix ms): monotonic times only compare
    /// within a run.
    pub run: i64,
    /// The clock's monotonic time, ms: what a job's execution windows are measured in.
    pub mono: i64,
    /// The time this sample stands for, ms.
    pub dt: i64,
    pub est: i64,
    /// By [`Metric::ALL`] order.
    pub values: [Option<i64>; 12],
    /// CPU time used by every process together over `dt`, ms.
    pub cpu_ms_all: u64,
    pub procs: Vec<ProcRow>,
}

impl Row {
    pub(crate) fn value(&self, m: Metric) -> Option<i64> {
        self.values[m.index()]
    }

    pub(crate) fn set(&mut self, m: Metric, v: Option<i64>) {
        self.values[m.index()] = v;
    }
}

/// One point of a series before it is formatted: `min`, `avg` and `max` over the bucket.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawPoint {
    pub ts: i64,
    pub min: i64,
    pub avg: f64,
    pub max: i64,
}

const SCHEMA: &str = "
CREATE TABLE s1 (
    ts INTEGER PRIMARY KEY,
    run INTEGER NOT NULL,
    mono INTEGER NOT NULL,
    dt INTEGER NOT NULL,
    est INTEGER NOT NULL DEFAULT 0,
    gpu_util INTEGER, gpu_power INTEGER, gpu_energy INTEGER, gpu_temp INTEGER,
    gpu_vram_used INTEGER,
    cpu_util INTEGER, cpu_power INTEGER, cpu_energy INTEGER,
    ram_used INTEGER, system_power INTEGER, net_rx INTEGER, net_tx INTEGER,
    cpu_ms_all INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX s1_run_mono ON s1 (run, mono);
CREATE TABLE m1 (
    ts INTEGER NOT NULL, metric TEXT NOT NULL, n INTEGER NOT NULL,
    min INTEGER, max INTEGER, sum INTEGER NOT NULL,
    PRIMARY KEY (ts, metric)
) WITHOUT ROWID;
CREATE TABLE h1 (
    ts INTEGER NOT NULL, metric TEXT NOT NULL, n INTEGER NOT NULL,
    min INTEGER, max INTEGER, sum INTEGER NOT NULL,
    PRIMARY KEY (ts, metric)
) WITHOUT ROWID;
CREATE TABLE proc_s1 (
    ts INTEGER NOT NULL, pid INTEGER NOT NULL,
    sm INTEGER NOT NULL, cpu_ms INTEGER NOT NULL,
    ram_mib INTEGER, vram_local INTEGER, vram_committed INTEGER,
    PRIMARY KEY (ts, pid)
) WITHOUT ROWID;
CREATE TABLE job_energy (
    job_id TEXT PRIMARY KEY,
    workspace TEXT NOT NULL,
    from_ms INTEGER NOT NULL, to_ms INTEGER NOT NULL,
    gpu_mj_gross INTEGER NOT NULL, gpu_mj_above_idle INTEGER NOT NULL,
    cpu_mj_gross INTEGER NOT NULL, cpu_mj_above_idle INTEGER NOT NULL,
    system_mj INTEGER NOT NULL, gpu_seconds_milli INTEGER NOT NULL,
    gpu_measured INTEGER NOT NULL, cpu_measured INTEGER NOT NULL,
    idle_unknown INTEGER NOT NULL
);
CREATE INDEX job_energy_to ON job_energy (to_ms);
";

const TABLES: [&str; 5] = ["s1", "m1", "h1", "proc_s1", "job_energy"];

/// The open database.
pub(crate) struct Db {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // The result row names the mode the file ended up in; a disposable store takes any.
    let _mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
}

fn create(conn: &Connection, layout: Layout<'_>) -> rusqlite::Result<()> {
    conn.execute_batch(layout.schema)?;
    conn.pragma_update(None, "user_version", layout.version)
}

/// Reads an existing file without touching it: its schema version (returned), which must be
/// the layout's or an older one it migrates from, and `quick_check`. The connection is
/// read-only (`sqlite_files::open_untouched`), so it never checkpoints a `-wal` into the file,
/// rolls a `-journal` back, or deletes a side file when it closes; a file that is then set
/// aside keeps every byte it had (CORE-004).
fn verify_existing(path: &Path, layout: Layout<'_>) -> Result<i64, String> {
    let conn =
        lil_platform::sqlite_files::open_untouched(path, false).map_err(|e| e.to_string())?;
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let steps = i64::try_from(layout.migrations.len()).unwrap_or(i64::MAX);
    if version > layout.version {
        return Err(format!(
            "user_version {version} is newer than this core's {}",
            layout.version
        ));
    }
    if version < 1 || version < layout.version - steps {
        return Err(format!(
            "user_version {version} is not a telemetry.db version"
        ));
    }
    let report = conn
        .prepare("PRAGMA quick_check")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| e.to_string())?;
    if report != ["ok"] {
        return Err(format!("quick_check: {}", report.join("; ")));
    }
    Ok(version)
}

/// Brings `conn`, at version `from`, to the layout's version: each step and its new
/// `user_version` in one transaction, so a step that fails leaves the file at the version
/// before it.
fn migrate(conn: &mut Connection, from: i64, layout: Layout<'_>) -> Result<(), String> {
    let steps = i64::try_from(layout.migrations.len()).unwrap_or(i64::MAX);
    for version in from..layout.version {
        // Entry `steps - (version_now - version)` takes `version` to `version + 1`.
        let step = usize::try_from(steps - (layout.version - version))
            .map_err(|_| format!("no migration from user_version {version}"))?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(layout.migrations[step])
            .and_then(|()| tx.pragma_update(None, "user_version", version + 1))
            .and_then(|()| tx.commit())
            .map_err(|e| format!("the migration from user_version {version} failed: {e}"))?;
        tracing::info!(from = version, to = version + 1, "telemetry.db migrated");
    }
    Ok(())
}

/// An existing file, checked ([`verify_existing`]), opened for writing, migrated to the
/// layout's version, and checked for every table. `Err` says why the file is not usable.
fn check_existing(path: &Path, layout: Layout<'_>) -> Result<Connection, String> {
    let version = verify_existing(path, layout)?;
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    configure(&conn).map_err(|e| e.to_string())?;
    migrate(&mut conn, version, layout)?;
    for table in TABLES {
        conn.prepare(&format!("SELECT * FROM {table} LIMIT 0"))
            .map_err(|e| format!("table {table}: {e}"))?;
    }
    Ok(conn)
}

fn open_file(path: &Path, now_utc_secs: i64, layout: Layout<'_>) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fsops::ensure_dir(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let exists = path
        .try_exists()
        .map_err(|e| format!("cannot look at {}: {e}", path.display()))?;
    // Side files without their database: a new database beside them could take an old
    // write-ahead log for its own, so they are set aside too.
    let orphans = !exists
        && !lil_platform::sqlite_files::orphan_sidecars(path)
            .map_err(|e| format!("cannot look beside {}: {e}", path.display()))?
            .is_empty();
    if exists || orphans {
        let verdict = if exists {
            check_existing(path, layout).map(Some)
        } else {
            Err("side files without a database".to_string())
        };
        match verdict {
            Ok(conn) => return Ok(conn.expect("a checked database")),
            Err(why) => {
                let stamp = lil_schema::time::compact(now_utc_secs);
                let moved = fsops::set_aside_db(path, &stamp)
                    .map_err(|e| format!("cannot set {} aside: {e}", path.display()))?;
                tracing::warn!(
                    path = %path.display(),
                    why,
                    moved = moved.len(),
                    "telemetry.db is set aside and recreated"
                );
            }
        }
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    configure(&conn).map_err(|e| e.to_string())?;
    create(&conn, layout).map_err(|e| e.to_string())?;
    Ok(conn)
}

impl Db {
    /// Opens (or sets aside and recreates) the database at `path` (module docs). Never fails:
    /// a database that can't be had on disk is an in-memory one.
    pub(crate) fn open(path: &Path, now_utc_secs: i64) -> Db {
        let conn = open_file(path, now_utc_secs, LAYOUT).unwrap_or_else(|why| {
            tracing::error!(path = %path.display(), why, "telemetry.db can't be opened; the telemetry is kept in memory only");
            let conn = Connection::open_in_memory().expect("SQLite opens in memory");
            create(&conn, LAYOUT).expect("the schema creates in memory");
            conn
        });
        Db {
            conn: Mutex::new(conn),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A statement that failed leaves the connection usable.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The newest sample time stored (`None` for an empty database).
    pub(crate) fn max_ts(&self) -> Option<i64> {
        let conn = self.lock();
        conn.query_row("SELECT MAX(ts) FROM s1", [], |r| r.get(0))
            .ok()
            .flatten()
    }

    /// Flushes the write-ahead log into the database file (at shutdown).
    pub(crate) fn checkpoint(&self) {
        let conn = self.lock();
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }

    /// Writes one sample and rolls it into the minute and hour rows, in one transaction.
    pub(crate) fn write(&self, row: &Row) -> rusqlite::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO s1 (ts, run, mono, dt, est, gpu_util, gpu_power, gpu_energy, gpu_temp, \
                 gpu_vram_used, cpu_util, cpu_power, cpu_energy, ram_used, system_power, \
                 net_rx, net_tx, cpu_ms_all) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            )?;
            insert.execute(params![
                row.ts,
                row.run,
                row.mono,
                row.dt,
                row.est,
                row.values[0],
                row.values[1],
                row.values[2],
                row.values[3],
                row.values[4],
                row.values[5],
                row.values[6],
                row.values[7],
                row.values[8],
                row.values[9],
                row.values[10],
                row.values[11],
                i64::try_from(row.cpu_ms_all).unwrap_or(i64::MAX),
            ])?;
            for (table, len) in [("m1", MINUTE_MS), ("h1", HOUR_MS)] {
                let bucket = row.ts - row.ts.rem_euclid(len);
                let mut upsert = tx.prepare_cached(&format!(
                    "INSERT INTO {table} (ts, metric, n, min, max, sum) VALUES (?1, ?2, 1, ?3, ?3, ?3) \
                     ON CONFLICT (ts, metric) DO UPDATE SET n = n + 1, \
                     min = MIN(min, excluded.min), max = MAX(max, excluded.max), \
                     sum = sum + excluded.sum"
                ))?;
                for m in Metric::ALL {
                    if let Some(v) = row.value(m) {
                        upsert.execute(params![bucket, m.name(), v])?;
                    }
                }
            }
            let mut proc = tx.prepare_cached(
                "INSERT INTO proc_s1 (ts, pid, sm, cpu_ms, ram_mib, vram_local, vram_committed) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (ts, pid) DO NOTHING",
            )?;
            for p in &row.procs {
                proc.execute(params![
                    row.ts,
                    p.pid,
                    p.sm,
                    i64::try_from(p.cpu_ms).unwrap_or(i64::MAX),
                    p.ram_mib.map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
                    p.vram_local.map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
                    p.vram_committed
                        .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
                ])?;
            }
        }
        tx.commit()
    }

    /// Drops what is past its retention at `now_ms`: samples and per-process rows older than
    /// 24 h, minute rows older than 30 days. Hourly rows and job energy stay (CORE-062).
    pub(crate) fn prune(&self, now_ms: i64) -> rusqlite::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM s1 WHERE ts < ?1", [now_ms - KEEP_S1_MS])?;
        tx.execute("DELETE FROM proc_s1 WHERE ts < ?1", [now_ms - KEEP_S1_MS])?;
        tx.execute("DELETE FROM m1 WHERE ts < ?1", [now_ms - KEEP_M1_MS])?;
        tx.commit()
    }

    /// `metric`'s points with `from_ms <= ts <= to_ms` (a coarser bucket counts when it holds
    /// any millisecond of the range), oldest first, at most `limit`.
    pub(crate) fn series(
        &self,
        metric: Metric,
        from_ms: i64,
        to_ms: i64,
        res: Resolution,
        limit: usize,
    ) -> rusqlite::Result<Vec<RawPoint>> {
        let conn = self.lock();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        match res {
            Resolution::Second => {
                let column = metric.column();
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT ts, {column} FROM s1 WHERE ts BETWEEN ?1 AND ?2 AND {column} IS NOT NULL \
                     ORDER BY ts LIMIT ?3"
                ))?;
                stmt.query_map(params![from_ms, to_ms, limit], |r| {
                    let v: i64 = r.get(1)?;
                    Ok(RawPoint {
                        ts: r.get(0)?,
                        min: v,
                        avg: v as f64,
                        max: v,
                    })
                })?
                .collect()
            }
            Resolution::Minute | Resolution::Hour => {
                let (table, len) = if res == Resolution::Minute {
                    ("m1", MINUTE_MS)
                } else {
                    ("h1", HOUR_MS)
                };
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT ts, n, min, max, sum FROM {table} \
                     WHERE metric = ?1 AND ts + ?2 > ?3 AND ts <= ?4 ORDER BY ts LIMIT ?5"
                ))?;
                let energy = metric.is_energy();
                stmt.query_map(params![metric.name(), len, from_ms, to_ms, limit], |r| {
                    let (n, sum): (i64, i64) = (r.get(1)?, r.get(4)?);
                    Ok(if energy {
                        RawPoint {
                            ts: r.get(0)?,
                            min: sum,
                            avg: sum as f64,
                            max: sum,
                        }
                    } else {
                        RawPoint {
                            ts: r.get(0)?,
                            min: r.get(2)?,
                            avg: sum as f64 / n.max(1) as f64,
                            max: r.get(3)?,
                        }
                    })
                })?
                .collect()
            }
        }
    }

    /// The figures of the samples of core run `run` whose monotonic time is in `(from, to]`
    /// (milliseconds; the monotonic clock restarts with each core), and the idle baselines before
    /// `from`: `wall_from` is `from` as a wall-clock time. A sample covers the second before its
    /// time, so the window is open at `from`: two windows that meet at a time count its sample
    /// once.
    pub(crate) fn slice(
        &self,
        run: i64,
        from_mono: i64,
        to_mono: i64,
        wall_from: i64,
    ) -> rusqlite::Result<EnergySlice> {
        let conn = self.lock();
        let mut slice = EnergySlice::default();
        let (mut rows, mut gpu_all, mut cpu_all) = (0_u64, true, true);
        let (mut first_ts, mut last_ts) = (i64::MAX, i64::MIN);
        let mut cpu_ms_total: u64 = 0;
        {
            let mut stmt = conn.prepare_cached(
                "SELECT ts, est, gpu_energy, cpu_energy, cpu_ms_all FROM s1 \
                 WHERE run = ?1 AND mono > ?2 AND mono <= ?3 ORDER BY ts",
            )?;
            let mut found = stmt.query(params![run, from_mono, to_mono])?;
            while let Some(r) = found.next()? {
                let ts: i64 = r.get(0)?;
                let est: i64 = r.get(1)?;
                let (gpu, cpu): (Option<i64>, Option<i64>) = (r.get(2)?, r.get(3)?);
                rows += 1;
                first_ts = first_ts.min(ts);
                last_ts = last_ts.max(ts);
                gpu_all &= gpu.is_some() && est & EST_GPU_ENERGY == 0;
                cpu_all &= cpu.is_some() && est & EST_CPU_ENERGY == 0;
                slice.gpu_mj += gpu.map_or(0, |v| u64::try_from(v).unwrap_or(0));
                slice.cpu_mj += cpu.map_or(0, |v| u64::try_from(v).unwrap_or(0));
                cpu_ms_total += u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0);
            }
        }
        slice.gpu_measured = rows > 0 && gpu_all;
        slice.cpu_measured = rows > 0 && cpu_all;
        if rows > 0 {
            let mut stmt = conn.prepare_cached(
                "SELECT p.pid, SUM(p.sm * s.dt), SUM(p.cpu_ms) FROM proc_s1 p \
                 JOIN s1 s ON s.ts = p.ts WHERE p.ts BETWEEN ?1 AND ?2 GROUP BY p.pid",
            )?;
            let mut found = stmt.query(params![first_ts, last_ts])?;
            let mut tracked_ms: u64 = 0;
            while let Some(r) = found.next()? {
                let pid = u32::try_from(r.get::<_, i64>(0)?).unwrap_or(0);
                let sm_ms = u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0);
                let cpu_ms = u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0);
                if sm_ms > 0 {
                    slice.sm_by_pid.insert(pid, sm_ms / 1000);
                }
                if cpu_ms > 0 {
                    slice.cpu_ms_by_pid.insert(pid, cpu_ms);
                    tracked_ms += cpu_ms;
                }
            }
            // Every other process together, under pid 0 (no real process has it), so the
            // map's sum is the machine's CPU time and a share of it is a real share.
            let others = cpu_ms_total.saturating_sub(tracked_ms);
            if others > 0 {
                slice.cpu_ms_by_pid.insert(0, others);
            }
        }
        slice.idle_gpu_mw = idle_baseline(
            &conn,
            run,
            (from_mono, wall_from),
            ("gpu_util", "gpu_power", 5, "gpu.power"),
        )?;
        slice.idle_cpu_mw = idle_baseline(
            &conn,
            run,
            (from_mono, wall_from),
            ("cpu_util", "cpu_power", 50, "cpu.power"),
        )?;
        Ok(slice)
    }

    /// The times (Unix ms) of the first and the last sample of core run `run` whose monotonic
    /// time is in `(from, to]`, as [`Db::slice`] selects them; `None` when there is none.
    pub(crate) fn sample_span(
        &self,
        run: i64,
        from_mono: i64,
        to_mono: i64,
    ) -> rusqlite::Result<Option<(i64, i64)>> {
        let conn = self.lock();
        let (first, last): (Option<i64>, Option<i64>) = conn.query_row(
            "SELECT MIN(ts), MAX(ts) FROM s1 WHERE run = ?1 AND mono > ?2 AND mono <= ?3",
            params![run, from_mono, to_mono],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(first.zip(last))
    }

    /// Writes (or replaces) the energy row of a job.
    pub(crate) fn put_job_energy(
        &self,
        e: &JobEnergy,
        from_ms: i64,
        to_ms: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.lock();
        let big = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        conn.execute(
            "INSERT OR REPLACE INTO job_energy (job_id, workspace, from_ms, to_ms, gpu_mj_gross, \
             gpu_mj_above_idle, cpu_mj_gross, cpu_mj_above_idle, system_mj, gpu_seconds_milli, \
             gpu_measured, cpu_measured, idle_unknown) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                e.job_id,
                e.workspace,
                from_ms,
                to_ms,
                big(e.gpu_mj_gross),
                big(e.gpu_mj_above_idle),
                big(e.cpu_mj_gross),
                big(e.cpu_mj_above_idle),
                big(e.system_mj),
                big(e.gpu_seconds_milli),
                e.measured.gpu,
                e.measured.cpu,
                e.idle_unknown,
            ],
        )?;
        Ok(())
    }

    /// The job rows `q` selects, newest (by end time) first. `from_ms` and `to_ms` are the
    /// query's bounds already parsed.
    pub(crate) fn job_energy(
        &self,
        q: &JobEnergyQuery,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: usize,
    ) -> rusqlite::Result<Vec<JobEnergy>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT job_id, workspace, from_ms, to_ms, gpu_mj_gross, gpu_mj_above_idle, \
             cpu_mj_gross, cpu_mj_above_idle, system_mj, gpu_seconds_milli, gpu_measured, \
             cpu_measured, idle_unknown FROM job_energy \
             WHERE (?1 IS NULL OR to_ms >= ?1) AND (?2 IS NULL OR from_ms <= ?2) \
             ORDER BY to_ms DESC, job_id",
        )?;
        let mut found = stmt.query(params![from_ms, to_ms])?;
        let mut out = Vec::new();
        let unsigned = |v: i64| u64::try_from(v).unwrap_or(0);
        while let Some(r) = found.next()? {
            let job_id: String = r.get(0)?;
            if let Some(ids) = &q.job_ids
                && !ids.contains(&job_id)
            {
                continue;
            }
            out.push(JobEnergy {
                job_id,
                workspace: r.get(1)?,
                from: super::format_ts(r.get(2)?),
                to: super::format_ts(r.get(3)?),
                gpu_mj_gross: unsigned(r.get(4)?),
                gpu_mj_above_idle: unsigned(r.get(5)?),
                cpu_mj_gross: unsigned(r.get(6)?),
                cpu_mj_above_idle: unsigned(r.get(7)?),
                system_mj: unsigned(r.get(8)?),
                gpu_seconds_milli: unsigned(r.get(9)?),
                measured: Measured {
                    gpu: r.get(10)?,
                    cpu: r.get(11)?,
                },
                idle_unknown: r.get(12)?,
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

/// The idle power before a window (CORE-065): the median power of the 60 s before it with
/// utilisation under 5 %; else the lowest 1-minute average of the last hour; else `None`.
/// `kind` is `(utilisation column, power column, utilisation limit, metric name)`.
fn idle_baseline(
    conn: &Connection,
    run: i64,
    (from_mono, wall_from): (i64, i64),
    (util, power, limit, metric): (&str, &str, i64, &str),
) -> rusqlite::Result<Option<u32>> {
    let mut quiet: Vec<u64> = {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {power} FROM s1 WHERE run = ?1 AND mono >= ?2 AND mono < ?3 \
             AND {util} < ?4 AND {power} IS NOT NULL"
        ))?;
        stmt.query_map(params![run, from_mono - 60_000, from_mono, limit], |r| {
            r.get::<_, i64>(0)
        })?
        .map(|v| v.map(|v| u64::try_from(v).unwrap_or(0)))
        .collect::<rusqlite::Result<_>>()?
    };
    if let Some(mw) = super::energy::median(&mut quiet) {
        return Ok(Some(u32::try_from(mw).unwrap_or(u32::MAX)));
    }
    let mut stmt = conn.prepare_cached(
        "SELECT MIN(CAST(sum AS REAL) / n) FROM m1 \
         WHERE metric = ?1 AND ts >= ?2 AND ts + ?3 <= ?4",
    )?;
    let lowest: Option<f64> = stmt.query_row(
        params![metric, wall_from - HOUR_MS, MINUTE_MS, wall_from],
        |r| r.get(0),
    )?;
    Ok(lowest.map(|mw| mw.max(0.0).floor() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: i64, values: &[(Metric, i64)]) -> Row {
        let mut row = Row {
            ts,
            run: 1,
            mono: ts,
            dt: 1000,
            est: 0,
            values: [None; 12],
            cpu_ms_all: 0,
            procs: Vec::new(),
        };
        for (m, v) in values {
            row.set(*m, Some(*v));
        }
        row
    }

    #[test] // spec: CORE-062
    fn rolled_up_rows_keep_min_avg_max_and_summed_energy() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("telemetry.db"), 1_800_000_000);
        let minute = 1_800_000_000_000 - 1_800_000_000_000 % MINUTE_MS;
        for (i, (power, energy)) in [(100, 10), (300, 20), (200, 30)].into_iter().enumerate() {
            let r = row(
                minute + i as i64 * 1000,
                &[(Metric::GpuPower, power), (Metric::GpuEnergy, energy)],
            );
            db.write(&r).unwrap();
        }
        let (from, to) = (minute, minute + 59_999);
        let power = db
            .series(Metric::GpuPower, from, to, Resolution::Minute, 10)
            .unwrap();
        assert_eq!(
            power,
            [RawPoint {
                ts: minute,
                min: 100,
                avg: 200.0,
                max: 300
            }]
        );
        let energy = db
            .series(Metric::GpuEnergy, from, to, Resolution::Minute, 10)
            .unwrap();
        assert_eq!(
            energy,
            [RawPoint {
                ts: minute,
                min: 60,
                avg: 60.0,
                max: 60
            }]
        );
        let hourly = db
            .series(Metric::GpuEnergy, from, to, Resolution::Hour, 10)
            .unwrap();
        assert_eq!(hourly[0].avg, 60.0, "the hour row has the same exact sum");
        // 1 s resolution: each sample is its own point, the bounds inclusive.
        let each = db
            .series(
                Metric::GpuEnergy,
                minute,
                minute + 1000,
                Resolution::Second,
                10,
            )
            .unwrap();
        assert_eq!(each.iter().map(|p| p.avg).collect::<Vec<_>>(), [10.0, 20.0]);
        let none = db
            .series(
                Metric::GpuEnergy,
                minute + 1,
                minute + 999,
                Resolution::Second,
                10,
            )
            .unwrap();
        assert!(none.is_empty());
        // A bucket counts when it holds any of the range: here the range sits inside it.
        let inside = db
            .series(
                Metric::GpuPower,
                minute + 10_000,
                minute + 20_000,
                Resolution::Minute,
                10,
            )
            .unwrap();
        assert_eq!(inside.len(), 1);
        let after = db
            .series(
                Metric::GpuPower,
                minute + MINUTE_MS,
                minute + 2 * MINUTE_MS,
                Resolution::Minute,
                10,
            )
            .unwrap();
        assert!(after.is_empty());
    }

    #[test] // spec: CORE-062
    fn prune_drops_old_samples_and_minute_rows_but_keeps_hours() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("telemetry.db"), 1_800_000_000);
        let now = 1_800_000_000_000_i64;
        let ages = [
            now - KEEP_M1_MS - 2 * HOUR_MS,
            now - KEEP_S1_MS - 2 * HOUR_MS,
            now - 1000,
        ];
        for ts in ages {
            db.write(&row(ts, &[(Metric::RamUsed, 7)])).unwrap();
        }
        db.prune(now).unwrap();
        let count = |table: &str| -> i64 {
            db.lock()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count("s1"), 1, "samples past 24 h");
        assert_eq!(count("m1"), 2, "minute rows past 30 days");
        assert_eq!(count("h1"), 3, "hourly rows are forever");
    }

    /// An older file is migrated forward in place and keeps its history; only a newer version,
    /// version 0 or a migration that fails sets a file aside, and nothing is deleted (CORE-004,
    /// CORE-062). The layout stands in for a later schema (a version 2 with one more table).
    #[test] // spec: CORE-004
    fn an_older_version_migrates_in_place_and_only_a_newer_or_broken_one_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let v2_schema = format!("{SCHEMA}CREATE TABLE later (x INTEGER);");
        let v2 = Layout {
            schema: &v2_schema,
            version: 2,
            migrations: &["CREATE TABLE later (x INTEGER);"],
        };
        let names = || -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("telemetry.db.damaged-"))
                .collect();
            names.sort();
            names
        };
        let db = Db::open(&path, 1_800_000_000);
        db.write(&row(1, &[(Metric::RamUsed, 7)])).unwrap();
        drop(db);

        // Version 1 under a core that knows version 2: migrated, every row kept.
        let conn = open_file(&path, 1_800_000_060, v2).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let kept: i64 = conn
            .query_row("SELECT ram_used FROM s1 WHERE ts = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 7, "the history survives the upgrade");
        conn.prepare("SELECT x FROM later").unwrap();
        drop(conn);
        assert!(names().is_empty(), "{:?}", names());

        // The same file (now version 2) under this core's version 1: newer, so set aside.
        let db = Db::open(&path, 1_800_000_120);
        drop(db);
        assert_eq!(names(), ["telemetry.db.damaged-20270115T080200Z"]);

        // Version 0 (not a telemetry.db) is set aside; so is a file whose migration fails,
        // and the failed step leaves it at its old version.
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 0).unwrap();
        }
        drop(open_file(&path, 1_800_000_180, LAYOUT).unwrap());
        let broken = Layout {
            schema: &v2_schema,
            version: 2,
            migrations: &["CREATE TABLE s1 (x INTEGER);"],
        };
        drop(open_file(&path, 1_800_000_240, broken).unwrap());
        assert_eq!(
            names(),
            [
                "telemetry.db.damaged-20270115T080200Z",
                "telemetry.db.damaged-20270115T080300Z",
                "telemetry.db.damaged-20270115T080400Z",
            ]
        );
        let aside =
            Connection::open(dir.path().join("telemetry.db.damaged-20270115T080400Z")).unwrap();
        let version: i64 = aside
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1, "a failed step is rolled back");
    }

    /// A file that isn't a database, and one of a newer schema version, are set aside whole
    /// and replaced (CORE-004).
    #[test] // spec: CORE-004
    fn a_damaged_or_other_version_file_is_set_aside_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        lil_testkit::files::write_file(&path, &[0xAB_u8; 4096]);
        let db = Db::open(&path, 1_800_000_000);
        db.write(&row(1, &[(Metric::RamUsed, 1)])).unwrap();
        drop(db);
        let aside = dir.path().join("telemetry.db.damaged-20270115T080000Z");
        assert_eq!(std::fs::read(&aside).unwrap(), vec![0xAB_u8; 4096]);

        // The same, for a good database of another version; its side files go with it.
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 7).unwrap();
        }
        let db = Db::open(&path, 1_800_000_060);
        drop(db);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n == "telemetry.db.damaged-20270115T080100Z"),
            "{names:?}"
        );
        assert!(names.iter().any(|n| n == "telemetry.db"), "{names:?}");
    }
}
