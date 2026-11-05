//! S0.3 (APP-138, CORE-042; P4b-R10, P4b-R23): `assets.query`'s page of 200 and its six
//! disjunctive facet counts in one read transaction, over a synthetic `index.db`.
//!
//! `facet-bench --work <dir> --out <json> [--sizes 10000,50000] [--runs 100]`
//!
//! The schema is today's `assets` table (crates/lil-core/src/index/schema.rs) with the "schema 2
//! adds" of the plan's Task 5 Interfaces block. The keys the plan leaves open are noted in
//! docs/spikes/12-browse-core.md §3.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use browse_core::stats::summary;
use browse_core::{Rng, arg, num_arg, path_arg, write_json};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags, params, params_from_iter};
use serde_json::{Value, json};

const SCHEMA: &str = "
CREATE TABLE assets (
    id INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    asset_type TEXT,
    status TEXT,
    version TEXT,
    approved_version TEXT,
    manifest TEXT NOT NULL,
    creation_uuid TEXT,
    updated INTEGER NOT NULL,
    needs_repair INTEGER NOT NULL,
    rating INTEGER NOT NULL DEFAULT 0,
    label TEXT,
    runs_waiting INTEGER NOT NULL DEFAULT 0,
    external INTEGER NOT NULL DEFAULT 0,
    working_doc INTEGER NOT NULL DEFAULT 0,
    display TEXT,
    last_change TEXT,
    repair TEXT,
    UNIQUE (project, asset_id)
);
CREATE INDEX assets_by_status ON assets(COALESCE(status, ''), project, asset_id);
CREATE INDEX assets_by_type ON assets(COALESCE(asset_type, ''), project, asset_id);
CREATE INDEX assets_by_updated ON assets(updated, project, asset_id);
CREATE TABLE asset_tags (
    project TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    tag_fold TEXT NOT NULL,
    PRIMARY KEY (project, asset_id, tag_fold)
) WITHOUT ROWID;
CREATE INDEX asset_tags_fold ON asset_tags(tag_fold);
CREATE TABLE take_marks (
    project TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    take INTEGER NOT NULL,
    flag TEXT,
    rating INTEGER,
    PRIMARY KEY (project, asset_id, run_id, take)
) WITHOUT ROWID;
CREATE INDEX take_marks_flag ON take_marks(flag);
";

const STATUSES: [(&str, u64); 10] = [
    ("planned", 8),
    ("in_progress", 20),
    ("candidate", 18),
    ("in_review", 10),
    ("approved", 25),
    ("exported", 6),
    ("released", 5),
    ("stale", 4),
    ("blocked", 2),
    ("deprecated", 2),
];
const TYPES: [(&str, u64); 8] = [
    ("image", 30),
    ("texture", 15),
    ("sprite", 15),
    ("concept", 12),
    ("model", 10),
    ("ui", 8),
    ("audio", 6),
    ("document", 4),
];
const LABELS: [Option<&str>; 6] = [
    None,
    Some("red"),
    Some("yellow"),
    Some("green"),
    Some("blue"),
    Some("purple"),
];
const BY: [&str; 4] = ["user", "assistant", "claude-code", "core"];

fn weighted<'a>(rng: &mut Rng, table: &[(&'a str, u64)]) -> &'a str {
    let total: u64 = table.iter().map(|t| t.1).sum();
    let mut r = rng.below(total);
    for (v, w) in table {
        if r < *w {
            return v;
        }
        r -= w;
    }
    table[0].0
}

fn hex64(rng: &mut Rng) -> String {
    (0..4).map(|_| format!("{:016x}", rng.next_u64())).collect()
}

