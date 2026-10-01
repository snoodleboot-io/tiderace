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
use std::path::Path;
use std::process::ChildStdin;
use std::time::Duration;

use serde_json::{json, Value};

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::process::{BudgetedReader, ShimLaunch, ShimMode, ShimProcess, ShimTarget};
use crate::exec::results::NotRun;
use crate::exec::shim_protocol::{ready_info, write_frame, ExecRequest, ExecResponse};
use crate::exec::transport::{ReadyInfo, ShimTransport, LOST_WORKER_MARGIN_MS};
use crate::exec::worker::Worker;

/// Sub-interpreter-pool executor (ADR-E015). `pool_size = None` ⇒ the shim's default (CPU count).
#[derive(Debug)]
pub struct SubInterpWorker {
    deadline_ms: u64,
    pool_size: Option<usize>,
    target: Option<ShimTarget>,
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
        self.target = Some(ShimTarget::new(python, shim, root));
        self
    }

    /// [`with_target`](Self::with_target) from a [`ShimTarget`].
    pub fn with_shim_target(mut self, target: ShimTarget) -> Self {
        self.target = Some(target);
        self
    }

    /// Fix the sub-interpreter pool size (default: the shim picks CPU count).
    pub fn with_pool_size(mut self, n: usize) -> Self {
        self.pool_size = Some(n);
        self
    }

    /// Launch `python <shim> <root> --subinterp [--pool-size N]` and complete the readiness
    /// handshake.
    fn launch(target: &ShimTarget, pool_size: Option<usize>) -> Result<Proc> {
        let mut process =
            ShimLaunch::new(target, ShimMode::SubInterp { pool: pool_size }).spawn()?;
        let (stdin, stdout) = process.take_pipes()?;
        let mut transport = SubInterpTransport {
            stdin: Some(stdin),
            frames: BudgetedReader::spawn(stdout, "tiderace-subinterp-reader"),
            budget: Duration::ZERO,
        };
        transport.ready()?;
        Ok(Proc { transport, process })
    }
}

impl Worker for SubInterpWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        let target = self.target.clone().ok_or(EngineError::NoTarget {
            worker: "SubInterpWorker",
        })?;
        let mut proc = SubInterpWorker::launch(&target, self.pool_size)?;

        // One batch out, one batch back — within a budget (TID-104). A test that blocks in a
        // sub-interpreter cannot be interrupted from inside (no signal lands there, and the
        // watchdog thread cannot be a daemon), so the pool's reply is waited for at most the
        // batch's share of the deadline plus the lost-worker margin; past that the pool is
        // killed and the batch reported — the shim's own per-task watchdog answers first when it
        // can, naming the task that blocked.
        let pool_size = self.pool_size.unwrap_or_else(default_pool_size).max(1);
        let rounds = items.len().div_ceil(pool_size) as u64;
        let budget = Duration::from_millis(
            self.deadline_ms
                .saturating_mul(rounds)
                .saturating_add(LOST_WORKER_MARGIN_MS),
        );
        proc.transport.budget = budget;
        let reqs: Vec<ExecRequest<'_>> = items
            .iter()
            .map(|it| ExecRequest::bare(&it.node_id, it.style, self.deadline_ms))
            .collect();
        let responses = match proc.transport.exchange_batch(&reqs) {
            Ok(responses) => responses,
            Err(_) if proc.transport.frames.is_lost() => {
                proc.process.kill();
                return Ok(items
                    .iter()
                    .map(|it| {
                        TestResult::not_run(
                            it.node_id.clone(),
                            NotRun::PoolKilled { budget },
                            Duration::ZERO,
                        )
                    })
                    .collect());
            }
            Err(e) => return Err(e),
        };

        // Each response is the shim's whole one for its node — expansion, skips, keywords — and
        // reads as one does on every other transport (TID-104). Indexed by node id, then
        // rebuilt in the caller's order; a node with no response is an error, never dropped.
        let mut by_node: HashMap<String, ExecResponse> = responses
            .into_iter()
            .map(|r| (r.node_id.to_string(), r))
            .collect();
        Ok(items
            .iter()
            .flat_map(|it| match by_node.remove(it.node_id.as_str()) {
                Some(resp) => resp.into_results(it, Duration::ZERO),
                None => vec![TestResult::not_run(
                    it.node_id.clone(),
                    NotRun::NoReply,
                    Duration::ZERO,
                )],
            })
            .collect())
    }
}

/// The sub-interpreter pool's protocol: one `{"batch": [...]}` frame out, one
/// `{"results": [...]}` frame back, the shim fanning the batch across its interpreters. A
/// single exchange is a batch of one.
struct SubInterpTransport {
    stdin: Option<ChildStdin>,
    frames: BudgetedReader,
    budget: Duration,
}

impl ShimTransport for SubInterpTransport {
    fn ready(&mut self) -> Result<ReadyInfo> {
        let frame: Value = self
            .frames
            .next(None)?
            .ok_or(EngineError::NoReadyFrame { what: "subinterp" })?;
        ready_info(frame)
    }

    fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse> {
        self.exchange_batch(std::slice::from_ref(req))?
            .pop()
            .ok_or_else(|| {
                EngineError::Protocol("subinterp answered a batch of one with nothing".into())
            })
    }

    fn exchange_batch(&mut self, reqs: &[ExecRequest<'_>]) -> Result<Vec<ExecResponse>> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(EngineError::AlreadyShutDown { what: "subinterp" })?;
        write_frame(stdin, &json!({ "batch": reqs }))?;
        let resp: Value = self
            .frames
            .next(Some(self.budget))?
            .ok_or(EngineError::PeerClosed { what: "subinterp" })?;
        let results = resp
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| EngineError::Protocol("subinterp response missing `results`".into()))?;
        Ok(results
            .iter()
            .filter_map(|r| serde_json::from_value::<ExecResponse>(r.clone()).ok())
            .collect())
    }
}

/// The pool size the shim takes when none is given: its own default, the CPU count.
fn default_pool_size() -> usize {
    crate::exec::limits::default_parallelism()
}

/// A live `--subinterp` process + its transport. Fields drop in order: the transport first
/// (its write half closes → EOF → the shim stops its workers and exits), then the process,
/// which reaps it.
struct Proc {
    transport: SubInterpTransport,
    process: ShimProcess,
}
