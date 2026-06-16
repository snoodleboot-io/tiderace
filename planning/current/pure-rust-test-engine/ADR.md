# ADR: Own the framework — a pure-Rust test engine, no pytest underneath

**Date:** 2026-06-15
**Status:** Accepted

This is the **headline** decision for the feature. The detailed, per-subsystem decision records
live in [`design/adr/`](design/adr/) as the **ADR-E0xx** series (E = engine) and are summarized
at the bottom.

## Context

`tiderace` orchestrates pytest, so pytest owns fixtures, assertions, marks, parametrization, and
collection-by-import. That caps performance and control. Python test bodies must still run in
CPython — we cannot run user Python in Rust. We want to reframe a "test run" to highly outdo
pytest (see [PRD.md](PRD.md)).

## Decision

**The framework engine is Rust; Python is demoted to an execution substrate** that only runs
user test/fixture bodies. Rust owns collection, the fixture graph, scope/lifecycle, scheduling,
fork orchestration, assertion-introspection, the plugin host, selection, caching, and reporting.

Three performance pillars:
1. **Wellspring/fork** — import once, `fork()` per test for free isolation; fixture scopes become
   layered memory snapshots.
2. **Content-addressed cache** — a test outcome is a pure function of its input closure; skip
   execution on a hit; share across machines/CI.
3. **Warm daemon** — sub-100ms edit→result.

unittest is supported by driving the **stdlib** `unittest.TestCase.run()` at method granularity
(stdlib, not pytest, not a third-party runner). pytest semantics are reimplemented natively.

## Alternatives Considered

- **Option A — Keep orchestrating pytest** (status quo): lowest effort, but the performance and
  control ceiling is exactly what we're trying to break.
- **Option B — Ship as a pytest plugin**: still pays pytest startup/hook costs; can't own the
  fork model, scheduler, or cache.
- **Option C — PyO3-embedded substrate**: tighter control, but libpython ABI/link matrix pain
  and delicate fork-after-embed; deferred as a future optimization, not the baseline.
- **Option D — Subinterpreters**: rejected — C-extension safety (old ADR-010 still holds).

## Rationale

Owning the framework is the only way to get free fork-isolation, sound content-addressed
caching, and daemon warmth — the levers that turn 2–3× into 100–1000×. Subprocess+shim keeps
full C-extension compatibility and avoids the libpython link matrix while still enabling the
wellspring/fork model on top.

## Consequences

- We take on a real reimplementation surface (fixtures, marks, parametrize, assert
  introspection). unittest is comparatively cheap (ride stdlib).
- Adoption hinges on compatibility fidelity → a staged pytest-compat layer + a conformance
  suite against real OSS projects.
- Linux/macOS lead; Windows is a fallback behind the `Worker` trait.
- Everything is built on swappable trait seams (`Worker`, `Cache`, `Collector`, `Scheduler`,
  `CoverageCollector`, `Reporter`) so fork→free-threaded→remote evolve without rewrites.

---

## Detailed decision records (design/adr/ — ADR-E series)

| ADR | Decision |
|-----|----------|
| [E001](design/adr/ADR-E001-pure-rust-engine-no-pytest.md) | Pure-Rust engine — own the framework, no pytest |
| [E002](design/adr/ADR-E002-execution-substrate.md) | Substrate: subprocess + Python shim + binary IPC (not PyO3/subinterpreters) |
| [E003](design/adr/ADR-E003-fork-snapshot-isolation.md) | Fork-from-snapshot isolation; fixture scopes = memory snapshots |
| [E004](design/adr/ADR-E004-content-addressed-cache.md) | Content-addressed result cache |
| [E005](design/adr/ADR-E005-workspace-trait-seams.md) | Cargo workspace + trait DI seams |
| [E006](design/adr/ADR-E006-coverage-sys-monitoring.md) | Coverage via `sys.monitoring`, settrace fallback |
| [E007](design/adr/ADR-E007-warm-daemon.md) | Warm daemon as primary host |
| [E008](design/adr/ADR-E008-cross-platform.md) | Fork-first; cross-platform fallback behind `Worker` |
| [E009](design/adr/ADR-E009-lazy-assertion-introspection.md) | Lazy assertion introspection (not import-time rewrite) |
| [E010](design/adr/ADR-E010-locality-scheduler.md) | Duration-aware, scope-locality scheduler |

> Per the [planning README](../../README.md) and the ADR template, the *cross-cutting*
> project ADR log is [`docs/design/decisions.md`](../../../docs/design/decisions.md) (currently
> the old orchestrator series ADR-001..011). These ADR-E records are kept with the feature while
> it is in `current/`; on ship, the cross-cutting subset should be promoted into the project log.
