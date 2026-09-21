//! TID-55 — `tiderace run --report <path>` writes a per-node JSON report, and the terminal summary
//! reports skips in both dimensions.
//!
//! The unit tests next to `Options::parse` prove the flag parses. This proves the flag *does*
//! something: that a file appears at the path, that it holds one record per node id rather than a
//! tally, and that the summary line a human reads carries the second skip dimension too. The
//! benchmark harness had to regex terminal output to compare node ids against pytest's, which is how
//! a 62-test gap stayed hidden as two opposite errors partly cancelling.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn any_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
    ["python3", "python"]
        .into_iter()
        .find(|cand| {
            Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

/// Two tests that pass, and one module holding two tests that never imports.
fn write_project() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t55_cli_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("test_plain.py"),
        "def test_one():\n    assert True\n\ndef test_two():\n    assert True\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_absent.py"),
        "import unittest\n\n\
         raise unittest.SkipTest(\"the optional dependency is not installed\")\n\n\
         def test_x():\n    assert False\n\ndef test_y():\n    assert False\n",
    )
    .unwrap();
    dir
}

#[test]
fn report_flag_writes_per_node_json_and_the_summary_names_both_dimensions() {
    let Some(python) = any_python() else {
        eprintln!("skipping: no Python interpreter available");
        return;
    };
    let dir = write_project();
    let report_path = dir.join("report.json");

    let out = Command::new(env!("CARGO_BIN_EXE_tiderace"))
        .arg("run")
        .arg("--workers")
        .arg("1")
        .arg("--report")
        .arg(&report_path)
        .arg(dir.join("tests"))
        .env("TIDERACE_PYTHON", &python)
        .env("TIDERACE_SHIM", repo_root().join("engine/py-shim/shim.py"))
        .output()
        .expect("the CLI runs");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("2 passed") && stderr.contains("2 skipped"),
        "the run itself is what we expect: {stderr}"
    );
    assert!(
        stderr.contains("(1 module skipped at import)"),
        "TID-55: the summary reports the skip *event* alongside the skipped tests, singular when \
         there is one — got: {stderr}"
    );

    let raw = std::fs::read_to_string(&report_path)
        .unwrap_or_else(|e| panic!("--report wrote a file at {}: {e}", report_path.display()));
    let json: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(json["total"], 4);
    assert_eq!(json["passed"], 2);
    assert_eq!(json["skipped"], 2);
    assert_eq!(json["skipped_modules"], 1);

    let ids: Vec<&str> = json["tests"]
        .as_array()
        .expect("per-node records")
        .iter()
        .filter_map(|t| t["node_id"].as_str())
        .collect();
    assert_eq!(ids.len(), 4, "one record per node, not a tally: {ids:?}");
    for want in [
        "test_plain.py::test_one",
        "test_plain.py::test_two",
        "test_absent.py::test_x",
        "test_absent.py::test_y",
    ] {
        assert!(
            ids.iter().any(|id| id.ends_with(want)),
            "{want} is reported by id so a consumer can diff sets of ids: {ids:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
