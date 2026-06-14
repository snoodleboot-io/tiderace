# Execution Model

tiderace runs your tests with real `pytest` for full compatibility (fixtures, plugins,
assertion rewriting), and uses Rust only to orchestrate. It has three execution strategies;
which one runs depends on your flags.

## Worker pool

A [Rayon](https://github.com/rayon-rs/rayon) thread pool, sized to `available_parallelism()`
by default (override with `-n`), drives concurrency. Each worker owns one Python process at a
time.

```bash
tiderace tests/ -n 16   # 16 concurrent workers
tiderace tests/ -n 1    # sequential (useful for debugging)
```

## Strategy 1 — Batched (default)

The selected tests are split into one chunk per worker, and **each chunk runs as a single
`pytest` process** over many node ids. This amortises interpreter + pytest startup: *N* tests
cost *W* interpreter startups (one per worker), not *N*.

```
Worker 1: pytest t1 t2 t3 … t13     (one process)
Worker 2: pytest t14 t15 … t26      (one process)
…
```

Per-test pass/fail is recovered from pytest's `-rA` summary, whose lines carry the exact node
id. This is what makes a cold full run roughly **8× faster** than a process-per-test approach.
See [ADR-009](decisions.md).

## Strategy 2 — Isolated (`--isolate`)

One `pytest` process per test — the original model. Slower (one interpreter startup per test)
but gives a fresh interpreter per test, for suites that genuinely need that isolation.

```bash
tiderace tests/ --isolate
```

Each test runs roughly as:

```bash
python -m pytest -- path/to/test_file.py::test_function -x --tb=short -q --no-header
```

The `--` separator means a hostile file name or path can never be parsed as a pytest flag,
and a per-test [`--timeout`](../api/cli.md) kills and records any test that hangs.

## Strategy 3 — Warm pool (`tiderace watch`)

[Watch mode](../guides/watch.md) keeps a pool of **long-lived** worker processes that import
pytest once and run node ids fed to them over a JSON protocol. Across edit→test cycles the
import cost is paid only once, giving sub-second re-runs. The pool is hardened against hung
tests (timeout → kill + respawn), crashed workers (detect → respawn), and stale code (changed
modules are evicted from each worker before a re-run).

## Coverage and batching

`--coverage` runs **batched too**: each chunk runs under `coverage run` with a per-test
dynamic context, so a single fast batched run still yields a precise per-test dependency
graph. See [Coverage Engine](coverage.md) and [ADR-011](decisions.md).

## Output ordering

Tests run concurrently, so output is printed as each completes — not in source order. The
`[N/total]` counter reflects completion order.

## Failure behaviour

In a batched run, the whole chunk runs and every result is reported (no early stop), so one
failure never hides the others. A run with any failed or errored test exits non-zero
(see [Exit Codes](../api/exit-codes.md)).

## Choosing a strategy

| You run | Strategy | Why |
|---|---|---|
| `tiderace tests/` | Batched | Fast default for runs and CI |
| `tiderace tests/ --coverage` | Batched + contexts | Fast *and* precise dependency graph |
| `tiderace tests/ --isolate` | Isolated | Per-test interpreter isolation |
| `tiderace watch tests/` | Warm pool | Sub-second local feedback loops |
