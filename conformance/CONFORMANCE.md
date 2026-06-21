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

| repo | pin | test files | mapped | can't-map | auto-map (first pass) | auto-map (post-B1) |
|---|---|---:|---:|---:|---:|---:|
| pallets/click | `8.1.7` (874ca2b) | 21 | 134 | 10 | 70% | **93%** |
| tkem/cachetools | `v5.5.0` (6c78a8f) | 12 | 0 | 0 | n/a | n/a |

**cachetools is a pure `unittest.TestCase` suite** — *nothing pytest-specific to migrate*. riptide
already drives it via stdlib `unittest.TestCase.run()` (ADR-E001), so it runs **as-is, no migration**.
Useful confirmation that the unittest path needs no surface work.

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

## Conclusion → next build item (data-driven)

The builtin blocker is **eliminated**. The remaining click gaps are exactly the next-ranked Track-B
items, now the dominant share:

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
