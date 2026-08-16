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

/// Every shape that can hold a reference across a restore. Each `_b` test runs after its `_a`
/// sibling (alphabetical within the module) and fails if the restore rebound instead of restoring.
const CORPUS: &str = "\
CALLS = {}
ITEMS = []
SEEN = set()


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


REC = _Recorder(CALLS)
SLOTTED = _Slotted(ITEMS)
ID_AT_IMPORT = {\"calls\": id(CALLS), \"items\": id(ITEMS), \"seen\": id(SEEN)}


def test_dict_a_mutates():
    REC.record(\"a\")
    assert CALLS == {\"a\": 1}


def test_dict_b_sees_its_own_write():
    REC.record(\"b\")
    # Rebinding leaves REC.sink pointing at the old dict, so this write lands where CALLS cannot
    # see it — and CALLS is empty rather than {\"b\": 1}.
    assert CALLS == {\"b\": 1}, f\"CALLS={CALLS!r} REC.sink={REC.sink!r}\"


def test_list_a_mutates():
    SLOTTED.bucket.append(\"a\")
    assert ITEMS == [\"a\"]


def test_list_b_sees_its_own_write():
    SLOTTED.bucket.append(\"b\")
    assert ITEMS == [\"b\"], f\"ITEMS={ITEMS!r} SLOTTED.bucket={SLOTTED.bucket!r}\"


def test_set_a_mutates():
    SEEN.add(\"a\")
    assert SEEN == {\"a\"}


def test_set_b_is_reset_in_place():
    assert SEEN == set(), f\"SEEN={SEEN!r}\"
    SEEN.add(\"b\")
    assert SEEN == {\"b\"}


def test_identity_survived_every_restore():
    # The strongest statement: the objects the module started with are still the objects it has.
    assert id(CALLS) == ID_AT_IMPORT[\"calls\"], \"CALLS was rebound\"
    assert id(ITEMS) == ID_AT_IMPORT[\"items\"], \"ITEMS was rebound\"
    assert id(SEEN) == ID_AT_IMPORT[\"seen\"], \"SEEN was rebound\"
    assert REC.sink is CALLS, \"REC.sink no longer aliases CALLS\"
    assert SLOTTED.bucket is ITEMS, \"SLOTTED.bucket no longer aliases ITEMS\"
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
    dir
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
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("nofork");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 7, "7 tests in the corpus");

    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs against real Python");
    assert_eq!(results.len(), 7, "one result per test");
    assert_all_passed(&results, "subprocess");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same corpus must also pass under fork, where each child is a pristine COW copy. If it did
/// not, the corpus would be asserting something about restore that is not true of the engine.
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
    assert_all_passed(&results, "fork");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The optimistic in-process ladder restores rather than forking, so it is the Unix path into the
/// same hazard — and the reason the ladder was left opt-in until this landed.
#[cfg(unix)]
#[test]
fn the_optimistic_ladder_preserves_identity_too() {
    use engine_core::exec::ForkWorker;

    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("optimistic");
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");
    assert_all_passed(&results, "fork --optimistic");
    let _ = std::fs::remove_dir_all(&dir);
}
