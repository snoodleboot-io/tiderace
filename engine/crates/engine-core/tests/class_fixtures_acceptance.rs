//! TID-47 — fixtures defined inside a test class.
//!
//! A `@pytest.fixture` written as a method of a test class was never registered: discovery only read
//! the module's own namespace. Every test in such a class then failed on the argument nobody supplied.
//! It is a common shape — flask's `TestRoutes` and click's runner classes both use it — and it cost
//! anyio 56 tests on its own.
//!
//! Two properties make this more than "register them too":
//!
//! **Scope.** pytest scopes a class's fixtures to that class, where they routinely *override* a
//! conftest fixture of the same name for that class alone. Registering them module-wide would let one
//! class's `app` leak into its neighbours.
//!
//! **Override-and-extend.** `def app(self, app)` inside a class is idiomatic pytest: the override
//! requests the very definition it shadows. That needs a resolver that can look *past* a definition,
//! and a closure keyed by definition rather than by name — otherwise the name resolves to the
//! override itself and is never set up at all (`KeyError: 'app'`).

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
        "tiderace_t47_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CONFTEST: &str = r#"
import pytest


@pytest.fixture
def app():
    return ["conftest"]
"#;

const CORPUS: &str = r#"
import pytest


class TestOverrides:
    """flask's `TestRoutes` shape: override a conftest fixture, and extend what it returns."""

    @pytest.fixture
    def app(self, app):
        return app + [self.tag()]

    @pytest.fixture
    def helper(self, app):
        return "-".join(app)

    def tag(self):
        return "class"

    def test_the_class_fixture_wraps_the_conftest_one(self, app):
        assert app == ["conftest", "class"]

    def test_a_class_fixture_may_depend_on_another(self, helper):
        assert helper == "conftest-class"


class TestNeighbour:
    """The neighbouring class must not see the override above."""

    def test_sees_the_conftest_fixture(self, app):
        assert app == ["conftest"]


class Base:
    @pytest.fixture
    def inherited(self):
        return "from the base"


class TestInherits(Base):
    def test_a_fixture_from_a_base_class_is_visible(self, inherited):
        assert inherited == "from the base"


def test_a_plain_function_sees_the_conftest_fixture(app):
    assert app == ["conftest"]
"#;

#[test]
fn fixtures_defined_in_a_test_class_resolve_and_stay_scoped_to_it() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("classfix");
    std::fs::write(dir.join("conftest.py"), CONFTEST).unwrap();
    std::fs::write(dir.join("test_class_fixtures.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    // Five tests, plus the `InheritedMethods` node the collector emits for `TestInherits` because it
    // has a base class (TID-26) — that node expands to nothing here, since `Base` holds no tests.
    assert_eq!(items.len(), 6);
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
