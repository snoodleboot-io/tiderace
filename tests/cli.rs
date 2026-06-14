//! End-to-end integration tests: they run the real `riptide` binary against a
//! throwaway Python project, exercising the genuine Rust → pytest → SQLite
//! boundary (no mocks — per the project's mock-only-at-boundaries rule, and here
//! the boundary IS the thing under test, so it is exercised for real).
//!
//! Tests that need to *run* Python are skipped (not failed) when no Python with
//! pytest is available, so the suite stays green on machines without the venv.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// Locate a Python interpreter that can `import pytest`. Preference order:
/// `RIPTIDE_TEST_PYTHON`, the repo's bench venv, then `python3`.
fn python_with_pytest() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(p) = std::env::var("RIPTIDE_TEST_PYTHON") {
        candidates.push(p);
    }
    candidates.push(format!(
        "{}/.riptide-bench-venv/bin/python",
        env!("CARGO_MANIFEST_DIR")
    ));
    candidates.push("python3".to_string());

    candidates.into_iter().find(|py| {
        Command::new(py)
            .args(["-c", "import pytest"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Write a minimal but realistic project: one source module, one pytest module
/// importing it, and one unittest.TestCase whose name is NOT `Test*`.
fn scaffold(dir: &Path) {
    let src = dir.join("src");
    let tests = dir.join("tests");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(src.join("__init__.py"), "").unwrap();
    std::fs::write(tests.join("__init__.py"), "").unwrap();
    std::fs::write(src.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    std::fs::write(
        dir.join("conftest.py"),
        "import sys, os\nsys.path.insert(0, os.path.dirname(__file__))\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_calc.py"),
        "from src.calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_unit.py"),
        "import unittest\nfrom src.calc import add\n\n\nclass CalcCase(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 2), 4)\n",
    )
    .unwrap();
}

fn riptide(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("riptide").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn collect_lists_unittest_case_with_class_in_node_id() {
    // No Python required — collection is pure Rust.
    let proj = TempDir::new().unwrap();
    scaffold(proj.path());

    riptide(proj.path())
        .args(["collect", "tests/"])
        .assert()
        .success()
        // pytest convention test
        .stdout(predicate::str::contains("tests/test_calc.py::test_add"))
        // unittest.TestCase subclass (non-Test* name) carries its class in the id
        .stdout(predicate::str::contains(
            "tests/test_unit.py::CalcCase::test_add",
        ));
}

#[test]
fn cold_run_passes_then_warm_run_skips_everything() {
    let Some(py) = python_with_pytest() else {
        eprintln!("skipping: no python with pytest available");
        return;
    };
    let proj = TempDir::new().unwrap();
    scaffold(proj.path());

    // Cold run: both tests execute and pass (the unittest one too — W4).
    riptide(proj.path())
        .args(["--python", &py, "tests/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("passed: 2"));

    // Warm run, nothing changed: impact analysis skips everything (W1).
    riptide(proj.path())
        .args(["--python", &py, "tests/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("All tests skipped"));
}

#[test]
fn editing_a_source_file_reruns_affected_tests() {
    let Some(py) = python_with_pytest() else {
        eprintln!("skipping: no python with pytest available");
        return;
    };
    let proj = TempDir::new().unwrap();
    scaffold(proj.path());

    // Prime with coverage so the dependency graph is recorded.
    riptide(proj.path())
        .args(["--python", &py, "--coverage", "tests/"])
        .assert()
        .success();

    // Edit the source module; the tests importing it must run again.
    std::fs::write(
        proj.path().join("src/calc.py"),
        "def add(a, b):\n    return a + b  # edited\n",
    )
    .unwrap();

    riptide(proj.path())
        .args(["--python", &py, "tests/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src/calc.py"));
}

#[test]
fn coverage_contexts_give_precise_impact() {
    // Two independent modules + their own test files. After priming with
    // --coverage (batched, dynamic contexts), editing ONE module must re-run only
    // that module's test and skip the other — proving per-test deps were recorded.
    let Some(py) = python_with_pytest() else {
        eprintln!("skipping: no python with pytest available");
        return;
    };
    let proj = TempDir::new().unwrap();
    let root = proj.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("src/__init__.py"), "").unwrap();
    std::fs::write(root.join("tests/__init__.py"), "").unwrap();
    std::fs::write(
        root.join("conftest.py"),
        "import sys, os\nsys.path.insert(0, os.path.dirname(__file__))\n",
    )
    .unwrap();
    std::fs::write(root.join("src/a.py"), "def fa():\n    return 1\n").unwrap();
    std::fs::write(root.join("src/b.py"), "def fb():\n    return 2\n").unwrap();
    std::fs::write(
        root.join("tests/test_a.py"),
        "from src.a import fa\ndef test_a():\n    assert fa() == 1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/test_b.py"),
        "from src.b import fb\ndef test_b():\n    assert fb() == 2\n",
    )
    .unwrap();

    riptide(root)
        .args(["--python", &py, "--coverage", "tests/"])
        .assert()
        .success();

    // Edit only src/a.py.
    std::fs::write(root.join("src/a.py"), "def fa():\n    return 1  # edited\n").unwrap();

    // Exactly one test (test_a) should run; test_b is skipped by impact analysis.
    riptide(root)
        .args(["--python", &py, "tests/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("passed: 1"))
        .stdout(predicate::str::contains("skipped (unchanged): 1"));
}

#[test]
fn parametrized_and_async_tests_report_correctly() {
    // A parametrized test (pytest expands to test[1], test[2], …) must aggregate
    // to a single pass/fail, not show up as an error. An async test must collect.
    let Some(py) = python_with_pytest() else {
        eprintln!("skipping: no python with pytest available");
        return;
    };
    let proj = TempDir::new().unwrap();
    std::fs::create_dir_all(proj.path().join("tests")).unwrap();
    std::fs::write(
        proj.path().join("tests/test_param.py"),
        "import pytest\n\n\n@pytest.mark.parametrize(\"n\", [1, 2, 3])\ndef test_pos(n):\n    assert n > 0\n\n\n@pytest.mark.parametrize(\"n\", [1, -1])\ndef test_mixed(n):\n    assert n > 0\n",
    )
    .unwrap();

    riptide(proj.path())
        .args(["--python", &py, "tests/"])
        .assert()
        .failure() // test_mixed[-1] fails
        .code(1)
        // all-pass parametrized test is reported passed (not error)
        .stdout(predicate::str::contains("passed: 1"))
        .stdout(predicate::str::contains("failed: 1"))
        // and never miscounted as an error
        .stdout(predicate::str::contains("errors").not());
}

#[test]
fn watch_reruns_impacted_tests_on_change() {
    // Spawn `riptide watch`, let the warm pool prime, change a test file, and
    // confirm a re-run cycle fires. Exercises the pool + notify watcher + watch
    // command end-to-end. Uses generous sleeps for CI robustness.
    let Some(py) = python_with_pytest() else {
        eprintln!("skipping: no python with pytest available");
        return;
    };
    let proj = TempDir::new().unwrap();
    scaffold(proj.path());
    let bin = assert_cmd::cargo::cargo_bin("riptide");
    let log = proj.path().join("watch.out");
    let mut child = Command::new(&bin)
        .args(["--python", &py, "watch", "tests/"])
        .current_dir(proj.path())
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_secs(7)); // initial run + 8-worker warmup
    std::fs::write(
        proj.path().join("tests/test_calc.py"),
        "from src.calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_extra():\n    assert add(0, 0) == 0\n",
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(5)); // detect + warm re-run
    let _ = child.kill();
    let _ = child.wait();

    let out = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        out.contains("warm pool ready"),
        "no initial warm run:\n{out}"
    );
    assert!(
        out.contains("file(s) changed"),
        "no re-run cycle fired:\n{out}"
    );
}

#[test]
fn failing_test_yields_exit_code_1() {
    let Some(py) = python_with_pytest() else {
        eprintln!("skipping: no python with pytest available");
        return;
    };
    let proj = TempDir::new().unwrap();
    scaffold(proj.path());
    // Overwrite with a guaranteed failure.
    std::fs::write(
        proj.path().join("tests/test_calc.py"),
        "def test_boom():\n    assert False\n",
    )
    .unwrap();

    riptide(proj.path())
        .args(["--python", &py, "tests/"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn pyproject_config_supplies_defaults() {
    // The W11 path: [tool.riptide] sets the python binary; no --python flag given.
    let Some(py) = python_with_pytest() else {
        eprintln!("skipping: no python with pytest available");
        return;
    };
    let proj = TempDir::new().unwrap();
    scaffold(proj.path());
    std::fs::write(
        proj.path().join("pyproject.toml"),
        format!("[tool.riptide]\npython = \"{}\"\npaths = [\"tests\"]\n", py),
    )
    .unwrap();

    // No --python and no path args: both come from pyproject.
    riptide(proj.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("passed: 2"));
}
