//! TID-5 — live acceptance for the no-COW `SubprocessWorker` (`--no-fork`, ADR-E008 / design 05 §7).
//!
//! Drives the real shim in its `--no-fork` mode against a **real Python** and asserts correct outcomes
//! across test styles. Unlike the fork-based acceptance suites (which need the rich `.tiderace-fx-venv`),
//! this uses a **stdlib-only** corpus, so it runs wherever *any* interpreter is on `PATH` — including
//! **Windows CI**, which has no `fork()`. That closes the ROADMAP Phase-7 gap: the no-fork fallback is
//! now exercised end-to-end on the platform it exists for.
//!
//! On fork-capable platforms it *additionally* asserts the no-fork path is **result-identical** to
//! `ForkWorker` on the same corpus — the §8 boundary-3 invariant that makes the fallback safe.

use engine_core::testing::skip_live;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

/// A usable interpreter: prefer the rich fx venv (local dev), else a bare `python3`/`python` on `PATH`
/// (CI, incl. Windows via `actions/setup-python`). `None` ⇒ skip cleanly.
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

/// A stdlib-only corpus spanning outcome types (pass / assert-fail / raise-error) and both styles
/// (module function + class method). No third-party imports, so a bare CI interpreter runs it.
const CORPUS: &str = "\
def test_pass():
    assert 1 + 1 == 2

def test_fail():
    assert 1 == 2

def test_error():
    raise RuntimeError(\"boom\")

class TestGroup:
    def test_method_pass(self):
        assert \"x\".upper() == \"X\"

    def test_method_fail(self):
        assert []
";

/// The expected outcome for a node, from its final name segment.
fn expected(node_id: &str) -> Outcome {
    let leaf = node_id.rsplit("::").next().unwrap_or(node_id);
    if leaf.contains("error") {
        Outcome::Error
    } else if leaf.contains("fail") {
        Outcome::Failed
    } else {
        Outcome::Passed
    }
}

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_nofork_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_nofork.py"), CORPUS).unwrap();
    dir
}

#[test]
fn subprocess_worker_no_fork_runs_a_real_python_correctly() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("run");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(
        items.len(),
        5,
        "5 tests collected (3 functions + 2 methods)"
    );

    let mut worker = SubprocessWorker::new(5_000, 1).with_target(python, &shim(), &dir);
    let results = worker
        .run(&items)
        .expect("no-fork batch runs against real Python");

    assert_eq!(results.len(), 5, "one result per test");
    for r in &results {
        assert_eq!(
            r.outcome,
            expected(r.node_id.as_str()),
            "no-fork outcome for {}",
            r.node_id
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// On a fork-capable platform, the no-fork fallback must be **result-identical** to the fork path over
/// the same corpus (the safety invariant of the no-COW fallback). Windows has no fork, so it's skipped
/// there — the correctness test above is what runs on Windows.
#[cfg(unix)]
#[test]
fn no_fork_is_result_identical_to_fork() {
    use engine_core::exec::ForkWorker;

    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("diff");
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let no_fork = SubprocessWorker::new(5_000, 1)
        .with_target(python.clone(), &shim(), &dir)
        .run(&items)
        .expect("no-fork run");
    let forked = ForkWorker::launch(&python, &shim(), &dir)
        .expect("wellspring")
        .run(&items)
        .expect("fork run");

    let map = |rs: &[engine_core::domain::TestResult]| {
        let mut v: Vec<(String, Outcome)> = rs
            .iter()
            .map(|r| (r.node_id.to_string(), r.outcome))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0)); // Outcome isn't Ord; node id is a total key
        v
    };
    assert_eq!(
        map(&no_fork),
        map(&forked),
        "the --no-fork fallback must match ForkWorker outcome-for-outcome"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An **opaque** module — one whose globals can't be snapshot-restored — under `--no-fork`.
///
/// The ladder's soundness argument is "an opaque module still forks." That rung doesn't exist on
/// Windows, and the shim used to handle it two wrong ways: the optimistic path called `os.fork()` and
/// raised an uncaught `AttributeError` (killing the worker), while `--no-fork` mode skipped the
/// restorability check entirely and ran the module in-process, leaking un-restorable state into the
/// next test. See `py-tiderace/proof_windows_opaque_fork.py`.
///
/// So the expectation is genuinely platform-dependent, and this asserts both halves:
/// * **fork-capable** (Unix) — the module forks and the tests pass, no leak;
/// * **fork-less** (Windows) — the tests are reported as `Error` rather than run without isolation.
///
/// Either way the worker survives, which is the part that used to fail.
const OPAQUE_CORPUS: &str = "\
_GEN = (i for i in range(100))

def test_a():
    assert next(_GEN) == 0

def test_b():
    v = next(_GEN)
    assert v == 0, f\"LEAK: generator advanced across tests, got {v}\"
";

#[test]
fn opaque_module_is_isolated_by_fork_or_refused_never_leaked() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("opaque");
    std::fs::write(dir.join("test_opaque.py"), OPAQUE_CORPUS).unwrap();
    // Drop the default corpus so only the opaque module is collected.
    let _ = std::fs::remove_file(dir.join("test_nofork.py"));

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2, "test_a + test_b");

    let results = SubprocessWorker::new(5_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("the worker must survive an opaque module, whatever the platform");
    assert_eq!(results.len(), 2);

    for r in &results {
        if cfg!(unix) {
            assert_eq!(
                r.outcome,
                Outcome::Passed,
                "fork-capable: {} should fork and pass (a leak shows up as Failed)",
                r.node_id
            );
        } else {
            assert_eq!(
                r.outcome,
                Outcome::Error,
                "fork-less: {} must be refused, not run without isolation",
                r.node_id
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A **restorable** module that mutates a module-level global — the ordinary case, and the one that
/// silently broke.
///
/// Without `fork()` there is no COW copy, so snapshot/restore is this worker's only isolation. The
/// worker didn't request it (no `--restore`, no `TIDERACE_RESTORE`); it inherited whatever the caller
/// exported. Under the daemon that happened to be `TIDERACE_RESTORE=1`, so this looked fine — but
/// standalone the flag was unset and every module-level mutation persisted into the next test on that
/// module. The existing acceptance corpus never mutated globals, so nothing caught it.
///
/// `test_b` fails iff `test_a`'s append survived.
const MUTATING_CORPUS: &str = "\
_SEEN = []

def test_a():
    _SEEN.append(1)
    assert _SEEN == [1]

def test_b():
    _SEEN.append(2)
    assert _SEEN == [2], f\"LEAK: state from a previous test survived: {_SEEN}\"
";

#[test]
fn no_fork_restores_module_state_between_tests() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("mutating");
    std::fs::write(dir.join("test_mutating.py"), MUTATING_CORPUS).unwrap();
    let _ = std::fs::remove_file(dir.join("test_nofork.py"));

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);

    let results = SubprocessWorker::new(5_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("no-fork run");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "{} — no-fork must restore module state between tests, not leak it",
            r.node_id
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
