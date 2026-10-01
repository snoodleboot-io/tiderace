//! The shim **transport** seam — the one thing a [`Worker`](crate::exec::Worker) needs from the
//! world below it: a synchronous request→response exchange with a shim.
//!
//! Until now "talk to the shim" was hand-inlined twice (once in [`Wellspring`](crate::exec::Wellspring),
//! once in `SubprocessWorker`'s `NoForkProc`) and the per-item run loop a third time (in both workers).
//! Both were welded to `ChildStdin`/`ChildStdout` — i.e. to a real OS process reached over pipes, which
//! means **no execution-path logic could be tested without `fork`/`exec`/a live venv**. The live
//! acceptance scenarios early-return `SKIP` when `.tiderace-fx-venv` is absent, so in CI-without-Python
//! the entire `Worker → frames → TestResult` path was simply *unverified*.
//!
//! [`ShimTransport`] names that boundary. Production wires it to a process over pipes
//! ([`PipeTransport`]); tests wire it to a pure-Rust object **in the same thread** (the `tests` module's
//! `ScriptedShim`) — same [`run_batch`] loop, zero syscalls, fully deterministic. This is also the seam
//! a future in-process / FFI backend (Rust-as-Python-extension, ADR ②) slots behind without touching
//! any `Worker`.

use std::io::{Read, Write};
use std::process::ChildStdin;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::knobs::RunKnobs;
use crate::exec::process::BudgetedReader;
use crate::exec::results::NotRun;
use crate::exec::shim_protocol::{ready_info, write_frame, ExecRequest, ExecResponse};

/// What a shim reports in its readiness handshake (the first frame it sends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyInfo {
    /// The shim/wellspring process id; `None` when a transport has no process of its own.
    pub pid: Option<u32>,
}

/// One synchronous request→response exchange with a shim. At most one request is ever in flight,
/// matching the existing wellspring/subprocess protocol (a dedicated reader, no pipelining).
///
/// The seam behind which live ([`PipeTransport`]) and in-process (test doubles; future FFI) shims are
/// interchangeable. Implementors own *how* a frame travels; they never own scheduling or result policy.
pub trait ShimTransport {
    /// Consume the shim's readiness handshake. Called once, before any [`exchange`](Self::exchange).
    fn ready(&mut self) -> Result<ReadyInfo>;

    /// Send one [`ExecRequest`] and block for its [`ExecResponse`].
    fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse>;

    /// Send a batch and block for every response, in request order. The default is one
    /// exchange after another; a transport whose shim takes whole batches (the sub-interpreter
    /// pool) answers them in one frame.
    fn exchange_batch(&mut self, reqs: &[ExecRequest<'_>]) -> Result<Vec<ExecResponse>> {
        reqs.iter().map(|req| self.exchange(req)).collect()
    }
}

/// What a batch does when its worker stops answering mid-batch (TID-93).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LostWorker {
    /// Fail the batch: the error propagates (a one-shot worker's owner relaunches or gives up).
    Fail,
    /// Report it: the in-flight node is an error naming the fault, every node after it in the
    /// batch is an error naming the node that took the worker down, and the batch returns `Ok` —
    /// the caller reads [`run_batch_lost`] to learn the worker is gone.
    Report,
}

/// Drive a whole batch through a transport, building one [`TestResult`] per item.
///
/// This is the per-item loop formerly copy-pasted into `ForkWorker::run` and `SubprocessWorker::run`;
/// both now delegate here, and tests drive it against an in-process [`ShimTransport`] with no process
/// at all. Requests are [`ExecRequest::bare`] — Phase 3 live fixture discovery lives in the shim, so
/// the wire request carries no fixture fields (CONTRACT §11.2).
pub(crate) fn run_batch<T: ShimTransport + ?Sized>(
    transport: &mut T,
    items: &[TestItem],
    knobs: &RunKnobs,
) -> Result<Vec<TestResult>> {
    run_batch_lost(transport, items, knobs, LostWorker::Fail).map(|(results, _)| results)
}

