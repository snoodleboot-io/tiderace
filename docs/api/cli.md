# CLI Reference

tiderace ships **two** binaries. Both are thin front-ends over `engine-core` (the Rust engine that
owns all the logic); the binaries add no behaviour of their own.

| Binary | Crate | Role |
|---|---|---|
| `riptide` | `engine-cli` | one-shot `collect` / `run` |
| `riptide-daemon` | `engine-daemon` | warm server: impact-aware `run`, full `run --all`, `serve` (RPC), `watch`, `bench`, `probe` |

!!! info "Naming"
    The product is **tiderace**. The engine binaries build as `riptide` / `riptide-daemon` — a retired
    codename pending a mechanical rename. Read them as tiderace.

## Environment

The binaries are configured entirely through environment variables (there are no config files or
long-option flags). All `RIPTIDE_*` names are read directly by the binaries / the wellspring child.

| Variable | Used by | Default | Meaning |
|---|---|---|---|
| `RIPTIDE_SHIM` | `riptide run`, all `riptide-daemon` modes | — (**required**) | Path to `py-shim/shim.py`, the Python executor. Missing ⇒ error + exit. |
| `RIPTIDE_PYTHON` | `riptide run`, all `riptide-daemon` modes | `python3` | The interpreter the wellspring launches. |
| `RIPTIDE_SOCKET` | `riptide-daemon serve` | `<tmp>/riptide-daemon.sock` | Unix-socket path for the RPC server. |
| `RIPTIDE_COVERAGE` | wellspring (set by `riptide-daemon run`) | off | Capture each test's source footprint via `sys.monitoring`. Set automatically by impact-aware `run`; cleared by `run --all`. |
| `RIPTIDE_RESTORE` | wellspring (set by all `riptide-daemon` modes) | on (daemon) | Enable the no-fork + snapshot/restore isolation ladder (the default execution path). |
| `RIPTIDE_FORCE_FORK` | wellspring | off | Debug/benchmark only: fork every test, bypassing the no-fork ladder. **Not a user flag.** |
| `RIPTIDE_SUBINTERP` | `riptide-daemon run --all` | off | Opt into the **sub-interpreter tier** (ADR-E015): sub-interpreter-*safe* modules run through a parallel sub-interpreter pool (no fork), the rest through fork. Its purpose is **Windows** parallelism (no `fork()` there); on Linux the fork pool already parallelizes, so it's ~parity. `RIPTIDE_SUBINTERP_WORKERS` sets the pool size (default: CPU count). |

The wellspring is a child process and inherits the parent's environment, so the engine sets
`RIPTIDE_COVERAGE` / `RIPTIDE_RESTORE` for the Python side; you normally only set `RIPTIDE_SHIM` and
`RIPTIDE_PYTHON` yourself.

```bash
export RIPTIDE_SHIM="$PWD/py-shim/shim.py"
export RIPTIDE_PYTHON="$(which python3)"
```

---

## `riptide` — one-shot CLI

```
riptide <collect|run> <path>
```

There are no options; the second argument is the test root. A missing/unknown command or a missing
path argument is a usage error (exit `64`).

### `riptide collect <path>`

Discover tests under `<path>` via the Rust regex collector and print each node id and its style. Does
not launch Python. Prints `collected N tests` to stderr.

```bash
riptide collect tests/
```

```
tests/test_auth.py::test_login	Function
tests/test_auth.py::TestRegistration::test_valid_email	ClassMethod
collected 2 tests
```

### `riptide run <path>`

Collect, launch a warm wellspring (`RIPTIDE_PYTHON` importing the project once), fork-execute every
test through it, print a per-test report, and exit with the pytest-style code (`0` all green, `1` on
any failure/error). Requires `RIPTIDE_SHIM`.

```bash
RIPTIDE_SHIM=py-shim/shim.py riptide run tests/
```

```
PASS	tests/test_auth.py::test_login
FAIL	tests/test_auth.py::test_logout
1 passed, 1 failed, 0 error, 0 skipped, 2 total
```

---

## `riptide-daemon` — warm server

