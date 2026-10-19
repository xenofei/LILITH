//! The CORE-014 module boundaries (CORE-010, CORE-014): the modules of `lil-core` depend on each
//! other without a cycle, the store depends on none of its consumers, the index on none of the
//! modules above it, `pipeline` takes no `tools` types (only `tools` → `pipeline`), so the
//! snapshot providers in `tools/` can call `pipeline::asset_detail`, and `jobs` takes no `tools`
//! types (only `tools` → `jobs`: tools submit jobs; `jobs` defines the `JobHandler` trait they
//! register with, CORE-010), and `telemetry` takes none either (only `tools` → `telemetry`: the
//! telemetry tools and `test.hw_tick` live in `tools`); `engines` uses `jobs` and `telemetry`, and
//! neither uses it (`EngineControl` is how `jobs` reaches an engine). `meter` uses `tools` (its
//! stage traits and `BillingBasis`), so `tools` never uses `meter`: a meter tool is registered
//! from `meter` (as the engine tools are from `engines`), and `jobs` and `telemetry` use no
//! `meter` item either (`JobCompletion` is how `meter` hears of a job). `confirm` uses `tools`
//! (it is the confirm stage), and neither `tools` nor `jobs` uses it.
//!
//! The scan reads every `crate::<module>` path in a module's files (`src/<module>.rs` and
//! `src/<module>/**.rs`), `use crate::{a::…, b::…}` groups included, outside comments.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The CORE-014 modules (each plan-3 module is added as it lands).
const MODULES: [&str; 14] = [
    "store",
    "index",
    "watch",
    "graph",
    "pipeline",
    "projects",
    "settings",
    "tools",
    "jobs",
    "telemetry",
    "engines",
    "meter",
    "confirm",
    "resources",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The first path segment of every `crate::` path in `text`, outside `//` comments: a plain
/// `crate::a::b` gives `a`; a group `crate::{a::x, b, c::{d}}` gives `a`, `b` and `c`.
fn crate_refs(text: &str) -> BTreeSet<String> {
    let code: String = text
        .lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut found = BTreeSet::new();
    let bytes = code.as_bytes();
    let mut at = 0;
    while let Some(i) = code[at..].find("crate::") {
        let start = at + i + "crate::".len();
        // `lil_core::…` or `my_crate::…` are other paths.
        let before = code[..at + i].chars().next_back();
        at = start;
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        if bytes.get(start) == Some(&b'{') {
            // A group: the first segment of each top-level item.
            let mut depth = 0;
            let mut item_start = true;
            let mut j = start;
            while j < bytes.len() {
                match bytes[j] {
                    b'{' => {
                        depth += 1;
                        if depth == 1 {
                            item_start = true;
                        }
                    }
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b',' if depth == 1 => item_start = true,
                    c if depth == 1 && item_start && (c.is_ascii_alphabetic() || c == b'_') => {
                        let end = code[j..]
                            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                            .map_or(code.len(), |k| j + k);
                        found.insert(code[j..end].to_string());
                        item_start = false;
                        j = end;
                        continue;
                    }
                    _ => {}
                }
                j += 1;
            }
            at = j;
        } else {
            let end = code[start..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .map_or(code.len(), |k| start + k);
            if end > start {
                found.insert(code[start..end].to_string());
            }
        }
    }
    found
}

/// Module → the CORE-014 modules its files name.
fn graph(src: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut graph = BTreeMap::new();
    for module in MODULES {
        let mut files = Vec::new();
        let single = src.join(format!("{module}.rs"));
        if single.is_file() {
            files.push(single);
        }
        rust_files(&src.join(module), &mut files);
        if files.is_empty() {
            continue;
        }
        let mut uses = BTreeSet::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            uses.extend(
                crate_refs(&text)
                    .into_iter()
                    .filter(|m| MODULES.contains(&m.as_str()) && m != module),
            );
        }
        graph.insert(module.to_string(), uses);
    }
    graph
}

/// A cycle in `graph`, as the modules along it, or `None`.
fn cycle(graph: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    fn visit(
        node: &str,
        graph: &BTreeMap<String, BTreeSet<String>>,
        path: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if let Some(i) = path.iter().position(|n| n == node) {
            let mut found = path[i..].to_vec();
            found.push(node.to_string());
            return Some(found);
        }
        if done.contains(node) {
            return None;
        }
        path.push(node.to_string());
        for next in graph.get(node).into_iter().flatten() {
            if let Some(found) = visit(next, graph, path, done) {
                return Some(found);
            }
        }
        path.pop();
        done.insert(node.to_string());
        None
    }
    let mut done = BTreeSet::new();
    for node in graph.keys() {
        if let Some(found) = visit(node, graph, &mut Vec::new(), &mut done) {
            return Some(found);
        }
    }
    None
}

#[test] // spec: CORE-010, CORE-014
fn module_dependencies_are_acyclic_and_store_depends_on_no_consumer() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let graph = graph(&src);
    for module in [
        "store",
        "index",
        "pipeline",
        "tools",
        "jobs",
        "telemetry",
        "engines",
        "meter",
        "resources",
    ] {
        assert!(
            graph.contains_key(module),
            "no {module} module found: {graph:?}"
        );
    }
    assert_eq!(cycle(&graph), None, "{graph:?}");
    let forbidden = |module: &str, consumers: &[&str]| {
        let uses = &graph[module];
        let bad: Vec<&&str> = consumers.iter().filter(|c| uses.contains(**c)).collect();
        assert!(bad.is_empty(), "{module} uses {bad:?}: {uses:?}");
    };
    forbidden(
        "store",
        &[
            "index", "watch", "graph", "pipeline", "projects", "settings", "tools", "jobs",
        ],
    );
    forbidden(
        "index",
        &["watch", "pipeline", "projects", "settings", "tools", "jobs"],
    );
    forbidden("pipeline", &["tools", "jobs"]);
    forbidden("jobs", &["tools", "engines", "meter"]);
    forbidden("telemetry", &["tools", "jobs", "engines", "meter"]);
    forbidden("tools", &["engines", "meter"]);
    // `confirm` implements the `tools` confirm stage; the dispatcher reaches it only through
    // that trait (`ConfirmStage::on_replay`).
    forbidden("tools", &["confirm"]);
    forbidden("jobs", &["confirm"]);
    assert!(graph["confirm"].contains("tools"), "{graph:?}");
    // `resources` is registered by the server like `engines`: no module names it, and it uses
    // `tools` (the call context and the registry) and the store.
    for module in graph.keys().filter(|m| *m != "resources") {
        forbidden(module, &["resources"]);
    }
    // The directions that exist: tools → jobs → pipeline, tools → pipeline → index → store.
    assert!(graph["tools"].contains("jobs"), "{graph:?}");
    assert!(graph["jobs"].contains("pipeline"), "{graph:?}");
    assert!(graph["tools"].contains("pipeline"), "{graph:?}");
    assert!(graph["tools"].contains("telemetry"), "{graph:?}");
    assert!(graph["engines"].contains("jobs"), "{graph:?}");
    assert!(graph["meter"].contains("tools"), "{graph:?}");
    assert!(graph["resources"].contains("store"), "{graph:?}");
    assert!(graph["resources"].contains("tools"), "{graph:?}");
    assert!(graph["pipeline"].contains("index"), "{graph:?}");
    assert!(graph["index"].contains("store"), "{graph:?}");
}

