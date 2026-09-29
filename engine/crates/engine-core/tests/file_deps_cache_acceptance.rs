//! TID-82 — the static import closure's per-file parse is carried across runs.
//!
//! TID-76 memoised it within a process; every worker still parsed the ~1,000 files its modules'
//! closures reach once per run, the +7% between capture-on and capture-off on pirn-core. Each worker
//! now writes what it parsed under `.tiderace-cache/file-deps/`, the next run's parent folds the
//! worker files into one index before forking, and an entry is used only while the file's mtime and
//! size are unchanged and `sys.path` is the one it was resolved under.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::skip_live;
use std::path::{Path, PathBuf};

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

fn write_corpus(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("conftest.py"),
        "import os, sys\nsys.path.insert(0, os.path.dirname(__file__))\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/__init__.py"), "").unwrap();
    std::fs::write(dir.join("src/one.py"), "def one():\n    return 1\n").unwrap();
    std::fs::write(dir.join("src/two.py"), "def two():\n    return 2\n").unwrap();
    std::fs::write(
        dir.join("tests/test_deps.py"),
        "from src.one import one\n\n\ndef test_one():\n    assert one() == 1\n",
    )
    .unwrap();
}

/// One run of the corpus in a fresh worker process, footprints on.
fn run_once(python: &str, dir: &Path) -> Vec<String> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    assert_eq!(items.len(), 1);
    std::env::set_var("TIDERACE_COVERAGE", "1");
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), dir);
    let results = worker.run(&items).expect("batch runs");
    drop(worker); // the process exits and writes its cache file at teardown
    std::env::remove_var("TIDERACE_COVERAGE");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, Outcome::Passed, "{}", results[0].detail);
    let mut deps = results[0].touched_files.clone();
    deps.sort();
    deps
}

fn index_of(dir: &Path) -> serde_json::Value {
    let index = dir.join(".tiderace-cache/file-deps/index.json");
    let text =
        std::fs::read_to_string(&index).unwrap_or_else(|e| panic!("{}: {e}", index.display()));
    serde_json::from_str(&text).expect("the index is JSON")
}

#[test]
fn the_closure_is_written_after_one_run_folded_by_the_next_and_invalidated_by_an_edit() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t82_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write_corpus(&dir);

    // Run 1: nothing cached; the worker parses and leaves a worker file behind.
    let first = run_once(&python, &dir);
    assert!(
        first.iter().any(|f| f == "src/one.py"),
        "footprint: {first:?}"
    );
    let cache_dir = dir.join(".tiderace-cache/file-deps");
    let worker_files = std::fs::read_dir(&cache_dir)
        .expect("the cache dir exists after a run")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("w-"))
        .count();
    assert!(worker_files >= 1, "TID-82: the worker wrote what it parsed");

    // Run 2: the parent folds the worker file into the index, and the footprint is the same.
    let second = run_once(&python, &dir);
    assert_eq!(second, first, "the cached closure is the parsed closure");
    let index = index_of(&dir);
    let files = index["files"].as_object().expect("files");
    assert!(
        files
            .keys()
            .any(|k| k.replace('\\', "/").ends_with("tests/test_deps.py")),
        "the index carries the test module's entry: {:?}",
        files.keys().collect::<Vec<_>>()
    );
    let leftovers = std::fs::read_dir(&cache_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("w-"))
        .count();
    assert_eq!(
        leftovers, 0,
        "worker files are folded into the index and removed"
    );

    // Edit the test module so it imports something new: the entry's mtime/size no longer match,
    // the file is parsed again, and the new dependency is in the footprint.
    std::fs::write(
        dir.join("tests/test_deps.py"),
        "from src.one import one\nfrom src.two import two\n\n\ndef test_one():\n    assert one() + two() == 3\n",
    )
    .unwrap();
    let third = run_once(&python, &dir);
    assert!(
        third.iter().any(|f| f == "src/two.py"),
        "TID-82: an edited file must be re-parsed, not served from the cache: {third:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The suite walker and the collector both skip the cache directory.
#[test]
fn the_cache_directory_is_never_collected() {
    let dir = std::env::temp_dir().join(format!("tiderace_t82_skip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write_corpus(&dir);
    std::fs::create_dir_all(dir.join(".tiderace-cache/file-deps")).unwrap();
    std::fs::write(
        dir.join(".tiderace-cache/test_not_a_test.py"),
        "def test_ghost():\n    assert False\n",
    )
    .unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(items.len(), 1, "only the suite's own test: {items:?}");
}
