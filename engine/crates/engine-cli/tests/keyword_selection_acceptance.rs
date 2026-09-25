//! TID-63 — `-k EXPR` selects by name, exactly as pytest's `-k` does.
//!
//! The runner could select by marker (`-m`, TID-59) and by path, and not by name — the everyday way
//! to run one test or one class while iterating. The check here is the one the ticket asks for: for
//! each expression, the set of node ids tiderace runs is compared against the set pytest collects
//! for the same `-k`, so the match rule (case-insensitive substrings of the file, class, function
//! and case id, and mark names) is pytest's by measurement rather than by reading.
//!
//! Deselected items are absent from the tally, as `-m` deselection and `--ignore` already are:
//! pytest does not collect them, and a skip would be a different, visible outcome.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// A Python that has pytest, since the oracle is pytest itself.
fn python_with_pytest() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut cands: Vec<String> = Vec::new();
    if venv.exists() {
        cands.push(venv.to_string_lossy().into_owned());
    }
    cands.extend(["python3".to_string(), "python".to_string()]);
    cands.into_iter().find(|p| {
        Command::new(p)
            .args(["-c", "import pytest"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn write_project() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t63_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("pyproject.toml"),
        "[tool.pytest.ini_options]\nmarkers = [\"slow: takes a while\"]\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_alpha.py"),
        "def test_one():\n    assert True\n\n\
         def test_two():\n    assert True\n\n\
         class TestGroup:\n    def test_three(self):\n        assert True\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_beta.py"),
        "import pytest\n\n\
         @pytest.mark.parametrize(\"n,tag\", [(1, \"a\"), (2, \"b\")])\n\
         def test_cases(n, tag):\n    assert True\n\n\
         @pytest.mark.skip(reason=\"never\")\n\
         def test_skipped():\n    assert False\n\n\
         @pytest.mark.slow\n\
         def test_marked():\n    assert True\n",
    )
    .unwrap();
    dir
}

/// The node ids pytest collects for `-k expr`, relative to `tests`.
fn pytest_selects(python: &str, tests: &Path, expr: &str) -> BTreeSet<String> {
    let out = Command::new(python)
        .args([
            "-m",
            "pytest",
            "--collect-only",
            "-q",
            "-p",
            "no:cacheprovider",
            "-k",
            expr,
        ])
        .current_dir(tests)
        .output()
        .expect("pytest runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("::"))
        .map(|l| l.trim().to_string())
        .collect()
}

/// The node ids tiderace runs for `-k expr`, from `--report`.
fn tiderace_selects(python: &str, tests: &Path, expr: &str) -> (BTreeSet<String>, String) {
    let report = tests.join(format!("report-{}.json", expr.len()));
    let out = Command::new(env!("CARGO_BIN_EXE_tiderace"))
        .args(["run", "-q", "--workers", "1", "-k", expr, "--report"])
        .arg(&report)
        .arg(tests)
        .env("TIDERACE_PYTHON", python)
        .env("TIDERACE_SHIM", repo_root().join("engine/py-shim/shim.py"))
        .output()
        .expect("the CLI runs");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "-k {expr:?}: {stderr}");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).expect("report written"))
            .expect("valid JSON");
    let ids = json["tests"]
        .as_array()
        .expect("per-node records")
        .iter()
        .filter_map(|t| t["node_id"].as_str().map(str::to_string))
        .collect();
    (ids, stderr)
}

#[test]
fn k_selects_exactly_what_pytest_selects() {
    let Some(python) = python_with_pytest() else {
        eprintln!("skipping: no Python with pytest available");
        return;
    };
    let dir = write_project();
    let tests = dir.join("tests");

    for expr in [
        "one",                  // a function name
        "TestGroup",            // a class selects its methods
        "cases and not 2-b",    // one parametrize case, by its id
        "alpha or 2-b",         // a file name, and a case id with a dash in it
        "b and not skipped",    // `b` is a substring of the FILE name, as pytest reads it
        "not two",              // everything but one
        "slow",                 // a mark name is a keyword too
        "test_cases[1-a]",      // the whole case id, brackets and all
        "beta and not skipped", // a deselected skip is not a skip
    ] {
        let want = pytest_selects(&python, &tests, expr);
        assert!(
            !want.is_empty(),
            "the oracle selects something for {expr:?}"
        );
        let (got, stderr) = tiderace_selects(&python, &tests, expr);
        assert_eq!(
            got, want,
            "TID-63: -k {expr:?} must select exactly what pytest selects — stderr: {stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_expression_that_selects_nothing_is_an_empty_run_not_an_error() {
    let Some(python) = python_with_pytest() else {
        eprintln!("skipping: no Python with pytest available");
        return;
    };
    let dir = write_project();
    let tests = dir.join("tests");
    let (got, stderr) = tiderace_selects(&python, &tests, "nomatch");
    assert!(got.is_empty(), "{got:?}");
    assert!(stderr.contains("0 total"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
