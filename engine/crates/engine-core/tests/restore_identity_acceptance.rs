//! TID-22 — the no-fork restore preserves object identity.
//!
//! `_restore_shared` undid a test's mutations by **rebinding** the module attribute
//! (`d[k] = deepcopy(old)`). That restores the *name*, not the *object*: anything holding a direct
//! reference to the original — a registered stub, a callback, a fixture that captured the sink, a
//! class attribute — kept writing into the old object while the module attribute pointed at a fresh
//! copy, and the two silently diverged.
//!
//! It matters far beyond a curiosity: `run_batch` selects `SubprocessWorker` on every non-Unix
//! platform, so this was the isolation **every Windows run** used, and the optimistic in-process
//! ladder reaches it on Unix too. The failure surfaced arbitrarily far from the cause — on the real
//! corpus it read as `KeyError` on a module-level dict, in a test that passed under fork.
//!
//! A plain module-level function is *not* affected (it resolves globals by name at call time), so
//! the corpus here deliberately holds references the way test doubles do.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Every shape that can hold a reference across a restore. Each `_b` test runs after its `_a`
/// sibling (alphabetical within the module) and fails if the restore rebound instead of restoring.
const CORPUS: &str = "\
import array
from collections import deque

CALLS = {}
ITEMS = []
SEEN = set()
EVENTS = deque()
NUMS = array.array(\"i\", [])


class _Recorder:
    \"\"\"Holds the sink by reference, the way a registered stub or a fixture-built double would.\"\"\"

    def __init__(self, sink):
        self.sink = sink

    def record(self, key):
        self.sink[key] = 1


class _Slotted:
    __slots__ = (\"bucket\",)

    def __init__(self, bucket):
        self.bucket = bucket


class _Queue:
    \"\"\"Holds containers that are neither a builtin collection nor an object (TID-23).\"\"\"

    def __init__(self, events, nums):
        self.events = events
        self.nums = nums


REC = _Recorder(CALLS)
SLOTTED = _Slotted(ITEMS)
QUEUE = _Queue(EVENTS, NUMS)
ID_AT_IMPORT = {
    \"calls\": id(CALLS),
    \"items\": id(ITEMS),
    \"seen\": id(SEEN),
    \"events\": id(EVENTS),
    \"nums\": id(NUMS),
}


def test_dict_a_mutates():
    REC.record(\"a\")
    assert CALLS == {\"a\": 1}


def test_dict_b_sees_its_own_write():
    REC.record(\"b\")
    # Rebinding would leave REC.sink pointing at the old dict, so this write would land where CALLS
    # cannot see it. Inside the file the earlier write is still here too, as under pytest.
    assert CALLS == {\"a\": 1, \"b\": 1}, f\"CALLS={CALLS!r} REC.sink={REC.sink!r}\"


def test_list_a_mutates():
    SLOTTED.bucket.append(\"a\")
    assert ITEMS == [\"a\"]


def test_list_b_sees_its_own_write():
    SLOTTED.bucket.append(\"b\")
    assert ITEMS == [\"a\", \"b\"], f\"ITEMS={ITEMS!r} SLOTTED.bucket={SLOTTED.bucket!r}\"


def test_set_a_mutates():
    SEEN.add(\"a\")
    assert SEEN == {\"a\"}


def test_set_b_accumulates():
    SEEN.add(\"b\")
    assert SEEN == {\"a\", \"b\"}, f\"SEEN={SEEN!r}\"


def test_deque_a_appends():
    QUEUE.events.append(\"a\")
    assert list(EVENTS) == [\"a\"]


def test_deque_b_sees_its_own_write():
    QUEUE.events.append(\"b\")
    assert list(EVENTS) == [\"a\", \"b\"], f\"EVENTS={list(EVENTS)!r} QUEUE.events={list(QUEUE.events)!r}\"


def test_array_a_appends():
    QUEUE.nums.append(1)
    assert list(NUMS) == [1]


def test_array_b_sees_its_own_write():
    QUEUE.nums.append(2)
    assert list(NUMS) == [1, 2], f\"NUMS={list(NUMS)!r} QUEUE.nums={list(QUEUE.nums)!r}\"


def test_identity_survived_every_restore():
    # The strongest statement: the objects the module started with are still the objects it has.
    assert id(CALLS) == ID_AT_IMPORT[\"calls\"], \"CALLS was rebound\"
    assert id(ITEMS) == ID_AT_IMPORT[\"items\"], \"ITEMS was rebound\"
    assert id(SEEN) == ID_AT_IMPORT[\"seen\"], \"SEEN was rebound\"
    assert id(EVENTS) == ID_AT_IMPORT[\"events\"], \"EVENTS was rebound\"
    assert id(NUMS) == ID_AT_IMPORT[\"nums\"], \"NUMS was rebound\"
    assert REC.sink is CALLS, \"REC.sink no longer aliases CALLS\"
    assert SLOTTED.bucket is ITEMS, \"SLOTTED.bucket no longer aliases ITEMS\"
    assert QUEUE.events is EVENTS, \"QUEUE.events no longer aliases EVENTS\"
    assert QUEUE.nums is NUMS, \"QUEUE.nums no longer aliases NUMS\"
";

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_restoreid_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_identity.py"), CORPUS).unwrap();
    // A second module, so the worker leaves `test_identity` — which is when its state is put back.
    std::fs::write(
        dir.join("test_zz_other.py"),
        "def test_elsewhere():\n    assert True\n",
    )
    .unwrap();
    dir
}

/// Run `test_identity` twice with the other module in between, on one worker. The `_a` tests pass
/// the second time only if the boundary restore emptied the containers **in place**; the identity
/// test passes only if nothing was rebound. The `_b` tests see the file's accumulated state each
/// time, as under pytest (TID-81).
fn run_leave_and_return(worker: &mut dyn Worker, dir: &std::path::Path, label: &str) {
    let all = RegexCollector::new().collect(dir).expect("collection");
    let identity: Vec<_> = all
        .iter()
        .filter(|i| i.node_id.as_str().starts_with("test_identity.py"))
        .cloned()
        .collect();
    let other: Vec<_> = all
        .iter()
        .filter(|i| i.node_id.as_str().starts_with("test_zz_other.py"))
        .cloned()
        .collect();
    assert_eq!(identity.len(), 11, "11 tests in the corpus");
    assert_eq!(other.len(), 1);
    let first = worker.run(&identity).expect("first pass runs");
    assert_eq!(first.len(), 11, "one result per test");
    assert_all_passed(&first, &format!("{label}, first pass"));
    let elsewhere = worker.run(&other).expect("the other module runs");
    assert_all_passed(&elsewhere, &format!("{label}, other module"));
    let again = worker.run(&identity).expect("second pass runs");
    assert_all_passed(
        &again,
        &format!("{label}, back in the module after the boundary restore"),
    );
}

fn assert_all_passed(results: &[TestResult], label: &str) {
    for r in results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-22 ({label}): {} did not pass; detail: {}",
            r.node_id,
            r.detail
        );
    }
}

/// One worker, no fork: every test shares a process, so restore is the only isolation and its
/// identity behaviour is observable. This is the configuration Windows always runs.
#[test]
fn restore_preserves_identity_on_the_no_fork_tier() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("nofork");
    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &dir);
    run_leave_and_return(&mut worker, &dir, "subprocess");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The optimistic in-process ladder restores rather than forking, so it is the Unix path into the
/// same hazard — and the reason the ladder was left opt-in until this landed.
#[cfg(unix)]
#[test]
fn the_optimistic_ladder_preserves_identity_too() {
    use engine_core::exec::ForkWorker;

    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("optimistic");
    let mut worker =
        ForkWorker::launch_optimistic(&python, &shim(), &dir).expect("wellspring with restore");
    run_leave_and_return(&mut worker, &dir, "fork --optimistic");
    let _ = std::fs::remove_dir_all(&dir);
}
