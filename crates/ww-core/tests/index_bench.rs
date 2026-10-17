//! The index budgets (CORE-042, APP-138): a cold rebuild of 2,000 assets and a warm reconcile
//! of 10,000 assets with nothing changed, each against its budget in `test-params.toml`
//! (`[budgets]`, read through `ww_testkit::params::budget`).
//!
//! These are the suite's only wall-clock measurements, the stated exception to QA-031: they
//! are performance gates, not correctness assertions. Each prints its measured time.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use ww_core::index::{Index, Scope};
use ww_platform::fs::SystemProbe;
use ww_testkit::files::write_file;
use ww_testkit::{TestEnv, params, sha256_bytes};

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
        .assets(ww_core::index::AssetQuery::default())
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
