//! TID-67 — a custom mark can be declared in code, so a tiderace-native suite needs no pytest
//! config block to use `--strict-markers`.
//!
//! TID-59 shipped apply (`@tiderace.mark.slow`), validate (`--strict-markers`) and select (`-m`).
//! Declaration had one surface: `markers` in a pytest config section. A suite with no `import
//! pytest` anywhere still had to keep a `[tool.pytest.ini_options]` block to declare its own marks,
//! or strict checking rejected them — the wrong dependency for a native API to have.
//!
//! The corpus here has **no config file at all**. A conftest registers `slow`; a test marked `slow`
//! runs, a test marked `slwo` errors, and `-m slow` selects the first. One test function, because
//! strictness and the marker expression travel through the environment and the environment is
//! process-wide.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
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

/// A Python that can `import tiderace` — the fx venv, or whatever `PYTHONPATH` makes work (CI sets
/// it to `engine/py-tiderace`).
fn python_with_tiderace() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut cands: Vec<String> = Vec::new();
    if venv.exists() {
        cands.push(venv.to_string_lossy().into_owned());
    }
    cands.extend(["python3".to_string(), "python".to_string()]);
    cands.into_iter().find(|p| {
        std::process::Command::new(p)
            .args(["-c", "import tiderace"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t67_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_project(dir: &Path) {
    // No pyproject.toml, no pytest.ini, no setup.cfg. The conftest is the declaration.
    std::fs::write(
        dir.join("conftest.py"),
        "import tiderace\n\ntiderace.mark.register(\"slow\", \"takes more than a second\")\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("test_marks.py"),
        "import tiderace\n\n\
         @tiderace.mark.slow\n\
         def test_declared():\n    assert True\n\n\
         @tiderace.mark.slwo\n\
         def test_typo():\n    assert True\n\n\
         def test_plain():\n    assert True\n",
    )
    .unwrap();
}

fn run(dir: &Path, python: &str) -> Vec<engine_core::domain::TestResult> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    SubprocessWorker::new(20_000, 1)
        .with_target(python.to_string(), &shim(), dir)
        .run(&items)
        .expect("batch runs")
}

fn outcome<'a>(
    results: &'a [engine_core::domain::TestResult],
    name: &str,
) -> &'a engine_core::domain::TestResult {
    results
        .iter()
        .find(|r| r.node_id.as_str().ends_with(name))
        .unwrap_or_else(|| panic!("{name} reports: {results:?}"))
}

#[test]
fn a_mark_registered_in_a_conftest_is_declared_without_any_config_file() {
    let Some(python) = python_with_tiderace() else {
        skip_live("no interpreter that can import tiderace");
        return;
    };
    let dir = scratch("native");
    write_project(&dir);

    // ── strict: the registered mark passes, the typo is an error ─────────────────────────────
    // SAFETY: every env-dependent assertion in this file lives in this one test.
    unsafe { std::env::set_var("TIDERACE_STRICT_MARKERS", "1") };
    let results = run(&dir, &python);
    assert_eq!(results.len(), 3, "{results:?}");
    let declared = outcome(&results, "test_declared");
    assert_eq!(
        declared.outcome,
        Outcome::Passed,
        "TID-67: `slow` was registered by the conftest, so it is declared — {}",
        declared.detail
    );
    let typo = outcome(&results, "test_typo");
    assert_eq!(typo.outcome, Outcome::Error, "{}", typo.detail);
    assert!(
        typo.detail.contains("slwo"),
        "the error names the undeclared mark: {}",
        typo.detail
    );
    assert_eq!(outcome(&results, "test_plain").outcome, Outcome::Passed);

    // ── and -m selects on it ─────────────────────────────────────────────────────────────────
    unsafe { std::env::set_var("TIDERACE_MARKER_EXPR", "slow") };
    let results = run(&dir, &python);
    let mut ran: Vec<&str> = results
        .iter()
        .filter(|r| r.outcome != Outcome::Error)
        .map(|r| r.node_id.as_str())
        .collect();
    ran.sort();
    assert_eq!(ran, vec!["test_marks.py::test_declared"], "{results:?}");
    unsafe { std::env::remove_var("TIDERACE_MARKER_EXPR") };
    unsafe { std::env::remove_var("TIDERACE_STRICT_MARKERS") };
    let _ = std::fs::remove_dir_all(&dir);
}
