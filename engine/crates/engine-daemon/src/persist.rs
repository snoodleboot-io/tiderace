//! The daemon's impact planner, over the shared on-disk state.
//!
//! `PersistedState` / `TestRecord` / `changed_files` moved into `engine-core` so the CLI can *read*
//! what the daemon learned without a second definition of the same JSON drifting from the writer's
//! (see `engine_core::runner::VerdictStore`). What stays here is the part only the daemon does:
//! deciding which tests to re-execute and which to serve from cache.

use std::collections::BTreeSet;

pub use engine_core::runner::{changed_files, PersistedState, TestRecord, STATE_FILE};

/// Partition `candidates` into (to_run, cached) given the changed-file set. A test runs if it has
/// never been seen, or **any** of its recorded deps changed; otherwise its cached outcome stands.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub to_run: Vec<String>,
    pub cached: Vec<String>,
}

pub fn plan(state: &PersistedState, candidates: &[String], changed: &BTreeSet<String>) -> Plan {
    let mut out = Plan::default();
    for node in candidates {
        let run = match state.tests.get(node) {
            None => true, // never seen → must run to establish a baseline
            Some(rec) => rec.deps.iter().any(|d| changed.contains(d)),
        };
        if run {
            out.to_run.push(node.clone());
        } else {
            out.cached.push(node.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> PersistedState {
        let mut s = PersistedState::default();
        s.files.insert("src.py".into(), "h1".into());
        s.files.insert("other.py".into(), "h2".into());
        s.tests.insert(
            "t.py::a".into(),
            TestRecord {
                outcome: "passed".into(),
                detail: String::new(),
                deps: vec!["src.py".into()],
                pure: Some(true),
                must_fork: false,
            },
        );
        s.tests.insert(
            "t.py::b".into(),
            TestRecord {
                outcome: "passed".into(),
                detail: String::new(),
                deps: vec!["other.py".into()],
                pure: None,
                must_fork: false,
            },
        );
        s
    }

    #[test]
    fn no_changes_caches_all_known_tests() {
        let s = state();
        let current = s.files.clone(); // identical hashes
        let changed = changed_files(&s, &current);
        assert!(changed.is_empty());
        let p = plan(&s, &["t.py::a".into(), "t.py::b".into()], &changed);
        assert!(p.to_run.is_empty());
        assert_eq!(p.cached.len(), 2);
    }

    #[test]
    fn changed_file_runs_only_its_dependents() {
        let s = state();
        let mut current = s.files.clone();
        current.insert("src.py".into(), "DIFFERENT".into()); // src.py edited
        let changed = changed_files(&s, &current);
        assert_eq!(changed, BTreeSet::from(["src.py".to_string()]));
        let p = plan(&s, &["t.py::a".into(), "t.py::b".into()], &changed);
        assert_eq!(p.to_run, vec!["t.py::a"]); // a depends on src.py
        assert_eq!(p.cached, vec!["t.py::b"]); // b depends on other.py (unchanged)
    }

    #[test]
    fn unseen_test_always_runs() {
        let s = state();
        let p = plan(&s, &["t.py::new".into()], &BTreeSet::new());
        assert_eq!(p.to_run, vec!["t.py::new"]);
    }
}
