# Benchmarks

tiderace ships a reproducible benchmark harness that compares it against the common Python
test runners on a generated fixture suite. Run it yourself rather than trusting a fixed
number — results vary by machine and, especially, by how fast `pytest` imports on your setup.

```bash
python benchmarks/run_benchmarks.py
```

This generates a deterministic fixture and times these scenarios with
[hyperfine](https://github.com/sharkdp/hyperfine), writing a table to `benchmarks/RESULTS.md`:

- **tiderace — cold full run** (`--all`)
- **tiderace — warm run, no changes** (skips everything)
- **tiderace — warm run, one module touched** (runs only affected tests)
- **pytest** baseline
- **pytest-xdist** (`-n auto`)
- **pytest-testmon** (cold and warm)
- **unittest** discovery

Tune the workload:

```bash
python benchmarks/run_benchmarks.py --modules 50 --tests-per-module 10 --runs 5
python benchmarks/run_benchmarks.py --work-ms 20   # simulate I/O-bound tests
```

## How to read the results

tiderace's advantage is **warm / impact runs** that skip unchanged tests — that is the
everyday edit→test loop, where it is dramatically faster than running the whole suite.

For a **cold full run of many fast tests**, tiderace trades some speed for compatibility: it
drives real `pytest` in subprocesses. By default it runs tests **batched** — one pytest
process per worker — which is far faster than one process per test, but still pays one
interpreter startup per worker, so single-process `pytest` can edge it out on trivial suites.
This is the documented trade-off in [ADR-009](../design/decisions.md); for running
*everything* once, `pytest-xdist` is often the fastest option.

!!! note "Honest framing"
    The numbers in `benchmarks/RESULTS.md` are illustrative and machine-specific. The shape
    is the point: **tiderace wins the warm/impact loop; it is competitive-but-not-fastest on
    cold full runs.** That is exactly what its design optimises for.

## Methodology

See `benchmarks/README.md` in the repository for the full workload model, the priming steps
(warm scenarios prime once with coverage to build the dependency graph), and how per-runner
errors are recorded rather than crashing the harness.
