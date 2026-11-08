//! The index budgets (CORE-042, APP-138): a cold rebuild of 2,000 assets and a warm reconcile
//! of 10,000 assets with nothing changed, `assets.query {limit: 200, facets: true}` at 10,000 and
//! 50,000 assets (P4b-R23), and `project.status` of a project of 10,000 assets, each against its
//! budget in `test-params.toml` (`[budgets]`, read through `lil_testkit::params::budget`). The
//! warm reconcile's setup and run together also stay within half of nextest's per-test cap
//! (`[timeouts] e2e_s`), so a slower machine fails it with its times printed before the cap
//! stops it.
//!
//! These are the suite's only wall-clock measurements, the stated exception to QA-031: they
//! are performance gates, not correctness assertions. Each prints its measured time. The facet
//! benches are `#[ignore = "bench"]` (their workspaces take minutes to build): run them with
//! `cargo nextest run -p lil-core --run-ignored only -E 'binary(index_bench) & test(/facets/)'`.
//! They time the index read (`Index::assets`: the page, `total` and the facets in one read
//! transaction), which is what the facet budgets cover; the tool layer (argument parsing, the
//! cursor, the summaries and the result's JSON) is outside them. The `project.status` bench is
//! ignored too (`-E 'binary(index_bench) & test(/project_status/)'`); it times
//! `projects::project_status` on an open workspace: all of the tool's work but its JSON.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use lil_core::events::EventBus;
use lil_core::index::{AssetQuery, Choice, Index, Scope};
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::workspace::{WorkspaceHub, WorkspaceRef};
use lil_platform::clock::SystemClock;
use lil_platform::fs::SystemProbe;
use lil_platform::{Clock, Env};
use lil_schema::{Flag, Label};
use lil_testkit::files::write_file;
use lil_testkit::{SeqIds, TestEnv, params, sha256_bytes};
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
    // The setup is most of this test's time: with the reconcile it stays within half of the cap,
    // so a slower machine fails here, with its times, before nextest stops the test.
    let whole = setup.elapsed().as_secs_f64();
    let cap = test_cap_s();
    println!("setup and warm reconcile: {whole:.1} s (per-test cap {cap} s)");
    assert!(
        whole <= cap / 2.0,
        "the setup and the warm reconcile took {whole:.1} s, over half of the {cap} s per-test cap"
    );
}

/// nextest's per-test cap for `lil-core`'s tests, in seconds: `[timeouts] e2e_s` of
/// `test-params.toml`, the record of `.config/nextest.toml`'s value.
fn test_cap_s() -> f64 {
    let path = params::file();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let table: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("{} is not TOML: {e}", path.display()));
    table
        .get("timeouts")
        .and_then(|t| t.get("e2e_s"))
        .and_then(toml::Value::as_integer)
        .map(|s| s as f64)
        .unwrap_or_else(|| panic!("{} has no [timeouts] e2e_s", path.display()))
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

// ---------------------------------------------------------------------------------------------
// `project.status` (CORE-038, CORE-042): its orphans come from the index's rows, so a project of
// 10,000 assets answers within `project_status_10000_assets_ms`.
// ---------------------------------------------------------------------------------------------

/// The orphans [`status_workspace`] makes: an orphan run folder in every 100th asset, an orphan
/// version in every 1,000th.
fn bench_orphans(assets: usize) -> usize {
    assets.div_ceil(100) + assets.div_ceil(1000)
}

