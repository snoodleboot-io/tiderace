//! Persistent worker pool (ADR-009, stage B).
//!
//! Spawns N long-lived Python workers (`worker.py`) that import pytest **once** and
//! run individual node ids fed as newline-delimited JSON. Because the workers stay
//! warm across `riptide watch` cycles, edit→test cycles after the first pay ~no
//! interpreter/pytest startup.
//!
//! Robustness (per the stage-B security review):
//! - Each request has a timeout; a hung test causes the worker to be killed and
//!   respawned, and the test is recorded as an Error — the run never hangs.
//! - A crashed worker (EOF on stdout) is detected and respawned the same way.
//! - Node ids are only ever carried inside JSON (serde escapes newlines), so a
//!   crafted file name cannot forge a second protocol line.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::collector::TestItem;
use crate::runner::{TestResult, TestStatus};

#[derive(Serialize)]
struct Request<'a> {
    nodeid: &'a str,
    invalidate: &'a [String],
}

#[derive(Deserialize)]
struct Response {
    status: String,
    duration_ms: i64,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Deserialize)]
struct Ready {
    #[allow(dead_code)]
    ready: bool,
}

/// A single long-lived worker process plus a background thread draining its stdout
/// into a channel (so reads can be bounded with `recv_timeout`).
/// Recycle a worker after this many requests, to bound memory/fd growth from
/// repeated in-process pytest sessions over a long `watch` session.
const MAX_WORKER_REQUESTS: u32 = 500;

struct Worker {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    requests: u32,
}

impl Worker {
    fn spawn(python: &str, worker_py: &Path, cwd: &Path) -> Result<Worker> {
        let mut cmd = Command::new(python);
        cmd.arg(worker_py)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::procutil::set_process_group(&mut cmd);
        let mut child = cmd.spawn().context("failed to spawn worker process")?;

        let stdin = child.stdin.take().context("worker stdin missing")?;
        let stdout = child.stdout.take().context("worker stdout missing")?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let worker = Worker {
            child,
            stdin,
            lines: rx,
            requests: 0,
        };
        // Block for the readiness handshake (pytest imported).
        let line = worker
            .lines
            .recv_timeout(Duration::from_secs(60))
            .context("worker did not report ready")?;
        let _: Ready = serde_json::from_str(&line).context("invalid ready handshake")?;
        Ok(worker)
    }

