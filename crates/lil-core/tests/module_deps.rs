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
//! `interchange` and `docs` (the handler registry and the holds) are used by no other module but
//! `server`, which builds every module and wires them together. `redact`, the redaction the
//! diagnostics bundle and the toasts share, names no other module of the crate, so the
//! notifier (`interchange`) redacts without naming `diagnostics` (PR #53).
//!
//! Every module of the crate is scanned, the infrastructure ones too (`state`, `workspace`,
//! `server`, `transport`, `rpc`, `config`, `events`, `identity`, `lifecycle`, `session`,
//! `desktop`, `diagnostics`, `redact`), and
//! the list is checked against `src/lib.rs`, so a new module can't be missed. The store names
//! none of them: it defines `StoreRows`, which `state` implements, and `Access`, which
//! `workspace` re-exports; `state` holds `WorkspaceRef`. The connection types (`ConnCtx`,
//! `Router`, `ConnObserver`) live in `transport`, below `rpc`, `docs` and `interchange`; only
//! `server` builds the router.
//!
//! The scan reads every `crate::<module>` path in a module's files (`src/<module>.rs` and
//! `src/<module>/**.rs`), `use crate::{a::…, b::…}` groups included, outside comments. It also
//! resolves the two forms a bare `crate::<module>` scan used to miss (I3): a crate-root
//! re-export reached by its item name (`crate::RunningCore` → `server`, from `src/lib.rs`'s
//! `pub use`), and a `super::…` / `super::super::…` path (resolved against the file's module
//! depth, so `super::super::state` from `src/store/write.rs` names `state`). `declared()` matches
//! a `mod` at any visibility, so a `pub(crate) mod` can't slip past the list check.
//!
//! `#[cfg(test)]` modules are **not** counted: they don't ship, so they aren't part of the
//! crate's production dependency graph (CORE-010). That is why a store unit test may build a
//! `state` database, and the `rpc` panic tests may spin a core (`crate::CoreThread`), without
//! those test-only uses being read as production edges.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Every module of the crate (`pub mod` in `src/lib.rs`): the CORE-014 modules and the
/// infrastructure.
const MODULES: [&str; 29] = [
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
    "interchange",
    "docs",
    "state",
    "workspace",
    "server",
    "transport",
    "rpc",
    "config",
    "events",
    "identity",
    "lifecycle",
    "session",
    "desktop",
    "diagnostics",
    "redact",
];

/// The modules `src/lib.rs` declares, at any visibility: `mod`, `pub mod`, `pub(crate) mod`,
/// `pub(super) mod`, … (a `pub(crate) mod` would otherwise slip past the scan).
fn declared(src: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(src.join("lib.rs"))
        .unwrap()
        .lines()
        .filter_map(|l| mod_name(l.trim()))
        .collect()
}

/// The module a `… mod <name>;` line declares, whatever its visibility; `None` for any other
/// line. `pub`, `pub(crate)`, `pub(super)` and `pub(in …)` all count.
fn mod_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("pub").map_or(line, |r| {
        let r = r.trim_start();
        // Skip a `(…)` visibility restriction.
        match r.strip_prefix('(') {
            Some(after) => after.split_once(')').map_or("", |(_, t)| t).trim_start(),
            None => r,
        }
    });
    rest.strip_prefix("mod ")
        .and_then(|r| r.trim().strip_suffix(';'))
        .map(|name| name.trim().to_string())
}

