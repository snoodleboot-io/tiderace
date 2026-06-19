//! `OverrideTable` (W6) — conftest-like override-by-location resolution.
//!
//! Fixture definitions are indexed by `(name, ScopePath)`; resolving a name for a test at path `P`
//! picks the definition whose `ScopePath` is the **longest prefix of `P`** (nearest wins) — a module
//! fixture shadows a session `conftest` fixture of the same name for tests in that module only
//! (design 04 §1.4).
//!
//! **Package tie-break (PROPOSED, CONTRACT §5 / design 04 §F3).** When two sibling locations define
//! the same name, the definition whose declaring location is the **longest path-segment prefix** of
//! the requesting location wins. This is the existing nearest-override mechanism extended to the
//! package layer — no new machinery. It is flagged in CONTRACT §5 as awaiting human ratification; if
//! a different rule is chosen, only this method changes (no frozen shape moves).
//!
//! **Implemented by** Lane FX-graph (subagent fx-model): W1/W6.

use std::collections::HashMap;

use crate::domain::{NodeId, ScopePath};

/// Indexes fixture definitions by `(name, ScopePath)` for nearest-override resolution.
#[derive(Debug, Default)]
pub struct OverrideTable {
    /// `(name, scope_path)` → defining fixture node id. Owned by the table; populated at build time.
    entries: HashMap<(String, String), NodeId>,
}

impl OverrideTable {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Register a definition of `name` declared at `at`.
    ///
    /// Defined here (pure data insertion) so Lane FX-graph builds tables without a scaffold; the
    /// keying mirrors the resolution key used by [`Self::nearest`].
    pub fn insert(&mut self, name: impl Into<String>, at: &ScopePath, node: NodeId) {
        self.entries.insert((name.into(), at.module.clone()), node);
    }

    /// Resolve `name` for a test at `from`: the definition whose location is the **longest prefix**
    /// of `from` (nearest wins). `None` if no visible definition.
    ///
    /// Prefix is measured at **path-segment** granularity over the slashed/dotted module identifier
    /// (so `pkg` is a prefix of `pkg/sub/test.py` but `pk` is not — avoiding spurious substring
    /// matches). A definition at the same module as `from` is the longest possible prefix and always
    /// wins; the session root (declared at `""`) is the shortest prefix and is the fallback.
    ///
    /// Ties on prefix length cannot occur here: two distinct keys with equal segment-prefix length
    /// that are both prefixes of `from` would have to be identical, so the table (a map) holds one.
    pub fn nearest(&self, name: &str, from: &ScopePath) -> Option<NodeId> {
        let target = Self::segments(&from.module);

        let mut best: Option<(usize, &NodeId)> = None;
        for ((entry_name, entry_module), node) in &self.entries {
            if entry_name != name {
                continue;
            }
            let candidate = Self::segments(entry_module);
            let Some(len) = Self::prefix_len(&candidate, &target) else {
                continue;
            };
            // Longer prefix = nearer definition. Strictly-greater keeps the first equal-length match
            // stable, though equal lengths that both match imply identical keys (see doc above).
            if best.is_none_or(|(best_len, _)| len > best_len) {
                best = Some((len, node));
            }
        }
        best.map(|(_, node)| node.clone())
    }

    /// Split a module identifier into path segments, treating `/` and `.` as separators and dropping
    /// empty segments (so the session root `""` yields zero segments — a prefix of everything).
    fn segments(module: &str) -> Vec<&str> {
        module.split(['/', '.']).filter(|s| !s.is_empty()).collect()
    }

    /// If `candidate` is a (possibly equal) segment-wise prefix of `target`, return its length in
    /// segments; otherwise `None`. The empty candidate (session root) is a prefix of everything.
    fn prefix_len(candidate: &[&str], target: &[&str]) -> Option<usize> {
        if candidate.len() > target.len() {
            return None;
        }
        if candidate.iter().zip(target).all(|(c, t)| c == t) {
            Some(candidate.len())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(s: &str) -> NodeId {
        NodeId::new(s)
    }

    #[test]
    fn nearest_picks_longest_prefix() {
        // happy: session root + module override; module wins for a test in that module.
        let mut t = OverrideTable::new();
        t.insert("client", &ScopePath::module(""), node("root::client"));
        t.insert(
            "client",
            &ScopePath::module("pkg/test_api.py"),
            node("mod::client"),
        );

        let from = ScopePath::module("pkg/test_api.py");
        assert_eq!(t.nearest("client", &from), Some(node("mod::client")));
    }

    #[test]
    fn nearest_falls_back_to_session_root() {
        // boundary: only a root definition exists → resolves to it from any location.
        let mut t = OverrideTable::new();
        t.insert("db", &ScopePath::module(""), node("root::db"));

        let from = ScopePath::module("pkg/sub/test_x.py");
        assert_eq!(t.nearest("db", &from), Some(node("root::db")));
    }

    #[test]
    fn nearest_package_tie_break_longest_prefix() {
        // ordering / package tie-break: a package conftest shadows the session root for tests inside
        // that package, but not for tests in a sibling package.
        let mut t = OverrideTable::new();
        t.insert("client", &ScopePath::module(""), node("root::client"));
        t.insert("client", &ScopePath::module("pkg_a"), node("pkg_a::client"));

        let inside_a = ScopePath::module("pkg_a/test_x.py");
        assert_eq!(t.nearest("client", &inside_a), Some(node("pkg_a::client")));

        let inside_b = ScopePath::module("pkg_b/test_y.py");
        assert_eq!(t.nearest("client", &inside_b), Some(node("root::client")));
    }

    #[test]
    fn nearest_rejects_non_prefix_sibling() {
        // adversarial: a sibling module's definition is NOT a prefix and must not leak across.
        let mut t = OverrideTable::new();
        t.insert("x", &ScopePath::module("pkg/test_a.py"), node("a::x"));

        let from = ScopePath::module("pkg/test_b.py");
        assert_eq!(t.nearest("x", &from), None);
    }

    #[test]
    fn nearest_no_substring_false_positive() {
        // adversarial: segment-wise prefix, not raw substring — `pk` must not match `pkg/...`.
        let mut t = OverrideTable::new();
        t.insert("x", &ScopePath::module("pk"), node("pk::x"));

        let from = ScopePath::module("pkg/test.py");
        assert_eq!(t.nearest("x", &from), None);
    }

    #[test]
    fn nearest_empty_table_is_none() {
        // empty: nothing registered → unresolved.
        let t = OverrideTable::new();
        assert_eq!(t.nearest("missing", &ScopePath::module("m.py")), None);
    }

    #[test]
    fn nearest_unknown_name_is_none() {
        // error-ish: a known location but the requested name was never registered.
        let mut t = OverrideTable::new();
        t.insert("a", &ScopePath::module(""), node("root::a"));
        assert_eq!(t.nearest("b", &ScopePath::module("m.py")), None);
    }
}
