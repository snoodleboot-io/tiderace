//! TID-77 — a module global that compares by identity does not make every test in the module impure.
//!
//! The purity snapshot deep-copies a module's mutable globals and compares them after the test. A
//! value whose type has no `__eq__` compares by identity, so its deep copy is never equal to the
//! original and every test in the module was judged "mutated module global `<name>`". `from
//! __future__ import annotations` binds exactly such a value (`annotations`, a `__future__._Feature`)
//! in almost every module of a modern codebase: on pirn-core 4,491 of 4,499 impure verdicts were that
//! one name, 16 of 5,657 tests were recorded pure, and the trusted-pure tier never engaged.
//!
//! Such a value is now snapshotted as itself: the name is unchanged while it still refers to the
//! same object, and a rebinding to a different object is still seen.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::skip_live;
use std::path::PathBuf;

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

const CORPUS: &str = r#"from __future__ import annotations

import sys

SENTINEL = object()  # identity-compared, like `annotations` above
TALLY = {"n": 0}


def test_touches_nothing():
    assert SENTINEL is not None


def test_reads_the_globals():
    assert TALLY["n"] == 0
    assert isinstance(annotations, type(annotations))


def test_rebinds_the_sentinel():
    sys.modules[__name__].SENTINEL = object()


def test_mutates_a_container():
    TALLY["n"] += 1
"#;

fn get<'a>(results: &'a [TestResult], leaf: &str) -> &'a TestResult {
    results
        .iter()
        .find(|r| r.node_id.as_str().ends_with(leaf))
        .unwrap_or_else(|| panic!("{leaf} was reported"))
}

#[test]
fn an_identity_compared_global_does_not_make_the_module_impure() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t77_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_future.py"), CORPUS).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 4);

    std::env::set_var("TIDERACE_PURITY", "1");
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    std::env::remove_var("TIDERACE_PURITY");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(results.len(), 4);
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
    }
    for leaf in ["test_touches_nothing", "test_reads_the_globals"] {
        assert_eq!(
            get(&results, leaf).pure,
            Some(true),
            "TID-77: {leaf} mutates nothing; `from __future__ import annotations` and an `object()` \
             sentinel are identity-compared globals whose deep copies never equal the original"
        );
    }
    // The other direction still holds: a rebinding is a mutation, and so is a container edit.
    for leaf in ["test_rebinds_the_sentinel", "test_mutates_a_container"] {
        assert_eq!(
            get(&results, leaf).pure,
            Some(false),
            "{leaf} changed a module global and must stay impure"
        );
    }
}
