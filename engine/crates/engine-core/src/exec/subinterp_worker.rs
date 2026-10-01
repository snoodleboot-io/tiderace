//! `SubInterpWorker` (ADR-E015 Phase 2) — the **sub-interpreter** executor. Runs a batch of
//! *sub-interpreter-safe* tests across a pool of isolated sub-interpreters in **one process**, parallel
//! via per-interpreter GILs (PEP 684 / `concurrent.interpreters`, CPython 3.14+) — **no fork, no
//! snapshot/restore across workers**. Per-interpreter state isolates the workers from each other; each
//! worker's `Engine` runs with `restore=True` for per-test isolation *within* an interpreter.
//!
//! Its clear win is **Windows** (no `fork()`): today the safe subset runs sequentially
//! ([`SubprocessWorker`](crate::exec::SubprocessWorker)); this runs it in parallel. The caller only
//! routes safe modules here (the probe classifies them, ADR-E015 Phase 1/3).
//!
//! Unlike the fork/no-fork paths (one request→one response per test), this uses a **batch** exchange:
//! send one `{"batch": [...]}` frame, receive one `{"results": [...]}` frame — the shim fans the batch
//! out across the pool and streams the results back. Result-identical to `ForkWorker` on the safe subset.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::domain::{Outcome, TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::shim_protocol::{read_frame, write_frame, ExecResponse};
use crate::exec::transport::{results_for, LOST_WORKER_MARGIN_MS};
use crate::exec::worker::Worker;

/// Sub-interpreter-pool executor (ADR-E015). `pool_size = None` ⇒ the shim's default (CPU count).
#[derive(Debug)]
pub struct SubInterpWorker {
    deadline_ms: u64,
    pool_size: Option<usize>,
    target: Option<Target>,
}

#[derive(Debug, Clone)]
struct Target {
    python: String,
    shim: PathBuf,
    root: PathBuf,
}

impl SubInterpWorker {
    /// Construct with a per-test deadline (ms).
    pub fn new(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            pool_size: None,
            target: None,
        }
    }

    /// Point at an interpreter + shim + corpus root (the no-COW analogue of `ForkWorker::launch`'s args).
    pub fn with_target(mut self, python: impl Into<String>, shim: &Path, root: &Path) -> Self {
        self.target = Some(Target {
            python: python.into(),
            shim: shim.to_path_buf(),
            root: root.to_path_buf(),
        });
        self
    }

    /// Fix the sub-interpreter pool size (default: the shim picks CPU count).
    pub fn with_pool_size(mut self, n: usize) -> Self {
        self.pool_size = Some(n);
        self
    }

    /// Launch `python <shim> <root> --subinterp` and complete the readiness handshake.
    fn launch(target: &Target, pool_size: Option<usize>) -> Result<Proc> {
        let mut cmd = Command::new(&target.python);
        cmd.arg(&target.shim)
            .arg(&target.root)
            .arg("--subinterp")
            // Pin native thread pools — parallel workers must not oversubscribe BLAS/OMP.
            .env("OPENBLAS_NUM_THREADS", "1")
            .env("OMP_NUM_THREADS", "1")
            .env("MKL_NUM_THREADS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some(n) = pool_size {
            cmd.env("TIDERACE_SUBINTERP_WORKERS", n.to_string());
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| EngineError::Exec(format!("failed to launch subinterp worker: {e}")))?;
        let stdin = Some(
            child
                .stdin
                .take()
                .ok_or_else(|| EngineError::Exec("subinterp stdin unavailable".into()))?,
        );
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| EngineError::Exec("subinterp stdout unavailable".into()))?,
        );
        let ready: Option<Value> = read_frame(&mut stdout)
            .map_err(|e| EngineError::Exec(format!("subinterp ready: {e}")))?;
        if ready.and_then(|v| v.get("ready").and_then(Value::as_bool)) != Some(true) {
            return Err(EngineError::Exec("subinterp failed to warm".into()));
        }
        Ok(Proc {
            child,
            stdin,
            stdout: Some(stdout),
        })
    }
}

