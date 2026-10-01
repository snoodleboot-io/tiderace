//! TID-45 — the working directory is part of the state a test can disturb.
//!
//! The fingerprint taken around every in-process test covered `sys.path`, the environment, warnings
//! filters, logging and threads — but not `os.getcwd()`. The working directory is process-wide, and
//! every relative path in the next test resolves against it, so a test that chdirs and forgets moves
//! its neighbours' footing. Real suites do this: flask's own tests chdir through `monkeypatch.chdir`
//! and fixtures that build throwaway trees.
//!
//! The signature is a result that depends on the tier — green under `--no-optimistic`, red on the
//! default ladder — which is the worst kind of answer a runner can give, because it is not
//! reproducible by the person reading the report.
//!
//! Both tests must land in **one** worker for the leak to be observable at all: with a worker per
//! core they simply run in separate processes and the bug hides. That is why this drives a single
//! worker directly rather than going through the CLI.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ForkWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};

/// The chdir happens inside a helper: a test that calls `os.chdir` in its own body reads as impure to
/// the static scan and forks from the start, which is not the path this is about.
const CORPUS: &str = r#"
import os
import tempfile

ROOT = os.getcwd()


def _work_somewhere_else():
    os.chdir(tempfile.mkdtemp())


def test_a_wanders_off():
    _work_somewhere_else()
    assert os.getcwd() != ROOT
"#;

/// The next module on the same worker: the working directory is put back when the worker leaves
/// the module that moved it (TID-81), not after every test — inside a file, pytest would not put it
/// back either.
const NEXT_MODULE: &str = r#"
import os

ROOT = os.getcwd()


def test_b_expects_its_footing():
    assert os.getcwd() == ROOT, f"the previous module left this worker in {os.getcwd()}"
"#;

#[test]
fn a_test_that_chdirs_does_not_move_its_neighbours() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("chdir");
    std::fs::write(dir.join("test_a_chdir.py"), CORPUS).unwrap();
    std::fs::write(dir.join("test_b_footing.py"), NEXT_MODULE).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);

    // One worker, optimistic ladder: both tests share a process, which is the only arrangement in
    // which one can move the other.
    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-45: {} — the working directory must be restored between modules — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
