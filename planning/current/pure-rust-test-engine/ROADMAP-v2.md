# ROADMAP v2 — Two-track delivery (the re-cut)

> Supplements (does not replace) [ROADMAP.md](ROADMAP.md). It records the **re-cut** caused by the
> native-authoring decision ([ADR-E012](design/adr/ADR-E012-native-type-driven-authoring.md)) and the
> transport/in-process work ([ADR-E011](design/adr/ADR-E011-shim-transport-seam.md)), and turns the
> remaining work into **two tracks of detailed checklists**.
>
> **Last updated:** 2026-06-21. Trunk: `main_v2` (phases 1–3 merged). Conformance data: `conformance/`.

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
| 4 Full styles + assertions | 🟢 **core done** | marks/`@cases` + RichDiff + async + unittest fidelity done; purity guard deferred to sandbox |
| 5 Coverage + cache | 🟢 **core done** | coverage→DepGraph→impact + content-addressed cache done; live-loop wiring with Phase-6 daemon |
| 6 Scheduler + daemon | 🟡 **scheduler done** | LocalityScheduler done; warm daemon / FS-watch need new deps (notify, JSON-RPC) |
| 7 Compat + reporting + hardening | 🟡 **reporters started** | terminal/JUnit/JSON done; GitHub/SARIF + plugin host + perf + Windows remain |

| Track B item | Status |
|---|---|
| Native surface (`@provides`/`@cases`/marks, type-DI) | ✅ N1–N4 |
| `riptide migrate` codemod + report | ✅ |
| Conformance harness (instrument) | ✅ |
| **Builtins (monkeypatch/tmp_path/…)** | ✅ done (click 70%→93%) |
| **Type-inference for untyped fixtures (B3)** | ✅ done (total 79%→85%) |
| **Corpus breadth (B7: +Flask +anyio, 4 repos)** | ✅ done |
| **Run-through tier (B6)** | 🟢 harness + cachetools 215/215 100%; migrated pytest repos pending venv |
| usefixtures (B2), request (B4), async/provider-params (B5) | ⬜ (long tail; 10/18/15% of remaining) |
| ② in-process/FFI backend | 🟡 spiked GO, design pending |

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
- [ ] ⏳ **Purity guard** (deferred) — cross-fork shared-state-mutation detection. The impurity *policy* seam already exists (`cache::{Purity, SandboxHooks}`, Phase 5d); the *runtime detector* that feeds it is the ADR-E004 stage-2 sandbox (fs/clock/net/state interception) — a substantial standalone effort, sequenced with that sandbox rather than here. Conservative-by-default holds until then.

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
- [ ] ⏳ **Live-loop wiring** — cache consult (hit→impact-skip→run) inside the worker loop + source content hashing + DepGraph persistence → lands with the Phase-6 daemon that owns the persistent run loop

### Phase 6 — Scheduler + daemon  ⬜
*Designs: [06-scheduler](design/06-scheduler.md), [08-daemon](design/08-daemon.md); [ADR-E007](design/adr/ADR-E007-warm-daemon.md), [ADR-E010](design/adr/ADR-E010-locality-scheduler.md). Consumes Phase-3 `Watermark.rss_bytes` via `MemoryGovernor`.*

- [x] **`LocalityScheduler`** — duration-aware LPT balancing + scope-locality (5 tests; makespan ≤ round-robin on uneven durations; a module co-locates; dominant group splits)
- [x] **FS watch + invalidation** — `engine-daemon`: content-hash `Invalidator` (conftest/config/C-ext recycle; test→recollect; source→impact; identical bytes→no-op) + `notify`-backed `FsWatcher` + noise-filtering `Debouncer`
- [🟢] **Warm daemon brain** — `Session` composes invalidation→impact→cache into the minimum re-run (`ChangeOutcome`); RPC protocol types (`RpcRequest`/`RpcResponse`). ⏳ remaining: the socket server + process lifecycle (start/reuse/health) glue, integration-tested e2e
- [ ] ⏳ **`tiderace watch`** native mode — the thin client over the daemon (needs the socket/lifecycle glue above)

### Phase 7 — Reporting + hardening (compat → migration)  🟡 **reporters done**
*Designs: [12-plugin-host](design/12-plugin-host.md), [13-cross-cutting](design/13-cross-cutting.md); [ADR-E008](design/adr/ADR-E008-cross-platform.md). Note: "pytest-compat layer" is **replaced** by Track B migration.*

- [x] **Reporters** — terminal + JUnit XML + JSON + GitHub annotations + SARIF, all behind the `Reporter` seam (8 tests; each validated against its consumer's shape)
- [ ] **Plugin host** — riptide's own hook host (trait-based), not pytest's; `PyPluginAdapter` boundary
- [x] **Conformance suite** (B6) — `conformance/runthrough.py` runs a suite **through the engine** vs an oracle; cachetools 215/215 = 100%. ⏳ extend to the migrated pytest repos (needs per-repo venvs)
- [ ] ⏳ **Perf hardening** — batching, governor tuning, startup → `benchmarks/RESULTS.md`
- [ ] ⛔ **Windows `SubprocessWorker` validation** — needs a Windows CI runner (not available in this env)

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

### B4 — `request` introspection  ⬜  *(2% — low priority)*
- [ ] Decide a narrow native equivalent (e.g. `Request` with `.param`/`.node`) vs. permanent can't-map
  - Done: a documented decision in ADR-E012's revisit section

### B5 — Async + provider-level params  🟢 **provider-params done (2026-06-23)**
*Proof `proof_b5_provider_params.py`; measured TOTAL 85%→87%, anyio 80%→89%.*
- [ ] Async providers (`async def @provides` + `await` in body) — pairs with Phase-4 async tests *(remaining)*
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
*[ADR-E011 ②](design/adr/ADR-E011-shim-transport-seam.md); spike `spike-inproc/` = GO. Independent of Tracks A/B — rides the `ShimTransport` seam.*

- [x] **Isolation design** — ratified [ADR-E013](design/adr/ADR-E013-inprocess-isolation.md): **fork-from-embedded** (keep the ADR-E003 COW model; ② swaps the *control plane*, not isolation, preserving cache soundness). Per-test reset/subinterpreters **parked** with a revisit trigger.
- [ ] `InProcessTransport: ShimTransport` in (or beside) `engine-core` — third backend, no `Worker` change (needs `pyo3`/libpython; fork-safety constraint per E013: single-threaded parent at the fork point)
- [ ] `PyConfig` home/venv plumbing (kill the spike's cosmetic warnings)
- [ ] Broader C-ext smoke (numpy/pandas/pydantic-core) in one interpreter
- [ ] Benchmark vs the subprocess `PipeTransport` baseline (prove the syscall win)
  - Done: ✅ ratified isolation ADR + ⏳ a working backend behind the seam + a perf delta

---

## 5. Recommended order (across both tracks)

1. **B1 builtins** (monkeypatch → tmp_path → capsys/capfd) — cheap, lifts adoption 70%→~95%.
2. **Phase 5** coverage + cache — the spine's load-bearing segment (impact analysis = the pitch).
3. Interleave **Phase 4 remainder** (RichDiff assertions, async) with 5, as the roadmap allows.
4. **B6/B7** conformance run-through + corpus breadth — continuously, as the tripwire.
5. **Phase 6** scheduler/daemon → **Phase 7** reporting/hardening.
6. **②** in-process backend — parallel, on its own clock; never blocks 1–5.
