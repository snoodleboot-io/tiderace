//! The engine's learned per-test state, and the read path both front ends share.
//!
//! The engine measures three things per test and remembers them: whether the test was **pure**
//! (TID-1), whether it **disturbed interpreter state** (TID-33), and which files it **touched**.
//! Together those decide how the next run executes it — a recorded-pure test whose dependencies are
//! unchanged skips the snapshot entirely, and a recorded state-disturber is forked from the start.
//!
//! Until now only the daemon could act on any of it. It wrote `.tiderace-state.json` and read it
//! back; a plain `tiderace run` started from zero every time, re-deriving verdicts it already had on
//! disk and then throwing them away. This module is the read path, so `run` can benefit from what
//! the daemon learned.
//!
//! **Reading only.** `run` deliberately does not write here. Writing is a separate decision — it
//! means a one-shot command mutating the user's tree, and for purity it would also mean turning on
//! coverage capture, since the dependency footprints that keep a purity verdict honest come from it.
//! Reading needs neither: the footprints are already recorded, and checking them is a re-hash.
//!
//! The two verdicts are **not** equally safe to trust, and this module treats them differently.
//! See [`VerdictStore::trusted_pure`] and [`VerdictStore::must_fork`].

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exec::SafeModule;
use crate::fixtures::ClosureHasher;

/// The state file, written by the daemon and read by both front ends.
pub const STATE_FILE: &str = ".tiderace-state.json";

/// On-disk warm state: each test's outcome, its dependency footprint, and the content hash of every
/// touched file, so a later run can tell what is still valid.
///
/// Lives here rather than in `engine-daemon` so the CLI can read it without a second definition of
/// the same JSON drifting out of step with the writer's.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    /// relative source path -> content hash (hex) at the time it was last run.
    pub files: BTreeMap<String, String>,
    /// node id -> last result + the files it touched.
    pub tests: BTreeMap<String, TestRecord>,
    /// module rel-path -> its cached sub-interpreter-safety verdict (ADR-E015 / TID-9 cache,
    /// consumed by TID-11 routing). Re-probed only when the module's content hash changes.
    #[serde(default)]
    pub safe_modules: BTreeMap<String, SafeModule>,
}

/// One test's persisted result + dependency footprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRecord {
    pub outcome: String,
    pub detail: String,
    pub deps: Vec<String>,
    /// Purity verdict (TID-1): `Some(true)` measured pure. A pure test whose deps are all unchanged
    /// is re-run BARE no-fork next time. `#[serde(default)]` ⇒ old state files load as `None`.
    #[serde(default)]
    pub pure: Option<bool>,
    /// This test disturbed interpreter state nothing undid (TID-33), so it is forked from the start
    /// on later runs instead of being rediscovered — a wasted in-process run plus a fork each time.
    /// Sticky until the test's own file changes. `#[serde(default)]` ⇒ old state files load as false.
    #[serde(default)]
    pub must_fork: bool,
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

/// The files whose current hash differs from what was persisted (changed, or vanished).
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

/// Hex content hash of `<root>/rel`; a sentinel for a missing or unreadable file, so it always
/// counts as changed.
pub fn hash_file(root: &Path, rel: &str) -> String {
    match std::fs::read(root.join(rel)) {
        Ok(bytes) => {
            let digest = ClosureHasher::new().feed(&bytes).finish();
            let mut s = String::with_capacity(64);
            for b in digest.as_bytes() {
                s.push_str(&format!("{b:02x}"));
            }
            s
        }
        Err(_) => "missing".to_string(),
    }
}

/// Read-only view of what previous runs learned, for a front end that does not persist.
pub struct VerdictStore {
    state: PersistedState,
    root: PathBuf,
}

impl VerdictStore {
    /// Read the state file beside `root`. Missing or unparseable is an empty store, not an error —
    /// every verdict here is an optimisation, and refusing to run because a cache file got truncated
    /// would be a far worse trade than running cold.
    pub fn load(root: &Path) -> Self {
        Self {
            state: PersistedState::load(&root.join(STATE_FILE)),
            root: root.to_path_buf(),
        }
    }

    /// Whether anything was learned at all — for deciding whether to say so in the run header.
    pub fn is_empty(&self) -> bool {
        self.state.tests.is_empty()
    }

    /// Node ids safe to run **bare no-fork**: recorded pure, with every recorded dependency
    /// unchanged since it was measured.
    ///
    /// The guard is not optional. This tier skips the snapshot entirely — no isolation, no restore —
    /// so a verdict that has gone stale is not a lost optimisation but a silent cross-test
    /// contamination, which is the exact failure class TID-22/23/27/33 were about. Every dependency
    /// is re-hashed here and any mismatch drops the test back to the ordinary path.
    ///
    /// A test with **no** recorded dependencies is refused even if it was recorded pure. An empty
    /// footprint means coverage was not capturing when it ran, so there is nothing to invalidate
    /// against, and "no evidence of change" would be standing in for "no evidence".
    ///
    /// **Not currently wired into `tiderace run`**, because the guard is only as good as the
    /// footprints and they are unsound today (TID-40): a module's imports execute once, for
    /// whichever test runs first, so on a 20-tests-per-module suite the source under test appears in
    /// one footprint out of twenty. The nineteen others would keep a stale `pure` verdict through a
    /// change to the very code they exercise — and this tier skips isolation entirely. The daemon
    /// has the same exposure; the difference is that its state is minutes old, while a file on disk
    /// can be arbitrarily stale. Kept here, tested, and connected once TID-40 lands.
    pub fn trusted_pure(&self) -> HashSet<String> {
        let current: BTreeMap<String, String> = self
            .state
            .files
            .keys()
            .map(|p| (p.clone(), hash_file(&self.root, p)))
            .collect();
        let changed = changed_files(&self.state, &current);
        self.state
            .tests
            .iter()
            .filter(|(_, rec)| {
                rec.pure == Some(true)
                    && !rec.deps.is_empty()
                    && !rec.deps.iter().any(|d| changed.contains(d))
            })
            .map(|(node, _)| node.clone())
            .collect()
    }

    /// Node ids recorded as disturbing interpreter state, which are forked from the start.
    ///
    /// No staleness guard, deliberately. This verdict only ever *removes* an optimisation: acting on
    /// a stale one forks a test that no longer needed it, which costs a little time and cannot
    /// produce a wrong answer. That asymmetry is the whole reason it can be trusted from a file
    /// nobody re-verified, while [`trusted_pure`](Self::trusted_pure) cannot.
    pub fn must_fork(&self) -> HashSet<String> {
        self.state
            .tests
            .iter()
            .filter(|(_, rec)| rec.must_fork)
            .map(|(node, _)| node.clone())
            .collect()
    }
}