/// `src/lib.rs`'s crate-root re-exports: each item name a `pub use <module>::…;` lifts to the
/// crate root, mapped to its `<module>` (only when `<module>` is one of [`MODULES`]). So a
/// `crate::RunningCore` elsewhere is read as an edge to `server`, not missed because its first
/// segment isn't a module name.
fn reexports(src: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(src.join("lib.rs")).unwrap();
    let text = strip_line_comments(&text);
    let mut map = BTreeMap::new();
    let mut at = 0;
    while let Some(i) = text[at..].find("pub use ") {
        let start = at + i + "pub use ".len();
        // The module the items come from: the first path segment.
        let seg_end = text[start..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(text.len(), |k| start + k);
        let module = text[start..seg_end].to_string();
        // The statement runs to its `;`.
        let stmt_end = text[start..].find(';').map_or(text.len(), |k| start + k);
        let rest = &text[seg_end..stmt_end];
        at = stmt_end;
        if !MODULES.contains(&module.as_str()) {
            continue;
        }
        // `pub use module::Item;` or `pub use module::{A, B, C};` or `pub use module::*;`.
        let items = rest.trim_start_matches("::");
        if let Some(group) = items.trim().strip_prefix('{') {
            let group = group.trim_end().trim_end_matches('}');
            for item in group.split(',') {
                // The last segment of each item (`a::b::C` re-exports `C`).
                let name = item.trim().rsplit("::").next().unwrap_or("").trim();
                if !name.is_empty() && name != "*" {
                    map.insert(name.to_string(), module.clone());
                }
            }
        } else {
            let name = items.trim().rsplit("::").next().unwrap_or("").trim();
            if !name.is_empty() && name != "*" {
                map.insert(name.to_string(), module.clone());
            }
        }
    }
    map
}

/// `//`-comment-stripped text (line comments only; enough for the `use`/`pub use` lines the
/// scan reads).
fn strip_line_comments(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` with every `#[cfg(test)]`-attributed inline `mod … { … }` block removed: those tests
/// don't ship, so they are not part of the crate's production dependency graph (CORE-010). Only
/// whole test modules are cut; a `#[cfg(test)]` on a field or a statement is left.
fn strip_cfg_test_mods(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = text[at..].find("#[cfg(test)]") {
        let marker = at + i;
        // What follows the attribute, past whitespace, then an optional visibility, then `mod`.
        let after_attr = marker + "#[cfg(test)]".len();
        let rest = text[after_attr..].trim_start();
        let after_vis = rest.strip_prefix("pub").map_or(rest, |r| {
            let r = r.trim_start();
            r.strip_prefix('(')
                .and_then(|a| a.split_once(')'))
                .map_or(r, |(_, t)| t.trim_start())
        });
        // The `{` that opens the module body, if this is `#[cfg(test)] … mod <name> { … }`.
        let open = after_vis
            .strip_prefix("mod ")
            .and_then(|after_mod| after_mod.find('{').map(|rel| (after_mod, rel)))
            .map(|(after_mod, rel)| {
                let base = text.len() - after_mod.len();
                base + rel
            });
        let Some(open) = open else {
            // Not a test module: keep the text through the attribute and move on.
            out.push_str(&text[at..after_attr]);
            at = after_attr;
            continue;
        };
        // Drop the whole block: keep up to the attribute, skip past the matching `}`.
        out.push_str(&text[at..marker]);
        let mut depth = 0i32;
        let mut k = open;
        while k < bytes.len() {
            match bytes[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        k += 1;
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        at = k;
    }
    out.push_str(&text[at..]);
    out
}

/// The module chain of a source file relative to `src`: `src/store/write.rs` → `["store",
/// "write"]`, `src/store.rs` and `src/store/mod.rs` → `["store"]`.
fn module_chain(path: &Path, src: &Path) -> Vec<String> {
    let rel = path.strip_prefix(src).unwrap_or(path);
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.last_mut() {
        if last == "mod.rs" {
            parts.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_string();
        }
    }
    parts
}

/// The targets of `super::…` paths in `code`, each as `(supers, first_segment)` (`super::x` →
/// `(1, "x")`, `super::super::state::T` → `(2, "state")`), outside `//` comments and `crate::`.
fn super_targets(code: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = code[at..].find("super::") {
        let pos = at + i;
        let before = code[..pos].chars().next_back();
        at = pos + "super::".len();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let mut supers = 1;
        let mut j = at;
        while code[j..].starts_with("super::") {
            supers += 1;
            j += "super::".len();
        }
        let end = code[j..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(code.len(), |k| j + k);
        if end > j {
            out.push((supers, code[j..end].to_string()));
        }
        at = j;
    }
    out
}

/// The top-level module `name` names, directly ([`MODULES`]) or through a crate-root re-export
/// (`reexports`); `None` for anything else.
fn resolve(name: &str, reexports: &BTreeMap<String, String>) -> Option<String> {
    if MODULES.contains(&name) {
        Some(name.to_string())
    } else {
        reexports.get(name).cloned()
    }
}

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

/// The modules a file depends on (resolved to top-level [`MODULES`] names), from both `crate::`
/// paths — including crate-root re-exports (`reexports`) — and `super::` chains resolved against
/// the file's module `chain`. `#[cfg(test)]` modules are not counted (they don't ship).
fn file_edges(
    text: &str,
    chain: &[String],
    reexports: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    let code = strip_cfg_test_mods(text);
    let mut uses = BTreeSet::new();
    for name in crate_refs(&code) {
        if let Some(m) = resolve(&name, reexports) {
            uses.insert(m);
        }
    }
    for (supers, name) in super_targets(&strip_line_comments(&code)) {
        // `super` from inside a file points at its parent module; `supers` of them climb that
        // many levels. Only when they reach the crate root does the next segment name another
        // top-level module; a shallower `super::` stays inside this module (a self-reference).
        if supers >= chain.len()
            && let Some(m) = resolve(&name, reexports)
        {
            uses.insert(m);
        }
    }
    uses
}

/// Module → the CORE-014 modules its files name.
fn graph(src: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let reexports = reexports(src);
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
            let chain = module_chain(&file, src);
            uses.extend(
                file_edges(&text, &chain, &reexports)
                    .into_iter()
                    .filter(|m| m != module),
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
    let listed: BTreeSet<String> = MODULES.iter().map(|m| (*m).to_string()).collect();
    assert_eq!(declared(&src), listed, "scan every module of src/lib.rs");
    let graph = graph(&src);
    for module in MODULES {
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
            "index",
            "watch",
            "graph",
            "pipeline",
            "projects",
            "settings",
            "tools",
            "jobs",
            "state",
            "workspace",
            "session",
            "config",
            "server",
            "transport",
            "rpc",
        ],
    );
    forbidden(
        "index",
        &[
            "watch",
            "pipeline",
            "projects",
            "settings",
            "tools",
            "jobs",
            "workspace",
        ],
    );
    // The state databases sit on the store (their folders are made through `fsops`) and below
    // the workspaces that open them.
    forbidden("state", &["workspace", "index", "watch", "session"]);
    // The running core builds everything, and nothing names it; only it builds the router.
    for module in graph.keys().filter(|m| *m != "server") {
        forbidden(module, &["server"]);
    }
    for module in graph.keys().filter(|m| *m != "server" && *m != "rpc") {
        forbidden(module, &["rpc"]);
    }
    forbidden("transport", &["rpc", "docs", "interchange", "tools"]);
    forbidden("pipeline", &["tools", "jobs"]);
    forbidden("jobs", &["tools", "engines", "meter"]);
    forbidden("telemetry", &["tools", "jobs", "engines", "meter"]);
    forbidden("tools", &["engines", "meter"]);
    // `confirm` implements the `tools` confirm stage; the dispatcher reaches it only through
    // that trait (`ConfirmStage::on_replay`).
    forbidden("tools", &["confirm"]);
    forbidden("jobs", &["confirm"]);
    // `desktop` is infrastructure (P4a-R24): it drives the jobs' pause and watches them for
    // keep-awake; `tools` registers its tools, so it never uses `tools`, and neither `jobs` nor
    // `telemetry` knows it.
    forbidden("desktop", &["tools", "confirm", "meter", "engines"]);
    forbidden("jobs", &["desktop"]);
    forbidden("telemetry", &["desktop"]);
    assert!(graph["desktop"].contains("jobs"), "{graph:?}");
    assert!(graph["tools"].contains("desktop"), "{graph:?}");
    assert!(graph["confirm"].contains("tools"), "{graph:?}");
    // `diagnostics` is infrastructure too (P4a-R24): it reads the engines, the telemetry and
    // the jobs for the bundle and registers its own tools, so it uses `tools`; the dispatcher
    // reaches it only through the `tools` error sink, and only the server names it.
    let users: Vec<&String> = graph
        .iter()
        .filter(|(m, uses)| m.as_str() != "diagnostics" && uses.contains("diagnostics"))
        .map(|(m, _)| m)
        .collect();
    assert_eq!(users, ["server"], "{users:?} use diagnostics");
    assert!(graph["diagnostics"].contains("tools"), "{graph:?}");
    // The redaction sits below both of its users and names no module of the crate (PR #53).
    assert!(
        graph["redact"].is_empty(),
        "redact uses {:?}",
        graph["redact"]
    );
    assert!(graph["diagnostics"].contains("redact"), "{graph:?}");
    assert!(graph["interchange"].contains("redact"), "{graph:?}");
    // `resources` is registered by the server like `engines`: no other module names it, and it
    // uses `tools` (the call context and the registry) and the store.
    for module in graph.keys().filter(|m| *m != "resources" && *m != "server") {
        forbidden(module, &["resources"]);
    }
    // The holds and the handler registry are leaves of the module graph: the server wires them
    // to the tools, and no other module names them.
    for leaf in ["interchange", "docs"] {
        let users: Vec<&String> = graph
            .iter()
            .filter(|(m, uses)| m.as_str() != leaf && uses.contains(leaf))
            .map(|(m, _)| m)
            .collect();
        assert_eq!(users, ["server"], "{users:?} use {leaf}");
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
    assert!(graph["state"].contains("store"), "{graph:?}");
    assert!(graph["workspace"].contains("state"), "{graph:?}");
    assert!(graph["workspace"].contains("store"), "{graph:?}");
    assert!(graph["server"].contains("rpc"), "{graph:?}");
    assert!(graph["rpc"].contains("transport"), "{graph:?}");
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

/// The three forms the reviewer's probes slipped past before (extras fix round 1, I3): a
/// `super::super::` path, a crate-root re-export reached by its item name, and a `pub(crate) mod`
/// declaration. Each must now be seen.
#[test] // spec: CORE-010
fn the_scan_sees_super_reexports_and_restricted_mods() {
    // 1. `super::super::state` from `src/store/write.rs` climbs to the crate root, so it names
    // the `state` module — a real store → state edge. A single `super::` stays inside `store`.
    let reexports: BTreeMap<String, String> = [
        ("RunningCore", "server"),
        ("CoreThread", "server"),
        ("start_in_thread", "server"),
        ("CoreConfig", "config"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    let chain = ["store".to_string(), "write".to_string()];
    let edges = file_edges(
        "use super::super::state::StateDb as _;\nuse super::fsops;\n",
        &chain,
        &reexports,
    );
    assert!(
        edges.contains("state"),
        "super::super::state missed: {edges:?}"
    );
    assert!(
        !edges.contains("store"),
        "super::fsops is a self-reference: {edges:?}"
    );

    // 2. A crate-root re-export reached by its item name is an edge to the module it comes from.
    let edges = file_edges(
        "use crate::RunningCore as _;\n",
        &["transport".into()],
        &reexports,
    );
    assert!(
        edges.contains("server"),
        "crate::RunningCore missed: {edges:?}"
    );
    let edges = file_edges(
        "use crate::{CoreThread, start_in_thread};\n",
        &["rpc".into()],
        &reexports,
    );
    assert!(
        edges.contains("server"),
        "crate::{{CoreThread, …}} missed: {edges:?}"
    );

    // 3. `pub(crate) mod` (and `pub(super) mod`) are declarations the list check must see.
    assert_eq!(
        mod_name("pub(crate) mod zzprobe;"),
        Some("zzprobe".to_string())
    );
    assert_eq!(mod_name("pub(super) mod inner;"), Some("inner".to_string()));
    assert_eq!(mod_name("pub mod server;"), Some("server".to_string()));
    assert_eq!(mod_name("mod conn;"), Some("conn".to_string()));
    assert_eq!(mod_name("    let mod_x = 1;"), None);

    // A `#[cfg(test)]` module doesn't ship, so its uses aren't production edges.
    let edges = file_edges(
        "use crate::store::Store;\n#[cfg(test)]\nmod tests {\n    use crate::server::RunningCore;\n}\n",
        &["rpc".into()],
        &reexports,
    );
    assert!(edges.contains("store"), "{edges:?}");
    assert!(
        !edges.contains("server"),
        "a cfg(test) module is not a production edge: {edges:?}"
    );
}
