//! TID-70 — a failure that reads import history says so.
//!
//! The one pirn-agents divergence in the whole benchmark was `assert "chromadb" not in sys.modules`:
//! true only if no earlier test in the same process imported it. Tiderace groups tests by module and
//! drains them from a queue, so which tests share a process — and in what order — is not pytest's
//! file order and is not promised to be. That is a stated non-goal. But a bare `AssertionError`
//! against a runner the author has just switched to reads as the runner's bug, so the failure now
//! appends one line naming the dependence — and only when it is real: when other tests ran before
//! this one in the same process. The same test run alone gets no such line, because it passes.
//!
//! One worker, one process, the subprocess tier: the two tests must share a process for the first
//! to leave its import behind for the second.

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
        "tiderace_t70_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const PROBE: &str = "VALUE = 1\n";

/// `test_a` imports a module; `test_b` asserts nobody has. Under pytest in file order that passes;
/// it passes in any order only if `test_b` runs first, which is the dependence.
const CORPUS: &str = r#"
import sys


def test_a_imports_it():
    import probe_mod
    assert probe_mod.VALUE == 1


def test_b_assumes_nobody_did():
    assert "probe_mod" not in sys.modules
"#;

/// The same assertion with nothing before it — an honest test, and no note.
const ALONE: &str = r#"
import sys


def test_b_assumes_nobody_did():
    assert "probe_mod" not in sys.modules
"#;

fn run(dir: &Path, python: &str) -> Vec<engine_core::domain::TestResult> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    SubprocessWorker::new(20_000, 1)
        .with_target(python.to_string(), &shim(), dir)
        .run(&items)
        .expect("batch runs")
}

#[test]
fn a_failure_that_reads_import_history_names_the_dependence() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("shared");
    std::fs::write(dir.join("probe_mod.py"), PROBE).unwrap();
    std::fs::write(dir.join("test_history.py"), CORPUS).unwrap();
    let results = run(&dir, &python);
    assert_eq!(results.len(), 2, "{results:?}");

    let b = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_b_assumes_nobody_did"))
        .expect("test_b reports");
    assert_eq!(
        b.outcome,
        Outcome::Failed,
        "the assertion is false once test_a has run in this process: {}",
        b.detail
    );
    assert!(
        b.detail.contains("reads import history"),
        "TID-70: the failure must say what it depends on, not read as the runner's bug — got:\n{}",
        b.detail
    );
    assert!(
        b.detail.contains("not pytest's file order"),
        "and name the non-goal: {}",
        b.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The note is not a blanket disclaimer on every `sys.modules` mention. Alone, the test passes,
/// and a passing test carries no note.
#[test]
fn the_same_assertion_alone_passes_without_a_note() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("alone");
    std::fs::write(dir.join("probe_mod.py"), PROBE).unwrap();
    std::fs::write(dir.join("test_alone.py"), ALONE).unwrap();
    let results = run(&dir, &python);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].outcome, Outcome::Passed, "{}", results[0].detail);
    assert!(!results[0].detail.contains("import history"));
    let _ = std::fs::remove_dir_all(&dir);
}
