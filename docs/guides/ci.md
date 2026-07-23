# Using tiderace in CI

tiderace works in CI out of the box. Whether you feel its speedup comes down to **one thing:
persisting `.riptide-state.json` between runs.** A fresh CI checkout has no state, so without a cache
**every run is a full cold run** — impact analysis has nothing to compare against. Cache the state
and subsequent runs re-run only what changed.

!!! info "Naming"
    The binaries currently build as `riptide` / `riptide-daemon` — a retired codename being
    consolidated under tiderace. Read them as tiderace.

## Two modes: pick your trade-off

| Mode | Command | What you get | When |
|---|---|---|---|
| **Safe** | `riptide-daemon run <tests> --all` | Full parallel run of the whole suite **every time**. No skipping, no dependence on prior state. | Protected branches, release pipelines, anywhere correctness must not depend on a cache. |
| **Fast** | `riptide-daemon run <tests>` (with cached state) | Impact-aware: re-runs only tests whose deps changed since the cached run; coverage kept fresh automatically. | PR / feature CI where you want speed and accept that selection is only as good as the cached graph. |

Impact analysis is **conservative** — it runs a test whenever its own file changed, a recorded
dependency changed, or it has no recorded footprint yet (first run / cache miss). So a cold or
stale cache is never *incorrect*, only un-accelerated.

## GitHub Actions

Build the engine from source (until prebuilt binaries are published), cache `.riptide-state.json`,
and run:

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
          python-version: "3.12"          # 3.12+ required (sys.monitoring coverage); 3.14+ for the sub-interpreter tier

      - uses: dtolnay/rust-toolchain@stable

      # Build the engine from the engine/ workspace.
      - name: Build tiderace
        run: cargo build --release --manifest-path engine/Cargo.toml

      - name: Configure the engine
        run: |
          echo "RIPTIDE_SHIM=$PWD/engine/py-shim/shim.py" >> "$GITHUB_ENV"
          echo "RIPTIDE_PYTHON=$(which python)"           >> "$GITHUB_ENV"

      # THE important bit for fast mode: persist impact-analysis state across runs.
      - uses: actions/cache@v4
        with:
          path: .riptide-state.json
          key: tiderace-${{ github.ref }}-${{ github.sha }}
          restore-keys: |
            tiderace-${{ github.ref }}-
            tiderace-

      # Fast mode: impact-aware run (coverage on, state persisted).
      - run: ./engine/target/release/riptide-daemon run tests/
```

For **safe mode**, swap the last step for a forced full run (and you can drop the cache step):

```yaml
      - run: ./engine/target/release/riptide-daemon run tests/ --all
```

### Notes on the cache key

- `restore-keys` lets a new commit reuse the most recent state for the branch (then any branch), so
  the dependency graph carries forward instead of starting cold on every commit.
- `.riptide-state.json` is small (per-test deps + content hashes). It's safe to cache: a stale or
  missing cache just produces a full run — never an incorrect result.

## Other CI systems

The pattern is identical anywhere — cache one path, run the daemon:

- **Cache** `.riptide-state.json` between pipeline runs (GitLab `cache:`, CircleCI
  `save_cache` / `restore_cache`, etc.).
- **Run** `riptide-daemon run tests/` (fast) or `riptide-daemon run tests/ --all` (safe).
- **Fail the build** on test failures — `run` exits non-zero when any test fails or errors. No extra
  wiring needed.

## What *not* to use in CI

`riptide-daemon watch` and the long-lived `serve` session are **local-development** tools — they
keep a warm process and share interpreter state across runs, which suits an editor loop, not a
one-shot CI job. CI should use a fresh `run` (impact-aware) or `run --all` (full). See
[Watch Mode](watch.md).

## First run / cache miss

A cold run (no cached state) runs the whole suite and **builds** the dependency graph. That run is
the slow one; it is correct, just not accelerated. Every cached run after it is where tiderace pays
you back.
