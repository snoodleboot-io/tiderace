---
hide:
  - navigation
  - toc
---

<div class="tiderace-hero">
  <h1>tide<span>race</span> ⚡</h1>
  <p class="tagline">A pure-Rust test engine for Python — its own runner, not a pytest wrapper. Isolation without the fork tax, and only runs what changed.</p>
  <div class="badges">
    <span class="badge">Rust</span>
    <span class="badge">no pytest at runtime</span>
    <span class="badge">no-fork isolation</span>
    <span class="badge">impact analysis</span>
    <span class="badge">coverage</span>
    <span class="badge">warm daemon</span>
  </div>
  <div class="cta">
    <a href="guides/quickstart/" class="btn-primary">Get Started →</a>
    <a href="https://github.com/snoodleboot-io/tiderace" class="btn-secondary">View on GitHub</a>
  </div>
</div>

## Why tiderace?

Python test suites are slow, and the usual fixes each solve only part of it. `pytest` runs every test
every time with **no isolation** between them. `pytest-forked` isolates by forking *everything* (safe but
slow). `pytest-xdist` parallelises but doesn't know what changed. `pytest-testmon` knows what changed but
is pure Python.

tiderace is a **compiled Rust engine that runs Python tests directly** — it owns collection, the fixture
graph, scheduling, isolation, coverage, and impact analysis. There is **no pytest at runtime**. A tiny
Python *shim* is the only code inside CPython, and it exists only to import your tests and call their
bodies. That lets tiderace do something no pytest plugin can: **pay for isolation only where a test
actually needs it.**

<div class="feature-grid">
  <div class="feature-card">
    <div class="icon">⚡</div>
    <h3>No-fork isolation ladder</h3>
    <p>tiderace classifies each test and runs it the cheapest <em>sound</em> way: pure tests in-process (no fork), state-mutating tests in-process with snapshot/restore, only opaque cases forked. Most tests never pay the ~4.5&nbsp;ms fork.</p>
  </div>
  <div class="feature-card">
    <div class="icon">🎯</div>
    <h3>Impact Analysis</h3>
    <p>Per-test source footprints (via <code>sys.monitoring</code>) + content hashing mean tiderace only re-runs tests affected by what you changed. No change → nothing runs.</p>
  </div>
  <div class="feature-card">
    <div class="icon">📊</div>
    <h3>Coverage Built In</h3>
    <p>Per-test coverage via CPython's <code>sys.monitoring</code> (PEP&nbsp;669) — captured on the same in-process run, no separate coverage.py pass.</p>
  </div>
  <div class="feature-card">
    <div class="icon">🦀</div>
    <h3>Its own engine, in Rust</h3>
    <p>Collection, fixture graph, scheduling, hashing, and impact all run at native speed. Python only ever runs your test bodies.</p>
  </div>
  <div class="feature-card">
    <div class="icon">💾</div>
    <h3>Build-system-for-tests</h3>
    <p>Outcomes are content-addressed: a result is a pure function of its inputs, so an unchanged test is free — and a result computed in CI is reusable on any machine.</p>
  </div>
  <div class="feature-card">
    <div class="icon">🔌</div>
    <h3>Compatible &amp; native authoring</h3>
    <p>Runs ordinary function / method / unittest-style tests with fixtures. Optionally drop pytest entirely with native type-DI authoring, and <code>riptide migrate</code> to convert existing suites.</p>
  </div>
  <div class="feature-card">
    <div class="icon">👀</div>
    <h3>Warm daemon</h3>
    <p>A warm interpreter keeps your project imported once; <code>watch</code> re-runs only impacted tests on save — millisecond feedback loops.</p>
  </div>
</div>

## Benchmarks

Measured on `benchmarks/fixtures/fx_corpus` (509 fixture tests; numpy/sqlite), hyperfine. Reproduce with
the [benchmark harness](guides/benchmarks.md). tiderace's win grows as you go from cold full runs to the
warm inner loop.

| scenario | pytest | **tiderace** | speedup |
|---|---:|---:|---:|
| **Cold** — full run (all 509 execute) | 0.94 s | **0.66 s** | **1.4× faster** |
| **Warm** — no changes (impact-skip) | 0.84 s | **9.4 ms** | **89×** |
| **Warm** — inner loop, 1 changed test | 0.27 s | **~5 ms** | **~50–70×** |

!!! tip "Why so fast?"
    Two levers compound. **Impact analysis** means an unchanged test never runs — after one coverage run
    tiderace knows exactly which tests touch which files. And for the tests that *do* run, the **no-fork
    ladder** removes the per-test `fork()` that dominated execution — so even a cold full run beats pytest.

## Quick Install

!!! note "Pre-release"
    The pure-Rust engine builds from source from the `engine/` workspace. Prebuilt binaries are not
    published yet.

```bash
# Build from source — needs Rust + a Python 3.12+ interpreter
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace/engine && cargo build --release

# Point the engine at your project's Python shim and interpreter
export RIPTIDE_SHIM="$PWD/py-shim/shim.py"
export RIPTIDE_PYTHON="$(which python3)"

# Run (impact-aware) / full run / warm watch loop
./target/release/riptide-daemon run   /path/to/tests
./target/release/riptide-daemon run   /path/to/tests --all
./target/release/riptide-daemon watch /path/to/tests
```

!!! info "Naming"
    The project is **tiderace**. The engine binaries currently build as `riptide` / `riptide-daemon`
    (a retired codename being consolidated under tiderace) — read them as tiderace. The legacy
    `tiderace` binary that orchestrated pytest is the previous generation.

## How It Works

```
┌──────────────────────────────────────────────────────────────┐
│                          tiderace                            │
│                                                              │
│  1. COLLECT    Discover tests via fast regex (Rust)         │
│       ↓                                                      │
│  2. GRAPH      Build the fixture closure per test (Rust)    │
│       ↓                                                      │
│  3. SCHEDULE   Locality + load-balance across N wellsprings │
│       ↓                                                      │
│  4. IMPACT     Skip tests whose deps didn't change          │
│       ↓                                                      │
│  5. ISOLATE    pure → no-fork · impure → restore · opaque → fork │
│       ↓                                                      │
│  6. RUN        Invoke body in the warm interpreter (shim)   │
│       ↓                                                      │
│  7. PERSIST    Outcomes + coverage footprint + purity       │
└──────────────────────────────────────────────────────────────┘
```

[Read the architecture docs →](design/architecture.md) · [Full system architecture (ARCHITECTURE.md) →](https://github.com/snoodleboot-io/tiderace/blob/main/ARCHITECTURE.md)
