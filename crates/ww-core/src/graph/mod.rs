//! The dependency graph (I6, CORE-048): pure functions over versions and edges, no I/O.
//!
//! - [`is_stale`]: the staleness rule for one input: its `approved_version` is now higher
//!   than the version in the dependent's ref, and the input isn't pinned.
//! - [`cycles`]: the cycles of a graph of `(project, asset_id)` nodes. Updates are processed
//!   in topological order, and a cycle is an error to report, never resolved silently (I6).

use std::collections::{BTreeMap, BTreeSet};

use ww_schema::Version;

/// Whether a dependent whose input ref names `input_version` is stale: the input's
/// `approved_version` is higher, and the input isn't in the dependent's `pinned_inputs` (I6).
pub fn is_stale(input_version: &Version, approved: Option<&Version>, pinned: bool) -> bool {
    !pinned && approved.is_some_and(|approved| approved > input_version)
}

/// A node: `(project, asset_id)`.
type Node = (String, String);

/// The cycles of the graph whose edges are `(from, to)`: each strongly connected component
/// with more than one node, or a node with an edge to itself, as its nodes in sorted order.
/// The cycles are sorted by their first node. A graph without cycles gives none.
pub fn cycles(edges: &[(Node, Node)]) -> Vec<Vec<Node>> {
    let mut graph: BTreeMap<&Node, BTreeSet<&Node>> = BTreeMap::new();
    for (from, to) in edges {
        graph.entry(from).or_default().insert(to);
        graph.entry(to).or_default();
    }
    let mut tarjan = Tarjan {
        graph: &graph,
        next: 0,
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        found: Vec::new(),
    };
    for node in graph.keys() {
        if !tarjan.index.contains_key(node) {
            tarjan.connect(node);
        }
    }
    let mut found: Vec<Vec<Node>> = tarjan
        .found
        .into_iter()
        .filter(|scc| scc.len() > 1 || graph[scc[0]].contains(scc[0]))
        .map(|scc| {
            let mut nodes: Vec<Node> = scc.into_iter().cloned().collect();
            nodes.sort();
            nodes
        })
        .collect();
    found.sort();
    found
}

/// Tarjan's strongly connected components, iterative so a long chain can't overflow the
/// stack.
struct Tarjan<'g> {
    graph: &'g BTreeMap<&'g Node, BTreeSet<&'g Node>>,
    next: usize,
    index: BTreeMap<&'g Node, usize>,
    low: BTreeMap<&'g Node, usize>,
    stack: Vec<&'g Node>,
    on_stack: BTreeSet<&'g Node>,
    found: Vec<Vec<&'g Node>>,
}

impl<'g> Tarjan<'g> {
    fn visit(&mut self, node: &'g Node) {
        self.index.insert(node, self.next);
        self.low.insert(node, self.next);
        self.next += 1;
        self.stack.push(node);
        self.on_stack.insert(node);
    }

    fn connect(&mut self, root: &'g Node) {
        // (node, its successors still to look at)
        let mut work: Vec<(&'g Node, Vec<&'g Node>)> = Vec::new();
        self.visit(root);
        work.push((root, self.graph[root].iter().copied().collect()));
        while let Some((node, pending)) = work.last_mut() {
            let node = *node;
            if let Some(next) = pending.pop() {
                if !self.index.contains_key(next) {
                    self.visit(next);
                    work.push((next, self.graph[next].iter().copied().collect()));
                } else if self.on_stack.contains(next) {
                    let low = self.low[node].min(self.index[next]);
                    self.low.insert(node, low);
                }
                continue;
            }
            work.pop();
            if let Some((parent, _)) = work.last() {
                let low = self.low[*parent].min(self.low[node]);
                self.low.insert(*parent, low);
            }
            if self.low[node] == self.index[node] {
                let mut scc = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(member);
                    scc.push(member);
                    if member == node {
                        break;
                    }
                }
                self.found.push(scc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(a: &str) -> Node {
        ("p".to_string(), a.to_string())
    }

    #[test] // spec: CORE-048
    fn stale_only_when_a_newer_version_is_approved_and_not_pinned() {
        let v = |s: &str| s.parse::<Version>().unwrap();
        assert!(is_stale(&v("1.0.0"), Some(&v("1.1.0")), false));
        assert!(!is_stale(&v("1.0.0"), Some(&v("1.1.0")), true));
        assert!(!is_stale(&v("1.1.0"), Some(&v("1.1.0")), false));
        assert!(!is_stale(&v("2.0.0"), Some(&v("1.1.0")), false));
        assert!(!is_stale(&v("1.0.0"), None, false));
    }

    #[test] // spec: CORE-048
    fn a_long_chain_has_no_cycle_and_does_not_overflow() {
        let edges: Vec<(Node, Node)> = (0..20_000)
            .map(|i| (n(&format!("a{i}")), n(&format!("a{}", i + 1))))
            .collect();
        assert!(cycles(&edges).is_empty());
    }
}
