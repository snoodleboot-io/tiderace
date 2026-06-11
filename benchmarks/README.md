# riptide benchmarks

This directory holds a self-contained benchmark harness that compares **riptide**
against common Python test runners on a deterministic reference fixture, using
[`hyperfine`](https://github.com/sharkdp/hyperfine) for wall-clock measurement.

```
benchmarks/
├── run_benchmarks.py        # the harness (this is what you run)
├── README.md                # you are here — methodology + reproduction
├── RESULTS.md               # generated: markdown comparison table
├── results.json             # generated: raw timings + hyperfine output
├── fixtures/
│   ├── generate.py          # deterministic fixture generator
│   └── sample_project/      # generated fixture (regenerated on every run)
└── .hyperfine/              # generated: per-scenario hyperfine JSON exports
```

## Reproduce

```bash
# Default run (50 modules × 10 tests/module = 1005 tests, 10 timed runs each):
python benchmarks/run_benchmarks.py

# A quick smoke run (tiny fixture, 2 runs) to prove the harness works:
python benchmarks/run_benchmarks.py --modules 2 --tests-per-module 3 --runs 2

# A latency-dominated workload (each test sleeps 5 ms) for parallelism comparisons:
python benchmarks/run_benchmarks.py --work-ms 5
```

The harness writes `benchmarks/RESULTS.md` (a markdown table) and
`benchmarks/results.json` (the raw data) on every run.

### Prerequisites (provisioned by Lane 0 — the harness verifies them)

- `riptide` binary at `target/debug/riptide` (build with `cargo build` if missing).
- A Python venv at `.riptide-bench-venv/` with `pytest`, `coverage`,
  `pytest-xdist`, and `pytest-testmon` installed.
- `hyperfine` (v1.x) on `PATH`.

Override paths with `--riptide <path>` and `--python <interpreter>` if your
layout differs.

## The workload model

The fixture (`fixtures/generate.py`) builds a small project with a **real import
graph** so impact analysis is meaningful:

- `src/shared.py` is imported by **every** test module — changing it affects all tests.
- `src/mod_{i}.py` is imported by exactly **one** pytest module — changing it
  affects only that module's tests.
- `tests/test_mod_{i}.py` are pytest-style tests; `tests/test_unit_case.py` is a
  `unittest.TestCase` (so the unittest scenario exercises real `unittest`).

By default each test does a **tiny fixed CPU computation** (`--work-ms 0`). With
fast tests, **Python interpreter startup dominates** the runtime — this is the
regime that exposes riptide's subprocess-per-test cost most starkly, so it is the
honest default. Pass `--work-ms MS` to add a fixed `time.sleep` per test when you
want per-test *latency* (rather than interpreter startup) to dominate; that
regime favours parallel runners like `pytest-xdist` and riptide's worker pool.

Fixture size is controlled by `--modules N` and `--tests-per-module M`
(pytest test count = `N × M × 2`, plus 5 baked-in unittest tests).

## Scenarios — what each one measures

All scenarios run against the **same** freshly generated fixture, in the fixture
directory, under `hyperfine --warmup 1 --runs <N>`. Per-run `--setup` / `--prepare`
/ `--cleanup` hooks establish deterministic state so every timed iteration
measures the intended thing.

| Scenario | Command (in fixture dir) | What it measures |
|----------|--------------------------|------------------|
| `riptide_cold_full` | `riptide --all --python <venv> tests/` | A cold full run with no skipping. `--prepare` runs `riptide clear` before each timed run, so every iteration is genuinely cold. This is riptide's **worst case** on fast tests. |
| `riptide_warm_noop` | `riptide --python <venv> tests/` | A warm impact run with **no source changes**. `--setup` primes the dependency graph once (`riptide clear` then a coverage run); timed runs change nothing, so impact analysis should skip every test. riptide's **best case**. |
| `riptide_warm_one_module` | `riptide --python <venv> tests/` | A warm impact run after editing **one** source module. `--setup` primes; `--prepare` restores the pristine module then touches it, so each run re-runs only the affected tests. riptide's **typical edit-loop case**. |
| `pytest_cold` | `python -m pytest -q` | The in-process pytest baseline — runs everything, every time. |
| `pytest_xdist` | `python -m pytest -q -n auto` | pytest parallelised across cores; the fastest way to run *everything* on this machine. |
| `pytest_testmon_cold` | `python -m pytest -q --testmon` | pytest-testmon with **no prior data** (`--prepare` deletes `.testmondata`). The fair cold comparison to riptide's cold run. |
| `pytest_testmon_warm` | `python -m pytest -q --testmon` | pytest-testmon **primed**, no changes — testmon's own impact-skipping. The fair warm comparison to riptide's warm run. |
| `unittest` | `python -m unittest discover -s tests -t .` | The stdlib runner, for reference. |

### Why priming uses coverage

riptide builds its test→file dependency graph from coverage data. The state
database only records `test_deps` when a test reports covered files, which
requires running riptide with `-c/--coverage`. So the warm scenarios prime with
`riptide -c ... tests/` before the timed (non-coverage) impact runs.

## The honest cold-vs-warm story

riptide runs each test in its **own** `python -m pytest <nodeid>` subprocess (see
**ADR-001** in `docs/design/decisions.md`). Interpreter startup is ~250 ms per
test, so:

- On a **cold full run of many fast tests**, riptide is **slower** than
  in-process pytest — it pays startup once per test instead of once per session.
  We measure and report this; it is not hidden.
- On a **warm run**, riptide hashes sources, consults its dependency graph, and
  re-runs only the tests affected by a change — skipping everything else. This is
  where the subprocess cost is amortised away and riptide wins, especially on
  large suites where a small edit touches few tests.

Reporting **both** regimes — and comparing riptide warm against pytest-testmon
warm, not just against full pytest — is what makes this benchmark fair rather
than cherry-picked.

> **Note (June 2026):** riptide's warm-skip path is being hardened on Lane A
> (impact-analysis fix). Until that merges, the warm scenarios may skip fewer
> tests than they should, so warm numbers will look pessimistic. The **harness**
> is correct regardless; warm numbers will improve once Lane A lands.

## Error handling

If a single runner errors (e.g. a runner exits non-zero, or hyperfine fails for
one scenario), the harness records the error in that row of `RESULTS.md` /
`results.json` and continues with the remaining scenarios — one broken runner
never aborts the whole benchmark.
```
