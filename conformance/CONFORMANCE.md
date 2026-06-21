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

## Results (first pass)

| repo | pin | test files | mapped | can't-map | auto-map |
|---|---|---:|---:|---:|---:|
| pallets/click | `8.1.7` (874ca2b) | 21 | 101 | 43 | **70%** |
| tkem/cachetools | `v5.5.0` (6c78a8f) | 12 | 0 | 0 | n/a |

**cachetools is a pure `unittest.TestCase` suite** — *nothing pytest-specific to migrate*. riptide
already drives it via stdlib `unittest.TestCase.run()` (ADR-E001), so it runs **as-is, no migration**.
Useful confirmation that the unittest path needs no surface work.

**click — can't-map distribution (43 total):**

| category | count | share |
|---|---:|---:|
| pytest builtin | 33 | 77% |
| usefixtures | 6 | 14% |
| untyped provider | 3 | 7% |
| request introspection | 1 | 2% |

**Which builtins (the 33):** `monkeypatch` 21 · `tmp_path` 4 · `capfd` 4 · `capsys` 2 · `tmpdir` 2.
`monkeypatch` + `tmp_path` alone are **25/33 (76%)** of builtin blockers.

## Conclusion → next build item (data-driven)

The dominant migration blocker is **pytest builtins**, and within them **`monkeypatch`** (64% of builtin
blockers; ~49% of *all* click can't-map). So the next increment is **native riptide builtin resources**,
ordered by this data:

1. `monkeypatch` (21) → 2. `tmp_path` (4) → 3. `capfd`/`capsys` (6) → 4. `tmpdir` (2, legacy alias)

Shipping the first two would lift click from **70% → ~90%** auto-map; all five → **~95%+**. `usefixtures`
(14%) is the next surface gap after builtins.

## Caveats

- Auto-map % counts *constructs*, not lines; ast.unparse normalizes formatting (the report is exact).
- Two repos is a starting sample — broaden `manifest.tsv` (a fixture-heavy app suite like Flask, an
  async lib) to harden the distribution before committing to the builtins' exact semantics.
