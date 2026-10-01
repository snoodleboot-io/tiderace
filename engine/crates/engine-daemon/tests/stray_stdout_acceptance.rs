//! TID-103 — a test that writes to stdout without capture does not corrupt the engine's
//! protocol, in the warm image or in a one-shot worker.
//!
//! The shim's frames are length-prefixed over fd 1. In the warm image (TID-84) the parent's
//! stdout is the daemon's control pipe, which every forked worker inherits as its fd 1: the
//! first test that printed put bytes in front of the next spawn's acknowledgement, which the
//! daemon read as a frame length and waited on forever. click's suite hung the daemon on its
//! second run, deterministically. The protocol now owns a private duplicate of fd 1, and fd 1
//! itself goes to stderr.

#![cfg(unix)]

use engine_core::collection::Collector;
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use engine_daemon::{EngineHandler, RpcHandler, RpcRequest, RpcResponse};
use std::sync::mpsc;
use std::time::Duration;

/// Every way a test reaches fd 1 past the shim: Python's buffered `print` with and without a
/// newline, a flush, a raw `os.write`, and a subprocess that inherits stdout.
const NOISY: &str = "\
import os
import subprocess
import sys


def test_prints_without_newline():
    print(\"noise\", end=\"\")
    assert True


def test_prints_a_line_and_flushes():
    print(\"a whole line\")
    sys.stdout.flush()
    assert True


def test_writes_raw_bytes_to_fd_1():
    os.write(1, b\"\\x00\\x00\\x00\\xff raw bytes that look like a frame length\")
    assert True


def test_subprocess_inherits_stdout():
    subprocess.run([sys.executable, \"-c\", \"print('child output')\"], check=True)
    assert True


def test_quiet():
    assert True
";

#[test]
fn stray_stdout_does_not_hang_the_warm_image_or_the_one_shot_worker() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t103_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_noisy.py"), NOISY).unwrap();

    // The daemon: a full run (the workers print), then a second run — whose spawn used to read
    // the printed bytes as its acknowledgement. The handler runs on a thread so a regression
    // fails in a minute rather than hanging the suite.
    let (tx, rx) = mpsc::channel();
    let handler_dir = dir.clone();
    let handler_python = python.clone();
    std::thread::spawn(move || {
        let mut handler = EngineHandler::new(handler_python, shim(), handler_dir);
        let full = handler.run_full_parallel().expect("full run");
        let again = match handler.handle(RpcRequest::RunFull {
            keyword: Some("quiet or raw".into()),
            marker: None,
            strict_markers: false,
        }) {
            RpcResponse::RanFull { results } => results,
            other => panic!("expected RanFull, got {other:?}"),
        };
        let third = handler.run_full_parallel().expect("third run");
        tx.send((full, again, third)).unwrap();
    });
    let (full, again, third) = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("TID-103: the run after a test printed to stdout must not hang the daemon");
    assert_eq!(full.len(), 5, "{full:?}");
    assert!(
        full.iter().all(|r| r.outcome == "passed"),
        "a print is not a failure: {full:?}"
    );
    let mut ids: Vec<&str> = again.iter().map(|r| r.node_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "test_noisy.py::test_quiet",
            "test_noisy.py::test_writes_raw_bytes_to_fd_1"
        ],
        "{again:?}"
    );
    assert_eq!(third.len(), 5, "{third:?}");

    // The one-shot worker: its stdout *is* the result stream, so the same prints used to
    // desynchronise it (an error for the printing test, or a lost worker).
    let items = engine_core::collection::RegexCollector::new()
        .collect(&dir)
        .expect("collection");
    let mut worker =
        engine_core::exec::SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = engine_core::exec::Worker::run(&mut worker, &items).expect("one-shot run");
    assert_eq!(results.len(), 5);
    for r in &results {
        assert_eq!(
            r.outcome,
            engine_core::domain::Outcome::Passed,
            "{}: {}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
