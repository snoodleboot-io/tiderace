use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use engine_core::cache::{Cache, CacheKey, CacheKeyBuilder, CachedOutcome, DirCache};
use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestItem, TestResult};
use engine_core::exec::{ForkWorker, SubInterpWorker, Worker};

use crate::persist::{changed_files, plan, PersistedState, TestRecord, STATE_FILE};
use crate::rpc_method::{RpcRequest, RpcResponse, RpcResult};
use crate::rpc_server::RpcHandler;
use crate::watch::content_hash;
use engine_core::exec::SafeSetCache;

/// Summary of an impact-aware run: which tests actually executed vs. were served from warm state.
#[derive(Debug)]
pub struct ImpactSummary {
    pub results: Vec<RpcResult>,
    pub ran: usize,
    pub cached: usize,
}

/// The live [`RpcHandler`]: turns RPC requests into real engine work over a **warm** wellspring
/// (design 08, ADR-E007). The `ForkWorker` is launched lazily on the first `Run` and **reused** across
/// requests, so the second run in a session pays no interpreter/import cost — the daemon's whole point.
pub struct EngineHandler {
    python: String,
    shim: PathBuf,
    root: PathBuf,
    worker: Option<ForkWorker>, // warm wellspring, kept alive across Run requests
    /// Content-addressed result cache (ADR-E004, TID-7). Enabled by `TIDERACE_CACHE_DIR` pointing at a
    /// directory (a CI cache path / shared mount), which makes a result computed on one machine a free
    /// hit on any other with the same inputs. `None` ⇒ cache off (impact-skip only).
    cache: Option<DirCache>,
}

