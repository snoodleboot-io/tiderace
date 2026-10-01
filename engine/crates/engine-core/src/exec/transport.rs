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

use std::io::{BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::domain::{NodeId, Outcome, TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::shim_protocol::{read_frame, ready_info, write_frame, ExecRequest, ExecResponse};

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
    deadline_ms: u64,
    force_no_fork: bool,
    trusted: &std::collections::HashSet<NodeId>,
    must_fork: &std::collections::HashSet<NodeId>,
) -> Result<Vec<TestResult>> {
    run_batch_lost(
        transport,
        items,
        deadline_ms,
        force_no_fork,
        trusted,
        must_fork,
        LostWorker::Fail,
    )
    .map(|(results, _)| results)
}

/// [`run_batch`] with a [`LostWorker`] policy; the second value names the fault that took the
/// worker down, when one did.
pub(crate) fn run_batch_lost<T: ShimTransport + ?Sized>(
    transport: &mut T,
    items: &[TestItem],
    deadline_ms: u64,
    force_no_fork: bool,
    trusted: &std::collections::HashSet<NodeId>,
    must_fork: &std::collections::HashSet<NodeId>,
    on_lost: LostWorker,
) -> Result<(Vec<TestResult>, Option<String>)> {
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
                let duration_ms = start.elapsed().as_millis() as u64;
                results.push(TestResult::new(
                    item.node_id.clone(),
                    Outcome::Error,
                    duration_ms,
                    fault.clone(),
                ));
                for later in &items[index + 1..] {
                    results.push(TestResult::new(
                        later.node_id.clone(),
                        Outcome::Error,
                        0,
                        format!("not run: the worker was lost at {} ({fault})", item.node_id),
                    ));
                }
                return Ok((results, Some(fault)));
            }
            Err(e) => return Err(e),
        };
        let duration_ms = start.elapsed().as_millis() as u64;
        results.extend(results_for(item, resp, duration_ms));
    }
    Ok((results, None))
}

#[cfg(unix)]
impl PipeTransport<std::os::unix::net::UnixStream, BufReader<std::os::unix::net::UnixStream>> {
    /// How long a read on this socket waits before it fails (TID-93). The two halves are one
    /// socket, so setting it on the write half covers the reads.
    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> std::io::Result<()> {
        match self.stdin.as_ref() {
            Some(sock) => sock.set_read_timeout(timeout),
            None => Ok(()),
        }
    }
}

/// The production transport: length-prefixed JSON frames over a pair of byte streams — in practice a
/// child process's `stdin`/`stdout` ([`Live`]), but generic over any `Write`/`Read` so an in-memory
/// pipe can stand in for the process in a test (see this module's loopback test).
pub struct PipeTransport<W: Write, R: Read> {
    /// `Option` so [`close_input`](Self::close_input) can drop the write half (→ shim EOF/exit) *before*
    /// the owner reaps the child — the ordering that avoids a deadlock on shutdown.
    stdin: Option<W>,
    stdout: R,
    /// The shim's pid as its ready frame reported it (`-1` before the handshake or when unknown):
    /// what a worker that stops answering is killed by (TID-93).
    peer_pid: Option<u32>,
}

/// The concrete transport over a child process's pipes (what `Wellspring` holds).
pub type Live = PipeTransport<ChildStdin, BufReader<ChildStdout>>;

/// How long past the per-test deadline a worker may stay silent before it is given up on
/// (TID-93): the deadline itself is the shim's to enforce; this is the engine's margin over it.
pub(crate) const LOST_WORKER_MARGIN_MS: u64 = 10_000;

/// [`PipeTransport`] with a read budget (TID-98): a thread drains the read half into a channel,
/// so a reply that does not arrive within `budget` is an error — the one guarantee a worker
/// blocked inside a C call cannot defeat from the inside, and the only deadline Windows has.
/// A Unix socket takes a read timeout directly (the warm pool); a child's stdout pipe does not,
/// on any platform.
pub struct BudgetedTransport<W: Write> {
    stdin: Option<W>,
    frames: mpsc::Receiver<std::io::Result<Option<std::vec::Vec<u8>>>>,
    budget: Duration,
    peer_pid: Option<u32>,
    lost: bool,
}

impl<W: Write> BudgetedTransport<W> {
    pub fn new<R: Read + Send + 'static>(stdin: W, mut stdout: R, budget: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        // Blocks in `read` until the child writes or exits; a hung child holds it until the
        // child is killed, which the owner does once a reply is overdue.
        std::thread::Builder::new()
            .name("tiderace-reader".into())
            .spawn(move || loop {
                let frame = read_raw_frame(&mut stdout);
                let last = !matches!(frame, Ok(Some(_)));
                if tx.send(frame).is_err() || last {
                    break;
                }
            })
            .expect("spawn the transport reader");
        Self {
            stdin: Some(stdin),
            frames: rx,
            budget,
            peer_pid: None,
            lost: false,
        }
    }

    /// Whether a reply was overdue: the worker is to be killed, not waited for.
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Close the write half (→ shim sees EOF and exits). Idempotent.
    pub fn close_input(&mut self) {
        self.stdin.take();
    }

    fn next_frame<T: serde::de::DeserializeOwned>(
        &mut self,
        wait: Option<Duration>,
    ) -> Result<Option<T>> {
        let received = match wait {
            Some(d) => self.frames.recv_timeout(d).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => Some(d),
                mpsc::RecvTimeoutError::Disconnected => None,
            }),
            None => self.frames.recv().map_err(|_| None),
        };
        match received {
            Ok(Ok(Some(bytes))) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| EngineError::Exec(e.to_string())),
            Ok(Ok(None)) | Err(None) => Ok(None),
            Ok(Err(e)) => Err(EngineError::Io(e)),
            Err(Some(budget)) => {
                self.lost = true;
                Err(EngineError::Exec(format!(
                    "no answer from the worker within {:.1}s — its test overran the deadline \
                     and the in-process timeout could not interrupt it; the worker is killed \
                     and reported lost (TID-98)",
                    budget.as_secs_f64()
                )))
            }
        }
    }
}

