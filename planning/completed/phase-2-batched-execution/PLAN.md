# Phase 2 — Faster execution: batch → persistent workers → subinterpreters

> **Status:** Stage A in progress on `feat/tiderace-batched-execution`. B and C planned.
> **Driver:** ADR-009. **Goal:** kill the per-test interpreter-startup tax that makes
> tiderace's cold full run ~Nx slower than in-process pytest, while keeping 100% pytest
> compatibility (fixtures, plugins, assertion rewriting all stay — real pytest runs).

## Why
The runner spawns one `python -m pytest <nodeid>` per test → *N* interpreter + pytest
imports (~0.5 s each). pytest itself runs the whole suite in **one** process. The fix is
to reduce the process count, not to replace pytest.

## Stage A — Batched subprocess (this branch)
**Change:** distribute selected tests across the worker pool; each worker runs ONE
`pytest <nodeid> <nodeid> …` process. Recover per-test outcomes from pytest's `-rA`
summary lines (each contains the exact node id). Keep the isolated per-test path for
`--coverage` runs (preserves dep-graph precision) and behind a new `--isolate` flag.

**Tasks**
1. `runner`: add a batched executor (chunk → one pytest per chunk → `-rA` parse → map
   node id → status → `TestResult` per test). Drop `-x` in batch mode; per-batch timeout.
2. `runner`: `parse_batch_summary()` — parse `PASSED|FAILED|ERROR|SKIPPED|XFAIL|XPASS <nodeid>`.
3. `main`: route — coverage or `--isolate` → isolated path; else → batched. Add `--isolate`.
4. Unit tests for `parse_batch_summary` (incl. failure-with-reason lines, missing test = error).
5. Integration: existing `tests/cli.rs` must still pass (batched is now the default path).
6. Benchmark **before/after** on the same fixture; record the cold-run drop.

**Acceptance**
- Cold full run wall-clock drops materially toward pytest-parity on the fixture.
- All existing unit + integration tests green; fmt + clippy(-Dwarnings); coverage ≥ 80.
- `--isolate` reproduces legacy one-process-per-test behaviour.

**Known trade-offs (documented in ADR-009)**
- Per-test wall-clock timing is coarse in batch mode (timeout is per batch).
- A hang kills its whole batch (recorded as errors), not just one test.
- Precise per-test coverage still needs a `--coverage` run (isolated path).

## Stage B — Persistent warm workers (next)
Long-lived CPython workers with pytest pre-imported, fed node ids over IPC (execnet-style,
as `pytest-xdist` does). Startup paid once per worker per daemon/watch session. Unlocks a
`tiderace watch` mode with tens-of-ms edit→result loops. Larger effort; needs an IPC
protocol and a worker lifecycle. Separate branch + ADR addendum.

## Stage C — Embedded CPython subinterpreters (longer-term)
Embed libpython via PyO3; per-core subinterpreters (PEP 684 per-interpreter GIL, 3.12+)
for true in-process parallelism, zero per-test startup. Gated on PyO3 subinterpreter
support maturing; highest performance and the clearest "more than pytest" story
(lighter than xdist's processes). Prototype-first, behind a feature flag.

## Explicitly NOT doing
- Reimplementing pytest's fixtures / assertion rewriting / plugin system in Rust
  (multi-year, fragments from the ecosystem).
- RustPython / a pure-Rust interpreter (breaks C-extension packages).
