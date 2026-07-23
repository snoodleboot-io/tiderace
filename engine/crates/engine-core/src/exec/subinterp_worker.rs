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

use serde_json::{json, Value};

use crate::domain::{Outcome, TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::shim_protocol::{read_frame, write_frame};
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
            stdout,
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

        // … one batch back.
        let resp: Value = read_frame(&mut proc.stdout)
            .map_err(|e| EngineError::Exec(format!("subinterp results read: {e}")))?
            .ok_or_else(|| EngineError::Exec("subinterp closed mid-batch".into()))?;
        let results = resp
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| EngineError::Exec("subinterp response missing `results`".into()))?;

        // Index by node id, then rebuild in the caller's order (a missing node ⇒ Error, never dropped).
        let by_node: HashMap<&str, (&str, &str)> = results
            .iter()
            .filter_map(|r| {
                let node = r.get("node_id")?.as_str()?;
                let outcome = r.get("outcome").and_then(Value::as_str).unwrap_or("error");
                let detail = r.get("detail").and_then(Value::as_str).unwrap_or("");
                Some((node, (outcome, detail)))
            })
            .collect();

        Ok(items
            .iter()
            .map(|it| {
                let (outcome, detail) = by_node
                    .get(it.node_id.as_str())
                    .copied()
                    .unwrap_or(("error", "no result returned by subinterp pool"));
                TestResult::new(it.node_id.clone(), Outcome::from_wire(outcome), 0, detail)
            })
            .collect())
    }
}

/// A live `--subinterp` process + its pipes (mirrors `NoForkProc`). `stdin` is an `Option` so `Drop`
/// can close the write half (→ shim EOF → workers stopped → exit) before reaping.
struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Proc {
    fn stdin(&mut self) -> &mut ChildStdin {
        self.stdin.as_mut().expect("subinterp stdin open")
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.stdin.take(); // close write half → EOF → the shim stops its workers and exits
        let mut sink = Vec::new();
        let _ = self.stdout.get_mut().read_to_end(&mut sink); // drain, then reap
        let _ = self.child.wait();
    }
}
