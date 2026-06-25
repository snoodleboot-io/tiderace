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
| **pytest** | **0.91 s** | 1.0× |
| native (riptide, `--all`) | 3.27 s | 3.6× slower |
| old (tiderace) | 4.39 s | 4.8× slower |

Cold, on cheap tests, **pytest wins** — both Rust engines pay an isolation tax pytest doesn't (native
forks per test; old spawns pytest workers). **Native is ~26% faster than the old engine**, and its time
is almost all *system* (fork syscalls) vs old's *user* time (pytest worker overhead). See "what's
better" below — native runs these **sequentially in one wellspring**, leaving cores on the table.

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

1. **Cold full run of cheap tests:** pytest is fastest; per-test isolation is the cost. Native is ~26%
   faster than the old engine.
2. **Warm, no-change:** **native now wins (4.9 ms)** — 7× the old engine, 185× pytest. The impact-skip
   is wired into `run` (was the gap; now closed).
3. **Warm inner loop (1 changed test):** native ~5 ms vs pytest ~320 ms.

So native beats the old engine in **all three** scenarios; pytest only wins the cold-from-scratch full
run, on the strength of not isolating tests.

## Do we batch? What can we do better? (the per-test isolation tax)

**No batching today** — the native engine `fork()`s **once per test** (511 forks for fx_corpus), each a
pristine COW child (ADR-E003). That isolation is the cost in scenario 1. Two facts about *why* it's
3.27 s, and the levers:

1. **It's sequential.** The biggest lever isn't the fork itself — it's that `run_batch` executes tests
   **one at a time in a single wellspring**. We *built* a `LocalityScheduler` (LPT + scope-locality,
   Phase 6) but it is **not yet wired into the run loop**, and there's no multi-wellspring worker pool.
   The old engine already parallelizes (pytest workers across cores); native does not. **Parallelizing
   across N cores is the single biggest win** — it could take the 3.27 s toward ~pytest territory,
   independent of the fork cost. *(Highest-leverage next task.)*
2. **The subprocess + pipe control plane.** Each test is a fork + a JSON frame over a pipe. The **②
   in-process / FFI backend** (ticketed, ADR-E013) deletes that control plane — fork-from-embedded, no
   per-test subprocess/pipe — shaving the per-test overhead while keeping isolation.
3. **Batch *pure* tests per fork.** Fork once, run K independent tests in the child. This trades
   isolation for speed and is only sound for tests that don't mutate shared state — so it needs the
   **purity guard / SandboxHooks** (Phase-4/5 deferred items) to decide which tests are batchable.

In short: the fork tax is real, but the *sequential* execution is the larger, lower-risk win — wire the
already-built scheduler to a worker pool. ② and pure-batching are the follow-ons.