```
riptide-daemon <run|serve|watch|bench|probe> <root> [--all] [iters]
```

All modes require `RIPTIDE_SHIM`. A missing root or unknown mode is a usage error (exit `64`). Every
mode sets `RIPTIDE_RESTORE=1`, so the no-fork isolation ladder is on by default.

### `riptide-daemon run <root>` — impact-aware (default)

The inner-loop one-shot. Runs **only** the tests whose dependencies changed since the last run
(reading `<root>/.riptide-state.json`); the rest are served from warm state. Coverage is enabled
(`RIPTIDE_COVERAGE=1` is set) so each run records footprints. With no changes, nothing executes and
the wellspring is never launched. Prints `R ran, C cached, T total, F failing`; exits `0` if no
failures, else `1`.

```bash
RIPTIDE_SHIM=py-shim/shim.py riptide-daemon run tests/
```

```
3 ran, 506 cached, 509 total, 0 failing
```

### `riptide-daemon run <root> --all` — full parallel run

Bypasses impact analysis: runs every collected test across the parallel pool (N wellsprings, one per
core). Coverage is **not** enabled in this mode. Prints one `OUTCOME\tnode_id` line per test and a
`N tests, F failing (parallel pool)` summary; exits `0` / `1`.

```bash
RIPTIDE_SHIM=py-shim/shim.py riptide-daemon run tests/ --all
```

```
PASS	tests/test_auth.py::test_login
PASS	tests/test_auth.py::test_logout
2 tests, 0 failing (parallel pool)
```

### `riptide-daemon serve <root>` — RPC server (Unix only)

Bind the per-project Unix socket and answer JSON-RPC requests over a persistent warm session until
`Shutdown`. The socket path comes from `RIPTIDE_SOCKET`, defaulting to
`<tmp>/riptide-daemon.sock`. Methods: `Discover`, `Run`, `Watch`, `Recycle`, `Health`, `Shutdown`
(see [Schema](schema.md) for the wire format). On non-Unix platforms this mode is unavailable and
exits `64`.

```bash
RIPTIDE_SHIM=py-shim/shim.py RIPTIDE_SOCKET=/tmp/td.sock riptide-daemon serve tests/
```

```
serving on /tmp/td.sock …
```

### `riptide-daemon watch <root>` — inner loop

Discover the candidate tests, then block and watch the tree (50 ms debounce). On each save, re-run
only the impacted tests using the DepGraph (which tightens as coverage accrues; the first edits
conservatively re-run all). Each filesystem event prints `path: Action`. Ctrl-C to stop.

```bash
RIPTIDE_SHIM=py-shim/shim.py riptide-daemon watch tests/
```

```
watching tests/ (Ctrl-C to stop)…
tests/test_auth.py: Modify
```

### `riptide-daemon bench <root> [iters]` — cold-vs-warm timing

Run the whole corpus `iters` times (default `5`) on one warm handler, timing each pass. Iter 0
includes the wellspring launch (cold); the rest reuse the warm import (warm). Exits `0` on success.

```bash
RIPTIDE_SHIM=py-shim/shim.py riptide-daemon bench tests/ 10
```

### `riptide-daemon probe <root>` — sub-interpreter safety classification

Classify each collected module as **sub-interpreter-safe** (ADR-E015): it imports the module in an
isolated sub-interpreter (`concurrent.interpreters`, CPython 3.14+) and prints `safe` / `UNSAFE` /
`unknown` per module. Read-only, runs nothing — the foundation for the sub-interpreter execution tier
(Windows parallelism). `unknown` on interpreters without the API (→ callers fall back to fork).

```bash
RIPTIDE_SHIM=py-shim/shim.py RIPTIDE_PYTHON=python3.14 riptide-daemon probe tests/
# safe    tests/test_pure.py
# UNSAFE  tests/test_uses_numpy.py
```

```
iter 0: 509 tests in 412.7 ms  [cold (+wellspring launch)]
iter 1: 509 tests in 9.4 ms  [warm]
…
```

---

**Exit codes:** see [Exit Codes](exit-codes.md).
