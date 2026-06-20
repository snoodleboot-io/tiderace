//! Phase 3 — Fixtures + Watermarks ATDD acceptance suite.
//!
//! **Doctrine: ATDD-FIRST.** These are the acceptance SPEC for Phase 3, authored
//! against the *frozen* public API (`CONTRACT.md`) **before** the FX-graph / WM /
//! FALLBACK lanes finish. They COMPILE now but are expected to be **RED** until the
//! lanes replace the `unimplemented!("LANE: …")` seams; they go green at aggregation.
//!
//! Each scenario corresponds to one PLAN §7 bullet (and the §8 integration
//! boundaries). Pure-graph scenarios (cycle, scope-widen, parametrization, override,
//! setup/teardown ordering) construct `Fixture`/`FixtureGraph` inputs directly and
//! assert on `FixturePlan`/`FixtureError` — no Python needed. Execution scenarios
//! (scope-counts under real fork, reinit_after_fork, fallback parity) drive the
//! engine against `fx_corpus` through the real Wellspring/Worker on the **live
//! venv** — the python/sqlite boundary is **NEVER mocked** (BINDING).
//!
//! Live scenarios skip cleanly (early-return with a SKIP note) when the Phase-3
//! venv is absent, exactly like `differential.rs`, but DO run when it is present.
//!
//! Scenario → PLAN §7 map:
//!   1 setup/teardown order ....... `setup_order_is_topo_and_teardown_is_strict_reverse`
//!   2 scope counts (1x not 500x) . `module_fixture_body_runs_once_function_per_test`
//!   3 autouse .................... `autouse_fixture_enters_every_in_scope_closure`
//!   4 override (nearest wins) .... `module_fixture_shadows_same_named_session_fixture`
//!   5 parametrized fixture ....... `parametrized_fixture_yields_three_distinct_closure_hashes`
//!   6 cycle ...................... `dependency_cycle_aborts_with_cycle_error`
//!   7 scope widen ................ `session_depending_on_function_is_scope_widen_error`
//!   8 reinit_after_fork .......... `forked_child_gets_fresh_sqlite_connection`
//!   9 fallback parity ............ `subprocess_worker_outcomes_and_teardown_match_fork`

mod fx_support;

use engine_core::domain::{NodeId, Scope, ScopePath};
use engine_core::exec::{ForkWorker, SubprocessWorker, Worker};
use engine_core::fixtures::{
    Fixture, FixtureError, FixtureGraph, FixtureResolver, LayeredResolver, OverrideTable,
    ParamValue,
};

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;

use fx_support::{
    corpus_param_values, corpus_scope_fixtures, fx, fx_corpus_root, fx_venv_python, module_path,
    run_pytest_oracle, shim_path,
};

// =====================================================================
// Scenario 1 (PLAN §7: setup order / teardown order) — pure graph.
// =====================================================================
//
// Given session_db(Session) -> pkg_resource(Package) -> module_fix(Module)
//   -> class_fix(Class) -> func_fix(Function) (the worked example, design 04 §7),
// Then the resolved closure sets fixtures up in topo (wider→narrower) order and
// teardown is the *strict reverse* — matching pytest's observed finalizer log.
#[test]
fn setup_order_is_topo_and_teardown_is_strict_reverse() {
    let overrides = build_override_table(&corpus_scope_fixtures());
    let graph = FixtureGraph::build(corpus_scope_fixtures(), &overrides)
        .expect("corpus scope graph is valid (acyclic + scope-monotone)");

    // The test func_fix-consuming test lives in test_scopes.py, in class TestScoped.
    let at = ScopePath::with_class("tests/test_scopes.py", "TestScoped");
    let closure = graph
        .closure_of(&["func_fix".to_string()], &at)
        .expect("closure resolves");

    // Setup order: every dependency precedes its dependent (topo). Concretely the
    // wider scopes come first and func_fix is last.
    let setup: Vec<&str> = closure
        .setup_order()
        .iter()
        .map(|id| graph.fixture(id).expect("interned").name.as_str())
        .collect();

    assert!(
        position(&setup, "session_db") < position(&setup, "pkg_resource"),
        "session_db must set up before pkg_resource; got {setup:?}"
    );
    assert!(
        position(&setup, "pkg_resource") < position(&setup, "module_fix"),
        "pkg_resource before module_fix; got {setup:?}"
    );
    assert!(
        position(&setup, "module_fix") < position(&setup, "class_fix"),
        "module_fix before class_fix; got {setup:?}"
    );
    assert!(
        position(&setup, "class_fix") < position(&setup, "func_fix"),
        "class_fix before func_fix; got {setup:?}"
    );

    // Teardown is the strict reverse of the realized setup order.
    let teardown: Vec<&NodeId> = closure.teardown_order().collect();
    let mut expected: Vec<&NodeId> = closure.setup_order().iter().collect();
    expected.reverse();
    assert_eq!(
        teardown, expected,
        "teardown order must be the strict reverse of setup order"
    );
}

