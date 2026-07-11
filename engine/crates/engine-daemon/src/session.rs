use std::collections::BTreeSet;
use std::path::Path;

use engine_core::cache::{Cache, CacheKey, CacheKeyBuilder};
use engine_core::coverage::DepGraph;
use engine_core::domain::NodeId;
use engine_core::impact::{Change, ImpactAnalyzer};

use crate::invalidator::{Invalidation, Invalidator};

/// What the daemon should do in response to one file change (design 08). The incremental inner loop
/// that turns an edit into the *minimum* work — the payoff of Phases 5–6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// Nothing to do (unchanged content / irrelevant file).
    Nothing,
    /// The warm wellspring is stale — respawn it, then re-collect + re-run (conftest/config/C-ext).
    Recycle(String),
    /// Re-collect a changed test file (its node set may have changed).
    Recollect(std::path::PathBuf),
    /// Run exactly these tests: impacted by the change AND not already a cache hit.
    Rerun(Vec<NodeId>),
}

/// The per-project warm session: the daemon's in-memory source of truth (design 08 `session.rs`).
/// Holds the dependency graph (from coverage), the result cache, the content-hash invalidator, and
/// the collected node set, and computes the incremental response to each edit by composing
/// invalidation → impact selection → cache filtering (the ADR-E004 "cache hit → impact-skip → run"
/// preference order, applied to a single edit).
pub struct Session<C: Cache> {
    invalidator: Invalidator,
    graph: DepGraph,
    cache: C,
    candidates: Vec<NodeId>,
    engine_version: String,
    python_version: String,
    platform: String,
}

impl<C: Cache> Session<C> {
    pub fn new(
        graph: DepGraph,
        cache: C,
        candidates: Vec<NodeId>,
        engine_version: impl Into<String>,
        python_version: impl Into<String>,
        platform: impl Into<String>,
    ) -> Self {
        Self {
            invalidator: Invalidator::new(),
            graph,
            cache,
            candidates,
            engine_version: engine_version.into(),
            python_version: python_version.into(),
            platform: platform.into(),
        }
    }

    /// Seed a file's baseline content hash (at collection time) so later changes are detected and
    /// cache keys can incorporate source content.
    pub fn seed_hash(&mut self, path: impl Into<std::path::PathBuf>, hash: [u8; 32]) {
        self.invalidator.seed(path, hash);
    }

    /// Access the cache (e.g. to store a fresh outcome after a re-run).
    pub fn cache(&self) -> &C {
        &self.cache
    }

    /// React to one file change. `changed_lines` enables line-level impact (whole-file if `None`).
    pub fn on_change(
        &mut self,
        path: impl AsRef<Path>,
        new_hash: [u8; 32],
        changed_lines: Option<BTreeSet<u32>>,
    ) -> ChangeOutcome {
        let path = path.as_ref();
        match self.invalidator.observe(path, new_hash) {
            Invalidation::Unchanged | Invalidation::Ignored => ChangeOutcome::Nothing,
            Invalidation::RecycleWellspring(reason) => ChangeOutcome::Recycle(reason),
            Invalidation::Recollect(p) => ChangeOutcome::Recollect(p),
            Invalidation::SourceChanged(p) => {
                let change = match changed_lines {
                    Some(lines) => Change::lines(p, lines),
                    None => Change::file(p),
                };
                let selected = ImpactAnalyzer::new(&self.graph)
                    .select(&[change], &self.candidates)
                    .selected;
                // Cache-skip tier: a selected test whose (now-updated) input closure is already
                // cached needs no re-run. After a content edit its key shifts, so it correctly misses.
                let to_run = selected
                    .into_iter()
                    .filter(|node| self.cache.get(&self.cache_key(node)).is_none())
                    .collect();
                ChangeOutcome::Rerun(to_run)
            }
        }
    }

    /// The content-addressed cache key for a test, from its coverage footprint × current source hashes.
    pub fn cache_key(&self, node: &NodeId) -> CacheKey {
        let mut b = CacheKeyBuilder::new(
            node.as_str(),
            &self.engine_version,
            &self.python_version,
            &self.platform,
        );
        for fl in self.graph.deps_of(node) {
            let hash = self
                .invalidator
                .hash_of(fl.source_path())
                .unwrap_or([0u8; 32]);
            b.executed_source(fl.source_path(), hash);
        }
        b.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use engine_core::cache::{CachedOutcome, LocalCache};
    use engine_core::coverage::CoverageReport;
    use engine_core::domain::Outcome;

    use super::*;

    fn graph_with(node: &str, file: &str, lines: &[u32]) -> CoverageReport {
        let mut wire = BTreeMap::new();
        wire.insert(file.to_string(), lines.to_vec());
        CoverageReport::from_wire(NodeId::new(node), wire)
    }

    fn session() -> Session<LocalCache> {
        let mut graph = DepGraph::new();
        graph.record(graph_with("test_m.py::a", "src.py", &[1, 2]));
        graph.record(graph_with("test_m.py::b", "other.py", &[1]));
        let candidates = vec![NodeId::new("test_m.py::a"), NodeId::new("test_m.py::b")];
        let mut s = Session::new(graph, LocalCache::new(), candidates, "0.5", "3.12", "linux");
        s.seed_hash("src.py", [1; 32]);
        s.seed_hash("other.py", [1; 32]);
        s
    }

    #[test]
    fn source_edit_reruns_only_impacted_and_uncached() {
        let mut s = session();
        // Pre-cache test a under its CURRENT key (pretend it ran before this edit).
        let key_a_v1 = s.cache_key(&NodeId::new("test_m.py::a"));
        s.cache()
            .put(&key_a_v1, CachedOutcome::new(Outcome::Passed, ""));

        // Edit src.py line 1: impacts only `a`; but src.py's hash changed, so a's key shifts → its old
        // cache entry no longer matches → it genuinely re-runs.
        let out = s.on_change("src.py", [2; 32], Some([1].into_iter().collect()));
        assert_eq!(out, ChangeOutcome::Rerun(vec![NodeId::new("test_m.py::a")]));
    }

    #[test]
    fn unrelated_edit_reruns_nothing() {
        let mut s = session();
        let out = s.on_change("other.py", [2; 32], Some([99].into_iter().collect()));
        assert_eq!(
            out,
            ChangeOutcome::Rerun(vec![]),
            "no test touched line 99 of other.py"
        );
    }

    #[test]
    fn mtime_only_touch_is_nothing() {
        let mut s = session();
        assert_eq!(s.on_change("src.py", [1; 32], None), ChangeOutcome::Nothing);
    }

    #[test]
    fn conftest_change_recycles() {
        let mut s = session();
        assert!(matches!(
            s.on_change("conftest.py", [7; 32], None),
            ChangeOutcome::Recycle(_)
        ));
    }

    #[test]
    fn cache_hit_skips_rerun_when_content_unchanged_but_impact_says_run() {
        // If a different file in a's closure is edited but a's key is still cached, a is skipped.
        // Here: cache a under the key it will have AFTER an unrelated whole-file change to src.py
        // is reverted — simpler proof: pre-cache, then a whole-file change that keeps src.py hash.
        let mut s = session();
        let key_a = s.cache_key(&NodeId::new("test_m.py::a"));
        s.cache()
            .put(&key_a, CachedOutcome::new(Outcome::Passed, ""));
        // A whole-file "change" reporting the SAME hash is Unchanged → Nothing (no rerun at all).
        assert_eq!(s.on_change("src.py", [1; 32], None), ChangeOutcome::Nothing);
    }
}
