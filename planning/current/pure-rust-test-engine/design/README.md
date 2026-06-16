# Pure-Rust Python Test Engine — Detailed Design

> **Status:** 🟡 Design in progress (architect phase). No implementation yet.
> **Planning home:** [`planning/current/pure-rust-test-engine/`](../) — see [PRD](../PRD.md),
> [ADR](../ADR.md), [DESIGN](../DESIGN.md).
> **Branch:** `feat/pure-rust-test-engine` (off `main_v2`).

This folder is the **detailed, diagram-heavy design** for the feature — one document per
subsystem, each with its classifier (UML class) diagram plus the relevant
sequence/state/activity/ERD/deployment diagrams. The feature's PRD/ADR/DESIGN summary lives one
level up in the planning folder.

The Rust engine *is* the framework: it owns collection, the fixture graph, scope/lifecycle,
scheduling, assertion introspection, the plugin host, selection, caching, and reporting. Python
is an **execution substrate** that only runs user test/fixture bodies — no pytest underneath.

> ⚠️ Distinct from the project-level [`../../../../docs/design/`](../../../../docs/design/) docs,
> which describe the *old* "tiderace orchestrates pytest" direction (superseded here).

## The thesis

Stop building a test *runner*. Build a **build system for tests**: content-addressed result
caching, hermetic + incremental execution, and interpreter snapshotting so per-test isolation
is free. Three pillars:

1. **Wellspring / fork** — import once; `fork()` per test for free isolation; fixture scopes become
   layered memory snapshots.
2. **Content-addressed cache** — a test outcome is a pure function of its input closure.
3. **Daemon** — an always-warm test server giving sub-100ms edit-to-result.

## Reading order

| # | Doc | What it covers | Status |
|---|-----|----------------|--------|
| 00 | [vision](00-vision.md) | Goals, non-goals, performance targets, the reframe | ✅ draft |
| 01 | [architecture](01-architecture.md) | C4, module map, master class diagram | ✅ draft |
| 02 | [domain-model](02-domain-model.md) | Classifier UML for core domain entities | ✅ draft |
| 03 | [collection](03-collection.md) | Test discovery / registration | ✅ draft |
| 04 | [fixture-graph](04-fixture-graph.md) | Fixture dependency injection + resolution | ✅ draft |
| 05 | [execution-wellspring](05-execution-wellspring.md) | Fork/snapshot execution model | ✅ draft |
| 06 | [scheduler](06-scheduler.md) | Duration-aware bin-packing + scope locality | ✅ draft |
| 07 | [cache](07-cache.md) | Content-addressed result cache | ✅ draft |
| 08 | [daemon](08-daemon.md) | Always-warm test server | ✅ draft |
| 09 | [assertions](09-assertions.md) | Lazy assertion introspection | ✅ draft |
| 10 | [test-styles](10-test-styles.md) | pytest function/class + unittest.TestCase protocols | ✅ draft |
| 11 | [coverage-impact](11-coverage-impact.md) | `sys.monitoring` coverage + impact analysis | ✅ draft |
| 12 | [plugin-host](12-plugin-host.md) | Hook system | ✅ draft |
| 13 | [cross-cutting](13-cross-cutting.md) | Config, reporting, error model, hermeticity/security | ✅ draft |
| — | [adr/](adr/) | Architecture Decision Records (ADR-E001..E010) | ✅ draft |

## UML coverage map

| Diagram type | Where |
|---|---|
| C4 context / container / component | 01, 08 (deployment) |
| Class / classifier (traits, structs, relationships) | 01 + 02 + every subsystem doc |
| Sequence | 03, 04, 05, 07, 08, 09, 10 |
| State machine | 02 (outcome), 05 (worker/test lifecycle), 07 (cache entry), 08 (daemon) |
| Activity | 04, 06 (scheduling), 11 (impact analysis) |
| Entity-relationship (data model) | 07 (cache store), 13 |
| Deployment | 08 (daemon + workers + remote cache) |

## Process

- **Design first, implement later** — implementation phases are planned only *after* sign-off.
- All diagrams are **Mermaid** (rendered by MkDocs + GitHub).