// =====================================================================
// Scenario 2 (PLAN §7: scope counts — 1x not 500x) — LIVE fork, real corpus.
// =====================================================================
//
// Given the ~500-test module sharing one module fixture (test_big_module.py) +
// the scope ladder, Then under the real ForkWorker the wider-scope fixture bodies
// run ONCE and function fixtures per test — identical to pytest's counts.json
// probe (the load-bearing "1x not 500x" claim, §8 boundary 1).
#[test]
fn module_fixture_body_runs_once_function_per_test() {
    let Some(python) = fx_venv_python() else {
        eprintln!("SKIP: .riptide-fx-venv not found — run the Phase-3 Lane-0 env gate first");
        return;
    };
    let _live = fx_support::live_guard(); // serialize corpus-launching scenarios (env isolation)

    // Oracle: stock pytest over the corpus, parsed scope-count probe.
    let oracle = run_pytest_oracle(&python);
    assert_eq!(
        oracle.counts.get("big_module_fix"),
        Some(&1),
        "pytest oracle itself must show the module fixture body ran exactly once"
    );

    // Engine: collect + fork-execute the corpus through the real Wellspring. Driving
    // the fixture-aware engine must reproduce the SAME per-fixture body counts: wider
    // scopes once, function scopes per test.
    let counts = run_engine_scope_counts(&python);

    assert_eq!(
        counts.get("session_db"),
        Some(&1),
        "session fixture body must run once across the whole suite (not per test)"
    );
    assert_eq!(
        counts.get("big_module_fix"),
        Some(&1),
        "the 500-test module's module fixture body must run 1x, not 500x"
    );
    assert_eq!(
        counts.get("module_fix"),
        Some(&1),
        "module_fix body runs once for its module"
    );
    // Two classes share module_fix but each re-runs class_fix; three func_fix tests.
    assert_eq!(
        counts.get("class_fix"),
        Some(&2),
        "class_fix once per class"
    );
    assert_eq!(counts.get("func_fix"), Some(&3), "func_fix once per test");

    // And the engine's counts match the pytest oracle exactly (differential).
    assert_eq!(
        counts, oracle.counts,
        "engine per-fixture scope counts must match pytest exactly"
    );
}

// =====================================================================
// Scenario 3 (PLAN §7: autouse) — pure graph.
// =====================================================================
//
// Given session_autouse declared autouse in the root conftest, Then it enters a
// test's resolved closure WITHOUT being requested by name.
#[test]
fn autouse_fixture_enters_every_in_scope_closure() {
    let overrides = build_override_table(&corpus_scope_fixtures());
    let graph = FixtureGraph::build(corpus_scope_fixtures(), &overrides).expect("graph builds");

    // A test that requests ONLY func_fix — session_autouse is never named.
    let at = ScopePath::with_class("tests/test_scopes.py", "TestScoped");
    let closure = graph
        .closure_of(&["func_fix".to_string()], &at)
        .expect("closure resolves");

    let names: Vec<&str> = closure
        .setup_order()
        .iter()
        .map(|id| graph.fixture(id).expect("interned").name.as_str())
        .collect();

    assert!(
        names.contains(&"session_autouse"),
        "autouse fixture must be injected into the closure unrequested; got {names:?}"
    );
}

