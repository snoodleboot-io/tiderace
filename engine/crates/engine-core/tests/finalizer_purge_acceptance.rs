//! TID-56 — a fixture that hands the test a callable still gets its finalizers run.
//!
//! Two defects with one symptom, both found by chasing flask's last tier-dependent failures.
//!
//! **Finalizers registered from the test body were dropped.** The teardown handle was wrapped only if
//! the fixture had registered a finalizer *during its own body*. flask's `purge_module` returns a
//! callable and registers nothing until the test calls it — so the decision was taken while the list
//! was still empty, and every finalizer added later went unrun. The module a test asked to have
//! purged stayed in `sys.modules` for its neighbours.
//!
//! **Restore reinstated modules a test had deleted.** `_restore_modules` put back any name whose
//! binding differed from the snapshot — and an absent name differs. A deliberate removal was undone.
//! Left in place, that would have hidden the fix above.
//!
//! The corpus is flask's shape reduced: a module written into a per-test temporary directory, a
//! fixture that purges it at teardown, and a second test that must import its own copy. If the first
//! test's module survives, the second sees the first test's directory — which is exactly how this
//! surfaced, as an assertion naming two different tmp paths.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ForkWorker, Worker};
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

fn python_with_pytest() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut candidates: Vec<String> = Vec::new();
    if venv.exists() {
        candidates.push(venv.to_string_lossy().into_owned());
    }
    candidates.extend(["python3".to_string(), "python".to_string()]);
    candidates.into_iter().find(|p| {
        std::process::Command::new(p)
            .args(["-c", "import pytest"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t56_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CONFTEST: &str = r#"
import sys
import tempfile
from pathlib import Path

import pytest


@pytest.fixture
def purge_module(request):
    """Hands the test a callable. Nothing is registered until the test calls it — the shape that was
    losing its finalizers."""

    def inner(name):
        request.addfinalizer(lambda: sys.modules.pop(name, None))

    return inner


@pytest.fixture
def module_dir(request):
    """A per-test directory on sys.path, holding a module that reports which directory it came from."""
    path = Path(tempfile.mkdtemp())
    (path / "throwaway.py").write_text(f"HOME = {str(path)!r}\n")
    sys.path.insert(0, str(path))
    request.addfinalizer(lambda: sys.path.remove(str(path)))
    return path
"#;

const CORPUS: &str = r#"
def test_first_imports_its_own_copy(module_dir, purge_module):
    purge_module("throwaway")
    import throwaway

    assert throwaway.HOME == str(module_dir)


def test_second_must_not_inherit_the_first_ones(module_dir, purge_module):
    purge_module("throwaway")
    import throwaway

    assert throwaway.HOME == str(module_dir), (
        "got the previous test's module: its finalizer never ran, or restore put it back"
    )
"#;

#[test]
fn a_finalizer_registered_from_the_test_body_runs_at_teardown() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("purge");
    std::fs::write(dir.join("conftest.py"), CONFTEST).unwrap();
    std::fs::write(dir.join("test_purge.py"), CORPUS).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);

    // One worker on the optimistic ladder: the tests must share a process for one to inherit the
    // other's `sys.modules`.
    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-56: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
