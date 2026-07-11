//! `FixtureGraph` (W2/W3/W4) — the fixture dependency DAG: single source of truth for ordering and
//! closure computation (design 04 §2).
//!
//! Nodes are post-override `Fixture` definitions; edges are `requests` dependencies. Built once per
//! collection pass. Validity checks at build time: acyclicity (3-color DFS → `FixtureError::Cycle`)
//! and scope-monotonicity (`FixtureError::ScopeWiden`).
//!
//! **Implemented by** Lane FX-graph (subagents fx-graph, fx-resolver): W2/W3/W4.
//!
//! ## Algorithm choices (understand-before-applying)
//! - **Build** resolves each `dep` *name* to a concrete [`NodeId`] via the [`OverrideTable`] from the
//!   *dependent's* `scope_path` — the same name can resolve to different definitions per location
//!   (design 04 §1.4), so edges are location-aware, not name-global.
//! - **Cycle detection** is a 3-color (White/Gray/Black) DFS. The standard choice: a Gray→Gray edge
//!   is a back-edge ⇒ cycle, and the Gray stack *is* the offending path (we slice it from the first
//!   re-encountered node so `path` is exactly the loop in request order, per `FixtureError::Cycle`).
//!   Detecting cycles before topo-sort means the run aborts collection instead of deadlocking at
//!   setup.
//! - **Topo order** is a deterministic Kahn-style sort: nodes are visited in a stable id order so the
//!   setup sequence is reproducible (load-bearing — teardown is its strict reverse and the closure
//!   hash depends on it).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::domain::{NodeId, ScopePath};
use crate::fixtures::fixture::Fixture;
use crate::fixtures::fixture_closure::FixtureClosure;
use crate::fixtures::fixture_error::FixtureError;
use crate::fixtures::override_table::OverrideTable;

/// DFS marking color for 3-color cycle detection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    /// Not yet visited.
    White,
    /// On the current DFS stack (a re-entry here is a back-edge ⇒ cycle).
    Gray,
    /// Fully explored.
    Black,
}

/// The fixture dependency DAG.
#[derive(Debug, Default)]
pub struct FixtureGraph {
    /// Interned fixture definitions, by node id.
    nodes: HashMap<NodeId, Fixture>,
    /// `requests` edges: a fixture → the fixtures it depends on (resolved node ids).
    edges: HashMap<NodeId, Vec<NodeId>>,
}

impl FixtureGraph {
    /// Build the DAG from collected fixtures and the override table, resolving each `dep` name to a
    /// concrete node id via nearest-override (W2), then validating it (W3).
    ///
    /// Resolution uses the *dependent fixture's own* `scope_path`. An unresolvable dep name is a
    /// [`FixtureError::Unresolved`]. After edges are built, both validity checks run; the first
    /// violation aborts the build.
    pub fn build(
        fixtures: Vec<Fixture>,
        overrides: &OverrideTable,
    ) -> std::result::Result<Self, FixtureError> {
        // Intern nodes in a deterministic (BTree) order so topo/iteration are reproducible.
        let mut nodes: HashMap<NodeId, Fixture> = HashMap::with_capacity(fixtures.len());
        for f in fixtures {
            nodes.insert(f.node_id.clone(), f);
        }

        let mut edges: HashMap<NodeId, Vec<NodeId>> = HashMap::with_capacity(nodes.len());
        // Deterministic order over nodes for stable edge lists.
        let ordered: BTreeMap<&NodeId, &Fixture> = nodes.iter().collect();
        for (node_id, fixture) in ordered {
            let mut resolved = Vec::with_capacity(fixture.deps.len());
            for dep_name in &fixture.deps {
                match overrides.nearest(dep_name, &fixture.scope_path) {
                    Some(dep_node) => resolved.push(dep_node),
                    None => {
                        return Err(FixtureError::Unresolved {
                            name: dep_name.clone(),
                            scope_path: fixture.scope_path.clone(),
                        })
                    }
                }
            }
            edges.insert(node_id.clone(), resolved);
        }

        let graph = Self { nodes, edges };
        graph.detect_cycles()?;
        graph.check_scope_monotonicity()?;
        Ok(graph)
    }

