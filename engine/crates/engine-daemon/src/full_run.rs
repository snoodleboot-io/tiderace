//! The full parallel run (TID-84, TID-90, TID-102): purity-aware, `-k`-prefiltered, and the
//! one that persists verdicts and footprints for the runs after it.

use engine_core::domain::{NodeId, TestItem, TestResult};
use engine_core::exec::{KeywordExpr, Selection, SubInterpWorker, Worker};
use engine_core::runner::{Learned, PhaseTimer, DEFAULT_DEADLINE_MS};

use crate::error::Result;
use crate::rpc::method::RpcResult;
use crate::state::fold::{disturbers, recorded_durations, save_or_warn, RunScope};
use crate::state::keyword_prefilter::keyword_prefilter;
use crate::state::plan::{changed_files, PersistedState, STATE_FILE};
use crate::{to_rpc, EngineHandler};

impl EngineHandler {
    /// Full run across the parallel pool (the one-shot `run --all` path). Now **purity-aware** (TID-1):
    /// it loads the persisted state, runs *recorded-pure + unchanged* tests BARE no-fork (skip the
    /// snapshot), re-verifies the rest under restore, and persists the updated verdicts + footprints.
    /// So the second `run --all` on an unchanged tree runs the pure suite at the bare-no-fork tier.
    pub fn run_full_parallel(&mut self) -> Result<Vec<RpcResult>> {
        Ok(self
            .run_full_results(None)?
            .into_iter()
            .map(to_rpc)
            .collect())
    }

    /// The same run, as the engine's own `TestResult`s — what a report is built from (TID-84) —
    /// under `selection`, the run's `-k` / `-m` / `--strict-markers` (TID-90), or none.
    pub fn run_full_results(&mut self, selection: Option<&Selection>) -> Result<Vec<TestResult>> {
        let mut phase = PhaseTimer::start("tiderace-daemon", "run_full");
        let state_path = self.root.join(STATE_FILE);
        let mut state = PersistedState::load(&state_path);
        phase.mark("load state");

        // Trusted = recorded pure AND none of its recorded deps changed since it was last verified.
        let current = self.hash_known_files(&state);
        let changed = changed_files(&state, &current);
        phase.mark("hash known files");
        let learned = Learned {
            trusted_pure: state
                .tests
                .iter()
                .filter(|(_, rec)| {
                    rec.pure == Some(true) && !rec.deps.iter().any(|d| changed.contains(d))
                })
                .map(|(node, _)| NodeId::new(node.clone()))
                .collect(),
            // TID-33: recorded state-disturbers are forked from the start. Separate from
            // `trusted` and its inverse: most impure tests are impure in ways restore handles
            // completely, and forking those would cost the ladder nearly everything it buys.
            must_fork: disturbers(&state),
            durations: recorded_durations(&state),
        };
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
        // `-k` decided here for every candidate the state can vouch for (TID-102): the workers
        // judged 5,482 nodes one by one to run one, ~250 ms of a 350 ms round trip. What survives
        // — matching, or not decidable — still travels with the run's `-k`, and the workers'
        // verdict is the one that counts for what they receive. Not under `--strict-markers`:
        // the shim's unknown-mark error precedes its `-k` verdict, as pytest's does.
        let prefiltered: Option<(Vec<String>, Vec<TestResult>)> = selection
            .filter(|s| !s.strict_markers)
            .and_then(|s| s.keyword.as_deref())
            .and_then(KeywordExpr::parse)
            .map(|expr| {
                let (keep, replayed) =
                    keyword_prefilter(&state, &changed, &current, &all_candidates, &expr);
                phase.mark(&format!(
                    "keyword prefilter: {} of {} candidates decided by the daemon, {} skips replayed",
                    all_candidates.len() - keep.len(),
                    all_candidates.len(),
                    replayed.len()
                ));
                (keep, replayed)
            });
        let fresh = if self.config.subinterp {
            let items = collected;
            let modules: Vec<String> = {
                let mut m: Vec<String> = items
                    .iter()
                    .map(|it| it.node_id.file().to_string())
                    .collect();
                m.sort();
                m.dedup();
                m
            };
            let safe = self.safe_set(&mut state, &modules)?;
            let (si_items, fork_items): (Vec<TestItem>, Vec<TestItem>) = items
                .into_iter()
                .partition(|it| safe.contains(it.node_id.file()));

            let mut fresh = Vec::new();
            if !si_items.is_empty() {
                let mut w = SubInterpWorker::new(DEFAULT_DEADLINE_MS)
                    .with_target(self.python.clone(), &self.shim, &self.root)
                    .with_pool_size(engine_core::runner::default_workers());
                fresh.extend(w.run(&si_items)?);
            }
            if !fork_items.is_empty() {
                let fork_nodes: Vec<String> =
                    fork_items.iter().map(|it| it.node_id.to_string()).collect();
                fresh.extend(self.run_items_parallel(
                    &fork_nodes,
                    &learned,
                    selection,
                    true,
                    None,
                )?);
            }
            fresh
        } else {
            match prefiltered {
                // Every candidate decided against: nothing to run, and no image to launch for it.
                Some((keep, replayed)) if keep.is_empty() => replayed,
                Some((keep, mut replayed)) => {
                    replayed.extend(self.run_items_parallel(
                        &keep,
                        &learned,
                        selection,
                        true,
                        Some(collected),
                    )?);
                    replayed
                }
                None => self.run_items_parallel(&[], &learned, selection, true, Some(collected))?,
            }
        };
        phase.mark("run");
        // A filtered run (TID-90) learns nothing about deselection: every unselected node
        // produces nothing by design, which is indistinguishable from an `addopts` deselection
        // (TID-73) — and recording it as one would make the next warm run skip the suite (TID-92).
        let scope = if selection.is_none_or(|s| s.is_empty()) {
            RunScope::Whole
        } else {
            RunScope::Selected
        };
        // Saved only when the run changed something (TID-94): a `-k` that selected nothing
        // records nothing, and the state is a few megabytes.
        if self.persist_results(&mut state, &all_candidates, &fresh, scope) {
            save_or_warn(&state, &state_path);
        }
        phase.mark("persist");
        Ok(fresh)
    }
}
