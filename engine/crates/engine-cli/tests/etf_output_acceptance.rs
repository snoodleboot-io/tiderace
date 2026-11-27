//! TID-128 — `tiderace run --etf <path>` writes the run's per-test traces in the Estuary Trace
//! Format, through the `estuary_etf` binding.
//!
//! estuary reuses a result on any snapshot where everything the test read is unchanged, so a
//! trace that misses a read is a stale pass waiting for the edit that exposes it. Every assertion
//! here is made on the **decoded file** — what a consumer gets — not on what the emitter meant
//! to write:
//!
//!   * TID-131: the file decodes under the v0 reader with verification on, re-encodes to the
//!     same bytes, holds one record per node, and asking for it changes nothing else about the
//!     run. Without the optional binding, `--etf` is one clear error before anything runs.
//!   * TID-132: every trace holds its conftest chain and the config sections the run reads, and
//!     editing a conftest changes the traces of exactly the tests beneath it.
//!   * TID-133: on the TID-40 fixture — twenty tests per module, because at two the defect is
//!     invisible — every one of a module's twenty traces holds the source file the module
//!     imports, not only the first test's.
//!
//! The scenarios that decode need an interpreter with the binding; it is an optional
//! dependency on no package index, so they skip with a notice where it is absent and fail there
//! under `TIDERACE_REQUIRE_ETF=1` (`engine_core::testing::skip_etf`).

use engine_core::testing::{
    many_tests_corpus, python, require_etf_python, scratch, shim, skip_live, PythonNeeds,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};

/// Decodes an ETF file and prints what the assertions read, as JSON: the header, whether
/// re-encoding reproduces the bytes, and per check its outcome, its trace digest and its
/// flattened keys in text form (`file:src/mod_1.py`, a trailing `=absent` when the key names
/// nothing).
const DECODE: &str = r#"
import json, sys
import estuary_etf as etf
data = open(sys.argv[1], "rb").read()
f = etf.decode(data, verify=True)
header = dict(f.header)
toolchain = header.pop("toolchain_fragment")
header.pop("tree")
records = {}
for r in f.records:
    keys = [f"{kind}:{name.decode()}" + ("" if value is not None else "=absent")
            for kind, name, value in f.flatten(r["root"])]
    children, _ = f.fragment(r["root"])
    records[r["check"].decode()] = {
        "outcome": r["outcome"], "digest": f.trace_digest(r["root"]).hex(), "keys": sorted(keys),
        "has_toolchain": toolchain in children}
print(json.dumps({"header": header, "roundtrip": f.encode() == data, "records": records,
                  "fragments": f.fragment_count}))
"#;

struct Decoded {
    json: serde_json::Value,
}

impl Decoded {
    fn checks(&self) -> BTreeSet<String> {
        self.json["records"]
            .as_object()
            .expect("records by check")
            .keys()
            .cloned()
            .collect()
    }

    fn keys(&self, check: &str) -> BTreeSet<String> {
        self.json["records"][check]["keys"]
            .as_array()
            .unwrap_or_else(|| panic!("no record for {check}"))
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    }

    fn digests(&self) -> BTreeMap<String, String> {
        self.json["records"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(check, r)| (check.clone(), r["digest"].as_str().unwrap().to_string()))
            .collect()
    }

    /// The checks whose trace differs between two files of the same run.
    fn changed_since(&self, earlier: &Decoded) -> BTreeSet<String> {
        let before = earlier.digests();
        self.digests()
            .into_iter()
            .filter(|(check, digest)| before.get(check) != Some(digest))
            .map(|(check, _)| check)
            .collect()
    }
}

fn decode(python: &str, etf: &Path) -> Decoded {
    let out = Command::new(python)
        .args(["-c", DECODE])
        .arg(etf)
        .output()
        .expect("the decoder runs");
    assert!(
        out.status.success(),
        "estuary_etf.decode accepts the file (verify on): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Decoded {
        json: serde_json::from_slice(&out.stdout).expect("the decoder prints JSON"),
    }
}

/// `tiderace run -q [args…]`, on one worker's worth of nothing shared with a daemon.
fn tiderace(python: &str, args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tiderace"))
        .arg("run")
        .args(args)
        .env("TIDERACE_PYTHON", python)
        .env("TIDERACE_SHIM", shim())
        .env("TIDERACE_NO_DAEMON", "1")
        .output()
        .expect("the CLI runs")
}

