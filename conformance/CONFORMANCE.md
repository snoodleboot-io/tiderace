# N5 conformance — `tiderace migrate` against real pytest suites

Measures how much **real-world** pytest authoring maps to tiderace's native type-DI surface, and ranks
exactly what doesn't — so the next build items are **data-driven**, not guessed.

## Method

- Targets are **pinned by SHA** ([`manifest.tsv`](manifest.tsv)) and cloned into `vendor/` — which is
  **gitignored**. We do **not** vendor third-party code/licenses into this repo.
- `migrate` is pure `ast` (no install, no execution, no C-ext builds), so this runs over any pytest repo
  immediately. *Running* the migrated suites through the engine is a heavier follow-on (needs each repo's
  deps + a venv) — not part of this first pass.

```bash
./setup.sh                                         # clone pinned SHAs into vendor/
python3 conformance.py vendor/click vendor/cachetools
```

## Results

| repo | pin | test files | first pass | post-B1 | post-B3 | post-B5/B4 | **post-no-value** |
|---|---|---:|---:|---:|---:|---:|---:|
| pallets/click | `8.1.7` (874ca2b) | 70% | 93% | 94% | 94% | **95%** |
| tkem/cachetools | `v5.5.0` (6c78a8f) | n/a | n/a | n/a | n/a | n/a |
| pallets/flask | `3.0.3` (c12a5d8) | — | 66% | 79% | 80% | **83%** |
| agronholm/anyio | `4.4.0` (053e8f0) | — | 80% | 80% | 99% | **99%** |
| **TOTAL** | | | 79% | 85% | 89% | **91%** |

*Progression: first pass 70% (click only) → B1 builtins → B3 type-inference (79%→85%) → B5 provider
params + B4 request decision (85%→89%, can't-map 61→43) → **no-value providers** (89%→**91%**, can't-map
43→36). A pytest fixture that yields/returns no value is a pure setup/teardown fixture (often autouse) —
it provides `None`, so `migrate` now annotates it `-> None` (mapped) instead of flagging it untyped;
this closed 8 (6 flask + 2 click). B2 usefixtures shipped but corpus-neutral (its targets are untyped).
Remaining 36: untyped value-providers/params (lowercase factory & method calls — un-inferable by AST,
correctly flagged), 6 usefixtures (untyped cross-file targets), 3 unsupported builtins, 1 from-pytest
import.*

**cachetools is a pure `unittest.TestCase` suite** — *nothing pytest-specific to migrate*. tiderace
already drives it via stdlib `unittest.TestCase.run()` (ADR-E001), so it runs **as-is, no migration**.
Useful confirmation that the unittest path needs no surface work.

### B7 delivered — corpus breadth (2026-06-21)

Added a **fixture-heavy app** (Flask) and an **async lib** (anyio) to `manifest.tsv` (pinned SHAs),
so the can't-map distribution is measured across **4 real repos (83 test files)** before locking
surface semantics. **TOTAL auto-map 79%** (mapped 312, can't-map 85). The breadth **re-ranked the
gaps** — builtins are no longer the blocker; untyped fixtures dominate (Flask is fixture-heavy and
largely *untyped*):

| category | count | share |
|---|---:|---:|
| untyped provider | 28 | 33% |
| untyped fixture param | 27 | 32% |
| request introspection | 11 | 13% |
| parametrized fixture | 9 | 11% |
| usefixtures | 6 | 7% |
| pytest builtin (unsupported: caplog/factories) | 3 | 4% |
| from-pytest import | 1 | 1% |

### B1 delivered — native builtin resources (2026-06-21)

`tiderace.builtins` (`MonkeyPatch`/`TmpPath`/`Capsys`/`Capfd`) now ships and `migrate` maps the five
builtin requests to typed params + injects `from tiderace.builtins import …` (see
[`engine/py-tiderace/tiderace/builtins/`](../engine/py-tiderace/tiderace/builtins/), proof
[`proof_n5_builtins.py`](../engine/py-tiderace/proof_n5_builtins.py)). The shim auto-registers them
globally, so they resolve by type (the migrated form) or by name (the pytest form).

**Measured effect on click:** `70% → 93%` auto-map; can't-map `43 → 10`. The entire **pytest-builtin
bucket (33) is gone** — `monkeypatch` 21 · `tmp_path` 4 · `capfd` 4 · `capsys` 2 · `tmpdir` 2 all map
now (`tmpdir` mapped with a py.path caveat).

**click — remaining can't-map distribution (10 total):**

| category | count | share |
|---|---:|---:|
| usefixtures | 6 | 60% |
| untyped provider | 3 | 30% |
| request introspection | 1 | 10% |

### B3 delivered — migration type-inference (2026-06-21)

`migrate` now infers an untyped provider's type from its body (`return/yield ClassName(...)`,
resolving one level through a local `x = ClassName()` assignment, plus literal types), emitting `-> X`
instead of flagging — which also types the dependent test params. **Precision over recall**: lowercase
factory calls, unresolved names, and conflicting returns are never given a wrong annotation (they stay
flagged). Proof: [`proof_b3_inference.py`](../engine/py-tiderace/proof_b3_inference.py).

**Measured: TOTAL 79% → 85%** (can't-map 61); **Flask 66% → 79%** (untyped-provider 25→19,
untyped-fixture-param 27→10). The remaining 21 untyped providers are the unconfident shapes correctly
left for the human.

### B6 delivered — run-through tier (2026-06-21)

Beyond the static auto-map %, [`runthrough.py`](runthrough.py) *executes* a real suite **through
tiderace's engine** and diffs every test's outcome against an oracle → an **execution pass-rate** (the
number adoption actually turns on). First target: **cachetools** (pure `unittest`, so no migration —
isolates the *execution* path; oracle = stock `unittest` `TestSuite`, which honors `setUpClass`).

```bash
python3 runthrough.py vendor/cachetools --src vendor/cachetools/src
```

**Result: 215 tests executed through the engine, 215/215 = 100% match, zero divergences.** The engine
reproduces stock unittest exactly on a real OSS suite — validating the Phase-4 unittest fidelity work
end-to-end. (Next run-through targets — the migrated pytest suites click/flask/anyio — need a per-repo
venv + deps and will surface engine gaps to file as the migrated-execution path matures.)

## Conclusion → next build item (data-driven)

Re-ranked after B3 (61 can't-map): `untyped provider` 21 (34% — the unconfident remainder),
`request introspection` 11 (18%, **B4**), `untyped fixture param` 10 (16%), `parametrized fixture` 9
(15%, **B5**), `usefixtures` 6 (10%, **B2**). The next *capability* lever is the migration
**run-through tier (B6)** — turning auto-map % into an *execution* pass-rate — and then **B5/B4/B2**
for the long tail.

### (superseded) post-B1 click-only ranking

When only click was measured, the remaining gaps ranked:

1. **`usefixtures` (6, 60%)** — B2: native `@tiderace.uses(Provider)` / autouse mapping.
2. **untyped provider (3, 30%)** — B3: infer a provider's type from its body when the annotation is
   absent.
3. request introspection (1, 10%) — B4: low-priority; decide a narrow native equivalent vs. permanent
   can't-map.

So the next increment after B1 is **B2 (usefixtures)**, then **B3 (type inference)**.

## Caveats

- Auto-map % counts *constructs*, not lines; ast.unparse normalizes formatting (the report is exact).
- Two repos is a starting sample — broaden `manifest.tsv` (a fixture-heavy app suite like Flask, an
  async lib) to harden the distribution before committing to the builtins' exact semantics.
