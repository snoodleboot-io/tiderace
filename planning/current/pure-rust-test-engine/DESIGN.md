# Design: Pure-Rust Python Test Engine

**Status:** Draft (in active development — `planning/current/`)
**Related:** [PRD.md](PRD.md) · [ADR.md](ADR.md)

## Overview

A new Python test framework whose engine is entirely Rust; Python is reduced to an execution
substrate (a tiny Rust-shipped shim) that only runs user test/fixture bodies. The engine owns
collection, the fixture graph, scope/lifecycle, scheduling, fork orchestration, assertion
introspection, the plugin host, selection, caching, and reporting.

It is architected as a **build system for tests**: content-addressed result caching + hermetic,
incremental execution + interpreter snapshotting so per-test isolation is free. Three pillars:
**wellspring/fork** ([ADR-E003](design/adr/ADR-E003-fork-snapshot-isolation.md)),
**content-addressed cache** ([ADR-E004](design/adr/ADR-E004-content-addressed-cache.md)),
**warm daemon** ([ADR-E007](design/adr/ADR-E007-warm-daemon.md)).

> **The detailed, diagram-heavy design lives under [`design/`](design/)** — one document per
> subsystem, each with its classifier (UML class) diagram plus the relevant
> sequence/state/activity/ERD/deployment diagrams. This file is the per-feature design summary
> required by the planning template; [`design/README.md`](design/README.md) is the full index.

### Detailed design index

| # | Doc | Subsystem | UML |
|---|-----|-----------|-----|
| 00 | [vision](design/00-vision.md) | Goals, non-goals, perf targets | — |
| 01 | [architecture](design/01-architecture.md) | C4, module map, master class diagram | C4, classifier |
| 02 | [domain-model](design/02-domain-model.md) | Core entities/vocabulary | classifier, state |
| 03 | [collection](design/03-collection.md) | Discovery / registration | classifier, sequence |
| 04 | [fixture-graph](design/04-fixture-graph.md) | Fixture DI + scope→snapshot | classifier, sequence, activity |
| 05 | execution-wellspring | Fork/snapshot execution | classifier, sequence, state |
| 06 | scheduler | Bin-pack + scope locality | classifier, activity |
| 07 | cache | Content-addressed cache | ERD, state, sequence |
| 08 | daemon | Warm test server | component, sequence, state, deployment |
| 09 | [assertions](design/09-assertions.md) | Lazy introspection | classifier, sequence |
| 10 | test-styles | pytest fn/class + unittest protocols | sequence ×3 |
| 11 | coverage-impact | `sys.monitoring` + impact | classifier, activity |
| 12 | plugin-host | Hook system | classifier |
| 13 | cross-cutting | Config, reporting, errors, hermeticity | classifier, ERD |
| — | [adr/](design/adr/) | Decision records (ADR-E001..E010) | — |

## Affected Modules

This is a near-total rebuild of the run path; today's [`../../../riptide/`](../../../riptide/)
single binary becomes a Cargo **workspace** ([ADR-E005](design/adr/ADR-E005-workspace-trait-seams.md)):

- **Port forward (evolve):** `collector.rs` (regex discovery → `collection/`), `hasher.rs`
  (SHA-256 → cache keys), `impact.rs` (impact analysis), `db.rs` (SQLite → cache index),
  `config.rs`, `watcher.rs` (→ daemon FS watch).
- **Replace:** `runner.rs` + `pool.rs` + `worker.py` (pytest drivers) → new `exec/` (wellspring +
  fork workers) and a tiny `py-shim/shim.py`.
- **New:** `fixtures/`, `scheduler/`, `cache/`, `coverage/`, `assertion/`, `hooks/`,
  `report/`, plus the `engine-daemon` crate.

Target layout (one type per file, snake_case filenames):

```text
crates/{engine-core, engine-cli, engine-daemon, py-shim}
```

## Data / Schema Changes

- SQLite evolves from the current 4 tables into a **cache index** (content-addressed) plus
  timing history and the per-test dependency graph. Full ERD in `design/07-cache.md` and
  `design/13-cross-cutting.md`.
- New on-disk **content store** for cached outcomes + captured output.
- Cache keys incorporate engine/python/platform versions to prevent cross-environment poisoning.

## Implementation Plan

**Design first, implement after sign-off.** Implementation will be split into phases, each
tracked as its own planning folder (mirroring `planning/completed/phase-*`). Anticipated order
(to be finalized in the "plan implementation phases" step):

1. **Spike / de-risk** — fork-from-warm-wellspring + tiny shim; benchmark vs pytest/xdist on a
   fixture-heavy suite (validates [ADR-E003](design/adr/ADR-E003-fork-snapshot-isolation.md)).
2. **Workspace + domain + collection** — engine-core skeleton, trait seams, `Collector`.
3. **Fixture graph + execution** — native fixtures, wellspring/fork, snapshot layers.
4. **Test styles** — pytest fn/class + unittest.TestCase protocols + assertion introspection.
5. **Coverage + cache** — `sys.monitoring`, content-addressed cache, impact.
6. **Scheduler + daemon** — locality scheduling, warm daemon + JSON-RPC + IDE.
7. **Compat + reporting + hardening** — pytest-compat layer, reporters, conformance suite.

## Testing

- Rust unit tests at every trait seam (mockable by design).
- Engine integration tests on real Python + real C-extension stacks (carry forward
  `tests/cli.rs` style end-to-end coverage).
- **Conformance suite:** run a basket of real OSS pytest/unittest projects unmodified; track
  pass-rate (PRD success metric).
- **Benchmarks:** extend `benchmarks/run_benchmarks.py` + `real_world.sh`; every performance
  claim is backed by a benchmark.

## Risks

- **Fork hazards** — fork+threads, non-fork-safe resources (DB/GPU handles), COW write
  amplification. Mitigations + fallback in
  [ADR-E003](design/adr/ADR-E003-fork-snapshot-isolation.md) / `design/05-execution-wellspring.md`.
- **Cache soundness** — impure tests must never be silently cached; conservative defaults +
  impurity detection ([ADR-E004](design/adr/ADR-E004-content-addressed-cache.md)).
- **Compat fidelity** — reimplementing pytest fixtures/marks faithfully is the bulk of the work;
  staged compat layer + conformance suite de-risk it.
- **Daemon state bugs** — stale-module invalidation + memory growth
  ([ADR-E007](design/adr/ADR-E007-warm-daemon.md)).
- **Reimplementation scope** — large; mitigated by riding stdlib for unittest and phasing.
