//! TID-19 — `conftest.py` above the run root must be collected, as pytest does.
//!
//! `_discover` walked `os.walk(root)` and nothing else, so it saw only the tree at or below the run
//! root. pytest collects `conftest.py` from **rootdir down**, and a conftest sitting beside
//! `pyproject.toml` is the conventional home for suite-wide setup: env defaults, warning filters,
//! `sys.path` surgery. Skipping it does not degrade gracefully — the tests below fail later, naming
//! a cause that has nothing to do with conftest discovery.
//!
//! Three properties are pinned here:
//!   * **side effects + ordering** — the ancestor conftest runs, and runs *before* test modules
//!     import. Stdlib-only, so this runs everywhere including Windows CI's bare interpreter.
//!   * **fixtures** — its fixtures are visible below it, and a nearer conftest still overrides them.
//!     Needs `pytest` to declare a fixture at all, so it self-skips where pytest is absent.
//!   * **the ceiling** — collection stops at rootdir instead of climbing to `/`.

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

/// An interpreter that can actually `import pytest`. Windows CI runs a bare `actions/setup-python`
/// with no pytest, so a corpus that declares fixtures has to self-skip there rather than fail.
fn python_with_pytest() -> Option<String> {
    let python = any_python()?;
    let ok = std::process::Command::new(&python)
        .args(["-c", "import pytest"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok.then_some(python)
}

/// A fresh `<pkg>/tests/` layout. Returns `(pkg, run_root)`; `pkg` is one level above the run root.
///
/// `pyproject.toml` is what makes `<pkg>` the rootdir, and therefore the ceiling for collection.
fn new_pkg(tag: &str) -> (PathBuf, PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pkg = std::env::temp_dir().join(format!(
        "tiderace_ancestor_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&pkg);
    let tests = pkg.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        pkg.join("pyproject.toml"),
        "[project]\nname = \"ancestor-probe\"\n",
    )
    .unwrap();
    (pkg, tests)
}

/// The bug as it was actually hit: an ancestor conftest whose whole job is a side effect.
///
/// `AT_IMPORT` is captured while the test *module* is imported, so it fails unless the conftest ran
/// before `_preimport` — the ordering half, which a check at call time would not catch.
/// Deliberately free of any `pytest` import so a bare interpreter runs it.
#[test]
fn ancestor_conftest_side_effect_applies_before_test_modules_import() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let (pkg, run_root) = new_pkg("sideeffect");
    std::fs::write(
        pkg.join("conftest.py"),
        "import os\n\nos.environ.setdefault(\"TIDERACE_ANCESTOR_PROBE\", \"set-by-root-conftest\")\n",
    )
    .unwrap();
    std::fs::write(
        run_root.join("test_ancestor.py"),
        "import os\n\
         \n\
         AT_IMPORT = os.environ.get(\"TIDERACE_ANCESTOR_PROBE\")\n\
         \n\
         \n\
         def test_side_effect_applied():\n\
         \x20   assert os.environ.get(\"TIDERACE_ANCESTOR_PROBE\") == \"set-by-root-conftest\"\n\
         \n\
         \n\
         def test_ran_before_module_import():\n\
         \x20   assert AT_IMPORT == \"set-by-root-conftest\"\n",
    )
    .unwrap();

    let items = RegexCollector::new()
        .collect(&run_root)
        .expect("collection");
    assert_eq!(items.len(), 2, "2 tests in the corpus");

    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &run_root);
    let results = worker.run(&items).expect("batch runs against real Python");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-19: {} did not pass; detail: {}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&pkg);
}

/// Fixtures declared in an ancestor conftest are visible below it — and a nearer conftest still wins.
///
/// Declaring a fixture requires pytest, so this self-skips on a bare interpreter. The side-effect
/// test above is what carries the coverage there.
#[test]
fn ancestor_conftest_fixtures_are_visible_and_still_overridable() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let (pkg, run_root) = new_pkg("fixtures");
    std::fs::write(
        pkg.join("conftest.py"),
        "import pytest\n\
         \n\
         \n\
         @pytest.fixture\n\
         def from_ancestor():\n\
         \x20   return \"ancestor\"\n\
         \n\
         \n\
         @pytest.fixture\n\
         def shadowed():\n\
         \x20   return \"ancestor\"\n",
    )
    .unwrap();
    // The nearer conftest overrides `shadowed` — the precedence half of the fix.
    std::fs::write(
        run_root.join("conftest.py"),
        "import pytest\n\
         \n\
         \n\
         @pytest.fixture\n\
         def shadowed():\n\
         \x20   return \"nearer\"\n",
    )
    .unwrap();
    std::fs::write(
        run_root.join("test_ancestor.py"),
        "def test_ancestor_fixture_is_visible(from_ancestor):\n\
         \x20   assert from_ancestor == \"ancestor\"\n\
         \n\
         \n\
         def test_nearer_conftest_still_overrides(shadowed):\n\
         \x20   assert shadowed == \"nearer\"\n",
    )
    .unwrap();

    let items = RegexCollector::new()
        .collect(&run_root)
        .expect("collection");
    assert_eq!(items.len(), 2, "2 tests in the corpus");

    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &run_root);
    let results = worker.run(&items).expect("batch runs against real Python");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-19: {} did not pass; detail: {}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&pkg);
}

/// Collection stops at rootdir rather than climbing to `/`.
///
/// Without a ceiling the walk would import whatever `conftest.py` happens to sit above the project —
/// someone's home directory, a CI scratch dir. The marker file is what bounds it, so with no marker
/// anywhere the conftest above must simply not be seen, and the run must still work.
#[test]
fn collection_stops_at_rootdir() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let (pkg, run_root) = new_pkg("ceiling");
    // No marker ⇒ `pkg` is not a rootdir, so its conftest is out of bounds.
    std::fs::remove_file(pkg.join("pyproject.toml")).unwrap();
    std::fs::write(
        pkg.join("conftest.py"),
        "import os\n\nos.environ[\"TIDERACE_ANCESTOR_PROBE\"] = \"should-not-be-applied\"\n",
    )
    .unwrap();
    std::fs::write(
        run_root.join("test_ancestor.py"),
        "import os\n\
         \n\
         \n\
         def test_out_of_bounds_conftest_is_not_applied():\n\
         \x20   assert os.environ.get(\"TIDERACE_ANCESTOR_PROBE\") is None\n",
    )
    .unwrap();

    let items = RegexCollector::new()
        .collect(&run_root)
        .expect("collection");
    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &run_root);
    let results = worker.run(&items).expect("batch runs");

    let r = results.first().expect("the test is reported");
    assert_eq!(
        r.outcome,
        Outcome::Passed,
        "an unmarked ancestor must stay out of bounds; detail: {}",
        r.detail
    );
    let _ = std::fs::remove_dir_all(&pkg);
}
