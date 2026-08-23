//! TID-34 — a `pytest.ini` marks a rootdir, and a fixture that fails errors its test.
//!
//! Two defects that together made the repo's own fixture corpus unrunnable by the repo's own CLI:
//! `tiderace run benchmarks/fixtures/fx_corpus/tests` died with a traceback on stderr, exit 101, and
//! not one result.
//!
//! **`pytest.ini` was not a rootdir marker.** TID-19 collects ancestor `conftest.py` files from the
//! rootdir down, bounded by the nearest ancestor holding a project marker — and the marker list was
//! `pyproject.toml`, `setup.cfg`, `tox.ini`, `setup.py`. `pytest.ini` is *first* in pytest's own
//! precedence, an explicit statement of where the project root is, and it was missing. A suite laid
//! out the conventional way — `pytest.ini` and a suite-wide `conftest.py` above the test directory —
//! therefore found no rootdir at all: the ancestor walk stopped immediately and every session
//! fixture in that conftest silently did not exist.
//!
//! **A fixture that could not be set up killed the worker.** The exception escaped `run()`, so every
//! *other* test on that worker was lost too and the run reported `shim closed mid-run` — naming the
//! transport rather than the fixture. pytest errors that one test and carries on. This is the same
//! lesson as TID-15, one level up: a failure must report itself rather than vanish.
//!
//! The two hid each other. The missing rootdir produced the fixture error; the missing containment
//! turned it into a dead worker with a traceback that pointed at neither cause.

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

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t34_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    dir
}

/// A suite in the conventional layout: `pytest.ini` at the root, a suite-wide `conftest.py` beside
/// it holding the session fixture, and the tests one directory down. Nothing here is exotic — the
/// point is that this is the *normal* shape, and it did not work.
fn write_rootdir_corpus() -> PathBuf {
    let dir = scratch("rootdir");
    std::fs::write(dir.join("pytest.ini"), "[pytest]\n").unwrap();
    std::fs::write(
        dir.join("conftest.py"),
        "import pytest\n\
         \n\
         \n\
         @pytest.fixture(scope=\"session\")\n\
         def suite_wide():\n\
         \x20   return \"from the ancestor conftest\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tests/test_uses_ancestor.py"),
        "def test_sees_the_session_fixture(suite_wide):\n\
         \x20   assert suite_wide == \"from the ancestor conftest\"\n",
    )
    .unwrap();
    dir
}

/// One test whose fixture raises, and three that are fine. The three are the assertion: before this
/// fix they were collateral damage, lost with the worker that died.
fn write_containment_corpus() -> PathBuf {
    let dir = scratch("containment");
    std::fs::write(dir.join("pytest.ini"), "[pytest]\n").unwrap();
    std::fs::write(
        dir.join("conftest.py"),
        "import pytest\n\
         \n\
         \n\
         @pytest.fixture(scope=\"session\")\n\
         def doomed():\n\
         \x20   raise RuntimeError(\"this fixture cannot be built\")\n",
    )
    .unwrap();
    // `a`/`c`/`d` sort around `b`, so the survivors are on both sides of the failure rather than
    // only after it — a worker that died at `b` would take `c` and `d` with it either way, but this
    // also catches a fix that merely stops *early* instead of continuing.
    std::fs::write(
        dir.join("tests/test_mixed.py"),
        "def test_a_fine():\n\
         \x20   assert True\n\
         \n\
         \n\
         def test_b_needs_the_doomed_fixture(doomed):\n\
         \x20   assert doomed\n\
         \n\
         \n\
         def test_c_fine():\n\
         \x20   assert True\n\
         \n\
         \n\
         def test_d_fine():\n\
         \x20   assert True\n",
    )
    .unwrap();
    dir
}

/// `pytest.ini` bounds the ancestor-conftest walk, so a session fixture defined beside it resolves.
#[test]
fn pytest_ini_marks_the_rootdir_so_ancestor_conftests_load() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_rootdir_corpus();
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), 1);

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &tests);
    let results = worker.run(&items).expect("batch runs against real Python");
    assert_eq!(
        results[0].outcome,
        Outcome::Passed,
        "TID-34: a `conftest.py` beside `pytest.ini` must be collected — {}",
        results[0].detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fixture that raises errors *its* test. The rest of the batch still runs and still reports.
#[test]
fn a_failing_fixture_errors_its_test_without_taking_the_batch_down() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_containment_corpus();
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), 4);

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &tests);
    let results = worker
        .run(&items)
        .expect("the batch survives the bad fixture");
    assert_eq!(
        results.len(),
        4,
        "TID-34: every test must report, not just the ones before the failure"
    );

    for r in &results {
        let leaf = r.node_id.as_str().rsplit("::").next().unwrap_or_default();
        if leaf.contains("doomed") {
            assert_eq!(r.outcome, Outcome::Error, "the bad fixture errors its test");
            // The message has to name the cause. `shim closed mid-run` named the transport, which
            // is why this was hard to diagnose in the first place.
            assert!(
                r.detail.contains("this fixture cannot be built"),
                "the error must carry the fixture's own failure; got: {:?}",
                r.detail
            );
        } else {
            assert_eq!(
                r.outcome,
                Outcome::Passed,
                "TID-34: {} is unrelated to the bad fixture and must still run — {}",
                r.node_id.as_str(),
                r.detail
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