fn build(path: &Path, n: usize) -> f64 {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let t = Instant::now();
    let mut conn = Connection::open(path).expect("open");
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))
        .unwrap();
    assert!(mode.eq_ignore_ascii_case("wal"));
    conn.pragma_update(None, "synchronous", "NORMAL").unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let mut rng = Rng::new(n as u64);
    let tx = conn.transaction().unwrap();
    {
        let mut a = tx.prepare("INSERT INTO assets (id, project, asset_id, asset_type, status, version, approved_version, manifest, creation_uuid, updated, needs_repair, rating, label, runs_waiting, external, working_doc, display, last_change, repair) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)").unwrap();
        let mut tg = tx
            .prepare(
                "INSERT INTO asset_tags (project, asset_id, tag, tag_fold) VALUES (?1, ?2, ?3, ?4)",
            )
            .unwrap();
        let mut k = tx
            .prepare("INSERT INTO take_marks (project, asset_id, run_id, take, flag, rating) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
            .unwrap();
        for i in 0..n {
            let id = i as i64 + 1;
            let project = format!("project-{:02}", rng.below(10));
            // Ids in random order, so the page's sort isn't the insertion order.
            let asset_id = format!("a{:08x}-{i}", rng.next_u64() as u32);
            let status = weighted(&mut rng, &STATUSES);
            let kind = weighted(&mut rng, &TYPES);
            let rating = [0, 0, 0, 1, 2, 3, 3, 4, 4, 5][rng.below(10) as usize];
            let label = if rng.below(3) == 0 {
                LABELS[1 + rng.below(5) as usize]
            } else {
                None
            };
            let manifest = format!("projects/{project}/assets/{asset_id}/manifest.yaml");

            let mut tags = std::collections::BTreeSet::new();
            while tags.len() < 3 {
                tags.insert(rng.below(200));
            }
            for t in tags {
                let tag = format!("Tag{t:03}");
                tg.execute(params![project, asset_id, tag, tag.to_lowercase()])
                    .unwrap();
            }
            // Runs: 1-3, 4 takes each. A trashed run's takes are not mirrored (P4b-R40).
            let mut open_runs = 0i64;
            for run in 0..(1 + rng.below(3)) {
                let run_id = format!("r{run}");
                let state = match rng.below(10) {
                    0 => "trashed",
                    1..=3 => "open",
                    _ => "signed_off",
                };
                if state == "open" {
                    open_runs += 1;
                }
                for take in 1..=4i64 {
                    let flag = match rng.below(100) {
                        0..=9 => Some("pick"),
                        10..=14 => Some("reject"),
                        _ => None,
                    };
                    let tr = if rng.below(4) == 0 {
                        Some(rng.below(6) as i64)
                    } else {
                        None
                    };
                    if state != "trashed" {
                        k.execute(params![project, asset_id, run_id, take, flag, tr])
                            .unwrap();
                    }
                }
            }
            // The tile fields (P4b-R30): `display` about 150 bytes, `last_change` about 60.
            let display = (rng.below(10) != 0).then(|| {
                json!({"path": format!("projects/{project}/assets/{asset_id}/candidates/r0/take-1.png"), "sha256": hex64(&mut rng), "media_kind": "image"}).to_string()
            });
            let last_change = (rng.below(20) != 0).then(|| {
                json!({"to": status, "by": BY[rng.below(4) as usize], "utc": format!("2026-{:02}-{:02}T{:02}:{:02}:{:02}Z", 1 + rng.below(10), 1 + rng.below(28), rng.below(24), rng.below(60), rng.below(60))}).to_string()
            });
            let repair = (rng.below(100) == 0).then(|| {
                json!({"kind": "moved", "from": format!("{project}/old-{asset_id}"), "to": format!("{project}/{asset_id}")}).to_string()
            });
            a.execute(params![
                id,
                project,
                asset_id,
                kind,
                status,
                "1.0.0",
                if status == "approved" {
                    Some("1.0.0")
                } else {
                    None
                },
                manifest,
                format!("{:032x}", rng.next_u64()),
                1_700_000_000_000i64 + rng.below(100_000_000_000) as i64,
                rating,
                label,
                open_runs,
                i64::from(rng.below(20) == 0),
                i64::from(rng.below(10) == 0),
                display,
                last_change,
                repair
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
    conn.execute_batch("ANALYZE; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    t.elapsed().as_secs_f64()
}

/// The filters of one query, each a WHERE fragment over `a` with its arguments.
struct Filters {
    parts: Vec<(&'static str, String, Vec<Sql>)>,
}

impl Filters {
    fn none() -> Filters {
        Filters { parts: Vec::new() }
    }

    fn five() -> Filters {
        let t = |s: &str| Sql::Text(s.into());
        Filters {
            parts: vec![
                ("status", "a.status IN (?, ?)".into(), vec![t("approved"), t("in_review")]),
                ("tags", "(a.project, a.asset_id) IN (SELECT project, asset_id FROM asset_tags WHERE tag_fold IN (?, ?))".into(), vec![t("tag007"), t("tag042")]),
                ("label", "(a.label IN (?) OR a.label IS NULL)".into(), vec![t("red")]),
                ("rating", "a.rating >= ?".into(), vec![Sql::Integer(2)]),
                ("flag", format!("(a.project, a.asset_id) IN ({})", flagged("?")), vec![t("pick")]),
            ],
        }
    }

    /// The WHERE clause and arguments with every filter except `skip` (P4b-R10).
    fn clause(&self, skip: Option<&str>) -> (String, Vec<Sql>) {
        let mut frags = Vec::new();
        let mut args = Vec::new();
        for (name, frag, a) in &self.parts {
            if Some(*name) == skip {
                continue;
            }
            frags.push(frag.clone());
            args.extend(a.iter().cloned());
        }
        if frags.is_empty() {
            (String::new(), args)
        } else {
            (format!(" WHERE {}", frags.join(" AND ")), args)
        }
    }
}

/// The assets with a take flagged `flag` (trashed runs aren't mirrored, P4b-R40): the filter's
/// form, one pass over the `flag` index.
fn flagged(flag: &str) -> String {
    format!("SELECT project, asset_id FROM take_marks WHERE flag = {flag}")
}

/// Whether asset `a` has a take flagged `flag`: the facet's form, by the primary key. (A row-value
/// `IN (SELECT …)` in a result column took about 2 s at 10,000 assets: SQLite 3.53 searched its
/// list per row.)
fn has_take(flag: &str) -> String {
    format!(
        "SELECT 1 FROM take_marks k WHERE k.project = a.project AND k.asset_id = a.asset_id AND k.flag = {flag}"
    )
}

const PAGE_COLUMNS: &str = "a.project, a.asset_id, a.asset_type, a.status, a.version, a.approved_version, a.manifest, a.creation_uuid, a.updated, a.needs_repair, \
    a.rating, a.label, a.runs_waiting, a.external, a.working_doc, a.display, a.last_change, a.repair, \
    (SELECT group_concat(t.tag, char(31)) FROM asset_tags t WHERE t.project = a.project AND t.asset_id = a.asset_id)";

/// The statements of one `assets.query {limit: 200, facets: true}`: (name, SQL, args).
fn statements(f: &Filters) -> Vec<(&'static str, String, Vec<Sql>)> {
    let (all, all_args) = f.clause(None);
    let mut page_args = all_args.clone();
    page_args.push(Sql::Integer(201));
    let group = |name: &'static str, col: &str| {
        let (w, a) = f.clause(Some(name));
        (
            name,
            format!("SELECT {col}, count(*) FROM assets a{w} GROUP BY {col}"),
            a,
        )
    };
    let (wt, at) = f.clause(Some("tags"));
    let (wf, af) = f.clause(Some("flag"));
    // With no other filter the tags facet needs no join to `assets`.
    let tags = if wt.is_empty() {
        "SELECT tag_fold, min(tag), count(*) AS c FROM asset_tags GROUP BY tag_fold ORDER BY c DESC, tag_fold ASC LIMIT 101".to_string()
    } else {
        format!(
            "SELECT t.tag_fold, min(t.tag), count(*) AS c FROM asset_tags t JOIN assets a ON a.project = t.project AND a.asset_id = t.asset_id{wt} GROUP BY t.tag_fold ORDER BY c DESC, t.tag_fold ASC LIMIT 101"
        )
    };
    vec![
        (
            "page",
            format!(
                "SELECT {PAGE_COLUMNS} FROM assets a{all} ORDER BY a.asset_id ASC, a.project ASC LIMIT ?"
            ),
            page_args,
        ),
        (
            "total",
            format!("SELECT count(*) FROM assets a{all}"),
            all_args,
        ),
        group("status", "a.status"),
        group("type", "a.asset_type"),
        group("rating", "a.rating"),
        group("label", "a.label"),
        ("tags", tags, at),
        (
            "flag",
            format!(
                "SELECT sum(p), sum(j), sum(NOT p AND NOT j) FROM (SELECT EXISTS ({}) AS p, EXISTS ({}) AS j FROM assets a{wf})",
                has_take("'pick'"),
                has_take("'reject'")
            ),
            af,
        ),
    ]
}

/// One query: every statement in one read transaction. Returns the row count of each.
fn run_once(
    conn: &mut Connection,
    stmts: &[(&'static str, String, Vec<Sql>)],
) -> BTreeMap<&'static str, usize> {
    let tx = conn.transaction().unwrap();
    let mut rows = BTreeMap::new();
    for (name, sql, args) in stmts {
        let mut st = tx.prepare_cached(sql).unwrap();
        let cols = st.column_count();
        let mut q = st.query(params_from_iter(args.iter())).unwrap();
        let mut n = 0usize;
        while let Some(r) = q.next().unwrap() {
            for c in 0..cols {
                let _: Sql = r.get(c).unwrap();
            }
            n += 1;
        }
        rows.insert(*name, n);
    }
    tx.commit().unwrap();
    rows
}

fn plans(conn: &Connection, stmts: &[(&'static str, String, Vec<Sql>)]) -> Value {
    let mut out = serde_json::Map::new();
    for (name, sql, args) in stmts {
        let mut st = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let lines: Vec<String> = st
            .query_map(params_from_iter(args.iter()), |r| {
                Ok(format!(
                    "{}|{}|{}",
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(3)?
                ))
            })
            .unwrap()
            .map(|l| l.unwrap())
            .collect();
        out.insert((*name).into(), json!({"sql": sql, "plan": lines}));
    }
    Value::Object(out)
}

fn main() {
    let work = path_arg("--work");
    let out = path_arg("--out");
    let runs: usize = num_arg("--runs", 100);
    let sizes: Vec<usize> = arg("--sizes")
        .unwrap_or_else(|| "10000,50000".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    std::fs::create_dir_all(&work).unwrap();
    let mut results = Vec::new();
    // The worst filter set's p95 per (assets, cache MiB; 0 = SQLite's default).
    let mut worst: BTreeMap<(usize, i64), f64> = BTreeMap::new();
    for &n in &sizes {
        let db = work.join(format!("index-{n}.db"));
        let build_s = build(&db, n);
        let open = || {
            Connection::open_with_flags(
                &db,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .unwrap()
        };
        let conn = open();
        let counts: (i64, i64, f64) = conn.query_row("SELECT (SELECT count(*) FROM asset_tags), (SELECT count(*) FROM take_marks), (SELECT total(runs_waiting) FROM assets)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        let db_mib = std::fs::metadata(&db)
            .map(|m| m.len() as f64 / 1048576.0)
            .unwrap_or(0.0);
        drop(conn);
        let mut sets = Vec::new();
        // The bar's verdict uses SQLite's default page cache (today's index sets none, and §3 fixed
        // it before the run); the 64 MiB cache the owner chose (2026-11-05) is measured beside it.
        for cache_mib in [0i64, 64] {
            let mut conn = open();
            if cache_mib > 0 {
                conn.pragma_update(None, "cache_size", -cache_mib * 1024)
                    .unwrap();
            }
            for (set, f) in [("none", Filters::none()), ("five", Filters::five())] {
                let stmts = statements(&f);
                for _ in 0..5 {
                    run_once(&mut conn, &stmts);
                }
                let mut total = Vec::new();
                let mut rows = BTreeMap::new();
                for _ in 0..runs {
                    let t = Instant::now();
                    rows = run_once(&mut conn, &stmts);
                    total.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                // Recorded: each statement alone (the same warm connection).
                let mut per_statement = serde_json::Map::new();
                for s in &stmts {
                    let one = std::slice::from_ref(s);
                    let mut times = Vec::new();
                    for _ in 0..20 {
                        let t = Instant::now();
                        run_once(&mut conn, one);
                        times.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    per_statement.insert(s.0.into(), summary(&times));
                }
                let sum = summary(&total);
                let p95 = sum["p95"].as_f64().unwrap_or(f64::INFINITY);
                let w = worst.entry((n, cache_mib)).or_insert(0.0);
                *w = w.max(p95);
                println!(
                    "facets n={n} cache={} set={set}: p50 {} p95 {} ms",
                    if cache_mib == 0 {
                        "default".to_string()
                    } else {
                        format!("{cache_mib} MiB")
                    },
                    sum["p50"],
                    sum["p95"]
                );
                sets.push(json!({"filters": set, "cache_mib": if cache_mib == 0 { Value::Null } else { json!(cache_mib) }, "runs": runs, "total_ms": sum, "rows": rows, "per_statement_ms": per_statement, "plans": plans(&conn, &stmts), "total_ms_raw": total}));
            }
        }
        results.push(json!({"assets": n, "asset_tags": counts.0, "take_marks": counts.1, "open_runs": counts.2, "db_mib": db_mib, "build_s": build_s, "sets": sets}));
    }
    let get = |n: usize, c: i64| worst.get(&(n, c)).copied();
    let pass = get(10_000, 0).is_some_and(|p| p < 100.0);
    // P4b-R23: twice the p95, rounded up to 10 ms.
    let budget = |p: Option<f64>| p.map(|p| ((2.0 * p) / 10.0).ceil() * 10.0);
    let (p10, p50) = (get(10_000, 0), get(50_000, 0));
    let (c10, c50) = (get(10_000, 64), get(50_000, 64));
    write_json(
        &out,
        &json!({"sqlite": rusqlite::version(), "sizes": results,
        "p95_10000_worst_ms": p10, "budget_10000_ms": budget(p10),
        "p95_50000_worst_ms": p50, "budget_50000_ms": budget(p50),
        "cache_64_mib": {"p95_10000_worst_ms": c10, "budget_10000_ms": budget(c10), "p95_50000_worst_ms": c50, "budget_50000_ms": budget(c50)},
        "pass": pass}),
    );
}
