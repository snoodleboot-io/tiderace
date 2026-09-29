# CLI Reference

tiderace ships **two** binaries. Both are thin front-ends over `engine-core` (the Rust engine that
owns all the logic); the binaries add no behaviour of their own.

| Binary | Crate | Role |
|---|---|---|
| `tiderace` | `engine-cli` | one-shot `collect` / `run` |
| `tiderace-daemon` | `engine-daemon` | warm server: impact-aware `run`, full `run --all`, `serve` (RPC), `watch`, `bench`, `probe` |

## Environment

The binaries are configured entirely through environment variables (there are no config files or
long-option flags). All `TIDERACE_*` names are read directly by the binaries / the wellspring child.

| Variable | Used by | Default | Meaning |
|---|---|---|---|
| `TIDERACE_SHIM` | `tiderace run`, all `tiderace-daemon` modes | — (**required**) | Path to `py-shim/shim.py`, the Python executor. Missing ⇒ error + exit. |
| `TIDERACE_PYTHON` | `tiderace run`, all `tiderace-daemon` modes | `python3` | The interpreter the wellspring launches. |
| `TIDERACE_SOCKET` | `tiderace-daemon serve` | `<tmp>/tiderace-<uid>/<digest of the root>.sock` | Unix-socket path for the RPC server. `tiderace run`/`daemon` only look at the default. |
| `TIDERACE_DAEMON_BIN` | `tiderace daemon start` | `tiderace-daemon` beside `tiderace` | The daemon binary to spawn. |
| `TIDERACE_PLUGINS` | shim (all modes) | all installed | Which pytest plugins' fixtures to take: `none`, or a comma-separated allow-list of plugin names. `[tool.tiderace] plugins = [...]` is the config spelling. See [Configuration](../guides/configuration.md). |
| `TIDERACE_NO_DAEMON` | `tiderace run` | unset | Set to anything to run in this process even when a daemon is serving the root — for a gate that must not share an image with earlier runs. A filtered run (`-k`, `-m`) goes through the daemon like any other. |
| `TIDERACE_COVERAGE` | wellspring (set by `tiderace-daemon run`) | off | Capture each test's source footprint via `sys.monitoring`. Set automatically by impact-aware `run`; cleared by `run --all`. |
| `TIDERACE_RESTORE` | wellspring (set by all `tiderace-daemon` modes) | on (daemon) | Enable the no-fork + snapshot/restore isolation ladder (the default execution path). |
| `TIDERACE_FORCE_FORK` | wellspring | off | Debug/benchmark only: fork every test, bypassing the no-fork ladder. **Not a user flag.** |
| `TIDERACE_SUBINTERP` | `tiderace-daemon run --all` | off | Opt into the **sub-interpreter tier** (ADR-E015): sub-interpreter-*safe* modules run through a parallel sub-interpreter pool (no fork), the rest through fork. Its purpose is **Windows** parallelism (no `fork()` there); on Linux the fork pool already parallelizes, so it's ~parity. Requires CPython 3.14+. |
| `TIDERACE_SUBINTERP_WORKERS` | `tiderace-daemon run --all` (when `TIDERACE_SUBINTERP=1`) | CPU count | Size of the sub-interpreter pool. |
| `TIDERACE_CACHE_DIR` | `tiderace-daemon run` | off | Directory for the **content-addressed result cache** (ADR-E004): a *pure* test's outcome computed elsewhere with the same inputs is served without re-running. Point it at a CI cache / shared mount. Off ⇒ impact-skip only. |
| `TIDERACE_REQUIRE_LIVE` | test harness / CI | off | Make the engine's own *live* test scenarios **fail** instead of self-skipping when their interpreter/venv is absent — so a broken test environment can't pass as a silent no-op. Not needed to use tiderace. |

The wellspring is a child process and inherits the parent's environment, so the engine sets
`TIDERACE_COVERAGE` / `TIDERACE_RESTORE` for the Python side; you normally only set `TIDERACE_SHIM` and
`TIDERACE_PYTHON` yourself.

```bash
export TIDERACE_SHIM="$PWD/py-shim/shim.py"
export TIDERACE_PYTHON="$(which python3)"
```

---

## `tiderace` — one-shot CLI

```
tiderace <collect|run|daemon> [options] <path>
```

The last argument is the test root. A missing/unknown command or a missing path argument is a usage
error (exit `64`).

### `tiderace collect <path>`

Discover tests under `<path>` via the Rust regex collector and print each node id and its style. Does
not launch Python. Prints `collected N tests` to stderr.

```bash
tiderace collect tests/
```

```
tests/test_auth.py::test_login	Function
tests/test_auth.py::TestRegistration::test_valid_email	ClassMethod
collected 2 tests
```

### `tiderace run <path>`

Collect, launch a warm wellspring (`TIDERACE_PYTHON` importing the project once), fork-execute every
test through it, print a per-test report, and exit with the pytest-style code (`0` all green, `1` on
any failure/error). Requires `TIDERACE_SHIM`.

