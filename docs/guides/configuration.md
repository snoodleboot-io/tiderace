# Configuration

The pure-Rust engine is **environment-driven**, not flag-heavy. Almost everything you'd configure is
an environment variable that the engine reads and passes through to the wellspring (the warm CPython
child). There is exactly one command-line flag — `--all` — on the daemon's `run` mode.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `TIDERACE_SHIM` | — (**required**) | Path to `py-shim/shim.py`. The engine launches CPython with this shim; it imports your code and invokes test bodies. Without it the binaries exit with an error. |
| `TIDERACE_PYTHON` | `python3` | The Python interpreter to run. Point this at your project's venv if your tests need installed dependencies. |
| `TIDERACE_COVERAGE` | off | Record per-test coverage via `sys.monitoring`. The daemon **sets this automatically** for impact-aware `run` (it needs the footprint to know what to skip next time). Set it yourself only for ad-hoc coverage. |
| `TIDERACE_RESTORE` | set by daemon | Enables the no-fork + snapshot/restore isolation path. The daemon sets `TIDERACE_RESTORE=1` on every mode — it's the **default** execution model, not an opt-in. Nothing to choose. |
| `TIDERACE_FORCE_FORK` | off | **Debug / benchmark only.** Reverts to `fork()`-per-test isolation, bypassing the no-fork ladder. Use it to A/B the ladder or chase an isolation bug — not in normal use. |
| `TIDERACE_CACHE_DIR` | off | Directory for the **content-addressed result cache** (ADR-E004). Point it at a CI cache path / shared mount / artifact dir and a *pure* test's outcome computed on one machine is served without re-running on any other with the same inputs — even when local impact state is stale. Off ⇒ impact-skip only. |
| `TIDERACE_SUBINTERP` | off | Opt into the **sub-interpreter tier** (ADR-E015) on `run --all`: sub-interpreter-*safe* modules run through a parallel sub-interpreter pool (no fork), the rest through the ordinary pool. Its purpose is **Windows** parallelism (no `fork()` there); on Linux the fork pool already parallelizes, so it measures at parity. Requires CPython 3.14+. |
| `TIDERACE_SUBINTERP_WORKERS` | CPU count | Size of the sub-interpreter pool when `TIDERACE_SUBINTERP=1`. |
| `TIDERACE_SOCKET` | `<tmp>/tiderace-daemon.sock` | `serve` mode: the Unix socket path the RPC server binds. |
| `TIDERACE_REQUIRE_LIVE` | off | Testing/CI: make the engine's own *live* test scenarios **fail** instead of self-skipping when their interpreter/venv is absent. Set in the CI jobs that provision Python, so a broken test environment can't pass as a silent no-op. Not needed to *use* tiderace. |

```bash
# A typical setup
export TIDERACE_SHIM="$PWD/engine/py-shim/shim.py"
export TIDERACE_PYTHON="$PWD/.venv/bin/python"
```

## The isolation default

No-fork + restore is **on by default** — there is no flag to enable it. The daemon requests no-fork
on every test and the shim runs it in-process, undoing any mutation from a pre-body snapshot; a
module it can't snapshot (opaque globals) automatically falls back to `fork()` for soundness. So a
wrong guess can only change speed, never correctness.

`TIDERACE_FORCE_FORK=1` is the escape hatch back to fork-per-test, kept purely as a debug and
benchmark baseline. See the [isolation ladder](../design/architecture.md#the-isolation-ladder).

## Windows parallelism: the sub-interpreter tier (opt-in)

On Windows there's no `fork()`, so the pool runs no-fork and **sequentially** within each process. The
**sub-interpreter tier** (ADR-E015) recovers parallelism there: with `TIDERACE_SUBINTERP=1` on
`run --all`, tiderace probes which modules are safe to import into an isolated sub-interpreter and runs
that safe subset across a parallel sub-interpreter pool (per-interpreter GIL, PEP 684 — genuine
parallelism, no fork); numpy-style modules that can't load isolated take the ordinary pool.

```bash
TIDERACE_SUBINTERP=1 tiderace-daemon run <tests> --all      # sizes the pool to CPU count
TIDERACE_SUBINTERP=1 TIDERACE_SUBINTERP_WORKERS=4 tiderace-daemon run <tests> --all
```

It's **opt-in** because the win is Windows-specific — on Linux the fork pool already parallelizes, so
the tier measures at parity and buys nothing. Needs CPython 3.14+ (`concurrent.interpreters`). Details:
[Execution Model](../design/parallel-execution.md#the-sub-interpreter-tier-windows-parallelism).

## The one flag: `--all`

```bash
tiderace-daemon run  <tests>          # impact-aware: only changed tests; coverage on; state persisted
tiderace-daemon run  <tests> --all    # full parallel run; opts out of impact-skip and coverage
```

- Plain `run` is **impact-aware**: it reads `.tiderace-state.json`, runs only tests whose deps
  changed, and re-persists the state. With no changes, nothing runs.
- `run --all` forces a full run across the parallel pool — your CI safe mode, or a clean baseline.

The one-shot `tiderace` binary has no impact analysis: `tiderace collect <path>` lists tests,
`tiderace run <path>` runs them all once.

## State file & `.gitignore`

Impact analysis persists to **`.tiderace-state.json`** in the directory you run from — each test's
dependency files plus per-file content hashes. It is machine-local; every developer and CI runner
keeps their own. Ignore it:

```gitignore
# tiderace impact-analysis state — machine-local, do not commit
.tiderace-state.json
```

## Future: pyproject configuration

!!! note "Not yet"
    There is currently **no** `pyproject.toml` / config-file support — configuration is entirely
    through the environment variables above. A native `[tool.tiderace]` section may arrive later;
    it does not exist today.
