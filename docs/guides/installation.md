# Installation

The pure-Rust engine builds from source from the `engine/` Cargo workspace. Prebuilt binaries are
**not published yet**.

## Prerequisites

- **Rust toolchain** (stable) — install via [rustup](https://rustup.rs).
- **Python 3.12+** — the engine captures coverage with CPython's `sys.monitoring` (PEP 669), which
  requires 3.12 or newer.
- **CPython 3.14+** — *only* if you want the [sub-interpreter tier](configuration.md#windows-parallelism-the-sub-interpreter-tier-opt-in)
  (`concurrent.interpreters`, for Windows parallelism). Everything else runs on 3.12+.

That's it. There is no pytest or coverage.py to install — tiderace is its own runner, and the only
Python it runs is a small shim shipped in the repo.

## Platforms

| Platform | Status | Notes |
|---|---|---|
| **Linux / macOS** | ✅ full | Fork-from-warm-wellspring isolation; the parallel pool forks per test. |
| **Windows** | ✅ supported | No `fork()`, so isolation is no-fork + snapshot/restore. `run`, `run --all`, and `watch` all work; the opt-in **sub-interpreter tier** (CPython 3.14+) adds parallel no-fork execution. The only Unix-only mode is the `serve` RPC socket — use `run` / `watch` on Windows. |

## Build from source

```bash
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace/engine            # build from the engine/ workspace, not the repo root
cargo build --release
```

The two binaries land in `engine/target/release/`:

| Binary | Crate | What it does |
|---|---|---|
| `tiderace` | `engine-cli` | one-shot `collect` / `run` |
| `tiderace-daemon` | `engine-daemon` | warm server: impact-aware `run`, `run --all`, `serve` (RPC, Unix), `watch`, `bench`, `probe` |

Optionally copy them onto your `PATH`:

```bash
install -m 0755 target/release/tiderace        /usr/local/bin/tiderace
install -m 0755 target/release/tiderace-daemon /usr/local/bin/tiderace-daemon
```

## Point the engine at Python

The engine is env-driven. Set the shim path (required) and, optionally, the interpreter:

```bash
# Required — the shim the engine runs inside CPython.
export TIDERACE_SHIM="$PWD/py-shim/shim.py"

# Optional — defaults to python3. Use your project's venv if tests have dependencies.
export TIDERACE_PYTHON="$(which python3)"
```

See [Configuration](configuration.md) for the rest of the variables.

## Verify

```bash
# List the tests the engine discovers (no execution)
./target/release/tiderace collect /path/to/tests

# Full run through the warm daemon
./target/release/tiderace-daemon run /path/to/tests --all
```

## Install as a Python wheel (maturin)

tiderace also builds a **Python wheel** that ships both binaries *and* the bundled shim, so an install
runs your suite with **no configuration** — the binaries locate the shim inside the installed package
automatically (no `TIDERACE_SHIM` needed):

```bash
scripts/build-wheel.sh --release -o dist        # stages the shim, then `maturin build`
uv pip install dist/*.whl                        # or: pip install dist/*.whl
tiderace-daemon run /path/to/tests --all         # just works, zero env vars
```

The wheel is built by [`scripts/build-wheel.sh`](https://github.com/snoodleboot-io/tiderace/blob/main/scripts/build-wheel.sh)
— the same script CI runs, so what ships is exactly what you can build here. It bundles the
`tiderace` authoring package (`@tiderace.provides`, `tiderace migrate`) alongside the binaries.

!!! note "PyPI"
    `pip install tiderace` from PyPI isn't live yet — publishing happens on the first tagged `v*`
    release (the `Wheels` workflow uploads via maturin once a `PYPI_API_TOKEN` is configured). Until
    then, build the wheel locally as above.

## Add to `.gitignore`

```gitignore
# tiderace impact-analysis state — machine-local, do not commit
.tiderace-state.json
```