impl EngineHandler {
    pub fn new(
        python: impl Into<String>,
        shim: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
    ) -> Self {
        let cache = std::env::var("TIDERACE_CACHE_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(DirCache::new);
        Self {
            python: python.into(),
            shim: shim.into(),
            root: root.into(),
            worker: None,
            cache,
        }
    }

    fn collect(&self) -> Result<Vec<TestItem>, String> {
        RegexCollector::new()
            .collect(&self.root)
            .map_err(|e| format!("collection failed: {e}"))
    }

    /// Launch the wellspring once; reuse it thereafter (warm). Runs tests no-fork + restore by default
    /// (the shim forks non-restorable modules for soundness) — the warm RPC `Run` path gets the same
    /// fast execution as the one-shot pool.
    fn worker(&mut self) -> Result<&mut ForkWorker, String> {
        if self.worker.is_none() {
            let w = ForkWorker::launch(&self.python, &self.shim, &self.root)
                .map_err(|e| format!("failed to launch wellspring: {e}"))?
                .with_optimistic_no_fork(optimistic_no_fork());
            self.worker = Some(w);
        }
        Ok(self.worker.as_mut().expect("just launched"))
    }

    /// Run the requested tests (empty ⇒ all), returning full `TestResult`s (with the touched-file
    /// footprint coverage captured, used by impact-aware re-runs).
    fn run_items(&mut self, requested: &[String]) -> Result<Vec<TestResult>, String> {
        let all = self.collect()?;
        let items: Vec<TestItem> = if requested.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|it| requested.iter().any(|r| r == it.node_id.as_str()))
                .collect()
        };
        self.worker()?
            .run(&items)
            .map_err(|e| format!("execution failed: {e}"))
    }

    fn run(&mut self, requested: &[String]) -> Result<Vec<RpcResult>, String> {
        Ok(self.run_items(requested)?.into_iter().map(to_rpc).collect())
    }

    /// Run the requested tests across a **parallel pool** of wellsprings (one per core), not the single
    /// warm wellspring — the fix for sequential full runs. Tests run no-fork + restore by default; the
    /// shim forks non-restorable (opaque) modules for soundness. `trusted` node ids (recorded pure +
    /// unchanged, TID-1) run BARE no-fork, skipping the snapshot. That is ~90× cheaper per test only in
    /// the trivial-test microbenchmark; measured on real corpora it is ~3.4× where it applies, and on a
    /// suite built with module-level test doubles it applies to no test at all (TID-41).
    fn run_items_parallel(
        &self,
        requested: &[String],
        trusted: &HashSet<String>,
        must_fork: &HashSet<String>,
        durations: &HashMap<String, u64>,
    ) -> Result<Vec<TestResult>, String> {
        let all = self.collect()?;
        let items: Vec<TestItem> = if requested.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|it| requested.iter().any(|r| r == it.node_id.as_str()))
                .collect()
        };
        crate::pool::run_parallel(
            &self.python,
            &self.shim,
            &self.root,
            items,
            crate::pool::default_workers(),
            5000,
            optimistic_no_fork(), // no-fork + restore by default (TIDERACE_FORCE_FORK=1 to disable)
            trusted,
            must_fork, // TID-33: recorded state-disturbers skip the in-process ladder entirely
            durations, // TID-62: what each node cost last time, so the heaviest module goes first
        )
    }

    /// Full run across the parallel pool (the one-shot `run --all` path). Now **purity-aware** (TID-1):
    /// it loads the persisted state, runs *recorded-pure + unchanged* tests BARE no-fork (skip the
    /// snapshot), re-verifies the rest under restore, and persists the updated verdicts + footprints.
    /// So the second `run --all` on an unchanged tree runs the pure suite at the bare-no-fork tier.
    pub fn run_full_parallel(&self) -> Result<Vec<RpcResult>, String> {
        let state_path = self.root.join(STATE_FILE);
        let mut state = PersistedState::load(&state_path);

        // Trusted = recorded pure AND none of its recorded deps changed since it was last verified.
        let current = self.hash_known_files(&state);
        let changed = changed_files(&state, &current);
        let trusted: HashSet<String> = state
            .tests
            .iter()
            .filter(|(_, rec)| {
                rec.pure == Some(true) && !rec.deps.iter().any(|d| changed.contains(d))
            })
            .map(|(node, _)| node.clone())
            .collect();
        // TID-33: recorded state-disturbers are forked from the start. Separate from `trusted` and
        // its inverse: most impure tests are impure in ways restore handles completely, and forking
        // those would cost the ladder nearly everything it buys.
        let must_fork: HashSet<String> = state
            .tests
            .iter()
            .filter(|(_, rec)| rec.must_fork)
            .map(|(node, _)| node.clone())
            .collect();
        let durations = recorded_durations(&state);

        // ADR-E015 / TID-11: with the sub-interpreter tier on (`TIDERACE_SUBINTERP=1`), route the
        // sub-interp-**safe** modules through a parallel sub-interpreter pool (no fork; sound because
        // both module globals and os.environ are per-interpreter) and everything else through the fork
        // pool. Sub-interpreters are the only parallelism Windows (no fork) has. Off ⇒ fork pool only.
        let fresh = if subinterp_enabled() {
            let items = self.collect()?;
            let modules: Vec<String> = {
                let mut m: Vec<String> = items.iter().map(|it| module_of(&it.node_id)).collect();
                m.sort();
                m.dedup();
                m
            };
            let safe = self.safe_set(&mut state, &modules)?;
            let (si_items, fork_items): (Vec<TestItem>, Vec<TestItem>) = items
                .into_iter()
                .partition(|it| safe.contains(&module_of(&it.node_id)));

            let mut fresh = Vec::new();
            if !si_items.is_empty() {
                let mut w = SubInterpWorker::new(5000)
                    .with_target(self.python.clone(), &self.shim, &self.root)
                    .with_pool_size(crate::pool::default_workers());
                fresh.extend(
                    w.run(&si_items)
                        .map_err(|e| format!("subinterp pool: {e}"))?,
                );
            }
            if !fork_items.is_empty() {
                let fork_nodes: Vec<String> =
                    fork_items.iter().map(|it| it.node_id.to_string()).collect();
                fresh.extend(self.run_items_parallel(
                    &fork_nodes,
                    &trusted,
                    &must_fork,
                    &durations,
                )?);
            }
            fresh
        } else {
            self.run_items_parallel(&[], &trusted, &must_fork, &durations)?
        };

        self.persist_results(&mut state, &fresh);
        state
            .save(&state_path)
            .map_err(|e| format!("state save failed: {e}"))?;
        Ok(fresh.into_iter().map(to_rpc).collect())
    }

    /// The sub-interpreter-safe module set for `modules` (ADR-E015 TID-9 cache + TID-11).
    ///
    /// The classification and content-hash invalidation live in `engine_core`'s [`SafeSetCache`], so
    /// the CLI gets the same behaviour instead of re-probing every run (TID-35). The daemon keeps
    /// *persisting* the verdicts in its own state file, which it already writes.
    fn safe_set(
        &self,
        state: &mut PersistedState,
        modules: &[String],
    ) -> Result<HashSet<String>, String> {
        let mut cache = SafeSetCache::from_entries(std::mem::take(&mut state.safe_modules));
        let safe = cache.resolve(&self.python, &self.shim, &self.root, modules);
        state.safe_modules = cache.into_entries();
        safe
    }

    /// Fold a batch of results into the persisted state (outcome + detail + deps + purity verdict) and
    /// rebaseline the content hashes of every touched file. Shared by the impact-aware + full runs.
    fn persist_results(&self, state: &mut PersistedState, results: &[TestResult]) {
        state.record_durations(results); // TID-62: the next run's scheduler weights
        for r in results {
            let prior = state.tests.get(r.node_id.as_str());
            state.tests.insert(
                r.node_id.to_string(),
                TestRecord {
                    outcome: outcome_token(r.outcome).to_string(),
                    detail: r.detail.clone(),
                    // An empty footprint means capture was off, not that the test depends on
                    // nothing — every test touches at least its own file. Overwriting a real
                    // footprint with "no data" would silently disarm the staleness guards that
                    // read it.
                    deps: if r.touched_files.is_empty() {
                        prior.map(|p| p.deps.clone()).unwrap_or_default()
                    } else {
                        r.touched_files.clone()
                    },
                    // Sticky for the same reason `must_fork` is, and missing it made the bare
                    // no-fork tier erase the verdict that grants it. `pure: None` means *this run
                    // did not measure* — because the test was forked, was async, or was trusted
                    // pure and therefore skipped the snapshot. In none of those did we learn the
                    // test became impure, so overwriting a recorded verdict with "unknown" throws
                    // away a fact for no reason.
                    //
                    // The effect was a perfect oscillation: run 1 measures pure, run 2 trusts it
                    // and goes bare (measuring nothing), run 3 finds no verdict and pays the full
                    // snapshot again. The tier could never apply twice in a row, so half its value
                    // was discarded. Measured on a snapshot-heavy corpus: 1.31s / 0.45s / 1.61s /
                    // 0.45s across four identical runs.
                    //
                    // Staleness is still handled where it belongs — `trusted` requires the
                    // recorded deps to be unchanged, and TID-40 made those footprints sound. A
                    // measured verdict (`Some`) always wins over the prior one.
                    pure: r.pure.or_else(|| prior.and_then(|p| p.pure)),
                    // Sticky: a forked re-run cannot observe the drift that earned the flag, so
                    // clearing it on a clean forked result would make the node oscillate between
                    // tiers forever. It clears when the test's own source changes.
                    must_fork: r.must_fork || prior.is_some_and(|p| p.must_fork),
                },
            );
        }
        self.rebaseline_hashes(state);
    }

    /// **Impact-aware run** (the warm-mode gap): load persisted state, re-run only the tests whose
    /// dependencies changed since last run (or have never run), serve the rest from cache, and persist
    /// the updated footprint. With no changes, nothing executes — not even a wellspring launch. Needs
    /// coverage capture on (the daemon sets `TIDERACE_COVERAGE=1`) so footprints are recorded.
    pub fn run_impacted(&mut self) -> Result<ImpactSummary, String> {
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
        let cached_count = p.cached.len();

        // impact-skip: serve locally-unchanged tests from the persisted record (no execution).
        let mut results: Vec<RpcResult> = p
            .cached
            .iter()
            .filter_map(|node| {
                state.tests.get(node).map(|rec| RpcResult {
                    node_id: node.clone(),
                    outcome: rec.outcome.clone(),
                    duration_ms: 0,
                })
            })
            .collect();

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
                results.push(RpcResult {
                    node_id: node.clone(),
                    outcome: outcome_token(outcome.outcome()).to_string(),
                    duration_ms: 0,
                });
                let was_disturber = state.tests.get(&node).is_some_and(|p| p.must_fork);
                state.tests.insert(
                    node,
                    TestRecord {
                        outcome: outcome_token(outcome.outcome()).to_string(),
                        detail: outcome.detail().to_string(),
                        deps,
                        pure: Some(true),
                        // A cache hit re-serves a previously *pure* result; it says nothing new
                        // about state disturbance, so preserve whatever was recorded.
                        must_fork: was_disturber,
                    },
                );
            }

            // Run the cache misses (stale purity ⇒ no trusted-pure; restore re-measures the verdict).
            if !to_execute.is_empty() {
                executed = to_execute.len();
                let disturbers: HashSet<String> = state
                    .tests
                    .iter()
                    .filter(|(_, rec)| rec.must_fork)
                    .map(|(node, _)| node.clone())
                    .collect();
                let durations = recorded_durations(&state);
                let fresh =
                    self.run_items_parallel(&to_execute, &HashSet::new(), &disturbers, &durations)?;
                for r in &fresh {
                    results.push(to_rpc(r.clone()));
                }
                self.persist_results(&mut state, &fresh);
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
            state
                .save(&state_path)
                .map_err(|e| format!("state save failed: {e}"))?;
        }

        Ok(ImpactSummary {
            results,
            ran: executed,
            cached: cached_count + cache_served,
        })
    }

    /// Current content hashes (hex) for every file already in the persisted state.
    fn hash_known_files(&self, state: &PersistedState) -> BTreeMap<String, String> {
        state
            .files
            .keys()
            .map(|rel| (rel.clone(), self.hash_file(rel)))
            .collect()
    }

    /// Set `state.files` to the current hash of every file any recorded test depends on.
    fn rebaseline_hashes(&self, state: &mut PersistedState) {
        let mut files = BTreeMap::new();
        for rec in state.tests.values() {
            for dep in &rec.deps {
                files
                    .entry(dep.clone())
                    .or_insert_with(|| self.hash_file(dep));
            }
        }
        state.files = files;
    }

    /// Hex content hash of `<root>/rel`; a sentinel for a missing/unreadable file (⇒ counts as changed).
    fn hash_file(&self, rel: &str) -> String {
        match std::fs::read(self.root.join(rel)) {
            Ok(bytes) => hex(&content_hash(&bytes)),
            Err(_) => "missing".to_string(),
        }
    }

    /// The platform term for the cache key — partitions the cache across OS/arch so a result never
    /// crosses platforms (ADR-E004 invalidation).
    fn platform() -> String {
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
    }

    /// The interpreter's version (e.g. `"3.12.4"`), a cache-key term so a result computed under one
    /// Python is never served under another. Best-effort — `"unknown"` on failure (still consistent
    /// within a machine, just coarser sharing). Queried once per `run_impacted` (a single subprocess).
    fn python_version(&self) -> String {
        std::process::Command::new(&self.python)
            .arg("--version")
            .output()
            .ok()
            .map(|o| {
                let out = if o.stdout.is_empty() {
                    o.stderr
                } else {
                    o.stdout
                };
                String::from_utf8_lossy(&out)
                    .split_whitespace()
                    .last()
                    .unwrap_or("unknown")
                    .to_string()
            })
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// The content-addressed [`CacheKey`] for `node` over `deps`' **current** content, or `None` if
    /// `deps` is empty (no recorded footprint ⇒ not soundly cacheable) or any dep is unreadable. Built
    /// the same way for `get` and `put`, so a hit ⟺ the executed-source closure is byte-identical to
    /// when the outcome was produced.
    fn cache_key(&self, node: &str, deps: &[String], py_version: &str) -> Option<CacheKey> {
        if deps.is_empty() {
            return None;
        }
        let mut b = CacheKeyBuilder::new(
            node,
            env!("CARGO_PKG_VERSION"),
            py_version,
            Self::platform(),
        );
        for dep in deps {
            let bytes = std::fs::read(self.root.join(dep)).ok()?; // unreadable dep ⇒ no sound key
            b.executed_source(dep.clone(), content_hash(&bytes));
        }
        Some(b.finish())
    }
}

