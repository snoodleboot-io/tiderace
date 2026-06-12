# Using riptide in CI

riptide works in CI out of the box — but whether you feel its speedup comes down to **one
thing: persisting its state between runs.** A fresh CI checkout has no `.riptide.db`, so
without a cache **every CI run is a full cold run** (impact analysis has nothing to compare
against). Cache the state and subsequent runs only re-run what changed.

## Two modes: pick your trade-off

| Mode | Command | What you get | When |
|---|---|---|---|
| **Safe** | `riptide tests/ --all` | Batched parallel run of the **whole** suite every time. No skipping. | Default for CI correctness; protected branches, release pipelines |
| **Fast** | `riptide tests/` (with cached state) | Impact analysis re-runs only affected tests. | PR/feature CI where you want speed and accept that selection is only as good as the dependency graph |

Impact analysis is conservative — it re-runs on any uncertainty (own file changed, never run,
previously failed, or a recorded dependency changed) — so the risk of skipping a test that
should run is low. But it is **only as accurate as the coverage dependency graph**. If you
rely on impact analysis in CI, run with `--coverage` so the graph stays fresh as imports
change (and you get a coverage report for free).

## GitHub Actions

Build riptide from source (until prebuilt binaries are published), cache its state, and run:

```yaml
name: tests
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
      - run: python -m pip install --upgrade pip pytest coverage

      # Build riptide (or download a release binary once published)
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install --git https://github.com/snoodleboot-io/riptide riptide

      # THE important bit: persist impact-analysis state across runs.
      - uses: actions/cache@v4
        with:
          path: |
            .riptide.db
            .riptide-coverage
          key: riptide-${{ github.ref }}-${{ github.sha }}
          restore-keys: |
            riptide-${{ github.ref }}-
            riptide-

      # Fast mode: only impacted tests, graph kept fresh with --coverage.
      - run: riptide tests/ --coverage --python python
```

For **safe mode**, replace the last step with `riptide tests/ --all --coverage` — you still get
batched parallelism, just no skipping.

### Notes on the cache key

- `restore-keys` lets a new commit reuse the most recent cache for the branch (then `main`),
  so the graph carries forward instead of starting cold on every commit.
- The state files are small (SQLite + coverage data). They're safe to cache; if the cache is
  ever stale or missing, riptide just does a full run — never an incorrect one.

## Other CI systems

The pattern is identical anywhere — cache two paths and run riptide:

- **Cache** `.riptide.db` and `.riptide-coverage/` between pipeline runs (GitLab `cache:`,
  CircleCI `save_cache`/`restore_cache`, etc.).
- **Run** `riptide tests/ [--all] --coverage --python <interpreter>`.
- **Fail the build** on test failures — riptide exits non-zero when any test fails or errors
  (see [Exit Codes](../api/cli.md)). No extra wiring needed.

## What *not* to use in CI

`riptide watch` and the warm worker pool are a **local development** convenience — they keep
a long-lived process and share interpreter state across runs, which suits an editor loop, not
a one-shot CI job. CI should use a normal run (fresh subprocesses), or `--isolate` if a suite
needs strict per-test isolation. See [ADR-009](../design/decisions.md).

## First run / cache miss

A cold run (no cache, or after `riptide clear`) runs the whole suite and **builds** the graph.
That run is the slow one; it is correct, just not accelerated. Every cached run after it is
where riptide pays you back.
