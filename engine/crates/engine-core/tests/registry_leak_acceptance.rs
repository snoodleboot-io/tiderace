//! TID-46 — a test's entry in another module's registry does not outlive it.
//!
//! click's suite registers a shell completion class with `add_completion_class`, which mutates a
//! module-level dict inside click. The next test asserts that dict is clean and failed, because the
//! in-process tier had kept the entry: 589 tests under `--no-optimistic`, 587 on the default ladder.
//! A result that depends on the tier is the shape of bug nobody can reproduce from the report.
//!
//! Snapshot/restore covers the *test module's* globals; a container living in an imported library is
//! outside it, and no amount of restoring the test's own module puts it back.
//!
//! **The hard half is telling pollution from a warm cache.** Both are additions to a module-level
//! dict. PIL registers its WEBP writer into `PIL.Image.SAVE` the first time `WebPImagePlugin` is
//! imported — tearing that out broke the next test that saved a WEBP, which is exactly what the first
//! cut of this did. The line is *origin*: an entry whose value the suite itself defined is the
//! suite's to clean up; one a library defined is the library warming up, and it stays.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ForkWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};

/// Stands in for click (a registry a test can add to) and for PIL (a registry the library fills in
/// lazily, which must survive).
const LIBRARY: &str = r#"
REGISTRY = {}


class BuiltIn:
    """Defined here, not in the test — the library's own object."""


def register(name, cls):
    REGISTRY[name] = cls


def lazy_self_registration():
    """What an import-time plugin hook does: the library registering its own handler."""
    REGISTRY.setdefault("builtin", BuiltIn)
"#;

const CORPUS: &str = r#"
import fakelib


def test_a_registers_its_own_class_and_warms_the_library():
    class MyHandler:
        pass

    fakelib.register("mine", MyHandler)
    fakelib.lazy_self_registration()
    assert "mine" in fakelib.REGISTRY
"#;

/// The next module on the same worker. Inside `test_registry.py` the entry stays — pytest would
/// leave it too — and it is pulled back out when the worker leaves that module (TID-81).
const NEXT_MODULE: &str = r#"
import fakelib


def test_b_sees_a_clean_registry_but_keeps_the_libraries_own_entry():
    assert "mine" not in fakelib.REGISTRY, (
        "a finished test's registration is still here: its neighbours are running in a world it "
        "changed"
    )
    assert fakelib.REGISTRY.get("builtin") is fakelib.BuiltIn, (
        "the library's own lazy registration was torn out — the next test that needs it will fail"
    )
"#;

#[test]
fn a_registry_entry_a_test_added_does_not_reach_the_next_test() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("registry");
    std::fs::create_dir_all(dir.join("fakelib")).unwrap();
    std::fs::write(dir.join("fakelib/__init__.py"), LIBRARY).unwrap();
    std::fs::write(dir.join("test_registry.py"), CORPUS).unwrap();
    std::fs::write(dir.join("test_registry_next.py"), NEXT_MODULE).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);

    // One worker on the optimistic ladder: the tests have to share a process for one to pollute the
    // other, which is the only arrangement in which this bug exists.
    let results = ForkWorker::launch_optimistic(&python, &shim(), &dir)
        .expect("wellspring with restore")
        .run(&items)
        .expect("optimistic batch runs");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-46: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