/// [`run_batch`] with a [`LostWorker`] policy; the second value names the fault that took the
/// worker down, when one did.
pub(crate) fn run_batch_lost<T: ShimTransport + ?Sized>(
    transport: &mut T,
    items: &[TestItem],
    knobs: &RunKnobs,
    on_lost: LostWorker,
) -> Result<(Vec<TestResult>, Option<String>)> {
    let RunKnobs {
        deadline_ms,
        optimistic_no_fork: force_no_fork,
        trusted_pure: trusted,
        must_fork,
    } = knobs;
    let (deadline_ms, force_no_fork) = (*deadline_ms, *force_no_fork);
    let mut results = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let mut req = ExecRequest::bare(&item.node_id, item.style, deadline_ms);
        // TID-33: a test recorded as disturbing interpreter state never takes the in-process ladder
        // again. The shim still catches a first offence at runtime and re-runs it forked, but that
        // costs a wasted in-process run every time; this is what stops paying it repeatedly.
        let recorded_disturber = must_fork.contains(item.node_id.as_str());
        let force_no_fork = force_no_fork && !recorded_disturber;
        req.force_no_fork = force_no_fork; // optimistic no-fork; the shim forks non-restorable modules
        req.must_fork = recorded_disturber; // TID-96: the shim gives it the module-child route
                                            // TID-1: a recorded-pure, unchanged test runs BARE no-fork (skip the snapshot). Only meaningful
                                            // on a no-fork request; the shim ignores it otherwise.
        req.trusted_pure = force_no_fork && trusted.contains(item.node_id.as_str());
        let start = Instant::now();
        let resp = match transport.exchange(&req) {
            Ok(resp) => resp,
            Err(e) if on_lost == LostWorker::Report => {
                // The worker is gone — hung past its deadline, or dead. Say so per node rather
                // than losing the batch (TID-93): this node names the fault, the rest name it.
                let fault = format!("{e}");
                results.push(TestResult::not_run(
                    item.node_id.clone(),
                    NotRun::WorkerFault {
                        fault: fault.clone(),
                    },
                    start.elapsed(),
                ));
                for later in &items[index + 1..] {
                    results.push(TestResult::not_run(
                        later.node_id.clone(),
                        NotRun::AfterLostWorker {
                            at: item.node_id.clone(),
                            fault: fault.clone(),
                        },
                        Duration::ZERO,
                    ));
                }
                return Ok((results, Some(fault)));
            }
            Err(e) => return Err(e),
        };
        results.extend(resp.into_results(item, start.elapsed()));
    }
    Ok((results, None))
}

/// The write half of a transport, and how it is closed. Dropping a pipe's write end is its
/// EOF; a socket shared with the reader thread (the pool's workers) must be shut down for
/// writing explicitly, or the worker never sees EOF while that thread still holds a clone.
pub trait WriteHalf: Write {
    /// Tell the peer no more will be written.
    fn close(self)
    where
        Self: Sized,
    {
    }
}

impl WriteHalf for ChildStdin {}
impl WriteHalf for std::io::PipeWriter {}
#[cfg(unix)]
impl WriteHalf for std::os::unix::net::UnixStream {
    fn close(self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

/// The production transport: length-prefixed JSON frames over a write half and a read half —
/// a child process's `stdin`/`stdout` ([`Live`]), a pooled worker's Unix socket, or an in-memory
/// pipe in a test. The read half is drained by a [`BudgetedReader`] thread, so a reply can be
/// waited for at most a [`budget`](Self::with_budget) (TID-98): the one per-test deadline that
/// holds when the shim cannot interrupt its own test, and the only one Windows has.
pub struct PipeTransport<W: WriteHalf> {
    /// `Option` so [`close_input`](Self::close_input) can drop the write half (→ shim EOF/exit)
    /// *before* the owner reaps the child — the ordering that avoids a deadlock on shutdown.
    stdin: Option<W>,
    frames: BudgetedReader,
    /// How long a reply may take; `None` waits as long as it takes.
    budget: Option<Duration>,
    /// The shim's pid as its ready frame reported it: what a worker that stops answering is
    /// killed by (TID-93).
    peer_pid: Option<u32>,
}

/// The concrete transport over a child process's pipes.
pub type Live = PipeTransport<ChildStdin>;

pub(crate) use crate::exec::limits::LOST_WORKER_MARGIN_MS;

impl<W: WriteHalf> PipeTransport<W> {
    /// Wrap a write half and a read half. Does not perform the handshake; call
    /// [`ready`](ShimTransport::ready) for that.
    pub fn new<R: Read + Send + 'static>(stdin: W, stdout: R) -> Self {
        Self {
            stdin: Some(stdin),
            frames: BudgetedReader::spawn(stdout, "tiderace-reader"),
            budget: None,
            peer_pid: None,
        }
    }

    /// Wait at most `budget` for each reply; past it the reply is an error and the transport
    /// is [lost](Self::is_lost).
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = Some(budget);
        self
    }

    /// [`with_budget`](Self::with_budget), on a transport already in hand.
    pub fn set_budget(&mut self, budget: Option<Duration>) {
        self.budget = budget;
    }

    /// Whether a reply was overdue: the worker is to be killed, not waited for.
    pub fn is_lost(&self) -> bool {
        self.frames.is_lost()
    }

    /// The shim's pid from the ready frame, when it reported one.
    pub fn peer_pid(&self) -> Option<u32> {
        self.peer_pid
    }

    /// Close the write half (→ shim sees EOF and exits, running wider-scope finalizers once).
    /// Idempotent. Owners call this from `Drop` *before* reaping the child.
    pub fn close_input(&mut self) {
        if let Some(w) = self.stdin.take() {
            w.close();
        }
    }

