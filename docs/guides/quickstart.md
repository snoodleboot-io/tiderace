# Quick Start

## Your First Run

Point tiderace at your test directory. It will discover all `test_*.py` and `*_test.py` files automatically:

```bash
tiderace tests/
```

On the first run, tiderace:

1. Collects all tests via fast regex-based scanning
2. Hashes every `.py` file in the project
3. Runs all tests in parallel
4. Stores results and file hashes in `.tiderace.db`

```
  ✓ collected 47 tests
  ⚡ no previous state — running all tests

  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    tiderace ⚡ Rust-powered test engine
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    tests: 47   skipped (unchanged): 0   workers: 8   coverage: off

  ✓ [1/47] tests/test_auth.py::test_login 312ms
  ✓ [2/47] tests/test_auth.py::test_logout 289ms
  ...
```

## Second Run (No Changes)

Run again without changing anything:

```bash
tiderace tests/
```

```
  ✓ collected 47 tests
  ⚡ no files changed
  ⚡ All tests skipped — no changes detected!
```

**Zero tests run. Instant feedback.** Skipping unchanged tests works with or without coverage.

## After Changing a Test File

Edit a test file, then run again:

```bash
tiderace tests/
```

```
  ✓ collected 47 tests
  ⚡ 1 file(s) changed:
    tests/test_auth.py

  tests: 5   skipped (unchanged): 42   workers: 8
```

A test is always re-run when its own test file changes (or when it never ran before, or previously failed/errored).

## Source-Level Impact Needs Coverage

Mapping a *source* edit (e.g. `src/auth.py`) to the specific tests that depend on it requires a coverage dependency graph. Build it by running once with `--coverage`:

```bash
tiderace tests/ --coverage
```

After the run a coverage report is printed and the dependency graph is stored. Now a source change re-runs only the affected tests:

```bash
# edit src/auth.py, then:
tiderace tests/
```

```
  ✓ collected 47 tests
  ⚡ 1 file(s) changed:
    src/auth.py

  tests: 8   skipped (unchanged): 39   workers: 8
```

Only the 8 tests whose recorded dependencies include `auth.py` are re-run.

Without a coverage graph, tiderace cannot map a source edit to individual tests, so it is conservative: it re-runs every test that lacks recorded dependencies. **Run once with `--coverage` to unlock precise source-level impact analysis.**

```
  Coverage
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  src/auth.py      [██████████] 100%  42/42
  src/models.py    [████████░░]  83%  25/30
  src/utils.py     [██████░░░░]  61%  11/18

  Overall: 87.4%
```

## Common Commands

```bash
# Run with 8 parallel workers
tiderace tests/ -n 8

# Force run all tests regardless of changes
tiderace tests/ --all

# Collect and list all tests without running
tiderace collect tests/

# Reset state (next run will re-run everything)
tiderace clear

# Use a specific Python binary
tiderace tests/ --python .venv/bin/python
```

## Next Steps

- [Configuration](configuration.md) — customize patterns, workers, DB path
- [How impact analysis works](../design/impact-analysis.md)
- [CI/CD setup](../guides/releases.md)

## Benchmark It Yourself

To compare tiderace against `pytest`, `pytest-xdist`, `pytest-testmon`, and `unittest` on a generated fixture suite, run the harness (results are written to `benchmarks/RESULTS.md`):

```bash
python benchmarks/run_benchmarks.py
```

Expect tiderace's cold full run to trail in-process pytest (subprocess-per-test, ~250ms each); its win is warm/impact runs that skip unchanged tests.
