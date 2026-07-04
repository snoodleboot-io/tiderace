# Development Setup

tiderace's engine lives in the `engine/` Cargo workspace. This is where you build, test, and lint.

!!! info "Naming"
    The binaries currently build as `riptide` / `riptide-daemon` — a retired codename being
    consolidated under tiderace. Read them as tiderace.

## Prerequisites

- **Rust toolchain** (stable) — `rustup install stable`.
- **Python 3.12+** — required for the `sys.monitoring` coverage path and the shim proofs.

No pytest or coverage.py needed: tiderace is its own runner.

## Clone and build

```bash
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace/engine
cargo build
```

Debug binaries land in `engine/target/release/` (or `engine/target/debug/` for `cargo build`):
`riptide` (the CLI) and `riptide-daemon` (the warm server).

## Run the tests

The engine's logic is unit- and integration-tested in pure Rust (the `ShimTransport` seam lets the
execution path run with no Python at all via a scripted test double):

```bash
cd engine

# Core engine: collection, fixtures, scheduler, exec, coverage, impact, cache
cargo test -p engine-core

# Daemon: impact-aware run, persistence, watch, RPC server
cargo test -p engine-daemon

# Everything
cargo test
```

## Lint & format

```bash
cargo clippy --all-targets -- -D warnings   # lint (warnings are errors in CI)
cargo fmt                                    # format
```

CI enforces both — PRs that fail `clippy` or `fmt` are blocked.

## Coverage gate

CI gates line coverage of the engine workspace at **≥ 88%** (`cargo llvm-cov`). To reproduce locally
you need `.riptide-fx-venv` at the repo root (numpy + pytest) so the fork/daemon live tests run rather
than self-skip — otherwise the exec paths look uncovered:

```bash
python -m venv .riptide-fx-venv && .riptide-fx-venv/bin/pip install numpy pytest   # once, at repo root
cd engine
cargo llvm-cov --workspace --ignore-filename-regex '(main|socket)\.rs' --fail-under-lines 88
```

`main.rs` (CLI entry) and `socket.rs` (the socket serve loop) are excluded — binary glue with no logic
that a killed process can't flush coverage for.

## The Python shim proofs

The shim and the native authoring package carry standalone **proof scripts** that demonstrate
specific behaviours (isolation tiers, purity, coverage, type-DI). They run directly with `python3`
(3.12+) — no Rust, no test framework:

```bash
cd engine/py-riptide

python3 proof_static_purity.py      # static AST impurity pre-filter
python3 proof_snapshot_restore.py   # no-fork + restore isolation
python3 proof_purity_guard.py       # purity verdict recording
python3 proof_n6_coverage.py        # sys.monitoring coverage capture
python3 proof_type_di.py            # native @provides / @uses type resolution
# …other proof_*.py in the same directory
```

The shim itself is `engine/py-shim/shim.py` — the only code that runs inside CPython.

## Repository layout

```
tiderace/
├── engine/                 # the pure-Rust engine (build from here)
│   ├── Cargo.toml          # workspace manifest
│   ├── crates/
│   │   ├── engine-core/    # collection · fixtures · scheduler · exec · coverage · impact · cache
│   │   ├── engine-cli/     # → riptide (collect, run)
│   │   ├── engine-daemon/  # → riptide-daemon (run, serve, watch, bench)
│   │   └── engine-inproc/  # → inproc-probe (experimental embedded-CPython / FFI backend)
│   ├── py-shim/            # shim.py — the execution substrate (import, invoke, isolate, coverage)
│   └── py-riptide/         # native authoring pkg (riptide/) + proof_*.py + migrate
├── benchmarks/             # bench_3way.sh, real_world.sh, RESULTS-*.md, fixtures/
├── docs/                   # MkDocs source — user guides + whole-system design
├── planning/               # per-feature planning (PRD / ADR / design)
└── ARCHITECTURE.md         # the authoritative architecture reference
```

## Branching model

tiderace uses **trunk-based development**:

- All work lands on `main` via short-lived branches.
- No long-lived feature branches; `main` is always releasable.

## Commit convention

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add no-fork restore tier to the isolation ladder
fix: handle empty test directories gracefully
docs: update impact-analysis design doc
chore: bump pyo3 to 0.26
```

CI uses these to compute semantic version bumps automatically.

| Prefix | Version bump |
|---|---|
| `feat:` | minor (0.x.0) |
| `fix:`, `perf:`, `docs:` | patch (0.0.x) |
| `feat!:` or `BREAKING CHANGE:` | **major — CI only** |
