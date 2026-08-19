//! Differential gate (TID-29): tiderace must collect the **same tests** as pytest and report the
//! **same outcome** for each, on a corpus that exercises the surface real suites use.
//!
//! This replaces a five-test version that compared outcomes only. Two things made it unable to
//! catch anything, and both are fixed here:
//!
//!   * **It never compared what was collected.** An uncollected test has no outcome to disagree
//!     about, which is exactly how 129 tests hid on a real corpus while the engine's own 234 tests
//!     stayed green (TID-26). The id-set assertion runs first and is the one that matters.
//!   * **It parsed only `PASSED`/`FAILED`**, so every skip was invisible — the whole of TID-16
//!     (`importorskip`) and TID-20 (collection hooks).
//!
//! Every defect in the TID-14 → TID-28 batch was found by running this comparison by hand. The
//! point of this file is that the next one costs a CI failure instead of a session.
//!
//! The corpus lives in `tests/differential_corpus/` as real files rather than string constants:
//! `conftest.py` sits *above* the run root deliberately (TID-19), and Python-inside-a-string is one
//! of the cases under test, which is unreadable when the whole corpus is itself a Rust string.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::runner::DEFAULT_DEADLINE_MS;
use engine_core::testing::skip_live;

/// Outcomes tiderace is knowingly expected to spell differently from pytest, as
/// `(node id suffix, tiderace, pytest)`.
///
/// Deliberately a fixed list of exactly the divergences that have a ticket, and deliberately
/// **verified**: any entry that stops diverging fails the run (see the end of the test). An
/// allowlist that merely tolerates differences rots into a list of forgotten bugs; one that breaks
/// when the bug is fixed cannot.
///
/// * `test_function_errors` — a body that raises is `error` here and `failed` under pytest, which
///   reserves `error` for tests it could not attempt. TID-30.
const KNOWN_DIVERGENCES: &[(&str, &str, &str)] =
    &[("test_styles.py::test_function_errors", "error", "failed")];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim_path() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

/// The corpus package root; `tests/` beneath it is the run root, so `conftest.py` is an ancestor.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/differential_corpus")
        .canonicalize()
        .expect("differential corpus")
}

