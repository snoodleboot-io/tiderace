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
| 4 Full styles + assertions | 🟡 **partial** | native marks/`@cases` done (Track B); **assertions/async/unittest-fidelity remain** |
| 5 Coverage + cache | ⬜ | the headline differentiator |
| 6 Scheduler + daemon | ⬜ | watch / warm daemon |
| 7 Compat + reporting + hardening | ⬜ | compat → migration (started); reporters/hardening remain |

| Track B item | Status |
|---|---|
| Native surface (`@provides`/`@cases`/marks, type-DI) | ✅ N1–N4 |
| `riptide migrate` codemod + report | ✅ |
| Conformance harness (instrument) | ✅ |
| **Builtins (monkeypatch/tmp_path/…)** | ✅ done (click 70%→93%) |
| usefixtures, async providers, provider-params | ⬜ |
| Migration run-through-engine tier | ⬜ |
| ② in-process/FFI backend | 🟡 spiked GO, design pending |

---

## 2. Track A — Capability phases (remaining)

### Phase 4 — Full styles + assertions  🟡 partial
*Designs: [09-assertions](design/09-assertions.md), [10-test-styles](design/10-test-styles.md); [ADR-E009](design/adr/ADR-E009-lazy-assertion-introspection.md).*

- [x] Native parametrization — `@riptide.cases` through the fork engine
- [x] Native marks — `@skip`/`@skip_if`/`@xfail`(+strict)/`@tag`, shim-honored
- [ ] **Lazy assertion introspection + RichDiff** (the big one)
  - Rewrite/inspect plain `assert` to produce a structured `RichDiff` (operands, op, per-element diff)
  - Lazy: only materialize the diff on failure (no cost on pass) — ADR-E009
  - Wire `RichDiff` into the `ExecEvent::AssertionFailure` path (shim → Rust)
  - Done: a failing `assert a == b` reports operand values + a diff; covered by unit + a live test
- [ ] **Async tests** — `async def test_*`
  - Detect coroutine tests in the shim; drive an event loop per test; same outcome mapping
  - Decide async **providers** (`async def` + `await`) — or defer to Track B (async providers item)
  - Done: an `async def test_` passes/fails correctly through the engine
- [ ] **unittest fidelity** — `subTest`, `expectedFailure`, `setUpClass`/`tearDownClass`
  - Phase-3 unittest is method-granularity with no DI; extend result mapping for subTest/expectedFailure
  - Done: a `TestCase` using `subTest` + `@expectedFailure` maps to correct per-node outcomes
- [ ] **Purity guard** — detect/flag tests that mutate shared (module/session) state across forks
  - Done: a shared-state-mutating test is flagged (design 09); no false positives on the corpus

### Phase 5 — Coverage + cache  ⬜  ← **recommended next spine segment**
*Designs: [07-cache](design/07-cache.md), [11-coverage-impact](design/11-coverage-impact.md), [13-cross-cutting](design/13-cross-cutting.md); [ADR-E004](design/adr/ADR-E004-content-addressed-cache.md), [ADR-E006](design/adr/ADR-E006-coverage-sys-monitoring.md). Consumes Phase-3 `ClosureHash`.*

- [ ] **Coverage via `sys.monitoring`** (3.12+) with `settrace` fallback (≤3.11)
  - Per-test line coverage captured in the shim child; streamed to Rust
  - Done: per-test covered-line sets recorded; differential vs `coverage.py` on the corpus
- [ ] **`DepGraph`** — file → tests that touch it (built from coverage)
  - Done: editing one source file yields exactly its dependent tests
- [ ] **`ImpactAnalyzer`** — select tests by changed files × DepGraph (port/adapt legacy `impact.rs` logic)
  - Done: warm run with no changes skips all; one change re-runs only impacted (integration test)
- [ ] **Content-addressed cache** — store + index keyed on Phase-3 `ClosureHash` (+ source hash)
  - `Cache` trait (ADR-E005 seam), `TieredCache(Local, Remote)`, `NullCache`
  - Done: identical inputs → cache hit (no re-run); a changed closure → miss
- [ ] **`SandboxHooks`** — impurity detection (network/fs/clock) to keep cache sound
  - Done: an impure test is detected and excluded from caching with a clear reason

### Phase 6 — Scheduler + daemon  ⬜
*Designs: [06-scheduler](design/06-scheduler.md), [08-daemon](design/08-daemon.md); [ADR-E007](design/adr/ADR-E007-warm-daemon.md), [ADR-E010](design/adr/ADR-E010-locality-scheduler.md). Consumes Phase-3 `Watermark.rss_bytes` via `MemoryGovernor`.*

- [ ] **`LocalityScheduler`** — duration-aware LPT balancing + scope-locality (group by deepest shared watermark)
  - Done: makespan beats naive round-robin on an uneven corpus; locality reduces re-setup count
- [ ] **Warm daemon** — JSON-RPC server over `engine-core`, long-lived wellspring pool
  - Done: a second request in a session pays no import; crash → respawn (reuse Phase-2b robustness)
