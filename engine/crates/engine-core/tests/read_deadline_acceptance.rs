//! TID-31 — the per-test deadline must cover the whole exchange, not just the first byte.
//!
//! `_fork_run` used `select` to bound the wait for the child's *first* byte and nothing after it.
//! A child that produced some output and then stopped satisfied that `select`, and the parent
//! blocked in `os.read` forever — taking the worker, and every remaining test in its batch, with
//! it. No message, no timeout: the run simply stopped producing output, which is the worst way for
//! a hang to present.
//!
//! Reproduced here the way it actually happens: the test forks a grandchild, which inherits the
//! result pipe's write end. The test child writes its frame and exits normally, but the pipe never
//! reaches EOF because the grandchild is still holding it open. The parent has data and no
//! terminator — precisely the state the old loop could not escape.
//!
//! Fork-only by construction: the bug lives in the fork path's pipe handling. `SubprocessWorker`
//! speaks the framed stdin/stdout protocol and never reaches this code.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ForkWorker, Worker};
use engine_core::testing::skip_live;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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
    for cand in ["python3", "python"] {
        let ok = std::process::Command::new(cand)
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

/// `test_a_…` sorts first so the hang happens before the test that proves the worker survived it.
const CORPUS: &str = "\
import os
import time


def test_a_leaves_a_grandchild_holding_the_pipe():
    # The grandchild inherits every open fd, including the result pipe's write end. This child
    # then exits normally, so the parent receives a complete frame but never sees EOF.
    if os.fork() == 0:
        time.sleep(60)
        os._exit(0)
    assert True


def test_b_the_worker_survived():
    assert True
";

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_readdl_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_hang.py"), CORPUS).unwrap();
    dir
}

#[test]
fn a_child_that_stops_mid_exchange_times_out_instead_of_hanging() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("run");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2, "the hanging test and its successor");

    // A short deadline so the test costs ~2s rather than the 60s default. Before the fix this call
    // never returned at all, so the harness itself is the assertion: reaching the next line means
    // the read loop is bounded.
    let results = ForkWorker::launch(&python, &shim(), &dir)
        .expect("wellspring")
        .with_deadline_ms(2_000)
        .run(&items)
        .expect("the batch completes rather than blocking forever");

    let hung = results
        .iter()
        .find(|r| {
            r.node_id
                .as_str()
                .ends_with("test_a_leaves_a_grandchild_holding_the_pipe")
        })
        .expect("the hanging test is reported at all");
    assert_eq!(
        hung.outcome,
        Outcome::Error,
        "a child that stopped mid-exchange must be reported, not waited on forever"
    );
    assert!(
        hung.detail.contains("timeout"),
        "the report must say it timed out; got {:?}",
        hung.detail
    );
    // TID-15's principle: a partial frame is a different fault from silence, and the parent says so.
    assert!(
        hung.detail.contains("partial result frame"),
        "a partial frame must be distinguished from a silent timeout; got {:?}",
        hung.detail
    );

    // The point of bounding the read: one stuck child must not cost the rest of the batch.
    let after = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_b_the_worker_survived"))
        .expect("the following test still runs");
    assert_eq!(
        after.outcome,
        Outcome::Passed,
        "the worker must survive a stuck child; detail: {}",
        after.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}
