//! TID-40 — every test in a module carries that module's import footprint, not just the first.
//!
//! A dependency footprint used to come only from per-test coverage, and a module's `import` lines
//! execute exactly **once** — for whichever test happens to run first. Every later test in that
//! module never re-executes them, so coverage never attributed the imported source to it. On a
//! twenty-test module the source under test appeared in one footprint out of twenty.
//!
//! That is not a missed optimisation. Impact selection re-runs a test when a recorded dependency
//! changed, so nineteen of twenty broken tests were served from cache and the daemon reported a
//! green suite that a full run reported as twenty failures. The same footprints also guard the
//! purity verdict, which decides whether a test skips isolation entirely.
//!
//! The fix is a static import closure: parsing sees every module's imports in any order, every
//! time, which is exactly what watching execution cannot do.
//!
//! These tests use **many tests per module on purpose**. At two tests per module every test happens
//! to carry the source dep and the defect is invisible — which is how it survived.

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

/// A suite where one source module is imported by a module holding many tests, plus a shared module
/// imported by everything, plus a conftest — the three kinds of dependency that runtime coverage
/// under-reports.
fn write_corpus(tag: &str, tests_per_module: usize) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t40_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();

    std::fs::write(
        dir.join("conftest.py"),
        "import os, sys\nsys.path.insert(0, os.path.dirname(__file__))\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/__init__.py"), "").unwrap();
    std::fs::write(
        dir.join("src/shared.py"),
        "def double(x):\n    return x * 2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/thing.py"),
        "from src.shared import double\n\n\ndef triple(x):\n    return double(x) + x\n",
    )
    .unwrap();

    let mut body = String::from("from src.thing import triple\n\n\n");
    for t in 0..tests_per_module {
        body.push_str(&format!(
            "def test_case_{t}():\n\x20   assert triple({t}) == {}\n\n\n",
            t * 3
        ));
    }
    std::fs::write(dir.join("tests/test_many.py"), body).unwrap();
    dir
}

/// Every test in the module records the source its module imports — not merely the first to run.
#[test]
fn every_test_in_a_module_carries_the_modules_imports() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("closure", 20);
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(
        items.len(),
        20,
        "20 tests in one module — the ratio matters"
    );

    // Coverage capture on, which is what produces footprints at all.
    std::env::set_var("TIDERACE_COVERAGE", "1");
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    std::env::remove_var("TIDERACE_COVERAGE");

    assert_eq!(results.len(), 20);
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}", r.detail);
    }

    // Each of the three under-reported kinds, for *every* test rather than for one of them.
    for want in ["src/thing.py", "src/shared.py", "conftest.py"] {
        let carrying = results
            .iter()
            .filter(|r| r.touched_files.iter().any(|f| f == want))
            .count();
        assert_eq!(
            carrying, 20,
            "TID-40: all 20 tests must record {want}; only {carrying} did. A module's imports run \
             once, so coverage alone credits them to whichever test ran first."
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The closure is transitive: `test_many` imports `src.thing`, which imports `src.shared`.
///
/// Asserted separately because a one-level fix would satisfy the test above for `src/thing.py` while
/// still missing everything reached through it — and the shared module is precisely the file whose
/// change should invalidate the whole suite.
#[test]
fn the_import_closure_is_transitive() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("transitive", 4);
    let items = RegexCollector::new().collect(&dir).expect("collection");

    std::env::set_var("TIDERACE_COVERAGE", "1");
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    std::env::remove_var("TIDERACE_COVERAGE");

    for r in &results {
        assert!(
            r.touched_files.iter().any(|f| f == "src/shared.py"),
            "TID-40: {} must reach src/shared.py through src/thing.py; got {:?}",
            r.node_id.as_str(),
            r.touched_files
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