impl Worker for SubInterpWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let target = self.target.clone().ok_or_else(|| {
            EngineError::Exec("SubInterpWorker has no target; call with_target".into())
        })?;
        let mut proc = SubInterpWorker::launch(&target, self.pool_size)?;

        // One batch out …
        let batch: Vec<Value> = items
            .iter()
            .map(|it| {
                json!({
                    "node_id": it.node_id.as_str(),
                    "style": it.style.wire(),
                    "deadline_ms": self.deadline_ms,
                })
            })
            .collect();
        write_frame(proc.stdin(), &json!({ "batch": batch }))
            .map_err(|e| EngineError::Exec(format!("subinterp batch write: {e}")))?;

        // … one batch back — within a budget (TID-104). A test that blocks in a sub-interpreter
        // cannot be interrupted from inside (no signal lands there, and the watchdog thread
        // cannot be a daemon), so the pool's reply is read on a thread and waited for at most
        // the batch's share of the deadline plus the lost-worker margin; past that the pool is
        // killed and the batch reported — the shim's own per-task watchdog answers first when it
        // can, naming the task that blocked.
        let pool_size = self.pool_size.unwrap_or_else(default_pool_size).max(1);
        let rounds = items.len().div_ceil(pool_size) as u64;
        let budget = Duration::from_millis(
            self.deadline_ms
                .saturating_mul(rounds)
                .saturating_add(LOST_WORKER_MARGIN_MS),
        );
        let mut stdout = proc.stdout.take().expect("the pool's stdout is open");
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tiderace-subinterp-reader".into())
            .spawn(move || {
                let frame = read_frame::<_, Value>(&mut stdout);
                let _ = tx.send((frame, stdout));
            })
            .map_err(|e| EngineError::Exec(format!("subinterp reader thread: {e}")))?;
        let (frame, stdout) = match rx.recv_timeout(budget) {
            Ok(got) => got,
            Err(_) => {
                let _ = proc.child.kill();
                let fault = format!(
                    "no result from the sub-interpreter pool within {:.0}s — a test in this \
                     batch blocked where nothing could interrupt it; the pool was killed (TID-104)",
                    budget.as_secs_f64()
                );
                return Ok(items
                    .iter()
                    .map(|it| TestResult::new(it.node_id.clone(), Outcome::Error, 0, fault.clone()))
                    .collect());
            }
        };
        proc.stdout = Some(stdout);
        let resp: Value = frame
            .map_err(|e| EngineError::Exec(format!("subinterp results read: {e}")))?
            .ok_or_else(|| EngineError::Exec("subinterp closed mid-batch".into()))?;
        let results = resp
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| EngineError::Exec("subinterp response missing `results`".into()))?;

        // Each result is the shim's whole response for its node — expansion, skips, keywords —
        // and reads as one does on every other transport (TID-104). Indexed by node id, then
        // rebuilt in the caller's order; a node with no response is an error, never dropped.
        let mut by_node: HashMap<String, ExecResponse> = HashMap::new();
        for r in results {
            if let Ok(resp) = serde_json::from_value::<ExecResponse>(r.clone()) {
                by_node.insert(resp.node_id.clone(), resp);
            }
        }
        Ok(items
            .iter()
            .flat_map(|it| match by_node.remove(it.node_id.as_str()) {
                Some(resp) => results_for(it, resp, 0),
                None => vec![TestResult::new(
                    it.node_id.clone(),
                    Outcome::Error,
                    0,
                    "no result returned by subinterp pool",
                )],
            })
            .collect())
    }
}

/// The pool size the shim takes when none is given: its own default, the CPU count.
fn default_pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// A live `--subinterp` process + its pipes (mirrors `NoForkProc`). `stdin` is an `Option` so `Drop`
/// can close the write half (→ shim EOF → workers stopped → exit) before reaping.
struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    /// Taken by the reader thread for the batch's reply and put back after (TID-104).
    stdout: Option<BufReader<ChildStdout>>,
}

impl Proc {
    fn stdin(&mut self) -> &mut ChildStdin {
        self.stdin.as_mut().expect("subinterp stdin open")
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.stdin.take(); // close write half → EOF → the shim stops its workers and exits
        if let Some(stdout) = self.stdout.as_mut() {
            let mut sink = Vec::new();
            let _ = stdout.get_mut().read_to_end(&mut sink); // drain, then reap
        }
        let _ = self.child.wait();
    }
}
