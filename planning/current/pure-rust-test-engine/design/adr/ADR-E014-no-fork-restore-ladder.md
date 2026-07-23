# ADR-E014 — No-fork + restore: the isolation ladder (default execution path)

**Status:** ✅ Accepted & **implemented + measured** (not design-only). Extends
[ADR-E003](ADR-E003-fork-snapshot-isolation.md).

**Relates to:** [ADR-E003](ADR-E003-fork-snapshot-isolation.md) (fork-from-warm-wellspring isolation —
this ADR makes the fork *conditional*), [ADR-E004](ADR-E004-content-addressed-cache.md) (purity is the
cache-soundness gate; the verdict recorded here feeds it), [ADR-E006](ADR-E006-coverage-sys-monitoring.md)
(coverage rides the same in-process run), [ADR-E013](ADR-E013-inprocess-isolation.md) (the ② backend
still forks; this ladder applies under the pipe transport).

## Context

[ADR-E003](ADR-E003-fork-snapshot-isolation.md) isolates each test with `fork()` from a warm wellspring.
Profiling the built engine showed `fork()` itself — not the transport, not the body of a cheap test — is
the dominant per-test cost (~4.5 ms; an in-process no-fork run is ~0.05 ms, ~90×). We `fork()` to keep one
test from corrupting another's view of process-global state (module globals, `os.environ`, native state).
But **most tests don't mutate shared state at all** — for them the fork buys nothing.

pytest sidesteps this by running everything in one process with *no* isolation (fast, but order-dependent
bugs are real); `pytest-forked` forks everything (safe, slow). Neither *knows* which tests need isolation.
The question this ADR answers: can the engine pay for isolation **only where a test actually needs it**,
without sacrificing soundness?

## Decision

**Make execution a per-test ladder; fork only when nothing cheaper is sound.** No-fork + snapshot/restore
is the **default** path (no user flag). The shim picks one of three tiers per test:

1. **bare no-fork** — test is *known pure* (a recorded purity verdict): run in-process, no snapshot.
   ~0.05 ms (90×). Verdict-gated optimization.
2. **no-fork + restore** — footprint is *restorable* (module globals + `os.environ` are deep-copyable):
   snapshot before the body, run, **restore** after (re-set changed globals, drop added ones, restore
   env). ~0.4–0.9 ms (5–14×). The purity guard verifies and records a verdict for next time.
3. **fork** — module has *opaque* (un-deep-copyable) globals, or the static pre-filter flags obvious
   shared-state mutation: fall back to ADR-E003 COW fork. ~4.5 ms (1×).

Supporting mechanisms (all in `py-shim/shim.py`):

- **Static pre-filter** (`static_impurity`) — an AST scan flagging obvious mutators (`global`, writes to
  free/module names, `os.environ`/`os.chdir`/`random.seed`-style calls) **without running**. A sufficient,
  conservative impurity signal: a false "impure" only costs a fork.
- **Snapshot/restore** (`_snapshot_shared` / `_restore_shared`, `_restorable`) — the tier-2 engine.
- **Purity guard** (`_purity_verdict`) — records whether a test actually mutated shared state; the verdict
  promotes a test to tier 1 and gates cache eligibility (E004).

The daemon turns this on by default: `TIDERACE_RESTORE=1` + `force_no_fork` on every `ExecRequest`; the
shim downgrades to fork where unsound. `TIDERACE_FORCE_FORK=1` reverts to fork-per-test — a debug/benchmark
escape, **not** a user-facing flag.

## Why (the trade-off)

| | **Ladder (chosen)** | fork-everything (E003 only) | no-isolation (pytest-style) |
|---|---|---|---|
| Pure test | no-fork, ~0.05–0.9 ms | fork, ~4.5 ms (wasted) | in-process, fast |
| Impure (bounded) test | no-fork + restore, isolated | fork, isolated | **contaminates neighbours** |
| Impure (opaque) test | fork, isolated | fork, isolated | contaminates neighbours |
| Knows which is which | ✅ static + runtime verdict | — | ❌ |
| Soundness | ✅ (contains mutation; opaque → fork) | ✅ | ❌ |

**Soundness is by construction, not by prediction.** Tier 2 *undoes* mutation rather than betting a test
is pure; a non-restorable module always forks. So a wrong/absent purity verdict can never cause
cross-test contamination — it only changes *speed*. This is why the ladder is safe to enable by default
and needs **no learning pass** (the first run is already correct; verdicts accrue as a free side effect).

## Consequences

- ➕ Most tests stop paying the fork. Measured (fx_corpus, 509 tests, cold full run): default no-fork
  **0.66–0.84 s** vs fork-per-test **1.07–1.20 s** (~1.6×), with fork *syscall* time **3.6 s → 0.6 s**.
  The no-fork suite **beats pytest cold** (~1.42×). Bigger fraction in warm/serve mode.
- ➕ Recorded purity verdicts double as the [E004](ADR-E004-content-addressed-cache.md) cache-soundness
  gate — one mechanism, two uses.
- ➖ Tier-2 pays a per-test deep-copy of module globals + env. Negligible for real test bodies; for suites
  of *many trivial* tests it's the gap to tier 1 (hence the verdict optimization).
- ➖ Restore covers module globals + `os.environ`. Mutation through opaque values or outside that
  footprint is handled by the opaque → fork fallback, not by restore.
- ➖ One more axis of behaviour to keep sound; covered by `proof_snapshot_restore.py`,
  `proof_static_purity.py`, `proof_purity_guard.py`, `proof_pure_batching.py`.

## Alternatives considered

- **Predict purity statically, then no-fork the "pure" ones.** Rejected as the *sole* mechanism: purity
  is undecidable in general (dynamic dispatch, C-ext, reflection), so a static "pure" verdict is unsound.
  Static analysis is kept only for the *conservative* direction (prove impurity → fork).
- **Persist verdicts and require a learning pass.** Rejected as a *requirement*: restore is sound on run
  one without any verdict. Persistence remains a future optimization (promote to tier 1).
- **Free-threading / "pure tests on threads."** Rejected: running batched tests on real threads races on
  the shared module dict that restore depends on — unsound. The sound parallel-in-process primitive is
  sub-interpreters (PEP 684), gated on C-ext support; tracked separately, not part of this ladder.

## Revisit trigger

Revisit if (1) the tier-2 snapshot cost becomes material on real workloads (→ build verdict persistence
to promote pure tests to tier 1), or (2) sub-interpreter + free-threading ecosystem support matures enough
to make a no-fork *parallel* tier sound (→ a new tier above fork).
