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
    ["python3", "python"]
        .into_iter()
        .find(|cand| {
            std::process::Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t45_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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


def test_b_expects_its_footing():
    assert os.getcwd() == ROOT, f"a neighbour left this worker in {os.getcwd()}"
"#;

#[test]
fn a_test_that_chdirs_does_not_move_its_neighbours() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("chdir");
    std::fs::write(dir.join("test_chdir.py"), CORPUS).unwrap();
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
            "TID-45: {} — the working directory must be restored between in-process tests — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
