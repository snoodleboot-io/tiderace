//! TID-33 — a test that disturbs interpreter state is caught, undone, and demoted to forking.
//!
//! The in-process ladder's snapshot/restore covers what it was told to cover: module globals,
//! `os.environ`, `sys.modules`, and the object graph reachable from them. Everything else in a
//! CPython process is invisible to it — `sys.path`, the warnings filters, the root logger's handlers
//! and level, a thread left running. A test that moves one of those leaks into every test that
//! follows it in the same worker, and the failure surfaces arbitrarily far from the cause. Under
//! fork it never reproduces, which is the worst shape a bug can have: `--optimistic` goes red and
//! the default goes green, so the ladder looks unsound when what is actually unsound is one test.
//!
//! The fix is a fingerprint rather than a longer list of categories. Enumerating state is a losing
//! game — the next leak is always the one nobody modelled — so the shim samples a handful of cheap
//! summary values around each in-process body and compares. That answers *whether* something moved
//! without needing to know what could move, and the key that moved is enough to act:
//!
//!   1. restore it, where the category is restorable, so the neighbours are not poisoned;
//!   2. re-run the offender in a fork, so the run that found the problem also reports the right
//!      answer rather than only teaching the next run;
//!   3. mark the node impure, so it stops taking the in-process path.
//!
//! Step 2 is the one that matters for CI. Detection alone converges — the second run is clean — but
//! the first run is exactly the one where somebody is staring at a red build.
//!
//! What is *not* claimed: a leaked thread cannot be unwound. That is detected and demoted, and the
//! offender's own result is made correct, but a neighbour that counts threads will still see it.
//! `test_g` below pins the honest boundary rather than papering over it.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::skip_live;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

fn any_python() -> Option<String> {
    for cand in ["python3", "python"] {
        let ok = std::process::Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

/// Each `_a`/`_c`/`_e` test leaks; its `_b`/`_d`/`_f` sibling runs next (the collector sorts by node
/// id) and fails if the leak survived. The baselines are captured at import, which on the in-process
/// ladder happens once in the pristine parent — so they are the values a fresh interpreter would
/// have, not whatever the previous test left behind.
///
/// `test_g` leaks a thread, which nothing can undo; it only has to be *detected*. `test_h` is
/// plainly pure and guards the other direction: a fingerprint that fired on everything would demote
/// every test and quietly turn the ladder back into fork-per-test.
const CORPUS: &str = "\
import logging
import sys
import threading

PATH_AT_IMPORT = list(sys.path)
HANDLERS_AT_IMPORT = len(logging.getLogger().handlers)
LEVEL_AT_IMPORT = logging.getLogger().level


def test_a_pollutes_sys_path():
    sys.path.insert(0, \"/tmp/tiderace-not-a-real-path\")
    assert sys.path[0] == \"/tmp/tiderace-not-a-real-path\"


def test_b_sys_path_is_clean():
    extra = [p for p in sys.path if p not in PATH_AT_IMPORT]
    assert not extra, f\"sys.path leaked: {extra}\"


def test_c_leaves_a_logging_handler():
    logging.getLogger().addHandler(logging.NullHandler())
    assert True


def test_d_no_handler_leaked():
    n = len(logging.getLogger().handlers)
    assert n == HANDLERS_AT_IMPORT, f\"leaked {n - HANDLERS_AT_IMPORT} handler(s)\"


def test_e_raises_the_root_log_level():
    logging.getLogger().setLevel(logging.DEBUG)
    assert logging.getLogger().level == logging.DEBUG


def test_f_log_level_restored():
    level = logging.getLogger().level
    assert level == LEVEL_AT_IMPORT, f\"root level leaked: {level} != {LEVEL_AT_IMPORT}\"


def test_g_leaks_a_thread():
    # Never set, so the thread outlives the test. Daemon, so it cannot wedge interpreter shutdown.
    threading.Thread(target=threading.Event().wait, daemon=True).start()
    assert True


def test_h_is_pure():
    assert sum(range(10)) == 45
";

/// A leak whose *own* result differs between the two ways of running it — the only direct evidence,
/// from outside the process, that the offender was re-run in a fork rather than merely flagged.
/// In-process the pid is the wellspring's and the body fails; in a forked child it differs and the
/// body passes. Reported as passed ⇒ the re-run happened and its result is the one that was kept.
const FORK_PROOF: &str = "\
import os
import sys

PID_AT_IMPORT = os.getpid()


def test_leak_forces_a_fork():
    sys.path.insert(0, \"/tmp/tiderace-fork-proof\")
    assert os.getpid() != PID_AT_IMPORT, \"ran in the wellspring: the leak did not force a fork\"
";

fn write_corpus(tag: &str, body: &str, file: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_fprint_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), body).unwrap();
    dir
}

fn get<'a>(results: &'a [TestResult], leaf: &str) -> &'a TestResult {
    results
        .iter()
        .find(|r| r.node_id.as_str().ends_with(leaf))
        .unwrap_or_else(|| panic!("{leaf} was reported"))
}

