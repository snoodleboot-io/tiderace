//! The shim **transport** seam — the one thing a [`Worker`](crate::exec::Worker) needs from the
//! world below it: a synchronous request→response exchange with a shim.
//!
//! Until now "talk to the shim" was hand-inlined twice (once in [`Wellspring`](crate::exec::Wellspring),
//! once in `SubprocessWorker`'s `NoForkProc`) and the per-item run loop a third time (in both workers).
//! Both were welded to `ChildStdin`/`ChildStdout` — i.e. to a real OS process reached over pipes, which
//! means **no execution-path logic could be tested without `fork`/`exec`/a live venv**. The live
//! acceptance scenarios early-return `SKIP` when `.riptide-fx-venv` is absent, so in CI-without-Python
//! the entire `Worker → frames → TestResult` path was simply *unverified*.
//!
//! [`ShimTransport`] names that boundary. Production wires it to a process over pipes
//! ([`PipeTransport`]); tests wire it to a pure-Rust object **in the same thread** (the `tests` module's
//! `ScriptedShim`) — same [`run_batch`] loop, zero syscalls, fully deterministic. This is also the seam
//! a future in-process / FFI backend (Rust-as-Python-extension, ADR ②) slots behind without touching
//! any `Worker`.

use std::io::{BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout};
use std::time::Instant;

use serde_json::Value;

use crate::domain::{Outcome, TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::shim_protocol::{read_frame, write_frame, ExecRequest, ExecResponse};

/// What a shim reports in its readiness handshake (the first frame it sends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyInfo {
    /// The shim/wellspring process id (`-1` when a transport has no underlying process, e.g. tests).
    pub pid: i64,
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
) -> Result<Vec<TestResult>> {
    let mut results = Vec::with_capacity(items.len());
    for item in items {
        let mut req = ExecRequest::bare(item.node_id.as_str(), item.style.wire(), deadline_ms);
        req.force_no_fork = force_no_fork; // optimistic no-fork; the shim forks non-restorable modules
        let start = Instant::now();
        let resp = transport.exchange(&req)?;
        let duration_ms = start.elapsed().as_millis() as u64;
        let touched = resp.coverage.keys().cloned().collect();
        results.push(
            TestResult::new(
                item.node_id.clone(),
                Outcome::from_wire(&resp.outcome),
                duration_ms,
                resp.detail,
            )
            .with_touched(touched),
        );
    }
    Ok(results)
}

/// The production transport: length-prefixed JSON frames over a pair of byte streams — in practice a
/// child process's `stdin`/`stdout` ([`Live`]), but generic over any `Write`/`Read` so an in-memory
/// pipe can stand in for the process in a test (see this module's loopback test).
pub struct PipeTransport<W: Write, R: Read> {
    /// `Option` so [`close_input`](Self::close_input) can drop the write half (→ shim EOF/exit) *before*
    /// the owner reaps the child — the ordering that avoids a deadlock on shutdown.
    stdin: Option<W>,
    stdout: R,
}

/// The concrete transport over a child process's pipes (what `Wellspring`/`NoForkProc` hold).
pub type Live = PipeTransport<ChildStdin, BufReader<ChildStdout>>;

