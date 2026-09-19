//! TID-50 — a test that leaks a thread is re-run from a pristine image, not from the process it dirtied.
//!
//! Tiderace runs restorable tests in-process and forks only what it must. When a test turns out to
//! have disturbed interpreter state, it is re-run in a fork so the current run still reports the right
//! answer (TID-33). That fork used to be taken from the worker that had just run the test — and when
//! what the test leaked was a **thread**, that is the classic POSIX hazard: `fork()` gives the child
//! the calling thread only, while every object the other threads owned comes across intact. A re-run
//! that waits on a background worker then waits for a thread that does not exist.
//!
//! It is not hypothetical. Three dask tests in one real suite hung there, each burning the full
//! 60-second deadline: 54 of that run's 73 seconds were the engine waiting on tests whose work takes
//! milliseconds, and the suite reported three errors that pytest passes.
//!
//! The corpus below is that shape in miniature: a library that hands work to a thread it starts once,
//! and a test that uses it. The test leaves the thread running, so it is demoted; the re-run needs the
//! thread to answer. Forked from the dirty worker it deadlocks, and the only thing that ends it is the
//! deadline. Forked from a clean image it passes in milliseconds.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::Worker;
use engine_core::testing::skip_live;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
    ["python3", "python"]
        .into_iter()
        .find(|cand| {
            std::process::Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t50_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A library whose work is done by a thread it starts exactly once — dask's shape, minus dask.
const LEAKY_LIB: &str = r#"
import threading

_REQUESTS = []
_READY = threading.Event()
_WANTED = threading.Event()
_WORKER = None
_STARTED = False   # tracked with a flag, as libraries do — so nothing restarts it after a fork


def _run():
    while True:
        _WANTED.wait()
        _WANTED.clear()
        _REQUESTS.append("done")
        _READY.set()


def ensure_started():
    global _WORKER, _STARTED
    if not _STARTED:
        _WORKER = threading.Thread(target=_run, daemon=True)
        _WORKER.start()
        _STARTED = True
    return _WORKER


def do_work(timeout=30):
    ensure_started()
    _READY.clear()
    _WANTED.set()
    if not _READY.wait(timeout):
        raise RuntimeError("the worker thread never answered")
    return _REQUESTS.pop()
"#;

/// The leak has to be reached through a helper: a test that starts a thread in its own body is
/// impure on sight and forks from the start, which is not the path this is about.
const LEAKY_TEST: &str = r#"
from . import leaky_lib


def test_uses_the_library():
    assert leaky_lib.do_work() == "done"


def test_ordinary():
    assert True
"#;

#[cfg(unix)]
#[test]
fn a_thread_leaking_test_is_rerun_from_a_clean_image_not_the_process_it_dirtied() {
    use engine_core::exec::ForkWorker;

    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("leak");
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("__init__.py"), "").unwrap();
    std::fs::write(tests.join("leaky_lib.py"), LEAKY_LIB).unwrap();
    std::fs::write(tests.join("test_leaky.py"), LEAKY_TEST).unwrap();

    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), 2);

    // The optimistic ladder is the configuration this is about: tests run in-process, and one that
    // disturbs state is demoted and re-run. A deadline long enough that a deadlocked re-run is
    // unmistakable, short enough to keep this test quick when it regresses.
    let deadline_ms = 8_000;
    let started = Instant::now();
    let results = ForkWorker::launch_optimistic(&python, &shim(), &tests)
        .expect("wellspring with restore")
        .with_deadline_ms(deadline_ms)
        .run(&items)
        .expect("optimistic batch runs");
    let elapsed = started.elapsed();

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-50: {} must pass — a demoted test has to be re-run from a pristine image, not from \
             the worker holding the thread it leaked — got {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    // The deadlock is visible in the clock as well as the outcome: the old path could only end by
    // timing out, so it could never finish in under the deadline.
    assert!(
        elapsed.as_millis() < u128::from(deadline_ms),
        "TID-50: the run took {elapsed:?}, at or past the {deadline_ms}ms deadline — that is a \
         deadlocked re-run waiting to be killed, not work"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
