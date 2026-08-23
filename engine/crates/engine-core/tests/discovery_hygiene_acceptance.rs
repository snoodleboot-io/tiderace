//! Two defects in the shim's discovery pass, both found while attempting TID-37.
//!
//! **A bare `pytestmark` crashed the whole run.** pytest accepts both `pytestmark = pytest.mark.slow`
//! and `pytestmark = [pytest.mark.slow, …]`. `_own_markers` assumed the list form and called
//! `out.extend(marks)` on it, and a `MarkDecorator` is not iterable — so a single module using the
//! scalar spelling raised `TypeError` out of `_discover` and took the entire run down before a test
//! ran. Not a wrong answer: no answer.
//!
//! **Discovery descended into `.venv/`.** The walk had no skip list, so it found and imported the
//! *dependencies'* test suites — numpy ships its own, and `.venv/…/numpy/conftest.py` was being
//! imported on every run of any suite with numpy installed. It stayed invisible because the module
//! names produced for those paths were unimportable, so each one failed and was swallowed. Slow,
//! wrong, and a latent source of exactly the kind of foreign `pytestmark` above.

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

/// These corpora define real pytest marks, so an interpreter without pytest cannot express what is
/// being tested. Prefer the fx venv, which CI provisions for exactly this.
fn python_with_pytest() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let candidates: Vec<String> = if venv.exists() {
        vec![venv.to_string_lossy().into_owned()]
    } else {
        vec!["python3".into(), "python".into()]
    };
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
        "tiderace_hyg_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A module using the scalar `pytestmark` spelling, alongside one using the list spelling, so the
/// test also pins that the list form still works.
#[test]
fn a_scalar_pytestmark_does_not_take_the_run_down() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("pytestmark");
    std::fs::write(
        dir.join("test_scalar_mark.py"),
        "import pytest\n\
         \n\
         pytestmark = pytest.mark.integration\n\
         \n\
         \n\
         def test_runs_anyway():\n\
         \x20   assert True\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("test_list_mark.py"),
        "import pytest\n\
         \n\
         pytestmark = [pytest.mark.integration]\n\
         \n\
         \n\
         def test_also_runs():\n\
         \x20   assert True\n",
    )
    .unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    // Before the fix this did not report a failure — it never got far enough to report anything,
    // because discovery raised before the first test ran.
    let results = worker
        .run(&items)
        .expect("a scalar pytestmark must not abort discovery");
    assert_eq!(results.len(), 2, "both modules report");
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "{} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.venv` inside the run root is not part of the suite, and must not be imported.
///
/// The planted file is booby-trapped: it raises on import. If discovery walks into `.venv/` it will
/// import it, and the failure is unmissable. If discovery skips the directory — as it must — the
/// booby trap never fires and the real test passes.
#[test]
fn discovery_does_not_descend_into_a_virtualenv() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("venvskip");
    std::fs::write(
        dir.join("test_real.py"),
        "def test_the_suites_own_test():\n\x20   assert True\n",
    )
    .unwrap();
    let vendored = dir.join(".venv/lib/python3.12/site-packages/somedep");
    std::fs::create_dir_all(&vendored).unwrap();
    std::fs::write(
        vendored.join("test_vendored.py"),
        "raise RuntimeError('discovery imported a dependency's own test suite')\n"
            .replace("dependency's", "dependencys"),
    )
    .unwrap();
    // A foreign conftest too — the numpy case that surfaced this. It leaves a sentinel file rather
    // than raising, because raising proves nothing: `_import_conftest` loads by file location and
    // catches whatever comes out, so an exception is swallowed and looks identical either way. A
    // side effect on disk is the only thing that distinguishes "skipped" from "imported and failed".
    let sentinel = dir.join("vendored-conftest-was-imported");
    std::fs::write(
        vendored.join("conftest.py"),
        format!(
            "import pathlib\n\npathlib.Path({:?}).write_text('imported')\n",
            sentinel.to_string_lossy()
        ),
    )
    .unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(
        items.len(),
        1,
        "collection sees only the suite's own test, not the vendored one"
    );

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    assert_eq!(results[0].outcome, Outcome::Passed, "{}", results[0].detail);
    assert!(
        !sentinel.exists(),
        "discovery imported a conftest from inside .venv — dependencies' test suites are not part \
         of this suite, and importing them is both slow and a source of foreign collection state"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
