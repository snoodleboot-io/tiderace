//! TID-37 — the run root's *basedir* goes on `sys.path`, not the run root itself.
//!
//! The shim used to insert whatever root it was handed, unconditionally. pytest instead walks up
//! past every directory holding `__init__.py` and inserts *that*. The difference matters whenever
//! the root is itself a package: `tests/` with an `__init__.py`, containing `tests/statistics/`,
//! makes that subpackage importable as bare `statistics` ahead of the standard library. Every module
//! in the run that touches it — directly or transitively — breaks, reported as ordinary test errors
//! with nothing pointing at the cause. On one real suite that was 73 of them.
//!
//! It bites unevenly, which is why it read as a batch-size effect rather than a naming one: a stdlib
//! module already in `sys.modules` when the path is poisoned (`types`, `json`, `os`) keeps working,
//! because nothing re-resolves it. Only names first imported *during* the run get captured — which
//! is why the originally-reported `types` case no longer reproduces while the hazard is unchanged.
//!
//! Fixing it required fixing something else first. `_discover` named test modules relative to the
//! run *root* while `_module_name` named them relative to the package basedir, so the root had to
//! stay importable and the insert could not be narrowed. Those two spellings now agree, which also
//! closes a latent double-import: while they disagreed, the same file could be imported twice under
//! two identities, and a module-level fixture could register against one copy while the test ran
//! against the other.

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
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
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

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t37_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A test package containing a subpackage named like a stdlib module does not capture that name.
///
/// `statistics` rather than the originally-reported `types`, deliberately: `types` is imported during
/// interpreter startup, so it is already in `sys.modules` and never re-resolved. Only a module first
/// imported *during* the run can be captured, so only such a module can test this.
#[test]
fn a_test_package_does_not_shadow_the_standard_library() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("shadow");
    let tests = dir.join("tests");
    std::fs::create_dir_all(tests.join("statistics")).unwrap();
    // `tests/` is a package, which is what makes it the wrong thing to put on `sys.path`.
    std::fs::write(tests.join("__init__.py"), "").unwrap();
    std::fs::write(tests.join("statistics/__init__.py"), "").unwrap();
    std::fs::write(
        tests.join("test_shadow.py"),
        "import statistics\n\
         \n\
         \n\
         def test_the_stdlib_statistics_resolves():\n\
         \x20   # Nothing exotic: the suite merely happens to contain a `statistics` package.\n\
         \x20   assert statistics.mean([1, 2, 3]) == 2\n",
    )
    .unwrap();

    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), 1);

    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &tests);
    let results = worker.run(&items).expect("batch runs");
    assert_eq!(
        results[0].outcome,
        Outcome::Passed,
        "TID-37: the suite's own `statistics` package must not displace the stdlib — {}",
        results[0].detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Discovery and execution import each file under **one** identity.
///
/// The two used to derive module names differently, so a file could be imported twice — once as
/// `test_x` and once as `tests.test_x` — producing two module objects with separate globals. A
/// module-level counter makes that directly observable: it is incremented at import time, and if the
/// module were imported twice under two names the copy the test reads would still say 1 while a
/// second copy existed. Asserting on `sys.modules` instead catches it properly.
#[test]
fn a_test_module_is_imported_under_exactly_one_name() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("identity");
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("__init__.py"), "").unwrap();
    std::fs::write(
        tests.join("test_identity.py"),
        "import sys\n\
         \n\
         \n\
         def test_this_file_has_one_module_object():\n\
         \x20   mine = [\n\
         \x20       n for n, m in list(sys.modules.items())\n\
         \x20       if getattr(m, \"__file__\", None) == __file__\n\
         \x20   ]\n\
         \x20   assert len(mine) == 1, f\"imported under {len(mine)} names: {sorted(mine)}\"\n",
    )
    .unwrap();

    let items = RegexCollector::new().collect(&tests).expect("collection");
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), &tests);
    let results = worker.run(&items).expect("batch runs");
    assert_eq!(
        results[0].outcome,
        Outcome::Passed,
        "TID-37: discovery and execution must agree on a file's module identity — {}",
        results[0].detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}
