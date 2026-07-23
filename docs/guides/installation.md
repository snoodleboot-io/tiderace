# Installation

The pure-Rust engine builds from source from the `engine/` Cargo workspace. Prebuilt binaries are
**not published yet**.

!!! info "Naming"
    The binaries currently build as `riptide` / `riptide-daemon` — a retired codename being
    consolidated under tiderace. Read them as tiderace.

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
| `riptide` | `engine-cli` | one-shot `collect` / `run` |
| `riptide-daemon` | `engine-daemon` | warm server: impact-aware `run`, `run --all`, `serve` (RPC, Unix), `watch`, `bench`, `probe` |

Optionally copy them onto your `PATH`:

```bash
install -m 0755 target/release/riptide        /usr/local/bin/riptide
install -m 0755 target/release/riptide-daemon /usr/local/bin/riptide-daemon
```

## Point the engine at Python

The engine is env-driven. Set the shim path (required) and, optionally, the interpreter:

```bash
# Required — the shim the engine runs inside CPython.
export RIPTIDE_SHIM="$PWD/py-shim/shim.py"

# Optional — defaults to python3. Use your project's venv if tests have dependencies.
export RIPTIDE_PYTHON="$(which python3)"
```

See [Configuration](configuration.md) for the rest of the variables.

## Verify

```bash
# List the tests the engine discovers (no execution)
./target/release/riptide collect /path/to/tests

# Full run through the warm daemon
./target/release/riptide-daemon run /path/to/tests --all
```

## Prebuilt binaries (future)

!!! note "Pre-release"
    crates.io publishing and GitHub Releases binaries are not available yet. Build from source as
    above. Release artifacts will be added once the rename from the `riptide` codename to tiderace
    is consolidated.

## Add to `.gitignore`

```gitignore
# tiderace impact-analysis state — machine-local, do not commit
.riptide-state.json
```
