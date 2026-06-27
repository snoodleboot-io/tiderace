# pytest vs old engine vs native engine — three-way benchmark

> Reproduce: `benchmarks/bench_3way.sh [corpus] [python]`. Measured 2026-06-26 on this host, over
> `benchmarks/fixtures/fx_corpus` (509 fixture tests: numpy/sqlite). hyperfine 1.20, 8 runs.
>
> - **pytest** — `python -m pytest` (single process, no per-test isolation).
> - **old** — legacy `tiderace` (repo root): Rust orchestrator over **parallel pytest workers** +
>   SQLite impact analysis.
> - **native (fork)** — `riptide-daemon run --all`: own Rust engine, **fork-per-test** isolation
>   (ADR-E003), parallel pool of warm wellsprings.
> - **native --fast** — `riptide-daemon run --fast`: optimistic **no-fork + snapshot/restore** (impure
>   tests run in-process and have their mutation undone; opaque modules auto-fork for soundness).

## Summary — warm + cold at a glance

| scenario | pytest | tiderace (old) | native (fork) | **native --fast** |
|---|---:|---:|---:|---:|
| **Cold** — full run (all 509 execute) | 0.94 s | 7.56 s | 1.07 s | **0.66 s** |
| **Warm** — no changes (impact-skip) | 0.84 s | 37.7 ms | 9.4 ms | **9.4 ms** |
| **Warm** — inner loop, 1 changed test | 0.27 s | — | ~5 ms | **~5 ms** |

`--fast` (no-fork+restore) only helps when tests **actually execute** — the cold/full-run row, where it
beats pytest 1.42×. In the warm rows impact-skip runs nothing (or one cheap test), so fork vs no-fork is
in the noise; those were already won by impact-skip + the warm daemon. Read `--fast` as the
**cold-run / large-changeset** lever, not a warm-loop one.

## Scenario 1 — cold full run (everything executes; all four pass the same tests)

| tool | mean time | vs pytest |
|---|---:|---:|
| **native --fast (no-fork+restore)** | **0.66 s** | **1.42× faster** |
| pytest | 0.94 s | 1.0× |
| native (fork, parallel pool) | 1.07 s | 1.14× slower |
| old (tiderace, parallel) | 7.56 s | 8.1× slower |

**`--fast` now wins the cold full run** — 1.42× faster than pytest, 1.63× faster than the fork path, and
11.5× faster than the old engine. Removing the `fork()` per test (the ~4.5 ms cost) drops System time
from **3.59 s → 0.54 s (6.6× fewer syscalls)**; the snapshot/restore that replaces it is cheap and keeps
full per-test isolation (`engine-daemon run --fast`, `shim.Engine(restore=)`).

The fork path's residual gap to pytest was the **per-worker import** (each of the N wellsprings imports
numpy once → ~N× import, visible as high total CPU). `--fast` doesn't remove that import cost but, by
deleting the fork, still comes out ahead of pytest. The import multiplication is what ② (one shared
embedded interpreter) — or a `SubInterpWorker` — would remove next.

## Scenario 2 — warm, no changes (re-run; impact analysis skips all) — **gap now filled**

| tool | mean time | vs pytest |
|---|---:|---:|
| **native (riptide, impact-skip)** | **9.4 ms** | **89× faster** |
| old (tiderace, impact-skip) | 37.7 ms | 22× faster |
| pytest (no warm mode) | 0.84 s | 1.0× |

`riptide-daemon run` is now **impact-aware** (`engine-daemon/src/persist.rs` + `run_impacted`): it
persists each test's coverage footprint + per-file content hashes (`.riptide-state.json`) and on re-run
executes only tests whose deps changed — and when nothing changed, it **doesn't even launch the
wellspring** (just collect + hash + serve cached). That's why native (4.9 ms) now **beats the old
engine (33.8 ms) by ~7×** — the old engine still does pytest-worker + SQLite setup. A source edit
re-runs only that file's dependents (verified: edit one module → 1 of 2 tests reran).

## Scenario 3 — inner loop: warm rerun of ONE changed test

| tool | time | vs pytest |
|---|---:|---:|
| **native (riptide), warm** | **~4–6 ms** | ~50–70× faster |
| pytest | ~270 ms | 1.0× |

## Takeaways (honest)

1. **Cold full run of cheap tests:** native **`--fast` now beats pytest** — **0.66 s vs 0.94 s (1.42×)**,
   1.63× faster than its own fork path, 11.5× faster than the old engine. The win is deleting the
   `fork()` per test (System time 3.59 s → 0.54 s) while keeping isolation via snapshot/restore.
2. **Warm, no-change:** **native wins (9.4 ms)** — 4× the old engine, 89× pytest (impact-skip: nothing
   re-runs, the wellspring isn't even launched).
3. **Warm inner loop (1 changed test):** native ~4–6 ms vs pytest ~270 ms (~50–70×).
4. **Remaining cold-run cost is per-worker import**, not the fork — addressed next by one shared
   interpreter (② / a `SubInterpWorker`, PEP 684), with free-threading to scale it across cores.

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
