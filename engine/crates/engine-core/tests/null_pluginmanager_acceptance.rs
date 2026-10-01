//! TID-62 — `pytestconfig.pluginmanager` exists and reports an empty runner.
//!
//! flask's logging tests switch pytest's own log capture off before asserting on a stream:
//!
//! ```python
//! logging_plugin = pytestconfig.pluginmanager.unregister(name="logging-plugin")
//! ```
//!
//! `RunConfig` had no `pluginmanager`, so fixture setup raised `AttributeError` and six tests errored
//! for a reason that had nothing to do with what they test.
//!
//! The honest answer is not to fake a registry: tiderace runs no pytest plugins, so the plugin the
//! fixture wants gone was never there. Unregistering returns `None`, nothing is ever found
//! registered, and the fixture proceeds to the assertions it came for.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};

/// flask's shape: a fixture that turns pytest's logging plugin off and back on around the test.
const CORPUS: &str = r#"
import pytest


@pytest.fixture(autouse=True)
def reset_logging(pytestconfig):
    logging_plugin = pytestconfig.pluginmanager.unregister(name="logging-plugin")
    yield
    pytestconfig.pluginmanager.register(logging_plugin, "logging-plugin")


def test_the_fixture_did_not_blow_up():
    assert True


def test_the_manager_reports_an_empty_runner(pytestconfig):
    pm = pytestconfig.pluginmanager
    assert pm.get_plugin("logging-plugin") is None
    assert pm.has_plugin("logging-plugin") is False
    assert pm.list_name_plugin() == []
"#;

#[test]
fn a_fixture_may_unregister_a_plugin_that_was_never_there() {
    let Some(python) = python(PythonNeeds::PytestAndTiderace) else {
        skip_live("no interpreter with both pytest and tiderace.builtins");
        return;
    };
    let dir = scratch("pluginmanager");
    std::fs::write(dir.join("test_pluginmanager.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    assert_eq!(results.len(), 2);
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-62: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
