use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

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
}

impl EngineHandler {
    pub fn new(
        python: impl Into<String>,
        shim: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            python: python.into(),
            shim: shim.into(),
            root: root.into(),
            worker: None,
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
        let (ran_count, cached_count) = (p.to_run.len(), p.cached.len());

        // Serve cached tests from the persisted record (no execution).
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

        // Execute only the impacted tests (skip the wellspring launch entirely if none), in parallel.
        // Impacted tests are stale (their deps changed), so we don't trust a prior purity verdict — they
        // run under restore, which re-measures the verdict that persist_results then records.
        if !p.to_run.is_empty() {
            let fresh = self.run_items_parallel(&p.to_run, &HashSet::new())?;
            for r in &fresh {
                results.push(to_rpc(r.clone()));
            }
            self.persist_results(&mut state, &fresh);
            state
                .save(&state_path)
                .map_err(|e| format!("state save failed: {e}"))?;
        }

        Ok(ImpactSummary {
            results,
            ran: ran_count,
            cached: cached_count,
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
