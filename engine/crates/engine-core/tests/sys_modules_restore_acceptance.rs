//! TID-27 — a test that swaps a module in `sys.modules` must not corrupt the next one.
//!
//! `_snapshot_shared` covers the running test's own module globals, so it cannot see a test that
//! evicts a *library* module and re-imports it. That leaves two copies of every class the module
//! defines: a test holding the original sets state the library — now bound to the replacement —
//! cannot see. The failure surfaces in an unrelated test with nothing pointing at the cause.
//!
//! Found on a real corpus, where one test's "restore the canonical modules" loop was a no-op
//! (`import_module` returns the *cached* module, so it cannot undo an eviction) and seven tests in
//! two other files failed. It only reproduced under the in-process ladder, and only in an order
//! pytest happens not to produce, which is what made it expensive to find.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
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

/// The library: class-level state, and a reader bound to it at *its* import time.
const LIB: &str = "\
class Registry:
    _shared = None

    @classmethod
    def put(cls, value):
        cls._shared = value

    @classmethod
    def get(cls):
        return cls._shared
";

const READER: &str = "\
from shared_lib import Registry


def read():
    \"\"\"Reads through the binding this module captured when IT was imported.\"\"\"
    return Registry.get()
";

/// Sorts first, so it runs before the victim — the order that exposes the bug.
const EVICTOR: &str = "\
import importlib
import sys


def test_a_evicts_and_reimports_the_library():
    for name in (\"reader\", \"shared_lib\"):
        sys.modules.pop(name, None)
    # Rebuilds both, so `reader` now binds a DIFFERENT Registry class.
    importlib.import_module(\"reader\")
    # The same no-op \"restore\" the real corpus used: the modules are already cached, so this
    # returns the replacements rather than putting the originals back.
    for name in (\"shared_lib\", \"reader\"):
        importlib.import_module(name)
    assert True
";

const VICTIM: &str = "\
import sys

import reader  # noqa: F401 — imported so the modules exist to capture
import shared_lib  # noqa: F401

# Captured when THIS module is imported, before any test runs.
ORIGINAL_LIB = sys.modules[\"shared_lib\"]
ORIGINAL_READER = sys.modules[\"reader\"]


def test_b_the_swapped_modules_were_put_back():
    # Asserted on identity rather than on a downstream symptom. A symptom test would depend on
    # which of the two class copies each caller happens to hold, which varies with import order and
    # made an earlier attempt at this test pass without the fix in place.
    assert sys.modules[\"shared_lib\"] is ORIGINAL_LIB, (
        \"shared_lib was replaced in sys.modules and never restored\"
    )
    assert sys.modules[\"reader\"] is ORIGINAL_READER, (
        \"reader was replaced in sys.modules and never restored\"
    )


def test_c_the_library_still_binds_the_original_class():
    from shared_lib import Registry

    assert sys.modules[\"reader\"].Registry is Registry, (
        \"the library and the test hold different Registry classes\"
    )
";

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_sysmod_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("shared_lib.py"), LIB).unwrap();
    std::fs::write(dir.join("reader.py"), READER).unwrap();
    std::fs::write(dir.join("test_a_evict.py"), EVICTOR).unwrap();
    std::fs::write(dir.join("test_b_victim.py"), VICTIM).unwrap();
    dir
}

/// The no-fork tier with one worker: every test shares a process, so a module swap persists unless
/// something puts it back. This is the configuration Windows always runs, and the one `--optimistic`
/// selects on Unix.
#[test]
fn a_module_swap_does_not_leak_into_the_next_test() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("nofork");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3, "the evictor and its two victims");

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs against real Python");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-27: {} did not pass; detail: {}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same corpus under fork, where each child is pristine. If this ever failed, the corpus would
/// be asserting something untrue of the engine rather than exercising the restore.
#[cfg(unix)]
#[test]
fn the_same_corpus_passes_under_fork() {
    use engine_core::exec::ForkWorker;

    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("fork");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = ForkWorker::launch(&python, &shim(), &dir)
        .expect("wellspring")
        .run(&items)
        .expect("fork batch runs");
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
