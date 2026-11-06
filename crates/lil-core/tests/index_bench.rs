//! The index budgets (CORE-042, APP-138): a cold rebuild of 2,000 assets and a warm reconcile
//! of 10,000 assets with nothing changed, and `assets.query {limit: 200, facets: true}` at
//! 10,000 and 50,000 assets (P4b-R23), each against its budget in `test-params.toml`
//! (`[budgets]`, read through `lil_testkit::params::budget`).
//!
//! These are the suite's only wall-clock measurements, the stated exception to QA-031: they
//! are performance gates, not correctness assertions. Each prints its measured time. The facet
//! benches are `#[ignore = "bench"]` (their workspaces take minutes to build): run them with
//! `cargo nextest run -p lil-core --run-ignored only -E 'binary(index_bench) & test(/facets/)'`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use lil_core::index::{AssetQuery, Choice, Index, Scope};
use lil_platform::fs::SystemProbe;
use lil_schema::{Flag, Label};
use lil_testkit::files::write_file;
use lil_testkit::{TestEnv, params, sha256_bytes};
use serde_json::json;

/// Creates a folder as an outside program would.
#[allow(clippy::disallowed_methods)]
fn make_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}

/// `assets` assets in three projects, each a manifest and two files its `hashes` lists.
fn workspace(env: &TestEnv, assets: usize) -> PathBuf {
    // A bare folder: only the asset files.
    let ws = env.root().join("ws-bench");
    make_dir(&ws);
    let ws = std::fs::canonicalize(&ws).unwrap();
    let ws = PathBuf::from(
        ws.to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or_else(|| ws.to_string_lossy().into_owned()),
    );
    for i in 0..assets {
        let project = ["alpha", "beta", "gamma"][i % 3];
        let asset = format!("asset_{i:05}");
        let dir: PathBuf = ["projects", project, "assets", &asset]
            .iter()
            .fold(ws.clone(), |p, c| p.join(c));
        let image = format!("image {i}").into_bytes();
        let notes = format!("notes {i}").into_bytes();
        write_file(&dir.join("v1.0.0").join("image.png"), &image);
        write_file(&dir.join("v1.0.0").join("notes.txt"), &notes);
        let manifest = json!({
            "schema_version": 1,
            "asset_id": asset,
            "project": project,
            "creation_uuid": format!("00000000-0000-4000-8000-{i:012x}"),
            "type": "image",
            "status": "approved",
            "version": "1.0.0",
            "approved_version": "1.0.0",
            "hashes": {
                "v1.0.0/image.png": format!("sha256:{}", sha256_bytes(&image)),
                "v1.0.0/notes.txt": format!("sha256:{}", sha256_bytes(&notes)),
            },
        });
        write_file(
            &dir.join(format!("{asset}.manifest.json")),
            manifest.to_string().as_bytes(),
        );
    }
    ws
}

