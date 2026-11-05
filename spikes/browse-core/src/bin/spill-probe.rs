//! S0.4 (APP-132; P4b-R19, P4b-R37): the engine's PDH `Shared Usage` (Step 3 only).
//!
//! `spill-probe sample --root-pid <pid> --out <jsonl> --stop-file <path> [--interval-ms 1000]
//! [--max-seconds 7200]`: once per interval, the sum of `\GPU Process Memory(*)\Shared Usage`
//! (and `Dedicated Usage`) over the discrete GPU's instances whose PID is `root` or one of its
//! descendants, plus the adapter's `Shared Usage`. Stops when the stop file exists or the root
//! process exits.
//! `spill-probe analyze --samples <jsonl> --phases <json> --out <json> [--window 10]`.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use browse_core::pdhnames::{is_on, number_after};
use browse_core::spill::{Phase, analyse};
use browse_core::win::{self, FMT_LARGE, MIB, Query};
use browse_core::{num_arg, path_arg, subcommand, unix_ms, write_json};
use serde_json::{Value, json};

fn sample() {
    let root: u32 = num_arg("--root-pid", 0);
    let out = path_arg("--out");
    let stop = path_arg("--stop-file");
    let interval = Duration::from_millis(num_arg("--interval-ms", 1000));
    let max = Duration::from_secs(num_arg("--max-seconds", 7200));
    let ad = win::discrete_adapter().expect("discrete adapter");
    let tag = ad.tag();
    let q = Query::open().expect("PdhOpenQueryW");
    let proc_shared = q
        .add_english(r"\GPU Process Memory(*)\Shared Usage")
        .expect("process Shared Usage");
    let proc_dedicated = q
        .add_english(r"\GPU Process Memory(*)\Dedicated Usage")
        .expect("process Dedicated Usage");
    let adapter_shared = q
        .add_english(r"\GPU Adapter Memory(*)\Shared Usage")
        .expect("adapter Shared Usage");
    q.collect();
    let mut file = std::fs::File::create(&out).expect("samples file");
    let watched = win::Watched::open(root);
    let start = Instant::now();
    let mut i = 0u32;
    while !stop.exists()
        && start.elapsed() < max
        && !watched.as_ref().is_none_or(|w| w.has_exited())
    {
        i += 1;
        if let Some(w) = (interval * i).checked_sub(start.elapsed()) {
            std::thread::sleep(w);
        }
        let status = q.collect();
        let tree: BTreeSet<u32> = win::process_tree(root);
        let sum = |c| -> (f64, usize) {
            let items = q.items(c, FMT_LARGE).unwrap_or_default();
            let mine: Vec<f64> = items
                .iter()
                .filter(|it| (it.status == 0 || it.status == 1) && is_on(&it.name, &tag))
                .filter(|it| {
                    number_after(&it.name, "pid").is_some_and(|p| tree.contains(&(p as u32)))
                })
                .map(|it| it.value)
                .collect();
            (mine.iter().sum::<f64>() / MIB, mine.len())
        };
        let (shared, instances) = sum(proc_shared);
        let (dedicated, _) = sum(proc_dedicated);
        let adapter: f64 = q
            .items(adapter_shared, FMT_LARGE)
            .unwrap_or_default()
            .iter()
            .filter(|it| is_on(&it.name, &tag))
            .map(|it| it.value)
            .sum::<f64>()
            / MIB;
        let line = json!({"t_ms": unix_ms(), "shared_mib": shared, "dedicated_mib": dedicated, "adapter_shared_mib": adapter, "instances": instances, "tree": tree.len(), "collect": status});
        writeln!(file, "{line}").expect("write sample");
        file.flush().ok();
    }
}

fn analyze() {
    let samples_path = path_arg("--samples");
    let phases_path = path_arg("--phases");
    let out = path_arg("--out");
    let window: usize = num_arg("--window", 10);
    let lines: Vec<Value> =
        std::io::BufReader::new(std::fs::File::open(&samples_path).expect("samples"))
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect();
    let series = |key: &str| -> Vec<(u64, f64)> {
        lines
            .iter()
            .filter_map(|v| Some((v["t_ms"].as_u64()?, v[key].as_f64()?)))
            .collect()
    };
    let phases_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&phases_path).expect("phases"))
            .expect("phases JSON");
    let phases: Vec<Phase> = phases_json
        .as_array()
        .expect("phases array")
        .iter()
        .map(|p| Phase {
            kind: p["kind"].as_str().unwrap_or("").into(),
            index: p["index"].as_u64().unwrap_or(0) as u32,
            start_ms: p["start_ms"].as_u64().unwrap_or(0),
            end_ms: p["end_ms"].as_u64().unwrap_or(0),
        })
        .collect();
    let mut result = analyse(&series("shared_mib"), &phases, window);
    // Recorded beside the verdict: the same analysis over the adapter's figure and the
    // process's dedicated figure.
    result["adapter_shared"] = analyse(&series("adapter_shared_mib"), &phases, window);
    result["process_dedicated"] = analyse(&series("dedicated_mib"), &phases, window);
    result["samples"] = json!(lines.len());
    write_json(&out, &result);
    println!(
        "spill: {} (idle {} MiB, fit max {} MiB, spill min {} MiB, margin {} MiB)",
        result["verdict"],
        result["idle_mib"],
        result["fit_max_mib"],
        result["spill_min_mib"],
        result["margin_mib"]
    );
}

fn main() {
    match subcommand().as_str() {
        "sample" => sample(),
        "analyze" => analyze(),
        other => {
            eprintln!("usage: spill-probe sample|analyze ... (got {other:?})");
            std::process::exit(2);
        }
    }
}
