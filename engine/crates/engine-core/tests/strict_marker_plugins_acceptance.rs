//! TID-60 — `--strict-markers` must not reject markers a plugin registered.
//!
//! Plugins register their markers at runtime: pytest-timeout's `timeout`, pytest-benchmark's
//! `benchmark`, pytest-django's `django_db`. None of that appears in the project's own `markers`
//! list, and tiderace does not run plugins — so the strict check, shipped in TID-59 against a
//! hand-written allowlist, flagged every one of them as a typo. On pirn-core that was **60 tests
//! erroring on `@pytest.mark.timeout`**, which pytest itself accepts without comment.
//!
//! The fix asks pytest, which already knows: `--markers` prints the registered set, plugins included.
//! One subprocess, only when a project has turned strict checking on.
//!
//! The important half is the failure mode. When that answer cannot be obtained, **nothing is
//! enforced** — a false error on a valid mark fails a suite that is correct, which is strictly worse
//! than missing a typo.

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

/// Any interpreter with pytest. The corpus registers its own marker through a conftest rather than
/// depending on a particular plugin being installed — `pytest_configure` + `addinivalue_line` is the
/// same mechanism every plugin uses, so this reproduces the case without requiring one.
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

/// Registers a marker the way a plugin does: from `pytest_configure`, at runtime, nowhere in the
/// project's own `markers` list.
const CONFTEST: &str = r#"
def pytest_configure(config):
    config.addinivalue_line("markers", "registered_at_runtime: added by a hook, as plugins do")
"#;

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

const CORPUS: &str = r#"
import pytest


@pytest.mark.mine
def test_a_mark_the_project_declared():
    assert True


@pytest.mark.registered_at_runtime
def test_a_mark_registered_by_a_hook():
    # Nothing in this project's `markers` declares this one — the conftest registers it at runtime,
    # exactly as pytest-timeout and pytest-benchmark register theirs.
    assert True


@pytest.mark.nobody_declared_this_one
def test_a_mark_nobody_declared():
    assert True
"#;

#[test]
fn a_plugin_registered_marker_is_not_a_typo() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("plugin");
    std::fs::write(
        dir.join("pyproject.toml"),
        "[tool.pytest.ini_options]\nmarkers = [\"mine: declared by the project\"]\n\
         addopts = \"--strict-markers\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("conftest.py"), CONFTEST).unwrap();
    std::fs::write(dir.join("test_marks.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(30_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    let outcome = |needle: &str| {
        results
            .iter()
            .find(|r| r.node_id.as_str().contains(needle))
            .unwrap_or_else(|| panic!("no result for {needle}"))
    };

    let declared = outcome("test_a_mark_the_project_declared");
    assert_eq!(declared.outcome, Outcome::Passed, "{}", declared.detail);

    let plugin = outcome("test_a_mark_registered_by_a_hook");
    assert_eq!(
        plugin.outcome,
        Outcome::Passed,
        "TID-60: this marker is registered at runtime by a hook, not by the project's `markers` \
         list — strict checking must accept it, as pytest does. Got: {}",
        plugin.detail
    );

    // The point of strict checking still holds for a name nobody registered anywhere.
    let typo = outcome("test_a_mark_nobody_declared");
    assert_eq!(
        typo.outcome,
        Outcome::Error,
        "a mark neither the project nor any plugin declares is still an error"
    );
    assert!(
        typo.detail.contains("nobody_declared_this_one"),
        "{}",
        typo.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}