/// The fixture workspace with `assets` more assets in its `sketchbook` project, each as the
/// store leaves a signed-off asset: a manifest whose approved version 1.0.0 was signed off from
/// a selected run, `v1.0.0/` with its snapshot and image, and the run's folder with its other
/// take; a third also have an open run with two takes. Every 100th asset has an orphan run
/// folder, every 1,000th an orphan version.
fn status_workspace(env: &TestEnv, assets: usize) -> PathBuf {
    let ws = env.copy_fixture_workspace("status");
    let ws = std::fs::canonicalize(&ws).unwrap();
    let ws = PathBuf::from(
        ws.to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or_else(|| ws.to_string_lossy().into_owned()),
    );
    let (signed, open, orphan) = ("r_20261001_1000", "r_20261002_1000", "r_20261003_1000");
    let make = |i: usize| {
        let asset = format!("bench_{i:05}");
        let dir: PathBuf = ["projects", "sketchbook", "assets", &asset]
            .iter()
            .fold(ws.clone(), |p, c| p.join(c));
        let put = |rel: &str, bytes: &[u8]| {
            let path = rel.split('/').fold(dir.clone(), |p, c| p.join(c));
            write_file(&path, bytes);
        };
        let take = |run: &str, n: u32| json!({"path": format!("candidates/{run}/take_{n}.png"), "role": "candidate"});
        let image = format!("image {i}").into_bytes();
        let mut runs = vec![json!({
            "run_id": signed, "path": format!("candidates/{signed}/"),
            "status": "selected", "created": "2026-10-01T10:00:00Z", "selected": ["take_1.png"],
            "signed_off": {"by": "user", "date": "2026-10-01", "version": "1.0.0"},
            "files": [take(signed, 1), take(signed, 2)],
        })];
        put("v1.0.0/image.png", &image);
        put("v1.0.0/manifest.snapshot.json", b"{}");
        put(&format!("candidates/{signed}/take_2.png"), b"take 2");
        if i.is_multiple_of(3) {
            runs.push(json!({
                "run_id": open, "path": format!("candidates/{open}/"),
                "status": "open", "created": "2026-10-02T10:00:00Z",
                "files": [take(open, 1), take(open, 2)],
            }));
            put(&format!("candidates/{open}/take_1.png"), b"open 1");
            put(&format!("candidates/{open}/take_2.png"), b"open 2");
        }
        if i.is_multiple_of(100) {
            put(&format!("candidates/{orphan}/left.png"), b"left behind");
        }
        if i.is_multiple_of(1000) {
            put("v2.0.0/manifest.snapshot.json", b"{}");
        }
        let manifest = json!({
            "schema_version": 1,
            "asset_id": asset,
            "project": "sketchbook",
            "creation_uuid": format!("00000000-0000-4000-8000-{i:012x}"),
            "type": "image",
            "status": "approved",
            "version": "1.0.0",
            "approved_version": "1.0.0",
            "candidate_runs": runs,
            "hashes": {"v1.0.0/image.png": format!("sha256:{}", sha256_bytes(&image))},
        });
        put(
            &format!("{asset}.manifest.json"),
            manifest.to_string().as_bytes(),
        );
    };
    // Written by eight threads: the setup is most of the bench's time.
    std::thread::scope(|scope| {
        for first in 0..8 {
            let make = &make;
            scope.spawn(move || (first..assets).step_by(8).for_each(make));
        }
    });
    ws
}

/// Removes the asset folders under `dir` from eight threads, as an outside program would, so the
/// bench's clean-up stays well inside the per-test cap.
#[allow(clippy::disallowed_methods)]
fn remove_assets(dir: &Path) {
    let folders: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    std::thread::scope(|scope| {
        for first in 0..8 {
            let folders = &folders;
            scope.spawn(move || {
                for folder in folders.iter().skip(first).step_by(8) {
                    std::fs::remove_dir_all(folder).unwrap();
                }
            });
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-038, CORE-042
#[ignore = "bench"]
async fn project_status_10000() {
    const ASSETS: usize = 10_000;
    let env = TestEnv::new();
    let setup = Instant::now();
    let ws = status_workspace(&env, ASSETS);
    println!("{ASSETS} assets written: {:.2?}", setup.elapsed());
    let states = Arc::new(
        StateRegistry::open(
            &env.env,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("the suite state.db"),
    );
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        Arc::new(EventBus::new(clock.clone())),
        clock,
        Arc::new(SeqIds::new()),
        FailPoints::none(),
        Arc::new(SystemProbe),
        lil_core::watch::WatchConfig::default(),
    );
    let ws_ref = WorkspaceRef {
        id: Env::workspace_hash(&ws),
        path: ws.clone(),
    };
    let open = hub.open(&ws_ref).await.expect("the workspace opens");
    open.index.ready().await;
    println!(
        "setup of {ASSETS} assets, opened and indexed: {:.2?}",
        setup.elapsed()
    );

    let status = || lil_core::projects::project_status(&open, "sketchbook");
    let first = status().await.expect("project.status");
    let ours = first
        .orphans
        .iter()
        .filter(|o| o.starts_with("projects/sketchbook/assets/bench_"))
        .count();
    assert_eq!(ours, bench_orphans(ASSETS), "{:?}", first.orphans);
    for _ in 0..5 {
        status().await.unwrap();
    }
    let mut ms = Vec::with_capacity(100);
    for _ in 0..100 {
        let started = Instant::now();
        let got = status().await.unwrap();
        ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(got.orphans.len(), first.orphans.len());
    }
    let p = p95(ms);
    hub.close_all().await;
    states.close();
    drop(open);
    remove_assets(&ws.join("projects").join("sketchbook").join("assets"));
    println!("project.status of {ASSETS} assets: p95 {p:.1} ms");
    let budget = params::budget("project_status_10000_assets_ms");
    println!("budget {budget} ms");
    assert!(
        p <= budget,
        "project.status's p95 is {p:.1} ms; the budget is {budget} ms"
    );
}
