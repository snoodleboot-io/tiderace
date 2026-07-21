# ROADMAP v2 — Two-track delivery (the re-cut)

> Supplements (does not replace) [ROADMAP.md](ROADMAP.md). It records the **re-cut** caused by the
> native-authoring decision ([ADR-E012](design/adr/ADR-E012-native-type-driven-authoring.md)) and the
> transport/in-process work ([ADR-E011](design/adr/ADR-E011-shim-transport-seam.md)), and turns the
> remaining work into **two tracks of detailed checklists**.
>
> **Last updated:** 2026-07-21 (see the 2026-07-21 addendum below; the 2026-06-24 summary is a snapshot). Trunk: `main_v2` (phases 1–3 merged); phases 4–7 + Track B delivered on
> `feat/n5-conformance`. Conformance data: `conformance/`. Native perf: `benchmarks/RESULTS-native.md`.

---

## Delivery summary (2026-06-24)

Both tracks are **core-complete**; the only open item is the ② in-process backend (a deliberate
side-bet — see the ticket [`planning/backlog/in-process-ffi-backend/`](../../backlog/in-process-ffi-backend/)).

- **Track A:** Phase 4 (RichDiff/async/unittest), Phase 5 (coverage→DepGraph→impact→cache), Phase 6
  (LocalityScheduler + **runnable warm daemon**: `EngineHandler` e2e, `tiderace watch`, `riptide-daemon`
  bin), Phase 7 (5 reporters, plugin host, Windows CI, measured perf). Purity guard + sandbox
  *interception* deferred (cache `Purity` seam exists).
- **Track B:** B1–B7 done — migrate conformance **70% → 89%** across 4 repos (anyio 99%, click 94%,
  flask 80%). B6 run-through: cachetools 215/215 = 100% through the engine.
- **Measured:** warm inner-loop rerun ≈ **7 ms vs pytest ≈ 650 ms (~90×)**; full cold runs of cheap
  tests are slower than pytest (fork-per-test isolation tax — the lever ② targets).
- **Health:** engine-core 118 lib + 2 integration + diff + 9 acceptance; engine-daemon 20 + 1 e2e;
  10 Python proofs; clippy -D + fmt clean.

---

## Addendum — delivered since (2026-07-21)

The summary above is a snapshot of 2026-06-24 and **understates the current state**; work continued
under Linear team `tidewire` (issues `TID-*`). What landed since, and what it changed:

- **No-fork isolation ladder** ([ADR-E014](design/adr/ADR-E014-no-fork-restore-ladder.md)) — now the *default*
  execution path, and the reason the "Purity guard" box above is checked: pure → bare no-fork (~90×),
  impure → no-fork + snapshot/restore (~5–14×), opaque → fork. Sound by construction (opaque forks).
