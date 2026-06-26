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

## Status (2026-06-25)

- ✅ **Purity guard built + proven** (`engine/py-shim/shim.py`, `proof_purity_guard.py`): per-test
  deep-copy snapshot of module globals + `os.environ`; in-place mutations caught; verdict surfaced as
  `pure: bool` (+ `impurity` reason). Gated by `RIPTIDE_PURITY`/`--purity`, default off.
- ✅ **Batching mechanism built + proven** (`run(force_no_fork=True)`, `proof_pure_batching.py`): pure
  tests run in-process — **200 pure tests 993 ms → 70 ms (14×)**, identical outcomes; an impure test run
  no-fork is still flagged (defense in depth).
- ⏭ **Daemon integration** (the product win): persist the purity verdict per test in
  `.riptide-state.json` (next to coverage deps); `run_impacted` routes proven-pure tests through
  `force_no_fork` and forks only impure ones.

## Future enhancements (ideas this unlocked)

1. **Drop the guard for *known*-pure tests** → the raw ~90× (not 14×). After the learning pass, a test
   recorded pure runs no-fork **without** re-snapshotting; re-verify periodically / when its deps
   change. (The 14×→90× gap is the guard's per-test `deepcopy(os.environ)` cost.)
2. **Snapshot/restore instead of fork for *impure* tests.** The guard already names exactly what a test
   mutates (`module global X`, `os.environ`). So for an impure test with a *small, known* footprint,
   save just those names before and restore after — **no fork**, near-free isolation. Only
   opaque/unbounded mutators fall back to fork. This could remove the fork from *most* tests, not just
   pure ones. (Soundness: footprint must be complete; this is ADR-E004's "pin the nondeterministic
   inputs" strategy applied to state.)
3. **Free-threading (PEP 703, CPython 3.13t/3.14t).** Pure tests are thread-safe by definition → run
   them on **threads** in one interpreter with no GIL: no fork **and** parallel across cores **and** one
   import — the trifecta. Needs the free-threaded build (`python3.14t`); the GIL build only parallelizes
   via processes. The purity guard is exactly the gate that says which tests are thread-safe.

## Risks

Soundness (PRD). Conservative default + the differential gate are the guardrails.
