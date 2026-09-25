//! The daemon's impact planner, over the shared on-disk state.
//!
//! `PersistedState` / `TestRecord` / `changed_files` moved into `engine-core` so the CLI can *read*
//! what the daemon learned without a second definition of the same JSON drifting from the writer's
//! (see `engine_core::runner::VerdictStore`). What stays here is the part only the daemon does:
//! deciding which tests to re-execute and which to serve from cache.

use std::collections::{BTreeSet, HashSet};

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
    let direct: HashSet<&str> = candidates.iter().map(String::as_str).collect();
    for node in candidates {
        match state.tests.get(node) {
            None => {
                // A parametrized node is *collected* as `test_x` and *recorded* per case as
                // `test_x[a]`, `test_x[b]` (TID-25); an inherited class as `mod::Class` and per
                // method. Judged by the candidate's own key alone, such a node was "never seen" on
                // every warm run and re-ran forever — one test on fx_corpus, 4,657 nodes' worth on
                // pirn-agents (TID-71). Judge it by its expansions, and serve those from cache.
                let expansions = expansions_of(state, node, &direct);
                if expansions.is_empty() {
                    out.to_run.push(node.clone()); // never seen → must run to establish a baseline
                } else if expansions
                    .iter()
                    .any(|k| state.tests[*k].deps.iter().any(|d| changed.contains(d)))
                {
                    out.to_run.push(node.clone());
                } else {
                    out.cached
                        .extend(expansions.iter().map(|k| (*k).to_string()));
                }
            }
            Some(rec) => {
                if rec.deps.iter().any(|d| changed.contains(d)) {
                    out.to_run.push(node.clone());
                } else {
                    out.cached.push(node.clone());
                }
            }
        }
    }
    out
}

/// The recorded ids that are runtime expansions of `node`: its own id followed by `[` (a
/// parametrize case) or `::` (an inherited method), never a sibling that merely shares a prefix
/// (`test_a` is not `test_ab[1]`), and never an id that is itself a collected candidate — an own
/// method of a class is judged and served as its own candidate, not twice. A range scan from the
/// node's key, since `tests` is a `BTreeMap`.
fn expansions_of<'a>(
    state: &'a PersistedState,
    node: &str,
    direct: &HashSet<&str>,
) -> Vec<&'a String> {
    state
        .tests
        .range(node.to_string()..)
        .take_while(|(k, _)| k.starts_with(node))
        .filter(|(k, _)| {
            k.len() > node.len()
                && (k.as_bytes()[node.len()] == b'[' || k[node.len()..].starts_with("::"))
                && !direct.contains(k.as_str())
        })
        .map(|(k, _)| k)
        .collect()
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

    fn recorded(s: &mut PersistedState, node: &str, deps: &[&str]) {
        s.tests.insert(
            node.into(),
            TestRecord {
                outcome: "passed".into(),
                detail: String::new(),
                deps: deps.iter().map(|d| d.to_string()).collect(),
                pure: Some(true),
                must_fork: false,
            },
        );
    }

    /// TID-71: a parametrized node is collected once and recorded per case. With nothing changed
    /// it is cached, and what is served is the cases — the ids the records are under.
    #[test]
    fn a_parametrized_node_is_judged_and_served_by_its_cases() {
        let mut s = state();
        recorded(&mut s, "t.py::test_p[a]", &["src.py"]);
        recorded(&mut s, "t.py::test_p[b]", &["src.py"]);
        let p = plan(&s, &["t.py::test_p".into()], &BTreeSet::new());
        assert!(
            p.to_run.is_empty(),
            "nothing changed, so it is not 'never seen': {p:?}"
        );
        assert_eq!(
            p.cached,
            vec!["t.py::test_p[a]".to_string(), "t.py::test_p[b]".to_string()]
        );
    }

    #[test]
    fn a_changed_dep_on_any_case_runs_the_node() {
        let mut s = state();
        recorded(&mut s, "t.py::test_p[a]", &["src.py"]);
        recorded(&mut s, "t.py::test_p[b]", &["other.py"]);
        let p = plan(
            &s,
            &["t.py::test_p".into()],
            &["other.py".to_string()].into_iter().collect(),
        );
        assert_eq!(p.to_run, vec!["t.py::test_p".to_string()]);
        assert!(p.cached.is_empty());
    }

    #[test]
    fn a_sibling_sharing_a_prefix_is_not_an_expansion() {
        let mut s = state();
        recorded(&mut s, "t.py::test_ab[1]", &["src.py"]);
        let p = plan(&s, &["t.py::test_a".into()], &BTreeSet::new());
        assert_eq!(
            p.to_run,
            vec!["t.py::test_a".to_string()],
            "never seen: {p:?}"
        );
    }

    /// An inherited-methods class is collected as the class; its own methods are collected as
    /// themselves. Each recorded method is served exactly once, under the candidate that owns it.
    #[test]
    fn a_class_candidate_does_not_double_serve_its_own_methods() {
        let mut s = state();
        recorded(&mut s, "t.py::Klass::test_inherited", &["src.py"]);
        recorded(&mut s, "t.py::Klass::test_own", &["src.py"]);
        let p = plan(
            &s,
            &["t.py::Klass".into(), "t.py::Klass::test_own".into()],
            &BTreeSet::new(),
        );
        assert!(p.to_run.is_empty(), "{p:?}");
        let mut cached = p.cached.clone();
        cached.sort();
        assert_eq!(
            cached,
            vec![
                "t.py::Klass::test_inherited".to_string(),
                "t.py::Klass::test_own".to_string()
            ]
        );
    }

    #[test]
    fn unseen_test_always_runs() {
        let s = state();
        let p = plan(&s, &["t.py::new".into()], &BTreeSet::new());
        assert_eq!(p.to_run, vec!["t.py::new"]);
    }
}
