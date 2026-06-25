# PRD: Batch pure tests per fork (reduce the per-test isolation tax)

**Status:** Ready (depends on the purity guard — see Risks)
**Owner:** _unassigned_
**Last updated:** 2026-06-25
**Refs:** [ADR-E003](../../current/pure-rust-test-engine/design/adr/ADR-E003-fork-snapshot-isolation.md)
(fork-snapshot isolation) · [ADR-E004](../../current/pure-rust-test-engine/design/adr/ADR-E004-content-addressed-cache.md)
(`Purity`/`SandboxHooks` seam) · ROADMAP-v2 Phase-4 "purity guard" (deferred) ·
baseline [`benchmarks/RESULTS-3way.md`](../../../benchmarks/RESULTS-3way.md)

## Problem

The native engine `fork()`s **once per test** for pristine COW isolation (ADR-E003). After
parallelizing across cores (cold full run 1.17 s, RESULTS-3way.md), the remaining structural cost on a
big cheap suite is the **fork count itself** — N forks for N tests. Many tests are **pure**: they don't
mutate module/session/global or native state, so several could safely share one forked child.

Batching K pure tests per fork cuts the fork count from N to ~N/K, attacking the residual per-test tax
that survives parallelism — complementary to ② (which removes per-worker import + the pipe plane).

## Goals

- A **batched execution mode**: a forked child runs **K tests** (then exits), for tests classified
  **pure** by the purity guard. Impure tests keep their own pristine fork (unchanged).
- The scheduler groups pure tests into batches of size K; impure tests stay 1-per-fork.
- A measured **fork-count reduction + speedup** on a pure-heavy corpus, with **identical outcomes** vs
  the per-test baseline (differential).

## Non-Goals

- **No weakening of isolation by default.** Isolate-per-test stays the default; batching is applied
  **only** to tests proven pure (conservative).
- Not batching impure tests (clock/RNG/network/shared-state mutators) — they must stay isolated.
- Not a replacement for ② — these stack (② = import once; batching = fewer forks).

## Success criteria

1. On a corpus of pure tests, fork count drops ~K× and wall-clock improves measurably.
2. **Soundness:** batched outcomes are byte-identical to per-test outcomes on the differential corpus.
3. A deliberately state-leaking test pair is **detected** (not batched, or flagged) — no silent
   contamination.

## Risks

- **Soundness is the whole risk.** A test wrongly classified pure could be contaminated by a batch-mate
  → wrong result. Mitigation: **conservative default** (isolate unless *proven* pure) + a differential
  gate in CI.
- **Prerequisite:** this needs the **purity guard** — the runtime detector of shared-state mutation
  (a deferred Phase-4 item). The *policy* seam (`cache::{Purity, SandboxHooks}`) already exists; the
  *detector* does not. **This ticket should not start until the purity guard lands** (or be split:
  guard first, batching second).
