# Configuration

The pure-Rust engine is **environment-driven**, not flag-heavy. Almost everything you'd configure is
an environment variable that the engine reads and passes through to the wellspring (the warm CPython
child). There is exactly one command-line flag — `--all` — on the daemon's `run` mode.

!!! info "Naming"
    The binaries currently build as `riptide` / `riptide-daemon` — a retired codename being
    consolidated under tiderace. Read them as tiderace.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `RIPTIDE_SHIM` | — (**required**) | Path to `py-shim/shim.py`. The engine launches CPython with this shim; it imports your code and invokes test bodies. Without it the binaries exit with an error. |
| `RIPTIDE_PYTHON` | `python3` | The Python interpreter to run. Point this at your project's venv if your tests need installed dependencies. |
| `RIPTIDE_COVERAGE` | off | Record per-test coverage via `sys.monitoring`. The daemon **sets this automatically** for impact-aware `run` (it needs the footprint to know what to skip next time). Set it yourself only for ad-hoc coverage. |
| `RIPTIDE_RESTORE` | set by daemon | Enables the no-fork + snapshot/restore isolation path. The daemon sets `RIPTIDE_RESTORE=1` on every mode — it's the **default** execution model, not an opt-in. Nothing to choose. |
| `RIPTIDE_FORCE_FORK` | off | **Debug / benchmark only.** Reverts to `fork()`-per-test isolation, bypassing the no-fork ladder. Use it to A/B the ladder or chase an isolation bug — not in normal use. |
| `RIPTIDE_SOCKET` | `<tmp>/riptide-daemon.sock` | `serve` mode: the Unix socket path the RPC server binds. |

```bash
# A typical setup
export RIPTIDE_SHIM="$PWD/engine/py-shim/shim.py"
export RIPTIDE_PYTHON="$PWD/.venv/bin/python"
```

## The isolation default

No-fork + restore is **on by default** — there is no flag to enable it. The daemon requests no-fork
on every test and the shim runs it in-process, undoing any mutation from a pre-body snapshot; a
module it can't snapshot (opaque globals) automatically falls back to `fork()` for soundness. So a
wrong guess can only change speed, never correctness.

`RIPTIDE_FORCE_FORK=1` is the escape hatch back to fork-per-test, kept purely as a debug and
benchmark baseline. See the [isolation ladder](../design/architecture.md#the-isolation-ladder).

## The one flag: `--all`

```bash
riptide-daemon run  <tests>          # impact-aware: only changed tests; coverage on; state persisted
riptide-daemon run  <tests> --all    # full parallel run; opts out of impact-skip and coverage
```

- Plain `run` is **impact-aware**: it reads `.riptide-state.json`, runs only tests whose deps
  changed, and re-persists the state. With no changes, nothing runs.
- `run --all` forces a full run across the parallel pool — your CI safe mode, or a clean baseline.

The one-shot `riptide` binary has no impact analysis: `riptide collect <path>` lists tests,
`riptide run <path>` runs them all once.

## State file & `.gitignore`

Impact analysis persists to **`.riptide-state.json`** in the directory you run from — each test's
dependency files plus per-file content hashes. It is machine-local; every developer and CI runner
keeps their own. Ignore it:

```gitignore
# tiderace impact-analysis state — machine-local, do not commit
.riptide-state.json
```

## Future: pyproject configuration

!!! note "Not yet"
    There is currently **no** `pyproject.toml` / config-file support — configuration is entirely
    through the environment variables above. A native `[tool.tiderace]` section may arrive later;
    it does not exist today. (If you've used the retired pytest-orchestrating engine, its
    `[tool.tiderace]` keys and `TIDERACE_*` variables are gone — they don't apply here.)
