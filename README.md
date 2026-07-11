<div align="center">

<img src="docs/assets/logo.svg" width="64" height="64" alt="tiderace logo">

# tiderace ⚡

**A pure-Rust test engine for Python**  
Its own runner (no pytest at runtime) · No-fork isolation · Impact analysis · Coverage · Warm daemon

[![CI](https://github.com/snoodleboot-io/tiderace/actions/workflows/ci.yml/badge.svg)](https://github.com/snoodleboot-io/tiderace/actions/workflows/ci.yml)
[![Release](https://github.com/snoodleboot-io/tiderace/actions/workflows/release.yml/badge.svg)](https://github.com/snoodleboot-io/tiderace/actions/workflows/release.yml)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache_2.0-C75B39.svg)](LICENSE)

</div>

---

## What is tiderace?

tiderace is a **compiled Rust engine that runs Python tests directly**. The Rust side owns collection,
the fixture graph, scheduling, isolation, coverage, and impact analysis; a small Python *shim* is the
only code inside CPython, and it exists only to import your tests and call their bodies. **There is no
pytest at runtime.**

That design unlocks two things no pytest plugin can do together:

- **Isolation without the fork tax.** Tests are isolated so one can't corrupt another, but tiderace pays
  for it *only where a test needs it* — pure tests run in-process (no fork), state-mutating tests run
  in-process with snapshot/restore, and only opaque cases are forked. The per-test `fork()` that
  dominated execution is gone for most tests.
- **Only run what changed.** Per-test source footprints (via CPython's `sys.monitoring`) plus content
  hashing mean an unchanged test never runs — and a result is content-addressed, so the same machinery
  works as a build-system-style cache.

> **Naming.** The project is **tiderace**. The engine binaries currently build as `riptide` /
> `riptide-daemon` (a retired codename being consolidated under tiderace) — read them as tiderace. An
> earlier generation that *orchestrated pytest* (a separate `tiderace` binary) has been removed.

## How it compares

| | tiderace | pytest | pytest-xdist | pytest-forked |
|---|:---:|:---:|:---:|:---:|
| Runs Python directly (no pytest) | ✅ own engine | — | — | — |
| Per-test isolation | ✅ only where needed | ❌ none | ❌ none | ✅ forks everything |
| Knows which tests need isolation | ✅ | ❌ | ❌ | ❌ |
| Impact analysis (run only what changed) | ✅ | ❌ | ❌ | ❌ |
| Coverage | ✅ `sys.monitoring` | via plugin | via plugin | via plugin |
| Written in | 🦀 Rust | 🐍 Python | 🐍 Python | 🐍 Python |

## Benchmarks

Measured on `benchmarks/fixtures/fx_corpus` (509 fixture tests; numpy/sqlite), hyperfine. Reproduce with
`benchmarks/bench_3way.sh`.

| scenario | pytest | **tiderace** | speedup |
|---|---:|---:|---:|
| **Cold** — full run (all 509 execute) | 0.94 s | **0.66 s** | **1.4× faster** |
| **Warm** — no changes (impact-skip) | 0.84 s | **9.4 ms** | **89×** |
| **Warm** — inner loop, 1 changed test | 0.27 s | **~5 ms** | **~50–70×** |

The no-fork isolation ladder makes even a *cold* full run beat pytest; impact-skip is where it dominates.

## Install

> **Pre-release:** build from source from the `engine/` Cargo workspace; prebuilt binaries are not
> published yet.

```bash
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace/engine && cargo build --release
# binaries: target/release/riptide  and  target/release/riptide-daemon
```

## Quick start

```bash
# The engine needs to know your shim + interpreter (it inherits these via env)
export RIPTIDE_SHIM="$PWD/py-shim/shim.py"
export RIPTIDE_PYTHON="$(which python3)"

# First run — executes all tests, records coverage footprints + state
./target/release/riptide-daemon run /path/to/tests

# Subsequent runs — only tests affected by changed files (no change → nothing runs)
./target/release/riptide-daemon run /path/to/tests

# Forced full run
./target/release/riptide-daemon run /path/to/tests --all

# Watch — warm interpreter, re-run impacted tests on save (millisecond loops)
./target/release/riptide-daemon watch /path/to/tests
```

## How it works

1. **Collect** — discover tests with Rust regex (no Python startup).
2. **Graph** — build each test's fixture closure (Rust).
3. **Schedule** — group by module (scope locality) and load-balance across N warm interpreters.
4. **Impact** — skip tests whose dependency files (from coverage) didn't change; with no changes,
   nothing runs — the interpreter isn't even launched.
5. **Isolate** — per test: pure → no-fork · state-mutating → no-fork + snapshot/restore · opaque → fork
   (sound by construction; see [ADR-E014](planning/current/pure-rust-test-engine/design/adr/ADR-E014-no-fork-restore-ladder.md)).
6. **Run** — invoke the body in the warm interpreter via the shim; capture coverage + purity.
7. **Persist** — outcomes, per-test footprints, and file hashes to `.riptide-state.json`.

See **[ARCHITECTURE.md](ARCHITECTURE.md)** for the full design with diagrams.

## Add to .gitignore

```gitignore
.riptide-state.json
```

## Documentation

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — full system architecture, diagrams, code map
- [Quick Start](https://snoodleboot-io.github.io/tiderace/guides/quickstart/)
- [Architecture](https://snoodleboot-io.github.io/tiderace/design/architecture/)
- [Impact Analysis](https://snoodleboot-io.github.io/tiderace/design/impact-analysis/)
- [CLI Reference](https://snoodleboot-io.github.io/tiderace/api/cli/)
- [Design decisions (ADRs)](planning/current/pure-rust-test-engine/design/adr/)

## License

Apache 2.0 — see [LICENSE](LICENSE)
