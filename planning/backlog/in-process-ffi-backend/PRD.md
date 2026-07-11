# PRD: ② In-process / FFI execution backend

**Status:** Ready (design ratified, spike GO)
**Owner:** _unassigned_
**Last updated:** 2026-06-24
**Refs:** [ADR-E011 ②](../../current/pure-rust-test-engine/design/adr/ADR-E011-shim-transport-seam.md) ·
[ADR-E013 (isolation, ratified)](../../current/pure-rust-test-engine/design/adr/ADR-E013-inprocess-isolation.md) ·
[ROADMAP-v2 §4](../../current/pure-rust-test-engine/ROADMAP-v2.md) ·
spike: `spike-inproc/` (GO — disposed; evidence captured in [DESIGN.md](DESIGN.md) + git history) ·
baseline [`benchmarks/RESULTS-native.md`](../../../benchmarks/RESULTS-native.md)

## Problem

Today the Rust kernel drives CPython as a **subprocess** over pipes (`PipeTransport`), and the parallel
runner launches **one wellspring per core** — so each worker imports the project independently. After
parallelizing (cold full run 3.27 s → 1.17 s, `benchmarks/RESULTS-3way.md`), the **residual gap to
pytest (1.17 s vs 0.86 s) is now the per-worker import**: the 8-way pool pays ~8× the project import
(visible as ~7 s total CPU over 1.17 s wall). The fork itself is cheap; the costs left are (a) N× import
and (b) the per-test pipe/JSON control plane.

## ⚠️ Benchmark finding (2026-06-25) — premise corrected

A working `InProcessTransport` was built and benchmarked ([`benchmarks/RESULTS-inproc.md`](../../../benchmarks/RESULTS-inproc.md)).
Result: **the transport/pipe was never the bottleneck.** On 500 trivial tests, in-process (FFI, no pipe)
and the subprocess+pipe path are **identical (~2.0 s)** — the cost is the **`fork()` per test (~4 ms)**,
which both pay. Deleting the control plane changes nothing measurable.

So **② is *not* a win as a transport swap.** Its only real lever is **import-once**, and that pays off
**only if combined with PARALLEL fork-out**: one embedded interpreter (import once) that forks **N
children in parallel** would match the pool's parallelism *without* the pool's N× import. The current
`InProcessTransport` is sequential, so it has import-once but not parallelism, and does not beat the
pool. **Re-scoped accordingly** (Goals below). Feasibility + correctness (incl. fork-from-embedded
isolation) are *proven*; what's unproven is the parallel-fork win.

## Goals (re-scoped after the benchmark)

- ✅ **`InProcessTransport: ShimTransport`** — embed one CPython (PyO3), import once, drive the executor
  by FFI, fork-from-embedded isolation. **Done + proven** (`engine/crates/engine-inproc`).
- ⏭ **Parallel fork-out from the one embedded interpreter** — the actual win: the single (main-thread)
  parent forks **N children concurrently** off the warm interpreter; children run in parallel; results
  reaped over N pipes. One import + parallelism — vs the pool's N imports + parallelism.
- ⏭ A measured delta showing **in-process parallel ≤ the subprocess pool** on an **import-heavy**
  suite (where the pool's N× import dominates) — that's the only regime ② wins. On cheap/light-import
  suites it ties the pool (both fork-bound). Record in `benchmarks/RESULTS-inproc.md`.

## Non-Goals

- **Not** the subinterpreter path (ADR-010 rejected N subinterpreters; this is one interpreter + fork +
  FFI).
- **Not** per-test module reset (ADR-E013 parked it — it breaks cache soundness).
- **No** change to isolation semantics, the cache, impact analysis, or the Windows `SubprocessWorker`
  fallback — all unchanged.

## Success criteria

1. `InProcessTransport: ShimTransport` exists behind the seam; the engine selects it by capability/flag.
2. C-extension smoke (numpy/pandas/pydantic-core) passes in one embedded interpreter under fork.
3. A reproducible benchmark shows the in-process backend beats the subprocess `PipeTransport` on a
   many-cheap-tests corpus (the case RESULTS-native.md flagged), with equal outcomes.
4. Cache soundness + per-test isolation preserved (the existing differential/acceptance suites stay
   green on the new backend).

## Risks

- **libpython linkage in the engine workspace** — the spike uses an isolated Cargo setup with
  `PYO3_PYTHON`; bringing PyO3 into `engine-core`/a new crate must not break the Linux + Windows CI
  build. **First step is a feasibility probe** (does it link/build in the workspace?).
- **fork + PyO3 + GIL** — forking a process with an initialized interpreter is delicate; the
  single-threaded-parent-at-fork constraint (ADR-E013) must be enforced and tested.
- **PyConfig/venv plumbing** — the spike has cosmetic home/venv warnings to resolve.