    /// Detect cycles via 3-color DFS; a back-edge is a [`FixtureError::Cycle`] (W3).
    ///
    /// Roots are visited in stable id order; the gray-path stack yields the cycle in request order.
    pub fn detect_cycles(&self) -> std::result::Result<(), FixtureError> {
        // Owned-key color map (NodeId is cheap-cloneable) — avoids borrow-lifetime invariance with
        // the iterative DFS that also mutates a path of owned NodeIds.
        let mut color: HashMap<NodeId, Color> = self
            .nodes
            .keys()
            .map(|n| (n.clone(), Color::White))
            .collect();
        // Stable visitation order so a reported cycle path is deterministic.
        let roots: BTreeSet<&NodeId> = self.nodes.keys().collect();
        for root in roots {
            if color.get(root).copied() == Some(Color::White) {
                let mut stack: Vec<NodeId> = Vec::new();
                if let Some(cycle) = self.dfs_cycle(root, &mut color, &mut stack) {
                    return Err(FixtureError::Cycle { path: cycle });
                }
            }
        }
        Ok(())
    }

    /// Iterative 3-color DFS from `start`. Returns the cycle (fixture names, request order) on a
    /// back-edge, else `None`. Iterative (explicit work stack) to avoid recursion depth limits on
    /// long dependency chains.
    fn dfs_cycle(
        &self,
        start: &NodeId,
        color: &mut HashMap<NodeId, Color>,
        path: &mut Vec<NodeId>,
    ) -> Option<Vec<String>> {
        // Work items: (node, index-of-next-child-to-visit). Enter = mark gray + push path.
        let mut work: Vec<(NodeId, usize)> = vec![(start.clone(), 0)];
        color.insert(start.clone(), Color::Gray);
        path.push(start.clone());

        while let Some((node, child_idx)) = work.last().cloned() {
            let children = self.edges.get(&node).map(Vec::as_slice).unwrap_or(&[]);
            if child_idx < children.len() {
                // Advance this frame's cursor before descending.
                if let Some(last) = work.last_mut() {
                    last.1 = child_idx + 1;
                }
                let child = &children[child_idx];
                match color.get(child).copied().unwrap_or(Color::White) {
                    Color::Gray => {
                        // Back-edge: slice the gray path from the first occurrence of `child`.
                        let start_at = path.iter().position(|n| n == child).unwrap_or(0);
                        let mut cycle: Vec<String> = path[start_at..]
                            .iter()
                            .map(|n| self.display_name(n))
                            .collect();
                        // Close the loop visually (a → b → a).
                        cycle.push(self.display_name(child));
                        return Some(cycle);
                    }
                    Color::White => {
                        color.insert(child.clone(), Color::Gray);
                        path.push(child.clone());
                        work.push((child.clone(), 0));
                    }
                    Color::Black => {}
                }
            } else {
                // Frame exhausted: mark black, pop path + work.
                color.insert(node.clone(), Color::Black);
                path.pop();
                work.pop();
            }
        }
        None
    }

    /// Verify scope-monotonicity: every dep is equal-or-wider scope than its dependent
    /// ([`FixtureError::ScopeWiden`] otherwise) (W3).
    ///
    /// Legal edge invariant: `dep.scope.outlives(node.scope) || dep.scope == node.scope`. A violation
    /// reports `wide` = the depending (offending) fixture's scope, `narrow` = the illegal dep's scope.
    pub fn check_scope_monotonicity(&self) -> std::result::Result<(), FixtureError> {
        // Deterministic iteration for a stable first-violation report.
        let ordered: BTreeSet<&NodeId> = self.nodes.keys().collect();
        for node in ordered {
            let Some(fixture) = self.nodes.get(node) else {
                continue;
            };
            for dep in self.edges.get(node).map(Vec::as_slice).unwrap_or(&[]) {
                let Some(dep_fixture) = self.nodes.get(dep) else {
                    continue;
                };
                let legal =
                    dep_fixture.scope.outlives(fixture.scope) || dep_fixture.scope == fixture.scope;
                if !legal {
                    return Err(FixtureError::ScopeWiden {
                        narrow: dep_fixture.scope,
                        wide: fixture.scope,
                    });
                }
            }
        }
        Ok(())
    }

