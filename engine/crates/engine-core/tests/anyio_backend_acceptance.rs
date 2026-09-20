//! TID-54 — `@pytest.mark.anyio` expands over the backends, as the plugin would.
//!
//! The fixture a marked test runs against is not declared by the suite. anyio's *plugin* declares it,
//! parametrised over the backends that happen to be installed:
//!
//! ```python
//! @pytest.fixture(scope="module", params=get_available_backends())
//! def anyio_backend(request): return request.param
//! ```
//!
//! Tiderace hosts no plugins, so nothing supplied it and every marked test ran **once**, where its
//! author asked for one run per backend. The tests passed, which is what made it dangerous: half the
//! intended coverage was missing and nothing said so.
//!
//! Two properties this pins down, both learned the hard way on a real suite:
//!
//! * **Async tests only.** The marker parametrises how a coroutine is *run*, so a synchronous test in
//!   a marked module is one test, not one per backend — which is how pytest collects it too.
//! * **A suite's own `anyio_backend` wins.** Registered at the root location, so a conftest that
//!   declares its own (anyio's test suite does, to add uvloop) overrides it by ordinary nearest-wins
//!   resolution.

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

/// Only pytest is needed. The corpus declares its own `anyio_backend`, so the expansion under test
/// does not depend on anyio being installed — and the run does not either: with a backend named but
/// anyio absent, the driver falls back to plain asyncio. Requiring anyio here would have made this
/// skip on any machine without it, and a skipped test proves nothing.
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
        "tiderace_t54_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The suite declares its own single-backend fixture, so the expansion is deterministic here rather
/// than depending on which backends the machine has installed.
const CONFTEST: &str = r#"
import pytest


@pytest.fixture(params=["asyncio"])
def anyio_backend(request):
    return request.param
"#;

const CORPUS: &str = r#"
import pytest

pytestmark = pytest.mark.anyio


async def test_async_one_runs_per_backend():
    assert True


def test_sync_is_not_expanded():
    # Synchronous: the marker is about how a coroutine is driven, so this stays a single test.
    assert True
"#;

#[test]
fn an_anyio_marked_async_test_expands_over_the_backends() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("anyio");
    std::fs::write(dir.join("conftest.py"), CONFTEST).unwrap();
    std::fs::write(dir.join("test_anyio.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    let mut ids: Vec<&str> = results.iter().map(|r| r.node_id.as_str()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "test_anyio.py::test_async_one_runs_per_backend[asyncio]",
            "test_anyio.py::test_sync_is_not_expanded",
        ],
        "TID-54: the async test carries the backend id the suite's own fixture gives it; the sync \
         one is untouched"
    );
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}", r.detail);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
