# ② in-process backend — benchmark findings (the premise was wrong)

> Measured 2026-06-25. `engine/crates/engine-inproc` (PyO3 0.26 + embedded CPython 3.14.4), built with
> `PYO3_PYTHON=<venv> RUSTFLAGS="-L native=<base>/lib"`, run with
> `LD_LIBRARY_PATH=<base>/lib PYTHONPATH=<venv>/site-packages`.

## What ② set out to prove

ADR-E011 ② / the backlog ticket premised that the native engine's cost was the **per-test transport**:
each test crosses a pipe as JSON between Rust and the Python subprocess. The in-process backend embeds
**one** CPython and drives the shim's executor by **FFI call** — deleting that subprocess + pipe — so
the premise was that this would be meaningfully faster.

## What was actually measured

`InProcessTransport` works and is **correct**: it imports once, drives tests by FFI returning native
`ExecResponse` values, and — verified — keeps **fork-from-embedded isolation** (a test that mutates a
module global in its forked child does **not** leak to the next test).

But the speed premise is **wrong**:

| corpus | in-process (FFI, no pipe) | single-pipe (subprocess) | verdict |
|---|---:|---:|---|
| 500 trivial tests | ~2024 ms | ~2012 ms | **identical** |
| fx_corpus (509 fixture tests) | ~3.7 s | ~3.2 s | comparable |

**The pipe/JSON control plane is negligible (~0 ms/test).** The real per-test cost is the **`fork()`
itself** (~4 ms/test: fork + child `_child_exec` setup + body + `_exit` + parent reap), which *both*
paths pay equally. Deleting the transport changes nothing measurable, because the transport was never
the bottleneck.

## Corrected understanding of the levers

1. **`fork()` per test (~4 ms) is the cost** — not the transport, not (for cheap tests) the body.
   Therefore the highest-leverage lever is **fewer forks**: [pure-test batching](../planning/backlog/pure-test-batching/)
   (run K pure tests per fork) directly attacks it. *Re-ranked to #1.*
2. **Parallelism** already banked the big win (pool: fx_corpus 3.27 s → 1.17 s) by running forks across
   N processes — at the cost of **N× project import**.
3. **② is only a win when import-once is combined with PARALLEL fork-out.** One embedded interpreter
   (import once) that forks **N children in parallel** would get the pool's parallelism *without* the
   N× import — beating the pool on import-heavy suites. The current `InProcessTransport` is sequential
   (the shim's `Engine.run` forks one child at a time), so it has the import-once half but not the
   parallelism half, and does **not** beat the pool. The follow-on is a **parallel-fork driver** on the
   embedded interpreter (fork from the single main thread, children run concurrently, reap via pipes).

## Status

- ✅ ② **feasibility + correctness proven** (PyO3 links 3.14; embed runs `_decimal`; FFI drives the
  executor; fork isolation real). A reusable `ShimTransport` backend exists behind the seam.
- ❌ ② **transport swap is not a perf win** on its own (this doc).
- ⏭ The win requires **parallel-fork-from-embedded** (import once + parallel) — re-scoped on the ticket.
  And independently, **pure-test batching** is now the #1 lever because the fork is the cost.