/// The scan and the cycle check catch what they are there for: a `pipeline` file that names a
/// `tools` type closes the cycle `tools` → `pipeline` → `tools`.
#[test] // spec: CORE-010
fn the_check_detects_a_cycle_and_every_use_form() {
    let text = "use crate::store::{Store, WsPath};\n\
                use crate::{index::Index, tools::{CallCtx, Registry}, pipeline};\n\
                // use crate::watch::Watcher;\n\
                fn f(x: crate::graph::Graph) {}\n\
                use lil_core::settings::Settings;\n\
                let y = my_crate::projects::P;\n";
    let refs = crate_refs(text);
    let want: BTreeSet<String> = ["store", "index", "tools", "pipeline", "graph"]
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(refs, want);

    let mut g: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut edge = |a: &str, b: &str| {
        g.entry(a.to_string()).or_default().insert(b.to_string());
    };
    edge("tools", "pipeline");
    edge("pipeline", "index");
    edge("index", "store");
    g.entry("store".into()).or_default();
    assert_eq!(cycle(&g), None);
    g.get_mut("pipeline").unwrap().insert("tools".into());
    let found = cycle(&g).expect("the cycle is found");
    assert!(
        found.contains(&"tools".to_string()) && found.contains(&"pipeline".to_string()),
        "{found:?}"
    );
    assert_eq!(found.first(), found.last());
}
