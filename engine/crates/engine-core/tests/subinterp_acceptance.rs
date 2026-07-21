//! TID-10 — live acceptance for the `SubInterpWorker` (ADR-E015 Phase 2). Runs a batch of
//! sub-interpreter-**safe** (pure-Python) tests across a pool of isolated sub-interpreters and asserts
//! (a) correct outcomes across styles and (b) **result-identity with `ForkWorker`** on the same corpus
//! — the §8 boundary-3 invariant that makes the tier safe. Needs CPython 3.14 (`concurrent.interpreters`),
//! so it gates on the fx venv (which is 3.14); self-skips otherwise.

use engine_core::testing::skip_live;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubInterpWorker, Worker};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}
fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}
/// The fx venv is CPython 3.14 (has `concurrent.interpreters`); used by the fork-identity check (Unix).
fn venv_python() -> Option<String> {
    let p = repo_root().join(".riptide-fx-venv/bin/python");
    p.exists().then(|| p.to_string_lossy().into_owned())
}

/// Any interpreter with `concurrent.interpreters` (CPython 3.14+): the fx venv, else a `python3`/`python`
/// on `PATH` that actually has the module. Lets the correctness check run on **Windows CI** too (where
/// `setup-python` provisions 3.14) — the platform the tier exists for. `None` ⇒ skip.
fn subinterp_python() -> Option<String> {
    if let Some(v) = venv_python() {
        return Some(v);
    }
    for cand in ["python3", "python"] {
        let ok = std::process::Command::new(cand)
            .args(["-c", "import concurrent.interpreters"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

/// A stdlib-only (sub-interpreter-safe) corpus across outcome types + both styles.
const CORPUS: &str = "\
def test_pass():
    assert 1 + 1 == 2

def test_fail():
    assert 1 == 2

def test_error():
    raise RuntimeError(\"boom\")

class TestGroup:
    def test_method_pass(self):
        assert \"x\".upper() == \"X\"

    def test_method_fail(self):
        assert []
";

fn expected(node_id: &str) -> Outcome {
    let leaf = node_id.rsplit("::").next().unwrap_or(node_id);
    if leaf.contains("error") {
        Outcome::Error
    } else if leaf.contains("fail") {
        Outcome::Failed
    } else {
        Outcome::Passed
    }
}

fn write_corpus() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "riptide_subinterp_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_si.py"), CORPUS).unwrap();
    dir
}

#[cfg(unix)]
fn outcome_map(rs: &[engine_core::domain::TestResult]) -> Vec<(String, Outcome)> {
    let mut v: Vec<(String, Outcome)> = rs
        .iter()
        .map(|r| (r.node_id.to_string(), r.outcome))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn subinterp_pool_runs_safe_tests_correctly() {
    let Some(python) = subinterp_python() else {
        skip_live("no CPython 3.14 (`concurrent.interpreters`) available");
        return;
    };
    let dir = write_corpus();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 5);

    let results = SubInterpWorker::new(5_000)
        .with_target(python, &shim(), &dir)
        .with_pool_size(3)
        .run(&items)
        .expect("subinterp pool runs the batch");

    assert_eq!(results.len(), 5, "one result per test");
    for r in &results {
        assert_eq!(
            r.outcome,
            expected(r.node_id.as_str()),
            "subinterp outcome for {}",
            r.node_id
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Result-identity with the fork path (the safety invariant). Uses `ForkWorker`, so Unix-only — the
/// correctness test above is what validates the tier on Windows.
#[cfg(unix)]
#[test]
fn subinterp_is_result_identical_to_fork() {
    use engine_core::exec::ForkWorker;

    let Some(python) = venv_python() else {
        skip_live("`.riptide-fx-venv` (CPython 3.14) not present");
        return;
    };
    let dir = write_corpus();
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let subinterp = SubInterpWorker::new(5_000)
        .with_target(python.clone(), &shim(), &dir)
        .with_pool_size(3)
        .run(&items)
        .expect("subinterp run");
    let forked = ForkWorker::launch(&python, &shim(), &dir)
        .expect("wellspring")
        .run(&items)
        .expect("fork run");

    assert_eq!(
        outcome_map(&subinterp),
        outcome_map(&forked),
        "the sub-interpreter pool must match ForkWorker outcome-for-outcome on the safe subset"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
