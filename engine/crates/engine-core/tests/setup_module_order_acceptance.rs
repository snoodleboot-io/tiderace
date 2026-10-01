//! TID-79 — xunit `setup_module` runs before the module's wider-scoped fixtures, as pytest orders it.
//!
//! pytest injects `setup_module` / `setUpModule` as the first module-scoped autouse fixture, so a
//! module-scoped fixture sees what the hook put in place. The engine ran the hook on the test's own
//! path, after the wider fixtures were already live: a moto mock started in `setup_module` never
//! reached the `boto3` client a module fixture had already built — `NoCredentialsError` on every
//! test in the file, under a mock that was running.
//!
//! The corpus declares its fixture with `@pytest.fixture`, which needs pytest on the interpreter; the
//! Windows CI job runs a bare one, so this gates on unix like its siblings.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};

/// The hook puts a credential in the environment; the module fixture is a "client" that captures
/// the environment at its creation, as a real SDK client does.
const CORPUS: &str = r#"import os
import pytest

ORDER = []


def setup_module(module):
    ORDER.append("setup_module")
    os.environ["T79_CREDENTIAL"] = "from-setup_module"


def teardown_module(module):
    ORDER.append("teardown_module")
    os.environ.pop("T79_CREDENTIAL", None)


@pytest.fixture(scope="module")
def client():
    ORDER.append("module fixture")
    return {"credential": os.environ.get("T79_CREDENTIAL")}


def test_client_was_built_after_the_hook(client):
    assert ORDER[:2] == ["setup_module", "module fixture"], ORDER
    assert client["credential"] == "from-setup_module", client


def test_second_test_sees_the_same_client(client):
    assert client["credential"] == "from-setup_module", client
"#;

#[test]
fn setup_module_runs_before_the_modules_wider_fixtures() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t79_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_hook_order.py"), CORPUS).unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(results.len(), 2);
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-79: {} — the module fixture must be built after setup_module ran: {}",
            r.node_id,
            r.detail
        );
    }
}