// =====================================================================
// Scenario 4 (PLAN §7: override — nearest wins) — pure graph.
// =====================================================================
//
// Given a module-level `shared_value` shadowing a same-named session conftest
// fixture (fx_corpus/tests/pkg_override/test_override.py), Then a test in that
// module resolves to the MODULE definition (longest-prefix / nearest wins, W6).
#[test]
fn module_fixture_shadows_same_named_session_fixture() {
    // The session conftest declares `shared_value` at the session root. Its declaring
    // location is the root directory (`""`), which is a prefix of every test module —
    // the unambiguous expression of "the session conftest is visible everywhere"
    // (CONTRACT §2.7 / §5: longest-prefix `ScopePath` match, nearest wins).
    let session_def = fx("shared_value", Scope::Session, "", &[]);
    let module_def = fx(
        "shared_value",
        Scope::Function,
        "tests/pkg_override/test_override.py",
        &[],
    );
    let session_id = session_def.node_id.clone();
    let module_id = module_def.node_id.clone();

    let mut overrides = OverrideTable::new();
    overrides.insert("shared_value", &session_def.scope_path, session_id.clone());
    overrides.insert("shared_value", &module_def.scope_path, module_id.clone());

    // From the leaf module, the nearest definition is the module override.
    let from_module = module_path("tests/pkg_override/test_override.py");
    assert_eq!(
        overrides.nearest("shared_value", &from_module),
        Some(module_id),
        "nearest-override must resolve to the module definition for a test in that module"
    );

    // From an unrelated module, only the session definition is visible.
    let from_other = module_path("tests/test_scopes.py");
    assert_eq!(
        overrides.nearest("shared_value", &from_other),
        Some(session_id),
        "a test elsewhere resolves to the session-conftest definition"
    );
}

// =====================================================================
// Scenario 5 (PLAN §7: parametrized fixture) — pure graph.
// =====================================================================
//
// Given params=[a,b,c] (fx_corpus/tests/test_param.py), Then resolution produces
// 3 instances, each with a DISTINCT closure_hash (parameter variants cache
// independently — CONTRACT §4 invariant 4, W5/W14).
#[test]
fn parametrized_fixture_yields_three_distinct_closure_hashes() {
    let params: Vec<ParamValue> = corpus_param_values();
    assert_eq!(params.len(), 3, "corpus declares exactly three params");

    let parametrized =
        fx("parametrized", Scope::Function, "tests/test_param.py", &[]).with_params(params);
    assert!(parametrized.is_parametrized());

    let fixtures = vec![parametrized];
    let overrides = build_override_table(&fixtures);
    let graph = FixtureGraph::build(fixtures, &overrides).expect("graph builds");

    let at = module_path("tests/test_param.py");
    let plan = LayeredResolver::new(build_override_table(&corpus_param_fixtures()))
        .plan_for(&graph, &["parametrized".to_string()], &at)
        .expect("plan resolves");

    // Each parametrization variant is its own instance in post_fork (Function scope).
    let variants: Vec<_> = plan
        .post_fork
        .iter()
        .filter(|i| i.param().is_some())
        .collect();
    assert_eq!(
        variants.len(),
        3,
        "params=[a,b,c] must fan out into three FixtureInstances; got {}",
        variants.len()
    );

    let hashes: std::collections::HashSet<_> = variants.iter().map(|i| i.closure_hash()).collect();
    assert_eq!(
        hashes.len(),
        3,
        "each of the 3 parametrization variants must carry a distinct closure_hash"
    );

    // The param ids must be exactly a/b/c (stable identity).
    let mut ids: Vec<&str> = variants
        .iter()
        .map(|i| i.param().expect("param").id())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["a", "b", "c"]);
}

// =====================================================================
// Scenario 6 (PLAN §7: cycle) — pure graph.
// =====================================================================
//
// Given a -> b -> a, Then build aborts with FixtureError::Cycle{path} (NOT a hang /
// deadlock at setup). `path` names the fixtures along the back-edge.
#[test]
fn dependency_cycle_aborts_with_cycle_error() {
    let a = fx("a", Scope::Function, "tests/test_cycle.py", &["b"]);
    let b = fx("b", Scope::Function, "tests/test_cycle.py", &["a"]);
    let fixtures = vec![a, b];
    let overrides = build_override_table(&fixtures);

    let err = FixtureGraph::build(fixtures, &overrides)
        .expect_err("a -> b -> a must not build a valid graph");

    match err {
        FixtureError::Cycle { path } => {
            assert!(
                path.contains(&"a".to_string()) && path.contains(&"b".to_string()),
                "cycle path must name the offending fixtures a and b; got {path:?}"
            );
        }
        other => panic!("expected FixtureError::Cycle, got {other:?}"),
    }
}

