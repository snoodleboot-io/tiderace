# ADR-E016 — A work queue over recorded durations (scheduling without a forecast)

**Status:** ✅ **Implemented + measured.** Amends [ADR-E010](ADR-E010-locality-scheduler.md): the
grouping and ordering it describes stay; the static assignment it describes is replaced.

**Relates to:** [ADR-E010](ADR-E010-locality-scheduler.md) (the two objectives this still serves),
[ADR-E014](ADR-E014-no-fork-restore-ladder.md) (the verdict store this extends, and the "reading
only" contract it keeps), [ADR-E004](ADR-E004-content-addressed-cache.md) (why a duration is not a
verdict).

## Context

ADR-E010 bin-packs tests onto a fixed set of workers up front: group by snapshot scope, order groups
longest-first, assign each to the least-loaded bin. That needs a weight per test *before anything
has run*, and on a cold run the engine had exactly one: `1`.

The benchmark (TID-52) measured what that costs. On pirn-agents — 4,019 collected items whose
per-test cost spans four orders of magnitude, the slowest 1% being 63% of all test time — bins
balanced perfectly by *count* ran **121 / 97 / 66 / 34 / 31 / 25 / 23 / 19 seconds**: 2.32× the
makespan a perfectly balanced run would take, the machine 57% idle. pirn-core was 1.69×. The
execution tier was not the problem (3 demotions in 4,657 tests); the forecast was. pytest-xdist,
which forecasts nothing and distributes test-by-test, beat the engine on that suite and only there.

Two facts made this structural rather than a tuning problem:

- A parametrized item is **one** collected item however many cases it expands into at runtime. It
  weighed 1 while contributing the time of all of them.
- The verdict store persisted outcome, deps, purity and `must_fork` — **not duration**. There was
  no history to weight with even on a warm run, although every result carried one and the
  `--report` file already wrote it.

## Decision

**1. Drain a queue; do not commit a partition.** `Scheduler::units()` is a new seam, defaulting to
the static plan. The locality scheduler overrides it: one unit per module, heaviest first, sharded
only when a module is heavier than one perfect bin (otherwise a single-module corpus is one unit,
one worker, and seven with nothing to take). The runner hands those units to a fixed pool of
threads; a worker that finishes early takes the next unit. Locality is intact — a module still runs
entirely on one worker — and the only thing dropped is the part that needed a forecast.

**2. Record durations, and weight units by them.** `PersistedState.durations` maps each *reported*
node id to its last wall-clock ms. The scheduler charges a collected item the sum over every node
it expanded into (its own id, `id[…]`, `id::…`), so a 40-case test weighs like 40 tests. A cold
item weighs 1.

**3. `tiderace run` may write durations — and nothing else.** The verdict store's "reading only"
rule (ADR-E014) exists because a verdict can change an answer: a stale `pure` promotes a test to a
tier with no isolation. A duration cannot change an answer; it orders work, and a stale one costs a
little balance. So `run` loads the state file, updates the durations map, and saves it with every
other field untouched. It **never** creates a `TestRecord`: the impact planner treats a node with a
record and no changed deps as up to date, so a record written only to carry a duration would turn
the next impact-aware run into a stale pass. That is why durations are a separate map and not a
field on the record.

**4. The run header says what it learned.** `learned=3 forced-fork,4657 durations`. A pasted number
is uninterpretable without knowing whether the run was warm.

## Consequences

Measured, interleaved and load-gated (TID-52, PR #90 — the queue alone, cold order):

| suite | static partition | queue | vs xdist before → after |
| -- | -- | -- | -- |
| pirn-core | 34.7s | **27.9s** (1.24×) | 1.09× → **1.53×** |
| pirn-agents | 69.6s | **60.2s** (1.16×) | 0.77× → 0.81× |

The queue fixed *where* work goes. Recorded durations fix *what order* — on the same measured
numbers, 1.50× → 1.13× of the perfect-balance floor for pirn-agents (TID-62).

- Order is decided per worker at run time, so which worker runs which module is not deterministic
  across runs. It never was promised; see the non-goal on test-order independence (TID-70).
- `SubprocessWorker` now keeps its process for the worker's lifetime. It used to launch one inside
  every `run()`, invisible while a worker ran exactly one batch and one process per module under a
  queue.
- A daemon and a `run` writing the same state file race last-writer-wins. Acceptable for a hint;
  not acceptable for a verdict, which is another reason `run` writes nothing else.

## Alternatives considered

- **Better cold weights without history** — file size, test count per module, AST size. None
  correlate with a 20-second test in a 12-millisecond suite. Rejected.
- **Per-test work stealing (xdist's model).** Reaches the floor exactly on pirn-agents (1.00×) but
  scatters a module across workers, rebuilding its snapshot on each — the cost ADR-E010 exists to
  avoid, and on pirn-core the floor is set by one 18.9s test anyway. Whole-module units get most of
  the way there and keep locality; revisit if a suite shows a single module larger than the floor.
- **Durations on `TestRecord`.** Simpler; unsafe, for the impact-planner reason above.
- **Daemon-only recording.** Keeps `run` pure but leaves the benchmarked front end permanently
  cold. Rejected once writing durations was shown to carry no soundness weight.

## Revisit trigger

A suite where whole-module units leave the machine idle — a single module heavier than the perfect
bin that sharding cannot split (one enormous test) — is the case for per-test stealing inside a
unit. Or a measured second run that does not close the pirn-agents gap.
