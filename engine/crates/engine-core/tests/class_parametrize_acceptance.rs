//! TID-53 — `@pytest.mark.parametrize` on a class (or a module's `pytestmark`) reaches its tests.
//!
//! Parametrize marks were read from the test function alone. pytest applies a mark on a class to
//! every method the class collects, and a module-level `pytestmark` to every test in the file — so a
//! class parametrized with five values and holding four methods is twenty tests in pytest, and was
//! four here, each failing on the argument nobody supplied.
//!
//! The shape hides in a tally, which is how it survived: the missing expansions and the failures they
//! cause partly cancel. One real suite showed a 62-test gap where only 36 tests had changed outcome —
//! the rest were nodes that never existed on one side.
//!
//! Ids are pytest's, and their order is load-bearing: axes run narrowest first, so a function
//! parameter precedes its class's in `test[1-A]`, and the class value varies fastest across cases.
//! An id is a selector — one that cannot be pasted from one runner into the other is a bug of its own.

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

fn python_with_pytest() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut candidates: Vec<String> = Vec::new();
    if venv.exists() {
        candidates.push(venv.to_string_lossy().into_owned());
    }
    candidates.extend(["python3".to_string(), "python".to_string()]);
    candidates.into_iter().find(|p| {
        std::process::Command::new(p)
            .args(["-c", "import pytest"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t53_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CORPUS: &str = r#"
import pytest


@pytest.mark.parametrize("outer", ["A", "B"])
class TestBoth:
    @pytest.mark.parametrize("inner", [1, 2])
    def test_stacked(self, outer, inner):
        assert outer in ("A", "B") and inner in (1, 2)

    def test_class_only(self, outer):
        assert outer in ("A", "B")
"#;

const MODULE_LEVEL: &str = r#"
import pytest

pytestmark = pytest.mark.parametrize("flavour", ["salt", "pepper"])


def test_module_mark_reaches_me(flavour):
    assert flavour in ("salt", "pepper")
"#;

fn run(dir: &std::path::Path, python: String) -> Vec<engine_core::domain::TestResult> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), dir)
        .run(&items)
        .expect("batch runs")
}

fn ids(results: &[engine_core::domain::TestResult]) -> Vec<String> {
    let mut v: Vec<String> = results
        .iter()
        .map(|r| r.node_id.as_str().to_string())
        .collect();
    v.sort();
    v
}

/// A class-level mark expands every method it holds, with pytest's ids.
#[test]
fn a_parametrize_on_the_class_reaches_every_method() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("class");
    std::fs::write(dir.join("test_class_param.py"), CORPUS).unwrap();
    let results = run(&dir, python);

    // Exactly pytest's ten node ids, in pytest's spelling.
    let expected = vec![
        "test_class_param.py::TestBoth::test_class_only[A]",
        "test_class_param.py::TestBoth::test_class_only[B]",
        "test_class_param.py::TestBoth::test_stacked[1-A]",
        "test_class_param.py::TestBoth::test_stacked[1-B]",
        "test_class_param.py::TestBoth::test_stacked[2-A]",
        "test_class_param.py::TestBoth::test_stacked[2-B]",
    ];
    assert_eq!(
        ids(&results),
        expected,
        "TID-53: a class-level parametrize must expand its methods with pytest's ids — note \
         `[1-A]`, the function's own parameter first"
    );
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "{} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same for a module-level `pytestmark`, which pytest applies to every test in the file.
#[test]
fn a_module_level_pytestmark_parametrizes_the_whole_file() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("module");
    std::fs::write(dir.join("test_module_param.py"), MODULE_LEVEL).unwrap();
    let results = run(&dir, python);

    assert_eq!(
        ids(&results),
        vec![
            "test_module_param.py::test_module_mark_reaches_me[pepper]",
            "test_module_param.py::test_module_mark_reaches_me[salt]",
        ],
        "TID-53: a module-level `pytestmark` parametrize applies to every test in the file"
    );
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}", r.detail);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
