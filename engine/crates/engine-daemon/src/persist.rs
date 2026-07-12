use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// On-disk warm state for impact-aware one-shot `run`s. Persists each test's outcome + its dependency
/// footprint (touched files, from
/// coverage) and the content hash of every touched file, so a later `run` re-executes **only** the
/// tests whose dependencies changed. Stored as JSON at `<root>/.riptide-state.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    /// relative source path -> content hash (hex) at the time it was last run.
    pub files: BTreeMap<String, String>,
    /// node id -> last result + the files it touched.
    pub tests: BTreeMap<String, TestRecord>,
    /// module rel-path -> its cached sub-interpreter-safety verdict (ADR-E015 / TID-9 cache, consumed
    /// by TID-11 routing). Re-probed only when the module's content hash changes. `#[serde(default)]`
    /// so older state files load fine.
    #[serde(default)]
    pub safe_modules: BTreeMap<String, SafeModule>,
}

/// A module's cached sub-interpreter-safety verdict, keyed by content so a change re-probes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafeModule {
    pub hash: String,
    pub safe: bool,
}

/// One test's persisted result + dependency footprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRecord {
    pub outcome: String,
    pub detail: String,
    pub deps: Vec<String>,
    /// Purity verdict (TID-1): `Some(true)` measured pure. A pure test whose deps are all unchanged is
    /// re-run BARE no-fork next time. `#[serde(default)]` ⇒ old state files (no field) load as `None`.
    #[serde(default)]
    pub pure: Option<bool>,
}

impl PersistedState {
    /// Load from `path`; a missing or unparseable file yields empty state (cold start).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to `path` (best-effort; errors are returned for the caller to log).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }
}

/// The files whose current hash differs from what was persisted (changed, or vanished). `current`
/// holds the freshly-computed hashes for the paths we re-hashed (typically every path in `state.files`).
pub fn changed_files(
    state: &PersistedState,
    current: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    state
        .files
        .iter()
        .filter(|(path, old)| current.get(*path).map(|c| c != *old).unwrap_or(true))
        .map(|(path, _)| path.clone())
        .collect()
}

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
            },
        );
        s.tests.insert(
            "t.py::b".into(),
            TestRecord {
                outcome: "passed".into(),
                detail: String::new(),
                deps: vec!["other.py".into()],
                pure: None,
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
