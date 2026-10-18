//! Dependency-boundary test (ARCH-111, QA-027): `lil-schema` and `lil-protocol` have no Qt or
//! Windows crates anywhere in their normal dependency closure.
//!
//! The closure is read from `cargo metadata` for the one supported target,
//! `x86_64-pc-windows-msvc`, so it is the real Windows build graph. Only normal dependency
//! edges count (`dep_kinds[].kind == null`); dev and build edges do not ship in the crate.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const TARGET: &str = "x86_64-pc-windows-msvc";

/// Runs `cargo metadata` for the workspace and parses its JSON.
fn cargo_metadata() -> Value {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            TARGET,
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("failed to run `cargo metadata`");
    assert!(
        output.status.success(),
        "`cargo metadata` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("`cargo metadata` did not print JSON")
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

fn normal_dependency_closure(root: &str) -> BTreeSet<String> {
    closure_from_metadata(&cargo_metadata(), root)
}

#[test] // spec: ARCH-111, QA-027
fn schema_and_protocol_have_no_platform_deps() {
    for root in ["lil-schema", "lil-protocol"] {
        let closure = normal_dependency_closure(root);
        let bad: Vec<_> = closure
            .iter()
            .filter(|n| {
                n.starts_with("windows")
                    || n.starts_with("winapi")
                    || n.starts_with("cxx")
                    || n.starts_with("qt")
            })
            .collect();
        assert!(bad.is_empty(), "{root} depends on {bad:?}");
    }
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
