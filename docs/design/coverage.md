# Coverage Engine

## Overview

riptide integrates with Python's `coverage.py`. Coverage serves two purposes:

1. **Reporting** — a line-coverage percentage per file after a run.
2. **Dependency mapping** — the per-test graph of which source files each test executed, which
   powers [impact analysis](impact-analysis.md).

## Per-test coverage from one batched run

riptide gets **per-test** attribution without running a separate process per test. A coverage
run is configured with dynamic contexts:

```ini
[run]
dynamic_context = test_function
branch = True
source = .
```

With `dynamic_context = test_function`, coverage.py tags every measured line with the test
that was executing. So a single **batched** run — `coverage run -m pytest <many node ids>`,
one process per worker — still records which lines (and therefore which files) each individual
test touched. This is the same fast batched execution as a normal run; only the wrapper
differs. See [ADR-011](decisions.md).

## Combining and extraction

After the batches finish, riptide combines the per-batch data files and reads the
context-annotated report:

```bash
python -m coverage combine --keep .riptide-coverage/
python -m coverage json --show-contexts -o .riptide-coverage/contexts.json
```

For each file, the report lists which **contexts** (tests) executed lines in it. Coverage
prefixes context names with the package-dependent module path (e.g.
`pkg.tests.test_auth.test_login`), so riptide matches tests on the stable **suffix**
`{file_stem}.{func}` / `{file_stem}.{Class}.{method}` rather than a predicted full name.

## Storing the dependency graph

The extracted `test → files` map is written to SQLite:

```sql
INSERT INTO test_file_deps (test_id, dep_path) VALUES ('tests/test_auth.py::test_login', 'src/auth.py');
INSERT INTO test_file_deps (test_id, dep_path) VALUES ('tests/test_auth.py::test_login', 'src/models.py');
```

On later runs **without** `--coverage`, this stored graph drives impact analysis — no
instrumentation needed until you choose to refresh it.

## Coverage report

```
  Coverage
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  src/auth.py      [██████████] 100%  42/42
  src/models.py    [████████░░]  83%  25/30
  src/utils.py     [██████░░░░]  61%  11/18

  Overall: 87.4%
```

## When to use `--coverage`

| Scenario | Recommendation |
|---|---|
| First run on a project | Use `--coverage` to build the dependency graph |
| Regular development | Omit `--coverage` — the graph persists and is reused |
| After adding source files | Re-run `--coverage` to refresh the graph |
| Before `riptide watch` | Prime once with `--coverage` for the tightest watch loops |

Because coverage now runs batched (not one process per test), building the graph is far
cheaper than it used to be — roughly **4–5× faster** than the legacy isolated path — while
remaining precise. `--isolate --coverage` still uses the one-process-per-test path if you need
strict isolation.
