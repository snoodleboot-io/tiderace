//! Phase-2 acceptance: the engine's outcomes must match stock pytest on a fixture-free corpus
//! covering all three test styles (pytest function, pytest class method, unittest.TestCase).
//!
//! This is a real end-to-end test across the Rust↔CPython boundary — never mocked. It is gated on
//! the Lane-0 venv (`.riptide-spike-venv` with pytest) and skips cleanly if absent.

use std::path::PathBuf;
use std::process::Command;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ForkWorker, Worker};

const CALC: &str = "\
def test_add():
    assert 1 + 1 == 2


def test_sub_fails():
    assert 5 - 3 == 1


class TestMath:
    def test_pow(self):
        assert 2 ** 3 == 8
";

const CASE: &str = "\
import unittest


class CalcCase(unittest.TestCase):
    def test_mul(self):
        self.assertEqual(6 * 7, 42)

    def test_div_fails(self):
        self.assertEqual(10 / 2, 6)
";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../py-shim/shim.py")
        .canonicalize()
        .expect("shim path")
}

fn venv_python() -> Option<PathBuf> {
    let p = repo_root().join(".riptide-spike-venv/bin/python");
    p.exists().then_some(p)
}

fn wire(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "passed",
        Outcome::Failed => "failed",
        Outcome::Skipped => "skipped",
        Outcome::XFail => "xfail",
        Outcome::XPass => "xpass",
        Outcome::Error => "error",
    }
}

#[test]
fn engine_outcomes_match_pytest_across_three_styles() {
    let Some(python) = venv_python() else {
        eprintln!("SKIP: .riptide-spike-venv not found — run the Phase-1 Lane-0 env gate first");
        return;
    };

    // Isolated corpus in a temp dir.
    let dir = std::env::temp_dir().join(format!("riptide_diff_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir corpus");
    std::fs::write(dir.join("test_calc.py"), CALC).expect("write calc");
    std::fs::write(dir.join("test_case.py"), CASE).expect("write case");

    // --- engine: collect (no import) → fork-execute (Wellspring) ---
    let items = RegexCollector::new().collect(&dir).expect("collect");
    let mut worker =
        ForkWorker::launch(python.to_str().unwrap(), &shim_path(), &dir).expect("launch worker");
    let results = worker.run(&items).expect("run");
    let mut engine: Vec<(String, String)> = results
        .iter()
        .map(|r| (r.node_id.to_string(), wire(r.outcome).to_string()))
        .collect();
    engine.sort();

    // --- pytest oracle: same corpus, node ids relative to cwd ---
    let output = Command::new(&python)
        .args(["-m", "pytest", "-rA", "-q", "."])
        .current_dir(&dir)
        .output()
        .expect("run pytest");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut pytest: Vec<(String, String)> = Vec::new();
    for line in stdout.lines() {
        for (kw, oc) in [("PASSED ", "passed"), ("FAILED ", "failed")] {
            if let Some(rest) = line.strip_prefix(kw) {
                // pytest's `-rA` FAILED lines append " - <reason>"; keep only the node id.
                let id = rest.split(" - ").next().unwrap_or(rest).trim();
                pytest.push((id.to_string(), oc.to_string()));
            }
        }
    }
    pytest.sort();

    let _ = std::fs::remove_dir_all(&dir);

    assert!(!engine.is_empty(), "engine collected/ran nothing");
    assert_eq!(engine, pytest, "engine outcomes must match pytest exactly");
}
