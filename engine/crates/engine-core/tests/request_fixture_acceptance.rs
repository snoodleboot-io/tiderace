//! TID-14 — a test asking for `request` gets one, and `request.config.getoption` works.
//!
//! `_bind_by_type` deliberately skips the parameter name `request`, so it never resolved as a
//! provider and the test was simply called without it: `TypeError: ... missing 1 required positional
//! argument: 'request'`. A test asking for `request` overwhelmingly wants
//! `request.config.getoption(...)` to decide whether to run at all, so `config` is the half that has
//! to be real — the option defaults come from the `pytest_addoption` hooks declared in conftests.
//!
//! Deliberately free of any `pytest` import: `request` is shim machinery, not a `tiderace.builtins`
//! provider, so this needs no `tiderace` install and runs on Windows CI's bare interpreter too.
//! (`caplog`, being a builtin provider, is proven in `py-tiderace/proof_n5b_caplog.py` instead —
//! the fx venv has no `tiderace` installed, so a builtin cannot be exercised from here.)

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

/// A conftest declaring an opt-in flag exactly the way real suites do, so `getoption` has something
/// to read. `--real` defaults to False, which is what makes the guarded test skip itself.
const CONFTEST: &str = "\
def pytest_addoption(parser):
    parser.addoption(
        \"--real\",
        action=\"store_true\",
        default=False,
        help=\"Run real-backend tests\",
    )
    parser.addoption(\"--endpoint\", default=\"http://localhost\")
";

/// The shape that actually failed in the wild, plus the identity attributes and the guard that an
/// undeclared option still raises rather than quietly reading as falsy.
const CORPUS: &str = "\
import unittest


def test_opt_in_flag_reads_its_declared_default(request):
    assert request.config.getoption(\"--real\") is False


def test_option_with_a_value_default(request):
    assert request.config.getoption(\"--endpoint\") == \"http://localhost\"


def test_dest_form_resolves_too(request):
    assert request.config.getoption(\"real\") is False


def test_explicit_default_is_used_for_undeclared(request):
    assert request.config.getoption(\"--never-declared\", \"fallback\") == \"fallback\"


def test_undeclared_option_raises(request):
    try:
        request.config.getoption(\"--never-declared\")
    except ValueError:
        return
    raise AssertionError(\"an undeclared option must raise, not read as falsy\")


def test_request_identifies_the_node(request):
    assert request.node.endswith(\"test_request_identifies_the_node\")
    assert request.function.__name__ == \"test_request_identifies_the_node\"
    assert request.param is None


def test_request_alongside_other_args(request, tmp_marker):
    assert request.config.getoption(\"--real\") is False
    assert tmp_marker == \"from-fixture\"


async def test_request_in_an_async_test(request):
    assert request.config.getoption(\"--real\") is False


class TestMethodStyle(unittest.TestCase):
    def test_request_on_a_method(self):
        # unittest methods drive their own setUp/tearDown and take no DI, so this only pins that
        # adding `request` injection did not disturb them.
        assert True
";

/// A user fixture, to prove `request` coexists with ordinary argument binding rather than replacing
/// it. Declared without pytest so the corpus stays importable on a bare interpreter.
const LOCAL_CONFTEST: &str = "\
import tiderace


@tiderace.provides
def tmp_marker() -> str:
    return \"from-fixture\"
";

fn write_corpus(tag: &str, with_fixture: bool) -> (PathBuf, PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pkg = std::env::temp_dir().join(format!(
        "tiderace_request_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&pkg);
    let tests = pkg.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        pkg.join("pyproject.toml"),
        "[project]\nname = \"request-probe\"\n",
    )
    .unwrap();
    // The option declaration lives in the ANCESTOR conftest, the way real suites write it — which
    // also pins that the TID-19 collection path feeds `pytest_addoption`.
    std::fs::write(pkg.join("conftest.py"), CONFTEST).unwrap();
    let body = if with_fixture {
        CORPUS.to_string()
    } else {
        CORPUS.replace(
            "def test_request_alongside_other_args(request, tmp_marker):\n    assert request.config.getoption(\"--real\") is False\n    assert tmp_marker == \"from-fixture\"\n\n\n",
            "",
        )
    };
    std::fs::write(tests.join("test_request.py"), body).unwrap();
    if with_fixture {
        std::fs::write(tests.join("conftest.py"), LOCAL_CONFTEST).unwrap();
    }
    (pkg, tests)
}

/// Whether the interpreter can `import tiderace` — the user-fixture case needs it to declare one.
fn has_tiderace(python: &str) -> bool {
    std::process::Command::new(python)
        .args(["-c", "import tiderace"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn a_test_asking_for_request_gets_a_working_one() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    // The `tmp_marker` case needs a declarable provider; drop it where `tiderace` is absent rather
    // than lose the other eight assertions.
    let with_fixture = has_tiderace(&python);
    let (pkg, run_root) = write_corpus("run", with_fixture);

    let items = RegexCollector::new()
        .collect(&run_root)
        .expect("collection");
    let expected_count = if with_fixture { 9 } else { 8 };
    assert_eq!(items.len(), expected_count, "collected the whole corpus");

    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &run_root);
    let results = worker.run(&items).expect("batch runs against real Python");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-14: {} did not pass; detail: {}",
            r.node_id,
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&pkg);
}
