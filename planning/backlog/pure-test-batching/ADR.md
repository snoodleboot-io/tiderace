# ADR: Batch only proven-pure tests; isolate by default

**Date:** 2026-06-25
**Status:** Proposed

## Context

Batching K tests into one forked child cuts the fork tax, but shares the child's address space across
those tests — so a test that mutates module/global/native state can contaminate its batch-mates. The
engine's correctness contract (and the content-addressed cache, ADR-E004) assumes each test's outcome
is a pure function of its inputs. We need batching's speed without breaking that.

## Decision

**Isolate per test by default; batch only tests the purity guard *proves* pure.** "Pure" = the test
does not mutate shared (module/session/global) or native state and does no unrecorded I/O/clock/RNG —
the same notion ADR-E004's `Purity`/`SandboxHooks` seam already names. Unknown ⇒ impure (conservative).
Batch size K is a tunable, validated by benchmark; a batch never mixes pure and impure tests.

## Alternatives considered

- **Batch everything (fixed K):** maximal speed, **unsound** — any shared-state mutator silently
  corrupts its batch. Rejected.
- **Batch by user `@pure` annotation only:** sound but low recall (devs won't annotate); keep as an
  *override*, not the primary mechanism.
- **Never batch (status quo):** safe, leaves the fork tax on the table after parallelism + ②.

## Rationale

Soundness is non-negotiable (it's why we fork at all, ADR-E003, and what the cache depends on,
ADR-E004). Conservative-by-default + a CI differential gate makes batching a pure win where it applies
and a no-op where purity can't be established.

## Consequences

- ➕ Fewer forks on pure-heavy suites, stacking with parallelism (pool) and ② (import-once).
- ➕ Isolation + cache soundness unchanged for everything not proven pure.
- ➖ Gated on a real **purity guard** (the deferred Phase-4 detector) — without it, nothing is
   batchable. That detector is the critical path; this feature is its first consumer.
- ➖ A misclassification is a correctness bug, so the differential gate is mandatory, not optional.

## Revisit trigger

If the purity guard's recall is too low (few tests provably pure) the speedup won't materialize —
revisit with frozen-clock / seeded-RNG / recorded-IO *pinning* (ADR-E004 soundness strategy step 3) to
make more tests batchable by removing their nondeterminism rather than excluding them.
