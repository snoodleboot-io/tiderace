//! TID-55 — a skip count answers two questions, and reporting one of them looks like a defect.
//!
//! `pytest.importorskip` at the top of a module skips every test that module holds. pytest's summary
//! counts the *event* (one skip); tiderace's counts the *tests* (one per test that did not run).
//! Neither number is wrong and neither substitutes for the other, but printed side by side —
//! pirn-core: `578 skipped` against pytest's `94 skipped` — they read as a disagreement, and during
//! the benchmark that cost hours of chasing a divergence that was not there.
//!
//! So a skip that came from a module's import now carries the module it came from, and the summary
//! reports both dimensions: `578 skipped (12 modules skipped at import)`. A per-test skip carries no
//! origin and is counted only in the first number.
//!
//! The corpus here uses `unittest.SkipTest` rather than `pytest.importorskip` deliberately: the
//! behaviour under test is the shim's, not pytest's, and a stdlib-only corpus runs wherever a Python
//! does — an acceptance test that silently skips itself for want of a plugin proves nothing (the
//! lesson of TID-60's first cut).

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, RunReport};
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::reporter::{JsonReporter, Reporter};
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
        "tiderace_t55_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Six skipped tests from three different causes: a module that does not import (3 tests), a
/// directory whose conftest does not import (2 tests), and one test that skips itself (1 test).
/// Only the first two are *module* skips; the third must not inflate the module count.
fn write_project(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    let tests = dir.join("tests");
    std::fs::create_dir_all(tests.join("optional")).unwrap();

    std::fs::write(
        tests.join("test_ordinary.py"),
        "def test_ordinary():\n    assert True\n",
    )
    .unwrap();

    // The module itself refuses to import. Three tests, one skip event.
    std::fs::write(
        tests.join("test_widgets.py"),
        "import unittest\n\
         \n\
         raise unittest.SkipTest(\"the widget library is not installed\")\n\
         \n\
         def test_widget_a():\n    assert False\n\
         \n\
         def test_widget_b():\n    assert False\n\
         \n\
         def test_widget_c():\n    assert False\n",
    )
    .unwrap();

    // The *conftest* refuses to import, which takes the whole directory with it. Two tests, one
    // skip event — and the tests' own modules are never touched, which is why they are counted by
    // the module they belong to rather than by the conftest that excluded them.
    std::fs::write(
        tests.join("optional/conftest.py"),
        "import unittest\n\nraise unittest.SkipTest(\"no backend for the optional suite\")\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("optional/test_backend.py"),
        "def test_backend_a():\n    assert False\n\
         \n\
         def test_backend_b():\n    assert False\n",
    )
    .unwrap();

    // One test, skipping itself at runtime. Its module imported fine.
    std::fs::write(
        tests.join("test_inline.py"),
        "import unittest\n\n\
         def test_inline():\n    raise unittest.SkipTest(\"this one test opted out\")\n",
    )
    .unwrap();

    dir
}

fn run(python: &str, tests: &std::path::Path) -> RunReport {
    let items = RegexCollector::new().collect(tests).expect("collection");
    assert_eq!(items.len(), 7, "seven tests are collected: {items:?}");
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), tests);
    RunReport::new(worker.run(&items).expect("batch runs"))
}

#[test]
fn skips_are_counted_in_both_dimensions() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project("both");
    let report = run(&python, &dir.join("tests"));

    assert_eq!(
        report.tally(Outcome::Passed),
        1,
        "only the ordinary test runs"
    );
    assert_eq!(
        report.tally(Outcome::Skipped),
        6,
        "six tests did not run — the dimension pytest's summary does not report: {:?}",
        outcomes(&report)
    );
    assert_eq!(
        report.skipped_modules(),
        2,
        "TID-55: two modules failed to import (test_widgets.py, optional/test_backend.py); the \
         inline skip is not a module skip and must not be counted as one — origins: {:?}",
        report
            .results
            .iter()
            .map(|r| (r.node_id.as_str(), r.skip_origin.as_str()))
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The two dimensions must be *independent*: the test that skips itself is one of the six and none
/// of the two, and the tests that come from a module skip name the module they came from.
#[test]
fn a_per_test_skip_carries_no_module_origin() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project("origins");
    let report = run(&python, &dir.join("tests"));

    let inline = find(&report, "test_inline");
    assert_eq!(inline.outcome, Outcome::Skipped, "{}", inline.detail);
    assert!(
        inline.skip_origin.is_empty(),
        "a test that skips itself did not take its module down with it, so it has no origin — got {:?}",
        inline.skip_origin
    );

    for (node, module) in [
        ("test_widget_a", "test_widgets.py"),
        ("test_backend_a", "optional/test_backend.py"),
    ] {
        let r = find(&report, node);
        assert_eq!(r.outcome, Outcome::Skipped, "{}", r.detail);
        assert_eq!(
            r.skip_origin, module,
            "{node} is skipped because {module} never imported, and says so"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The machine-readable report is the other half of TID-55: a consumer compares *sets of node ids*
/// rather than parsing a terminal tally, because a tally hides two errors that cancel.
#[test]
fn the_json_report_carries_node_ids_and_both_dimensions() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project("json");
    let report = run(&python, &dir.join("tests"));

    let json: serde_json::Value =
        serde_json::from_str(&JsonReporter.render(&report)).expect("valid JSON");
    assert_eq!(json["skipped"], 6);
    assert_eq!(json["skipped_modules"], 2);
    assert_eq!(json["total"], 7);

    let tests = json["tests"].as_array().expect("per-node records");
    assert_eq!(tests.len(), 7, "one record per node, not a tally");
    let widget = tests
        .iter()
        .find(|t| {
            t["node_id"]
                .as_str()
                .unwrap_or("")
                .contains("test_widget_a")
        })
        .expect("every node appears by id");
    assert_eq!(widget["outcome"], "skipped");
    assert_eq!(widget["skip_origin"], "test_widgets.py");

    let inline = tests
        .iter()
        .find(|t| t["node_id"].as_str().unwrap_or("").contains("test_inline"))
        .expect("every node appears by id");
    assert!(
        inline.get("skip_origin").is_none(),
        "an absent origin is absent from the record, not an empty string: {inline}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn find<'a>(report: &'a RunReport, needle: &str) -> &'a engine_core::domain::TestResult {
    report
        .results
        .iter()
        .find(|r| r.node_id.as_str().contains(needle))
        .unwrap_or_else(|| panic!("{needle} reports a result: {:?}", outcomes(report)))
}

fn outcomes(report: &RunReport) -> Vec<(&str, Outcome)> {
    report
        .results
        .iter()
        .map(|r| (r.node_id.as_str(), r.outcome))
        .collect()
}
