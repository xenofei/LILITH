//! Dependency-boundary test (ARCH-111, QA-027): `lil-schema`, `lil-protocol` and `lil-parse` have
//! no Qt or Windows crates anywhere in their normal dependency closure.
//!
//! Each crate's closure is resolved on its own, as a standalone build of that crate resolves
//! it: `cargo tree -p <crate> -e normal --prefix none` for the one supported target,
//! `x86_64-pc-windows-msvc` (ruling P3-R8). The workspace-wide `cargo metadata` graph unifies
//! features across every member: once `lil-mcp` depends on `rmcp`, `rmcp`'s `schemars`
//! `chrono04` feature puts `chrono` (and through it `windows-link`) into `lil-protocol`'s and
//! `lil-schema`'s unified closures, although neither crate uses it. Only normal dependency
//! edges count; dev and build edges do not ship in the crate.
//!
//! The `cargo metadata` walker stays for the workspace-level check that `rmcp` really is in
//! the graph, so the per-crate check runs where it matters.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const TARGET: &str = "x86_64-pc-windows-msvc";

/// Runs `cargo <args> --manifest-path <workspace>\Cargo.toml` and returns its stdout.
fn cargo(args: &[&str]) -> Vec<u8> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args(args)
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .unwrap_or_else(|e| panic!("failed to run `cargo {}`: {e}", args.join(" ")));
    assert!(
        output.status.success(),
        "`cargo {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Runs `cargo metadata` for the workspace and parses its JSON.
fn cargo_metadata() -> Value {
    let stdout = cargo(&[
        "metadata",
        "--format-version",
        "1",
        "--filter-platform",
        TARGET,
    ]);
    serde_json::from_slice(&stdout).expect("`cargo metadata` did not print JSON")
}

/// Names of every crate reachable from `root` through normal dependency edges.
/// `root` itself is not included. Panics if `root` is not a package in the graph.
fn closure_from_metadata(metadata: &Value, root: &str) -> BTreeSet<String> {
    let packages = metadata["packages"].as_array().expect("packages array");
    let name_of: BTreeMap<&str, &str> = packages
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve.nodes array");
    let node_by_id: BTreeMap<&str, &Value> = nodes
        .iter()
        .map(|n| (n["id"].as_str().unwrap(), n))
        .collect();

    let root_id = name_of
        .iter()
        .find(|(_, name)| **name == root)
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("package `{root}` not found in cargo metadata"));

    let mut seen: BTreeSet<&str> = BTreeSet::from([root_id]);
    let mut queue: VecDeque<&str> = VecDeque::from([root_id]);
    let mut closure = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        let Some(node) = node_by_id.get(id) else {
            continue;
        };
        for dep in node["deps"].as_array().expect("deps array") {
            let is_normal = dep["dep_kinds"]
                .as_array()
                .expect("dep_kinds array")
                .iter()
                .any(|k| k["kind"].is_null());
            if !is_normal {
                continue;
            }
            let dep_id = dep["pkg"].as_str().unwrap();
            if seen.insert(dep_id) {
                closure.insert(name_of[dep_id].to_string());
                queue.push_back(dep_id);
            }
        }
    }
    closure
}

/// The crate names in `cargo tree --prefix none` output, without its first line (the root
/// crate). Each line is `<name> v<version>` with an optional `(<path>)` or `(*)`.
fn closure_from_tree(text: &str) -> BTreeSet<String> {
    text.lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// `root`'s normal closure as a standalone build of `root` resolves it (P3-R8).
fn standalone_closure(root: &str) -> BTreeSet<String> {
    let stdout = cargo(&[
        "tree", "-p", root, "-e", "normal", "--prefix", "none", "--target", TARGET,
    ]);
    let text = String::from_utf8(stdout).expect("`cargo tree` printed UTF-8");
    let first = text.lines().next().unwrap_or_default();
    assert!(
        first.starts_with(&format!("{root} v")),
        "`cargo tree -p {root}` did not start with {root}: {text}"
    );
    closure_from_tree(&text)
}

/// The Qt and Windows crates in `closure`.
fn platform_crates(closure: &BTreeSet<String>) -> Vec<&String> {
    closure
        .iter()
        .filter(|n| {
            n.starts_with("windows")
                || n.starts_with("winapi")
                || n.starts_with("cxx")
                || n.starts_with("qt")
        })
        .collect()
}

#[test] // spec: ARCH-111, QA-027
fn schema_and_protocol_have_no_platform_deps() {
    for root in ["lil-schema", "lil-protocol", "lil-parse"] {
        let closure = standalone_closure(root);
        assert!(closure.contains("serde_json"), "{root}: {closure:?}");
        let bad = platform_crates(&closure);
        assert!(bad.is_empty(), "{root} depends on {bad:?}");
    }
}

/// P3-R8: with `rmcp` in the workspace (through `lil-mcp`), each crate's own closure still
/// holds no platform crate, and one added to a closure on purpose is reported.
#[test] // spec: ARCH-111, QA-027
fn boundaries_hold_per_crate_with_rmcp_in_the_workspace() {
    let metadata = cargo_metadata();
    assert!(
        closure_from_metadata(&metadata, "lil-mcp").contains("rmcp"),
        "rmcp is not in the workspace graph, so this test would prove nothing"
    );
    for root in ["lil-schema", "lil-protocol", "lil-parse"] {
        let closure = standalone_closure(root);
        assert!(!closure.contains("rmcp"), "{root}: {closure:?}");
        let bad = platform_crates(&closure);
        assert!(bad.is_empty(), "{root} depends on {bad:?}");
    }

    // A parsed `cargo tree` fixture with a platform crate added on purpose.
    let fixture = [
        r"lil-protocol v0.0.0 (C:\src\crates\lil-protocol)",
        "serde v1.0.229",
        "chrono v0.4.45",
        "windows-link v0.2.1",
        "serde v1.0.229 (*)",
    ]
    .join("\n");
    let closure = closure_from_tree(&fixture);
    assert_eq!(
        closure,
        BTreeSet::from(["serde", "chrono", "windows-link"].map(String::from))
    );
    assert_eq!(platform_crates(&closure), [&"windows-link".to_string()]);
}

/// The walker follows transitive normal edges and ignores dev and build edges.
#[test] // spec: QA-027
fn closure_follows_only_normal_edges() {
    let metadata = serde_json::json!({
        "packages": [
            {"id": "root", "name": "root"},
            {"id": "mid", "name": "mid"},
            {"id": "leaf", "name": "leaf"},
            {"id": "dev-only", "name": "windows-sys"},
            {"id": "build-only", "name": "cxx-build"},
        ],
        "resolve": {"nodes": [
            {"id": "root", "deps": [
                {"pkg": "mid", "dep_kinds": [{"kind": null, "target": null}]},
                {"pkg": "dev-only", "dep_kinds": [{"kind": "dev", "target": null}]},
                {"pkg": "build-only", "dep_kinds": [{"kind": "build", "target": null}]},
            ]},
            {"id": "mid", "deps": [
                {"pkg": "leaf", "dep_kinds": [
                    {"kind": "dev", "target": null},
                    {"kind": null, "target": "cfg(windows)"},
                ]},
            ]},
            {"id": "leaf", "deps": []},
            {"id": "dev-only", "deps": []},
            {"id": "build-only", "deps": []},
        ]},
    });
    let closure = closure_from_metadata(&metadata, "root");
    assert_eq!(
        closure,
        BTreeSet::from(["mid".to_string(), "leaf".to_string()])
    );
}