    /// Send one node id and await its response within `timeout`. Returns `None` if
    /// the worker timed out (hung test) or died mid-request — the caller respawns.
    fn run(&mut self, nodeid: &str, invalidate: &[String], timeout: Duration) -> Option<Response> {
        self.requests += 1;
        let req = Request { nodeid, invalidate };
        let mut line = serde_json::to_string(&req).ok()?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).ok()?;
        self.stdin.flush().ok()?;
        match self.lines.recv_timeout(timeout) {
            Ok(resp) => serde_json::from_str(&resp).ok(),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn kill(&mut self) {
        crate::procutil::kill_tree(&mut self.child);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A pool of warm workers. Lives across `watch` cycles so startup is amortised.
pub struct WorkerPool {
    python: String,
    worker_py: PathBuf,
    cwd: PathBuf,
    timeout: Duration,
    workers: Vec<Worker>,
}

impl WorkerPool {
    pub fn new(
        python: &str,
        worker_py: &Path,
        cwd: &Path,
        n_workers: usize,
        timeout: Duration,
    ) -> Result<Self> {
        let n = n_workers.max(1);
        let mut workers = Vec::with_capacity(n);
        for _ in 0..n {
            workers.push(Worker::spawn(python, worker_py, cwd)?);
        }
        Ok(WorkerPool {
            python: python.to_string(),
            worker_py: worker_py.to_path_buf(),
            cwd: cwd.to_path_buf(),
            timeout,
            workers,
        })
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Run a set of tests across the pool. `invalidate` lists files changed since the
    /// last cycle, whose modules the workers evict before running. Results come back
    /// in completion order.
    pub fn run_batch(&mut self, items: &[TestItem], invalidate: &[String]) -> Vec<TestResult> {
        if items.is_empty() {
            return Vec::new();
        }
        // Recycle workers that have served many requests, to bound long-session
        // memory/fd growth from repeated in-process pytest runs.
        for w in &mut self.workers {
            if w.requests >= MAX_WORKER_REQUESTS {
                if let Ok(fresh) = Worker::spawn(&self.python, &self.worker_py, &self.cwd) {
                    *w = fresh;
                }
            }
        }
        let by_node: HashMap<String, &TestItem> =
            items.iter().map(|t| (t.pytest_nodeid(), t)).collect();
        let queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(
            items.iter().map(|t| t.pytest_nodeid()).collect(),
        ));
        let (res_tx, res_rx) = mpsc::channel::<TestResult>();

        let timeout = self.timeout;
        let python = self.python.clone();
        let worker_py = self.worker_py.clone();
        let cwd = self.cwd.clone();
        let by_node = &by_node;

        thread::scope(|scope| {
            for worker in self.workers.iter_mut() {
                let queue = Arc::clone(&queue);
                let res_tx = res_tx.clone();
                let python = &python;
                let worker_py = &worker_py;
                let cwd = &cwd;
                scope.spawn(move || loop {
                    let nodeid = {
                        let mut q = queue.lock().unwrap();
                        q.pop_front()
                    };
                    let Some(nodeid) = nodeid else { break };
                    let item = by_node[&nodeid];

                    let result = match worker.run(&nodeid, invalidate, timeout) {
                        Some(resp) => response_to_result(item, &resp),
                        None => {
                            // Hung or crashed: kill, respawn a fresh worker so the
                            // pool stays healthy, and record this test as an error.
                            worker.kill();
                            if let Ok(fresh) = Worker::spawn(python, worker_py, cwd) {
                                *worker = fresh;
                            }
                            error_result(item, "test timed out or worker crashed")
                        }
                    };
                    let _ = res_tx.send(result);
                });
            }
        });

        drop(res_tx);
        res_rx.iter().collect()
    }
}

fn response_to_result(item: &TestItem, resp: &Response) -> TestResult {
    let status = match resp.status.as_str() {
        "passed" => TestStatus::Passed,
        "failed" => TestStatus::Failed,
        "skipped" => TestStatus::Skipped,
        _ => TestStatus::Error,
    };
    TestResult {
        test_id: item.test_id.clone(),
        file_path: item.file_path.clone(),
        status,
        duration_ms: resp.duration_ms,
        stdout: resp.summary.clone(),
        stderr: None,
        covered_files: Vec::new(),
    }
}

fn error_result(item: &TestItem, reason: &str) -> TestResult {
    TestResult {
        test_id: item.test_id.clone(),
        file_path: item.file_path.clone(),
        status: TestStatus::Error,
        duration_ms: 0,
        stdout: Some(format!("[riptide] {}", reason)),
        stderr: None,
        covered_files: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn request_framing_resists_newline_injection() {
        // Security review §1: a node id with a raw newline must NOT be able to
        // forge a second protocol line — serde escapes it, and the round trip
        // recovers the original.
        let req = Request {
            nodeid: "tests/t.py::test[\"a\nb\r\0\"]",
            invalidate: &[],
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n'), "raw newline leaked into the frame");
        assert!(!line.contains('\r'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["nodeid"], "tests/t.py::test[\"a\nb\r\0\"]");
    }

    #[test]
    fn response_maps_to_status() {
        let item = TestItem {
            test_id: "t.py::x".into(),
            file_path: "t.py".into(),
            function_name: "x".into(),
            class_name: None,
        };
        let r = response_to_result(
            &item,
            &Response {
                status: "passed".into(),
                duration_ms: 5,
                summary: None,
            },
        );
        assert_eq!(r.status, TestStatus::Passed);
        assert_eq!(r.duration_ms, 5);
        let r = response_to_result(
            &item,
            &Response {
                status: "weird".into(),
                duration_ms: 0,
                summary: None,
            },
        );
        assert_eq!(r.status, TestStatus::Error);
    }

    fn python_with_pytest() -> Option<String> {
        let candidates = [
            std::env::var("RIPTIDE_TEST_PYTHON").unwrap_or_default(),
            format!(
                "{}/.riptide-bench-venv/bin/python",
                env!("CARGO_MANIFEST_DIR")
            ),
            "python3".to_string(),
        ];
        candidates.into_iter().find(|py| {
            !py.is_empty()
                && Command::new(py)
                    .args(["-c", "import pytest"])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
        })
    }

    #[test]
    fn pool_runs_passing_and_failing_tests() {
        let Some(py) = python_with_pytest() else {
            eprintln!("skipping: no python with pytest");
            return;
        };
        let worker = Path::new(env!("CARGO_MANIFEST_DIR")).join("riptide/worker.py");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("conftest.py"),
            "import sys, os\nsys.path.insert(0, os.path.dirname(__file__))\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/test_p.py"),
            "def test_ok():\n    assert 1 == 1\n\ndef test_bad():\n    assert 1 == 2\n",
        )
        .unwrap();

        let mut pool = WorkerPool::new(&py, &worker, root, 2, Duration::from_secs(30)).unwrap();
        let items = vec![
            TestItem {
                test_id: "tests/test_p.py::test_ok".into(),
                file_path: "tests/test_p.py".into(),
                function_name: "test_ok".into(),
                class_name: None,
            },
            TestItem {
                test_id: "tests/test_p.py::test_bad".into(),
                file_path: "tests/test_p.py".into(),
                function_name: "test_bad".into(),
                class_name: None,
            },
        ];
        let results = pool.run_batch(&items, &[]);
        assert_eq!(results.len(), 2);
        let ok = results
            .iter()
            .find(|r| r.test_id.ends_with("test_ok"))
            .unwrap();
        let bad = results
            .iter()
            .find(|r| r.test_id.ends_with("test_bad"))
            .unwrap();
        assert_eq!(ok.status, TestStatus::Passed);
        assert_eq!(bad.status, TestStatus::Failed);
    }
}
