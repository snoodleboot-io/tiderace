# Development Setup

## Prerequisites

- Rust 1.75+ (`rustup install stable`)
- Python 3.8+
- `pytest` and `coverage` (`pip install pytest coverage`)

## Clone and Build

```bash
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace
cargo build
```

The debug binary is at `target/debug/tiderace`.

## Run Tests

```bash
# Unit tests (pure Rust, no Python needed)
cargo test --bins

# Full suite incl. integration tests that run the real binary against temporary
# Python projects. Point them at a Python that has pytest installed:
TIDERACE_TEST_PYTHON=/path/to/python cargo test --all
```

Integration tests (`tests/cli.rs`) scaffold throwaway projects and exercise the genuine
Rust → pytest → SQLite path. They **skip** (not fail) when no Python with pytest is found, so
`cargo test` stays green on a machine without it.

## Code Style

```bash
cargo fmt        # format
cargo clippy --all-targets -- -D warnings   # lint (warnings are errors in CI)
```

CI enforces both — PRs that fail `clippy` or `fmt` are blocked.

## Coverage

```bash
cargo install cargo-llvm-cov   # once
TIDERACE_TEST_PYTHON=/path/to/python \
  cargo llvm-cov --all --ignore-filename-regex 'watcher\.rs' --fail-under-lines 80
```

`watcher.rs` is excluded because its blocking `notify` loop only runs inside a long-lived
`tiderace watch` process that the test must kill — a killed process never flushes its coverage
profile, so it's validated by an integration test instead of being counted.

## Mutation testing

Beyond line coverage, [`cargo-mutants`](https://mutants.rs) checks that the tests actually
*catch* changes by mutating the source and confirming a test fails:

```bash
cargo install cargo-mutants   # once
TIDERACE_TEST_PYTHON=/path/to/python cargo mutants
```

Mutation runs are slow (they rebuild and re-test per mutant), so run them on the pure-logic
modules while iterating rather than the whole crate:

```bash
cargo mutants --file tiderace/collector.rs --file tiderace/impact.rs --file tiderace/runner.rs
```

A surviving mutant means a behaviour no test pins down — add a test for it.

## Project Layout

```
tiderace/
├── tiderace/             # Rust source
│   ├── main.rs          # CLI + orchestration (run/collect/clear/coverage/watch)
│   ├── config.rs        # pyproject.toml [tool.tiderace]
│   ├── collector.rs     # test discovery (functions, classes, unittest, async)
│   ├── hasher.rs        # file fingerprinting
│   ├── db.rs            # SQLite layer
│   ├── impact.rs        # affected test selection
│   ├── runner.rs        # batched/isolated execution + coverage contexts
│   ├── pool.rs          # persistent warm worker pool (watch)
│   ├── watcher.rs       # debounced file watching (notify)
│   ├── worker.py        # embedded Python worker for the warm pool
│   └── reporter.rs      # terminal output
├── tests/cli.rs         # end-to-end integration tests
├── benchmarks/          # fixture generator + comparison harness
├── docs/                # MkDocs source — user docs + whole-system design
├── planning/            # development planning (per-feature PRD/ADR/design)
├── .github/workflows/   # CI · release · docs
├── Cargo.toml
└── mkdocs.yml
```

## Branching Model

tiderace uses **trunk-based development**:

- All work lands on `main` via short-lived branches (< 2 days)
- No long-lived feature branches
- Feature flags in code for in-progress work
- `main` is always releasable

## Commit Convention

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add pyproject.toml config support
fix: handle empty test directories gracefully
docs: update impact analysis design doc
chore: bump rusqlite to 0.31.1
```

The CI uses these to compute semantic version bumps automatically.

| Prefix | Version bump |
|---|---|
| `feat:` | minor (0.x.0) |
| `fix:`, `perf:`, `docs:` | patch (0.0.x) |
| `feat!:` or `BREAKING CHANGE:` | **major — CI only** |
