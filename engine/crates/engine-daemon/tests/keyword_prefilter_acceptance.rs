//! TID-102 — the daemon decides `-k` itself for nodes it can vouch for, and the answer is the
//! one the workers would have given.
//!
//! After a full run every record carries the keywords the shim matched against. A `-k` run
//! then sends the workers only the candidates whose recorded keywords match — or that the daemon
//! cannot vouch for — and the results must be exactly what a daemon with no records (every node
//! judged by the shim) reports for the same expression. Then an edit: a new test has no record
//! and a re-marked test's dependency changed, so both go to the workers and are found.

#![cfg(unix)]

use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use engine_daemon::{EngineHandler, RpcHandler, RpcRequest, RpcResponse};
use std::collections::BTreeSet;
use std::path::Path;

/// Plain, parametrized, marked, skip-marked, inherited, and a module that skips at import: every
/// kind of node the verdict has to get right, and one the shim never judges.
fn write_corpus(dir: &Path) {
    std::fs::create_dir_all(dir.join("unit")).unwrap();
    // The project's own `-m`: a `-k` run through the daemon must keep it (TID-102 found it dropped).
    std::fs::write(
        dir.join("pytest.ini"),
        "[pytest]\naddopts = -m \"not heavy\"\nmarkers =\n    slow: slow\n    heavy: heavy\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("test_alpha.py"),
        "import pytest\n\n\
         def test_plain():\n    assert True\n\n\
         @pytest.mark.parametrize(\"n,tag\", [(1, \"a\"), (2, \"b\")])\n\
         def test_cases(n, tag):\n    assert True\n\n\
         @pytest.mark.slow\n\
         def test_marked():\n    assert True\n\n\
         @pytest.mark.skip(reason=\"never\")\n\
         def test_skipme():\n    assert False\n\n\
         @pytest.mark.heavy\n\
         def test_heavy_deselected_by_addopts():\n    assert False\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("test_gamma.py"),
        "class Base:\n    def test_inherited(self):\n        assert True\n\n\
         class TestDerived(Base):\n    pass\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("unit").join("test_delta.py"),
        "def test_under_unit():\n    assert True\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("test_omega.py"),
        "import pytest\n\npytest.importorskip(\"no_such_module_tiderace_t102\")\n\n\
         def test_never():\n    assert False\n",
    )
    .unwrap();
}

fn keyword_run(handler: &mut EngineHandler, expr: &str) -> BTreeSet<(String, String)> {
    match handler.handle(RpcRequest::RunFull {
        keyword: Some(expr.into()),
        marker: None,
        strict_markers: false,
    }) {
        RpcResponse::RanFull { results } => results
            .into_iter()
            .map(|r| (r.node_id.to_string(), r.outcome.to_string()))
            .collect(),
        other => panic!("expected RanFull, got {other:?}"),
    }
}

/// The oracle's verdict for `expr`: a daemon that holds no records, so its workers judge every
/// node. Its own `-k` runs would leave records behind — and vouching would begin — so the state
/// file goes before each run.
fn oracle_run(handler: &mut EngineHandler, dir: &Path, expr: &str) -> BTreeSet<(String, String)> {
    let _ = std::fs::remove_file(dir.join(".tiderace-state.json"));
    keyword_run(handler, expr)
}

fn records_with_keywords(dir: &Path) -> (usize, usize) {
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".tiderace-state.json")).unwrap_or_default(),
    )
    .unwrap_or_default();
    let tests = state["tests"].as_object().cloned().unwrap_or_default();
    let with = tests
        .values()
        .filter(|r| r["keywords"].as_array().is_some_and(|k| !k.is_empty()))
        .count();
    (with, tests.len())
}

