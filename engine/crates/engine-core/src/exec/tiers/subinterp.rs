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

/// The sub-interpreter tier for one run (ADR-E015 / TID-11, TID-118): probe each module, run the
/// **safe** subset on one parallel sub-interpreter pool, and hand everything else to the
/// platform fallback's lanes.
///
/// It is hybrid by necessity rather than by policy. A sub-interpreter cannot load a single-phase
/// C extension — numpy's `_multiarray_umath` is the canonical refusal — so "run this whole corpus
/// on sub-interpreters" is not a configuration that exists for any corpus with a compiled
/// dependency. The pool is not threaded per scheduler unit either: `SubInterpWorker` takes a
/// whole batch and fans it out across its own interpreters in one process, so threading it per
/// unit would nest two pools and oversubscribe the machine.
///
/// A probe that cannot classify a module (CPython < 3.14, no probe API) returns `None`, and
/// `None` routes to the fallback — always sound, never wrong, just not accelerated.
pub struct SubInterpTier<'a> {
    target: ShimTarget,
    deadline_ms: u64,
    workers: usize,
    fallback: Box<dyn crate::exec::tier::TierFactory + 'a>,
}

impl<'a> SubInterpTier<'a> {
    pub fn new(
        target: ShimTarget,
        plan: &crate::runner::RunPlan,
        fallback: Box<dyn crate::exec::tier::TierFactory + 'a>,
    ) -> Self {
        Self {
            target,
            deadline_ms: plan.deadline_ms,
            workers: plan.workers.get(),
            fallback,
        }
    }
}

impl crate::exec::tier::TierFactory for SubInterpTier<'_> {
    fn claim(
        &mut self,
        items: Vec<TestItem>,
        notes: &mut crate::runner::RunNotes,
    ) -> Result<(Vec<TestResult>, Vec<TestItem>)> {
        let mut modules: Vec<String> = items.iter().map(|i| i.node_id.file().to_string()).collect();
        modules.sort();
        modules.dedup();

        // Probing means launching a fresh interpreter per module, so it is cached by content hash
        // and only new or changed modules pay (TID-35). Without this the CLI re-probed the whole
        // corpus on every invocation, which on a small module count is most of this tier's cost —
        // and it hurt most on Windows, the one platform the tier exists for and the one with no
        // daemon to lean on.
        let mut cache = crate::exec::SafeSetCache::load(&self.target.root);
        let safe = cache
            .resolve(
                &self.target.python,
                &self.target.shim,
                &self.target.root,
                &modules,
            )
            .map_err(EngineError::Exec)?;
        // Best-effort: an unwritable tree must still run, just without the speedup next time —
        // but say so, or the re-probe on every run looks like the tier being slow.
        if let Err(e) = cache.save(&self.target.root) {
            notes.push(format!("sub-interpreter safe-set cache not saved: {e}"));
        }

        let (safe_items, rest): (Vec<TestItem>, Vec<TestItem>) = items
            .into_iter()
            .partition(|it| safe.contains(it.node_id.file()));
        let mut results = Vec::new();
        if !safe_items.is_empty() {
            let pool = self.workers.max(1).min(safe_items.len().max(1));
            let mut worker = SubInterpWorker::new(self.deadline_ms)
                .with_shim_target(self.target.clone())
                .with_pool_size(pool);
            results.extend(worker.run(&safe_items)?);
        }
        Ok((results, rest))
    }

    fn prepare(
        &mut self,
        lanes: usize,
        modules: &Path,
        notes: &mut crate::runner::RunNotes,
    ) -> Result<usize> {
        self.fallback.prepare(lanes, modules, notes)
    }

    fn lane(
        &mut self,
        index: usize,
        modules: &Path,
    ) -> Result<Box<dyn crate::exec::tier::LaneSeed>> {
        self.fallback.lane(index, modules)
    }
}