/// No-fork + restore is the default. `TIDERACE_FORCE_FORK=1` reverts to fork-per-test — a debug/benchmark
/// escape only (not a user-facing flag), so the fork baseline stays measurable for regression checks.
fn optimistic_no_fork() -> bool {
    std::env::var("TIDERACE_FORCE_FORK").as_deref() != Ok("1")
}

/// Whether the sub-interpreter tier is enabled (ADR-E015 / TID-11). `TIDERACE_SUBINTERP=1` opts in —
/// safe modules then run through the parallel sub-interpreter pool. Its purpose is Windows parallelism.
fn subinterp_enabled() -> bool {
    std::env::var("TIDERACE_SUBINTERP").as_deref() == Ok("1")
}

/// The module rel-path of a node id (`pkg/test_x.py::C::t` -> `pkg/test_x.py`).
fn module_of(node: &engine_core::domain::NodeId) -> String {
    node.as_str().split("::").next().unwrap_or("").to_string()
}

fn to_rpc(r: TestResult) -> RpcResult {
    RpcResult {
        node_id: r.node_id.to_string(),
        outcome: outcome_token(r.outcome).to_string(),
        duration_ms: r.duration_ms,
    }
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl RpcHandler for EngineHandler {
    fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        match request {
            RpcRequest::Discover => match self.collect() {
                Ok(items) => RpcResponse::Discovered {
                    node_ids: items.iter().map(|i| i.node_id.to_string()).collect(),
                },
                Err(message) => RpcResponse::Error { message },
            },
            RpcRequest::Run { node_ids } => match self.run(&node_ids) {
                Ok(results) => RpcResponse::Ran { results },
                Err(message) => RpcResponse::Error { message },
            },
            RpcRequest::Recycle => {
                self.worker = None; // drop the stale warm interpreter; next Run relaunches it
                match self.run(&[]) {
                    Ok(results) => RpcResponse::Ran { results },
                    Err(message) => RpcResponse::Error { message },
                }
            }
            RpcRequest::Watch => RpcResponse::Watching,
            RpcRequest::Health => RpcResponse::Healthy {
                pid: self
                    .worker
                    .as_ref()
                    .map(ForkWorker::wellspring_pid)
                    .unwrap_or(-1),
                warm: self.worker.is_some(),
            },
            RpcRequest::Shutdown => RpcResponse::ShuttingDown,
        }
    }
}