fn open(env: &TestEnv, ws: &Path) -> Index {
    Index::open(
        &env.root().join("local-bench").join("index.db"),
        ws,
        Arc::new(SystemProbe),
    )
    .expect("the index opens")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-042
async fn cold_rebuild_of_2000_assets_fits_the_budget() {
    let env = TestEnv::new();
    let setup = Instant::now();
    let ws = workspace(&env, 2000);
    let index = open(&env, &ws);
    println!("setup of 2,000 assets x 3 files: {:.2?}", setup.elapsed());

    let started = Instant::now();
    let report = index.rebuild().await.expect("the rebuild");
    let took = started.elapsed().as_secs_f64();
    let budget = params::budget("index_rebuild_2000_assets_s");
    println!(
        "cold rebuild of 2,000 assets: {took:.3} s (budget {budget} s); {report:?}",
        report = (report.hashed, report.unchanged)
    );
    assert_eq!(report.hashed, 2000, "only the manifests are read");
    let total = index
        .assets(lil_core::index::AssetQuery::default())
        .await
        .unwrap()
        .total;
    assert_eq!(total, 2000);
    assert!(
        took <= budget,
        "the cold rebuild took {took:.3} s; the budget is {budget} s"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-042
async fn warm_reconcile_of_10000_assets_fits_the_budget() {
    let env = TestEnv::new();
    let setup = Instant::now();
    let ws = workspace(&env, 10_000);
    let index = open(&env, &ws);
    index.rebuild().await.expect("indexed once");
    println!(
        "setup of 10,000 assets x 3 files, indexed once: {:.2?}",
        setup.elapsed()
    );

    let started = Instant::now();
    let report = index.reconcile(Scope::All).await.expect("the reconcile");
    let took = started.elapsed().as_secs_f64();
    let budget = params::budget("index_warm_reconcile_10000_assets_s");
    println!(
        "warm reconcile of 10,000 assets: {took:.3} s (budget {budget} s); unchanged {}",
        report.unchanged
    );
    assert_eq!(
        report.hashed, 0,
        "nothing changed, nothing is hashed: {report:?}"
    );
    assert_eq!(report.unchanged, 30_000);
    assert!(
        took <= budget,
        "the warm reconcile took {took:.3} s; the budget is {budget} s"
    );
}

// ---------------------------------------------------------------------------------------------
// The facet query (APP-138, CORE-042; P4b-R10, P4b-R23): `assets.query {limit: 200, facets:
// true}`, the page and its six disjunctive facet counts in one read transaction.
// ---------------------------------------------------------------------------------------------

/// A small deterministic generator (xorshift64*), so every run builds the same workspace.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, table: &[(&'a str, u64)]) -> &'a str {
        let mut r = self.below(table.iter().map(|t| t.1).sum());
        for (value, weight) in table {
            if r < *weight {
                return value;
            }
            r -= weight;
        }
        table[0].0
    }
}

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
const LABELS: [&str; 5] = ["red", "yellow", "green", "blue", "purple"];

/// `assets` assets in ten projects, manifests only (as `docs/spikes/12-browse-core.md` S0.3
/// spreads them): the ten statuses and eight types unevenly, three tags each from 200, ratings
/// 0-5, a label on a third, and 1-3 runs of four takes each (about 10 % of takes picked and 5 %
/// rejected, about 30 % of runs open and 10 % trashed). The runs' folders aren't on disk, as on
/// a fresh clone.
fn facet_workspace(env: &TestEnv, assets: usize) -> PathBuf {
    let ws = env.root().join("ws-facets");
    make_dir(&ws);
    let ws = std::fs::canonicalize(&ws).unwrap();
    let ws = PathBuf::from(
        ws.to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or_else(|| ws.to_string_lossy().into_owned()),
    );
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ assets as u64);
    for i in 0..assets {
        let project = format!("project-{:02}", rng.below(10));
        // Ids out of order, so the page's sort isn't the walk's order.
        let asset = format!("a{:08x}_{i}", rng.next() as u32);
        let status = rng.pick(&STATUSES);
        let kind = rng.pick(&TYPES);
        let rating = [0, 0, 0, 1, 2, 3, 3, 4, 4, 5][rng.below(10) as usize];
        let mut tags = std::collections::BTreeSet::new();
        while tags.len() < 3 {
            tags.insert(rng.below(200));
        }
        let tags: Vec<String> = tags.into_iter().map(|t| format!("Tag{t:03}")).collect();
        let runs: Vec<serde_json::Value> = (0..=rng.below(3))
            .map(|r| {
                let run_id = format!("r_{r}");
                let state = rng.below(10);
                let files: Vec<serde_json::Value> = (1..=4)
                    .map(|t| {
                        let mut f = json!({"path": format!("candidates/{run_id}/take_{t}.png"),
                                           "role": "candidate"});
                        match rng.below(100) {
                            0..=9 => f["flag"] = json!("pick"),
                            10..=14 => f["flag"] = json!("reject"),
                            _ => {}
                        }
                        f
                    })
                    .collect();
                let mut run = json!({"run_id": run_id, "path": format!("candidates/{run_id}/"),
                    "status": if (1..=3).contains(&state) { "open" } else { "rejected" },
                    "created": "2026-10-01T00:00:00Z", "files": files});
                if state == 0 {
                    run["trashed_utc"] = json!("2026-10-02T00:00:00Z");
                }
                run
            })
            .collect();
        let mut manifest = json!({
            "schema_version": 1,
            "asset_id": asset,
            "project": project,
            "creation_uuid": format!("00000000-0000-4000-8000-{i:012x}"),
            "type": kind,
            "status": status,
            "version": "1.0.0",
            "tags": tags,
            "rating": rating,
            "candidate_runs": runs,
            "status_log": [{"from": "planned", "to": status, "by": "user",
                            "utc": "2026-10-01T00:00:00Z"}],
        });
        if rng.below(3) == 0 {
            manifest["label"] = json!(LABELS[rng.below(5) as usize]);
        }
        let dir: PathBuf = ["projects", &project, "assets", &asset]
            .iter()
            .fold(ws.clone(), |p, c| p.join(c));
        write_file(
            &dir.join(format!("{asset}.manifest.json")),
            manifest.to_string().as_bytes(),
        );
    }
    ws
}

