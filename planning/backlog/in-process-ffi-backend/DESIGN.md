# Design: ② In-process / FFI execution backend

**Status:** Draft (ready to start) · **Related:** [PRD.md](PRD.md) ·
[ADR-E013](../../current/pure-rust-test-engine/design/adr/ADR-E013-inprocess-isolation.md) (isolation, ratified) ·
[ADR-E011](../../current/pure-rust-test-engine/design/adr/ADR-E011-shim-transport-seam.md) (the seam)

## Overview

Add `InProcessTransport`, a third implementation of the existing `ShimTransport` trait
(`engine-core/src/exec/transport.rs`). Production wires `PipeTransport` (subprocess over pipes); tests
wire an in-process double; this adds an in-process **real** backend that embeds CPython via PyO3 and
drives riptide's executor by FFI. Isolation stays fork-from-embedded (ADR-E013): the warm embedded
interpreter is the in-process Wellspring, and a `fork()` per test inherits it copy-on-write. Only the
**control plane** changes (FFI call vs JSON-over-pipe); the **isolation** and everything above the seam
(Worker, scheduler, cache, impact, reporters) are untouched.

## Affected modules

- `engine/crates/engine-core/src/exec/transport.rs` — the `ShimTransport` seam already exists; the new
  impl plugs in beside `PipeTransport`. (Or a new `engine-inproc` crate behind a feature flag, to keep
  the libpython dependency optional and off the default/Windows build — decide in step 0.)
- `engine/crates/engine-core/src/exec/` — capability detection chooses `InProcessTransport` when
  available; `--worker=`/`--transport=` override (mirrors ADR-E008 selection).
- New: `in_process_transport.rs` — `ready()` + `exchange()` over the embedded interpreter; the
  fork-per-test + result handoff (a minimal pipe/shared-mem for outcome+coverage, not the full JSON
  control plane).
- `spike-inproc/` — the proven reference (PyO3 0.23, `auto-initialize`); port its embed+drive logic.

## Data / schema changes

None. `ExecRequest`/`ExecResponse` shapes are unchanged (the transport is swapped, not the protocol);
coverage rides the same additive field.

## Implementation plan

0. **Feasibility probe (gate).** Add PyO3 to a throwaway target in the engine workspace and confirm it
   links libpython on Linux CI **and** that the Windows job still builds (feature-gated off there).
   If it can't be made clean, keep `InProcessTransport` in a separate optional crate.
1. **`InProcessTransport: ShimTransport`** — embed one interpreter (PyO3), implement `ready()` (boot +
   import the project once = the in-process Wellspring) and `exchange()` (run one test).
2. **Fork-from-embedded** (ADR-E013) — `exchange()` forks per test from the warm interpreter; enforce
   the single-threaded-parent-at-fork constraint; stream outcome+coverage back over a minimal pipe.
3. **`PyConfig` home/venv plumbing** — resolve the spike's cosmetic warnings; honor the target venv.
4. **C-ext smoke** — numpy/pandas/pydantic-core imported + used in one interpreter under fork.
5. **Benchmark** — `riptide-daemon bench`-style cold/warm over the many-cheap-tests corpus, in-process
   vs `PipeTransport`; record the delta in `benchmarks/RESULTS-native.md`.

## Testing

- The existing differential + fixtures-acceptance suites run **on the new backend** (same outcomes →
  isolation + correctness preserved).
- A C-ext smoke test under fork (the spike's risk area).
- The benchmark is the perf acceptance (must beat `PipeTransport` on the flagged case).

## Risks

See [PRD.md](PRD.md#risks): libpython linkage in the workspace (probe first), fork+PyO3+GIL safety,
PyConfig/venv plumbing. None of these touch isolation or cache soundness — those are settled by E013.
