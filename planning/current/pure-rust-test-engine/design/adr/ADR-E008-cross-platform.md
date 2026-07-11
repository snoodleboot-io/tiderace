# ADR-E008 — Fork-first; cross-platform fallback behind the `Worker` trait

**Status:** ✅ Accepted (design) · Depends on E003, E005.

## Context

The performance thesis rests on `fork()` (E003), which exists on Linux and macOS but **not**
Windows. We still want correctness everywhere, and we want a clean path to future execution
modes (free-threaded CPython, distributed) without touching call sites.

## Decision

Make execution mode a runtime-selected implementation of the `Worker` trait (E005):

| Impl | Platform / mode | Isolation | Startup amortization |
|---|---|---|---|
| **`ForkWorker`** (primary) | Linux, macOS | pristine per test (COW) | import once (wellspring) |
| **`SubprocessWorker`** (fallback) | Windows / no-fork | fresh process per batch | warm pool, no COW snapshot |
| **`ThreadWorker`** (future) | free-threaded CPython (PEP 703) | per-thread | shared warm imports |
| **`RemoteWorker`** (future) | distributed | remote process | remote warm hosts + cache |

- The orchestrator/scheduler are **platform-agnostic**: they speak `Worker`, never `fork`.
- Platform/capability detection picks the impl at startup; `--worker=` can override.
- On Windows, fixture-scope *snapshots* degrade to **re-execution of scope setup per worker**
  (correct, just less amortized) since there's no COW.

## Consequences

- ➕ Full feature set on Linux/macOS; correct, degraded performance on Windows.
- ➕ Free-threaded and distributed futures are drop-in, not rewrites.
- ➖ Multiple worker impls to test; a capability matrix in CI.
- ➖ Snapshot-layering logic must have a no-COW code path (scope setup re-run) for fallbacks.

## Alternatives considered

- **Fork-only:** drops Windows entirely — rejected (acceptable to *lead* with Linux/macOS, not
  to *exclude* Windows forever).
- **CRIU checkpoint/restore for non-fork snapshotting:** Linux-only and heavy — parked as a
  future `Worker` impl, not a Windows answer.
- **WSL-only on Windows:** documented as a recommendation, not a substitute for a native
  fallback.

## Revisit trigger

Strong Windows demand → prioritize CRIU/WSL investigation or a process-snapshot scheme to bring
COW-like amortization to the `SubprocessWorker` path.