impl<W: Write> ShimTransport for BudgetedTransport<W> {
    fn ready(&mut self) -> Result<ReadyInfo> {
        // The import of the suite happens before the ready frame: no budget on this one.
        let frame: Value = self
            .next_frame(None)?
            .ok_or_else(|| EngineError::Exec("shim sent no ready frame".into()))?;
        let info = ready_info(frame)?;
        self.peer_pid = info.pid;
        Ok(info)
    }

    fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse> {
        if self.lost {
            return Err(EngineError::Exec("the worker was lost".into()));
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| EngineError::Exec("shim already shut down".into()))?;
        write_frame(stdin, req)?;
        self.next_frame(Some(self.budget))?
            .ok_or_else(|| EngineError::Exec("shim closed mid-run".into()))
    }
}

/// The results one shim response stands for: the node's own, or one per case when the node
/// expanded (TID-25) — and none at all for an empty expansion, which is how a deselected node and
/// a class that inherits nothing report themselves. Shared by every transport (TID-104): a tier
/// that read a response as one outcome counted a deselected node as a pass.
pub(crate) fn results_for(
    item: &TestItem,
    resp: ExecResponse,
    duration_ms: u64,
) -> Vec<TestResult> {
    // A parametrized node reports one result per case (TID-25). The cases already ran and forked
    // individually, so this reports what was executed rather than the worst of it.
    if resp.expanded || !resp.variants.is_empty() {
        return resp
            .variants
            .into_iter()
            .map(|v| {
                let touched = v.coverage.keys().cloned().collect();
                TestResult::new(v.node_id, v.outcome, v.duration_ms, v.detail)
                    .with_touched(touched)
                    .with_pure(v.pure)
                    .with_must_fork(v.must_fork)
                    .with_keywords(v.keywords)
                    // These ids did not come from the static collector — they were produced here, by
                    // expanding a parametrized node or an inherited class (TID-55).
                    .with_expanded(true)
            })
            .collect();
    }
    let touched = resp.coverage.keys().cloned().collect();
    vec![
        TestResult::new(item.node_id.clone(), resp.outcome, duration_ms, resp.detail)
            .with_touched(touched)
            .with_pure(resp.pure)
            .with_must_fork(resp.must_fork)
            .with_skip_origin(resp.skip_origin)
            .with_keywords(resp.keywords),
    ]
}

/// One frame's payload bytes, `None` at EOF — [`read_frame`] without the parse, for a reader
/// thread that cannot know the type its owner wants.
fn read_raw_frame<R: Read>(r: &mut R) -> std::io::Result<Option<std::vec::Vec<u8>>> {
    let mut header = [0u8; 4];
    if let Err(e) = r.read_exact(&mut header) {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e);
    }
    let len = u32::from_le_bytes(header) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

impl<W: Write, R: Read> PipeTransport<W, R> {
    /// Wrap a write half and an (already-buffered) read half. Does not perform the handshake; call
    /// [`ready`](ShimTransport::ready) for that.
    pub fn new(stdin: W, stdout: R) -> Self {
        Self {
            stdin: Some(stdin),
            stdout,
            peer_pid: None,
        }
    }

    /// The shim's pid from the ready frame, `-1` when unknown.
    pub fn peer_pid(&self) -> Option<u32> {
        self.peer_pid
    }

    /// Close the write half (→ shim sees EOF and exits, running wider-scope finalizers once). Idempotent.
    /// Owners call this from `Drop` *before* reaping the child.
    pub fn close_input(&mut self) {
        self.stdin.take();
    }
}

impl<W: Write, R: Read> ShimTransport for PipeTransport<W, R> {
    fn ready(&mut self) -> Result<ReadyInfo> {
        let frame: Value = read_frame(&mut self.stdout)?
            .ok_or_else(|| EngineError::Exec("shim sent no ready frame".into()))?;
        let info = ready_info(frame)?;
        self.peer_pid = info.pid;
        Ok(info)
    }

    fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| EngineError::Exec("shim already shut down".into()))?;
        write_frame(stdin, req)?;
        read_frame(&mut self.stdout)?.ok_or_else(|| EngineError::Exec("shim closed mid-run".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NodeId, ScopePath, TestStyle};
    use crate::testing::ScriptedShim;

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

        let results = run_batch(
            &mut shim,
            &items,
            5_000,
            false,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .expect("offline batch runs");

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
        let results = run_batch(
            &mut shim,
            &[item("m.py::t")],
            5_000,
            false,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert_eq!(results[0].outcome, Outcome::Error);
    }

    #[test]
    fn mid_run_shim_close_surfaces_as_a_typed_error_not_a_panic() {
        let mut shim = ScriptedShim::new().closes_after(1);
        let err = run_batch(
            &mut shim,
            &[item("m.py::a"), item("m.py::b")],
            5_000,
            false,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .expect_err("a shim that closes mid-batch must error");
        assert!(matches!(err, EngineError::Exec(_)));
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
        let results = run_batch(
            &mut transport,
            &items,
            5_000,
            false,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .expect("loopback batch");

        transport.close_input(); // EOF → the fake-shim thread's read loop ends
        shim.join().expect("fake shim thread");

        assert_eq!(results[0].outcome, Outcome::Passed);
        assert_eq!(results[1].outcome, Outcome::Failed);
    }
}