/// The two filter sets measured: none (the opening view), and five filters, so every facet
/// has its own WHERE (`status` any of 2, `tags` any of 2, `label` red or none, `rating_min` 2,
/// `flag` pick).
fn facet_queries() -> [(&'static str, AssetQuery); 2] {
    let base = AssetQuery {
        limit: 200,
        facets: true,
        ..AssetQuery::default()
    };
    let five = AssetQuery {
        status: Some(Choice::AnyOf(vec!["approved".into(), "in_review".into()])),
        tags: Some(vec!["tag007".into(), "tag042".into()]),
        label: Some(vec![Some(Label::Red), None]),
        rating_min: Some(2),
        flag: Some(vec![Flag::Pick]),
        ..base.clone()
    };
    [("none", base), ("five filters", five)]
}

/// The 95th percentile (nearest rank) of `ms`.
fn p95(mut ms: Vec<f64>) -> f64 {
    ms.sort_by(f64::total_cmp);
    let rank = (ms.len() * 95).div_ceil(100);
    ms[rank.saturating_sub(1)]
}

/// Builds `assets` assets, then times 100 runs of each filter set after 5 warm-up runs and
/// checks the slower set's p95 against `budget_key`.
async fn facet_bench(assets: usize, budget_key: &str) {
    let env = TestEnv::new();
    let setup = Instant::now();
    let ws = facet_workspace(&env, assets);
    let index = open(&env, &ws);
    index.rebuild().await.expect("indexed once");
    println!(
        "setup of {assets} assets, indexed once: {:.2?}",
        setup.elapsed()
    );

    let mut worst = 0.0_f64;
    for (name, q) in facet_queries() {
        for _ in 0..5 {
            index.assets(q.clone()).await.unwrap();
        }
        let mut ms = Vec::with_capacity(100);
        let mut total = 0;
        for _ in 0..100 {
            let started = Instant::now();
            let page = index.assets(q.clone()).await.unwrap();
            ms.push(started.elapsed().as_secs_f64() * 1000.0);
            assert!(page.facets.is_some());
            total = page.total;
        }
        let p = p95(ms);
        println!(
            "assets.query with facets, {assets} assets, {name}: p95 {p:.1} ms ({total} match)"
        );
        worst = worst.max(p);
    }
    let budget = params::budget(budget_key);
    println!("worst p95 {worst:.1} ms (budget {budget} ms)");
    assert!(
        worst <= budget,
        "the facet query's p95 is {worst:.1} ms; the budget is {budget} ms"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: APP-138, CORE-042
#[ignore = "bench"]
async fn assets_query_with_facets_10000() {
    facet_bench(10_000, "assets_query_facets_10000_ms").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: APP-138, CORE-042
#[ignore = "bench"]
async fn assets_query_with_facets_50000() {
    facet_bench(50_000, "assets_query_facets_50000_ms").await;
}
