//! How a run's results fold into the persisted state (TID-73, TID-102, TID-119): outcomes,
//! footprints and the sticky verdicts, the deselections a run learned, and the file hashes
//! rebaselined to "as of now".

use std::collections::{BTreeMap, HashMap, HashSet};

use engine_core::domain::{NodeId, TestResult};
use engine_core::runner::RecordedOutcome;

use crate::state::plan::{PersistedState, TestRecord};
use crate::EngineHandler;

/// What a run was asked for, as far as the state's deselection records are concerned (TID-92).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunScope {
    /// The whole tree: a candidate that produced nothing was deselected by the project's own
    /// `addopts`, and the state learns that.
    Whole,
    /// A `-k` / `-m` selection: what produced nothing was unselected by this run, not by the
    /// project, and the records stay as they were.
    Selected,
}

/// Save the state beside the tree, or say why not and go on. The tests have run and their
/// results are in hand: an unwritable tree must not turn that answer into an I/O error. What was
/// not recorded is simply run again next time — repeated work, never a stale verdict — which is
/// the same contract `tiderace run` has for its durations hint. The one persistence-failure
/// policy, decided on TID-119.
pub(crate) fn save_or_warn(state: &PersistedState, path: &std::path::Path) {
    if let Err(e) = state.save(path) {
        eprintln!(
            "tiderace: warning: state not saved to {}: {e} — the next run repeats what this one learned",
            path.display()
        );
    }
}

