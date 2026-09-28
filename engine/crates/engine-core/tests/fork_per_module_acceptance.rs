//! TID-80 — an opaque module's tests run in one forked child, so state a test leaves in a wider-scoped
//! fixture is there for the next test, as under pytest; other modules still never see it.
//!
//! The fork tier used to fork once per test. That kept a module's own tests apart from each other,
//! which pytest never does: an object one test put into a module-scoped moto mock was gone for the
//! next, because it lived and died in that test's child. Now the child is the boundary *between*
//! modules — which is what an opaque global (a lock, a client, a socket) makes necessary — and inside
//! it the file behaves as it would under pytest. A child that dies mid-module is reported on the test
//! that killed it, and the module's remaining tests run in a fresh one.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
use engine_core::exec::{ForkWorker, SubprocessWorker, Worker};
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
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
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

/// A module-level lock cannot be deep-copied, so the module is opaque and takes the fork tier.
/// Its tests hand each other state through a module-scoped fixture, and one of them leaks into
/// the environment on purpose.
const SHARING: &str = r#"import os
import threading
import pytest

LOCK = threading.Lock()


@pytest.fixture(scope="module")
def store():
    return {}


def test_a_puts(store):
    store["k"] = 1
    os.environ["T80_LEAK"] = "from test_a_puts"


def test_b_sees_it(store):
    assert store.get("k") == 1, f"the previous test's write is gone: {store}"


def test_c_still_sees_it(store):
    assert store.get("k") == 1, f"the previous tests' write is gone: {store}"
    assert os.environ.get("T80_LEAK") == "from test_a_puts", "the file runs as one process"
"#;

/// Another opaque module: what the first one leaked must not reach it.
const ISOLATED: &str = r#"import os
import threading

LOCK = threading.Lock()


def test_not_polluted_by_the_other_module():
    assert "T80_LEAK" not in os.environ, "another module's child leaked into this one"
"#;

/// A test that kills the child mid-module: reported on that test, and the rest still run.
const CRASHING: &str = r#"import os
import threading
import pytest

LOCK = threading.Lock()


@pytest.fixture(scope="module")
def store():
    return {"alive": True}


def test_a_ok(store):
    assert store["alive"]


def test_b_kills_the_process(store):
    os._exit(3)


def test_c_runs_after_the_crash(store):
    assert store["alive"]
"#;

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t80_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_a_sharing.py"), SHARING).unwrap();
    std::fs::write(dir.join("test_b_isolated.py"), ISOLATED).unwrap();
    std::fs::write(dir.join("test_c_crashing.py"), CRASHING).unwrap();
    dir
}

fn get<'a>(results: &'a [TestResult], leaf: &str) -> &'a TestResult {
    results
        .iter()
        .find(|r| r.node_id.as_str().ends_with(leaf))
        .unwrap_or_else(|| panic!("{leaf} was reported"))
}

fn check(results: &[TestResult], tier: &str) {
    assert_eq!(results.len(), 7, "{tier}: one result per test");
    for leaf in [
        "test_a_puts",
        "test_b_sees_it",
        "test_c_still_sees_it",
        "test_not_polluted_by_the_other_module",
        "test_a_ok",
        "test_c_runs_after_the_crash",
    ] {
        let r = get(results, leaf);
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-80/{tier}: {leaf}: {}",
            r.detail
        );
    }
    let crash = get(results, "test_b_kills_the_process");
    assert_eq!(
        crash.outcome,
        Outcome::Error,
        "{tier}: the crash is reported on its test"
    );
    assert!(
        crash.detail.contains("exited 3"),
        "{tier}: the detail names how the child died: {}",
        crash.detail
    );
}

/// `--no-fork` + restore: the opaque modules take the fork tier from the no-fork worker.
#[test]
fn an_opaque_modules_tests_share_one_child_on_the_no_fork_worker() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("nofork");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    let _ = std::fs::remove_dir_all(&dir);
    check(&results, "no-fork");
}

/// The optimistic ladder — the default tier — routes the same way.
#[test]
fn an_opaque_modules_tests_share_one_child_on_the_optimistic_ladder() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("optimistic");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");
    let _ = std::fs::remove_dir_all(&dir);
    check(&results, "optimistic");
}
