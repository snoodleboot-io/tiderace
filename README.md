<div align="center">

<img src="docs/assets/logo.svg" width="64" height="64" alt="riptide logo">

# riptide ⚡

**Rust-powered Python test engine**  
Parallel execution · Impact analysis · Coverage · Zero config

[![CI](https://github.com/snoodleboot-io/riptide/actions/workflows/ci.yml/badge.svg)](https://github.com/snoodleboot-io/riptide/actions/workflows/ci.yml)
[![Release](https://github.com/snoodleboot-io/riptide/actions/workflows/release.yml/badge.svg)](https://github.com/snoodleboot-io/riptide/actions/workflows/release.yml)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache_2.0-C75B39.svg)](LICENSE)

</div>

---

## What is riptide?

riptide is a compiled Rust binary that orchestrates your Python test suite. It runs tests in parallel and — once you've built a coverage dependency graph — **only re-runs tests affected by files you actually changed.**

The example below assumes you have already run once with `--coverage`, so riptide knows which tests depend on `src/auth.py`:

```
$ riptide tests/

  ✓ collected 200 tests
  ⚡ 1 file changed: src/auth.py

  tests: 8   skipped (unchanged): 192   workers: 8   coverage: on

  ✓ [1/8] tests/test_auth.py::test_login              312ms
  ✓ [2/8] tests/test_auth.py::test_logout             289ms
  ✓ [3/8] tests/test_auth.py::test_session_expire     301ms
  ...

  ✓ passed: 8
  ⚡ skipped (unchanged): 192 (impact analysis)
  time: 0.71s
```

## Features

| | riptide | pytest | pytest-xdist | pytest-testmon |
|---|:---:|:---:|:---:|:---:|
| Parallel execution | ✅ Rust/Rayon | ❌ | ✅ Python | ❌ |
| Impact analysis | ✅ | ❌ | ❌ | ✅ Python |
| Coverage | ✅ | via plugin | via plugin | via plugin |
| Written in | 🦀 Rust | 🐍 Python | 🐍 Python | 🐍 Python |
| Subprocess overhead | ~250ms/test | shared | shared | shared |
| State persistence | SQLite | none | none | `.testmondata` |

## Install

> **Pre-release:** riptide is not yet published to crates.io or GitHub Releases. Build from source with `cargo build --release` (binary lands at `target/release/riptide`). The download URLs below are placeholders for a future release.

```bash
# Build from source (the working path today)
cargo build --release
# binary at target/release/riptide

# Future / illustrative — Linux x86_64 prebuilt binary
curl -sSfL https://github.com/snoodleboot-io/riptide/releases/latest/download/riptide-linux-x86_64 \
  -o /usr/local/bin/riptide && chmod +x /usr/local/bin/riptide

# Future / illustrative — once published to crates.io
cargo install riptide
```

## Quick Start

```bash
# First run — run with --coverage to build the dependency graph.
# This is what unlocks precise source-level impact analysis.
riptide tests/ --all --coverage

# All subsequent runs — only tests affected by changed files
riptide tests/

# CI
riptide tests/ -n 8 --coverage --python .venv/bin/python

# Watch mode — warm worker pool, sub-second re-runs of impacted tests on save
riptide watch tests/
```

Without a coverage graph, riptide stays conservative: any source-file change re-runs every test that lacks recorded dependencies, since it cannot map the edit to specific tests. Run once with `--coverage` to teach it which tests depend on which source files.

## How It Works

1. **Collect** — Scan `test_*.py` files with Rust regex (no Python startup)
2. **Hash** — SHA-256 fingerprint every `.py` file in the tree
3. **Diff** — Compare against hashes stored in `.riptide.db`
4. **Impact** — A test re-runs if its own test file changed, if it never ran before, or if it previously failed/errored. With a stored coverage dep graph, a source-file change re-runs only the tests whose recorded dependencies changed; without one, riptide conservatively re-runs all tests lacking a dep graph. With no changes at all, a warm run skips everything.
5. **Run** — Rayon parallel pool. By default tests are **batched** — one `pytest` process per worker — so interpreter startup is paid per worker, not per test (≈8× faster cold start than one process per test). `--coverage` is also batched and records per-test dependencies via coverage dynamic contexts ([ADR-011](docs/design/decisions.md)); `--isolate` forces one process per test (see [ADR-009](docs/design/decisions.md))
6. **Persist** — Store new hashes, results, and coverage dep graph

## Benchmarks

`benchmarks/run_benchmarks.py` compares riptide (cold and warm runs) against `pytest`, `pytest-xdist`, `pytest-testmon`, and `unittest` on a generated fixture suite, writing results to `benchmarks/RESULTS.md`.

```bash
python benchmarks/run_benchmarks.py
```

Honest framing: riptide's strongest advantage is **warm / impact** runs that skip unchanged tests. For the **cold** full run, batched execution (one pytest process per worker, the default) is ~8× faster than the legacy one-process-per-test path, but still pays one interpreter startup per worker — so on many trivial tests it can trail single-process `pytest`. Persistent warm workers (`riptide watch`, ADR-009 stage B) close most of that gap for the edit loop; embedded subinterpreters (stage C) were evaluated and **rejected** for breaking C-extension compatibility (see [ADR-010](docs/design/decisions.md)). Numbers vary by machine, so run the harness yourself rather than trusting a fixed figure.

## Add to .gitignore

```gitignore
.riptide.db
.riptide-coverage/
```

## Documentation

Full documentation at **[snoodleboot-io.github.io/riptide](https://snoodleboot-io.github.io/riptide)**:

- [Quick Start](https://snoodleboot-io.github.io/riptide/guides/quickstart/)
- [Architecture](https://snoodleboot-io.github.io/riptide/design/architecture/)
- [Impact Analysis Deep Dive](https://snoodleboot-io.github.io/riptide/design/impact-analysis/)
- [CLI Reference](https://snoodleboot-io.github.io/riptide/api/cli/)

## License

Apache 2.0 — see [LICENSE](LICENSE)
