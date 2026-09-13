//! TID-48 — two ways a real monorepo took the whole run down before a single test executed.
//!
//! **A package directory already on `sys.path`, just not first.** A monorepo venv's editable-install
//! `.pth` files put every package directory on `sys.path`, and each one holds its own `tests`
//! package. The shim only inserted the run's basedir when it was *absent*, so it stayed behind its
//! siblings and `tests.unit` resolved against the wrong package: 4,855 of 4,855 tests errored on one
//! real suite. `python -m pytest` hides this, because `-m` puts the cwd ahead of every `.pth` entry.
//!
//! **A skip raised while importing.** `pytest.importorskip("ray")` at the top of a conftest skips
//! that directory, and pytest collects nothing below it. The same call at the top of a test module
//! skips that module. Both raise `_pytest.outcomes.Skipped`, a `BaseException`, which went straight
//! past handlers written for `Exception`. Under the shared-import pool, discovery happens in the pool
//! parent, so one optional dependency missing anywhere in the tree failed the entire run.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{PooledWorker, SubprocessWorker, WellspringPool, Worker};
use engine_core::testing::skip_live;
use std::path::{Path, PathBuf};
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

/// The skip cases raise pytest's own `Skipped`, so they need an interpreter with pytest.
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
        "tiderace_t48_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn outcome_of<'a>(
    results: &'a [engine_core::domain::TestResult],
    needle: &str,
) -> &'a engine_core::domain::TestResult {
    results
        .iter()
        .find(|r| r.node_id.as_str().contains(needle))
        .unwrap_or_else(|| panic!("no result for {needle}"))
}

const MISSING: &str = "tiderace_t48_this_module_does_not_exist";

/// The run's package directory wins even when a sibling's same-named `tests` package is ahead of it.
///
/// The interpreter is a wrapper that sets `PYTHONPATH` to `sibling:project`, the shape editable-install
/// `.pth` files produce, so the project directory is already on `sys.path`, second.
#[test]
#[cfg(unix)]
fn the_run_roots_basedir_wins_over_a_sibling_already_on_sys_path() {
    use std::os::unix::fs::PermissionsExt;
    let Some(python) = python_with_pytest() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("syspath");
    let sibling = dir.join("sibling");
    let project = dir.join("project");
    // The sibling's `tests` is a package with no `unit` in it, the way pirn-agents' was.
    write(&sibling.join("tests/__init__.py"), "");
    write(&project.join("tests/__init__.py"), "");
    write(&project.join("tests/unit/__init__.py"), "");
    write(
        &project.join("tests/unit/test_mine.py"),
        "def test_resolves_against_this_project():\n    assert __name__ == 'tests.unit.test_mine'\n",
    );
    let wrapper = dir.join("python");
    write(
        &wrapper,
        &format!(
            "#!/bin/sh\nPYTHONPATH={}:{} exec {} \"$@\"\n",
            sibling.display(),
            project.display(),
            python
        ),
    );
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    let tests = project.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), 1);
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(
        wrapper.to_string_lossy().into_owned(),
        &shim(),
        &tests,
    );
    let results = worker.run(&items).expect("batch runs");
    assert_eq!(
        results[0].outcome,
        Outcome::Passed,
        "TID-48: `tests.unit` must resolve against the project being run, not a sibling earlier on \
         sys.path — {}",
        results[0].detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A conftest that skips its directory costs exactly that directory, under the shared-import pool.
#[test]
fn a_conftest_that_skips_its_directory_does_not_take_down_the_pool() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("conftest_skip");
    write(
        &dir.join("fine/test_fine.py"),
        "def test_fine():\n    assert True\n",
    );
    write(
        &dir.join("optional/conftest.py"),
        &format!("import pytest\n\npytest.importorskip({MISSING:?})\n"),
    );
    write(
        &dir.join("optional/test_optional.py"),
        &format!("import {MISSING}\n\n\ndef test_needs_it():\n    assert {MISSING}\n"),
    );

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);
    let mut pool = WellspringPool::launch(&python, &shim(), &dir, true, 1)
        .expect("TID-48: a directory-level skip must not kill the pool parent during discovery");
    let transport = pool.take_worker().expect("a worker is available");
    let results = PooledWorker::new(transport, 20_000)
        .run(&items)
        .expect("pooled batch runs");

    let fine = outcome_of(&results, "test_fine");
    assert_eq!(fine.outcome, Outcome::Passed, "{}", fine.detail);
    let optional = outcome_of(&results, "test_needs_it");
    assert_eq!(
        optional.outcome,
        Outcome::Skipped,
        "a test under a skipped conftest is skipped, as pytest reports it — {}",
        optional.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A test module that skips itself at import is skipped; its neighbours on the same worker still run.
///
/// Both spellings: pytest's `importorskip` (a `BaseException`) and `unittest.SkipTest` (an `Exception`,
/// which used to be reported as an error rather than a skip).
#[test]
fn a_module_level_skip_is_a_skip_and_the_worker_survives() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("module_skip");
    write(
        &dir.join("test_a_pytest_skip.py"),
        &format!("import pytest\n\npytest.importorskip({MISSING:?})\n\n\ndef test_pytest_skipped():\n    pass\n"),
    );
    write(
        &dir.join("test_b_unittest_skip.py"),
        "import unittest\n\nraise unittest.SkipTest('optional backend absent')\n\n\ndef test_unittest_skipped():\n    pass\n",
    );
    write(
        &dir.join("test_c_after.py"),
        "def test_after():\n    assert True\n",
    );

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3);
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker
        .run(&items)
        .expect("TID-48: a module-level skip must not close the shim mid-run");

    for needle in ["test_pytest_skipped", "test_unittest_skipped"] {
        let r = outcome_of(&results, needle);
        assert_eq!(r.outcome, Outcome::Skipped, "{needle} — {}", r.detail);
    }
    let after = outcome_of(&results, "test_after");
    assert_eq!(after.outcome, Outcome::Passed, "{}", after.detail);
    let _ = std::fs::remove_dir_all(&dir);
}
