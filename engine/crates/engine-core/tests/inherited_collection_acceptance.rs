//! TID-26 — tests the source scan cannot see must still be collected.
//!
//! `RegexCollector` reads source text, which made three different things invisible. On a real corpus
//! they cost **129 tests that never ran**, in a green run, including every backend conformance suite
//! in the project. That is worse than any bug that reports itself.
//!
//!   1. **Inherited** methods — `class TestKuzuConformance(GraphStoreConformance)` has no `test_*`
//!      in its body; they live in another module entirely.
//!   2. A class **nested inside a method** — one `class ReadTimeout(Exception):` in a test body
//!      closed the enclosing class, dropping every test after it.
//!   3. A class **inside a triple-quoted string** — suites embed Python source to feed `ast.parse`,
//!      and a `class P:` at column 0 in a literal read as real code.
//!
//! Plus the class pytest collects by inheritance rather than by name: `PackOverridesBuiltinTests`
//! is a `unittest.TestCase` subclass *transitively*, so the text says neither "Test…" nor "TestCase".

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

fn any_python() -> Option<String> {
    for cand in ["python3", "python"] {
        let ok = std::process::Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

/// The shared contract suite, in its own module — nothing here appears in the test file.
const BASE: &str = "\
import unittest


class StoreConformance:
    def make_store(self):
        raise NotImplementedError

    def test_round_trips(self):
        assert self.make_store() == self.expected

    def test_is_not_empty(self):
        assert self.make_store()


class _SharedCase(unittest.TestCase):
    \"\"\"An intermediate base: subclasses are TestCase subclasses transitively.\"\"\"

    def helper(self):
        return \"shared\"
";

const CORPUS: &str = "\
import unittest

from conformance import StoreConformance, _SharedCase


class TestMemoryBackend(StoreConformance):
    expected = \"mem\"

    def make_store(self):
        return \"mem\"


class TestDiskBackend(StoreConformance):
    expected = \"disk\"

    def make_store(self):
        return \"disk\"

    def test_own_method_too(self):
        assert True


class PackOverridesBuiltinTests(_SharedCase):
    # Collected by pytest because it is a TestCase subclass, though the name says otherwise.
    def test_transitive_unittest_subclass(self):
        assert self.helper() == \"shared\"


class TestNestedClassInABody(unittest.TestCase):
    def test_defines_a_local_class(self):
        class ReadTimeout(Exception):
            pass

        assert issubclass(ReadTimeout, Exception)

    def test_after_the_nested_class(self):
        # Used to be dropped: the nested class closed the enclosing one.
        assert True


class TestSourceInAString(unittest.TestCase):
    SNIPPET = '''
class P:
    async def process(self, x):
        return x
'''

    def test_after_the_string_literal(self):
        # Used to be dropped: `class P:` at column 0 inside the literal read as real code.
        assert \"class P\" in self.SNIPPET


class NotATestClass:
    # No `Test` prefix, not a TestCase — pytest ignores it, and so must we.
    def test_never_collected(self):
        raise AssertionError(\"this class must not be collected\")
";

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_inherit_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("conformance.py"), BASE).unwrap();
    std::fs::write(dir.join("test_backends.py"), CORPUS).unwrap();
    dir
}

#[test]
fn tests_the_source_scan_cannot_see_are_still_collected_and_run() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("run");
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs against real Python");
    let ids: Vec<String> = results.iter().map(|r| r.node_id.to_string()).collect();
    let has = |suffix: &str| ids.iter().any(|i| i.ends_with(suffix));

    // Inherited from a base in another module — the headline case.
    for id in [
        "TestMemoryBackend::test_round_trips",
        "TestMemoryBackend::test_is_not_empty",
        "TestDiskBackend::test_round_trips",
        "TestDiskBackend::test_is_not_empty",
    ] {
        assert!(
            has(id),
            "inherited test {id} was not collected; got {ids:#?}"
        );
    }
    // A class with both kinds reports each exactly once.
    assert!(has("TestDiskBackend::test_own_method_too"));
    assert_eq!(
        ids.iter()
            .filter(|i| i.ends_with("TestDiskBackend::test_round_trips"))
            .count(),
        1,
        "an inherited test must not be double-counted; got {ids:#?}"
    );
    // Collected by pytest through inheritance, not by name.
    assert!(
        has("PackOverridesBuiltinTests::test_transitive_unittest_subclass"),
        "a transitive TestCase subclass must be collected; got {ids:#?}"
    );
    // A nested class in a body no longer truncates its class.
    assert!(has("TestNestedClassInABody::test_after_the_nested_class"));
    // Nor does Python source inside a string literal.
    assert!(has("TestSourceInAString::test_after_the_string_literal"));
    // And nothing over-collects: pytest ignores this class, so must we.
    assert!(
        !ids.iter().any(|i| i.contains("NotATestClass")),
        "a non-test class must stay uncollected; got {ids:#?}"
    );

    assert_eq!(
        results.len(),
        9,
        "exactly the 9 tests pytest would collect; got {ids:#?}"
    );
    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-26: {} did not pass; detail: {}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
