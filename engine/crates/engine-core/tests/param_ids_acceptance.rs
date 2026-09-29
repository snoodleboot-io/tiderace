//! TID-86 — node ids are pytest's: a `pytest.param(id=)` inside a fixture's `params` names the
//! case (and `request.param` is the value, not the ParameterSet), and a value with no string form
//! is numbered by its position in its own axis, not by the case's position across the product.
//!
//! The expected ids are pytest's own, from `--collect-only` on this corpus. The corpus imports
//! pytest, so this gates on unix like its siblings.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::skip_live;
use std::collections::BTreeSet;
use std::path::PathBuf;

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

const CORPUS: &str = r#"import pytest

@pytest.fixture(params=[pytest.param(("asyncio", {}), id="asyncio"), pytest.param(("trio", {}), id="trio")])
def backend(request):
    return request.param[0]

@pytest.fixture(params=[[1], [2]])
def bucket(request):
    return request.param

def test_backend(backend):
    assert backend in ("asyncio", "trio")

def test_bucket_and_backend(bucket, backend):
    assert bucket and backend

@pytest.mark.parametrize("default", [True, False])
@pytest.mark.parametrize(("args", "expect"), [(["--f"], True), ([], False)])
def test_boolean_flag(default, args, expect):
    assert isinstance(default, bool)
"#;

#[test]
fn fixture_param_ids_and_axis_positions_are_pytests() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t86_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_ids.py"), CORPUS).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3);
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    let _ = std::fs::remove_dir_all(&dir);

    let want: BTreeSet<&str> = [
        "test_ids.py::test_backend[asyncio]",
        "test_ids.py::test_backend[trio]",
        "test_ids.py::test_bucket_and_backend[bucket0-asyncio]",
        "test_ids.py::test_bucket_and_backend[bucket0-trio]",
        "test_ids.py::test_bucket_and_backend[bucket1-asyncio]",
        "test_ids.py::test_bucket_and_backend[bucket1-trio]",
        "test_ids.py::test_boolean_flag[args0-True-True]",
        "test_ids.py::test_boolean_flag[args0-True-False]",
        "test_ids.py::test_boolean_flag[args1-False-True]",
        "test_ids.py::test_boolean_flag[args1-False-False]",
    ]
    .into_iter()
    .collect();
    let got: BTreeSet<&str> = results.iter().map(|r| r.node_id.as_str()).collect();
    assert_eq!(got, want, "TID-86: pytest's ids exactly");
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "{}: {} — `request.param` must be the value pytest.param wraps, not the ParameterSet",
            r.node_id,
            r.detail
        );
    }
}

/// Bytes ids, `@pytest.mark.usefixtures`, an indirect parametrize of a params fixture, and a
/// class-scoped fixture spelling its instance `cls` and asking for `tmp_path_factory`.
const MORE: &str = r#"import pytest

REGISTRY = {}


@pytest.fixture(params=[pytest.param(("asyncio", {}), id="asyncio"), pytest.param(("trio", {}), id="trio")])
def backend(request):
    return request.param[0]


@pytest.fixture
def _restore_registry():
    saved = dict(REGISTRY)
    REGISTRY.clear()
    yield
    REGISTRY.clear()
    REGISTRY.update(saved)


@pytest.mark.parametrize(("value", "expect"), [(123, b"\x1b[45m123\x1b[0m"), (b"test", b"test")])
def test_bytes_ids(value, expect):
    assert expect


@pytest.mark.usefixtures("_restore_registry")
def test_registers_under_the_fixture():
    REGISTRY["mine"] = object()
    assert "mine" in REGISTRY


def test_registry_is_clean_afterwards():
    assert "mine" not in REGISTRY


@pytest.mark.parametrize("backend", [("asyncio", {})], indirect=True)
def test_indirect_backend_is_one_case(backend):
    assert backend == "asyncio"


class TestFactory:
    @pytest.fixture(scope="class")
    def shared_dir(cls, tmp_path_factory):
        return tmp_path_factory.mktemp("shared")

    def test_dir_exists(self, shared_dir):
        assert shared_dir.is_dir()

    def test_same_dir(self, shared_dir):
        assert shared_dir.name.startswith("shared")
"#;

#[test]
fn bytes_ids_usefixtures_indirect_fixture_params_and_tmp_path_factory_match_pytest() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t86_more_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_more.py"), MORE).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 6);
    // `tmp_path_factory` is a builtin provider, which the shim finds through the `tiderace`
    // package: put it on the path the way an installed wheel would.
    std::env::set_var("PYTHONPATH", repo_root().join("engine/py-tiderace"));
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    std::env::remove_var("PYTHONPATH");
    let _ = std::fs::remove_dir_all(&dir);

    let want: BTreeSet<&str> = [
        r"test_more.py::test_bytes_ids[123-\x1b[45m123\x1b[0m]",
        "test_more.py::test_bytes_ids[test-test]",
        "test_more.py::test_registers_under_the_fixture",
        "test_more.py::test_registry_is_clean_afterwards",
        "test_more.py::test_indirect_backend_is_one_case[backend0]",
        "test_more.py::TestFactory::test_dir_exists",
        "test_more.py::TestFactory::test_same_dir",
    ]
    .into_iter()
    .collect();
    let got: BTreeSet<&str> = results.iter().map(|r| r.node_id.as_str()).collect();
    assert_eq!(got, want, "TID-86: pytest's ids exactly");
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
    }
}
