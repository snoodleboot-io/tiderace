//! TID-47 — the remaining pytest builtins, each as a native resource with pytest's name bound to it.
//!
//! `recwarn`, `tmpdir` and `pytestconfig` were the builtins a real suite still asked for and did not
//! get, along with two `monkeypatch` gaps: `setattr(..., raising=)` and `context()`.
//!
//! Each is one provider registered once and reachable two ways — by type for migrated code
//! (`w: Warnings`, `cfg: RunConfig`) and by pytest's name for unmodified suites (`recwarn`,
//! `pytestconfig`). That is the same arrangement the existing builtins use, and it is what keeps the
//! two surfaces from drifting: there is one implementation, not a native one and a facade that
//! reimplements it.
//!
//! `tmpdir` is the exception worth naming: it is pytest's *legacy* `py.path.local`, and the modern
//! spelling `tmp_path` already exists here. It is supported so an old suite runs unmodified, and the
//! migration tool rewrites toward `TmpPath` rather than blessing it.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};

const CORPUS: &str = r#"
import os
import warnings

import pytest

from tiderace.builtins import RunConfig, Warnings


def test_recwarn_records_and_pops(recwarn):
    warnings.warn("careful", UserWarning)
    assert len(recwarn) == 1
    assert recwarn.pop(UserWarning).category is UserWarning


def test_popping_a_warning_nobody_raised_fails(recwarn):
    with pytest.raises(AssertionError):
        recwarn.pop(UserWarning)


def test_tmpdir_is_a_usable_directory(tmpdir):
    assert os.path.isdir(str(tmpdir))


def test_pytestconfig_answers_rootpath_and_unknown_options(pytestconfig):
    assert pytestconfig.rootpath is not None
    assert pytestconfig.getoption("--not-declared", "fallback") == "fallback"


def test_monkeypatch_context_undoes_at_the_end_of_the_block(monkeypatch):
    with monkeypatch.context() as m:
        m.setenv("TIDERACE_CTX", "yes")
        assert os.environ["TIDERACE_CTX"] == "yes"
    assert "TIDERACE_CTX" not in os.environ


def test_monkeypatch_refuses_to_invent_an_attribute(monkeypatch):
    # pytest's guard: patching a name that does not exist is usually a typo, not an intention.
    with pytest.raises(AttributeError):
        monkeypatch.setattr(os, "definitely_not_there", 1)
    monkeypatch.setattr(os, "definitely_not_there", 1, raising=False)
    assert os.definitely_not_there == 1


def test_the_same_resources_resolve_by_type(w: Warnings, cfg: RunConfig):
    # The migrated form. Same providers, reached by type rather than by pytest's name.
    warnings.warn("native", DeprecationWarning)
    assert len(w) == 1
    assert cfg.rootpath is not None
"#;

#[test]
fn the_remaining_pytest_builtins_resolve_by_name_and_by_type() {
    let Some(python) = python(PythonNeeds::PytestAndTiderace) else {
        skip_live("no interpreter with both pytest and tiderace.builtins");
        return;
    };
    let dir = scratch("builtins");
    std::fs::write(dir.join("test_builtins.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 7);
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-47: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
