# Watch Mode

`tiderace watch` keeps a pool of **warm** Python workers running and re-runs only the
tests impacted by each file you save. Because the workers import `pytest` once and stay
alive between runs, every cycle after the first pays **no interpreter or pytest startup** —
you get sub-second feedback as you edit.

```bash
tiderace watch tests/
```

```
  ✓ collected 47 tests
  ⚡ warm pool ready: 8 workers
  ✓ 47 passed · 0 failed · 0 skipped · 0.9s

  👀 watching for changes — Ctrl-C to stop

  ⚡ 1 file(s) changed → 6 test(s)
  ✓ 6 passed · 0 failed · 0 skipped · 0.18s
```

## How it works

1. On startup, tiderace collects your tests and spawns `-n` warm workers (default: CPU count),
   each of which imports pytest once.
2. It runs an initial pass to establish a baseline, then watches the tree.
3. When you save a file, tiderace hashes the change, runs [impact analysis](../design/impact-analysis.md)
   to pick the affected tests, and dispatches just those node ids to the warm pool.
4. Changed files are evicted from each worker's module cache before the run, so a warm
   re-run **always reflects the code on disk** — never a stale pass.

## Options

`watch` honours the same global options as a normal run:

```bash
# More workers, explicit interpreter
tiderace watch tests/ -n 12 --python .venv/bin/python

# Watch specific paths
tiderace watch tests/unit tests/integration
```

| Option | Effect in watch mode |
|---|---|
| `-n, --workers` | Size of the warm pool |
| `--python` | Interpreter the workers run |
| `--timeout` | Per-test limit; a hung test is killed and that worker respawned |
| `--pattern` | Test-file discovery regex |

See the [CLI Reference](../api/cli.md) and [Configuration](configuration.md) for the full
list and `pyproject.toml` defaults.

## Precise vs. conservative selection

How narrowly watch re-runs depends on whether a per-test dependency graph exists:

- **With a coverage graph** (you have run `tiderace --coverage` at least once), editing a
  source file re-runs only the tests whose recorded dependencies changed.
- **Without one**, tiderace is conservative: editing a *source* file re-runs every test that
  lacks a dependency graph (it cannot map the edit to specific tests). Editing a *test* file
  always re-runs just that file's tests.

To get the tightest watch loops, prime the graph once:

```bash
tiderace --coverage tests/      # build the per-test dependency graph
tiderace watch tests/           # now source edits re-run only impacted tests
```

## Robustness

The warm pool is built to survive a long editing session:

- A test that **hangs** is killed at `--timeout` and its worker is respawned — the loop never wedges.
- A test that **crashes** its worker (e.g. a segfaulting C extension) is detected; the worker
  is replaced and the run continues.
- A change to **`conftest.py`** recycles the pool, since fixtures and collection are cached in
  the warm interpreters.

## When *not* to use it

Warm workers share a process across runs, which is a convenience for **trusted local
development**. For CI, or when running untrusted code, prefer a normal run (which uses
fresh subprocesses) or `--isolate` for one process per test. See
[ADR-009](../design/decisions.md) for the design rationale.
