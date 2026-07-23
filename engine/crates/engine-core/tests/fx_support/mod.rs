//! Shared helpers for the Phase 3 fixtures + watermarks acceptance suite.
//!
//! This module is `mod`-included by `fixtures_acceptance.rs`. It holds:
//!   * venv / corpus / shim path discovery (mirroring `differential.rs`),
//!   * a live runner that drives stock pytest over `fx_corpus` with an isolated
//!     probe dir and parses the `fx_probe` artifacts (`events.log`, `counts.json`),
//!   * small constructors for building `Fixture` / `FixtureGraph` inputs that mirror
//!     the corpus topology, so the pure-Rust scenarios (cycle, scope-widen,
//!     parametrization, override) can assert without Python.
//!
//! **No mocks at the python/sqlite boundary** (BINDING, test-mocking-rules): the
//! live scenarios shell out to the real `.tiderace-fx-venv` interpreter running real
//! pytest + numpy + sqlite, exactly as the contract's §8 boundaries demand.

#![allow(dead_code)] // each integration-test binary uses a different subset of these helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

/// Serializes the live, corpus-launching scenarios. They drive real wellsprings and one of them
/// (`run_engine_scope_counts`) sets the **process-global** `FX_CORPUS_PROBE_DIR`; cargo runs tests
/// in parallel threads, so without this lock a concurrently-launched wellspring would inherit the
/// leaked env var and double-count the shared probe. Poison-tolerant: a panicking live test must not
/// cascade-poison the others.
static LIVE_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the live-scenario serialization guard (held for the duration of an engine/oracle run).
pub fn live_guard() -> MutexGuard<'static, ()> {
    LIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

use engine_core::domain::{NodeId, Scope, ScopePath};
use engine_core::fixtures::{Fixture, ParamValue};

/// The repository root (three levels up from this crate's manifest dir).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// The path to the Python shim the wellspring drives.
pub fn shim_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../py-shim/shim.py")
        .canonicalize()
        .expect("shim path")
}

/// The fixture-heavy conformance corpus root (511 pytest tests).
pub fn fx_corpus_root() -> PathBuf {
    repo_root().join("benchmarks/fixtures/fx_corpus")
}

/// The Phase-3 venv interpreter, or `None` if Lane 0 has not provisioned it yet.
///
/// Live scenarios call this and skip cleanly (returning early) when it is absent —
/// the same venv-presence guard `differential.rs` uses — but they DO run when the
/// venv is present.
pub fn fx_venv_python() -> Option<PathBuf> {
    let p = repo_root().join(".tiderace-fx-venv/bin/python");
    p.exists().then_some(p)
}

/// One ordered probe event: a setup or teardown of a named fixture body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeEvent {
    /// A fixture body's setup half ran (`SETUP   <name>`).
    Setup(String),
    /// A fixture body's teardown half ran (`TEARDOWN <name>`).
    Teardown(String),
}

impl ProbeEvent {
    /// The fixture name this event refers to.
    pub fn name(&self) -> &str {
        match self {
            ProbeEvent::Setup(n) | ProbeEvent::Teardown(n) => n,
        }
    }

    /// `true` if this is a setup event.
    pub fn is_setup(&self) -> bool {
        matches!(self, ProbeEvent::Setup(_))
    }
}

/// The parsed `fx_probe` artifacts after a run: the ordered event log and the
/// per-fixture body run-counts. These are the differential oracle for setup/teardown
/// ordering (scenario 1) and the 1x-not-Nx scope-count claim (scenario 2).
#[derive(Debug, Clone, Default)]
pub struct ProbeReport {
    /// Every setup/teardown line, in execution order.
    pub events: Vec<ProbeEvent>,
    /// `fixture_name -> times its body ran`.
    pub counts: BTreeMap<String, u64>,
}

impl ProbeReport {
    /// Parse `events.log` + `counts.json` written into `probe_dir`.
    pub fn read_from(probe_dir: &Path) -> Self {
        let events = std::fs::read_to_string(probe_dir.join("events.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(parse_event_line)
            .collect();
        let counts = std::fs::read_to_string(probe_dir.join("counts.json"))
            .ok()
            .map(|s| parse_counts_json(&s))
            .unwrap_or_default();
        Self { events, counts }
    }

    /// The setup events, in order.
    pub fn setups(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|e| e.is_setup())
            .map(|e| e.name().to_string())
            .collect()
    }

    /// The teardown events, in order.
    pub fn teardowns(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|e| !e.is_setup())
            .map(|e| e.name().to_string())
            .collect()
    }
}

