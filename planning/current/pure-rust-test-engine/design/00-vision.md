# 00 — Vision, Goals & Performance Targets

> **Status:** ✅ draft for discussion

## 1. Problem statement

pytest's performance ceiling comes from one assumption it never questions: *a test run means
"start Python, import the world, execute every selected test top to bottom."* Every pytest
accelerator (xdist, testmon, cache plugins) patches that assumption rather than rejecting it.
If we reject the assumption itself, speedups stop being 2–3× and become 10–1000×, because the
fastest test is the one we **never import, never start, and never run**.

## 2. The reframe: a build system for tests

We treat a test result the way Bazel/Nix treat a build artifact:

- **Content-addressed** — outcome is a deterministic function of the test's transitive input
  closure (test bytecode + executed source + fixture closure + declared environment).
- **Incremental** — only out-of-date tests run; everything else is served from cache.
- **Hermetic** — execution is sandboxed enough that the true input closure is *known*, which
  is what makes caching sound.
- **Snapshotted** — interpreter + fixture state is captured once and `fork()`ed per test, so
  per-test isolation costs ~nothing.

## 3. What "pure Rust, no pytest" means precisely

Python test bodies *must* run in a CPython interpreter — that is non-negotiable. "Pure Rust"
therefore means:

> The **framework engine** is Rust. Python is an **execution substrate** that runs only the
> user's test/fixture bodies. No pytest, and not even the stdlib `unittest` *runner* — though
> we *do* drive the stdlib `unittest.TestCase` per-method contract directly (that's stdlib, not
> a third-party framework; see [10-test-styles](10-test-styles.md)).

Rust owns: collection, fixture graph, scope/lifecycle, scheduling, fork orchestration, cache,
assertion-introspection orchestration, plugin host, selection, reporting.
Python (a tiny Rust-shipped shim) owns: import a module, call a callable, capture an outcome.

## 4. Goals

| # | Goal | Why it matters |
|---|------|----------------|
| G1 | **Run existing pytest + unittest suites** with minimal/zero edits | Adoption gate — nobody rewrites 10k tests |
| G2 | **Free per-test isolation** via fork-from-snapshot | Kills order-dependent flakiness *and* startup cost at once |
| G3 | **Content-addressed, shareable result cache** | Inner-loop and CI runs approach O(changed tests) |
| G4 | **Sub-100ms edit→result** via a warm daemon | Tests feel like a type-checker, not a chore |
| G5 | **Rich assertion failure output** ≥ pytest quality | Output quality is non-negotiable for adoption |
| G6 | **Performance-first architecture** | The entire reason this project exists |
| G7 | **Adheres to project conventions** (SOLID, one-class-per-file, trait-based DI) | Maintainability + reviewability |

## 5. Non-goals (initial scope)

- **Windows-first.** `fork()` is the core mechanism; Linux + macOS first. Windows gets a
  process-pool fallback later (or CRIU-style checkpointing). (See [ADR](adr/) — substrate.)
- **100% pytest plugin compatibility on day one.** A pytest-compat shim is a deliberate,
  staged effort, not a launch blocker.
- **Replacing the Python interpreter.** We run CPython; we don't reimplement it.
- **A new assertion DSL.** Plain `assert` (with introspection) + unittest `self.assert*` is the
  surface. A native API is additive, later.

## 6. Performance targets

Targets are **directional** and will be validated by the de-risking spike before we commit.
Baseline = `pytest` / `pytest-xdist` on the same suite/hardware.

| Scenario | Target vs pytest | Primary lever |
|---|---|---|
| Inner edit loop (1 file changed, warm daemon) | **100–1000×** | cache + impact + warm daemon (P2,P3) |
| CI, mostly-unchanged tree (shared remote cache) | **≫10× (→∞ on full cache hit)** | content-addressed remote cache (P2) |
| Cold full run, large suite | **5–50×** | import-once wellspring + fork parallelism (P1) |
| Fixture-heavy suite (expensive session/class setup) | **10–100×** | fork from post-fixture snapshot (P1) |
| State-leak / order-dependent flakiness | **eliminated** | every test forks a pristine interpreter (P1) |

**Honest floor:** a truly cold, all-pure, all-changed suite of trivial tests is bounded by raw
CPython import+exec — there we beat xdist on startup but cannot beat physics. We will *say so*
in docs rather than over-claim.

## 7. Guiding principles

1. **Don't run what you can skip** (cache → impact → run, in that order of preference).
2. **Pay setup once** (snapshot it; fork from it).
3. **Isolation is a feature, made free** (fork gives pristine state per test).
4. **Measure, don't assert.** Every perf claim is backed by a benchmark in `benchmarks/`.
5. **Substrate is replaceable.** `fork` worker, `thread` worker (free-threaded CPython),
   `remote` worker must be interchangeable behind one trait.

## 8. Open questions (to resolve during design)

- O1: Execution substrate detail — subprocess+shim vs PyO3-embedded worker binary? (→ ADR)
- O2: Cache soundness — how aggressively do we sandbox to detect impurity? (→ 07-cache)
- O3: pytest-compat surface — how much of the fixture/mark API do we replicate vs adapt? (→ 10)
- O4: Coverage mechanism — `sys.monitoring` (3.12+) only, or `settrace` fallback for ≤3.11?
- O5: Cross-platform fallback when `fork` is unavailable. (→ 05-execution)