- **Conditional sub-interpreter tier** ([ADR-E015](design/adr/ADR-E015-subinterp-tier.md), epic
  **TID-2**, PRs #6–#8) — not in the v2 plan at all. A *universal* sub-interpreter backend was spiked
  and rejected (numpy's `_multiarray_umath` refuses isolated sub-interpreters, taking pandas/scipy/
  torch with it). What shipped instead is a **detect-and-route hybrid**: TID-9 probes each module's
  sub-interpreter safety and caches the verdict; TID-10 adds `SubInterpWorker` (PEP 684 per-interpreter
  GIL); TID-11 routes the safe subset to that pool and the rest to fork. Purpose is **Windows**
  parallelism — Linux measures at parity (1.45 s vs 1.49 s) because the fork pool already parallelizes.
  The Windows payoff is *proven correct but unmeasured* — **TID-12**.
- **Cache** — remote/`DirCache` backend (TID-6) + live-loop wiring (TID-7), i.e. the two boxes checked
  above.
- **Migrate conformance** — **89% → 91%** (click 95%, flask 83%, anyio 99%; 352 mapped / 36 can't-map).
  The `70% → 89%` figures in the summary above are superseded.
- **Test-environment integrity** (PR #9) — live acceptance tests self-skipped when `.riptide-fx-venv`
  was absent, and libtest reports an early return as `ok`; `engine · linux` never provisioned that venv,
  so it had been running the whole live suite as no-ops. `RIPTIDE_REQUIRE_LIVE=1` now turns a
  live-scenario skip into a failure in both venv-provisioning CI jobs. Treat any pre-#9 "suite green"
  claim in this document with that caveat.

**Still open:** TID-3 (free-threading — blocked, needs a `python3.14t` build), TID-4 (② parallel
fork-out, below), TID-12 (Windows fallback + benchmark).

---

## 0. Mental model (how to read this)

Two tracks, different natures, gating adoption at **different times**:

- **Track A — Capability phases (the spine).** Planned, dependency-ordered (4→5→6→7). *"What can the
  engine do?"* The critical path to a product worth switching to.
- **Track B — Adoption / fidelity gaps (the surface).** Emergent, **measured by conformance**,
  cross-phase. *"Can a real pytest user switch and be happy?"* High-leverage, slotted opportunistically.

Early, **B** decides whether anyone *tries* it (auto-map %); later, **A** decides whether they *stay*
(impact analysis, watch). The native decision **absorbed** Phase-4 marks/params into Track B (done) and
**reconceived** Phase-7 pytest-compat as the migration codemod.

**Sequencing heuristic:** (1) cheap B-gaps that move the adoption number → now; (2) then drive the spine
at **Phase 5** (coverage/impact — the reason riptide exists); (3) run conformance continuously as the
tripwire back to B; (4) **②** in-process backend stays a parallel side-bet, never a blocker.

**Definition of Done (every item):** code + a focused test/proof (no pytest in native paths) + green
`cargo test`/`clippy -D warnings` where Rust is touched + (Track B) a measured conformance delta + a
one-line ADR/doc note if a decision was made.

---

## 1. Status snapshot

| Phase (Track A) | Status | Notes |
|---|---|---|
| 1 Fork/Wellspring spike | ✅ done | GO |
| 2 Workspace + domain + collection | ✅ done | |
| 3 Fixtures + watermarks | ✅ done | merged to `main_v2` |
| 4 Full styles + assertions | ✅ **done** | marks/`@cases` + RichDiff + async + unittest fidelity; **purity guard shipped** via the no-fork ladder (E014) — fs/clock/net interception still open |
| 5 Coverage + cache | ✅ **done** | coverage→DepGraph→impact + content-addressed cache; **live-loop wiring done** (TID-7) + remote `DirCache` (TID-6) |
| 6 Scheduler + daemon | ✅ **done** | LocalityScheduler + warm daemon (EngineHandler e2e, watch, `riptide-daemon` bin); inner loop ~7ms |
| 7 Compat + reporting + hardening | 🟢 **core done** | 5 reporters + plugin host + Windows CI + measured perf; further governor tuning iterative |

| Track B item | Status |
|---|---|
| Native surface (`@provides`/`@cases`/marks, type-DI) | ✅ N1–N4 |
| `riptide migrate` codemod + report | ✅ |
| Conformance harness (instrument) | ✅ |
| **Builtins (monkeypatch/tmp_path/…)** | ✅ done (click 70%→93%) |
| **Type-inference for untyped fixtures (B3)** | ✅ done (total 79%→85%) |
| **Corpus breadth (B7: +Flask +anyio, 4 repos)** | ✅ done |
| **Run-through tier (B6)** | 🟢 harness + cachetools 215/215 100%; migrated pytest repos pending venv |
| **usefixtures (B2)** | ✅ done (capability; corpus-neutral — untyped targets) |
| **request introspection (B4)** | ✅ done (decision; anyio 89%→99%) |
| **provider-params (B5)** | ✅ done (anyio→99%); async providers remain |
| **Migrate conformance** | **91% across 4 repos** (anyio 99%, click 95%, flask 83%) — TID-8 |
| ② in-process/FFI backend | 🟡 isolation ADR ratified (E013); `engine-inproc` built, benchmark refuted the transport premise; parallel fork-out = TID-4 (Backlog) |
| **Sub-interpreter tier (E015)** | ✅ **done** — detect (TID-9) → `SubInterpWorker` (TID-10) → route (TID-11); Windows benchmark = TID-12 |

---

## 2. Track A — Capability phases (remaining)

### Phase 4 — Full styles + assertions  🟢 **core delivered (2026-06-21)**
*Designs: [09-assertions](design/09-assertions.md), [10-test-styles](design/10-test-styles.md); [ADR-E009](design/adr/ADR-E009-lazy-assertion-introspection.md). Proofs: `proof_n7_assertions.py`, `proof_n8_async_unittest.py`.*

- [x] Native parametrization — `@riptide.cases` through the fork engine
- [x] Native marks — `@skip`/`@skip_if`/`@xfail`(+strict)/`@tag`, shim-honored
- [x] **Lazy assertion introspection + RichDiff** (the big one) — ADR-E009
  - Failing `assert` re-evaluated once in the live frame → operand source + values + element/line/key diff
  - Lazy: passes cost nothing; purity guard (double-eval) falls back on side-effecting/non-reproducing asserts
  - Done: failing `assert a == b` reports operands + a diff (`proof_n7`); ⏳ structured `RichDiff` Rust type + reporter wiring lands with Phase 7 reporters (currently rendered into `detail`)
- [x] **Async tests** — `async def test_*` driven on a per-test event loop (`proof_n8`); async providers deferred to Track B (B5)
- [x] **unittest fidelity** — `setUpClass`/`tearDownClass` honored; `@expectedFailure`→xfail, unexpected-success→failed, `subTest` failure→failed (`proof_n8`)
- [x] **Purity guard** — shipped as the gate of the no-fork isolation ladder ([ADR-E014](design/adr/ADR-E014-no-fork-restore-ladder.md)), not as the ADR-E004 stage-2 sandbox originally scoped here.
  - Done: static AST pre-filter + runtime shared-state-mutation detection (`shim.py`), tri-state verdict persisted per test (`TestRecord.pure`, **TID-1**) → known-pure tests take the bare no-fork tier; `proof_purity_guard.py`, `proof_static_purity.py`, `proof_trusted_pure.py`
  - ⏳ still open: fs/clock/network *interception* (ADR-E004 stage 2). Purity here is about state mutation; a test that reads the clock or network is still trusted. Conservative-by-default holds for those.

### Phase 5 — Coverage + cache  🟢 **core delivered (2026-06-21)**
*Designs: [07-cache](design/07-cache.md), [11-coverage-impact](design/11-coverage-impact.md), [13-cross-cutting](design/13-cross-cutting.md); [ADR-E004](design/adr/ADR-E004-content-addressed-cache.md), [ADR-E006](design/adr/ADR-E006-coverage-sys-monitoring.md). Consumes Phase-3 `ClosureHash`.*
*Commits: `4b83948` (coverage→DepGraph→Impact), `7115a05` (cache). Proof `proof_n6_coverage.py`; integration `tests/cache_impact_integration.rs`.*

- [x] **Coverage via `sys.monitoring`** (3.12+) with `settrace` fallback (≤3.11)
  - Per-test line coverage captured in the shim child; streamed to Rust (additive `coverage` wire field)
  - Done: per-test covered-line sets recorded (`proof_n6`); ⏳ remaining: differential vs `coverage.py` on the corpus + flip capture default-on
- [x] **`DepGraph`** — file → tests that touch it (built from coverage); forward + reverse edges, re-record supersedes
- [x] **`ImpactAnalyzer`** — select tests by changed files × DepGraph (line-level; supersedes file-only legacy `impact.rs`)
  - Done: warm run with no changes skips all; one change re-runs only impacted (unit + integration test)
- [x] **Content-addressed cache** — `CacheKey` over closure (ClosureHash + source-content hash + coverage closure + env)
  - `Cache` trait (ADR-E005 seam), `TieredCache(Local, Remote)`, `LocalCache`, `NullCache`
  - Done: identical inputs → hit; changed source/closure/env → miss (15 unit tests)
- [x] **`SandboxHooks` / `Purity`** — impurity policy seam; impure tests excluded from caching with a reason
  - Done: `Purity::impure(reason)` is never cached; `NoSandbox` default trusts the coverage closure
  - ⏳ remaining: actual fs/clock/network *interception* collector (ADR-E004 stage 2, conservative-by-default holds until then)
- [x] **Live-loop wiring** — cache consult (hit→impact-skip→run) in the daemon run loop + source content hashing + DepGraph persistence (**TID-7**, PR #3)
  - Done: `EngineHandler` consults the `DirCache` before impact-skip (`engine_handler.rs`); `warm_run_is_a_cache_hit_then_an_edit_invalidates_only_the_impacted_test` + `impure_test_is_never_cached` prove hit→skip→run and the purity gate

### Phase 6 — Scheduler + daemon  🟢 **delivered**
*Designs: [06-scheduler](design/06-scheduler.md), [08-daemon](design/08-daemon.md); [ADR-E007](design/adr/ADR-E007-warm-daemon.md), [ADR-E010](design/adr/ADR-E010-locality-scheduler.md). Consumes Phase-3 `Watermark.rss_bytes` via `MemoryGovernor`.*

- [x] **`LocalityScheduler`** — duration-aware LPT balancing + scope-locality (5 tests; makespan ≤ round-robin on uneven durations; a module co-locates; dominant group splits)
- [x] **FS watch + invalidation** — `engine-daemon`: content-hash `Invalidator` (conftest/config/C-ext recycle; test→recollect; source→impact; identical bytes→no-op) + `notify`-backed `FsWatcher` + noise-filtering `Debouncer`
- [x] **Warm daemon** — full + runnable: `Session` (invalidation→impact→cache→`ChangeOutcome`); RPC protocol + `serve_connection` + `serve_unix_socket`; **`EngineHandler` over a warm reused wellspring** (e2e-proven: discover/run real Python, warm); `riptide-daemon` binary (run/serve/watch/bench)
- [x] **`tiderace watch`** native mode — `react_to_change` (edit→minimum-rerun, unit-tested) + `watch_loop` (FsWatcher→debounce→react) + `riptide-daemon watch`. Measured warm rerun ≈ **7 ms** vs pytest ≈ 650 ms ([RESULTS-native.md](../../../benchmarks/RESULTS-native.md))

### Phase 7 — Reporting + hardening (compat → migration)  🟡 **reporters done**
*Designs: [12-plugin-host](design/12-plugin-host.md), [13-cross-cutting](design/13-cross-cutting.md); [ADR-E008](design/adr/ADR-E008-cross-platform.md). Note: "pytest-compat layer" is **replaced** by Track B migration.*

- [x] **Reporters** — terminal + JUnit XML + JSON + GitHub annotations + SARIF, all behind the `Reporter` seam (8 tests; each validated against its consumer's shape)
- [x] **Plugin host** — `hooks::HookHost`: registers `Hook` plugins, dispatches typed `HookEvent`s by static call (no `pluggy`), `Priority`+stable order resolved once (2 tests: a sample plugin observes all events; priority ordering). `PyPluginAdapter` (Python-plugin FFI bridge) deferred to ②.
- [x] **Conformance suite** (B6) — `conformance/runthrough.py` runs a suite **through the engine** vs an oracle; cachetools 215/215 = 100%. ⏳ extend to the migrated pytest repos (needs per-repo venvs)
- [x] **Perf hardening (measured)** — three-way pytest vs old vs native ([benchmarks/RESULTS-3way.md](../../../benchmarks/RESULTS-3way.md), `bench_3way.sh`): native **beats the old engine in all 3 scenarios** and is within **1.36×** of pytest on the cold full run after two wins this cycle — (a) **impact-aware `run`** (warm no-change **4.9 ms**, 7× the old engine) and (b) **parallel wellspring pool** (`LocalityScheduler`→pool: cold **3.27 s→1.17 s**, 2.8×). Inner loop ≈ **5 ms vs pytest ≈ 320 ms**. ⏭ remaining perf levers (re-ranked after the ② benchmark, which found **`fork()`/test ≈ 4 ms is the real cost**, not the transport — [RESULTS-inproc.md](../../../benchmarks/RESULTS-inproc.md)): **#1 [pure-test batching](../../backlog/pure-test-batching/)** (fewer forks — directly hits the cost), **#2 [② parallel-fork-from-embedded](../../backlog/in-process-ffi-backend/)** (import-once + parallel; the transport swap alone is *not* a win)
- [🟢] **Windows validation** — `engine-windows` CI job added (`.github/workflows/ci.yml`): `windows-latest` builds the engine workspace + runs clippy/fmt + `cargo test --all` (pure-Rust unit/lib/daemon pass; fork integration self-skips without the venv). Engine compiles cross-platform (only `cache_key` has unix code, with a `cfg(not(unix))` fallback). ⏳ remaining: the no-fork `SubprocessWorker` *acceptance* against a real Python on Windows (drive the `--no-fork` shim) — and confirming the job green on its first CI run

---

## 3. Track B — Adoption / fidelity gaps

### B1 — Native builtin resources  ✅ **done (2026-06-21)**
*Conformance: builtins were **77%** of click's can't-map; `monkeypatch` 21 + `tmp_path` 4 = 76% of those.*
*Delivered: `engine/py-riptide/riptide/builtins/`; proof `proof_n5_builtins.py`; [ADR-E012](design/adr/ADR-E012-native-type-driven-authoring.md) B1 note.*

- [x] `riptide.builtins.monkeypatch` — `@provides`-style, function-scoped, **with teardown** (undo on yield-exit)
  - API: `setattr`/`delattr`/`setitem`/`delitem`/`setenv`/`delenv`/`syspath_prepend`/`chdir`; injected as `mp: MonkeyPatch`
  - Done: proof shows env/attr mutation isolated + teardown restores (no pytest), through the real shim
- [x] `riptide.builtins.tmp_path` — function-scoped `TmpPath(pathlib.Path)` to a fresh temp dir (cleaned on teardown)
- [x] `capsys` / `capfd` — `Capsys`/`Capfd` capture providers returning a `.readouterr()` `CaptureResult`
- [x] `tmpdir` — legacy alias mapped by `migrate` to `TmpPath` (with a py.path caveat)
- [x] **Teach `migrate`** to map these builtins to the riptide providers (stop flagging them)
  - Done: re-ran conformance → **click auto-map 70% → 93%** (can't-map 43→10; entire builtin bucket eliminated)
  - **Decision:** builtins injected by *distinct* types (not bare `pathlib.Path`) to keep type-DI unambiguous

### B2 — `usefixtures` handling  ✅ **done (2026-06-22)**  *(capability shipped; corpus bucket gated upstream — see note)*
*Proof `proof_b2_uses.py`.*
- [x] Native `@riptide.uses(Provider)` — by type; the shim sets the provider up (and tears it down) in the closure without injecting it
- [x] `migrate`: `@pytest.mark.usefixtures("x")` → `@riptide.uses(<TypeOfX>)` when the referenced fixture's type is known; flag otherwise
  - **Honest finding:** the corpus's usefixtures bucket did **not** shrink — click's 6 all reference *untyped* fixtures, so they're blocked upstream by inference precision (B3), not by usefixtures support. The capability is delivered + proven; the bucket clears once those fixtures become typeable.

### B3 — Migration type-inference for untyped fixtures  ✅ **done (2026-06-21)**  *(was 65% of gaps across 4 repos)*
*Proof `proof_b3_inference.py`; measured TOTAL 79%→85%, Flask 66%→79%.*
- [x] In `migrate`, infer a provider's type from its body (`return X()` / `yield X()`, resolving one level through a local assignment) when annotation absent
- [x] Emit the inferred annotation (`-> X`) instead of flagging, when confident; flag when not
  - Done: untyped-provider + untyped-fixture-param buckets shrank (Flask 25→19 / 27→10); precision-tested — lowercase factories / bare names / conflicting returns never mis-annotated

### B4 — `request` introspection  ✅ **done (2026-06-23)**  *(decision in [ADR-E012](design/adr/ADR-E012-native-type-driven-authoring.md))*
- [x] Decided per case: `request.param` → **supported** (B5); `request.getfixturevalue` → **permanent** can't-map (dynamic name lookup); other `request.*` → manual port. No broad `Request` object (revisit trigger recorded).
  - Done: `migrate` stops flagging `request.param` on `params=` providers → anyio **89%→99%**, total **87%→89%**

### B5 — Async + provider-level params  ✅ **done (2026-06-23)**
*Proofs `proof_b5_provider_params.py`, `proof_b5_async_providers.py`; measured TOTAL 85%→87%, anyio 80%→89%.*
- [x] Async providers (`async def @provides`, coroutine or async-gen w/ teardown) — set up + torn down on the **same event loop** as the (async or sync) body, wired by type; function-scope (wider-scope async is the documented edge). Sync hot path untouched.
- [x] Provider-level parametrization — `@riptide.provides(params=[...])` fans the test out (value via `request.param`); `migrate` carries `params=` over instead of flagging
  - Done: proof shows fan-out across params + worst-wins aggregation; the **parametrized-fixture can't-map bucket cleared** (anyio 8→0, total can't-map 61→52)

### B6 — Migration **run-through-engine** tier  🟢 **harness + first repo done (2026-06-21)**
*`conformance/runthrough.py`; first target cachetools.*
- [x] Run a suite through the shim/engine and diff per-test outcomes vs an oracle → **execution pass-rate**
- [x] First repo (cachetools, pure unittest, no migration needed): **215/215 = 100%** match vs the stock-unittest oracle; zero divergences (validates Phase-4 unittest fidelity end-to-end)
  - ⏳ remaining: the **migrated pytest** suites (click/flask/anyio) need a per-repo venv + deps install; pointing the harness at them is the continuous next step (will surface engine gaps to file)

### B7 — Conformance corpus breadth  ✅ **done (2026-06-21)**
- [x] Added a fixture-heavy **app** suite (Flask `3.0.3`) and an **async** lib (anyio `4.4.0`) to `manifest.tsv` (pinned SHAs)
  - Done: can't-map distribution re-measured across **4 repos** (83 files); re-ranked the gaps (→ B3, now done)

---

## 4. The side-bet — ② in-process / FFI backend
*[ADR-E011 ②](design/adr/ADR-E011-shim-transport-seam.md); spike was GO (`spike-inproc/` since disposed — evidence in the [ticket](../../backlog/in-process-ffi-backend/DESIGN.md) + git history). Independent of Tracks A/B — rides the `ShimTransport` seam.*

- [x] **Isolation design** — ratified [ADR-E013](design/adr/ADR-E013-inprocess-isolation.md): **fork-from-embedded**. Per-test reset/subinterpreters parked.
- [x] **`InProcessTransport: ShimTransport`** — built in `engine/crates/engine-inproc` (PyO3 0.26 + embedded CPython 3.14; excluded crate, default/Windows builds untouched). Imports once, drives the executor by FFI, **fork-from-embedded isolation proven** (mutation in a forked child doesn't leak).
- [x] **Benchmark — done, and it REFUTED the premise** ([RESULTS-inproc.md](../../../benchmarks/RESULTS-inproc.md)): the pipe/transport is **not** the bottleneck — 500 trivial tests run **identically** in-process (~2.0 s) vs subprocess+pipe (~2.0 s); the cost is **`fork()` per test (~4 ms)**. ② as a transport swap is **not a perf win**.
- [ ] ⏭ **Parallel fork-out from the one embedded interpreter** — the *actual* win (import-once **+** parallelism, vs the pool's N× import). Re-scoped on the [ticket](../../backlog/in-process-ffi-backend/). `PyConfig`/venv plumbing + broader C-ext smoke fold in here. **Tracked as TID-4 (Backlog, Low)** — marginal wall-clock by its own analysis; the real gain is CI CPU-efficiency, ~zero warm.

---

## 5. Recommended order (across both tracks)

1. **B1 builtins** (monkeypatch → tmp_path → capsys/capfd) — cheap, lifts adoption 70%→~95%.
2. **Phase 5** coverage + cache — the spine's load-bearing segment (impact analysis = the pitch).
3. Interleave **Phase 4 remainder** (RichDiff assertions, async) with 5, as the roadmap allows.
4. **B6/B7** conformance run-through + corpus breadth — continuously, as the tripwire.
5. **Phase 6** scheduler/daemon → **Phase 7** reporting/hardening.
6. **②** in-process backend — parallel, on its own clock; never blocks 1–5.
