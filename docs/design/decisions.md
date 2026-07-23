# Design Decisions (ADRs)

The pure-Rust engine has its own ADR series — `ADR-E0xx` ("E" = engine) — recorded in
[`planning/current/pure-rust-test-engine/design/adr/`](https://github.com/snoodleboot-io/tiderace/tree/main/planning/current/pure-rust-test-engine/design/adr/).
It is deliberately separate from the old `ADR-001..011` series, which belonged to the **superseded**
orchestrate-pytest design (Rust driving `pytest` + SQLite + coverage.py). Those are retired.

Each record follows **Context → Decision → Consequences → Alternatives considered**, plus a
**Revisit trigger**. `ADR-E014` (the default execution path) and `ADR-E015` (the sub-interpreter tier)
are implemented and measured; E001–E013 are accepted for the design phase and several are now built as
well.

| ADR | Decision | One-line summary |
|---|---|---|
| [E001](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E001-pure-rust-engine-no-pytest.md) | Pure-Rust engine, no pytest at runtime | tiderace owns the framework; pytest is not a runtime dependency. |
| [E002](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E002-execution-substrate.md) | Execution substrate: subprocess + Python shim | A thin shim over a subprocess, not PyO3 embedding or subinterpreters. |
| [**E003**](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E003-fork-snapshot-isolation.md) | **Fork-from-warm-wellspring snapshot isolation** | Import the project once; `fork()` per test gives each a pristine COW view — fixture scopes as memory snapshots. |
| [**E004**](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E004-content-addressed-cache.md) | **Content-addressed result cache** | A test's outcome is a pure function of its input closure (`CacheKey`); a `TieredCache` makes a CI result reusable on any machine. |
| [E005](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E005-workspace-trait-seams.md) | Cargo workspace + trait-based DI seams | Each boundary (collector, scheduler, cache, transport, reporter) is a trait, so it is testable in isolation. |
| [**E006**](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E006-coverage-sys-monitoring.md) | **Coverage via `sys.monitoring` (PEP 669)** | Per-test footprints captured in-process with `sys.monitoring`, not coverage.py; `settrace` fallback for older CPython. |
| [E007](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E007-warm-daemon.md) | Warm daemon as the primary host | Keep CPython warm so the project is imported once, not per run. |
| [E008](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E008-cross-platform.md) | Fork-first cross-platform strategy | Fork on Unix; the cross-platform fallback lives behind the `Worker` trait. |
| [E009](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E009-lazy-assertion-introspection.md) | Lazy assertion introspection | Introspect a failing `assert` on demand, not via import-time rewrite. |
| [E010](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E010-locality-scheduler.md) | Locality scheduler (LPT + scope locality) | Keep a module's tests on one worker while LPT-balancing load across workers. |
| [E011](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E011-shim-transport-seam.md) | `ShimTransport` seam | One swappable boundary: `PipeTransport` (JSON frames) in production, `InProcessTransport` (FFI) experimental. |
| [E012](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E012-native-type-driven-authoring.md) | Native type-driven authoring | `@provides`/`@cases`/`@uses` resolve fixtures by type, so a suite can drop pytest entirely. |
| [E013](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E013-inprocess-isolation.md) | In-process / FFI isolation (②) | Embedded CPython + fork-from-embedded over the transport seam — a research path. |
| [**E014**](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E014-no-fork-restore-ladder.md) | **No-fork + restore isolation ladder** *(implemented + measured)* | The default execution path: bare no-fork (pure) / no-fork + restore (mutating) / fork (opaque), sound by construction. |
| [**E015**](https://github.com/snoodleboot-io/tiderace/blob/main/planning/current/pure-rust-test-engine/design/adr/ADR-E015-subinterp-tier.md) | **Conditional sub-interpreter tier** *(implemented + measured)* | Detect which modules are sub-interpreter-safe, then run the safe subset across a parallel sub-interpreter pool (per-interpreter GIL, PEP 684) — no fork. The one parallel path **Windows** has. Opt-in. |

## The four that matter most

- **E003 — fork from a warm wellspring.** Import the project once; isolate by forking COW children
  rather than re-importing per test. This is the substrate the ladder optimizes.
- **E004 — content-addressed cache.** Outcomes are content-addressed, turning the test suite into a
  *build system*: an unchanged test is free, and a result computed in CI is reusable anywhere with
  the same inputs. A `purity` gate keeps nondeterministic tests out of the cache.
- **E006 — `sys.monitoring` coverage.** Per-test executed-source footprints captured on the same
  in-process run — no coverage.py, no separate pass — feeding both impact analysis and the cache key.
- **E014 — the no-fork ladder.** The engine's default today: most tests skip the fork entirely and
  run in-process (with snapshot/restore where they mutate state), falling back to fork only for opaque
  modules. Sound by construction, so it needs no flag and no learning pass.

For the full narrative with diagrams, see
[`ARCHITECTURE.md`](https://github.com/snoodleboot-io/tiderace/blob/main/ARCHITECTURE.md).
