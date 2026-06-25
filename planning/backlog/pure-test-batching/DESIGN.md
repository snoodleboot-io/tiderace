# Design: Batch pure tests per fork

**Status:** Draft (gated on the purity guard) · **Related:** [PRD.md](PRD.md) · [ADR.md](ADR.md) ·
[ADR-E003](../../current/pure-rust-test-engine/design/adr/ADR-E003-fork-snapshot-isolation.md)

## Overview

Today each test is its own forked COW child (`engine/py-shim/shim.py` `_fork_run` → `_child_exec`).
This adds a **batched** path: a single forked child runs a *group* of **pure** tests in sequence, then
exits — so K pure tests cost one fork instead of K. Impure tests are untouched (one pristine fork each).
Purity is decided by the **purity guard** (the deferred Phase-4 detector); the scheduler packs pure
tests into batches and leaves impure tests solo.

## Affected modules

- **Purity guard (prerequisite, new):** a runtime detector that flags a test as impure if it mutates
  module/session/global or native state, or does I/O/clock/RNG. Feeds `cache::{Purity, SandboxHooks}`
  (the policy seam already exists, ADR-E004). Likely a shim-side observer (snapshot module dict / watch
  fs/clock/net) producing a per-test verdict, persisted alongside coverage in `.riptide-state.json`.
- `engine/py-shim/shim.py` — a `_batch_exec(node_ids)` that, in one forked child, runs each test's
  function-scope setup + body + teardown in sequence and returns per-node outcomes. (Wider-scope
  fixtures are already inherited from the warm parent; only function scope repeats per test.)
- `engine-core` scheduler / `engine-daemon` pool — group pure tests (by purity verdict, within a
  locality group) into batches of ≤ K node ids; emit batched exec requests for those, solo for the rest.
- Wire protocol — a batched request carries N node ids; the response carries N `(node, outcome, detail,
  coverage)`. Additive to the Phase-2/3 frame (`ExecRequest`/`ExecResponse`), same as coverage was.

## Data / schema changes

`.riptide-state.json` (persist.rs) gains a per-test **purity verdict** (pure | impure+reason), so the
scheduler can batch without re-deriving it every run. Additive.

## Implementation plan

1. **Purity guard** (own sub-task, gates the rest): detect shared-state mutation; conservative —
   *unknown ⇒ impure*. Differential: a guarded run's outcomes == an unguarded run's.
2. **Shim `_batch_exec`**: run K tests in one child; per-test setup/teardown; collect N results. Prove
   (no pytest) that batched outcomes == solo outcomes for pure tests.
3. **Scheduler/pool batching**: pack pure tests into batches of ≤ K (default tuned by benchmark);
   impure tests stay solo. Reuse the `LocalityScheduler` groups; batch *within* a group.
4. **Benchmark**: fork count + wall-clock on a pure-heavy corpus, batched vs solo; record in
   `benchmarks/RESULTS-3way.md`.

## Testing

- **Differential (soundness gate):** batched outcomes byte-identical to per-test outcomes across the
  acceptance corpus.
- **Contamination test:** a pair where test A mutates module state and test B reads it — the guard must
  mark them impure (or at least not co-batch) so B is unaffected.
- Unit tests for the batch-packing (pure → batched ≤ K; impure → solo).

## Risks

Soundness (PRD). Conservative default + the differential gate are the guardrails. The purity guard is
the hard, prerequisite piece — see [ADR.md](ADR.md).
