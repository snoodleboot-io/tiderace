# ADR-E015 — Conditional sub-interpreter tier (`SubInterpWorker`) for Windows parallelism

**Status:** ✅ Accepted (design) · **spikes done, build phased** (TID-2 → TID-9/10/11). Extends
[ADR-E008](ADR-E008-cross-platform.md) (cross-platform `Worker` fallback) and
[ADR-E014](ADR-E014-no-fork-restore-ladder.md) (the isolation ladder).

**Relates to:** [ADR-E002](ADR-E002-execution-substrate.md) (which *rejected* sub-interpreters as the
default substrate — this ADR revisits that with data, for a *conditional* tier only),
[ADR-E003](ADR-E003-fork-snapshot-isolation.md) (fork isolation — the Unix default),
[ADR-E011](ADR-E011-shim-transport-seam.md) (the `Worker`/`ShimTransport` seam this slots behind).

## Context

Windows has no `fork()`. Its execution path is the no-COW `SubprocessWorker`
([ADR-E008](ADR-E008-cross-platform.md)), which runs a batch **sequentially** in one warm process
(verified end-to-end in TID-5). So on Windows, tiderace has **no parallelism at all** — the pool's
per-core wellsprings (a fork-based mechanism) don't exist there.

PEP 684 (per-interpreter GIL, CPython 3.12+) plus PEP 734 (`concurrent.interpreters`, 3.14) make it
possible to run N sub-interpreters, **each with its own GIL**, on N OS threads in **one process** —
true parallel Python execution, no fork, no free-threaded build. Two spikes decided the shape:

- **Universal backend — NO-GO.** numpy's core C-extension refuses to load in an isolated sub-interpreter
  (`module numpy._core._multiarray_umath does not support loading in subinterpreters`). numpy underpins
  pandas/scipy/sklearn/torch, so a *universal* sub-interpreter backend fails on most real suites. This
  is the same hazard [ADR-E002] cited, now confirmed empirically on the current stack (numpy 2.4.6,
  CPython 3.14.4).
- **Hybrid — GO.** (a) **Detection is cheap + reliable**: importing a module in a throwaway isolated
  sub-interpreter classifies it `safe`/`unsafe` (`pure-python`/`stdlib`/`pytest` → safe; `numpy` →
  unsafe). (b) **The parallelism is real**: 4 CPU-bound units took 4.73 s sequentially vs **1.63 s
  (2.9×)** across 4 sub-interpreters on 4 threads — one process, no fork.

## Decision

Add a **conditional sub-interpreter tier** — a `SubInterpWorker` behind the existing `Worker` seam —
that runs the **sub-interp-safe subset** of a suite across a pool of isolated sub-interpreters, and
routes everything else to the existing fork (Unix) / subprocess (Windows) path. It follows the
[ADR-E014](ADR-E014-no-fork-restore-ladder.md) ladder pattern: **detect, then route**.

- **Detect** (per module): probe its import closure in an isolated sub-interpreter; persist the
  `safe`/`unsafe` verdict content-addressed, like the purity verdicts (compute once, share CI→laptop).
- **Route**: safe modules → the sub-interpreter pool (parallel, one process, no fork); unsafe modules →
  fork (Unix) or `SubprocessWorker` (Windows).
- **Isolate**: per-interpreter state (own `sys.modules`/globals) **is** the isolation — no fork, no
  snapshot/restore on this path.

**Target = Windows.** This is Windows-first: it's the one place sub-interpreters add a capability the
ladder doesn't (parallel no-fork). On Unix the fork pool already parallelizes and the ladder already
removes the fork cost for pure tests, so the tier is **opt-in on Linux** (a lower-memory, single-process
option), not a default.

## Why (the trade-off)

| | **Conditional tier (chosen)** | universal sub-interp backend (rejected) | status quo |
|---|---|---|---|
| numpy-class suites | fork/subprocess as today (unaffected) | **fail to load** | work |
| pure-Python on **Windows** | **parallel, no fork** (the win) | parallel (but whole suite must be safe) | **sequential** |
| pure-Python on Linux | opt-in, single-process parallel | — | already parallel (fork pool) |
| import cost | unchanged (each sub-interp re-imports; cheap for pure-Python) | worse | n/a |
| new surface | one `Worker` + detect/route + a safe-set cache | a whole backend + ecosystem matrix | — |

The decisive facts: the universal backend is **impossible today** (numpy), but the hybrid's benefit is
**real and load-bearing on Windows** (from zero parallelism to cores-bounded, for the pure-Python
fraction), at the cost of one more `Worker` behind an existing seam. Scope is honest: the payoff is
proportional to a suite's pure-Python fraction, and on Linux it's marginal.

## Consequences

- ➕ Windows gains parallel no-fork execution for the pure-Python subset (the only new capability here).
- ➕ Reuses the ladder's shape: detection + a content-addressed verdict cache (shareable), the same
  machinery as purity (TID-1) and the result cache (E004).
- ➕ No PyO3/embedding required in the favoured design — a shim `--subinterp` mode drives a
  `concurrent.interpreters` pool behind the existing pipe/`ShimTransport` seam (Phase 2).
- ➖ Benefit is **scope-limited** to sub-interp-safe modules; a numpy-touching test is never accelerated.
- ➖ New wire/protocol work: parallel dispatch needs batched or out-of-order responses (the pipe is
  currently one-in-flight) — resolved in Phase 2.
- ➖ Requires CPython ≥ 3.12 for per-interpreter GIL (≥ 3.14 for `concurrent.interpreters`); older
  interpreters simply don't get the tier (self-skip → fork/subprocess).

## Alternatives considered

- **Universal sub-interpreter backend** — rejected: numpy (→ the scientific stack) can't load in an
  isolated interpreter (spike). Would fail or force-serialize most real suites.
- **Free-threading (PEP 703, TID-3)** — orthogonal and currently blocked (no `python3.14t` build here).
  Not required for this tier: per-interpreter GIL already delivers the parallelism (measured 2.9×).
- **Do nothing (keep Windows sequential)** — acceptable if Windows + pure-Python suites aren't a target;
  they are, so we build the tier.

## Revisit trigger

- **Widen automatically** when numpy (and the top C-exts) ship sub-interpreter support — the safe-set
  detector will start classifying them `safe` with no code change; re-run the TID-2 spike to confirm.
- **Reconsider Linux default** if a single-process, lower-memory pool proves materially better than the
  fork pool on pure-Python suites at scale (benchmark in Phase 3).
