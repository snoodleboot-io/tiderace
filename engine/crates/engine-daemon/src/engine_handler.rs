use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use engine_core::cache::{Cache, CacheKey, CacheKeyBuilder, CachedOutcome, DirCache};
use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestItem, TestResult};
use engine_core::exec::{ForkWorker, Worker};

use crate::persist::{changed_files, plan, PersistedState, TestRecord};
use crate::rpc_method::{RpcRequest, RpcResponse, RpcResult};
use crate::rpc_server::RpcHandler;
use crate::watch::content_hash;

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
    /// Content-addressed result cache (ADR-E004, TID-7). Enabled by `RIPTIDE_CACHE_DIR` pointing at a
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
        let cache = std::env::var("RIPTIDE_CACHE_DIR")
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
    /// unchanged, TID-1) run BARE no-fork (skip the snapshot → ~90×).
    fn run_items_parallel(
        &self,
        requested: &[String],
        trusted: &HashSet<String>,
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
            optimistic_no_fork(), // no-fork + restore by default (RIPTIDE_FORCE_FORK=1 to disable)
            trusted,
        )
    }

    /// Full run across the parallel pool (the one-shot `run --all` path). Now **purity-aware** (TID-1):
    /// it loads the persisted state, runs *recorded-pure + unchanged* tests BARE no-fork (skip the
    /// snapshot), re-verifies the rest under restore, and persists the updated verdicts + footprints.
    /// So the second `run --all` on an unchanged tree runs the pure suite at the bare-no-fork tier.
    pub fn run_full_parallel(&self) -> Result<Vec<RpcResult>, String> {
        let state_path = self.root.join(".riptide-state.json");
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

        let fresh = self.run_items_parallel(&[], &trusted)?;
        self.persist_results(&mut state, &fresh);
        state
            .save(&state_path)
            .map_err(|e| format!("state save failed: {e}"))?;
        Ok(fresh.into_iter().map(to_rpc).collect())
    }

    /// Fold a batch of results into the persisted state (outcome + detail + deps + purity verdict) and
    /// rebaseline the content hashes of every touched file. Shared by the impact-aware + full runs.
    fn persist_results(&self, state: &mut PersistedState, results: &[TestResult]) {
        for r in results {
            state.tests.insert(
                r.node_id.to_string(),
                TestRecord {
                    outcome: outcome_token(r.outcome).to_string(),
                    detail: r.detail.clone(),
                    deps: r.touched_files.clone(),
                    pure: r.pure,
                },
            );
        }
        self.rebaseline_hashes(state);
    }

    /// **Impact-aware run** (the warm-mode gap): load persisted state, re-run only the tests whose
    /// dependencies changed since last run (or have never run), serve the rest from cache, and persist
    /// the updated footprint. With no changes, nothing executes — not even a wellspring launch. Needs
    /// coverage capture on (the daemon sets `RIPTIDE_COVERAGE=1`) so footprints are recorded.
    pub fn run_impacted(&mut self) -> Result<ImpactSummary, String> {
        let candidates: Vec<String> = self
            .collect()?
            .iter()
            .map(|i| i.node_id.to_string())
            .collect();
        let state_path = self.root.join(".riptide-state.json");
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
        // shared `RIPTIDE_CACHE_DIR`) is served without running, even though this machine's local state
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
                state.tests.insert(
                    node,
                    TestRecord {
                        outcome: outcome_token(outcome.outcome()).to_string(),
                        detail: outcome.detail().to_string(),
                        deps,
                        pure: Some(true),
                    },
                );
            }

            // Run the cache misses (stale purity ⇒ no trusted-pure; restore re-measures the verdict).
            if !to_execute.is_empty() {
                executed = to_execute.len();
                let fresh = self.run_items_parallel(&to_execute, &HashSet::new())?;
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

/// No-fork + restore is the default. `RIPTIDE_FORCE_FORK=1` reverts to fork-per-test — a debug/benchmark
/// escape only (not a user-facing flag), so the fork baseline stays measurable for regression checks.
fn optimistic_no_fork() -> bool {
    std::env::var("RIPTIDE_FORCE_FORK").as_deref() != Ok("1")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("riptide_ck_{tag}_{}", std::process::id()));
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
