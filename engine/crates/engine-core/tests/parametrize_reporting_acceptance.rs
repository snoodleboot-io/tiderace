//! TID-25 — a parametrized test reports one result per case, with pytest's ids.
//!
//! `Engine.run` already forks and runs each case separately, then `_aggregate` kept only the worst
//! outcome and threw the rest away. So four cases reported as `1 total`: the passes were invisible,
//! a node with several failures kept one detail, and the totals could not be reconciled against
//! pytest (3,617 nodes against 4,519 items on the real corpus).
//!
//! The ids are the other half, and they are not cosmetic — they are **selectors**. Someone who
//! copies `test_rejected[(SELECT 1)]` out of a pytest run has to be able to paste it into tiderace,
//! so this asserts pytest's own spelling rather than a scheme of our own: printable ASCII verbatim,
//! non-ASCII escaped, classes and functions by `__name__`, and author-supplied `ids=` winning.

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
    candidates.into_iter().find(|cand| {
        std::process::Command::new(cand)
            .args(["-c", "import pytest"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

const CORPUS: &str = "\
import pytest


@pytest.mark.parametrize(\"n\", [1, 2, 3, 4])
def test_scalar_ids(n):
    assert n != 3


@pytest.mark.parametrize(\"s\", [\"plain\", \"(SELECT 1)\", \"a b\", \"\"])
def test_string_ids_keep_printable_ascii(s):
    assert isinstance(s, str)


class _Liar:
    pass


def _helper():
    pass


@pytest.mark.parametrize(\"obj\", [_Liar, _helper])
def test_named_objects_id_by_name(obj):
    assert obj is not None


@pytest.mark.parametrize(\"size\", [1, 2], ids=[\"small\", \"large\"])
def test_explicit_ids_win(size):
    assert size > 0


@pytest.mark.parametrize(\"v\", [pytest.param(9, id=\"nine\")])
def test_param_id_wins(v):
    assert v == 9


def test_unparametrized_keeps_its_bare_id():
    assert True
";

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_param_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_param.py"), CORPUS).unwrap();
    dir
}

#[test]
fn each_parametrized_case_reports_its_own_result_with_pytest_ids() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = write_corpus("run");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 6, "6 test functions before expansion");

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs against real Python");

    let ids: Vec<String> = results.iter().map(|r| r.node_id.to_string()).collect();
    let has = |suffix: &str| ids.iter().any(|i| i.ends_with(suffix));

    // 4 + 4 + 2 + 2 + 1 cases, plus the unparametrized test.
    assert_eq!(
        results.len(),
        14,
        "one result per case, not per function; got {ids:#?}"
    );

    // Scalars print as themselves.
    for n in ["[1]", "[2]", "[3]", "[4]"] {
        assert!(has(n), "missing scalar id {n} in {ids:#?}");
    }
    // Strings keep printable ASCII verbatim — spaces, parens and all — and an empty string is `[]`.
    for s in ["[plain]", "[(SELECT 1)]", "[a b]", "[]"] {
        assert!(has(s), "missing string id {s} in {ids:#?}");
    }
    // Classes and functions id by __name__, as pytest does.
    assert!(has("[_Liar]"), "class should id by name in {ids:#?}");
    assert!(has("[_helper]"), "function should id by name in {ids:#?}");
    // Author-supplied ids win over anything generated.
    for s in ["[small]", "[large]", "[nine]"] {
        assert!(has(s), "explicit id {s} must win in {ids:#?}");
    }
    // An unparametrized test keeps its bare id — no empty bracket pair.
    assert!(
        ids.iter()
            .any(|i| i.ends_with("test_unparametrized_keeps_its_bare_id")),
        "unparametrized id must be unchanged in {ids:#?}"
    );

    // Exactly one case fails, and it is the one that should.
    let failing: Vec<String> = results
        .iter()
        .filter(|r| r.outcome != Outcome::Passed)
        .map(|r| r.node_id.to_string())
        .collect();
    assert_eq!(failing.len(), 1, "only n=3 should fail; got {failing:#?}");
    assert!(
        failing[0].ends_with("test_scalar_ids[3]"),
        "the failing case must be identified by its own id; got {failing:#?}"
    );
}
