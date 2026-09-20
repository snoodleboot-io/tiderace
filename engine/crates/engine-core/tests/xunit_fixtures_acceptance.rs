//! TID-60 — module- and function-level fixtures, in both dialects.
//!
//! A suite uses `setUpModule` to put something in place for a whole file — stubbing an optional SDK
//! in `sys.modules` is the classic case — and none of these hooks were run at all: not unittest's
//! `setUpModule`, not pytest's `setup_module`, and none of the function/method/class spellings. Every
//! test in such a file then failed on the very thing the hook existed to provide. On one real suite
//! that was the entire remaining divergence: nine tests, all in one file, all dying inside an
//! optional-dependency import.
//!
//! Both dialects are honoured at every level, because a suite mid-migration has files in each.
//!
//! **Scope is per process, not per test.** A forked run re-enters module setup in each child, which
//! is right — every child is its own interpreter — but running a non-idempotent hook once per
//! in-process test would be wrong. Teardown happens when the worker is finished with the module.

#![cfg(unix)]

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
        "tiderace_t60_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// unittest's spelling, including the real-world use: a module-scoped stub in `sys.modules`.
const UNITTEST_STYLE: &str = r#"
import sys

SEEN = []


def setUpModule():
    sys.modules["tiderace_fake_sdk"] = "installed by setUpModule"
    SEEN.append("module")


def tearDownModule():
    sys.modules.pop("tiderace_fake_sdk", None)


def setup_function(function):
    SEEN.append(f"function:{function.__name__}")


def test_the_module_stub_is_in_place():
    # Without setUpModule this import is the ImportError that took out a whole real file.
    assert sys.modules.get("tiderace_fake_sdk") == "installed by setUpModule"
    assert SEEN[0] == "module", "module setup runs before anything else"
    assert SEEN[-1] == "function:test_the_module_stub_is_in_place"


class TestMethodLevel:
    def setup_method(self, method):
        self.prepared = f"for {method.__name__}"

    def test_setup_method_ran(self):
        assert self.prepared == "for test_setup_method_ran"
"#;

/// pytest's xunit spelling of the same things.
const PYTEST_STYLE: &str = r#"
STATE = {}


def setup_module(module):
    STATE["module"] = module.__name__


class TestClassLevel:
    @classmethod
    def setup_class(cls):
        cls.from_class = "set"

    def test_module_and_class_hooks_both_ran(self):
        assert STATE.get("module", "").endswith("test_pytest_style")
        assert self.from_class == "set"
"#;

#[test]
fn xunit_hooks_run_in_both_dialects() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("xunit");
    std::fs::write(dir.join("test_unittest_style.py"), UNITTEST_STYLE).unwrap();
    std::fs::write(dir.join("test_pytest_style.py"), PYTEST_STYLE).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3);
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-60: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
