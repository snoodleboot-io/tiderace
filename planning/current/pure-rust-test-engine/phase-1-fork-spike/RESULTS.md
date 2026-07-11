# Phase 1 — Fork/Wellspring Spike: Results & Go/No-Go

> **Verdict: 🟢 GO.** Date: 2026-06-16. Branch: `feat/phase-1-fork-spike`.
> Validates [ADR-E002](../design/adr/ADR-E002-execution-substrate.md) (subprocess + shim) and
> [ADR-E003](../design/adr/ADR-E003-fork-snapshot-isolation.md) (fork-from-warm isolation).
> Spike code (`spike/`) has since been disposed (GO captured here + in git history); it was a
> throwaway crate with `run_spike.sh`.

## What was built

A real, end-to-end harness (no pytest underneath):

- **Wellspring** (`spike/shim.py`, disposed) — one CPython process imports the
  corpus + numpy **once**, then `os.fork()`s a pristine COW child per test and runs it: a
  pytest-style function by call, or a `unittest.TestCase` via stdlib `TestCase.run()` at method
  granularity. Child→parent result over an `os.pipe`; crash/timeout detected via `waitpid`/`select`.
- **Orchestrator** (`spike/src/main.rs`, disposed) — Rust drives the
  Wellspring over a length-prefixed (u32 LE) JSON frame protocol; `warm` mode (fork-from-warm) and
  `fresh` mode (process-per-test) for comparison. `cargo fmt`/`clippy -Dwarnings` clean; 3 unit
  tests for the frame codec + spec parsing.

## Go/No-Go criteria

| # | Criterion | Result |
|---|-----------|--------|
| **C1** | Real outcomes for pytest-fn + unittest agree with stock pytest (differential oracle) | ✅ **PASS** — engine outcome map == pytest, exactly (2 passed, … incl. 2 intentional FAILED) |
| **C2** | Fork isolation — a mutated module global is invisible to the next test | ✅ **PASS** — engine passes both isolation tests; stock pytest (one process) **fails** the 2nd. The divergence *is* the isolation win |
| **C3** | Crash + timeout → `Outcome::Error`, Wellspring survives | ✅ **PASS** — `os._exit(139)`→error, 5 s hang→error(timeout), and a normal test **after both** still passed |
| **C4** | Fork-from-warm with a C-extension (numpy) imported pre-fork, clean across many forks | ✅ **PASS** — 50 sequential forks with numpy warm ran clean (with BLAS/OMP threads pinned to 1) |
| **C5** | Spike is real & tested (lint clean, unit tests, e2e runner) | ✅ **PASS** — fmt + clippy `-Dwarnings` clean; 3 unit tests; `run_spike.sh` exercises C1–C6 |
| **C6** | Fork-from-warm beats process-per-test and is competitive with pytest | ✅ **PASS** — see numbers below |

## Benchmark numbers (hyperfine, this host)

**Small suite — 5 tests (cold start, mean ± σ):**

| Runner | Time | vs warm |
|--------|------|---------|
| **warm (fork-from-warm)** | **176.7 ms ± 19.6** | 1.0× |
| pytest (in-process) | 407.7 ms ± 10.7 | 2.31× slower |
| fresh (process/test) | 851.7 ms ± 25.2 | 4.82× slower |

**Scale — 50 executions (import amortization):**

| Runner | Time | vs warm |
|--------|------|---------|
| **warm x50 (1 import + 50 forks)** | **367.4 ms ± 23.0** | 1.0× |
| fresh x50 (50 imports) | 12.466 s ± 1.72 | **33.9× slower** |

Fork-from-warm even beats stock pytest **2.31×** on a *cold* tiny suite (lighter startup: no
plugin/collection machinery), and the import-once advantage compounds with suite size (33.9× over
process-per-test at 50 execs). This is the core thesis, validated.

## Learnings handed to Phase 2 (contract)

1. **Substrate shape confirmed:** Rust orchestrates; the `fork()` happens **Python-side** in the
   Wellspring (so children inherit warm imports via COW). Rust↔Wellspring over stdin/stdout; the
   intra-process child→parent result over an `os.pipe`. This is the [05-execution-wellspring](../design/05-execution-wellspring.md) shape.
2. **Wire protocol:** length-prefixed (u32 LE) JSON frames worked cleanly. The bincode-vs-msgpack
   decision ([ADR-E002](../design/adr/ADR-E002-execution-substrate.md)) is **deferred** — JSON is
   adequate at this scale; revisit only if serialization shows up in Phase-5/6 profiling. Python
   has no native bincode, so msgpack is the likely binary choice if/when needed.
3. **C-extension fork-safety (feeds Phase 3):** numpy required pinning native thread pools
   (`OPENBLAS/OMP/MKL_NUM_THREADS=1`) to fork safely. Generalize as a thread-policy /
   `reinit_after_fork` mechanism for non-fork-safe resources. **This is the [ADR-E003](../design/adr/ADR-E003-fork-snapshot-isolation.md)
   risk, and it is real but tractable.**
4. **Per-fork cost seed (feeds Phase 3 `MemoryGovernor`):** 50 warm execs in ~367 ms with ~150 ms
   one-time import ⇒ on the order of a few ms per fork incl. a numpy-using child on this host.
5. **Environment:** `uv` provisions CPython 3.14.4 (≥3.12, `sys.monitoring`-ready for Phase 5).
6. **No reshape needed:** the GO verdict means Phases 2–3 proceed with `ForkWorker` as the default
   (the [ADR-E003](../design/adr/ADR-E003-fork-snapshot-isolation.md) `SubprocessWorker` fallback
   is **not** triggered).

## Honest caveats

- Spike scope: 5-test corpus, single host, no fixtures/parametrize/coverage (those are Phases 3–5).
  The numbers establish the *mechanism* and its scaling, not final product benchmarks.
- The wire codec, error model, and worker lifecycle here are spike-grade and will be rebuilt to the
  design's trait seams in Phase 2 (this is a spike, not a stub).
