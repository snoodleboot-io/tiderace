# Development Setup

## Prerequisites

- Rust 1.75+ (`rustup install stable`)
- Python 3.8+
- `pytest` and `coverage` (`pip install pytest coverage`)

## Clone and Build

```bash
git clone https://github.com/snoodleboot-io/riptide
cd riptide
cargo build
```

The debug binary is at `target/debug/riptide`.

## Run Tests

```bash
# Rust unit tests
cargo test

# Integration test against the demo project
cd demo && ../target/debug/riptide tests/ --all
```

## Code Style

```bash
cargo fmt        # format
cargo clippy     # lint
```

CI enforces both — PRs that fail `clippy` or `fmt` are blocked.

## Project Layout

```
riptide/
├── riptide/             # Rust source
│   ├── main.rs          # CLI + orchestration
│   ├── collector.rs     # test discovery
│   ├── hasher.rs        # file fingerprinting
│   ├── db.rs            # SQLite layer
│   ├── impact.rs        # affected test selection
│   ├── runner.rs        # parallel execution
│   └── reporter.rs      # terminal output
├── docs/                # MkDocs source — user docs + whole-system design
├── planning/            # development planning (per-feature PRD/ADR/design)
│   ├── current/         # features in active development
│   ├── backlog/         # planned feature folders
│   └── completed/       # shipped feature folders
├── .github/workflows/   # CI/CD
├── demo/                # example Python project for integration tests
├── Cargo.toml
└── mkdocs.yml
```

## Branching Model

riptide uses **trunk-based development**:

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
