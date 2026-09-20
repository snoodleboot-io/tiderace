//! TID-51 — three gaps a real unittest-shaped suite exposed, and one bug in the suite itself.
//!
//! **A helper function did not close the class above it.** Scope was tracked with the regex that
//! matches *test-named* functions, so an ordinary `async def double(...)` at module level left the
//! preceding class open, and every indented `def test_*` after it was attributed to that class. The
//! nodes it produced cannot run — the methods are not on the class — and pytest never collected them
//! either. On one real file that was 11 phantom failures, and the `def test_*` lines in question sat
//! inside the helper's body after a `return`: dead code in the suite, which nobody had noticed
//! because neither runner had ever reported it.
//!
//! **`request.node` did not exist.** A fixture naming a resource after the test asking for it —
//! `f"db-{request.node.name}"` — got an `AttributeError`, and the test request carried the node *id
//! string* rather than an object. A marker added at runtime (`request.node.add_marker(xfail)`) has to
//! reach the outcome too, or a failure the author chose to tolerate is reported as a failure.
//!
//! **A unittest class whose base is named indirectly ran as a pytest class.** The source scan reads
//! bases as text, so `class TestThing(_MyBase)` hides an `IsolatedAsyncioTestCase`. Running it the
//! pytest way calls the method directly and never runs `setUp` / `asyncSetUp`, so every attribute the
//! setup assigned is missing. The shim holds the live class and can simply ask.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
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
        "tiderace_t51_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A helper between the class and the stray `def test_*`, exactly as the real file had it.
const SCOPE: &str = r#"
import unittest


class TestReal(unittest.TestCase):
    def test_a_real_method(self):
        self.assertTrue(True)


async def a_helper(x: int) -> int:
    return x * 2

    def test_that_is_dead_code(self):
        raise AssertionError("this can never run: it is inside a_helper, after its return")
"#;

const NODE: &str = r#"
import pytest


@pytest.fixture
def named(request):
    return f"resource-for-{request.node.name}"


@pytest.mark.parametrize("case", ["a", "b"])
def test_a_fixture_sees_the_variants_own_name(named, case):
    # The parametrize id has to be part of it, or every case would name the same resource.
    assert named == f"resource-for-test_a_fixture_sees_the_variants_own_name[{case}]"


def test_the_test_request_carries_a_node_object(request):
    assert request.node.name == "test_the_test_request_carries_a_node_object"
    assert request.node.nodeid.endswith("::test_the_test_request_carries_a_node_object")


@pytest.fixture
def declares_it_broken(request):
    request.node.add_marker(pytest.mark.xfail(reason="known broken"))


def test_a_runtime_xfail_is_honoured(declares_it_broken):
    raise AssertionError("the author expects this to be tolerated")
"#;

/// The base is named indirectly, so the source scan cannot see the `TestCase` in it.
const INDIRECT_BASE: &str = r#"
import unittest


class _Base(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.prepared = "set up asynchronously"


class TestLooksLikeAPytestClass(_Base):
    async def test_async_setup_ran(self):
        # Without asyncSetUp this is an AttributeError naming this class.
        self.assertEqual(self.prepared, "set up asynchronously")
"#;

#[test]
fn a_helper_between_class_and_function_closes_the_class() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("scope");
    std::fs::write(dir.join("test_scope.py"), SCOPE).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    let ids: Vec<&str> = items.iter().map(|i| i.node_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["test_scope.py::TestReal::test_a_real_method"],
        "TID-51: a `def test_*` nested inside a helper is not a test — pytest does not collect it, \
         and a node for it can only ever fail"
    );

    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");
    assert_eq!(results[0].outcome, Outcome::Passed, "{}", results[0].detail);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn request_node_names_the_variant_and_carries_runtime_markers() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("node");
    std::fs::write(dir.join("test_node.py"), NODE).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    for r in &results {
        let expected = if r.node_id.as_str().contains("runtime_xfail") {
            Outcome::XFail
        } else {
            Outcome::Passed
        };
        assert_eq!(
            r.outcome,
            expected,
            "TID-51: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_unittest_class_behind_an_indirect_base_still_gets_its_setup() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("indirect_base");
    std::fs::write(dir.join("test_indirect_base.py"), INDIRECT_BASE).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    let test = results
        .iter()
        .find(|r| r.node_id.as_str().contains("test_async_setup_ran"))
        .expect("the test reports");
    assert_eq!(
        test.outcome,
        Outcome::Passed,
        "TID-51: the base is `unittest.IsolatedAsyncioTestCase` by inheritance, so `asyncSetUp` has \
         to run — {}",
        test.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}