    /// A stable topological setup order over all nodes (W4).
    ///
    /// Kahn's algorithm with a deterministic (BTree) ready-set so the order is reproducible. Assumes
    /// acyclicity (guaranteed by [`Self::build`]); if a cycle somehow remains, any nodes that never
    /// reach in-degree zero are appended in id order so the output still contains every node.
    pub fn topo_order(&self) -> Vec<NodeId> {
        // in-degree(node) = number of its deps (prerequisites) that exist as nodes. A node with no
        // deps is immediately ready.
        let mut in_degree: BTreeMap<&NodeId, usize> =
            self.nodes.keys().map(|n| (n, 0usize)).collect();
        for (node, deps) in &self.edges {
            if let Some(d) = in_degree.get_mut(node) {
                *d = deps
                    .iter()
                    .filter(|dep| self.nodes.contains_key(*dep))
                    .count();
            }
        }

        // Build reverse adjacency: dep → dependents, so removing a dep decrements its dependents.
        let mut dependents: BTreeMap<&NodeId, Vec<&NodeId>> = BTreeMap::new();
        for (node, deps) in &self.edges {
            for dep in deps {
                if self.nodes.contains_key(dep) {
                    dependents.entry(dep).or_default().push(node);
                }
            }
        }

        let mut ready: BTreeSet<&NodeId> = in_degree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(n, _)| *n)
            .collect();
        let mut order: Vec<NodeId> = Vec::with_capacity(self.nodes.len());

        while let Some(&next) = ready.iter().next() {
            ready.remove(next);
            order.push(next.clone());
            if let Some(deps) = dependents.get(next) {
                for &dependent in deps {
                    if let Some(d) = in_degree.get_mut(dependent) {
                        *d = d.saturating_sub(1);
                        if *d == 0 {
                            ready.insert(dependent);
                        }
                    }
                }
            }
        }

