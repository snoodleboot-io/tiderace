//! TID-49 — `--ignore` in the project's own `addopts` is honoured.
//!
//! A project that excludes a directory from its default run means it. pirn-core's
//! `addopts = "... --ignore=tests/perf ..."` holds benchmarks that only run under the
//! `pytest-benchmark` plugin; collecting them anyway reported 23 failures for tests pytest never
//! runs, all of them "missing 1 required positional argument: 'benchmark'".
//!
//! The shim already read `addopts` — for the `-m` marker expression (TID-32) — and read it *after*
//! walking the tree, so nothing there could prune the walk. It is now read up front, which is also
//! where the config's own directory is known: pytest resolves `--ignore` paths against that
//! directory, not against the run root.
//!
//! Ignored tests are reported the way a `-m` deselect is reported — absent from the tally — because
//! pytest does not collect them at all. A skip would be a different, visible outcome.

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
        "tiderace_t49_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a project whose `pyproject.toml` ignores `tests/perf`, and whose `tests/perf` test would
/// fail loudly if it ever ran (it requests a fixture only a plugin provides).
fn write_project(tag: &str, addopts: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::write(
        dir.join("pyproject.toml"),
        format!("[tool.pytest.ini_options]\naddopts = \"{addopts}\"\n"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("tests/perf")).unwrap();
    std::fs::write(
        dir.join("tests/test_ordinary.py"),
        "def test_ordinary():\n    assert True\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tests/perf/test_bench.py"),
        "def test_bench(benchmark):\n    # `benchmark` comes from pytest-benchmark, which this run has\n\
         \x20   # no reason to load — the project excludes this directory.\n    benchmark(lambda: None)\n",
    )
    .unwrap();
    dir
}

#[test]
fn a_directory_the_project_ignores_is_not_collected() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project("ignored", "-ra --ignore=tests/perf");
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), 2, "the collector still walks the whole tree");

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &tests);
    let results = worker.run(&items).expect("batch runs");

    let ordinary = results
        .iter()
        .find(|r| r.node_id.as_str().contains("test_ordinary"))
        .expect("the ordinary test reports");
    assert_eq!(ordinary.outcome, Outcome::Passed, "{}", ordinary.detail);
    assert!(
        !results.iter().any(|r| r.node_id.as_str().contains("test_bench")),
        "TID-49: `--ignore=tests/perf` must drop that test from the tally entirely, as pytest does — \
         got {:?}",
        results
            .iter()
            .map(|r| (r.node_id.as_str().to_string(), r.outcome))
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the `--ignore`, the same tree does collect and run that test — so the assertion above is
/// about the flag, not about the fixture happening to be missing.
#[test]
fn without_the_flag_the_same_directory_does_run() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project("not_ignored", "-ra");
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &tests);
    let results = worker.run(&items).expect("batch runs");

    let bench = results
        .iter()
        .find(|r| r.node_id.as_str().contains("test_bench"))
        .expect("the benchmark test is collected when nothing excludes it");
    assert_ne!(
        bench.outcome,
        Outcome::Passed,
        "it requests a fixture nothing provides, so running it must report a problem"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
