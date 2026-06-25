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
| **pytest** | **0.87 s** | 1.0× |
| native (riptide) | 3.83 s | **4.4× slower** |
| old (tiderace) | 4.18 s | 4.8× slower |

Cold, on cheap tests, **pytest wins** — both Rust engines pay an isolation tax pytest doesn't (native
forks per test; old spawns pytest workers). **Native is ~9% faster than the old engine** and spends its
time differently: native is almost all *system* time (fork syscalls), old is more *user* time (pytest
worker overhead).

## Scenario 2 — warm, no changes (re-run after a clean run; impact analysis should skip)

| tool | mean time | note |
|---|---:|---|
| **old (tiderace, impact-skip)** | **~32 ms** | impact analysis skips all unchanged tests — **26× faster than pytest** |
| pytest | ~0.82 s | no warm mode — re-runs everything every time |
| native (riptide) | ~3.7 s | ⚠️ `run`/`bench` **execute all** — see gap below |

**Honest gap:** the native engine's impact-skip + cache **exist** (the daemon's `Session`/`react_to_change`)
but are only wired into **`watch`** mode, *not* the one-shot `run`/`bench` CLI — so a one-shot native
re-run still executes everything. The **old engine's impact analysis is wired into its main run path**,
which is why it crushes this scenario (32 ms). Closing this — consult impact/cache in `run` — is the
clearest native follow-up.

## Scenario 3 — inner loop: warm rerun of ONE changed test (the daemon's pitch)

| tool | time | note |
|---|---:|---|
| **native (riptide), warm** | **~5 ms** | warm wellspring + one fork |
| pytest | ~260 ms | full interpreter + collection startup, every time |

Warm, the native daemon reruns a single test in **~5 ms vs pytest's ~260 ms (~50×)** — the sub-100ms
edit→result loop. (The old engine's warm pool also serves a single impacted test quickly; its strength
is the same warm + impact idea, which the native rebuild reimplements from scratch in Rust.)

## Takeaways (honest)

1. **Cold full run of cheap tests:** pytest is fastest; the per-test isolation both Rust engines provide
   is the cost. Native edges the old engine by ~9%.
2. **Warm, no-change:** the **old engine wins decisively (32 ms)** because its impact analysis runs in
   the main path; the native engine has the machinery but only behind `watch` — a wiring gap, not a
   capability gap.
3. **Warm inner loop (1 changed test):** native ~5 ms vs pytest ~260 ms — the rewrite's headline.
4. The fork-per-test tax (scenario 1) and the impact-in-`run` gap (scenario 2) are the two concrete
   levers left: ② (in-process backend) targets the former; wiring impact/cache into `run` targets the
   latter.
