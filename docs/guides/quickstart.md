# Quick Start

From an install to the warm inner loop in a few minutes. tiderace is a **pure-Rust test engine** —
it runs your Python tests directly, with **no pytest at runtime** — and it runs an unmodified
pytest suite: fixtures, marks, parametrize, conftests, plugin fixtures.

## 1. Install

```bash
pip install tiderace        # or: uv pip install tiderace
```

That ships two binaries, `tiderace` and `tiderace-daemon`, with the Python shim bundled — no
`TIDERACE_SHIM`, no configuration. Install it into the interpreter your tests run under, or point
`TIDERACE_PYTHON` at that interpreter. Requires Python 3.12+.

Building from source instead (`cd engine && cargo build --release`) leaves the binaries under
`engine/target/release/` and needs `TIDERACE_SHIM` pointed at `engine/py-shim/shim.py`; see
[Installation](installation.md).

!!! info "Platforms"
    Linux, macOS, and **Windows** are all supported. Windows has no `fork()`, so isolation there is
    no-fork + snapshot/restore, and the opt-in [sub-interpreter tier](configuration.md#windows-parallelism-the-sub-interpreter-tier-opt-in)
    (CPython 3.14+) adds parallel no-fork execution. The daemon's socket, and with it
    `tiderace daemon`, is Unix-only.

## 2. Run the suite

```bash
tiderace run tests/
```

```
tiderace: strategy=fork scheduler=locality workers=8 timeout=60000ms optimistic-no-fork shared-import
PASS	tests/test_auth.py::test_login
…
5036 passed, 0 failed, 0 error, 0 skipped, 5036 total
```

The same tests pytest would collect, the same outcomes, the pytest-style exit code (`0` green,
`1` on any failure). Behind that line, tiderace:

1. Collected the tests with a regex scan of the files — no Python started yet.
2. Imported the suite **once**, in one process, and forked a worker per core from that image.
3. Handed out the files as work units, each file's tests in file order on one worker, and ran every
   test through the [isolation ladder](../design/architecture.md#the-isolation-ladder): in-process
   when the test's module can be snapshotted and restored, forked only when it cannot.

`-k EXPR`, `-m EXPR`, `--strict-markers`, `--workers N`, `--timeout MS` and `--report path.json`
do what you expect; the [CLI reference](../api/cli.md) has the rest.

## 3. Keep it warm

The suite's import graph is most of what a run pays before the first test starts. Keep a daemon
for the tree and later runs skip it:

```bash
tiderace daemon start tests/     # once per session
tiderace run tests/              # "tiderace: … via daemon"
tiderace run -k test_login tests/
```

A `-k` run of one test on a 5,600-test suite is **0.6s** through the daemon and 5s without. The
daemon re-imports its image whenever a `.py` or pytest config file under the tree changes, so a
stale module is never executed. `tiderace daemon status` says whether one is serving and whether
its image is warm; `tiderace daemon stop` ends it. Set `TIDERACE_NO_DAEMON=1` for a run that must
not share an image with earlier runs — a gate.

## 4. Only run what changed

`tiderace run` always runs what you asked for. The daemon's own `run` mode runs what an edit
**touched**: the first pass records every test's source footprint, and later passes hash the
files and re-run only the tests whose recorded dependencies changed.

```bash
tiderace-daemon run tests/        # first pass: everything runs, footprints recorded
tiderace-daemon run tests/        # nothing changed: nothing runs
```

```
0 ran, 5602 cached, 5602 total, 0 failing
```

Edit a source file and run again:

```
4 ran, 5598 cached, 5602 total, 0 failing
```

Selection is by recorded footprint, not by guess, and conservative: a test re-runs when its own
file changed, when a recorded dependency changed, or when it has no footprint yet. A hub module
that thousands of tests depend on re-runs thousands of tests. `run --all` forces the whole suite
— the CI gate. The state lives in `.tiderace-state.json`; add it and `.tiderace-cache/` to
`.gitignore`.

## 5. The editor loop — `watch`

```bash
tiderace-daemon watch tests/
```

```
watching tests/ (Ctrl-C to stop)…
src/auth.py: Ran(2)
test_auth.py: Recollected(5)
conftest.py: Recycled(12)
```

Each save classifies the change — source edit, test file, conftest — and does the minimum: re-run
the impacted tests, re-collect the file, recycle the interpreter. `watch` and `daemon start` keep a
long-lived warm process, so they are local-development tools; CI runs a fresh `run` / `run --all`.
See [Watch Mode](watch.md).

## Next steps

- [Configuration](configuration.md) — every environment variable and the project config tiderace reads
- [Migrating from pytest](migration.md) — what runs unchanged, and the native API
- [CI](ci.md) — safe vs fast modes, caching `.tiderace-state.json`
- [Benchmarks](benchmarks.md) — eight real suites against pytest and pytest-xdist