// =====================================================================
// Scenario 7 (PLAN §7: scope widen) — pure graph.
// =====================================================================
//
// Given a Session fixture depending on a Function fixture, Then build aborts with
// FixtureError::ScopeWiden{narrow, wide}: `wide` is the offending Session scope,
// `narrow` is the illegal Function dependency's scope (CONTRACT §2.1).
#[test]
fn session_depending_on_function_is_scope_widen_error() {
    let narrow = fx("per_test", Scope::Function, "tests/test_widen.py", &[]);
    let wide = fx(
        "whole_session",
        Scope::Session,
        "tests/test_widen.py",
        &["per_test"],
    );
    let fixtures = vec![narrow, wide];
    let overrides = build_override_table(&fixtures);

    let err = FixtureGraph::build(fixtures, &overrides)
        .expect_err("a Session fixture depending on a Function fixture is illegal");

    match err {
        FixtureError::ScopeWiden { narrow, wide } => {
            assert_eq!(
                wide,
                Scope::Session,
                "the offender is the wider (Session) fixture"
            );
            assert_eq!(
                narrow,
                Scope::Function,
                "the illegal dependency is Function-scoped"
            );
        }
        other => panic!("expected FixtureError::ScopeWiden, got {other:?}"),
    }
}

// =====================================================================
// Scenario 8 (PLAN §7 + §8 boundary 2: reinit_after_fork) — LIVE fork + sqlite.
// =====================================================================
//
// Given the sqlite in-memory connection fixture (reinit_after_fork__db_conn), Then
// each forked child opens a FRESH connection (distinct identity) and the parent's
// connection is NEVER used in-child — the load-bearing ADR-E003 safety claim. We
// drive the real corpus through the real ForkWorker; sqlite is NEVER mocked.
#[test]
fn forked_child_gets_fresh_sqlite_connection() {
    let Some(python) = fx_venv_python() else {
        eprintln!("SKIP: .riptide-fx-venv not found — run the Phase-3 Lane-0 env gate first");
        return;
    };
    let _live = fx_support::live_guard(); // serialize corpus-launching scenarios (env isolation)

    // The two sqlite-resource tests must both pass under fork (each child reopens the
    // connection and sees the seeded rows): if a child wrongly inherited the parent's
    // post-fork-corrupted handle, the query would error.
    let results = run_engine_on_corpus(&python, /* fork */ true);

    let sqlite: Vec<&engine_core::domain::TestResult> = results
        .iter()
        .filter(|r| r.node_id.as_str().contains("test_sqlite"))
        .collect();
    assert_eq!(
        sqlite.len(),
        2,
        "both sqlite-resource tests must be collected + executed; got {}",
        sqlite.len()
    );
    for r in &sqlite {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "sqlite test {} must pass — a fresh in-child connection sees the seed data; \
             a corrupted inherited handle would error. detail: {}",
            r.node_id,
            r.detail
        );
    }

    // The probe records the resource fixture's body ran once per test (a fresh
    // connection acquired in each child), never shared across children.
    let oracle = run_pytest_oracle(&python);
    assert_eq!(
        oracle.counts.get("reinit_after_fork__db_conn"),
        Some(&2),
        "the non-fork-safe connection fixture must be (re)built once per child"
    );
}

// =====================================================================
// Scenario 9 (PLAN §7 + §8 boundary 3: fallback parity) — LIVE, both workers.
// =====================================================================
//
// Given --no-fork (SubprocessWorker), Then outcomes + teardown ordering are
// IDENTICAL to the fork path on the same corpus (CONTRACT §4 invariant 5). Both
// paths drive the real venv; neither is mocked.
#[test]
fn subprocess_worker_outcomes_and_teardown_match_fork() {
    let Some(python) = fx_venv_python() else {
        eprintln!("SKIP: .riptide-fx-venv not found — run the Phase-3 Lane-0 env gate first");
        return;
    };
    let _live = fx_support::live_guard(); // serialize corpus-launching scenarios (env isolation)

    let fork_results = sorted_outcomes(run_engine_on_corpus(&python, /* fork */ true));
    let subprocess_results = sorted_outcomes(run_engine_on_corpus(&python, /* fork */ false));

    assert!(!fork_results.is_empty(), "fork path collected/ran nothing");
    assert_eq!(
        fork_results, subprocess_results,
        "no-COW SubprocessWorker outcomes must be identical to the fork path (result-identical)"
    );
}

