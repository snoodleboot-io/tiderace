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

② attacks both directly: **one** embedded interpreter imports the project **once**, and tests are driven
by FFI call instead of a pipe frame. This is the highest-leverage remaining perf lever and the last open
ROADMAP-v2 item — a deliberate side-bet, independent of Tracks A/B, riding the existing `ShimTransport`
seam, blocking nothing.

## Goals

- A third `ShimTransport` impl, **`InProcessTransport`**, that embeds **one** CPython interpreter in the
  Rust process (PyO3) and drives riptide's own executor by **FFI call** instead of pipe frame — deleting
  the JSON-over-pipe control plane. **No `Worker` change** (rides the seam).
- Keep **fork-from-embedded** isolation (ADR-E013): per-test `fork()` from the warm embedded
  interpreter; the Rust parent stays single-threaded at the fork point.
- A measured **perf delta vs the `PipeTransport` baseline** (import-once + the syscall win), recorded
  in `benchmarks/RESULTS-native.md` / `RESULTS-3way.md`. Target: close the residual cold-run gap to
  pytest (1.17 s → ≤ pytest) by importing once instead of N×.

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
