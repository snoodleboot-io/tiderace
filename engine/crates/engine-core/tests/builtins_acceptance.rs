//! TID-21 — the builtin providers are registered, resolve, and tear down.
//!
//! `_register_builtins` in the shim is best-effort: if `import tiderace.builtins` fails it returns,
//! and **no** builtin is registered. The CI fixture venv is provisioned with numpy and pytest and
//! nothing else, so that import always failed there and `monkeypatch` / `tmp_path` / `capsys` /
//! `capfd` / `caplog` had **zero** coverage — the suite stayed green because no test in it ever
//! asked for one. That silence is how `tmp_path` sat recorded as 36 open errors long after it
//! worked (TID-14).
//!
//! This test asks for them, so a regression that unregisters a builtin fails a job instead of
//! passing unnoticed. It resolves `tiderace` the same way the shim does — via the spawned
//! interpreter's import path — and uses [`skip_live`] when that is not possible, which
//! `TIDERACE_REQUIRE_LIVE=1` turns into a hard failure in the jobs that promise the environment.

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

/// An interpreter that can `import tiderace` — the precondition for any builtin to exist.
///
/// CI puts `engine/py-tiderace` on `PYTHONPATH`, which the spawned workers inherit; a developer can
/// do the same. Without it there is nothing to assert, so this self-skips.
fn python_with_tiderace() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut candidates: Vec<String> = Vec::new();
    if venv.exists() {
        candidates.push(venv.to_string_lossy().into_owned());
    }
    candidates.extend(["python3".to_string(), "python".to_string()]);

    candidates.into_iter().find(|cand| {
        std::process::Command::new(cand)
            .args(["-c", "import tiderace.builtins"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// One test per builtin, each asserting the thing that would break if it silently vanished.
///
/// Written against the **pytest spellings** (`monkeypatch`, `tmp_path`, …) because that is the
/// compatibility surface; the by-type spellings are covered by `py-tiderace/proof_n5_builtins.py`
/// and `proof_n5b_caplog.py`.
const CORPUS: &str = "\
import logging
import os
import pathlib


def test_monkeypatch_sets_and_is_undone(monkeypatch):
    monkeypatch.setenv(\"TIDERACE_BUILTIN_PROBE\", \"yes\")
    assert os.environ[\"TIDERACE_BUILTIN_PROBE\"] == \"yes\"


def test_monkeypatch_undo_happened():
    # The previous test's teardown must have reversed it. Ordering is alphabetical within the
    # module, and this name sorts after the setter above.
    assert \"TIDERACE_BUILTIN_PROBE\" not in os.environ


def test_tmp_path_is_a_real_writable_dir(tmp_path):
    assert isinstance(tmp_path, pathlib.Path)
    f = tmp_path / \"data.txt\"
    f.write_text(\"hi\")
    assert f.read_text() == \"hi\"


def test_capsys_captures_stdout(capsys):
    print(\"captured-line\")
    assert capsys.readouterr().out == \"captured-line\\n\"


def test_capfd_captures_fd_writes(capfd):
    os.write(1, b\"fd-level-write\\n\")
    assert \"fd-level-write\" in capfd.readouterr().out


def test_caplog_records_carry_messages(caplog):
    logging.getLogger(\"probe\").warning(\"span finish\")
    assert any(\"span finish\" in rec.message for rec in caplog.records)
";

/// Every builtin the shim advertises. Named explicitly so *adding* one without covering it is a
/// visible omission rather than an invisible one.
const EXPECTED: &[&str] = &["monkeypatch", "tmp_path", "capsys", "capfd", "caplog"];

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_builtins_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_builtins.py"), CORPUS).unwrap();
    dir
}

#[test]
fn every_builtin_provider_resolves_and_tears_down() {
    let Some(python) = python_with_tiderace() else {
        skip_live(
            "no interpreter can import `tiderace` — put engine/py-tiderace on PYTHONPATH \
             (CI does; see TID-21)",
        );
        return;
    };
    let dir = write_corpus("run");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 6, "one test per builtin, plus the undo check");

    // `pool_size = 1` and the no-fork tier on purpose: the monkeypatch-undo assertion is only
    // meaningful if the previous test's teardown ran in *this* process. Under fork each child is a
    // pristine COW copy, which would make the undo check pass without proving anything.
    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs against real Python");

    for r in &results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "TID-21: builtin test {} did not pass — a provider is missing or broken; detail: {}",
            r.node_id,
            r.detail
        );
    }

    // Belt and braces: the corpus above is only as good as its coverage of the advertised set.
    for name in EXPECTED {
        assert!(
            CORPUS.contains(&format!("({name})")) || CORPUS.contains(&format!("{name})")),
            "builtin {name:?} is advertised but no test in this corpus requests it"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
