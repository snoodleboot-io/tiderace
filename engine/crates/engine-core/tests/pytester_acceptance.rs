//! TID-105 — `pytester` and `testdir`, pytest's fixtures for testing pytest plugins, run under
//! tiderace: a test writes a throwaway pytest project and runs real pytest in it, in-process and
//! as a subprocess, and asserts on the result; the worker is intact afterwards.
//!
//! Driven against the fx venv's interpreter (pytest 9) with the no-fork worker, so the inner
//! pytest session's effect on the worker — `sys.modules`, `sys.path`, the cwd — is what the next
//! test in the same process sees, and must not.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};

const CORPUS: &str = "\
import os
import sys

pytest_plugins = [\"pytester\"]


def test_a_runpytest_in_process(pytester):
    pytester.makepyfile(\"def test_inner():\\n    assert 1 + 1 == 2\\n\")
    result = pytester.runpytest(\"-q\")
    result.assert_outcomes(passed=1)


def test_b_legacy_testdir(testdir):
    testdir.makepyfile(\"def test_inner():\\n    assert False\\n\")
    result = testdir.runpytest(\"-q\")
    result.assert_outcomes(failed=1)
    assert result.ret == 1


def test_c_conftest_and_subprocess(pytester):
    pytester.makeconftest(
        \"import pytest\\n\\n@pytest.fixture\\ndef answer():\\n    return 42\\n\"
    )
    pytester.makepyfile(\"def test_uses(answer):\\n    assert answer == 42\\n\")
    result = pytester.runpytest_subprocess(\"-q\")
    result.assert_outcomes(passed=1)


def test_d_the_worker_is_intact_afterwards():
    # The inner sessions imported the throwaway modules and chdir'd into their directories;
    # Pytester's own finalizer puts sys.modules and sys.path back, and the monkeypatch the cwd.
    assert \"test_inner\" not in sys.modules
    assert not os.path.basename(os.getcwd()).startswith(\"pytester\")
";

#[test]
fn pytester_and_testdir_run_an_inner_pytest_and_leave_the_worker_intact() {
    let Some(python) = python(PythonNeeds::PytestAndTiderace) else {
        skip_live("no interpreter with both pytest and tiderace importable");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t105_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_plugin_suite.py"), CORPUS).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 4);
    let mut worker = SubprocessWorker::new(120_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("the batch runs");
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-105: {} — {}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
