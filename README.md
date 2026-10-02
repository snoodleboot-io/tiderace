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
scheduling, isolation, coverage, and impact analysis; a Python *shim* is the only code inside CPython —
it imports your tests, resolves their fixtures and calls their bodies. **There is no
pytest at runtime.**

That design unlocks two things no pytest plugin can do together:

- **Isolation without the fork tax.** Tests are isolated so one can't corrupt another, but tiderace pays
  for it *only where a test needs it* — pure tests run in-process (no fork), state-mutating tests run
  in-process with snapshot/restore, and only opaque cases are forked. The per-test `fork()` that
  dominated execution is gone for most tests.
- **Only run what changed.** Per-test source footprints (via CPython's `sys.monitoring`) plus content
  hashing mean an unchanged test never runs — and a result is content-addressed, so the same machinery
  works as a build-system-style cache.

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

Eight real suites — click, flask, cachetools, anyio, and four packages of an internal monorepo —
run by pytest and by tiderace in the same virtualenv, compared **test by test** before anything is
timed. Medians of interleaved rounds; the full tables, method and caveats are in the
[benchmarks guide](https://snoodleboot-io.github.io/tiderace/guides/benchmarks/).

| suite | tests | pytest | pytest -n auto | **tiderace** | vs pytest | vs xdist |
|---|---:|---:|---:|---:|---:|---:|
| pirn-core (monorepo) | 5,036 | 78.0 s | 39.9 s | **27.2 s** | 2.9× | 1.5× |
| pirn-agents (monorepo) | 4,652 | 104.7 s | 47.9 s | **32.4 s** | 3.2× | 1.5× |
| anyio | 1,479 | 48.4 s | 15.0 s | **9.6 s** | 5.1× | 1.6× |
| click | 589 | 1.38 s | 1.92 s | **0.69 s** | 2.0× | 2.8× |
| flask | 482 | 2.08 s | 2.40 s | **0.99 s** | 2.1× | 2.4× |

Outcomes are identical to pytest's on seven of the eight suites, node id for node id. And the run
a developer actually waits on, on the 5,600-test suite: **nothing edited, 0.16 s** (pytest has no
warm mode: 80 s); **one leaf module edited, 1.4 s**; **one test by name through a warm daemon,
0.3 s**.

## Install

```bash
pip install tiderace        # or: uv pip install tiderace
```

That ships the `tiderace` and `tiderace-daemon` binaries plus the authoring package — no Rust
toolchain and no configuration. Requires Python 3.12+.

**Prebuilt wheels:** Linux x86_64 and aarch64 (glibc 2.28+, so RHEL 8, Debian 10+, Ubuntu 18.10+),
macOS universal2 (11.0+, Apple Silicon and Intel), and Windows x86_64. Anything else falls back to
the sdist, which does need a Rust toolchain.

### From source

```bash
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace/engine && cargo build --release
# binaries: target/release/tiderace  and  target/release/tiderace-daemon
```

## Quick start

```bash
pip install tiderace                  # into the interpreter your tests run under

tiderace run tests/                   # the whole suite, pytest's outcomes and exit code
tiderace run -k test_login tests/     # one test by name

tiderace daemon start tests/          # keep the imported suite warm for the session…
tiderace run -k test_login tests/     # …0.3s on a 5,600-test suite instead of 5s
tiderace daemon stop tests/

tiderace-daemon run tests/            # first pass records footprints; later passes run only
tiderace-daemon run tests/            #   what an edit touched — nothing changed, nothing runs
tiderace-daemon run tests/ --all      # the CI gate: everything, every time
tiderace-daemon watch tests/          # the editor loop: re-run what each save impacts
```

Built from source, the binaries are under `engine/target/release/` and need `TIDERACE_SHIM`
pointed at `engine/py-shim/shim.py`; the wheel bundles the shim. `TIDERACE_PYTHON` picks the
interpreter when tiderace is not installed into it.

## How it works

1. **Collect** — discover tests with Rust regex (no Python startup).
2. **Graph** — build each test's fixture closure (Rust).
3. **Schedule** — group by module (scope locality) and hand the groups to N warm interpreters from a
   queue. A file's tests run in one process in file order, as under pytest; the order *between*
   files is not pytest's and is not promised to be.
4. **Impact** — skip tests whose dependency files (from coverage) didn't change; with no changes,
   nothing runs — the interpreter isn't even launched.
5. **Isolate** — per test: pure → no-fork · state-mutating → no-fork + snapshot/restore · opaque → fork
   (sound by construction; see [ADR-E014](planning/current/pure-rust-test-engine/design/adr/ADR-E014-no-fork-restore-ladder.md)).
6. **Run** — invoke the body in the warm interpreter via the shim; capture coverage + purity.
7. **Persist** — outcomes, per-test footprints, and file hashes to `.tiderace-state.json`.

See **[ARCHITECTURE.md](ARCHITECTURE.md)** for the full design with diagrams.

## Add to .gitignore

```gitignore
.tiderace-state.json
.tiderace-cache/
```

## Documentation

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — full system architecture, diagrams, code map
- [Quick Start](https://snoodleboot-io.github.io/tiderace/guides/quickstart/)
- [Migrating from pytest](https://snoodleboot-io.github.io/tiderace/guides/migration/)
- [Benchmarks](https://snoodleboot-io.github.io/tiderace/guides/benchmarks/)
- [Execution model](https://snoodleboot-io.github.io/tiderace/design/parallel-execution/)
- [Impact Analysis](https://snoodleboot-io.github.io/tiderace/design/impact-analysis/)
- [CLI Reference](https://snoodleboot-io.github.io/tiderace/api/cli/)
- [Design decisions (ADRs)](planning/current/pure-rust-test-engine/design/adr/)

## License

Apache 2.0 — see [LICENSE](LICENSE)