/// The neighbours pass (the restore undid the leak) and the leakers are marked impure (so they stop
/// taking this path), while the pure test is left alone.
fn assert_detected_and_restored(results: &[TestResult], tier: &str) {
    for leaf in [
        "test_b_sys_path_is_clean",
        "test_d_no_handler_leaked",
        "test_f_log_level_restored",
    ] {
        let r = get(results, leaf);
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-33/{tier}: {leaf} saw the previous test's leak — {}",
            r.detail
        );
    }
    for leaf in [
        "test_a_pollutes_sys_path",
        "test_c_leaves_a_logging_handler",
        "test_e_raises_the_root_log_level",
        "test_g_leaks_a_thread",
    ] {
        assert_eq!(
            get(results, leaf).pure,
            Some(false),
            "TID-33/{tier}: {leaf} disturbed interpreter state and must be recorded impure"
        );
    }
    // The other direction: if the fingerprint fired on an ordinary test, every test would be
    // demoted to forking and the ladder would be pure overhead.
    let pure = get(results, "test_h_is_pure");
    assert_eq!(pure.outcome, Outcome::Passed, "detail: {}", pure.detail);
    assert_ne!(
        pure.pure,
        Some(false),
        "TID-33/{tier}: a test that touches nothing must not be flagged as disturbing state"
    );
}

/// `--strategy subprocess`: one process for the whole batch, restore as the only isolation. This is
/// what every non-Unix run uses, and there is no fork to fall back to — so restore-and-demote is the
/// entire remedy here, and the leakers' own results are not re-run.
#[test]
fn leaks_are_restored_and_demoted_on_the_no_fork_tier() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("nofork", CORPUS, "test_leaks.py");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 8, "8 tests in the corpus");

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs against real Python");
    assert_eq!(results.len(), 8, "one result per test");
    assert_detected_and_restored(&results, "subprocess");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The optimistic ladder — the configuration this exists for. Same guarantees, plus the offender's
/// own result comes from a fork.
#[cfg(unix)]
#[test]
fn leaks_are_restored_and_demoted_on_the_optimistic_ladder() {
    use engine_core::exec::ForkWorker;

    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("optimistic", CORPUS, "test_leaks.py");
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");
    assert_eq!(results.len(), 8, "one result per test");
    assert_detected_and_restored(&results, "optimistic");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The step that fixes the *current* run rather than the next one: a test caught disturbing state is
/// re-run in a fork and the forked result is the one reported.
#[cfg(unix)]
#[test]
fn the_offender_is_re_run_in_a_fork() {
    use engine_core::exec::ForkWorker;

    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("rerun", FORK_PROOF, "test_rerun.py");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 1);

    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");
    let r = &results[0];
    assert_eq!(
        r.outcome,
        Outcome::Passed,
        "TID-33: the leaking test must be re-run forked and the forked result kept — {}",
        r.detail
    );
    assert_eq!(r.pure, Some(false), "and it must still be recorded impure");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same corpus under plain fork, where each test gets a pristine COW child. If it did not pass
/// here, the corpus would be asserting something about restore that is not true of the engine.
#[cfg(unix)]
#[test]
fn the_same_corpus_passes_under_fork() {
    use engine_core::exec::ForkWorker;

    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("fork", CORPUS, "test_leaks.py");
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let results = ForkWorker::launch(&python, &shim(), &dir)
        .expect("wellspring")
        .run(&items)
        .expect("fork batch runs");
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "under fork every test is pristine: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
