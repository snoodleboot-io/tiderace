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
| [E011](ADR-E011-shim-transport-seam.md) | `ShimTransport` seam (pipe / in-process backends) | ✅ Accepted (design) | 05 |
| [E012](ADR-E012-native-type-driven-authoring.md) | Native type-driven authoring (`@provides`/`@cases`/`@uses`) | ✅ Accepted (design) | 04, 10 |
| [E013](ADR-E013-inprocess-isolation.md) | In-process backend isolation: fork-from-embedded (②) | ✅ Accepted (design) | 05, 11 |
| [E014](ADR-E014-no-fork-restore-ladder.md) | No-fork + restore isolation ladder (default execution path) | ✅ **Implemented + measured** | 05, 04, 06 |
| [E015](ADR-E015-subinterp-tier.md) | Conditional sub-interpreter tier (`SubInterpWorker`) for Windows parallelism | ✅ Accepted (design) · spiked, build phased (TID-2) | 05, 08 |

**Status legend:** "Accepted (design)" = agreed for the design phase; revisitable when the
de-risking spike produces real numbers. **E014 is implemented + measured** — the no-fork ladder is the
engine's default execution path today; several other ADRs (E001–E013) are now built as well.

## ADR format

Each record uses: **Context → Decision → Consequences → Alternatives considered**, plus a
short **Revisit trigger** noting what evidence would reopen it.