    fn input(&mut self) -> Result<&mut W> {
        self.stdin
            .as_mut()
            .ok_or(EngineError::AlreadyShutDown { what: "shim" })
    }
}

impl<W: WriteHalf> Drop for PipeTransport<W> {
    fn drop(&mut self) {
        self.close_input();
    }
}

impl<W: WriteHalf> ShimTransport for PipeTransport<W> {
    fn ready(&mut self) -> Result<ReadyInfo> {
        // The import of the suite happens before the ready frame: no budget on this one.
        let frame: Value = self
            .frames
            .next(None)?
            .ok_or(EngineError::NoReadyFrame { what: "shim" })?;
        let info = ready_info(frame)?;
        self.peer_pid = info.pid;
        Ok(info)
    }

    fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse> {
        if self.is_lost() {
            return Err(EngineError::WorkerGone);
        }
        write_frame(self.input()?, req)?;
        self.frames
            .next(self.budget)?
            .ok_or(EngineError::PeerClosed { what: "shim" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NodeId, Outcome, ScopePath, TestStyle};
    use crate::exec::read_frame;
    use crate::testing::ScriptedShim;
    use std::io::BufReader;

    fn item(node_id: &str) -> TestItem {
        TestItem::new(
            NodeId::new(node_id),
            TestStyle::Function,
            ScopePath::module("m.py"),
        )
    }

    #[test]
    fn run_batch_maps_each_item_to_its_scripted_outcome_in_order() {
        let mut shim = ScriptedShim::new()
            .answer("m.py::test_ok", "passed", "")
            .answer("m.py::test_bad", "failed", "assert 1 == 2");
        let items = [item("m.py::test_ok"), item("m.py::test_bad")];

        let results =
            run_batch(&mut shim, &items, &RunKnobs::new(5_000)).expect("offline batch runs");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].node_id.as_str(), "m.py::test_ok");
        assert_eq!(results[0].outcome, Outcome::Passed);
        assert_eq!(results[1].outcome, Outcome::Failed);
        assert_eq!(results[1].detail, "assert 1 == 2");
        // Dispatch order is the item order.
        assert_eq!(shim.seen(), ["m.py::test_ok", "m.py::test_bad"]);
    }

    #[test]
    fn unknown_wire_token_becomes_error_outcome_through_the_loop() {
        let mut shim = ScriptedShim::new().answer("m.py::t", "kaboom", "weird");
        let results = run_batch(&mut shim, &[item("m.py::t")], &RunKnobs::new(5_000)).unwrap();
        assert_eq!(results[0].outcome, Outcome::Error);
    }

    #[test]
    fn mid_run_shim_close_surfaces_as_a_typed_error_not_a_panic() {
        let mut shim = ScriptedShim::new().closes_after(1);
        let err = run_batch(
            &mut shim,
            &[item("m.py::a"), item("m.py::b")],
            &RunKnobs::new(5_000),
        )
        .expect_err("a shim that closes mid-batch must error");
        assert!(matches!(err, EngineError::PeerClosed { .. }), "{err:?}");
    }

    /// The loopback tier: a real `std::io::pipe` + a Rust "fake shim" thread speaking the **actual**
    /// `write_frame`/`read_frame` protocol — so the wire codec, the ready handshake, and the
    /// close-input→EOF shutdown are all exercised **without** `fork`/`exec` or a Python venv.
    #[test]
    fn loopback_exercises_real_framing_without_a_process() {
        use std::io::pipe;

        let (req_r, req_w) = pipe().expect("req pipe");
        let (resp_r, resp_w) = pipe().expect("resp pipe");

        let shim = std::thread::spawn(move || {
            let mut from_engine = BufReader::new(req_r);
            let mut to_engine = resp_w;
            // Handshake first, exactly like the real shim.
            write_frame(
                &mut to_engine,
                &serde_json::json!({"ready": true, "pid": 4242}),
            )
            .unwrap();
            while let Some(req) = read_frame::<_, Value>(&mut from_engine).expect("read req frame")
            {
                let node = req
                    .get("node_id")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_string();
                let outcome = if node.contains("bad") {
                    "failed"
                } else {
                    "passed"
                };
                write_frame(
                    &mut to_engine,
                    &serde_json::json!({"node_id": node, "outcome": outcome, "detail": ""}),
                )
                .unwrap();
            }
        });

        let mut transport = PipeTransport::new(req_w, BufReader::new(resp_r));
        assert_eq!(transport.ready().unwrap().pid, Some(4242));

        let items = [item("m.py::test_ok"), item("m.py::test_bad")];
        let results =
            run_batch(&mut transport, &items, &RunKnobs::new(5_000)).expect("loopback batch");

        transport.close_input(); // EOF → the fake-shim thread's read loop ends
        shim.join().expect("fake shim thread");

        assert_eq!(results[0].outcome, Outcome::Passed);
        assert_eq!(results[1].outcome, Outcome::Failed);
    }
}
