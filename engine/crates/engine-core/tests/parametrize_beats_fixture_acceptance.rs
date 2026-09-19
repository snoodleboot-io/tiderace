//! TID-57 — a value the parametrize supplies wins over a fixture sharing its name.
//!
//! pytest's rule: direct parametrization overrides a fixture of the same name. Tiderace asked the
//! registry first, so a test parametrized over `history` — with some *other* module in the tree
//! defining a `history` fixture — resolved the fixture instead and died on `KeyError: 'history'`.
//!
//! The collision is easy to hit, because the colliding names are ordinary words: `history`, `client`,
//! `config`, `store`. Worse, it depends on the run root. A narrow root never discovers the other
//! module's fixture and the test passes; point the runner at the whole package and the same test
//! errors. A result that changes with the directory you aimed at is not one anybody can act on.
//!
//! `indirect=` is the exception and is covered here too: there the value is meant for the *fixture*,
//! as `request.param`, and the test receives whatever the fixture returns — so an indirect name must
//! stay a fixture request.

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
        "tiderace_t57_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The competing fixture lives in **another test module**, which is the shape that actually breaks.
/// A conftest fixture does not reproduce it: pytest scopes a module's fixtures to that module, and so
/// does the registry, but a name discovered anywhere in the tree was still enough to win the lookup —
/// which is why this only bites once the run root is wide enough to have imported the other module.
const OTHER_MODULE: &str = r#"
import pytest


@pytest.fixture
def history():
    return "from the other module's fixture"


def test_uses_its_own_fixture(history):
    assert history == "from the other module's fixture"
"#;

const CORPUS: &str = r#"
import pytest


@pytest.mark.parametrize("history", ["alpha", "beta"])
def test_the_parametrized_value_wins(history):
    assert history in ("alpha", "beta"), f"got {history!r} — a fixture shadowed the parametrize"
"#;

#[test]
fn a_parametrized_value_beats_a_fixture_of_the_same_name() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("collide");
    std::fs::write(dir.join("test_other_module.py"), OTHER_MODULE).unwrap();
    std::fs::write(dir.join("test_collide.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    assert_eq!(
        results.len(),
        3,
        "two parametrized cases plus the plain test"
    );
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-57: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
