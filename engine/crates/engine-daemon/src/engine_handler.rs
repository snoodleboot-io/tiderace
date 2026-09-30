use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use engine_core::cache::{Cache, CacheKey, CacheKeyBuilder, CachedOutcome, DirCache};
use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestItem, TestResult};
use engine_core::exec::{ForkWorker, SubInterpWorker, Worker};
use engine_core::runner::DEFAULT_DEADLINE_MS;

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
    /// The warm **image** for full parallel runs (TID-84): a persistent pool parent holding the
    /// imported suite, from which every `RunFull` forks its workers. Dropped and relaunched when
    /// the tree's `.py` files change (`warm_stamp`), so a stale module is never executed.
    #[cfg(unix)]
    warm: Option<engine_core::exec::WellspringPool>,
    #[cfg(unix)]
    warm_stamp: Option<u64>,
    /// The `-k` / `-m` / `--strict-markers` of the `RunFull` being served (TID-90): handed to the
    /// warm image's workers, or to a one-shot pool through its environment. `None` between runs.
    /// Read on the Unix path only; the non-Unix pool takes no selection (TID-90).
    #[cfg_attr(not(unix), allow(dead_code))]
    selection: Option<engine_core::exec::Selection>,
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
            #[cfg(unix)]
            warm: None,
            #[cfg(unix)]
            warm_stamp: None,
            selection: None,
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
                .map(|w| w.with_deadline_ms(DEFAULT_DEADLINE_MS))
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
    /// A digest of every `.py` file and pytest config file under the root — path, mtime, size —
    /// cheap enough to take per run. It is what decides whether the warm image still describes
    /// the tree (TID-84): the shim reads the config (`addopts`, markers) at start-up, so a config
    /// edit stales the image exactly as a source edit does.
    #[cfg(unix)]
    fn tree_stamp(root: &Path) -> u64 {
        use std::hash::{Hash, Hasher};
        fn walk(dir: &Path, root: &Path, h: &mut std::collections::hash_map::DefaultHasher) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if path.is_dir() {
                    if !engine_core::collection::SKIP_DIRS.contains(&name.as_ref()) {
                        walk(&path, root, h);
                    }
                } else if name.ends_with(".py")
                    || matches!(
                        name.as_ref(),
                        "pytest.ini" | "pyproject.toml" | "tox.ini" | "setup.cfg"
                    )
                {
                    if let Ok(meta) = entry.metadata() {
                        path.strip_prefix(root).unwrap_or(&path).hash(h);
                        meta.len().hash(h);
                        if let Ok(m) = meta.modified() {
                            m.hash(h);
                        }
                    }
                }
            }
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        walk(root, root, &mut h);
        h.finish()
    }

    /// The warm image for this run, taken out of `self` for the run's duration: reused while the
    /// tree is unchanged and the parent alive; otherwise dropped, and — for a full run — relaunched,
    /// which is the full import a full run pays anyway (TID-84). An impacted run on a changed tree
    /// gets `None`: it runs on the one-shot pool with its selective import (TID-75), which is
    /// cheaper than importing the whole tree into a new image it may not need.
    #[cfg(unix)]
    fn warm_pool(
        &mut self,
        launch: bool,
    ) -> Result<Option<engine_core::exec::WellspringPool>, String> {
        let stamp = Self::tree_stamp(&self.root);
        if let Some(mut pool) = self.warm.take() {
            if self.warm_stamp == Some(stamp) && pool.is_alive() {
                return Ok(Some(pool));
            }
            drop(pool); // stale or dead: its parent exits
        }
        if !launch {
            return Ok(None);
        }
        let pool = engine_core::exec::WellspringPool::launch_persistent(
            &self.python,
            &self.shim,
            &self.root,
            true,
        )
        .map_err(|e| format!("failed to launch the warm image: {e}"))?;
        self.warm_stamp = Some(stamp);
        Ok(Some(pool))
    }

    /// The warm image's parent pid, when one is held.
    fn warm_pid(&self) -> Option<i64> {
        #[cfg(unix)]
        {
            self.warm.as_ref().map(|p| i64::from(p.pid()))
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Whether a warm image is currently held.
    pub fn is_warm(&self) -> bool {
        #[cfg(unix)]
        {
            self.warm.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// The root this handler serves.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run_items_parallel(
        &mut self,
        requested: &[String],
        trusted: &HashSet<String>,
        must_fork: &HashSet<String>,
        durations: &HashMap<String, u64>,
        full_run: bool,
        collected: Option<Vec<TestItem>>,
    ) -> Result<Vec<TestResult>, String> {
        let mut phase = PhaseTimer::start("run_items");
        // A caller that already collected the tree hands it over (TID-94): a full run collected
        // it for its candidates a moment ago, and collection is a walk of every test file.
        let all = match collected {
            Some(items) => items,
            None => self.collect()?,
        };
        phase.mark("collect items");
        let items: Vec<TestItem> = if requested.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|it| requested.iter().any(|r| r == it.node_id.as_str()))
                .collect()
        };
        #[cfg(unix)]
        {
            // Warm image (TID-84): this run's workers are forked off a persistent parent that
            // already holds the imported suite — when one is held and still describes the tree,
            // or when this is a full run, which launches it. When the ladder is off
            // (`TIDERACE_FORCE_FORK=1`) the one-shot pool runs as before.
            let mut pool = if optimistic_no_fork() {
                self.warm_pool(full_run)?
            } else {
                None
            };
            phase.mark("warm pool");
            // This run's selection (TID-90): a warm image's workers apply it after the fork; a
            // one-shot pool reads it the way `tiderace run` hands it over, from the environment,
            // set for this run alone so the next request (or a later image launch) sees none of it.
            let selection = self.selection.clone();
            let env_guard = match pool.as_mut() {
                Some(p) => {
                    p.set_selection(selection);
                    None
                }
                None => selection.map(SelectionEnv::set),
            };
            let out = crate::pool::run_parallel(
                &self.python,
                &self.shim,
                &self.root,
                items,
                crate::pool::default_workers(),
                DEFAULT_DEADLINE_MS,
                optimistic_no_fork(),
                trusted,
                must_fork,
                durations,
                pool.as_mut(),
            );
            self.warm = pool; // back for the next run, whatever this one's outcome
            drop(env_guard);
            phase.mark("run_parallel");
            out
        }
        #[cfg(not(unix))]
        let _ = full_run;
        #[cfg(not(unix))]
        crate::pool::run_parallel(
            &self.python,
            &self.shim,
            &self.root,
            items,
            crate::pool::default_workers(),
            DEFAULT_DEADLINE_MS,
            optimistic_no_fork(), // no-fork + restore by default (TIDERACE_FORCE_FORK=1 to disable)
            trusted,
            must_fork, // TID-33: recorded state-disturbers skip the in-process ladder entirely
            durations, // TID-62: what each node cost last time, so the heaviest module goes first
            None,
        )
    }

    /// Full run across the parallel pool (the one-shot `run --all` path). Now **purity-aware** (TID-1):
    /// it loads the persisted state, runs *recorded-pure + unchanged* tests BARE no-fork (skip the
    /// snapshot), re-verifies the rest under restore, and persists the updated verdicts + footprints.
    /// So the second `run --all` on an unchanged tree runs the pure suite at the bare-no-fork tier.
    pub fn run_full_parallel(&mut self) -> Result<Vec<RpcResult>, String> {
        Ok(self.run_full_results()?.into_iter().map(to_rpc).collect())
    }

    /// The same run, as the engine's own `TestResult`s — what a report is built from (TID-84).
    pub fn run_full_results(&mut self) -> Result<Vec<TestResult>, String> {
        let mut phase = PhaseTimer::start("run_full");
        let state_path = self.root.join(STATE_FILE);
        let mut state = PersistedState::load(&state_path);
        phase.mark("load state");

        // Trusted = recorded pure AND none of its recorded deps changed since it was last verified.
        let current = self.hash_known_files(&state);
        let changed = changed_files(&state, &current);
        phase.mark("hash known files");
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
        phase.mark("trusted / must-fork / durations");

        // ADR-E015 / TID-11: with the sub-interpreter tier on (`TIDERACE_SUBINTERP=1`), route the
        // sub-interp-**safe** modules through a parallel sub-interpreter pool (no fork; sound because
        // both module globals and os.environ are per-interpreter) and everything else through the fork
        // pool. Sub-interpreters are the only parallelism Windows (no fork) has. Off ⇒ fork pool only.
        // Every collected node id: what the planner will be asked about next time, so a candidate
        // that executes and produces nothing can be recorded as deselected (TID-73).
        let collected = self.collect()?;
        let all_candidates: Vec<String> = collected.iter().map(|i| i.node_id.to_string()).collect();
        phase.mark("collect candidates");
        let fresh = if subinterp_enabled() {
            let items = collected;
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
                let mut w = SubInterpWorker::new(DEFAULT_DEADLINE_MS)
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
                    true,
                    None,
                )?);
            }
            fresh
        } else {
            self.run_items_parallel(&[], &trusted, &must_fork, &durations, true, Some(collected))?
        };
        phase.mark("run");
        // A filtered run (TID-90) learns nothing about deselection: every unselected node
        // produces nothing by design, which is indistinguishable from an `addopts` deselection
        // (TID-73) — and recording it as one would make the next warm run skip the suite (TID-92).
        let unfiltered = self.selection.as_ref().is_none_or(|s| s.is_empty());
        // Saved only when the run changed something (TID-94): a `-k` that selected nothing
        // records nothing, and the state is a few megabytes.
        if self.persist_results(&mut state, &all_candidates, &fresh, unfiltered) {
            state
                .save(&state_path)
                .map_err(|e| format!("state save failed: {e}"))?;
        }
        phase.mark("persist");
        Ok(fresh)
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
    fn persist_results(
        &self,
        state: &mut PersistedState,
        executed: &[String],
        results: &[TestResult],
        learn_deselection: bool,
    ) -> bool {
        let mut changed = !results.is_empty();
        state.record_durations(results); // TID-62: the next run's scheduler weights
                                         // A candidate that ran and produced nothing is one the project's own `addopts` deselects
                                         // or ignores: the shim answers it with an empty expansion, so it never had a record, so
                                         // the planner called it "never seen" on every warm run — 55 phantoms on pirn-core, each a
                                         // request, together forcing a wellspring launch to run nothing (TID-73). Record what was
                                         // learned: it is deselected, and that verdict depends on its own module and on the config
                                         // that deselected it. A candidate that produces results again drops the record.
        let config_deps = self.config_deps();
        // `learn_deselection` is false for a run under `-k` / `-m` (TID-92): what produced nothing
        // was unselected by this run, not by the project, and the records stay as they were.
        let deselected = if learn_deselection {
            deselected_candidates(executed, results)
        } else {
            Vec::new()
        };
        for cand in executed {
            // Selected again — it produced results of its own this time, so those are the record
            // now. Same rule as `deselected_candidates`, or a class whose own methods are their own
            // candidates would be recorded as deselected and un-recorded in the same breath.
            if learn_deselection
                && !deselected.contains(cand)
                && state
                    .tests
                    .get(cand)
                    .is_some_and(|rec| rec.outcome == DESELECTED)
            {
                state.tests.remove(cand);
                changed = true;
            }
        }
        if !deselected.is_empty() {
            changed = true;
        }
        for cand in deselected {
            let mut deps = vec![engine_core::runner::locality_key(&cand)];
            deps.extend(config_deps.iter().cloned());
            state.tests.insert(
                cand,
                TestRecord {
                    outcome: DESELECTED.to_string(),
                    detail: String::new(),
                    deps,
                    pure: None,
                    must_fork: false,
                },
            );
        }
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
        // The dependency hashes move to "as of now" only after an unfiltered run (TID-94): a
        // filtered run re-ran only what it selected, and re-baselining then would mark an edited
        // file as current while the tests that depend on it still carry the verdicts from before
        // the edit — the next impacted run would serve them from cache.
        if learn_deselection {
            self.rebaseline_hashes(state);
        }
        changed
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

        // impact-skip: serve locally-unchanged tests from the persisted record (no execution). A
        // `deselected` record is a verdict with nothing to serve — the node is absent from the
        // tally, as it is in pytest (TID-73) — so it is neither a result nor a cached count.
        let mut results: Vec<RpcResult> = p
            .cached
            .iter()
            .filter_map(|node| {
                state
                    .tests
                    .get(node)
                    .filter(|rec| rec.outcome != DESELECTED)
                    .map(|rec| RpcResult {
                        node_id: node.clone(),
                        outcome: rec.outcome.clone(),
                        duration_ms: 0,
                    })
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
                let fresh = self.run_items_parallel(
                    &to_execute,
                    &HashSet::new(),
                    &disturbers,
                    &durations,
                    false,
                    None,
                )?;
                for r in &fresh {
                    results.push(to_rpc(r.clone()));
                }
                self.persist_results(&mut state, &to_execute, &fresh, true);
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

    /// The config files at the root that can deselect a test (`addopts`, `markers`) — the deps a
    /// `deselected` verdict carries, so a change to the config re-evaluates it (TID-73).
    fn config_deps(&self) -> Vec<String> {
        ["pyproject.toml", "pytest.ini", "setup.cfg", "tox.ini"]
            .into_iter()
            .filter(|name| self.root.join(name).exists())
            .map(str::to_string)
            .collect()
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

/// The outcome recorded for a candidate the project's own `addopts` deselects or ignores (TID-73).
/// A verdict, not a result: the planner judges it like any test, and nothing is ever served for it.
const DESELECTED: &str = "deselected";

/// Whether `id` is `cand` itself or one of its runtime expansions — a parametrize case
/// (`cand[…]`) or an inherited method (`cand::…`) — and not a sibling sharing a prefix.
fn expands(cand: &str, id: &str) -> bool {
    id == cand
        || (id.starts_with(cand)
            && (id.as_bytes()[cand.len()] == b'[' || id[cand.len()..].starts_with("::")))
}

/// The executed candidates that produced no result at all: the shim answered each with an empty
/// expansion, which is how a node the project deselects or ignores reports itself.
fn deselected_candidates(executed: &[String], results: &[TestResult]) -> Vec<String> {
    // A result that is itself a candidate counts for nobody but itself: a class's own methods are
    // collected and judged as their own candidates, so they are not the class's results. The same
    // rule `plan()` applies — or the two disagree, and a class with only own methods is "never
    // seen" on the first warm run.
    let direct: HashSet<&str> = executed.iter().map(String::as_str).collect();
    executed
        .iter()
        .filter(|cand| {
            !results.iter().any(|r| {
                let id = r.node_id.as_str();
                id == cand.as_str() || (expands(cand, id) && !direct.contains(id))
            })
        })
        .cloned()
        .collect()
}

fn to_rpc_full(r: TestResult) -> crate::rpc_method::RpcFullResult {
    crate::rpc_method::RpcFullResult {
        node_id: r.node_id.to_string(),
        outcome: outcome_token(r.outcome).to_string(),
        duration_ms: r.duration_ms,
        detail: r.detail,
        touched_files: r.touched_files,
        pure: r.pure,
        must_fork: r.must_fork,
        skip_origin: r.skip_origin,
        expanded: r.expanded,
        worker: r.worker,
        unit: r.unit,
        unit_started_ms: r.unit_started_ms,
        unit_ended_ms: r.unit_ended_ms,
    }
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
            RpcRequest::RunFull {
                keyword,
                marker,
                strict_markers,
            } => {
                self.selection = Some(engine_core::exec::Selection {
                    keyword,
                    marker,
                    strict_markers,
                });
                let out = self.run_full_results();
                self.selection = None;
                match out {
                    Ok(results) => RpcResponse::RanFull {
                        results: results.into_iter().map(to_rpc_full).collect(),
                    },
                    Err(message) => RpcResponse::Error { message },
                }
            }
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
                    .or_else(|| self.warm_pid())
                    .unwrap_or_else(|| i64::from(std::process::id())),
                warm: self.worker.is_some() || self.is_warm(),
            },
            RpcRequest::Shutdown => RpcResponse::ShuttingDown,
        }
    }
}

/// Where a daemon-served run's time goes, phase by phase, on stderr when `TIDERACE_TIMING=1` —
/// the daemon's counterpart of the shim's start-up timer (TID-91). Silent otherwise.
struct PhaseTimer {
    on: bool,
    name: &'static str,
    started: std::time::Instant,
    last: std::time::Instant,
}

impl PhaseTimer {
    fn start(name: &'static str) -> Self {
        let now = std::time::Instant::now();
        Self {
            on: std::env::var_os("TIDERACE_TIMING").is_some(),
            name,
            started: now,
            last: now,
        }
    }

    fn mark(&mut self, label: &str) {
        if !self.on {
            return;
        }
        let now = std::time::Instant::now();
        eprintln!(
            "tiderace-daemon: timing: {}: {label} {}ms (at {}ms)",
            self.name,
            now.duration_since(self.last).as_millis(),
            now.duration_since(self.started).as_millis()
        );
        self.last = now;
    }
}

/// The selection as the shim's environment, for a one-shot pool, restored on drop (TID-90).
#[cfg(unix)]
struct SelectionEnv {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

#[cfg(unix)]
impl SelectionEnv {
    const KEYS: [&'static str; 3] = [
        "TIDERACE_KEYWORD_EXPR",
        "TIDERACE_MARKER_EXPR",
        "TIDERACE_STRICT_MARKERS",
    ];

    fn set(selection: engine_core::exec::Selection) -> Self {
        let saved = Self::KEYS
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        let values = [
            selection.keyword,
            selection.marker,
            selection.strict_markers.then(|| "1".to_string()),
        ];
        for (key, value) in Self::KEYS.iter().zip(values) {
            // SAFETY: the daemon serves one request at a time on this thread, and no other
            // thread reads the environment while a run is being set up.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
        Self { saved }
    }
}

#[cfg(unix)]
impl Drop for SelectionEnv {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            // SAFETY: as in `set`.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
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
    #[test]
    fn a_candidate_with_no_result_of_its_own_or_of_its_expansions_is_deselected() {
        use super::{deselected_candidates, expands};
        use engine_core::domain::{NodeId, Outcome, TestResult};
        let r = |id: &str| TestResult::new(NodeId::new(id), Outcome::Passed, 1, "");
        let results = [
            r("t.py::a[1]"),
            r("t.py::Klass::test_inherited"),
            r("t.py::Own::test_own"),
            r("t.py::plain"),
        ];
        let executed: Vec<String> = [
            "t.py::a",
            "t.py::Klass",
            "t.py::Own",
            "t.py::Own::test_own",
            "t.py::plain",
            "t.py::gone",
            "t.py::pla",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            deselected_candidates(&executed, &results),
            vec![
                "t.py::Own".to_string(),
                "t.py::gone".to_string(),
                "t.py::pla".to_string()
            ],
            "a case and an inherited method count for their candidate; an own method that is \
             itself a candidate counts only for itself, so a class with only own methods is \
             deselected; a prefix counts for nothing"
        );
        assert!(expands("t.py::a", "t.py::a[1]"));
        assert!(expands("t.py::Klass", "t.py::Klass::test_x"));
        assert!(!expands("t.py::pla", "t.py::plain"));
    }

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

        handler.persist_results(
            &mut state,
            &[],
            &[result("t.py::a", Some(true), &["t.py"])],
            true,
        );
        assert_eq!(state.tests["t.py::a"].pure, Some(true));

        // The bare run: nothing measured.
        handler.persist_results(&mut state, &[], &[result("t.py::a", None, &["t.py"])], true);
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

        handler.persist_results(
            &mut state,
            &[],
            &[result("t.py::a", Some(true), &["t.py"])],
            true,
        );
        handler.persist_results(
            &mut state,
            &[],
            &[result("t.py::a", Some(false), &["t.py"])],
            true,
        );
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
            &[],
            &[result("t.py::a", Some(true), &["t.py", "src.py"])],
            true,
        );
        handler.persist_results(&mut state, &[], &[result("t.py::a", None, &[])], true);
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
