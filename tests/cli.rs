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
