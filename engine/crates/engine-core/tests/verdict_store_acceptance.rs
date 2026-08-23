//! `tiderace run` reads what the daemon learned, and is careful about which parts it trusts.
//!
//! Three things are measured per test and remembered: purity (TID-1), whether the test disturbed
//! interpreter state (TID-33), and which files it touched. Until now only the daemon could act on
//! any of it — a plain `run` started cold every time, re-deriving verdicts that were already on
//! disk. `VerdictStore` is the read path.
//!
//! The interesting part is that the two verdicts are **not** equally safe to trust from a file
//! nobody re-verified, and the store treats them differently:
//!
//! * `must_fork` only ever *removes* an optimisation. Acting on a stale one forks a test that no
//!   longer needs it — a little time, never a wrong answer. No staleness guard.
//! * `trusted_pure` promotes a test to the bare no-fork tier, which skips the snapshot **entirely**.
//!   A stale verdict there is silent cross-test contamination, the exact failure class TID-22/23/27
//!   and TID-33 were about. Every recorded dependency is re-hashed, and anything that moved drops
//!   the test back to the ordinary path.
//!
//! These tests drive the store directly rather than through a live run: the question is what the
//! store *decides*, and asserting on a timing would prove nothing about the decision.

use engine_core::runner::{PersistedState, TestRecord, VerdictStore, STATE_FILE};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn temp(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_verdict_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(deps: &[&str], pure: Option<bool>, must_fork: bool) -> TestRecord {
    TestRecord {
        outcome: "passed".into(),
        detail: String::new(),
        deps: deps.iter().map(|d| (*d).to_string()).collect(),
        pure,
        must_fork,
    }
}

/// Write a state file the way the daemon would, with `files` holding the *current* hashes.
fn seed(dir: &Path, tests: &[(&str, TestRecord)], sources: &[(&str, &str)]) {
    for (path, body) in sources {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, body).unwrap();
    }
    let mut state = PersistedState {
        files: sources
            .iter()
            .map(|(p, _)| ((*p).to_string(), engine_core::runner::hash_file(dir, p)))
            .collect::<BTreeMap<_, _>>(),
        ..PersistedState::default()
    };
    for (node, rec) in tests {
        state.tests.insert((*node).to_string(), rec.clone());
    }
    state.save(&dir.join(STATE_FILE)).unwrap();
}

/// A pure test whose dependencies are untouched is trusted.
#[test]
fn an_unchanged_pure_test_is_trusted() {
    let dir = temp("trusted");
    seed(
        &dir,
        &[(
            "test_a.py::test_x",
            record(&["src/a.py"], Some(true), false),
        )],
        &[("src/a.py", "VALUE = 1\n")],
    );
    let store = VerdictStore::load(&dir);
    assert!(store.trusted_pure().contains("test_a.py::test_x"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Editing a dependency drops the test back to the ordinary path.
///
/// This is the guard the whole design rests on: without it, reading a verdict from disk would be
/// promoting a test to *no isolation at all* on the strength of a fact that may no longer hold.
#[test]
fn a_changed_dependency_revokes_trust() {
    let dir = temp("stale");
    seed(
        &dir,
        &[(
            "test_a.py::test_x",
            record(&["src/a.py"], Some(true), false),
        )],
        &[("src/a.py", "VALUE = 1\n")],
    );
    assert!(VerdictStore::load(&dir)
        .trusted_pure()
        .contains("test_a.py::test_x"));

    std::fs::write(dir.join("src/a.py"), "VALUE = 2\n").unwrap();
    assert!(
        !VerdictStore::load(&dir)
            .trusted_pure()
            .contains("test_a.py::test_x"),
        "a purity verdict must not survive a change to what the test depends on"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A deleted dependency counts as changed, not as "nothing to check".
#[test]
fn a_vanished_dependency_revokes_trust() {
    let dir = temp("vanished");
    seed(
        &dir,
        &[(
            "test_a.py::test_x",
            record(&["src/a.py"], Some(true), false),
        )],
        &[("src/a.py", "VALUE = 1\n")],
    );
    std::fs::remove_file(dir.join("src/a.py")).unwrap();
    assert!(!VerdictStore::load(&dir)
        .trusted_pure()
        .contains("test_a.py::test_x"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Recorded pure but with an empty footprint is refused.
///
/// An empty `deps` means coverage was not capturing when the verdict was taken, so there is nothing
/// to invalidate against. Trusting it would let "no evidence of change" stand in for "no evidence",
/// which is how a stale verdict would survive forever.
#[test]
fn a_pure_verdict_with_no_recorded_dependencies_is_refused() {
    let dir = temp("nodeps");
    seed(
        &dir,
        &[("test_a.py::test_x", record(&[], Some(true), false))],
        &[],
    );
    assert!(
        VerdictStore::load(&dir).trusted_pure().is_empty(),
        "a purity verdict with no footprint has nothing keeping it honest"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An impure or unmeasured test is never trusted.
#[test]
fn only_measured_pure_tests_are_trusted() {
    let dir = temp("impure");
    seed(
        &dir,
        &[
            (
                "test_a.py::impure",
                record(&["src/a.py"], Some(false), false),
            ),
            ("test_a.py::unmeasured", record(&["src/a.py"], None, false)),
        ],
        &[("src/a.py", "VALUE = 1\n")],
    );
    assert!(VerdictStore::load(&dir).trusted_pure().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `must_fork` is honoured with no staleness guard — including when its dependencies changed.
///
/// The asymmetry, asserted directly. Forking a test that no longer needs it costs a fork; *not*
/// forking one that does is a wrong answer. Only one of those is worth guarding against, and
/// guarding this one would throw away the verdict exactly when it is most likely still true.
#[test]
fn must_fork_is_honoured_even_when_dependencies_changed() {
    let dir = temp("mustfork");
    seed(
        &dir,
        &[(
            "test_a.py::disturber",
            record(&["src/a.py"], Some(false), true),
        )],
        &[("src/a.py", "VALUE = 1\n")],
    );
    std::fs::write(dir.join("src/a.py"), "VALUE = 999\n").unwrap();

    let store = VerdictStore::load(&dir);
    assert!(
        store.must_fork().contains("test_a.py::disturber"),
        "a stale must-fork only costs a fork; dropping it risks a wrong answer"
    );
    assert!(
        store.trusted_pure().is_empty(),
        "...while the purity verdict in the same file is correctly revoked"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// No state file, or a truncated one, is a cold start rather than a failure.
///
/// Every verdict here is an optimisation. A run that refuses to start because a cache file got
/// half-written would be a far worse trade than one that simply runs cold.
#[test]
fn a_missing_or_corrupt_state_file_is_a_cold_start() {
    let dir = temp("cold");
    let store = VerdictStore::load(&dir);
    assert!(store.is_empty() && store.trusted_pure().is_empty() && store.must_fork().is_empty());

    std::fs::write(dir.join(STATE_FILE), "{ not json").unwrap();
    let store = VerdictStore::load(&dir);
    assert!(store.is_empty() && store.trusted_pure().is_empty() && store.must_fork().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
