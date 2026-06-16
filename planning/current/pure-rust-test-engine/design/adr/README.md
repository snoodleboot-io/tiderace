# Architecture Decision Records — Pure-Rust Test Engine

These ADRs record the **foundational** decisions for the new engine. They are a fresh series
(`ADR-E0xx`, "E" = engine) deliberately separate from the parent `docs/design/decisions.md`
(`ADR-001..011`), which belongs to the superseded *orchestrate-pytest* design.

Where an old ADR is relevant, it is cited (e.g. old **ADR-010** rejecting subinterpreters
informs **ADR-E002**).

| ADR | Title | Status | Gates |
|-----|-------|--------|-------|
| [E001](ADR-E001-pure-rust-engine-no-pytest.md) | Pure-Rust engine — own the framework, no pytest underneath | ✅ Accepted (design) | everything |
| [E002](ADR-E002-execution-substrate.md) | Execution substrate: subprocess + Python shim (not PyO3/subinterpreters) | ✅ Accepted (design) | 05, 08, 10 |
| [E003](ADR-E003-fork-snapshot-isolation.md) | Fork-from-snapshot isolation; fixture scopes as memory snapshots | ✅ Accepted (design) | 04, 05, 06 |
| [E004](ADR-E004-content-addressed-cache.md) | Content-addressed result cache (build-system-for-tests) | ✅ Accepted (design) | 07, 11 |
| [E005](ADR-E005-workspace-trait-seams.md) | Cargo workspace + trait-based DI seams | ✅ Accepted (design) | 01, all |
| [E006](ADR-E006-coverage-sys-monitoring.md) | Coverage via `sys.monitoring` (PEP 669), settrace fallback | ✅ Accepted (design) | 07, 11 |
| [E007](ADR-E007-warm-daemon.md) | Warm daemon as the primary execution host | ✅ Accepted (design) | 08 |
| [E008](ADR-E008-cross-platform.md) | Fork-first; cross-platform fallback behind `Worker` trait | ✅ Accepted (design) | 05 |
| [E009](ADR-E009-lazy-assertion-introspection.md) | Lazy assertion introspection (not import-time rewrite) | ✅ Accepted (design) | 09 |
| [E010](ADR-E010-locality-scheduler.md) | Duration-aware, scope-locality scheduler | ✅ Accepted (design) | 06 |

**Status legend:** "Accepted (design)" = agreed for the design phase; revisitable when the
de-risking spike produces real numbers. Nothing here is implemented yet.

## ADR format

Each record uses: **Context → Decision → Consequences → Alternatives considered**, plus a
short **Revisit trigger** noting what evidence would reopen it.