        // Safety net for any node not emitted (only reachable if a cycle slipped through).
        if order.len() < self.nodes.len() {
            let emitted: BTreeSet<NodeId> = order.iter().cloned().collect();
            for node in self.nodes.keys().collect::<BTreeSet<_>>() {
                if !emitted.contains(node) {
                    order.push(node.clone());
                }
            }
        }
        order
    }

    /// The transitive closure (requested + autouse + transitive deps) for a test at `scope_path`,
    /// topo-ordered (W4).
    ///
    /// Resolves each requested name and every autouse fixture visible at `scope_path` to a node id,
    /// transitively pulls in their deps, then orders the resulting set by the graph's global topo
    /// order so setup is correct. An unresolvable requested name is [`FixtureError::Unresolved`].
    pub fn closure_of(
        &self,
        requested: &[String],
        scope_path: &ScopePath,
    ) -> std::result::Result<FixtureClosure, FixtureError> {
        let mut seeds: Vec<NodeId> = Vec::new();

        // Requested fixtures must resolve from this test's location.
        for name in requested {
            match self.resolve_visible(name, scope_path) {
                Some(node) => seeds.push(node),
                None => {
                    return Err(FixtureError::Unresolved {
                        name: name.clone(),
                        scope_path: scope_path.clone(),
                    })
                }
            }
        }

        // Autouse fixtures enter through the location door (design 04 §1.2): any autouse fixture
        // whose declaring scope_path is a prefix of this test's location is injected.
        for (node_id, fixture) in self.deterministic_nodes() {
            if fixture.autouse && Self::location_applies(&fixture.scope_path, scope_path) {
                seeds.push(node_id.clone());
            }
        }

        // Transitive expansion over edges.
        let mut included: BTreeSet<NodeId> = BTreeSet::new();
        let mut frontier: Vec<NodeId> = seeds;
        while let Some(node) = frontier.pop() {
            if !included.insert(node.clone()) {
                continue;
            }
            for dep in self.edges.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
                if !included.contains(dep) {
                    frontier.push(dep.clone());
                }
            }
        }

        // Order the closure by the graph's global topo order (setup order).
        let ordered: Vec<NodeId> = self
            .topo_order()
            .into_iter()
            .filter(|n| included.contains(n))
            .collect();
        Ok(FixtureClosure::new(ordered))
    }

    /// Resolve a fixture *name* visible from `scope_path` to a node id: prefer the override-table
    /// nearest match; fall back to a same-name node whose declaring location applies (covers
    /// fixtures inserted directly without a populated override table, used widely in unit tests).
    fn resolve_visible(&self, name: &str, scope_path: &ScopePath) -> Option<NodeId> {
        // First, any node literally named `name` at a location that applies. Choose the one whose
        // declaring location is the longest applicable prefix (nearest wins), mirroring the table.
        let mut best: Option<(usize, &NodeId)> = None;
        for (node_id, fixture) in self.deterministic_nodes() {
            if fixture.name != name {
                continue;
            }
            if let Some(len) = Self::prefix_len(&fixture.scope_path, scope_path) {
                if best.is_none_or(|(b, _)| len > b) {
                    best = Some((len, node_id));
                }
            }
        }
        best.map(|(_, n)| n.clone())
    }

    /// Nodes in a deterministic (id-sorted) order — stable autouse injection + name resolution.
    fn deterministic_nodes(&self) -> impl Iterator<Item = (&NodeId, &Fixture)> {
        let ordered: BTreeMap<&NodeId, &Fixture> = self.nodes.iter().collect();
        ordered.into_iter()
    }

    /// `true` if a fixture declared at `decl` applies to a test at `test_loc` (decl is a segment-wise
    /// prefix of the test location; the session root applies everywhere).
    fn location_applies(decl: &ScopePath, test_loc: &ScopePath) -> bool {
        Self::prefix_len(decl, test_loc).is_some()
    }

    /// Segment-wise prefix length (in segments) of `decl.module` within `test_loc.module`, or `None`
    /// if not a prefix. Shares the segment semantics of `OverrideTable` (`/` and `.` separators).
    fn prefix_len(decl: &ScopePath, test_loc: &ScopePath) -> Option<usize> {
        let dseg: Vec<&str> = decl
            .module
            .split(['/', '.'])
            .filter(|s| !s.is_empty())
            .collect();
        let tseg: Vec<&str> = test_loc
            .module
            .split(['/', '.'])
            .filter(|s| !s.is_empty())
            .collect();
        if dseg.len() > tseg.len() {
            return None;
        }
        if dseg.iter().zip(&tseg).all(|(d, t)| d == t) {
            Some(dseg.len())
        } else {
            None
        }
    }

    /// The fixture's `name` for diagnostics (falls back to the node id string if unknown).
    fn display_name(&self, node: &NodeId) -> String {
        self.nodes
            .get(node)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| node.to_string())
    }

    /// Read-only access to an interned fixture by node id.
    ///
    /// Defined (trivial accessor) so dependent lanes can read node metadata without a scaffold.
    pub fn fixture(&self, node: &NodeId) -> Option<&Fixture> {
        self.nodes.get(node)
    }

    /// The direct dependency edges of `node`, if present.
    pub fn deps_of(&self, node: &NodeId) -> Option<&[NodeId]> {
        self.edges.get(node).map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Scope;

    fn fix(id: &str, name: &str, scope: Scope) -> Fixture {
        Fixture::new(NodeId::new(id), name, scope, ScopePath::module(""))
    }

    fn fix_at(id: &str, name: &str, scope: Scope, module: &str) -> Fixture {
        Fixture::new(NodeId::new(id), name, scope, ScopePath::module(module))
    }

    /// Build a table mapping every fixture's (name, scope_path) to its node id so deps resolve.
    fn table(fixtures: &[Fixture]) -> OverrideTable {
        let mut t = OverrideTable::new();
        for f in fixtures {
            t.insert(f.name.clone(), &f.scope_path, f.node_id.clone());
        }
        t
    }

    #[test]
    fn build_single_node_no_deps() {
        // happy / single node.
        let f = fix("a", "a", Scope::Function);
        let g = FixtureGraph::build(vec![f], &OverrideTable::new()).expect("builds");
        assert_eq!(g.topo_order(), vec![NodeId::new("a")]);
    }

    #[test]
    fn build_empty_graph() {
        // empty.
        let g = FixtureGraph::build(vec![], &OverrideTable::new()).expect("builds");
        assert!(g.topo_order().is_empty());
    }

    #[test]
    fn build_resolves_deps_via_table() {
        // happy: a depends on b; topo puts b before a.
        let fa = fix("a", "a", Scope::Function).with_deps(vec!["b".into()]);
        let fb = fix("b", "b", Scope::Function);
        let fixtures = vec![fa, fb];
        let t = table(&fixtures);
        let g = FixtureGraph::build(fixtures, &t).expect("builds");
        let order = g.topo_order();
        let pos_a = order.iter().position(|n| n.as_str() == "a").unwrap();
        let pos_b = order.iter().position(|n| n.as_str() == "b").unwrap();
        assert!(pos_b < pos_a, "dep b must precede dependent a: {order:?}");
    }

    #[test]
    fn build_unresolved_dep_errors() {
        // error: dep name has no definition.
        let fa = fix("a", "a", Scope::Function).with_deps(vec!["ghost".into()]);
        let err = FixtureGraph::build(vec![fa], &OverrideTable::new()).unwrap_err();
        assert!(matches!(err, FixtureError::Unresolved { name, .. } if name == "ghost"));
    }

    #[test]
    fn detect_self_loop() {
        // error / cycle: self-dependency.
        let fa = fix("a", "a", Scope::Function).with_deps(vec!["a".into()]);
        let fixtures = vec![fa];
        let t = table(&fixtures);
        let err = FixtureGraph::build(fixtures, &t).unwrap_err();
        match err {
            FixtureError::Cycle { path } => assert!(path.contains(&"a".to_string())),
            other => panic!("expected cycle, got {other:?}"),
        }
    }

    #[test]
    fn detect_long_cycle() {
        // adversarial: a→b→c→a long chain cycle.
        let fa = fix("a", "a", Scope::Function).with_deps(vec!["b".into()]);
        let fb = fix("b", "b", Scope::Function).with_deps(vec!["c".into()]);
        let fc = fix("c", "c", Scope::Function).with_deps(vec!["a".into()]);
        let fixtures = vec![fa, fb, fc];
        let t = table(&fixtures);
        let err = FixtureGraph::build(fixtures, &t).unwrap_err();
        match err {
            FixtureError::Cycle { path } => {
                assert!(path.contains(&"a".to_string()));
                assert!(path.contains(&"b".to_string()));
                assert!(path.contains(&"c".to_string()));
            }
            other => panic!("expected cycle, got {other:?}"),
        }
    }

    #[test]
    fn no_cycle_in_dag() {
        // happy: diamond is acyclic.
        let top = fix("top", "top", Scope::Function).with_deps(vec!["l".into(), "r".into()]);
        let l = fix("l", "l", Scope::Function).with_deps(vec!["base".into()]);
        let r = fix("r", "r", Scope::Function).with_deps(vec!["base".into()]);
        let base = fix("base", "base", Scope::Function);
        let fixtures = vec![top, l, r, base];
        let t = table(&fixtures);
        let g = FixtureGraph::build(fixtures, &t).expect("acyclic builds");
        g.detect_cycles().expect("no cycle");
    }

    #[test]
    fn scope_widen_violation() {
        // error: session fixture depends on a function fixture.
        let session = fix("s", "s", Scope::Session).with_deps(vec!["f".into()]);
        let function = fix("f", "f", Scope::Function);
        let fixtures = vec![session, function];
        let t = table(&fixtures);
        let err = FixtureGraph::build(fixtures, &t).unwrap_err();
        match err {
            FixtureError::ScopeWiden { narrow, wide } => {
                assert_eq!(narrow, Scope::Function);
                assert_eq!(wide, Scope::Session);
            }
            other => panic!("expected ScopeWiden, got {other:?}"),
        }
    }

    #[test]
    fn scope_equal_or_wider_dep_is_legal() {
        // boundary: function depends on session (legal — narrower depends on wider).
        let function = fix("f", "f", Scope::Function).with_deps(vec!["s".into()]);
        let session = fix("s", "s", Scope::Session);
        let fixtures = vec![function, session];
        let t = table(&fixtures);
        FixtureGraph::build(fixtures, &t).expect("narrower-depends-on-wider is legal");
    }

    #[test]
    fn closure_includes_transitive_and_autouse() {
        // happy + autouse injection.
        let order_fix = fix("order", "order", Scope::Function).with_deps(vec!["seeded".into()]);
        let seeded = fix("seeded", "seeded", Scope::Module).with_deps(vec!["db".into()]);
        let db = fix("db", "db", Scope::Session);
        let auto = fix("auto", "auto", Scope::Session).autouse();
        let fixtures = vec![order_fix, seeded, db, auto];
        let t = table(&fixtures);
        let g = FixtureGraph::build(fixtures, &t).expect("builds");

        let closure = g
            .closure_of(&["order".to_string()], &ScopePath::module("test_x.py"))
            .expect("closure");
        let ids: Vec<&str> = closure.setup_order().iter().map(|n| n.as_str()).collect();
        assert!(ids.contains(&"order"));
        assert!(ids.contains(&"seeded"));
        assert!(ids.contains(&"db"));
        assert!(ids.contains(&"auto"), "autouse injected: {ids:?}");
        // db before seeded before order (topo).
        let p = |s: &str| ids.iter().position(|x| *x == s).unwrap();
        assert!(p("db") < p("seeded"));
        assert!(p("seeded") < p("order"));
    }

    #[test]
    fn closure_empty_when_nothing_requested() {
        // empty.
        let db = fix("db", "db", Scope::Session);
        let fixtures = vec![db];
        let t = table(&fixtures);
        let g = FixtureGraph::build(fixtures, &t).expect("builds");
        let closure = g
            .closure_of(&[], &ScopePath::module("m.py"))
            .expect("closure");
        assert!(closure.is_empty());
    }

    #[test]
    fn closure_unresolved_requested_errors() {
        // error.
        let g = FixtureGraph::build(vec![], &OverrideTable::new()).expect("builds");
        let err = g
            .closure_of(&["nope".to_string()], &ScopePath::module("m.py"))
            .unwrap_err();
        assert!(matches!(err, FixtureError::Unresolved { .. }));
    }

    #[test]
    fn autouse_only_injected_in_scope() {
        // ordering / locality: an autouse fixture declared in pkg_a does not reach pkg_b tests.
        let auto = fix_at("auto", "auto", Scope::Module, "pkg_a").autouse();
        let fixtures = vec![auto];
        let t = table(&fixtures);
        let g = FixtureGraph::build(fixtures, &t).expect("builds");

        let in_a = g
            .closure_of(&[], &ScopePath::module("pkg_a/test.py"))
            .expect("closure a");
        assert_eq!(in_a.setup_order().len(), 1);

        let in_b = g
            .closure_of(&[], &ScopePath::module("pkg_b/test.py"))
            .expect("closure b");
        assert!(in_b.is_empty(), "autouse must not leak across packages");
    }

    #[test]
    fn diamond_closure_dedupes_shared_base() {
        // boundary: shared base appears once.
        let top = fix("top", "top", Scope::Function).with_deps(vec!["l".into(), "r".into()]);
        let l = fix("l", "l", Scope::Function).with_deps(vec!["base".into()]);
        let r = fix("r", "r", Scope::Function).with_deps(vec!["base".into()]);
        let base = fix("base", "base", Scope::Function);
        let fixtures = vec![top, l, r, base];
        let t = table(&fixtures);
        let g = FixtureGraph::build(fixtures, &t).expect("builds");
        let closure = g
            .closure_of(&["top".to_string()], &ScopePath::module("m.py"))
            .expect("closure");
        let base_count = closure
            .setup_order()
            .iter()
            .filter(|n| n.as_str() == "base")
            .count();
        assert_eq!(base_count, 1, "shared base deduped");
    }
}
