# Development Setup

tiderace's engine lives in the `engine/` Cargo workspace. This is where you build, test, and lint.

## Prerequisites

- **Rust toolchain** (stable) — `rustup install stable`.
- **Python 3.12+** — required for the `sys.monitoring` coverage path.

No pytest or coverage.py needed: tiderace is its own runner.

## Clone and build

```bash
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace/engine
cargo build
```

Debug binaries land in `engine/target/release/` (or `engine/target/debug/` for `cargo build`):
`tiderace` (the CLI) and `tiderace-daemon` (the warm server).

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

## Live tests and the fx venv

The acceptance suites that assert the engine's load-bearing invariants — no-fork ≡ fork,
sub-interpreter ≡ fork, purity/safety detection, the daemon end-to-end — need a real interpreter, and
resolve `.tiderace-fx-venv` at the repo root **by path**. Without it they self-skip.

A skip is not visibly different from a pass. libtest has no "skipped" state, so an early `return`
reports as `ok`, and the harness swallows the skip marker unless you pass `--nocapture`. A green
`cargo test` can therefore mean the isolation ladder is sound *or* that none of it ran. (This is not
theoretical: the venv symlinked into a versioned VSCode snap path, that revision was
garbage-collected, and the workspace stayed green with 10 live tests skipping.)

Provision it once, at the repo root:

```bash
python -m venv .tiderace-fx-venv && .tiderace-fx-venv/bin/pip install numpy pytest
```

!!! warning "Point at a stable interpreter"
    `python` here must be a path that survives upgrades. A `uv`/snap interpreter under a
    *revision-numbered* directory will break silently when that revision is collected.

Then, to prove the live paths actually executed:

```bash
cd engine
TIDERACE_REQUIRE_LIVE=1 cargo test --workspace   # a skip becomes a failure
```

`TIDERACE_REQUIRE_LIVE=1` turns every live-scenario skip into a panic (`engine_core::testing`). Both
CI jobs that provision the venv set it, so a broken environment fails the build instead of passing as
a no-op. Leave it unset if you're working without a venv — the suites will skip as before.

## Coverage gate

CI gates line coverage of the engine workspace at **≥ 88%** (`cargo llvm-cov`). Reproducing it needs
the fx venv above — without it the exec paths look uncovered and the gate only measures pure logic:

```bash
cd engine
TIDERACE_REQUIRE_LIVE=1 cargo llvm-cov --workspace --ignore-filename-regex '(main|socket)\.rs' --fail-under-lines 88
```

`main.rs` (CLI entry) and `socket.rs` (the socket serve loop) are excluded — binary glue with no logic
that a killed process can't flush coverage for.

## The Python shim's own tests

The shim (`engine/py-shim/tiderace_shim/`, entered through `engine/py-shim/shim.py`) has unit tests
beside it — the result frames, the node resolver, the selection grammar, discovery over a scratch
suite, the invoke path, the isolation object, the package layout. They run with the fx venv:

```bash
.tiderace-fx-venv/bin/python -m pytest engine/py-shim/tests -q
```

Everything that needs a live interpreter is an acceptance suite under `engine/crates/*/tests/`
(above). The proof scripts that once demonstrated the isolation tiers, purity, coverage and type-DI
are archived under `planning/proofs/` with a note of which suite covers each now.

## Repository layout

```
tiderace/
├── engine/                 # the pure-Rust engine (build from here)
│   ├── Cargo.toml          # workspace manifest
│   ├── crates/
│   │   ├── engine-core/    # collection · fixtures · scheduler · exec · coverage · impact · cache
│   │   ├── engine-cli/     # → tiderace (collect, run)
│   │   ├── engine-daemon/  # → tiderace-daemon (run, serve, watch, bench)
│   │   └── engine-inproc/  # → inproc-probe (experimental embedded-CPython / FFI backend)
│   ├── py-shim/            # shim.py + tiderace_shim/ — the execution substrate (import, invoke, isolate, coverage)
│   └── py-tiderace/         # native authoring pkg (tiderace/) + migrate
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
