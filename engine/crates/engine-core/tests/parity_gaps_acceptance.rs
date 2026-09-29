//! TID-88 — four small parity gaps anyio exposed after TID-87, each checked against pytest's own
//! collection of the same corpus:
//!
//! 1. a `parametrize` of a name that is not one of the test's parameters but is a fixture in its
//!    closure sets that fixture's `request.param` (an indirect parametrize), rather than being
//!    passed to the test as a keyword it never asked for;
//! 2. duplicate ids are disambiguated the way the suite's pytest does it — `1_0`/`1_1` when the
//!    id ends in a digit (pytest 8+), `a0`/`a1` otherwise;
//! 3. a skip-marked parametrized test is reported once per variant, with the variant ids;
//! 4. a fixture whose name starts with `test` is not a test.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::skip_live;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

fn venv_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    venv.exists().then(|| venv.to_string_lossy().into_owned())
}

fn write_corpus() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t88_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_gaps.py"),
        "import pytest\n\n\
         SEEN = []\n\n\
         @pytest.fixture\n\
         def flavor(request):\n    SEEN.append(request.param)\n    return request.param\n\n\
         # 1. the name is a fixture in the closure (usefixtures), not one of the test's arguments\n\
         @pytest.mark.usefixtures('flavor')\n\
         @pytest.mark.parametrize('flavor', ['plain', 'spicy'])\n\
         def test_flavor_is_routed_to_the_fixture():\n    \
             assert SEEN[-1] in ('plain', 'spicy')\n\n\
         # 2. duplicate ids\n\
         @pytest.mark.parametrize('x', [1, 1, 'a', 'a'])\n\
         def test_duplicates(x):\n    assert x\n\n\
         # 3. a skip-marked parametrized test is one variant per case, each skipped\n\
         @pytest.mark.skipif(True, reason='always')\n\
         @pytest.mark.parametrize('y', ['p', 'q'])\n\
         def test_skipped_variants(y):\n    assert False\n\n\
         # 4. a fixture named like a test is not a test\n\
         class TestThing:\n    \
             @pytest.fixture\n    \
             def testdata(self):\n        return 3\n\n    \
             def test_uses_it(self, testdata):\n        assert testdata == 3\n\n\
         @pytest.fixture\n\
         def testhelper():\n    return 1\n\n\
         def test_module_fixture(testhelper):\n    assert testhelper == 1\n",
    )
    .unwrap();
    dir
}

/// pytest's own node ids for the corpus, from `--collect-only`.
fn pytest_ids(dir: &Path, python: &str) -> BTreeSet<String> {
    let out = Command::new(python)
        .args([
            "-m",
            "pytest",
            "--collect-only",
            "-q",
            "-p",
            "no:cacheprovider",
        ])
        .current_dir(dir)
        .env_remove("PYTHONPATH")
        .output()
        .expect("pytest runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("::"))
        .map(|l| l.trim().to_string())
        .collect()
}

fn tiderace_results(dir: &Path, python: &str) -> Vec<TestResult> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    SubprocessWorker::new(20_000, 1)
        .with_target(python.to_string(), &shim(), dir)
        .run(&items)
        .expect("batch runs")
}

#[test]
fn the_four_gaps_match_pytests_collection() {
    let Some(python) = venv_python() else {
        skip_live("`.tiderace-fx-venv` not present");
        return;
    };
    let dir = write_corpus();
    let expected = pytest_ids(&dir, &python);
    let results = tiderace_results(&dir, &python);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!expected.is_empty(), "pytest collected nothing");

    let got: BTreeMap<String, (Outcome, String)> = results
        .iter()
        .map(|r| (r.node_id.to_string(), (r.outcome, r.detail.clone())))
        .collect();
    let got_ids: BTreeSet<String> = got.keys().cloned().collect();
    assert_eq!(
        got_ids,
        expected,
        "node ids differ from pytest's\nonly tiderace: {:?}\nonly pytest: {:?}",
        got_ids.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&got_ids).collect::<Vec<_>>()
    );
    for (id, (outcome, detail)) in &got {
        let want = if id.contains("test_skipped_variants") {
            Outcome::Skipped
        } else {
            Outcome::Passed
        };
        assert_eq!(*outcome, want, "{id}: {detail}");
    }
    // Every expected id shape is present, spelled out so a regression names itself.
    for id in [
        "test_gaps.py::test_flavor_is_routed_to_the_fixture[plain]",
        "test_gaps.py::test_duplicates[1_0]",
        "test_gaps.py::test_duplicates[a1]",
        "test_gaps.py::test_skipped_variants[q]",
        "test_gaps.py::TestThing::test_uses_it",
    ] {
        assert!(got.contains_key(id), "{id} missing from {got_ids:?}");
    }
    assert!(!got_ids
        .iter()
        .any(|i| i.ends_with("::testdata") || i.ends_with("::testhelper")));
}