impl<W: Write, R: Read> PipeTransport<W, R> {
    /// Wrap a write half and an (already-buffered) read half. Does not perform the handshake; call
    /// [`ready`](ShimTransport::ready) for that.
    pub fn new(stdin: W, stdout: R) -> Self {
        Self {
            stdin: Some(stdin),
            stdout,
        }
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
        if frame.get("ready").and_then(Value::as_bool) != Some(true) {
            return Err(EngineError::Exec(format!("shim failed to warm: {frame}")));
        }
        Ok(ReadyInfo {
            pid: frame.get("pid").and_then(Value::as_i64).unwrap_or(-1),
        })
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

    /// A pure-Rust shim that answers from a script — **no process, no pipe, no syscall**. Proves the
    /// `Worker` run loop (request build → exchange → `TestResult` assembly) end to end, offline.
    struct ScriptedShim {
        pid: i64,
        /// node_id → (outcome wire token, detail).
        script: std::collections::HashMap<String, (String, String)>,
        /// Outcome for any node_id not in `script`.
        default_outcome: String,
        /// node_ids in the order they were asked — lets a test assert on dispatch order.
        seen: std::vec::Vec<String>,
        /// If set, the Nth (0-based) exchange and every one after fails as if the shim closed mid-run.
        close_after: Option<usize>,
        calls: usize,
    }

    impl ScriptedShim {
        fn new() -> Self {
            Self {
                pid: 0,
                script: std::collections::HashMap::new(),
                default_outcome: "passed".into(),
                seen: Vec::new(),
                close_after: None,
                calls: 0,
            }
        }

        fn answer(mut self, node_id: &str, outcome: &str, detail: &str) -> Self {
            self.script
                .insert(node_id.into(), (outcome.into(), detail.into()));
            self
        }

        fn closes_after(mut self, n: usize) -> Self {
            self.close_after = Some(n);
            self
        }
    }

    impl ShimTransport for ScriptedShim {
        fn ready(&mut self) -> Result<ReadyInfo> {
            Ok(ReadyInfo { pid: self.pid })
        }

        fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse> {
            if matches!(self.close_after, Some(n) if self.calls >= n) {
                return Err(EngineError::Exec("shim closed mid-run".into()));
            }
            self.calls += 1;
            self.seen.push(req.node_id.to_string());
            let (outcome, detail) = self
                .script
                .get(req.node_id)
                .cloned()
                .unwrap_or((self.default_outcome.clone(), String::new()));
            Ok(ExecResponse {
                node_id: req.node_id.to_string(),
                outcome,
                detail,
                coverage: Default::default(),
            })
        }
    }

    fn item(node_id: &str) -> TestItem {
        TestItem::new(
            NodeId::new(node_id),
            TestStyle::PytestFunction,
            ScopePath::module("m.py"),
        )
    }

    #[test]
    fn run_batch_maps_each_item_to_its_scripted_outcome_in_order() {
        let mut shim = ScriptedShim::new()
            .answer("m.py::test_ok", "passed", "")
            .answer("m.py::test_bad", "failed", "assert 1 == 2");
        let items = [item("m.py::test_ok"), item("m.py::test_bad")];

        let results = run_batch(&mut shim, &items, 5_000, false).expect("offline batch runs");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].node_id.as_str(), "m.py::test_ok");
        assert_eq!(results[0].outcome, Outcome::Passed);
        assert_eq!(results[1].outcome, Outcome::Failed);
        assert_eq!(results[1].detail, "assert 1 == 2");
        // Dispatch order is the item order.
        assert_eq!(shim.seen, vec!["m.py::test_ok", "m.py::test_bad"]);
    }

    #[test]
    fn unknown_wire_token_becomes_error_outcome_through_the_loop() {
        let mut shim = ScriptedShim::new().answer("m.py::t", "kaboom", "weird");
        let results = run_batch(&mut shim, &[item("m.py::t")], 5_000, false).unwrap();
        assert_eq!(results[0].outcome, Outcome::Error);
    }

    #[test]
    fn mid_run_shim_close_surfaces_as_a_typed_error_not_a_panic() {
        let mut shim = ScriptedShim::new().closes_after(1);
        let err = run_batch(&mut shim, &[item("m.py::a"), item("m.py::b")], 5_000, false)
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
        assert_eq!(transport.ready().unwrap().pid, 4242);

        let items = [item("m.py::test_ok"), item("m.py::test_bad")];
        let results = run_batch(&mut transport, &items, 5_000, false).expect("loopback batch");

        transport.close_input(); // EOF → the fake-shim thread's read loop ends
        shim.join().expect("fake shim thread");

        assert_eq!(results[0].outcome, Outcome::Passed);
        assert_eq!(results[1].outcome, Outcome::Failed);
    }
}