fn parse_event_line(line: &str) -> Option<ProbeEvent> {
    // `fx_probe` writes "SETUP   <name>" and "TEARDOWN <name>" (variable spaces).
    if let Some(rest) = line.strip_prefix("SETUP") {
        return Some(ProbeEvent::Setup(rest.trim().to_string()));
    }
    if let Some(rest) = line.strip_prefix("TEARDOWN") {
        return Some(ProbeEvent::Teardown(rest.trim().to_string()));
    }
    None
}

/// A deliberately tiny JSON object parser for `{"name": <int>, ...}` — the exact,
/// stable shape `fx_probe.bump_count` writes (`json.dumps(sort_keys=True)`). Kept
/// dependency-free so the acceptance crate needs no serde_json.
fn parse_counts_json(s: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let body = s.trim().trim_start_matches('{').trim_end_matches('}');
    for pair in body.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((k, v)) = pair.split_once(':') else {
            continue;
        };
        let key = k.trim().trim_matches('"').to_string();
        if let Ok(n) = v.trim().parse::<u64>() {
            out.insert(key, n);
        }
    }
    out
}

/// Run stock pytest over the whole `fx_corpus` against an isolated probe dir and
/// return the parsed probe artifacts. This is the **pytest oracle** for the
/// differential scenarios — the engine's observed ordering/counts must match it.
///
/// The probe dir is created fresh and `FX_CORPUS_PROBE_DIR` is exported so the
/// corpus's `probe_dir` session fixture honors it (see `fx_corpus/conftest.py`).
pub fn run_pytest_oracle(python: &Path) -> ProbeReport {
    let probe_dir = std::env::temp_dir().join(format!(
        "fx_oracle_probe_{}_{}",
        std::process::id(),
        nonce()
    ));
    let _ = std::fs::remove_dir_all(&probe_dir);
    std::fs::create_dir_all(&probe_dir).expect("mkdir probe dir");

    let status = Command::new(python)
        .args(["-m", "pytest", "-q", "."])
        .current_dir(fx_corpus_root())
        .env("FX_CORPUS_PROBE_DIR", &probe_dir)
        .status()
        .expect("run pytest oracle");
    assert!(status.success(), "pytest oracle must run the corpus green");

    let report = ProbeReport::read_from(&probe_dir);
    let _ = std::fs::remove_dir_all(&probe_dir);
    report
}

/// A cheap per-call nonce so concurrent test fns get distinct probe dirs.
fn nonce() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// --------------------------------------------------------------------------
// Pure-Rust fixture-graph input builders (mirror the corpus topology).
// --------------------------------------------------------------------------

/// A `ScopePath` for a module (no class).
pub fn module_path(module: &str) -> ScopePath {
    ScopePath::module(module)
}

/// Build a plain (return-style, fork-safe) `Fixture` named `name` at `scope`,
/// declared in `module`, depending on `deps` (by name).
pub fn fx(name: &str, scope: Scope, module: &str, deps: &[&str]) -> Fixture {
    Fixture::new(
        NodeId::new(format!("{module}::{name}")),
        name,
        scope,
        module_path(module),
    )
    .with_deps(deps.iter().map(|s| s.to_string()).collect())
}

/// The corpus's scope topology as a `Fixture` set, mirroring `fx_corpus`:
/// `session_db (Session) -> pkg_resource (Package) -> module_fix (Module)
///  -> class_fix (Class) -> func_fix (Function)`, plus a session autouse fixture.
/// Built so the pure-Rust resolver scenarios can assert layering/closure without
/// Python (scenarios that need real fork drive the live corpus instead).
///
/// Declaring locations use the **directory-ancestor** model the override table keys
/// off (CONTRACT §2.7): the session conftest's fixtures are declared at the root
/// (`""`, a prefix of every module), the package conftest's at `"tests"`, and the
/// module fixtures at their own module path — so all are visible from a test in
/// `tests/test_scopes.py` via longest-prefix resolution.
pub fn corpus_scope_fixtures() -> Vec<Fixture> {
    vec![
        fx("session_db", Scope::Session, "", &[]),
        fx("session_autouse", Scope::Session, "", &[]).autouse(),
        fx("pkg_resource", Scope::Package, "tests", &["session_db"]),
        fx(
            "module_fix",
            Scope::Module,
            "tests/test_scopes.py",
            &["pkg_resource"],
        )
        .yielding(),
        fx(
            "class_fix",
            Scope::Class,
            "tests/test_scopes.py",
            &["module_fix"],
        )
        .yielding(),
        fx(
            "func_fix",
            Scope::Function,
            "tests/test_scopes.py",
            &["class_fix"],
        )
        .yielding(),
    ]
}

/// The three corpus param ids (`['a','b','c']`) as `ParamValue`s — mirrors
/// `fx_corpus/tests/test_param.py`.
pub fn corpus_param_values() -> Vec<ParamValue> {
    vec![
        ParamValue::new("a", 0),
        ParamValue::new("b", 1),
        ParamValue::new("c", 2),
    ]
}
