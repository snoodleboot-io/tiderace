# N5 conformance — `riptide migrate` against real pytest suites

Measures how much **real-world** pytest authoring maps to riptide's native type-DI surface, and ranks
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

| repo | pin | test files | mapped | can't-map | auto-map (post-B1) |
|---|---|---:|---:|---:|---:|
| pallets/click | `8.1.7` (874ca2b) | 21 | 134 | 10 | **93%** |
| tkem/cachetools | `v5.5.0` (6c78a8f) | 12 | 0 | 0 | n/a |
| pallets/flask | `3.0.3` (c12a5d8) | 29 | 111 | 58 | **66%** |
| agronholm/anyio | `4.4.0` (053e8f0) | 21 | 67 | 17 | **80%** |

**cachetools is a pure `unittest.TestCase` suite** — *nothing pytest-specific to migrate*. riptide
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

`riptide.builtins` (`MonkeyPatch`/`TmpPath`/`Capsys`/`Capfd`) now ships and `migrate` maps the five
builtin requests to typed params + injects `from riptide.builtins import …` (see
[`engine/py-riptide/riptide/builtins/`](../engine/py-riptide/riptide/builtins/), proof
[`proof_n5_builtins.py`](../engine/py-riptide/proof_n5_builtins.py)). The shim auto-registers them
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

## Conclusion → next build item (data-driven, re-ranked by the 4-repo breadth)

The builtin blocker is **eliminated**. With Flask + anyio in the corpus the dominant gap **flipped to
untyped fixtures** — `untyped provider` (33%) + `untyped fixture param` (32%) = **65% of all can't-map**.
So the next increment is **B3 (migration type-inference for untyped fixtures)**: infer a provider's
type from its body (`return X()` / `yield X()`) and emit `-> X` instead of flagging; once the provider
is typed, its dependent test params type-resolve too — addressing *both* top buckets at once.

After B3: **B5 parametrized fixtures** (11%) and **B4 request introspection** (13%), then **B2
usefixtures** (now only 7%).

### (superseded) post-B1 click-only ranking

When only click was measured, the remaining gaps ranked:

1. **`usefixtures` (6, 60%)** — B2: native `@riptide.uses(Provider)` / autouse mapping.
2. **untyped provider (3, 30%)** — B3: infer a provider's type from its body when the annotation is
   absent.
3. request introspection (1, 10%) — B4: low-priority; decide a narrow native equivalent vs. permanent
   can't-map.

So the next increment after B1 is **B2 (usefixtures)**, then **B3 (type inference)**.

## Caveats

- Auto-map % counts *constructs*, not lines; ast.unparse normalizes formatting (the report is exact).
- Two repos is a starting sample — broaden `manifest.tsv` (a fixture-heavy app suite like Flask, an
  async lib) to harden the distribution before committing to the builtins' exact semantics.
