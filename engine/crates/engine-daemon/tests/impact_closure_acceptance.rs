//! TID-40, held at the front end a developer uses, and the two defects of the same family found
//! while carrying it into trace output (TID-128, TID-133).
//!
//! `module_footprint_acceptance` proves every test in a module *records* the module's imports.
//! This proves what that is for: after an edit, the impact-aware run — `tiderace-daemon run`,
//! which serves every test whose recorded dependencies are unchanged from cache — reports what a
//! full run reports. A stale pass here is a green suite over broken code.
//!
//! Three edits, three kinds of dependency no test's coverage names, because each one's code ran
//! before any test did:
//!
//!   * a source module imported at the top of a test module (the TID-40 repro itself);
//!   * the `__init__.py` of the package that module is in — nothing in an import statement
//!     names it, so a closure built from import statements alone did not hold it, and breaking
//!     it left all 60 tests cached and green;
//!   * a helper a `conftest.py` imports — the conftest was in the closure, what it imports was
//!     not, so a fixture built from the helper's constant went stale.
//!
//! Twenty tests per module, as the TID-40 fixture demands: at two per module the defect is
//! invisible.

use engine_core::testing::{many_tests_corpus, python, scratch, shim, skip_live, PythonNeeds};
use std::path::Path;
use std::process::Command;

/// `tiderace-daemon run <root>`: its tally line (`N ran, N cached, N total, N failing`).
fn impacted_run(python: &str, root: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_tiderace-daemon"))
        .arg("run")
        .arg(root)
        .env("TIDERACE_PYTHON", python)
        .env("TIDERACE_SHIM", shim())
        .env_remove("TIDERACE_CACHE_DIR")
        .output()
        .expect("the daemon binary runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr
        .lines()
        .rev()
        .find(|l| l.contains(" total, "))
        .unwrap_or_else(|| panic!("no tally line in: {stderr}"))
        .to_string()
}

#[test]
fn breaking_a_source_function_fails_every_test_of_its_module_on_the_impact_aware_run() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = many_tests_corpus("t40_impact", 3, 20);
    assert_eq!(
        impacted_run(&python, &dir),
        "60 ran, 0 cached, 60 total, 0 failing"
    );
    assert_eq!(
        impacted_run(&python, &dir),
        "0 ran, 60 cached, 60 total, 0 failing",
        "nothing edited, nothing runs"
    );

    std::fs::write(dir.join("src/mod_1.py"), "def f(x):\n    return x + 100\n").unwrap();
    assert_eq!(
        impacted_run(&python, &dir),
        "20 ran, 40 cached, 60 total, 20 failing",
        "TID-40: all twenty tests of the module that imports src/mod_1.py re-run and fail — not \
         the one that happened to run first — and the forty that do not import it stay cached"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn breaking_a_package_init_fails_every_test_that_imports_through_it() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = many_tests_corpus("t128_init", 3, 20);
    assert_eq!(
        impacted_run(&python, &dir),
        "60 ran, 0 cached, 60 total, 0 failing"
    );

    std::fs::write(
        dir.join("src/__init__.py"),
        "raise RuntimeError('broken package')\n",
    )
    .unwrap();
    assert_eq!(
        impacted_run(&python, &dir),
        "60 ran, 0 cached, 60 total, 60 failing",
        "TID-128: `from src.mod_1 import f` runs src/__init__.py first. Every test imports \
         through it, so every test depends on it; before this was recorded the run said \
         `0 ran, 60 cached, 60 total, 0 failing` over a suite that could not import"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editing_what_a_conftest_imports_reruns_the_tests_beneath_it() {
    let Some(python) = python(PythonNeeds::Pytest) else {
        skip_live("no interpreter with pytest");
        return;
    };
    let dir = scratch("t128_conftest_import");
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("helpers.py"), "VALUE = 1\n").unwrap();
    std::fs::write(
        dir.join("conftest.py"),
        "import os, sys\nsys.path.insert(0, os.path.dirname(__file__))\nimport pytest\n\
         from helpers import VALUE\n\n\n@pytest.fixture\ndef value():\n    return VALUE\n",
    )
    .unwrap();
    let mut body = String::new();
    for t in 0..20 {
        body.push_str(&format!(
            "def test_{t}(value):\n    assert value == 1\n\n\n"
        ));
    }
    std::fs::write(dir.join("tests/test_value.py"), body).unwrap();

    assert_eq!(
        impacted_run(&python, &dir),
        "20 ran, 0 cached, 20 total, 0 failing"
    );
    // A different length, not only a different digit: CPython trusts a `.pyc` whose source has
    // the same size and the same mtime to the second, and this edit lands within that second.
    std::fs::write(dir.join("helpers.py"), "VALUE = 2  # edited\n").unwrap();
    assert_eq!(
        impacted_run(&python, &dir),
        "20 ran, 0 cached, 20 total, 20 failing",
        "TID-128: the fixture returns a constant from a module the conftest imports. That \
         module's body ran when the conftest was imported, before any test, so no test's \
         coverage names it — the closure has to"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
