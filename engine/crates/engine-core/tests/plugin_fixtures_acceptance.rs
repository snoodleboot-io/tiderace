//! TID-87 — fixtures a pytest plugin provides are available, at the lowest precedence.
//!
//! `mocker` (pytest-mock) and `anyio_backend_name` (anyio's plugin) were `KeyError`s: the shim
//! registered a conftest's fixtures but never a plugin's. Discovery now enumerates the `pytest11`
//! entry points (plus `-p NAME` in `addopts` and each conftest's `pytest_plugins`), imports each
//! plugin module, and registers the fixtures it defines at the root location, after everything
//! else — so a conftest's fixture of the same name wins, `-p no:NAME` and `[tool.tiderace]
//! plugins = []` leave it out, and an ini value the plugin declared reads back through
//! `config.getini`.
//!
//! The fake plugin's `dist-info` is put on `sys.path` by the root conftest. pytest resolves entry
//! points before any conftest runs, so under pytest this corpus would need the plugin installed;
//! the shim's discovery order reads the entry points after the conftests, which is what lets one
//! temp directory stand in for `pip install`.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
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

/// The fixture venv: pytest is needed, since the plugin is written against `pytest.fixture`.
fn venv_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    venv.exists().then(|| venv.to_string_lossy().into_owned())
}

/// A distribution on `sys.path` that declares a `pytest11` entry point — what `pip install` of a
/// plugin leaves behind, minus pip: a module and a `dist-info` with `entry_points.txt`.
fn write_corpus(tag: &str, addopts: &str, tiderace_toml: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t87_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let plugins = dir.join("plugins");
    let info = plugins.join("fake_plugin-0.1.dist-info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        plugins.join("fake_plugin.py"),
        "import pytest\n\n\
         def pytest_addoption(parser):\n    \
             parser.addini('fake_flag', 'declared by the plugin', type='bool', default=True)\n\n\
         @pytest.fixture\n\
         def helper():\n    return 'from the plugin'\n\n\
         @pytest.fixture\n\
         def token():\n    return 'plugin token'\n",
    )
    .unwrap();
    std::fs::write(
        info.join("METADATA"),
        "Metadata-Version: 2.1\nName: fake-plugin\nVersion: 0.1\n",
    )
    .unwrap();
    std::fs::write(
        info.join("entry_points.txt"),
        "[pytest11]\nfake = fake_plugin\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("conftest.py"),
        "import os, sys\nimport pytest\n\
         sys.path.insert(0, os.path.join(os.path.dirname(__file__), 'plugins'))\n\n\
         @pytest.fixture\n\
         def token():\n    return 'conftest token'\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("pyproject.toml"),
        format!("[tool.pytest.ini_options]\naddopts = \"{addopts}\"\n\n[tool.tiderace]\n{tiderace_toml}\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("tests/test_plugin.py"),
        "def test_helper(helper):\n    assert helper == 'from the plugin'\n\n\
         def test_token(token):\n    assert token == 'conftest token'\n\n\
         def test_ini(pytestconfig):\n    assert pytestconfig.getini('fake_flag') is True\n\n\
         def test_ini_unknown(pytestconfig):\n    assert pytestconfig.getini('nobody_declared') is None\n",
    )
    .unwrap();
    dir
}

fn run(dir: &Path, python: &str) -> Vec<TestResult> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    assert_eq!(items.len(), 4, "{items:?}");
    SubprocessWorker::new(20_000, 1)
        .with_target(python.to_string(), &shim(), dir)
        .run(&items)
        .expect("batch runs")
}

fn outcome<'a>(results: &'a [TestResult], leaf: &str) -> &'a TestResult {
    results
        .iter()
        .find(|r| r.node_id.as_str().ends_with(leaf))
        .unwrap_or_else(|| panic!("{leaf} missing from {results:?}"))
}

#[test]
fn a_plugins_fixtures_resolve_and_a_conftest_of_the_same_name_wins() {
    let Some(python) = venv_python() else {
        skip_live("`.tiderace-fx-venv` not present");
        return;
    };
    let dir = write_corpus("plain", "-q", "");
    let results = run(&dir, &python);
    let _ = std::fs::remove_dir_all(&dir);
    for leaf in ["test_helper", "test_token", "test_ini", "test_ini_unknown"] {
        let r = outcome(&results, leaf);
        assert_eq!(r.outcome, Outcome::Passed, "{leaf}: {}", r.detail);
    }
}

#[test]
fn p_no_name_in_addopts_leaves_the_plugin_out() {
    let Some(python) = venv_python() else {
        skip_live("`.tiderace-fx-venv` not present");
        return;
    };
    let dir = write_corpus("disabled", "-q -p no:fake", "");
    let results = run(&dir, &python);
    let _ = std::fs::remove_dir_all(&dir);
    // A fixture nobody provides is a missing argument, as it is for any unknown name.
    let helper = outcome(&results, "test_helper");
    assert_eq!(helper.outcome, Outcome::Failed, "{}", helper.detail);
    assert!(helper.detail.contains("'helper'"), "{}", helper.detail);
    assert_eq!(outcome(&results, "test_token").outcome, Outcome::Passed);
    // The ini declaration went with the plugin: nobody declared it, so it reads unset.
    assert_eq!(outcome(&results, "test_ini").outcome, Outcome::Failed);
}

/// `[tool.tiderace] plugins = []` — the project-level opt-out (`TIDERACE_PLUGINS=none` is the same
/// switch from the environment, not exercised here: the workers of every scenario in this binary
/// inherit one process environment).
#[test]
fn an_empty_plugins_list_in_the_project_config_turns_every_plugin_off() {
    let Some(python) = venv_python() else {
        skip_live("`.tiderace-fx-venv` not present");
        return;
    };
    let dir = write_corpus("config", "-q", "plugins = []");
    let results = run(&dir, &python);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(outcome(&results, "test_helper").outcome, Outcome::Failed);
    assert_eq!(outcome(&results, "test_token").outcome, Outcome::Passed);
}

/// An allow-list keeps only the named plugins.
#[test]
fn a_plugins_allow_list_keeps_only_the_named_ones() {
    let Some(python) = venv_python() else {
        skip_live("`.tiderace-fx-venv` not present");
        return;
    };
    let dir = write_corpus("allow", "-q", "plugins = [\"fake\"]");
    let results = run(&dir, &python);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(outcome(&results, "test_helper").outcome, Outcome::Passed);
    assert_eq!(outcome(&results, "test_ini").outcome, Outcome::Passed);
}