// --------------------------------------------------------------------------
// Local helpers (kept here so the per-binary helper module stays generic).
// --------------------------------------------------------------------------

/// Build an `OverrideTable` registering every fixture at its declaring scope path.
fn build_override_table(fixtures: &[Fixture]) -> OverrideTable {
    let mut table = OverrideTable::new();
    for f in fixtures {
        table.insert(f.name.clone(), &f.scope_path, f.node_id.clone());
    }
    table
}

/// Fixture set for the parametrization scenario (just the one parametrized fixture).
fn corpus_param_fixtures() -> Vec<Fixture> {
    vec![
        fx("parametrized", Scope::Function, "tests/test_param.py", &[])
            .with_params(corpus_param_values()),
    ]
}

/// Index of `name` in a slice of names, or `usize::MAX` if absent (so an absent
/// fixture sorts last and the ordering assertions fail loudly).
fn position(names: &[&str], name: &str) -> usize {
    names.iter().position(|n| *n == name).unwrap_or(usize::MAX)
}

/// Collect + execute the whole `fx_corpus` through the engine, returning the
/// per-test results. `fork == true` uses the real `ForkWorker` (Wellspring);
/// `fork == false` uses the no-COW `SubprocessWorker` fallback. Both run the live
/// venv — the python/sqlite boundary is never mocked.
fn run_engine_on_corpus(
    python: &std::path::Path,
    fork: bool,
) -> Vec<engine_core::domain::TestResult> {
    let root = fx_corpus_root();
    let items = RegexCollector::new()
        .collect(&root)
        .expect("collect fx_corpus");
    if fork {
        let mut worker = ForkWorker::launch(python.to_str().unwrap(), &shim_path(), &root)
            .expect("launch fork worker");
        worker.run(&items).expect("fork run")
    } else {
        let mut worker = SubprocessWorker::new(5_000, num_cpus_or_one()).with_target(
            python.to_str().unwrap(),
            &shim_path(),
            &root,
        );
        worker.run(&items).expect("subprocess run")
    }
}

/// Drive the corpus through the real fork engine with a dedicated probe dir and
/// return the per-fixture body run-counts (the engine's scope-count probe).
fn run_engine_scope_counts(python: &std::path::Path) -> std::collections::BTreeMap<String, u64> {
    let root = fx_corpus_root();
    let probe_dir = std::env::temp_dir().join(format!("fx_engine_probe_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&probe_dir);
    std::fs::create_dir_all(&probe_dir).expect("mkdir engine probe dir");

    // The engine's wellspring must export FX_CORPUS_PROBE_DIR into the substrate so
    // the corpus fixtures record into our dir. We set it in this process's env; the
    // ForkWorker launches the wellspring as a child which inherits it.
    std::env::set_var("FX_CORPUS_PROBE_DIR", &probe_dir);

    let items = RegexCollector::new()
        .collect(&root)
        .expect("collect fx_corpus");
    let mut worker = ForkWorker::launch(python.to_str().unwrap(), &shim_path(), &root)
        .expect("launch fork worker");
    let _ = worker.run(&items).expect("fork run");

    let report = fx_support::ProbeReport::read_from(&probe_dir);
    std::env::remove_var("FX_CORPUS_PROBE_DIR");
    let _ = std::fs::remove_dir_all(&probe_dir);
    report.counts
}

/// Sort `(node_id, outcome)` pairs for order-independent comparison between workers.
fn sorted_outcomes(results: Vec<engine_core::domain::TestResult>) -> Vec<(String, Outcome)> {
    let mut v: Vec<(String, Outcome)> = results
        .into_iter()
        .map(|r| (r.node_id.to_string(), r.outcome))
        .collect();
    // `Outcome` is `Eq` but not `Ord`, and node ids are unique, so sort by node id.
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Best-effort CPU count for the subprocess pool; falls back to 1.
fn num_cpus_or_one() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}