When a daemon is serving `<path>` (see [`tiderace daemon`](#tiderace-daemon-startstatusstop-path--a-warm-image-for-run)),
the `run` is handed to it instead and the header says `via daemon`: the same results, the same
report and exit code, but the workers fork from an image that already imported the suite. `-k`,
`-m` and `--strict-markers` travel with the request and are applied by those workers after the
fork (TID-90), so running one test by name pays neither start-up nor imports. A run with
`TIDERACE_NO_DAEMON` set stays in this process.

```bash
TIDERACE_SHIM=py-shim/shim.py tiderace run tests/
```

```
PASS	tests/test_auth.py::test_login
FAIL	tests/test_auth.py::test_logout
1 passed, 1 failed, 0 error, 0 skipped, 2 total
```

### `tiderace daemon <start|status|stop> <path>` — a warm image for `run`

Keep a `tiderace-daemon serve` for `<path>` alive between runs (TID-84). The daemon imports the
suite once and holds that image; every later `tiderace run <path>` forks its workers from it, so the
run pays scheduling and execution but no interpreter start-up and no imports — on a 5,600-test
monorepo suite, 24s instead of 28s. The image is dropped and re-imported when any `.py` or pytest
config file under `<path>` changes (path, size or mtime), so a stale module is never executed —
which is why this helps the runs that *don't* edit the tree: a re-run, a CI-style gate on an
unchanged tree. Any edit still pays the import.

| Verb | Does |
|---|---|
| `start` | Spawn `tiderace-daemon serve <path>` in its own process group (found beside this binary, or at `TIDERACE_DAEMON_BIN`), log to `<path>/.tiderace-cache/daemon.log`, and wait up to 30s for it to answer. A no-op when one already serves the path. |
| `status` | Print whether one is serving and whether its image is warm; exit `1` when none is. |
| `stop` | Ask it to shut down; exit `1` when none is serving. |

```bash
tiderace daemon start tests/     # once per session
tiderace run tests/              # "tiderace: ... via daemon"
tiderace daemon stop tests/
```

Unix only: the daemon answers on `<tmp>/tiderace-<uid>/<digest of the path>.sock` — not under
the project, since a Unix socket path is limited to ~108 bytes and a project's path can be longer;
the log names it. The daemon shares the
interpreter image across runs of the same tree; see [When not to use watch](../guides/watch.md#when-not-to-use-it)
for why a CI gate should stay a one-shot.

---

## `tiderace-daemon` — warm server

```
tiderace-daemon <run|serve|watch|bench|probe> <root> [--all] [iters]
```

All modes require `TIDERACE_SHIM`. A missing root or unknown mode is a usage error (exit `64`). Every
mode sets `TIDERACE_RESTORE=1`, so the no-fork isolation ladder is on by default.

### `tiderace-daemon run <root>` — impact-aware (default)

The inner-loop one-shot. Runs **only** the tests whose dependencies changed since the last run
(reading `<root>/.tiderace-state.json`); the rest are served from warm state. Coverage is enabled
(`TIDERACE_COVERAGE=1` is set) so each run records footprints. With no changes, nothing executes and
the wellspring is never launched. Prints `R ran, C cached, T total, F failing`; exits `0` if no
failures, else `1`.

```bash
TIDERACE_SHIM=py-shim/shim.py tiderace-daemon run tests/
```

```
3 ran, 506 cached, 509 total, 0 failing
```

### `tiderace-daemon run <root> --all` — full parallel run

Bypasses impact analysis: runs every collected test across the parallel pool (N wellsprings, one per
core). Coverage is **not** enabled in this mode. Prints one `OUTCOME\tnode_id` line per test and a
`N tests, F failing (parallel pool)` summary; exits `0` / `1`.

```bash
TIDERACE_SHIM=py-shim/shim.py tiderace-daemon run tests/ --all
```

```
PASS	tests/test_auth.py::test_login
PASS	tests/test_auth.py::test_logout
2 tests, 0 failing (parallel pool)
```

### `tiderace-daemon serve <root>` — RPC server (Unix only)

Bind the per-project Unix socket and answer JSON-RPC requests over a persistent warm session until
`Shutdown`. The socket path comes from `TIDERACE_SOCKET`, defaulting to
`<tmp>/tiderace-<uid>/<digest of the root>.sock` — where `tiderace run` and `tiderace daemon` look
for it.
Methods: `Discover`, `Run`, `RunFull`, `Watch`, `Recycle`, `Health`, `Shutdown` (see
[Schema](schema.md) for the wire format). `RunFull` is the full parallel run `tiderace run` performs,
served from the warm image and answered with the engine's per-node results. On non-Unix platforms this mode is unavailable and
exits `64`.

```bash
TIDERACE_SHIM=py-shim/shim.py TIDERACE_SOCKET=/tmp/td.sock tiderace-daemon serve tests/
```

```
serving on /tmp/td.sock …
```

### `tiderace-daemon watch <root>` — inner loop

Discover the candidate tests, then block and watch the tree (50 ms debounce). On each save, re-run
only the impacted tests using the DepGraph (which tightens as coverage accrues; the first edits
conservatively re-run all). Each filesystem event prints `path: Action`. Ctrl-C to stop.

```bash
TIDERACE_SHIM=py-shim/shim.py tiderace-daemon watch tests/
```

```
watching tests/ (Ctrl-C to stop)…
tests/test_auth.py: Modify
```

### `tiderace-daemon bench <root> [iters]` — cold-vs-warm timing

Run the whole corpus `iters` times (default `5`) on one warm handler, timing each pass. Iter 0
includes the wellspring launch (cold); the rest reuse the warm import (warm). Exits `0` on success.

```bash
TIDERACE_SHIM=py-shim/shim.py tiderace-daemon bench tests/ 10
```

### `tiderace-daemon probe <root>` — sub-interpreter safety classification

Classify each collected module as **sub-interpreter-safe** (ADR-E015): it imports the module in an
isolated sub-interpreter (`concurrent.interpreters`, CPython 3.14+) and prints `safe` / `UNSAFE` /
`unknown` per module. Read-only, runs nothing — the foundation for the sub-interpreter execution tier
(Windows parallelism). `unknown` on interpreters without the API (→ callers fall back to fork).

```bash
TIDERACE_SHIM=py-shim/shim.py TIDERACE_PYTHON=python3.14 tiderace-daemon probe tests/
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
