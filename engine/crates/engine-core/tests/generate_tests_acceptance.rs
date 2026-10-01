//! TID-85 — `pytest_generate_tests(metafunc)`: parametrization declared above the test.
//!
//! A conftest or a test module can parametrize the tests below it by calling
//! `metafunc.parametrize(...)` from a `pytest_generate_tests` hook — the third way pytest
//! parametrizes, beside the decorator mark and `@pytest.fixture(params=...)`. The engine now calls
//! the hooks that govern a test (its module's own, then its conftests deepest to root) and treats
//! each call as an axis exactly as a decorator mark would, in pytest's order: hook axes before mark
//! axes, so `test_x[sqlite-1]` for a conftest's `backend` and the function's own `n`.
//!
//! The expected node ids below are pytest's own, taken from `pytest --collect-only` on the same
//! corpus (pytest 8, moto scratch venv), so a divergence here is a divergence from pytest.
//!
//! The corpus imports pytest, so the live half gates on unix like its siblings.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use std::collections::BTreeSet;
use std::path::PathBuf;

const ROOT_CONFTEST: &str = r#"import pytest

def pytest_generate_tests(metafunc):
    if "backend" in metafunc.fixturenames:
        metafunc.parametrize("backend", ["sqlite", "postgres"])
    if "routed" in metafunc.fixturenames:
        metafunc.parametrize("routed", [10, 20], indirect=True)

@pytest.fixture
def routed(request):
    return request.param * 2
"#;

const SUB_CONFTEST: &str = r#"def pytest_generate_tests(metafunc):
    if "region" in metafunc.fixturenames:
        metafunc.parametrize("region", ["eu", "us"], ids=["EU", "US"])
"#;

const MODULE: &str = r#"import pytest

def pytest_generate_tests(metafunc):
    if "mod" in metafunc.fixturenames:
        metafunc.parametrize("mod", ["m1"])

def test_backend(backend):
    assert backend in ("sqlite", "postgres")

@pytest.mark.parametrize("n", [1, 2])
def test_backend_and_mark(backend, n):
    assert n in (1, 2)

def test_region_and_backend(region, backend):
    assert region and backend

def test_indirect(routed):
    assert routed in (20, 40)

@pytest.mark.parametrize("n", [1])
def test_all_three(mod, region, n):
    assert mod == "m1"

class TestCls:
    def test_method(self, backend):
        assert backend
"#;

/// A hook that parametrizes a name the test does not take: pytest refuses it at that test.
const BAD_MODULE: &str = r#"def pytest_generate_tests(metafunc):
    metafunc.parametrize("nobody", [1])

def test_plain():
    assert True
"#;

fn write_corpus(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t85_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("conftest.py"), ROOT_CONFTEST).unwrap();
    std::fs::write(dir.join("sub/conftest.py"), SUB_CONFTEST).unwrap();
    std::fs::write(dir.join("sub/test_gen.py"), MODULE).unwrap();
    dir
}

#[test]
fn hooks_in_the_module_and_its_conftests_parametrize_in_pytests_order() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("order");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 6, "six collected nodes before expansion");
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    let _ = std::fs::remove_dir_all(&dir);

    let want: BTreeSet<&str> = [
        "sub/test_gen.py::test_backend[sqlite]",
        "sub/test_gen.py::test_backend[postgres]",
        "sub/test_gen.py::test_backend_and_mark[sqlite-1]",
        "sub/test_gen.py::test_backend_and_mark[sqlite-2]",
        "sub/test_gen.py::test_backend_and_mark[postgres-1]",
        "sub/test_gen.py::test_backend_and_mark[postgres-2]",
        "sub/test_gen.py::test_region_and_backend[EU-sqlite]",
        "sub/test_gen.py::test_region_and_backend[EU-postgres]",
        "sub/test_gen.py::test_region_and_backend[US-sqlite]",
        "sub/test_gen.py::test_region_and_backend[US-postgres]",
        "sub/test_gen.py::test_indirect[10]",
        "sub/test_gen.py::test_indirect[20]",
        "sub/test_gen.py::test_all_three[m1-EU-1]",
        "sub/test_gen.py::test_all_three[m1-US-1]",
        "sub/test_gen.py::TestCls::test_method[sqlite]",
        "sub/test_gen.py::TestCls::test_method[postgres]",
    ]
    .into_iter()
    .collect();
    // Only the expanded variants report; they are what pytest collects.
    let variants: BTreeSet<&str> = results
        .iter()
        .filter(|r| r.expanded)
        .map(|r| r.node_id.as_str())
        .collect();
    assert_eq!(
        variants, want,
        "TID-85: the expanded node ids must be pytest's, in pytest's axis order"
    );
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
    }
}

#[test]
fn a_hook_parametrizing_a_name_the_test_does_not_take_is_that_tests_error() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t85_bad_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_bad.py"), BAD_MODULE).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 1);
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, Outcome::Error, "{}", results[0].detail);
    assert!(
        results[0].detail.contains("uses no argument 'nobody'"),
        "pytest's wording: {}",
        results[0].detail
    );
}
