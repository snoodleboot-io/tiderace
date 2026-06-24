# PRD: ② In-process / FFI execution backend

**Status:** Ready (design ratified, spike GO)
**Owner:** _unassigned_
**Last updated:** 2026-06-24
**Refs:** [ADR-E011 ②](../../current/pure-rust-test-engine/design/adr/ADR-E011-shim-transport-seam.md) ·
[ADR-E013 (isolation, ratified)](../../current/pure-rust-test-engine/design/adr/ADR-E013-inprocess-isolation.md) ·
[ROADMAP-v2 §4](../../current/pure-rust-test-engine/ROADMAP-v2.md) ·
spike [`spike-inproc/`](../../../spike-inproc/RESULTS.md) (GO) ·
baseline [`benchmarks/RESULTS-native.md`](../../../benchmarks/RESULTS-native.md)

## Problem

Today the Rust kernel drives CPython as a **subprocess**: every test result crosses a pipe as
length-prefixed JSON (`PipeTransport`). For a suite of many cheap tests that per-test control-plane
overhead is measurable — `benchmarks/RESULTS-native.md` shows the native engine *slower* than pytest on
a full cold run of 511 cheap tests, dominated by the per-test fork + pipe round-trip. The inner loop is
already excellent (~7 ms warm vs pytest ~650 ms); the open lever is the **per-test transport cost**.

This is the last open item on ROADMAP-v2 — a deliberate side-bet, independent of Tracks A/B, that rides
the existing `ShimTransport` seam and blocks nothing.

## Goals

- A third `ShimTransport` impl, **`InProcessTransport`**, that embeds **one** CPython interpreter in the
  Rust process (PyO3) and drives riptide's own executor by **FFI call** instead of pipe frame — deleting
  the JSON-over-pipe control plane. **No `Worker` change** (rides the seam).
- Keep **fork-from-embedded** isolation (ADR-E013): per-test `fork()` from the warm embedded
  interpreter; the Rust parent stays single-threaded at the fork point.
- A measured **perf delta vs the `PipeTransport` baseline** (the syscall win), recorded in
  `benchmarks/RESULTS-native.md`.

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