fn outcome_token(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "passed",
        Outcome::Failed => "failed",
        Outcome::Skipped => "skipped",
        Outcome::XFail => "xfail",
        Outcome::XPass => "xpass",
        Outcome::Error => "error",
    }
}

/// The last run's per-node cost, as the scheduler's weights (TID-62). Kept as a `HashMap` for the
/// `RunPlan`; the state stores a `BTreeMap` so the file is stable across saves.
fn recorded_durations(state: &PersistedState) -> HashMap<String, u64> {
    state
        .durations
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::domain::{NodeId, Outcome};
    use engine_core::testing::skip_live;

    fn handler_for(dir: &std::path::Path) -> EngineHandler {
        EngineHandler::new("python3", PathBuf::from("shim.py"), dir.to_path_buf())
    }

    fn result(node: &str, pure: Option<bool>, deps: &[&str]) -> TestResult {
        TestResult::new(NodeId::new(node), Outcome::Passed, 1, "")
            .with_touched(deps.iter().map(|d| (*d).to_string()).collect())
            .with_pure(pure)
    }

    /// A run that did not measure purity must not erase the verdict a previous run recorded.
    ///
    /// `pure: None` means *this run did not measure* — the test was forked, was async, or was
    /// trusted pure and so skipped the snapshot. None of those learned that the test became impure.
    ///
    /// Missing this made the bare no-fork tier erase the very verdict that grants it, in a perfect
    /// oscillation: measure pure, trust it and go bare (measuring nothing), find no verdict, pay the
    /// full snapshot again. On a snapshot-heavy corpus that alternated 1.31s / 0.45s / 1.61s / 0.45s
    /// across four identical runs — the tier could never apply twice in a row.
    #[test]
    fn an_unmeasured_run_keeps_the_recorded_purity_verdict() {
        let dir = std::env::temp_dir().join(format!("tiderace_sticky_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let handler = handler_for(&dir);
        let mut state = PersistedState::default();

        handler.persist_results(&mut state, &[result("t.py::a", Some(true), &["t.py"])]);
        assert_eq!(state.tests["t.py::a"].pure, Some(true));

        // The bare run: nothing measured.
        handler.persist_results(&mut state, &[result("t.py::a", None, &["t.py"])]);
        assert_eq!(
            state.tests["t.py::a"].pure,
            Some(true),
            "an unmeasured run must not downgrade a recorded verdict to unknown"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A *measured* verdict always wins, in both directions — stickiness must not mean deafness.
    #[test]
    fn a_measured_verdict_overwrites_the_recorded_one() {
        let dir = std::env::temp_dir().join(format!("tiderace_measured_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let handler = handler_for(&dir);
        let mut state = PersistedState::default();

        handler.persist_results(&mut state, &[result("t.py::a", Some(true), &["t.py"])]);
        handler.persist_results(&mut state, &[result("t.py::a", Some(false), &["t.py"])]);
        assert_eq!(
            state.tests["t.py::a"].pure,
            Some(false),
            "a test measured impure must lose its pure verdict"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty footprint means capture was off, not that the test depends on nothing.
    ///
    /// Every test touches at least its own file, so an empty `deps` is missing data. Writing it over
    /// a real footprint would silently disarm the staleness guards that read it — including the one
    /// protecting the tier that skips isolation.
    #[test]
    fn an_empty_footprint_does_not_erase_a_recorded_one() {
        let dir = std::env::temp_dir().join(format!("tiderace_deps_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let handler = handler_for(&dir);
        let mut state = PersistedState::default();

        handler.persist_results(
            &mut state,
            &[result("t.py::a", Some(true), &["t.py", "src.py"])],
        );
        handler.persist_results(&mut state, &[result("t.py::a", None, &[])]);
        assert_eq!(
            state.tests["t.py::a"].deps,
            vec!["t.py".to_string(), "src.py".to_string()],
            "a run with capture off must not wipe the footprint"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("repo root")
    }

    /// Sub-interp safety needs CPython 3.14 (`concurrent.interpreters`) + numpy for the unsafe case —
    /// gate the `safe_set` test on the fx venv.
    fn fx_venv() -> Option<String> {
        let p = repo_root().join(".tiderace-fx-venv/bin/python");
        p.exists().then(|| p.to_string_lossy().into_owned())
    }

    #[test]
    fn safe_set_classifies_probes_once_and_caches() {
        let Some(python) = fx_venv() else {
            skip_live("`.tiderace-fx-venv` (CPython 3.14 + numpy) not present");
            return;
        };
        let dir = temp("safeset");
        std::fs::write(
            dir.join("test_pure.py"),
            "def test_a():\n    assert 1 == 1\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("test_np.py"),
            "import numpy\ndef test_n():\n    assert int(numpy.array([1]).sum()) == 1\n",
        )
        .unwrap();
        let handler = EngineHandler::new(
            python,
            repo_root().join("engine/py-shim/shim.py"),
            dir.clone(),
        );
        let mut state = PersistedState::default();
        let modules = vec!["test_pure.py".to_string(), "test_np.py".to_string()];

        let safe = handler.safe_set(&mut state, &modules).expect("safe_set");
        assert!(
            safe.contains("test_pure.py"),
            "pure module is sub-interp-safe"
        );
        assert!(!safe.contains("test_np.py"), "numpy module is not safe");
        assert_eq!(
            state.safe_modules.get("test_pure.py").map(|r| r.safe),
            Some(true)
        );
        assert_eq!(
            state.safe_modules.get("test_np.py").map(|r| r.safe),
            Some(false)
        );

        // Second call: verdicts are cached by content hash, so the result is stable (no re-probe needed).
        let hashes: Vec<String> = state
            .safe_modules
            .values()
            .map(|r| r.hash.clone())
            .collect();
        let safe2 = handler
            .safe_set(&mut state, &modules)
            .expect("safe_set cached");
        assert_eq!(safe, safe2);
        assert_eq!(
            hashes,
            state
                .safe_modules
                .values()
                .map(|r| r.hash.clone())
                .collect::<Vec<_>>(),
            "unchanged modules keep their cached verdict"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tiderace_ck_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn cache_key_is_deterministic_node_python_and_content_sensitive() {
        let dir = temp("sens");
        std::fs::write(dir.join("src.py"), b"x = 1").unwrap();
        let h = EngineHandler::new("python3", "shim.py", dir.clone());
        let deps = vec!["src.py".to_string()];

        let k = h.cache_key("t.py::a", &deps, "3.12").unwrap();
        assert_eq!(
            k,
            h.cache_key("t.py::a", &deps, "3.12").unwrap(),
            "same inputs → same key"
        );
        assert_ne!(
            k,
            h.cache_key("t.py::b", &deps, "3.12").unwrap(),
            "node partitions the key"
        );
        assert_ne!(
            k,
            h.cache_key("t.py::a", &deps, "3.13").unwrap(),
            "python version partitions the key"
        );

        std::fs::write(dir.join("src.py"), b"x = 2").unwrap();
        assert_ne!(
            k,
            h.cache_key("t.py::a", &deps, "3.12").unwrap(),
            "a content change misses the cache"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_key_none_when_unsound() {
        let dir = temp("unsound");
        let h = EngineHandler::new("python3", "shim.py", dir.clone());
        assert!(
            h.cache_key("t.py::a", &[], "3.12").is_none(),
            "no recorded deps → no sound key"
        );
        assert!(
            h.cache_key("t.py::a", &["missing.py".to_string()], "3.12")
                .is_none(),
            "an unreadable dep → no key (run instead)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
