//! TID-76 — coverage capture records files by default, lines only when asked, and sees every file a
//! test enters rather than only the files no earlier test in the same process entered first.
//!
//! Nothing on a production path reads a line number: the transport reduces the wire footprint to
//! its file names before anyone else sees it, and the daemon selects by file. Yet every test paid for
//! LINE events on every executed line, a sort per file, a JSON encode, and a serde parse of the
//! result — a third of a cold pirn-core run. The default is now `PY_START` (one event per code
//! object entered) with empty line lists, the convention the import closure already uses for "any
//! change to this file counts". Line-level capture is opt-in.
//!
//! The second defect is quieter. `sys.monitoring.DISABLE` is per location and survives
//! `free_tool_id`; only `restart_events()` clears it. Without that call the first test in a process to
//! enter a function is the only one ever credited with the file — the same shape as TID-40, but for
//! files the static import closure cannot see because nothing imports them by name.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ExecRequest, PipeTransport, ShimTransport, SubprocessWorker, Worker};
use engine_core::testing::skip_live;

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
    for cand in ["python3", "python"] {
        let ok = Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

fn fresh_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t76_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("src/__init__.py"), "").unwrap();
    dir
}

/// A source file no test module imports by name: the conftest loads it through `importlib`, and the
/// tests reach it through `sys.modules`. The static import closure (TID-40) cannot see it, so what
/// the footprint says about it comes from runtime capture alone.
fn write_dynamic_corpus(tests: usize) -> PathBuf {
    let dir = fresh_dir("dynamic");
    std::fs::write(
        dir.join("conftest.py"),
        "import importlib, os, sys\nsys.path.insert(0, os.path.dirname(__file__))\n\
         importlib.import_module('src.lazy')\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lazy.py"), "def bump(x):\n    return x + 1\n").unwrap();
    let mut body = String::from("import sys\n\n\n");
    for t in 0..tests {
        body.push_str(&format!(
            "def test_case_{t}():\n\x20   assert sys.modules['src.lazy'].bump({t}) == {}\n\n\n",
            t + 1
        ));
    }
    std::fs::write(dir.join("tests/test_dynamic.py"), body).unwrap();
    dir
}

/// Every test that enters a file carries it — not only the first test in the process to do so.
///
/// The no-fork worker runs its batch in one interpreter, which is the case where a per-location
/// `DISABLE` from one test would silence the next.
#[test]
fn every_test_carries_a_file_it_reaches_only_through_a_dynamic_import() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_dynamic_corpus(3);
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3);

    std::env::set_var("TIDERACE_COVERAGE", "1");
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    std::env::remove_var("TIDERACE_COVERAGE");

    assert_eq!(results.len(), 3);
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}", r.detail);
    }
    let carrying: Vec<&str> = results
        .iter()
        .filter(|r| r.touched_files.iter().any(|f| f == "src/lazy.py"))
        .map(|r| r.node_id.as_str())
        .collect();
    assert_eq!(
        carrying.len(),
        3,
        "TID-76: all 3 tests enter src/lazy.py, only {carrying:?} recorded it. A `DISABLE`d \
         location stays disabled after the tool id is freed; capture must `restart_events()` per test."
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// One request→response over the real shim protocol, with the given extra shim flags.
fn exchange_one(
    python: &str,
    dir: &Path,
    flags: &[&str],
    node_id: &str,
) -> engine_core::exec::ExecResponse {
    let mut child = Command::new(python)
        .arg(shim())
        .arg(dir)
        .args(["--no-fork", "--restore"])
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("shim launches");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut transport = PipeTransport::new(stdin, stdout);
    transport.ready().expect("handshake");
    let resp = transport
        .exchange(&ExecRequest::bare(node_id, "function", 30_000))
        .expect("exchange");
    transport.close_input();
    let _ = child.wait();
    resp
}

fn write_plain_corpus() -> PathBuf {
    let dir = fresh_dir("plain");
    std::fs::write(
        dir.join("conftest.py"),
        "import os, sys\nsys.path.insert(0, os.path.dirname(__file__))\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/thing.py"),
        "def triple(x):\n    return x * 3\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tests/test_plain.py"),
        "from src.thing import triple\n\n\ndef test_triple():\n    assert triple(2) == 6\n",
    )
    .unwrap();
    dir
}

/// The default footprint names files and carries no line numbers — the shape the consumers read.
#[test]
fn default_capture_reports_files_without_lines() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_plain_corpus();
    let resp = exchange_one(
        &python,
        &dir,
        &["--coverage"],
        "tests/test_plain.py::test_triple",
    );
    assert_eq!(resp.outcome, "passed", "{}", resp.detail);
    let thing = resp
        .coverage
        .get("src/thing.py")
        .unwrap_or_else(|| panic!("src/thing.py in footprint: {:?}", resp.coverage.keys()));
    assert!(
        thing.is_empty(),
        "TID-76: the default footprint carries line numbers nobody reads: {thing:?}"
    );
    assert!(
        resp.coverage.contains_key("tests/test_plain.py"),
        "the test's own file is in its footprint: {:?}",
        resp.coverage.keys()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Line-level capture is still there for a consumer that wants it — behind a flag.
#[test]
fn coverage_lines_is_an_opt_in() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_plain_corpus();
    let resp = exchange_one(
        &python,
        &dir,
        &["--coverage", "--coverage-lines"],
        "tests/test_plain.py::test_triple",
    );
    assert_eq!(resp.outcome, "passed", "{}", resp.detail);
    let thing = resp
        .coverage
        .get("src/thing.py")
        .expect("src/thing.py in footprint");
    assert_eq!(
        thing,
        &vec![2],
        "with --coverage-lines the footprint says which lines ran: `return x * 3` is line 2"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