impl EngineHandler {
    /// Fold a batch of results into the persisted state (outcome + detail + deps + purity verdict) and
    /// rebaseline the content hashes of every touched file. Shared by the impact-aware + full runs.
    pub(crate) fn persist_results(
        &self,
        state: &mut PersistedState,
        executed: &[String],
        results: &[TestResult],
        scope: RunScope,
    ) -> bool {
        // Durations are refreshed in memory on every run but do not count as a change on their
        // own (TID-102): a `-k` run that re-ran one test and replayed its skips would otherwise
        // rewrite the whole state — 11 MB on pirn-core, 55 ms of a 300 ms round trip — for a
        // millisecond of jitter. A duration survives to disk with the next real change.
        let mut changed = false;
        state.record_durations(results); // TID-62: the next run's scheduler weights
                                         // A candidate that ran and produced nothing is one the project's own `addopts` deselects
                                         // or ignores: the shim answers it with an empty expansion, so it never had a record, so
                                         // the planner called it "never seen" on every warm run — 55 phantoms on pirn-core, each a
                                         // request, together forcing a wellspring launch to run nothing (TID-73). Record what was
                                         // learned: it is deselected, and that verdict depends on its own module and on the config
                                         // that deselected it. A candidate that produces results again drops the record.
        let config_deps = self.config_deps();
        let learn_deselection = scope == RunScope::Whole;
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
                    .is_some_and(|rec| rec.outcome.is_deselected())
            {
                state.tests.remove(cand);
                changed = true;
            }
        }
        if !deselected.is_empty() {
            changed = true;
        }
        for cand in deselected {
            let mut deps = vec![NodeId::file_of(&cand).to_string()];
            deps.extend(config_deps.iter().cloned());
            state.tests.insert(cand, TestRecord::deselected(deps));
        }
        for r in results {
            let prior = state.tests.get(r.node_id.as_str());
            let record = TestRecord {
                outcome: RecordedOutcome::Ran(r.outcome),
                detail: r.detail.clone(),
                // An empty footprint means capture was off, not that the test depends on
                // nothing — every test touches at least its own file. Overwriting a real
                // footprint with "no data" would silently disarm the staleness guards that
                // read it.
                deps: if !r.touched_files.is_empty() {
                    r.touched_files.clone()
                } else if let Some(p) = prior.filter(|p| !p.deps.is_empty()) {
                    p.deps.clone()
                } else if let Some(origin) = &r.skip_origin {
                    // A module-import skip touches nothing but its module: that is what the
                    // replay of the skip hangs off (TID-102).
                    vec![origin.clone()]
                } else {
                    Vec::new()
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
                // Sticky like `pure`: a run that did not reach the verdict reports none (TID-102).
                keywords: if r.keywords.is_empty() {
                    prior.map(|p| p.keywords.clone()).unwrap_or_default()
                } else {
                    r.keywords.clone()
                },
                skip_origin: r.skip_origin.clone(),
            };
            if prior != Some(&record) {
                changed = true;
                state.tests.insert(r.node_id.to_string(), record);
            }
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

    /// Current content hashes (hex) for every file already in the persisted state.
    pub(crate) fn hash_known_files(&self, state: &PersistedState) -> BTreeMap<String, String> {
        state
            .files
            .keys()
            .map(|rel| (rel.clone(), self.hash_file(rel)))
            .collect()
    }

    /// Set `state.files` to the current hash of every file any recorded test depends on.
    pub(crate) fn rebaseline_hashes(&self, state: &mut PersistedState) {
        let mut files = BTreeMap::new();
        // The config files too (TID-102): the rootdir a node's path names hang off is where the
        // ini was found, so a config edit must show as a change to the keyword records.
        for rel in self.config_deps() {
            files.insert(rel.clone(), self.hash_file(&rel));
        }
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
    pub(crate) fn config_deps(&self) -> Vec<String> {
        engine_core::collection::CONFIG_FILES
            .iter()
            .filter(|name| self.root.join(name).exists())
            .map(|name| name.to_string())
            .collect()
    }

    /// Hex content hash of `<root>/rel`; the sentinel for a missing file (⇒ counts as changed).
    pub(crate) fn hash_file(&self, rel: &str) -> String {
        engine_core::runner::hash_file_or_missing(&self.root, rel)
    }
}

pub(crate) fn expands(cand: &str, id: &str) -> bool {
    id == cand || NodeId::expands(id, cand)
}

/// The executed candidates that produced no result at all: the shim answered each with an empty
/// expansion, which is how a node the project deselects or ignores reports itself.
pub(crate) fn deselected_candidates(executed: &[String], results: &[TestResult]) -> Vec<String> {
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

/// Node ids recorded as disturbing interpreter state: forked from the start (TID-33).
pub(crate) fn disturbers(state: &PersistedState) -> HashSet<NodeId> {
    state
        .tests
        .iter()
        .filter(|(_, rec)| rec.must_fork)
        .map(|(node, _)| NodeId::new(node.clone()))
        .collect()
}

/// The last run's per-node cost, as the scheduler's weights (TID-62). Kept as a `HashMap` for the
/// `RunPlan`; the state stores a `BTreeMap` so the file is stable across saves.
pub(crate) fn recorded_durations(state: &PersistedState) -> HashMap<NodeId, u64> {
    state
        .durations
        .iter()
        .map(|(k, v)| (NodeId::new(k.clone()), *v))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use engine_core::domain::{NodeId, Outcome, TestResult};

    use super::{deselected_candidates, expands, RunScope};
    use crate::state::plan::PersistedState;
    use crate::EngineHandler;

    fn handler_for(dir: &std::path::Path) -> EngineHandler {
        EngineHandler::new("python3", PathBuf::from("shim.py"), dir.to_path_buf())
    }

    fn result(node: &str, pure: Option<bool>, deps: &[&str]) -> TestResult {
        TestResult::new(NodeId::new(node), Outcome::Passed, 1, "")
            .with_touched(deps.iter().map(|d| (*d).to_string()).collect())
            .with_pure(pure)
    }

    #[test]
    fn a_candidate_with_no_result_of_its_own_or_of_its_expansions_is_deselected() {
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
            RunScope::Whole,
        );
        assert_eq!(state.tests["t.py::a"].pure, Some(true));

        // The bare run: nothing measured.
        handler.persist_results(
            &mut state,
            &[],
            &[result("t.py::a", None, &["t.py"])],
            RunScope::Whole,
        );
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
            RunScope::Whole,
        );
        handler.persist_results(
            &mut state,
            &[],
            &[result("t.py::a", Some(false), &["t.py"])],
            RunScope::Whole,
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
            RunScope::Whole,
        );
        handler.persist_results(
            &mut state,
            &[],
            &[result("t.py::a", None, &[])],
            RunScope::Whole,
        );
        assert_eq!(
            state.tests["t.py::a"].deps,
            vec!["t.py".to_string(), "src.py".to_string()],
            "a run with capture off must not wipe the footprint"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