fn run_with_etf(python: &str, root: &Path, etf: &Path, extra: &[&std::ffi::OsStr]) -> Output {
    let mut args: Vec<&std::ffi::OsStr> = vec!["--etf".as_ref(), etf.as_os_str()];
    args.extend_from_slice(extra);
    args.push(root.as_os_str());
    let out = tiderace(python, &args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("tiderace: etf: ") && etf.exists(),
        "--etf wrote {}: {stderr}",
        etf.display()
    );
    out
}

/// The per-test lines of a run, sorted: what "the same run" means when comparing two.
fn outcome_lines(out: &Output) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

#[test]
fn the_tid40_fixture_every_test_of_a_module_carries_the_source_its_module_imports() {
    let Some(python) = require_etf_python() else {
        return;
    };
    let dir = many_tests_corpus("t128_etf_tid40", 3, 20);
    std::fs::create_dir_all(dir.join(".git")).unwrap(); // what marks the repository root
    let etf_path = dir.join("run.etf");

    let plain = tiderace(&python, &[dir.as_os_str()]);
    let traced = run_with_etf(&python, &dir, &etf_path, &[]);
    assert_eq!(
        outcome_lines(&plain),
        outcome_lines(&traced),
        "TID-131: asking for a trace changes nothing about what the run reports"
    );
    assert_eq!(plain.status.code(), traced.status.code());

    let decoded = decode(&python, &etf_path);
    assert_eq!(decoded.json["header"]["harness"], "tiderace");
    assert_eq!(decoded.json["header"]["granularity"], "file");
    assert_eq!(decoded.json["header"]["u_version"], "");
    assert!(
        !decoded.json["header"]["harness_version"]
            .as_str()
            .unwrap()
            .is_empty(),
        "the header names the harness version"
    );
    assert_eq!(
        decoded.json["roundtrip"], true,
        "decode then encode is the same bytes"
    );
    assert_eq!(
        decoded.checks().len(),
        60,
        "one record per node: {:?}",
        decoded.checks()
    );

    for m in 0..3 {
        for t in 0..20 {
            let check = format!("tests/test_mod_{m}.py::test_case_{t}");
            let keys = decoded.keys(&check);
            for other in 0..3 {
                let source = format!("file:src/mod_{other}.py");
                assert_eq!(
                    keys.contains(&source),
                    other == m,
                    "TID-133: {check} reads {source} exactly when its module imports it — all \
                     twenty tests of the module, not the one that ran first. Its trace: {keys:?}"
                );
            }
            for want in [
                "file:src/__init__.py".to_string(),
                "file:conftest.py".to_string(),
                format!("file:tests/test_mod_{m}.py"),
                "tool:python".to_string(),
                "tool:pyenv".to_string(),
                "tool:tiderace".to_string(),
            ] {
                assert!(keys.contains(&want), "{check} reads {want}: {keys:?}");
            }
            assert_eq!(decoded.json["records"][&check]["outcome"], "pass");
            assert_eq!(
                decoded.json["records"][&check]["has_toolchain"], true,
                "the header's toolchain fragment is a child of every root"
            );
        }
    }

    // The same suite, started one directory down: the same checks under the same ids, reading
    // the same keys. A footprint that stopped at the run root named no source file at all.
    let below = dir.join("below.etf");
    std::fs::write(dir.join("pytest.ini"), "[pytest]\n").unwrap(); // tests/ needs a rootdir above it
    let again = dir.join("again.etf");
    run_with_etf(&python, &dir, &again, &[]);
    run_with_etf(&python, &dir.join("tests"), &below, &[]);
    let (from_root, from_below) = (decode(&python, &again), decode(&python, &below));
    assert_eq!(
        from_below.checks(),
        from_root.checks(),
        "`run tests/` and `run .` name the same checks"
    );
    // The one thing that differs is true of the runs themselves: started on tests/, the config
    // loader looks there first, so those four probes are reads of that run and not of the other.
    let probes: BTreeSet<String> = ["pytest.ini", "pyproject.toml", "tox.ini", "setup.cfg"]
        .iter()
        .map(|name| format!("file:tests/{name}=absent"))
        .collect();
    for check in from_root.checks() {
        let mut expected = from_root.keys(&check);
        expected.extend(probes.iter().cloned());
        assert_eq!(
            from_below.keys(&check),
            expected,
            "{check}: the same keys from either root, source files included"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A root conftest, a config file, and two test directories — one with its own conftest (which
/// imports a helper), one without.
fn write_layered_project() -> std::path::PathBuf {
    let dir = scratch("t128_etf_layers");
    for sub in ["tests/a", "tests/b"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let write = |rel: &str, body: &str| std::fs::write(dir.join(rel), body).unwrap();
    write("pytest.ini", "[pytest]\naddopts = -q\n");
    write(
        "pyproject.toml",
        "[project]\nname = \"layers\"\nversion = \"1\"\n",
    );
    write(
        "conftest.py",
        "import os, sys\nsys.path.insert(0, os.path.dirname(__file__))\n",
    );
    write("a_helper.py", "A = 1\n");
    write(
        "tests/a/conftest.py",
        "import pytest\nfrom a_helper import A\n\n\n@pytest.fixture\ndef a():\n    return A\n",
    );
    write(
        "tests/a/test_a.py",
        "import pytest\n\n\ndef test_a0(a):\n    assert a == 1\n\n\ndef test_a1(a):\n    assert a == 1\n\n\n\
         def test_a_fails(a):\n    assert a == 2\n\n\n@pytest.mark.skip(reason='not today')\n\
         def test_a_skipped():\n    assert False\n",
    );
    write(
        "tests/b/test_b.py",
        "def test_b0():\n    assert True\n\n\ndef test_b1():\n    assert True\n",
    );
    dir
}

#[test]
fn every_trace_holds_its_conftest_chain_and_config_and_an_edit_reaches_exactly_what_is_below() {
    let Some(python) = require_etf_python() else {
        return;
    };
    if !Command::new(&python)
        .args(["-c", "import pytest"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        skip_live("the interpreter with estuary_etf has no pytest");
        return;
    }
    let dir = write_layered_project();
    let emit = |name: &str| {
        let path = dir.join(name);
        let out = run_with_etf(&python, &dir, &path, &[]);
        assert_eq!(
            out.status.code(),
            Some(1),
            "one test fails; the trace is written all the same"
        );
        decode(&python, &path)
    };
    let base = emit("base.etf");
    let under_a: BTreeSet<String> = ["a0", "a1", "a_fails", "a_skipped"]
        .iter()
        .map(|t| format!("tests/a/test_a.py::test_{t}"))
        .collect();
    let under_b: BTreeSet<String> = ["b0", "b1"]
        .iter()
        .map(|t| format!("tests/b/test_b.py::test_{t}"))
        .collect();
    let all: BTreeSet<String> = under_a.union(&under_b).cloned().collect();
    assert_eq!(base.checks(), all);

    // TID-132, the chain: every conftest from the root down, present or not.
    let a0 = base.keys("tests/a/test_a.py::test_a0");
    for want in [
        "file:conftest.py",
        "file:tests/conftest.py=absent",
        "file:tests/a/conftest.py",
        "file:a_helper.py",
    ] {
        assert!(a0.contains(want), "test_a0 reads {want}: {a0:?}");
    }
    let b0 = base.keys("tests/b/test_b.py::test_b0");
    for want in [
        "file:conftest.py",
        "file:tests/conftest.py=absent",
        "file:tests/b/conftest.py=absent",
    ] {
        assert!(b0.contains(want), "test_b0 reads {want}: {b0:?}");
    }
    assert!(
        !b0.iter()
            .any(|k| k.contains("tests/a/") || k.contains("a_helper")),
        "a conftest governs its own directory and nothing beside it: {b0:?}"
    );
    // TID-132, the config: the sections the run reads, each its own key.
    for want in [
        "derived:cfg/pytest.ini#pytest",
        "derived:cfg/pyproject.toml#tool.pytest.ini_options",
        "derived:cfg/pyproject.toml#tool.tiderace",
        "file:tox.ini=absent",
        "file:setup.cfg=absent",
    ] {
        assert!(b0.contains(want), "test_b0 reads {want}: {b0:?}");
    }
    // The outcomes, in ETF's four.
    assert_eq!(
        base.json["records"]["tests/a/test_a.py::test_a_fails"]["outcome"],
        "fail"
    );
    assert_eq!(
        base.json["records"]["tests/a/test_a.py::test_a_skipped"]["outcome"],
        "skip"
    );

    // Nothing edited: nothing changes, whatever the clock says.
    assert_eq!(emit("same.etf").changed_since(&base), BTreeSet::new());

    // A directory's conftest: exactly the tests beneath it.
    let append = |rel: &str, text: &str| {
        let path = dir.join(rel);
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(text);
        std::fs::write(&path, body).unwrap();
    };
    append("tests/a/conftest.py", "\nEDITED = 1\n");
    let after_a = emit("after_a.etf");
    assert_eq!(
        after_a.changed_since(&base),
        under_a,
        "TID-132: editing tests/a/conftest.py invalidates the tests under tests/a and no other"
    );
    // What that conftest imports: the same tests.
    append("a_helper.py", "\nEDITED = 1\n");
    let after_helper = emit("after_helper.etf");
    assert_eq!(after_helper.changed_since(&after_a), under_a);
    // A conftest appearing where there was none.
    std::fs::write(dir.join("tests/b/conftest.py"), "X = 1\n").unwrap();
    let after_b = emit("after_b.etf");
    assert_eq!(
        after_b.changed_since(&after_helper),
        under_b,
        "a new conftest invalidates what is beneath it: the key was there, naming nothing"
    );
    // The root conftest: everything.
    append("conftest.py", "\nEDITED = 1\n");
    let after_root = emit("after_root.etf");
    assert_eq!(after_root.changed_since(&after_b), all);
    // A config section the run does not read: nothing. One it does: everything.
    append("pyproject.toml", "description = \"edited\"\n");
    let after_project = emit("after_project.etf");
    assert_eq!(
        after_project.changed_since(&after_root),
        BTreeSet::new(),
        "[project] is not a section the run reads"
    );
    append("pytest.ini", "markers =\n    slow: takes a while\n");
    assert_eq!(emit("after_ini.etf").changed_since(&after_project), all);
    let _ = std::fs::remove_dir_all(&dir);
}

/// TID-131: the binding is optional, and `--etf` without it is one error before anything runs —
/// not a suite's worth of waiting followed by a traceback. Needs no binding, so this one runs
/// everywhere: a module of the same name that refuses to import stands in for its absence.
#[test]
fn without_the_binding_etf_is_one_clear_error_and_nothing_runs() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = many_tests_corpus("t128_etf_absent", 1, 2);
    let blocker = scratch("t128_etf_blocker");
    std::fs::write(
        blocker.join("estuary_etf.py"),
        "raise ImportError('No module named estuary_etf')\n",
    )
    .unwrap();
    let etf_path = dir.join("run.etf");

    let out = Command::new(env!("CARGO_BIN_EXE_tiderace"))
        .arg("run")
        .arg("--etf")
        .arg(&etf_path)
        .arg(&dir)
        .env("TIDERACE_PYTHON", &python)
        .env("TIDERACE_SHIM", shim())
        .env("TIDERACE_NO_DAEMON", "1")
        .env("PYTHONPATH", &blocker)
        .output()
        .expect("the CLI runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an error, not a test verdict: {stderr}"
    );
    assert!(
        stderr.contains("error: --etf writes its output through the `estuary_etf` module")
            && stderr.contains("pip install 'tiderace[estuary]'"),
        "the message names what is missing and how to get it: {stderr}"
    );
    assert!(
        out.stdout.is_empty() && !stderr.contains(" total"),
        "nothing ran: {stderr}"
    );
    assert!(!etf_path.exists());

    // And the same run without the flag never looks for the module at all.
    let plain = Command::new(env!("CARGO_BIN_EXE_tiderace"))
        .arg("run")
        .arg(&dir)
        .env("TIDERACE_PYTHON", &python)
        .env("TIDERACE_SHIM", shim())
        .env("TIDERACE_NO_DAEMON", "1")
        .env("PYTHONPATH", &blocker)
        .output()
        .expect("the CLI runs");
    assert!(
        String::from_utf8_lossy(&plain.stderr).contains("2 passed"),
        "a run that did not ask for a trace does not need the binding"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&blocker);
}
