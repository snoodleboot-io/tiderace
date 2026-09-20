//! TID-58 — `indirect=` routes a parametrized value to the fixture, not to the test.
//!
//! ```python
//! @pytest.mark.parametrize("store", [_memory, _disk], indirect=True)
//! def test_round_trip(store):        # `store` is what the FIXTURE returned
//!     store.put(...)
//! ```
//!
//! The value goes to the fixture named `store` as `request.param`; the fixture builds something from
//! it; the test never sees the raw value. Handing the raw value to the test instead is not a near
//! miss — it is usually the fixture *function object*, and the test dies on the first attribute it
//! touches (`'function' object has no attribute 'put'`).
//!
//! The machinery was already there: a fixture declared with `params=[...]` runs once per param with
//! `request.param` set, and the per-fixture param map is what the engine calls a `combo`. Indirect
//! parametrisation is the same thing with the params supplied at the call site, so an indirect value
//! simply joins that map — and has to be kept out of the test's own kwargs, or the raw value would
//! shadow what the fixture returned.

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
        "tiderace_t58_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CORPUS: &str = r#"
import pytest


@pytest.fixture
def store(request):
    return f"built from {request.param}"


@pytest.mark.parametrize("store", ["memory", "disk"], indirect=True)
def test_the_fixture_gets_the_value(store):
    assert store in ("built from memory", "built from disk")


@pytest.fixture
def doubled(request):
    return request.param * 2


@pytest.mark.parametrize("doubled,plain", [(3, "a"), (4, "b")], indirect=["doubled"])
def test_one_indirect_one_direct(doubled, plain):
    # `doubled` came through its fixture; `plain` is the literal from the parametrize.
    assert (doubled, plain) in ((6, "a"), (8, "b"))


@pytest.fixture
def untouched():
    return "a plain fixture"


@pytest.mark.parametrize("value", ["x", "y"])
def test_direct_parametrize_still_reaches_the_test(value, untouched):
    assert value in ("x", "y")
    assert untouched == "a plain fixture"
"#;

#[test]
fn an_indirect_value_reaches_the_fixture_and_the_test_gets_its_result() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("indirect");
    std::fs::write(dir.join("test_indirect.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    let mut ids: Vec<&str> = results.iter().map(|r| r.node_id.as_str()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "test_indirect.py::test_direct_parametrize_still_reaches_the_test[x]",
            "test_indirect.py::test_direct_parametrize_still_reaches_the_test[y]",
            "test_indirect.py::test_one_indirect_one_direct[3-a]",
            "test_indirect.py::test_one_indirect_one_direct[4-b]",
            "test_indirect.py::test_the_fixture_gets_the_value[disk]",
            "test_indirect.py::test_the_fixture_gets_the_value[memory]",
        ],
        "ids come from the parametrize either way — an indirect case is still named by its value"
    );
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-58: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
