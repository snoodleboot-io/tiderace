# pytest vs old engine vs native engine — three-way benchmark

> Reproduce: `benchmarks/bench_3way.sh [corpus] [python]`. Measured 2026-06-24 on this host, over
> `benchmarks/fixtures/fx_corpus` (509 fixture tests: numpy/sqlite). hyperfine 1.20, 8 runs.
>
> - **pytest** — `python -m pytest` (single process, no per-test isolation).
> - **old** — legacy `tiderace` (repo root): Rust orchestrator over **parallel pytest workers** +
>   SQLite impact analysis.
> - **native** — `riptide-daemon` (the `engine/` rebuild): own Rust engine, **fork-per-test** isolation
>   (ADR-E003), warm wellspring.

## Scenario 1 — cold full run (everything executes; all three pass the same tests)

| tool | mean time | vs pytest |
|---|---:|---:|
| **pytest** | **0.86 s** | 1.0× |
| native (riptide, parallel pool) | 1.17 s | 1.36× slower |
| old (tiderace, parallel) | 4.22 s | 4.9× slower |

**Now parallelized.** The native engine runs across a **pool of wellsprings (one per core)** — the
`LocalityScheduler` (LPT + scope-locality) wired to a multi-worker pool (`engine-daemon/src/pool.rs`).
That took the cold full run from **3.27 s (sequential) → 1.17 s (2.8× faster)**. Native is now only
**~1.36× behind pytest** (was 3.6×) and **3.6× faster than the old engine** (which also parallelizes,
via pytest workers, but pays pytest's overhead).

The residual gap to pytest is the **per-worker import**: each of the N wellsprings imports the project
(numpy here) once, so on this host the 8-way pool pays ~8× the import — visible as `native`'s high
total CPU (≈ user+sys 7 s over 1.17 s wall). That import multiplication, not the fork, is the remaining
cold-run cost — and exactly what ② (one shared embedded interpreter) removes.

## Scenario 2 — warm, no changes (re-run; impact analysis skips all) — **gap now filled**

| tool | mean time | vs pytest |
|---|---:|---:|
| **native (riptide, impact-skip)** | **4.9 ms** | **185× faster** |
| old (tiderace, impact-skip) | 33.8 ms | 27× faster |
| pytest (no warm mode) | 0.90 s | 1.0× |

`riptide-daemon run` is now **impact-aware** (`engine-daemon/src/persist.rs` + `run_impacted`): it
persists each test's coverage footprint + per-file content hashes (`.riptide-state.json`) and on re-run
executes only tests whose deps changed — and when nothing changed, it **doesn't even launch the
wellspring** (just collect + hash + serve cached). That's why native (4.9 ms) now **beats the old
engine (33.8 ms) by ~7×** — the old engine still does pytest-worker + SQLite setup. A source edit
re-runs only that file's dependents (verified: edit one module → 1 of 2 tests reran).

## Scenario 3 — inner loop: warm rerun of ONE changed test

| tool | time | vs pytest |
|---|---:|---:|
| **native (riptide), warm** | **~5 ms** | ~64× faster |
| pytest | ~320 ms | 1.0× |

## Takeaways (honest)

1. **Cold full run of cheap tests:** native (parallel pool) **1.17 s vs pytest 0.86 s (1.36×)** and
   **3.6× faster than the old engine** — was 3.6× *behind* pytest before parallelizing.
2. **Warm, no-change:** **native wins (4.9 ms)** — 7× the old engine, 185× pytest.
3. **Warm inner loop (1 changed test):** native ~5 ms vs pytest ~320 ms.

So native beats the old engine in **all three** scenarios and is now within ~1.4× of pytest even on the
cold-from-scratch full run — the one case pytest still leads, on the strength of not isolating tests
(one interpreter, no per-worker import).

## Do we batch? What can we do better? (the per-test isolation tax)

**No per-fork batching** — the native engine `fork()`s **once per test** (pristine COW child,
ADR-E003). The levers, in order of leverage:

1. ✅ **Parallelize across cores — DONE.** This was the big one: native ran tests *sequentially in one
   wellspring*. Now the `LocalityScheduler` (LPT + scope-locality) drives a **pool of wellsprings, one
   per core** (`engine-daemon/src/pool.rs`): cold full run **3.27 s → 1.17 s (2.8×)**, from 3.6× behind
   pytest to 1.36×.
2. ⏭ **② in-process / FFI backend** (ticketed, ADR-E013) — the residual cold-run gap is now the
   **per-worker import** (each of N wellsprings imports the project once → ~N× import). ② embeds **one**
   interpreter and forks from it, so the project is imported **once** and the subprocess/pipe control
   plane disappears — directly attacking what's left. *(Next.)*
3. ⏭ **Batch *pure* tests per fork** — fork once, run K independent tests in the child. Trades isolation
   for speed; only sound for tests that don't mutate shared state, so it needs a **purity guard /
   `SandboxHooks`** to decide which tests are batchable. *(After ②.)*

The sequential-execution win is banked. ② (import-once) and pure-batching (fewer forks) are the planned
follow-ons — see `planning/backlog/`.
