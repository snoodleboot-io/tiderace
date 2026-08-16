//! TID-15 — a fork child must never die without saying why.
//!
//! `_invoke` in the shim guards the test **body**, so a body that raises always comes back as a
//! formatted `Outcome::Error`. Everything *around* the body — fixture setup, fixture teardown, the
//! coverage probe, the purity snapshot — used to land in a bare `except BaseException: pass` that
//! exited 0 with an empty pipe. The parent had nothing to report but the string `no result from
//! child`, which on a real suite is indistinguishable from a test that genuinely failed: no
//! traceback, no stage, no exception type. That is the failure mode this file pins shut.
//!
//! Fork-only by construction (the swallow lived in the child branch of `_fork_run`), and stdlib-only
//! so it runs against any interpreter on `PATH`.

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

/// Prefer the rich fx venv (local dev), else a bare interpreter on `PATH` (CI). `None` ⇒ skip.
fn any_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
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

/// Two fixtures that raise, one sync and one async, plus a passing control.
///
/// The async case is the one observed in the wild: an async fixture failure travels out through
/// `asyncio.run`, so it bypasses every handler the sync path happens to have. Both must arrive as a
/// reported error carrying the original exception, and the control must still pass — a child that
/// reports its own fault must not disturb the tests around it.
const CORPUS: &str = "\
import pytest


@pytest.fixture
def exploding():
    raise RuntimeError(\"fixture setup blew up\")


@pytest.fixture
async def exploding_async():
    raise RuntimeError(\"async fixture setup blew up\")


def test_error_from_sync_fixture(exploding):
    assert exploding is not None


async def test_error_from_async_fixture(exploding_async):
    assert exploding_async is not None


def test_control_still_passes():
    assert 1 + 1 == 2
";

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_childfault_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_child_fault.py"), CORPUS).unwrap();
    dir
}

/// A fixture that raises is reported with its cause, not as a lost result.
#[test]
fn fixture_failure_is_reported_with_a_traceback_not_a_lost_result() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("fixture");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3, "2 failing-fixture tests + 1 control");

    let results = ForkWorker::launch(&python, &shim(), &dir)
        .expect("wellspring")
        .run(&items)
        .expect("fork batch runs");
    assert_eq!(results.len(), 3, "one result per test");

    for r in &results {
        let leaf = r.node_id.as_str().rsplit("::").next().unwrap_or_default();
        if leaf == "test_control_still_passes" {
            assert_eq!(
                r.outcome,
                Outcome::Passed,
                "the control must be unaffected by its neighbours' faults; detail: {}",
                r.detail
            );
            continue;
        }

        assert_eq!(
            r.outcome,
            Outcome::Error,
            "a raising fixture makes the test an error, not a pass ({})",
            r.node_id
        );

        // The regression itself: the old child exited 0 with an empty pipe and this was the only
        // thing the parent could say.
        assert!(
            !r.detail.contains("no result from child"),
            "TID-15: {} lost its result instead of reporting the fault; detail: {:?}",
            r.node_id,
            r.detail
        );

        // And the diagnostic has to be worth having — the exception type and message the fixture
        // actually raised, so the failure names its own cause without a re-run.
        assert!(
            r.detail.contains("RuntimeError"),
            "detail must carry the exception type for {}; got: {:?}",
            r.node_id,
            r.detail
        );
        assert!(
            r.detail.contains("fixture setup blew up"),
            "detail must carry the original message for {}; got: {:?}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A test that kills its own process still reports something a human can act on.
///
/// `os._exit` skips the child's handler entirely, so no frame is ever written — the one case that
/// legitimately reaches the parent's empty-pipe branch. It must still name what happened rather
/// than emit the old bare string, and, critically, the worker must survive to run the next test.
#[test]
fn a_child_that_exits_itself_reports_that_and_the_worker_survives() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("selfexit");
    std::fs::write(
        dir.join("test_self_exit.py"),
        "import os\n\n\ndef test_kills_itself():\n    os._exit(0)\n\n\ndef test_after_the_kill():\n    assert True\n",
    )
    .unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let results = ForkWorker::launch(&python, &shim(), &dir)
        .expect("wellspring")
        .run(&items)
        .expect("the batch survives a child that exits itself");

    let killed = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_kills_itself"))
        .expect("the self-exiting test is reported at all");
    assert_eq!(killed.outcome, Outcome::Error, "a lost result is an error");
    assert!(
        killed.detail.contains("terminated itself") || killed.detail.contains("exited 0"),
        "the parent must say the child ended itself; got: {:?}",
        killed.detail
    );

    let after = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_after_the_kill"))
        .expect("the worker survives to run the following test");
    assert_eq!(
        after.outcome,
        Outcome::Passed,
        "one child ending itself must not take the batch down; detail: {}",
        after.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}
