# PRD: Pure-Rust Python Test Engine

**Status:** Draft
**Owner:** John Aven
**Last updated:** 2026-06-15

## Problem

`tiderace` today is a Rust *orchestrator* that runs pytest underneath (see
[`planning/completed/`](../../completed/) phases). pytest still owns fixtures, assertions,
marks, parametrization, and collection-by-import — which caps our performance ceiling and our
control surface at whatever pytest does. We cannot fork-per-test cheaply, cannot
content-address results soundly, and cannot escape pytest's import-time and hook overheads.

We want a **new** Python test framework with a complete Rust backend that does **not** run
pytest underneath — the Rust engine *is* the framework. The goal is to **highly outdo pytest**,
not by 2–3× but by reframing what a test run is.

**Why now:** the orchestrator approach has hit its design ceiling; CPython 3.12+
(`sys.monitoring`, PEP 669) and modern snapshotting techniques make a far more aggressive
architecture viable.

## Goals

- **G1** — Run existing **pytest** (function + class) and **unittest.TestCase** suites with
  minimal/zero edits (adoption gate).
- **G2** — **Free per-test isolation** via fork-from-snapshot, eliminating order-dependent
  flakiness while removing per-test startup cost.
- **G3** — **Content-addressed, shareable result cache** (Bazel/Nix-style) so inner-loop and CI
  runs approach O(changed tests).
- **G4** — **Sub-100ms edit→result** via a warm daemon.
- **G5** — Assertion failure output **≥ pytest quality**.
- **G6** — **Performance-first** architecture (the reason this exists).
- **G7** — Adheres to project conventions (SOLID, one-class-per-file, trait-based DI).

## Non-Goals

- **Windows-first.** `fork()` is the core mechanism → Linux + macOS lead; Windows gets a
  process-pool fallback later.
- **100% pytest plugin compatibility on day one** — staged compat layer, not a launch blocker.
- **Replacing the Python interpreter** — we run CPython, we don't reimplement it.
- **A new assertion DSL** — plain `assert` (introspected) + unittest `self.assert*` is the
  surface; a native API is additive, later.

## Scope

The smallest shippable slice that proves the thesis, then expands. Full detail in
[DESIGN.md](DESIGN.md); the design breaks into subsystems under [`design/`](design/):
domain model, collection, fixtures, wellspring/fork execution, scheduler, cache, daemon,
assertions, test styles, coverage/impact, plugin host, cross-cutting. Implementation is phased
**after** design sign-off (phases will be tracked as their own planning folders).

## Success Metrics

Validated by `benchmarks/` (extend `run_benchmarks.py` + `real_world.sh`) vs
pytest / pytest-xdist on identical suites/hardware:

| Scenario | Target vs pytest |
|---|---|
| Inner edit loop (1 file changed, warm daemon) | 100–1000× |
| CI, mostly-unchanged tree (shared remote cache) | ≫10× (→∞ on full cache hit) |
| Cold full run, large suite | 5–50× |
| Fixture-heavy suite | 10–100× |
| State-leak / order-dependent flakiness | eliminated |

Plus: a **compatibility pass-rate** metric — % of tests in a basket of real OSS suites that run
unmodified.

## Open Questions

- **O1** — Substrate detail: subprocess+shim (chosen) vs PyO3-embedded (deferred). See
  [ADR.md](ADR.md) / [design/adr/ADR-E002](design/adr/ADR-E002-execution-substrate.md).
- **O2** — Cache soundness: how aggressively do we sandbox to detect impurity?
  [design/adr/ADR-E004](design/adr/ADR-E004-content-addressed-cache.md).
- **O3** — pytest-compat surface: how much of the fixture/mark API do we replicate vs adapt?
- **O4** — Coverage: `sys.monitoring` (3.12+) only, or `settrace` fallback for ≤3.11?
  [design/adr/ADR-E006](design/adr/ADR-E006-coverage-sys-monitoring.md).
- **O5** — Cross-platform fallback when `fork` is unavailable.
  [design/adr/ADR-E008](design/adr/ADR-E008-cross-platform.md).
