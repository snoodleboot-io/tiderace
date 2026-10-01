//! TID-68 — a container a test adds to an already-imported library module is watched from the next
//! test on.
//!
//! The registry-leak scan (TID-46) is cached per test module and was redone only when `sys.modules`
//! had grown — "the only way a new one can appear", said its docstring. It is not. A test or fixture
//! that does `lib.LATE = {}` on a module that already exists adds a container without importing
//! anything, and under the old key that container was never scanned: the next test's addition to it
//! was never rolled back, so the test after that ran in a world its neighbour had changed.
//!
//! The cache now also checks, per watched module, that it is still the same object and that its
//! namespace has not grown — one identity check and one `len` for each package the test file imports.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ForkWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};

/// A library with one registry at import time and one it creates **lazily**, on first use — the
/// shape of PIL registering a codec when a plugin loads, or a framework building its app table on
/// first request. The scan runs before any test and never sees `LATE`.
const LIBRARY: &str = r#"
REGISTRY = {}


def registry():
    """The lazy one. Library code creates it, so no test depends on another having run first."""
    global LATE
    try:
        return LATE
    except NameError:
        LATE = {}
        return LATE
"#;

/// Three tests, in file order, on one worker. `test_a` makes the library create the container;
/// `test_b` puts its own class in it; `test_c` must not see that class. Nothing here depends on
/// order — each test would pass alone — but under the old cache key nothing happened between the
/// scan and `test_b` that `len(sys.modules)` could notice, so `LATE` was never a target and
/// `test_b`'s entry was never rolled back.
const CORPUS: &str = r#"
import fakelib


def test_a_makes_the_library_create_a_registry_the_scan_never_saw():
    assert fakelib.registry() == {}


def test_b_registers_its_own_class_in_it():
    class Mine:
        pass

    fakelib.registry()["mine"] = Mine
    assert "mine" in fakelib.registry()
"#;

/// The next module on the same worker (TID-81): a container the library created *during* the
/// previous module was never in that module's entry snapshot, so leaving the module has to find
/// it and pull the suite's entries out — the same rescan TID-68 required per test.
const NEXT_MODULE: &str = r#"
import fakelib


def test_c_sees_it_clean():
    assert "mine" not in fakelib.registry(), (
        "TID-68: a container the library created during test_a was never scanned, so test_b's "
        "registration in it leaked into this test"
    )
"#;

#[test]
fn a_container_added_to_an_existing_module_is_watched_from_the_next_test_on() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("late");
    std::fs::create_dir_all(dir.join("fakelib")).unwrap();
    std::fs::write(dir.join("fakelib/__init__.py"), LIBRARY).unwrap();
    std::fs::write(dir.join("test_late.py"), CORPUS).unwrap();
    std::fs::write(dir.join("test_late_next.py"), NEXT_MODULE).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3);

    // One worker on the optimistic ladder: the tests must share a process for one to pollute the
    // next, which is the only arrangement in which this bug exists.
    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-68: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
