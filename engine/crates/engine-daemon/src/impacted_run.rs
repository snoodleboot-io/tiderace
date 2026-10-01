//! The impact-aware run: re-run only the tests whose dependencies changed since last run, serve
//! the rest from the persisted record or the content-addressed cache, and persist the updated
//! footprint. With no changes, nothing executes — not even a wellspring launch.

use std::collections::HashSet;

use engine_core::cache::{Cache, CachedOutcome};
use engine_core::domain::{NodeId, TestResult};
use engine_core::runner::Learned;

use crate::error::Result;
use crate::state::fold::{disturbers, recorded_durations, RunScope};
use crate::state::plan::{changed_files, plan, PersistedState, TestRecord, STATE_FILE};
use crate::{EngineHandler, ImpactSummary};

impl EngineHandler {
    /// **Impact-aware run** (the warm-mode gap): load persisted state, re-run only the tests whose
    /// dependencies changed since last run (or have never run), serve the rest from cache, and persist
    /// the updated footprint. With no changes, nothing executes — not even a wellspring launch. Needs
    /// coverage capture on (the daemon sets `TIDERACE_COVERAGE=1`) so footprints are recorded.
    pub fn run_impacted(&mut self) -> Result<ImpactSummary> {
        let candidates: Vec<String> = self
            .collect()?
            .iter()
            .map(|i| i.node_id.to_string())
            .collect();
        let state_path = self.root.join(STATE_FILE);
        let mut state = PersistedState::load(&state_path);

        let current = self.hash_known_files(&state);
        let changed = changed_files(&state, &current);
        let p = plan(&state, &candidates, &changed);

        // impact-skip: serve locally-unchanged tests from the persisted record (no execution). A
        // `deselected` record is a verdict with nothing to serve — the node is absent from the
        // tally, as it is in pytest (TID-73) — so it is neither a result nor a cached count.
        let mut results: Vec<TestResult> = p
            .cached
            .iter()
            .filter_map(|node| {
                let rec = state.tests.get(node)?;
                let outcome = rec.outcome.ran()?;
                Some(TestResult::new(NodeId::new(node), outcome, 0, &rec.detail))
            })
            .collect();
        let cached_count = results.len();

        // Preference order (ADR-E004): **cache hit → impact-skip → run**. impact-skip handled the
        // locally-unchanged set above; for the impacted set, consult the content-addressed cache before
        // executing — a test whose exact inputs were already computed elsewhere (e.g. CI populated the
        // shared `TIDERACE_CACHE_DIR`) is served without running, even though this machine's local state
        // was stale.
        let mut executed = 0usize;
        let mut cache_served = 0usize;
        if !p.to_run.is_empty() {
            let py_ver = self.python_version();

            let mut hits: Vec<(String, CachedOutcome, Vec<String>)> = Vec::new();
            let mut to_execute: Vec<String> = Vec::new();
            for node in &p.to_run {
                let served = self.cache.as_ref().and_then(|cache| {
                    let deps = state.tests.get(node)?.deps.clone();
                    let key = self.cache_key(node, &deps, &py_ver)?;
                    cache.get(&key).map(|o| (o, deps))
                });
                match served {
                    Some((outcome, deps)) => hits.push((node.clone(), outcome, deps)),
                    None => to_execute.push(node.clone()),
                }
            }
            cache_served = hits.len();

            // Serve cache hits and refresh their local record so impact-skip serves them next time too.
            // (Only pure outcomes are ever cached — see the `put` below — so `pure: Some(true)` holds.)
            for (node, outcome, deps) in hits {
                results.push(TestResult::new(
                    NodeId::new(&node),
                    outcome.outcome(),
                    0,
                    outcome.detail(),
                ));
                let was_disturber = state.tests.get(&node).is_some_and(|p| p.must_fork);
                state.tests.insert(
                    node,
                    TestRecord {
                        pure: Some(true),
                        // A cache hit re-serves a previously *pure* result; it says nothing new
                        // about state disturbance, so preserve whatever was recorded.
                        must_fork: was_disturber,
                        ..TestRecord::ran(outcome.outcome(), outcome.detail(), deps)
                    },
                );
            }

            // Run the cache misses (stale purity ⇒ no trusted-pure; restore re-measures the verdict).
            if !to_execute.is_empty() {
                executed = to_execute.len();
                // Stale purity ⇒ nothing trusted pure here; restore re-measures the verdict.
                let learned = Learned {
                    trusted_pure: HashSet::new(),
                    must_fork: disturbers(&state),
                    durations: recorded_durations(&state),
                };
                let fresh = self.run_items_parallel(&to_execute, &learned, None, false, None)?;
                for r in &fresh {
                    results.push(r.clone());
                }
                self.persist_results(&mut state, &to_execute, &fresh, RunScope::Whole);
                // Populate the shared cache with fresh **pure** outcomes (impure is never cached —
                // ADR-E004 soundness). The key is the executed-source closure from this run's coverage.
                if let Some(cache) = &self.cache {
                    for r in &fresh {
                        if r.pure == Some(true) {
                            if let Some(key) =
                                self.cache_key(&r.node_id.to_string(), &r.touched_files, &py_ver)
                            {
                                cache.put(&key, CachedOutcome::new(r.outcome, r.detail.clone()));
                            }
                        }
                    }
                }
            } else {
                self.rebaseline_hashes(&mut state); // cache-hit-only: keep file hashes current
            }
            state.save(&state_path)?;
        }

        Ok(ImpactSummary {
            results,
            ran: executed,
            cached: cached_count + cache_served,
        })
    }
}
