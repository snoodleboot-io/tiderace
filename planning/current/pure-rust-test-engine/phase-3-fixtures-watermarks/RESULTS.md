# Phase 3 — Fixtures + Watermarks · Gate Results

> Verdict: **GO**. All work items W1–W15 delivered; all 9 ATDD acceptance scenarios green against the
> live `python + numpy + sqlite` stack; coverage 95.1% line / 88.6% fn; clippy `-D warnings` + fmt
> clean; zero stubs. Implemented on `feat/phase-3-fixtures-watermarks`
> (`17fd37b` → `28a311f` → `060aace`).

## Integration boundaries (§8) — all verified LIVE, no mocks

| # | Boundary | Result |
|---|----------|--------|
| 1 | Fork-from-Watermark with real fixtures across all scopes | ✅ engine per-fixture body counts match pytest **exactly** (`session_db`/`probe_dir`/`session_autouse` 1×, `pkg_resource`/`pkg_autouse` 1×, `module_fix`/`big_module_fix`/`warm_array` 1×, `class_fix` 2×, `func_fix` 3×, `parametrized[a/b/c]` 1× each) — the 1×-not-500× claim |
| 2 | Non-fork-safe resource re-initialized post-fork | ✅ each forked child opens a **fresh** in-memory sqlite connection (`reinit_after_fork__db_conn` body runs once per child = 2×); both sqlite tests pass — a corrupted inherited handle would error |
| 3 | No-COW `SubprocessWorker` result-identical to fork | ✅ `--no-fork` path outcomes identical to the fork path across the whole corpus |

## Coverage gate (G-C2 ≥80 line / ≥70 branch)

Full suite (lib + live integration): **95.13% line · 94.11% region · 88.57% fn** — PASS. Pure-logic
modules at/near 100% (`watermark_stack`, `fork_plan`, `fixture_plan`, `memory_governor` 99%,
`override_table` 99%, `fixture_graph` 95%, `layered_resolver` 95%).

## Enforcement gate

PASS. No stubs (`grep unimplemented!/todo!` → none in delivered code). No `unwrap/expect/panic!` in
Phase-3 lib code (all in `#[cfg(test)]`; the only non-test hits are pre-existing Phase-2
`regex_collector` const-regex compilation). `FixtureError` + `EngineError` are `thiserror`; one
public type per snake_case file; `cargo clippy --all-targets -D warnings` + `cargo fmt --check` clean.

## Performance gate — snapshot reuse

**Mechanism — PASS (proven by counts):** wider-than-function fixtures execute **once** and are
inherited by every forked child via COW; function fixtures run per test. The scope-count differential
(boundary 1) is the load-bearing proof — `big_module_fix` runs **1×** while its module has 500 tests.

**Wall-time (hyperfine, 8 runs, `fx_corpus`, warm):**

| Runner | Mean | Notes |
|--------|------|-------|
| pytest (in-process) | 0.85 s ± 0.01 | optimized single-process loop |
| tiderace fork path | 2.86 s ± 0.13 | 509 **sequential** `fork()`s; System 2.48 s = syscall-bound |

**Honest reading (not a regression):** on a corpus of *trivially cheap* fixtures + tests, per-test
`fork()` overhead dominates and the engine is ~3.4× slower than pytest **here**. The snapshot-reuse
wall-time advantage is **asymptotic** and appears when (a) fixture setup is expensive enough that 1×
amortization beats fork cost, and (b) the **Phase 6 scheduler** drives *concurrent*, `MemoryGovernor`-
bounded forks (Phase 3 forks strictly sequentially — no parallelism yet). The phase's claim is the
snapshot **layering correctness**, which is proven; a wall-time win on trivial sequential workloads is
not claimed. `MemoryGovernor` already emits the `max_concurrent_forks` bound Phase 6 will fan out on.

## Security gate — fork + live resource handles (STRIDE-focused)

| Concern | Finding |
|---------|---------|
| Handle leakage across `fork()` (E-2 hazard) | **Mitigated/verified.** `reinit_after_fork` resources (sqlite) are rebuilt per child; boundary 2 asserts the parent handle is never used in-child. Non-reinit fork-fragile resources require the explicit `reinit_after_fork` declaration (auto-detection deferred to Phase 5 per §9). |
| fd leakage on the result pipe | Child `close(read_fd)`; parent `close(write_fd)` then closes `read_fd` after read; `waitpid` reaps every child (no zombies). Timeout path `SIGKILL`s then `waitpid`s. |
| C-extension fork-from-warm (numpy) | Native thread pools pinned (`OPENBLAS/OMP/MKL=1`) before any fork; no fork+numpy crash observed across the full corpus — ADR-E003 revisit trigger **not** hit. |
| `--no-fork` runs test code in the worker process (no isolation) | **Accepted, documented.** Fallback-only path for fork-unsafe platforms; a crashing/hostile test takes down the worker. Test code is trusted input (the user's own suite), consistent with every test runner. |
| Memory-budget DoS (COW write amplification) | `MemoryGovernor` bounds concurrent forks by RSS budget (`min(cpu, budget/per_fork_est)`), seeded from `Watermark.rss_bytes`, refined from observed RSS — the Phase 6 fan-out throttle. |

No high/critical findings. One accepted low (`--no-fork` isolation), one cross-phase deferral
(auto-detect fork-fragile resources → Phase 5).

## Deferrals to later phases (not stubs — phase boundaries)

- Live execution discovers fixtures **in the shim** (reads `@pytest.fixture` markers) because no
  Python-introspecting collector feeds Rust `Fixture`s yet; the Rust `FixtureGraph`/`LayeredResolver`/
  `WatermarkStack` are unit- + pure-acceptance-tested and are the **frozen contract** Phases 4–6
  consume. Wiring a native discovery collector that feeds the Rust resolver into the live path is a
  later-phase seam (noted in CONTRACT.md).
- Test-level `@parametrize` expansion (one node → N results) → Phase 4. Fixture-level parametrization
  **is** executed (the shim fans a parametrized-fixture test into N forks internally, one node result).
- Concurrent fork fan-out + duration-aware scheduling → Phase 6 (`MemoryGovernor` input ready).