- [ ] **FS watch + invalidation** — `notify` debounced; conftest/provider change recycles correctly
  - Done: editing a provider re-runs its dependents; editing a test re-runs that test only
- [ ] **`tiderace watch`** native mode — sub-second impacted re-runs against the warm pool
  - Done: edit→save→result loop under the daemon, native engine (no pytest)

### Phase 7 — Reporting + hardening (compat → migration)  ⬜
*Designs: [12-plugin-host](design/12-plugin-host.md), [13-cross-cutting](design/13-cross-cutting.md); [ADR-E008](design/adr/ADR-E008-cross-platform.md). Note: "pytest-compat layer" is **replaced** by Track B migration.*

- [ ] **Reporters** — terminal (default) + JUnit XML + JSON + GitHub annotations + SARIF (`Reporter` seam)
  - Done: each format validated against its schema/consumer on the corpus
- [ ] **Plugin host** — riptide's own hook host (trait-based), not pytest's; `PyPluginAdapter` boundary
  - Done: a sample native plugin observes start/finish hooks
- [ ] **Conformance suite** (formalize Track-B harness) — pass-rate metric vs real OSS suites
  - Done: `conformance/` extended to run **migrated** suites through the engine (not just migrate)
- [ ] **Perf hardening** — batching, governor tuning, startup
  - Done: cold/warm numbers in `benchmarks/RESULTS.md`, native engine
- [ ] **Windows `SubprocessWorker` validation** — the no-fork path on Windows CI
  - Done: acceptance scenarios green on Windows runner

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

### B2 — `usefixtures` handling  ⬜  *(14% of click can't-map)*
- [ ] Native `@riptide.uses(Provider)` (by type) and/or autouse mapping
- [ ] `migrate`: `@pytest.mark.usefixtures("x")` → `@riptide.uses(<TypeOfX>)` when the type is known; flag otherwise
  - Done: conformance usefixtures bucket shrinks measurably

### B3 — Migration type-inference for untyped fixtures  ⬜  *(7%)*
- [ ] In `migrate`, infer a provider's type from its body (`return X()` / `yield X()`) when annotation absent
- [ ] Emit the inferred annotation (`-> X`) instead of flagging, when confident; flag when not
  - Done: untyped-provider bucket shrinks; no wrong inferences (precision over recall)

### B4 — `request` introspection  ⬜  *(2% — low priority)*
- [ ] Decide a narrow native equivalent (e.g. `Request` with `.param`/`.node`) vs. permanent can't-map
  - Done: a documented decision in ADR-E012's revisit section

### B5 — Async + provider-level params  ⬜
- [ ] Async providers (`async def @provides` + `await` in body) — pairs with Phase-4 async tests
- [ ] Provider-level parametrization (`@provides` that fans out) — currently can't-map in `migrate`
  - Done: each has a proof; `migrate` parametrized-fixture bucket addressed

### B6 — Migration **run-through-engine** tier  ⬜  *(the heavier conformance step)*
- [ ] Per-repo venv + deps install; run the **migrated** suite through the shim/engine
- [ ] Compare outcomes to a pytest oracle run (differential) → an *execution* pass-rate, not just auto-map
  - Done: `conformance/` reports run-through pass-rate for ≥1 repo; gaps filed as engine bugs

### B7 — Conformance corpus breadth  ⬜
- [ ] Add a fixture-heavy **app** suite (Flask) and an **async** lib to `manifest.tsv` (pinned SHAs)
  - Done: the can't-map distribution is re-measured across ≥4 repos before locking builtin semantics

---

## 4. The side-bet — ② in-process / FFI backend
*[ADR-E011 ②](design/adr/ADR-E011-shim-transport-seam.md); spike `spike-inproc/` = GO. Independent of Tracks A/B — rides the `ShimTransport` seam.*

- [ ] **Isolation design** (the one open question): fork-from-embedded vs. per-test module reset — pick one, ADR it
- [ ] `InProcessTransport: ShimTransport` in (or beside) `engine-core` — third backend, no `Worker` change
- [ ] `PyConfig` home/venv plumbing (kill the spike's cosmetic warnings)
- [ ] Broader C-ext smoke (numpy/pandas/pydantic-core) in one interpreter
- [ ] Benchmark vs the subprocess `PipeTransport` baseline (prove the syscall win)
  - Done: a ratified isolation ADR + a working backend behind the seam + a perf delta

---

## 5. Recommended order (across both tracks)

1. **B1 builtins** (monkeypatch → tmp_path → capsys/capfd) — cheap, lifts adoption 70%→~95%.
2. **Phase 5** coverage + cache — the spine's load-bearing segment (impact analysis = the pitch).
3. Interleave **Phase 4 remainder** (RichDiff assertions, async) with 5, as the roadmap allows.
4. **B6/B7** conformance run-through + corpus breadth — continuously, as the tripwire.
5. **Phase 6** scheduler/daemon → **Phase 7** reporting/hardening.
6. **②** in-process backend — parallel, on its own clock; never blocks 1–5.
