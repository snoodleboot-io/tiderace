//! Phase 5 integration — coverage → DepGraph → {CacheKey, ImpactAnalyzer} compose into the ADR-E004
//! preference order **cache hit → impact-skip → run**, using only the public engine-core API.
//!
//! This is the seam the Phase-6 daemon will drive on its live run loop; here we exercise it with
//! synthetic coverage so the policy is proven without a venv/fork.

use std::collections::BTreeMap;

use engine_core::cache::{
    Cache, CacheKeyBuilder, CachedOutcome, LocalCache, NoSandbox, Purity, SandboxHooks,
};
use engine_core::coverage::{CoverageReport, DepGraph};
use engine_core::domain::{NodeId, Outcome};
use engine_core::impact::{Change, ImpactAnalyzer};

/// A test's synthetic coverage: it touched `src.py` lines 1-2.
fn coverage(node: &str) -> CoverageReport {
    let mut wire = BTreeMap::new();
    wire.insert("src.py".to_string(), vec![1, 2]);
    CoverageReport::from_wire(NodeId::new(node), wire)
}

/// Build the cache key for a test from its coverage footprint + a source-content hash (the
/// `executed_source_closure` term that makes the cache sound — ADR-E004/E006).
fn key_for(graph: &DepGraph, node: &NodeId, src_hash: [u8; 32]) -> engine_core::cache::CacheKey {
    let mut b = CacheKeyBuilder::new(node.as_str(), "0.5.0", "3.12.3", "linux-x86_64");
    for fl in graph.deps_of(node) {
        b.executed_source(fl.source_path(), src_hash);
    }
    b.finish()
}

#[test]
fn warm_run_is_a_cache_hit_then_an_edit_invalidates_only_the_impacted_test() {
    // --- cold run: two tests execute, coverage recorded, outcomes cached ---
    let mut graph = DepGraph::new();
    let cache = LocalCache::new();
    let sandbox = NoSandbox;

    let a = NodeId::new("test_m.py::test_a");
    let b = NodeId::new("test_m.py::test_b");
    let src_v1 = [1u8; 32]; // content hash of src.py at version 1

    for node in [&a, &b] {
        graph.record(coverage(node.as_str()));
        let key = key_for(&graph, node, src_v1);
        assert!(cache.get(&key).is_none(), "cold run misses");
        // ...the test runs (synthetically: passed)...
        if sandbox.verdict(node.as_str()).is_cacheable() {
            cache.put(&key, CachedOutcome::new(Outcome::Passed, ""));
        }
    }
    assert_eq!(cache.len(), 2);

    // --- warm run, no changes: every test is a CACHE HIT (preference order tier 1) ---
    for node in [&a, &b] {
        let key = key_for(&graph, node, src_v1);
        assert_eq!(
            cache.get(&key).unwrap().outcome(),
            Outcome::Passed,
            "warm run hits cache"
        );
    }

    // --- warm run, no changes, via IMPACT analysis (tier 2): nothing selected ---
    let candidates = vec![a.clone(), b.clone()];
    let sel = ImpactAnalyzer::new(&graph).select(&[], &candidates);
    assert_eq!(
        sel.selected_count(),
        0,
        "no changes ⇒ impact-skip everything"
    );

    // --- edit src.py line 1: impact selects both (both touched it); but content-hash change means
    //     the cache key MISSES, so they genuinely re-run (cache stays sound across content edits) ---
    let sel = ImpactAnalyzer::new(&graph).select(&[Change::lines("src.py", [1])], &candidates);
    assert_eq!(sel.selected_count(), 2, "both tests touch the edited line");
    let src_v2 = [2u8; 32]; // src.py content changed ⇒ new hash
    let key_v2 = key_for(&graph, &a, src_v2);
    assert!(
        cache.get(&key_v2).is_none(),
        "changed source content ⇒ cache miss ⇒ real re-run"
    );
}

#[test]
fn impure_test_is_never_cached() {
    let mut graph = DepGraph::new();
    let cache = LocalCache::new();
    let node = NodeId::new("test_m.py::test_clock");
    graph.record(coverage(node.as_str()));
    let key = key_for(&graph, &node, [3u8; 32]);

    // The sandbox flags this test impure (e.g. it read the wall clock) → it must not be cached.
    let verdict = Purity::impure("read wall clock");
    if verdict.is_cacheable() {
        cache.put(&key, CachedOutcome::new(Outcome::Passed, ""));
    }
    assert!(
        cache.get(&key).is_none(),
        "impure outcomes are never silently cached (ADR-E004)"
    );
}