#[test]
fn the_daemons_keyword_verdict_is_the_workers_verdict() {
    let Some(python) = python(PythonNeeds::Pytest) else {
        skip_live("no Python with pytest available");
        return;
    };
    // Footprints are what vouching reads: a record with no recorded deps is never decided.
    // SAFETY: this binary holds one test; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("TIDERACE_COVERAGE", "1") };
    let base = std::env::temp_dir().join(format!("tiderace_t102_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let recorded = base.join("recorded");
    let fresh = base.join("fresh");
    write_corpus(&recorded);
    write_corpus(&fresh);

    // The daemon with records: one full run, after which every judged record carries keywords.
    let mut daemon = EngineHandler::new(python.clone(), shim(), recorded.clone());
    let full = daemon.run_full_parallel().expect("full run");
    assert!(full.len() >= 7, "{full:?}");
    let (with, total) = records_with_keywords(&recorded);
    // Two records are never judged: the import-skipped module's, and the test the project's own
    // `-m` deselects (a `deselected` record, which the shim answers before any `-k`).
    assert!(
        with >= 7 && total - with <= 2,
        "every judged record carries keywords: {with}/{total}"
    );

    // The oracle: a daemon with no records at all, so its workers judge every node.
    let mut oracle = EngineHandler::new(python.clone(), shim(), fresh.clone());
    for expr in [
        "plain",
        "cases and not 2-b",
        "test_cases[1-a]",
        "slow",           // a mark name
        "inherited",      // an inherited method, collected as its class
        "TestDerived",    // the class
        "skipme",         // a skip-marked test is reported skipped
        "unit",           // a directory below the rootdir (TID-100)
        "omega or never", // the import-skipped module is reported skipped, `-k` or not
        "heavy",          // what the project's `addopts -m` deselects stays deselected
        "not alpha",
        "nomatch_zzz",
    ] {
        let got = keyword_run(&mut daemon, expr);
        let want = oracle_run(&mut oracle, &fresh, expr);
        assert_eq!(
            got, want,
            "TID-102: -k {expr:?} decided by the daemon must equal the workers' verdict"
        );
        assert!(
            !got.iter().any(|(id, _)| id.contains("heavy_deselected")),
            "-k {expr:?}: the project's own `-m` was dropped on the way through the daemon: {got:?}"
        );
    }
    // `-k` that matches nothing: the import-skipped module is still reported, replayed from its
    // record — exactly what the workers report, which pytest does too.
    let nothing = keyword_run(&mut daemon, "nomatch_zzz");
    assert_eq!(nothing.len(), 1, "{nothing:?}");
    assert!(nothing
        .iter()
        .all(|(id, oc)| id.starts_with("test_omega.py") && oc == "skipped"));

    // An edit: a new test (no record) and a re-marked one (its own file changed). The next `-k`
    // for either name goes to the workers and finds them; the untouched file is still decided
    // from its records, and its answer is unchanged.
    std::fs::write(
        recorded.join("test_alpha.py"),
        "import pytest\n\n\
         def test_plain():\n    assert True\n\n\
         @pytest.mark.parametrize(\"n,tag\", [(1, \"a\"), (2, \"b\")])\n\
         def test_cases(n, tag):\n    assert True\n\n\
         @pytest.mark.slow\n\
         def test_marked():\n    assert True\n\n\
         @pytest.mark.slow\n\
         def test_newly_marked():\n    assert True\n\n\
         def test_brand_new():\n    assert True\n",
    )
    .unwrap();
    std::fs::write(
        fresh.join("test_alpha.py"),
        std::fs::read_to_string(recorded.join("test_alpha.py")).unwrap(),
    )
    .unwrap();
    for expr in ["brand_new", "slow", "skipme", "inherited or unit"] {
        let got = keyword_run(&mut daemon, expr);
        let want = oracle_run(&mut oracle, &fresh, expr);
        assert_eq!(got, want, "after the edit: -k {expr:?}");
    }
    let slow = keyword_run(&mut daemon, "slow");
    let marked: Vec<&str> = slow
        .iter()
        .filter(|(_, oc)| oc == "passed")
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(
        marked,
        [
            "test_alpha.py::test_marked",
            "test_alpha.py::test_newly_marked"
        ],
        "both marked tests, the new one included (the module skip replayed beside them): {slow:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}
