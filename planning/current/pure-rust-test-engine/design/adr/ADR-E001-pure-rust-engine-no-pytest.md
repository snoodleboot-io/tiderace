# ADR-E001 — Pure-Rust engine: own the framework, no pytest underneath

**Status:** ✅ Accepted (design)

## Context

The prior direction (`docs/design/decisions.md`, ADR-001..011) was a Rust binary that
*orchestrates pytest*: pytest still owned fixtures, assertions, marks, parametrization, and
collection-by-import. That caps our performance ceiling and our control surface at whatever
pytest does — we cannot fork-per-test cheaply, cannot content-address results soundly, and
cannot avoid pytest's import-time and hook overheads.

The user's directive is explicit: build a **new framework** with a complete Rust backend that
does **not** run pytest underneath — the Rust side *is* the framework.

Hard constraint: Python test/fixture bodies must execute in a CPython interpreter. We cannot
run user Python in Rust.

## Decision

**The framework engine is implemented in Rust. Python is reduced to an execution substrate**
that runs only user test/fixture bodies. Specifically, Rust owns:

- collection / registration
- the fixture dependency graph, scopes, and lifecycle
- scheduling and fork orchestration
- assertion-introspection orchestration
- the plugin/hook host
- selection, caching, and reporting

We **reimplement** pytest's semantics that matter (fixtures + DI, marks, parametrization, plain
`assert` introspection) natively in Rust + the shim.

For **unittest** tests we do **not** reimplement the contract — we drive the stdlib
`unittest.TestCase.run()` method **at method granularity** from the shim. That is stdlib, not a
third-party framework or a third-party *runner* (we replace `TestSuite`/`TestRunner`
orchestration with our scheduler + wellspring). See [10-test-styles](../10-test-styles.md).

Adoption is served by a **staged pytest-compatibility layer** (understand `@pytest.fixture`,
`@pytest.mark.*`, `conftest.py`, plain `assert`) — staged, not a launch blocker.

## Consequences

- ➕ Full control over performance, isolation (fork), and caching — the whole point.
- ➕ Extensibility via our own trait-based hook host, not bounded by pytest.
- ➖ Significant reimplementation surface (fixtures, marks, parametrize, assert introspection).
  This is the bulk of the engine work and is sequenced explicitly in the implementation phases.
- ➖ Adoption hinges on compat fidelity; we mitigate with a conformance suite run against real
  OSS projects (extend `benchmarks/real_world.sh`).
- ➕ unittest support is comparatively cheap (ride stdlib), de-risking a whole test style.

## Alternatives considered

1. **Orchestrate pytest** (old design) — rejected: performance/control ceiling; can't fork or
   content-address.
2. **Ship as a pytest plugin** — rejected: a plugin can't own scheduling, the fork model, or
   the cache; still pays pytest's startup/hook costs.
3. **Reimplement unittest semantics too** — rejected as wasteful: stdlib already exposes the
   per-method contract; driving it is less code and is bug-compatible with users' expectations.

## Revisit trigger

If the cost of reimplementing pytest fixture/mark fidelity proves larger than the pytest-compat
shim's value (e.g., compat-suite pass rate stalls), reconsider a hybrid where a vendored pytest
runs *inside* a fork worker purely for compat-mode suites, while native mode stays pytest-free.
