# Quick Start

This walks you from a fresh build to the warm inner loop in a few minutes. tiderace is a
**pure-Rust test engine** — it runs your Python tests directly, with **no pytest at runtime**.

!!! info "Naming"
    The product is **tiderace**. The engine binaries currently build as `riptide` and
    `riptide-daemon` (a retired codename being consolidated under tiderace) — read them as
    tiderace. The legacy `tiderace` binary that orchestrated pytest is the previous generation.

## 1. Build the engine

tiderace builds from source from the `engine/` Cargo workspace. You need a Rust toolchain and a
Python 3.12+ interpreter (the engine uses CPython's `sys.monitoring` for coverage).

```bash
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace/engine
cargo build --release
```

This produces two binaries under `engine/target/release/`:

- `riptide` — one-shot CLI (`collect`, `run`).
- `riptide-daemon` — the warm server (`run`, `run --all`, `serve`, `watch`, `bench`).

## 2. Point the engine at Python

The engine is **env-driven**. Two variables matter to start:

```bash
# Required: the Python shim the engine runs inside CPython (it imports your code & invokes bodies).
export RIPTIDE_SHIM="$PWD/py-shim/shim.py"

# Optional: the interpreter (defaults to python3). Use your project's venv if it has deps.
export RIPTIDE_PYTHON="$(which python3)"
```

`RIPTIDE_SHIM` is mandatory — without it the binaries exit with an error. See
[Configuration](configuration.md) for the full set of variables.

## 3. First run — everything executes

Point the daemon at your tests. The impact-aware `run` does a full pass the first time (there's no
prior state to compare against), recording each test's coverage footprint:

```bash
./target/release/riptide-daemon run /path/to/tests
```

```
12 ran, 0 cached, 12 total, 0 failing
```

Behind that line, tiderace:

1. Collected your tests via fast regex scanning (Rust).
2. Built the fixture closure per test (Rust).
3. Launched a warm wellspring per core and ran every test through the
   [isolation ladder](../design/architecture.md#the-isolation-ladder) — pure tests in-process, the
   rest snapshot/restored, only opaque modules forked.
4. Captured per-test coverage via `sys.monitoring` and persisted it to **`.riptide-state.json`**
   (per-test deps + file content hashes).

## 4. Second run — nothing changes, nothing runs

Run the exact same command again without touching any files:

```bash
./target/release/riptide-daemon run /path/to/tests
```

```
0 ran, 12 cached, 12 total, 0 failing
```

**Zero tests execute** — tiderace hashes the files, sees nothing changed, and serves the prior
outcomes. With no changes the warm interpreter isn't even launched. This is the impact-skip path.

## 5. After an edit — only impacted tests re-run

Edit a test file or a source file it depends on, then run again:

```bash
# edit src/auth.py, then:
./target/release/riptide-daemon run /path/to/tests
```

```
2 ran, 10 cached, 12 total, 0 failing
```

Only the tests whose recorded dependencies include the changed file re-execute. Impact analysis is
**conservative**: a test is re-run when its own file changed, when a recorded dependency changed, or
when it has no recorded footprint yet (e.g. the very first run).

## 6. Force a full run

When you want every test to execute regardless of state — a clean baseline, or a CI gate — use
`run --all`:

```bash
./target/release/riptide-daemon run /path/to/tests --all
```

This runs the whole suite across the parallel pool. (`--all` opts out of the impact-skip and its
coverage recording; use plain `run` to keep the dependency graph fresh.)

## 7. The inner loop — `watch`

For an editor loop, keep the interpreter warm and re-run only what each save impacts:

```bash
./target/release/riptide-daemon watch /path/to/tests
```

```
watching /path/to/tests (Ctrl-C to stop)…
src/auth.py: Ran(2)
test_auth.py: Recollected(5)
conftest.py: Recycled(12)
```

Each save classifies the change (source edit → re-run impacted; test file → re-collect; conftest →
recycle the warm interpreter) and does the **minimum** work — millisecond feedback. `watch` keeps a
long-lived warm process, so it's a **local-dev** tool; CI should use fresh `run` / `run --all`. See
[Watch Mode](watch.md).

## Next steps

- [Configuration](configuration.md) — every environment variable and the `--all` flag
- [Watch Mode](watch.md) — the warm inner loop in detail
- [CI](ci.md) — safe vs fast modes, caching `.riptide-state.json`
- [How impact analysis works](../design/impact-analysis.md)
- [Benchmarks](benchmarks.md) — run the comparison yourself