/// An interpreter with pytest — the oracle. Without one there is nothing to differ against.
fn python_with_pytest() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut candidates: Vec<String> = Vec::new();
    if venv.exists() {
        candidates.push(venv.to_string_lossy().into_owned());
    }
    candidates.extend(["python3".to_string(), "python".to_string()]);
    candidates.into_iter().find(|cand| {
        Command::new(cand)
            .args(["-c", "import pytest"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// pytest's ids are relative to the package root (`tests/x.py::t`); tiderace's are relative to the
/// run root (`x.py::t`). Strip the one leading segment so the two are comparable.
fn strip_tests_prefix(id: &str) -> String {
    id.strip_prefix("tests/").unwrap_or(id).to_string()
}

/// The set of node ids pytest collects, from `--collect-only -q`.
///
/// One id per line and no other formatting, so this is parse-exact — which matters because ids
/// legitimately contain spaces and brackets (`test_x[a b]`, `test_x[(SELECT 1)]`).
fn pytest_collected(python: &str, pkg: &Path) -> Vec<String> {
    let out = Command::new(python)
        .args([
            "-m",
            "pytest",
            "--collect-only",
            "-q",
            "-p",
            "no:cacheprovider",
            "tests",
        ])
        .current_dir(pkg)
        .output()
        .expect("pytest --collect-only");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut ids: Vec<String> = stdout
        .lines()
        .filter(|l| l.contains("::"))
        .map(|l| strip_tests_prefix(l.trim()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// `node_id -> outcome` from pytest, via `-v`.
fn pytest_outcomes(python: &str, pkg: &Path) -> BTreeMap<String, String> {
    let out = Command::new(python)
        .args([
            "-m",
            "pytest",
            "-v",
            "--tb=no",
            "-p",
            "no:cacheprovider",
            "tests",
        ])
        .current_dir(pkg)
        .output()
        .expect("pytest -v");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut map = BTreeMap::new();
    for line in stdout.lines() {
        if !line.contains("::") {
            continue;
        }
        // The short summary at the end repeats failures as `FAILED <id> - <reason>`; those are not
        // per-test result lines and their reason text can end in anything.
        if [
            "FAILED ", "ERROR ", "PASSED ", "SKIPPED ", "XFAIL ", "XPASS ",
        ]
        .iter()
        .any(|kw| line.starts_with(kw))
        {
            continue;
        }
        // Drop a trailing progress column, e.g. "[ 42%]". `rfind` is deliberate: a node id can end
        // in `]` too (`test_x[(SELECT 1)]`), and the progress column is always last.
        let body = match line.rfind('[') {
            Some(i) if line[i..].ends_with(']') && line[i..].contains('%') => line[..i].trim_end(),
            _ => line.trim_end(),
        };
        // pytest appends a truncated reason to non-pass outcomes: `SKIPPED (sk...)`. Strip it, but
        // only when the body really ends in a paren group — an id may contain parens, and in that
        // case the line ends with the outcome word instead.
        let body = match body.rfind('(') {
            Some(i) if body.ends_with(')') => body[..i].trim_end(),
            _ => body,
        };
        // Parsed from the RIGHT: a node id may contain spaces, so splitting from the left truncates
        // ids like `test_x[a b]`.
        let Some((id, outcome)) = body.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let outcome = outcome.trim().to_ascii_lowercase();
        if matches!(
            outcome.as_str(),
            "passed" | "failed" | "skipped" | "error" | "xfail" | "xpass"
        ) {
            map.insert(strip_tests_prefix(id.trim()), outcome);
        }
    }
    map
}

/// tiderace's wire spelling for an outcome, to compare against pytest's.
fn wire(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "passed",
        Outcome::Failed => "failed",
        Outcome::Skipped => "skipped",
        Outcome::Error => "error",
        Outcome::XFail => "xfail",
        Outcome::XPass => "xpass",
    }
}

/// Format a set difference so a failure names the tests rather than the counts.
fn diff(label: &str, a: &[String], b: &[String]) -> String {
    let mut only: Vec<String> = a.iter().filter(|x| !b.contains(x)).cloned().collect();
    if only.is_empty() {
        return String::new();
    }
    only.sort();
    format!(
        "\n  {} only ({}):\n    {}",
        label,
        only.len(),
        only.join("\n    ")
    )
}

#[test]
fn tiderace_collects_and_reports_exactly_what_pytest_does() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest — the differential has no oracle without one");
        return;
    };
    let pkg = corpus_root();
    let run_root = pkg.join("tests");

    // --- the oracle ---
    let pytest_ids = pytest_collected(&python, &pkg);
    assert!(
        !pytest_ids.is_empty(),
        "the oracle collected nothing — the corpus or pytest invocation is broken, \
         and a differential against an empty oracle would pass vacuously"
    );

    // --- tiderace: collect (no import), then execute ---
    let items = RegexCollector::new().collect(&run_root).expect("collect");
    let mut worker = SubprocessWorker::new(DEFAULT_DEADLINE_MS, 1).with_target(
        python.clone(),
        &shim_path(),
        &run_root,
    );
    let results = worker.run(&items).expect("run corpus");

    let mut engine_ids: Vec<String> = results.iter().map(|r| r.node_id.to_string()).collect();
    engine_ids.sort();
    engine_ids.dedup();

    // COLLECTION FIRST. A test tiderace never collected has no outcome to disagree about, so
    // comparing outcomes alone cannot see it — which is how 129 tests stayed hidden (TID-26).
    assert_eq!(
        engine_ids,
        pytest_ids,
        "tiderace and pytest collected different tests ({} vs {}){}{}",
        engine_ids.len(),
        pytest_ids.len(),
        diff("pytest", &pytest_ids, &engine_ids),
        diff("tiderace", &engine_ids, &pytest_ids),
    );

    // --- outcomes, now that the sets are known equal ---
    let pytest_out = pytest_outcomes(&python, &pkg);
    assert_eq!(
        pytest_out.len(),
        pytest_ids.len(),
        "the oracle reported {} outcomes for {} collected tests — the `-v` parse is wrong, \
         and a partial oracle would let real disagreements through",
        pytest_out.len(),
        pytest_ids.len()
    );

    let mut mismatches: Vec<String> = Vec::new();
    let mut seen_divergences: Vec<&str> = Vec::new();
    for r in &results {
        let id = r.node_id.to_string();
        let ours = wire(r.outcome);
        let theirs = match pytest_out.get(&id) {
            Some(t) => t.as_str(),
            None => {
                mismatches.push(format!("{id}: tiderace={ours} pytest=<no outcome>"));
                continue;
            }
        };
        if theirs == ours {
            continue;
        }
        match KNOWN_DIVERGENCES
            .iter()
            .find(|(suffix, t, p)| id.ends_with(suffix) && *t == ours && *p == theirs)
        {
            Some((suffix, _, _)) => seen_divergences.push(suffix),
            None => mismatches.push(format!("{id}: tiderace={ours} pytest={theirs}")),
        }
    }
    assert!(
        mismatches.is_empty(),
        "outcomes disagree with pytest:\n    {}\n\nIf a difference here is intended, give it a \
         ticket and add it to KNOWN_DIVERGENCES rather than widening the comparison.",
        mismatches.join("\n    ")
    );

    // The half that keeps the list from rotting: an entry that no longer diverges is a bug that got
    // fixed, and the entry has to go with it.
    let stale: Vec<&str> = KNOWN_DIVERGENCES
        .iter()
        .map(|(suffix, _, _)| *suffix)
        .filter(|suffix| !seen_divergences.contains(suffix))
        .collect();
    assert!(
        stale.is_empty(),
        "KNOWN_DIVERGENCES lists differences that no longer occur — delete them and close their \
         tickets:\n    {}",
        stale.join("\n    ")
    );
}
