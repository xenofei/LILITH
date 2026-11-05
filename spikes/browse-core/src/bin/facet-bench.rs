//! S0.3 (APP-138, CORE-042; P4b-R10, P4b-R23): `assets.query`'s page of 200 and its six
//! disjunctive facet counts in one read transaction, over a synthetic `index.db`.
//!
//! `facet-bench --work <dir> --out <json> [--sizes 10000,50000] [--runs 100]`
//!
//! The schema is today's `assets` table (crates/lil-core/src/index/schema.rs) plus a draft of
//! Task 5's additions; the draft is this spike's guess (docs/spikes/12-browse-core.md §3).

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
    UNIQUE (project, asset_id)
);
CREATE INDEX assets_by_status ON assets(COALESCE(status, ''), project, asset_id);
CREATE INDEX assets_by_type ON assets(COALESCE(asset_type, ''), project, asset_id);
CREATE INDEX assets_by_updated ON assets(updated, project, asset_id);
CREATE INDEX assets_by_rating ON assets(rating);
CREATE INDEX assets_by_label ON assets(label);
CREATE TABLE asset_tags (
    asset INTEGER NOT NULL,
    tag_key TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (asset, tag_key)
) WITHOUT ROWID;
CREATE INDEX asset_tags_by_tag ON asset_tags(tag_key, asset);
CREATE TABLE runs (
    asset INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY (asset, run_id)
) WITHOUT ROWID;
CREATE TABLE takes (
    asset INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    take INTEGER NOT NULL,
    flag TEXT,
    rating INTEGER,
    PRIMARY KEY (asset, run_id, take)
) WITHOUT ROWID;
CREATE INDEX takes_flagged ON takes(flag, asset) WHERE flag IS NOT NULL;
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
        let mut a = tx.prepare("INSERT INTO assets (id, project, asset_id, asset_type, status, version, approved_version, manifest, creation_uuid, updated, needs_repair, rating, label) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12)").unwrap();
        let mut tg = tx
            .prepare("INSERT INTO asset_tags (asset, tag_key, tag) VALUES (?1, ?2, ?3)")
            .unwrap();
        let mut r = tx
            .prepare("INSERT INTO runs (asset, run_id, state) VALUES (?1, ?2, ?3)")
            .unwrap();
        let mut k = tx
            .prepare(
                "INSERT INTO takes (asset, run_id, take, flag, rating) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
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
                label
            ])
            .unwrap();
            let mut tags = std::collections::BTreeSet::new();
            while tags.len() < 3 {
                tags.insert(rng.below(200));
            }
            for t in tags {
                let tag = format!("Tag{t:03}");
                tg.execute(params![id, tag.to_lowercase(), tag]).unwrap();
            }
            for run in 0..(1 + rng.below(3)) {
                let run_id = format!("r{run}");
                let state = match rng.below(10) {
                    0 => "trashed",
                    1..=3 => "open",
                    _ => "signed_off",
                };
                r.execute(params![id, run_id, state]).unwrap();
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
                    k.execute(params![id, run_id, take, flag, tr]).unwrap();
                }
            }
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
                ("tags", "EXISTS (SELECT 1 FROM asset_tags t WHERE t.asset = a.id AND t.tag_key IN (?, ?))".into(), vec![t("tag007"), t("tag042")]),
                ("label", "(a.label IN (?) OR a.label IS NULL)".into(), vec![t("red")]),
                ("rating", "a.rating >= ?".into(), vec![Sql::Integer(2)]),
                ("flag", format!("EXISTS ({})", flagged("?")), vec![t("pick")]),
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

/// A take of `a` with flag `flag` in a run that isn't trashed (P4b-R40).
fn flagged(flag: &str) -> String {
    format!(
        "SELECT 1 FROM takes k JOIN runs r ON r.asset = k.asset AND r.run_id = k.run_id WHERE k.asset = a.id AND k.flag = {flag} AND r.state <> 'trashed'"
    )
}

const PAGE_COLUMNS: &str = "a.project, a.asset_id, a.asset_type, a.status, a.version, a.approved_version, a.manifest, a.creation_uuid, a.updated, a.needs_repair, a.rating, a.label, \
    (SELECT group_concat(t.tag, char(31)) FROM asset_tags t WHERE t.asset = a.id), \
    (SELECT count(*) FROM runs r WHERE r.asset = a.id AND r.state = 'open')";

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
        (
            "tags",
            format!(
                "SELECT t.tag_key, count(*) AS c FROM asset_tags t JOIN assets a ON a.id = t.asset{wt} GROUP BY t.tag_key ORDER BY c DESC, t.tag_key ASC LIMIT 101"
            ),
            at,
        ),
        (
            "flag",
            format!(
                "SELECT sum(p), sum(j), sum(NOT p AND NOT j) FROM (SELECT EXISTS ({}) AS p, EXISTS ({}) AS j FROM assets a{wf})",
                flagged("'pick'"),
                flagged("'reject'")
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
    let mut p95_10000_worst: Option<f64> = None;
    let mut p95_50000_worst: Option<f64> = None;
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
        let counts: (i64, i64, i64) = conn.query_row("SELECT (SELECT count(*) FROM asset_tags), (SELECT count(*) FROM runs), (SELECT count(*) FROM takes)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        let db_mib = std::fs::metadata(&db)
            .map(|m| m.len() as f64 / 1048576.0)
            .unwrap_or(0.0);
        drop(conn);
        let mut sets = Vec::new();
        // The verdict uses SQLite's default page cache (today's index sets none); a 64 MiB
        // cache is recorded beside it.
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
                let worst = match (n, cache_mib) {
                    (10_000, 0) => Some(&mut p95_10000_worst),
                    (50_000, 0) => Some(&mut p95_50000_worst),
                    _ => None,
                };
                if let Some(w) = worst {
                    *w = Some(w.map_or(p95, |v: f64| v.max(p95)));
                }
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
        results.push(json!({"assets": n, "asset_tags": counts.0, "runs": counts.1, "takes": counts.2, "db_mib": db_mib, "build_s": build_s, "sets": sets}));
    }
    let pass = p95_10000_worst.is_some_and(|p| p < 100.0);
    // P4b-R23: twice the p95, rounded up to 10 ms.
    let budget = |p: Option<f64>| p.map(|p| ((2.0 * p) / 10.0).ceil() * 10.0);
    write_json(
        &out,
        &json!({"sqlite": rusqlite::version(), "sizes": results,
        "p95_10000_worst_ms": p95_10000_worst, "budget_10000_ms": budget(p95_10000_worst),
        "p95_50000_worst_ms": p95_50000_worst, "budget_50000_ms": budget(p95_50000_worst), "pass": pass}),
    );
}
