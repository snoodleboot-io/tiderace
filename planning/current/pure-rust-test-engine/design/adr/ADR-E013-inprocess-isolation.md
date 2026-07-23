# ADR-E013 — In-process backend isolation: fork-from-embedded (not per-test reset)

**Status:** ✅ Accepted (design) · Ratifies the open question left by
[ADR-E011](ADR-E011-shim-transport-seam.md) ②.

**Relates to:** [ADR-E011](ADR-E011-shim-transport-seam.md) (the `ShimTransport` seam + the proposed
in-process / FFI backend), [ADR-E003](ADR-E003-fork-snapshot-isolation.md) (fork-snapshot isolation,
the watermark/COW model), [ADR-E004](ADR-E004-content-addressed-cache.md) (cache soundness),
[ADR-E008](ADR-E008-cross-platform.md) (no-fork fallback for Windows).

## Context

[ADR-E011](ADR-E011-shim-transport-seam.md) introduced the `ShimTransport` seam and proposed an
`InProcessTransport` ② that embeds **one** CPython interpreter in the Rust process (via PyO3) and
drives tiderace's own executor by FFI — deleting the subprocess + JSON-over-pipe control plane. The
the `spike-inproc/` spike proved embedding is feasible (GO; spike since disposed — evidence in the
[in-process backend ticket](../../../../backlog/in-process-ffi-backend/DESIGN.md) + git history). E011
explicitly left **one** question open:

> "isolation under an embedded interpreter — fork-from-embedded (retain the watermark model) vs.
> per-test module reset. ② replaces the pipe/JSON *control plane*, not the fork-based *isolation*."

This ADR answers it.

## Decision

**Isolate with `fork()` from the embedded interpreter — the same per-test COW model the engine already
uses ([ADR-E003](ADR-E003-fork-snapshot-isolation.md)).** The in-process backend changes only *how Rust
talks to Python* (FFI call instead of pipe frame); it does **not** change *how tests are isolated*.

Concretely: the Rust process boots and warms one embedded interpreter (the in-process Wellspring);
wider-scope fixtures live in that parent; a pristine child is `fork()`ed per test, inherits the warm
interpreter via copy-on-write, runs the function-scope setup + body + teardown, and exits. The result
crosses the fork boundary over a minimal pipe (outcome + coverage), not the full JSON control plane.

**Per-test module reset is rejected as the default and parked** (see Revisit trigger).

### Fork-safety requirement (the cost of this choice)

`fork()` in a multi-threaded process is only safe if no other thread holds a lock the child will need.
So the in-process backend must **fork before (or without) spawning Rust worker threads in the parent**:
the embedded-interpreter parent stays single-threaded up to the fork point; parallelism comes from
**multiple forked children across cores** (each a separate process, separate GIL), exactly as today —
not from threads in the embedded parent. This is a hard constraint on the `InProcessTransport`
implementation, validated by a C-ext smoke test (numpy/pandas/pydantic-core) under fork.

## Why (the trade-off)

| | **A: fork-from-embedded (chosen)** | B: per-test module reset (rejected) |
|---|---|---|
| Isolation | pristine per test (COW) — identical to ADR-E003 | leaky: C-extension/native global state survives a reset |
| Cache soundness (E004) | preserved (a result stays a pure function of its inputs) | **at risk** — order-dependent outcomes poison the cache |
| Parallelism | free, across cores (forked processes, separate GILs) | one interpreter = one GIL; needs extra processes anyway |
| Raw per-test cost | a cheap `fork()` remains | none (direct call) — the fastest path *if* it were sound |
| Build risk | fork-in-threaded-process hazard (bounded by the constraint above) | open-ended state-leak debugging |

The decisive factor is **soundness, not speed**. The cache + impact spine (E004/E006) is the engine's
reason to exist, and it assumes per-test purity. B trades that away for a raw-speed win that A largely
already captures (deleting the pipe/JSON control plane is where ②'s win comes from; the `fork()` itself
is sub-millisecond and amortized by COW). Keeping the proven isolation model also means ② reuses the
entire Phase-3 watermark/`ForkPlan` machinery rather than inventing a new reset protocol.

This is **not** the subinterpreter path [ADR-010 rejected](../../../../../docs/design/decisions.md): that
was *N* PEP-684 subinterpreters per process (single-phase-init C extensions segfault the process). A is
*one* interpreter + `fork()` for isolation + an FFI control plane — no subinterpreters involved.

## Consequences

- ➕ ② becomes a pure transport optimization: same isolation, same cache soundness, same Windows story
  (the `SubprocessWorker` no-fork fallback, E008, is unchanged — A is a fork-path backend).
- ➕ Reuses Phase-3 fork/watermark code; the diff is the transport, not the executor.
- ➖ The `InProcessTransport` must police fork-safety (single-threaded-parent-at-fork) — a real
  implementation constraint, not free.
- ➖ Retains a (cheap) fork per test, so B's theoretical raw-call floor is left on the table.

## Revisit trigger

Promote **B (per-test reset / subinterpreters)** from parked to a candidate if **either**: (1) profiling
shows `fork()` itself is a material cost on the target workloads (many ultra-cheap tests where even a
sub-ms fork dominates), **and** (2) per-interpreter-GIL subinterpreters (PEP 684) plus per-interpreter
C-ext state isolation mature enough to make reset *sound*. Until both hold, A stands.
