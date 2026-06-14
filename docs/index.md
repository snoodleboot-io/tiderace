---
hide:
  - navigation
  - toc
---

<div class="tiderace-hero">
  <h1>tide<span>race</span> ⚡</h1>
  <p class="tagline">The Rust-powered Python test engine that only runs what changed.</p>
  <div class="badges">
    <span class="badge">Rust</span>
    <span class="badge">batched parallel</span>
    <span class="badge">impact analysis</span>
    <span class="badge">coverage</span>
    <span class="badge">watch mode</span>
  </div>
  <div class="cta">
    <a href="guides/quickstart/" class="btn-primary">Get Started →</a>
    <a href="https://github.com/snoodleboot-io/tiderace" class="btn-secondary">View on GitHub</a>
  </div>
</div>

## Why tiderace?

Python test suites are slow. `pytest` runs every test every time. `pytest-xdist` parallelises but doesn't know what changed. `pytest-testmon` knows what changed but is pure Python. No tool does all three — until now.

<div class="feature-grid">
  <div class="feature-card">
    <div class="icon">⚡</div>
    <h3>Batched & Parallel</h3>
    <p>A Rayon worker pool runs pytest batched — one process per worker — across all cores, paying interpreter startup once per worker, not per test.</p>
  </div>
  <div class="feature-card">
    <div class="icon">🎯</div>
    <h3>Impact Analysis</h3>
    <p>SHA-256 file fingerprinting + coverage dep maps mean tiderace only reruns tests affected by your changes.</p>
  </div>
  <div class="feature-card">
    <div class="icon">📊</div>
    <h3>Coverage Built In</h3>
    <p>Per-test coverage tracking via coverage.py, merged into a unified report with visual progress bars.</p>
  </div>
  <div class="feature-card">
    <div class="icon">🦀</div>
    <h3>Written in Rust</h3>
    <p>Test collection, file hashing, parallelism, and state management all run at native speed.</p>
  </div>
  <div class="feature-card">
    <div class="icon">💾</div>
    <h3>Persistent State</h3>
    <p>SQLite database remembers test results, file hashes, and dependency graphs across runs.</p>
  </div>
  <div class="feature-card">
    <div class="icon">🔌</div>
    <h3>Drop-in Compatible</h3>
    <p>Real pytest under the hood — fixtures, plugins, parametrize, async, and unittest all just work. No rewrites, no annotations.</p>
  </div>
  <div class="feature-card">
    <div class="icon">👀</div>
    <h3>Watch Mode</h3>
    <p><code>tiderace watch</code> keeps a warm pool of workers (pytest imported once) and re-runs only impacted tests on save — sub-second feedback loops.</p>
  </div>
</div>

## Benchmarks

Illustrative — how the runners compare on a ~200-test project after changing a single source file (tiderace re-runs only the impacted tests; the others re-run more). Numbers vary by machine; reproduce with the [benchmark harness](guides/benchmarks.md).

<div style="margin: 1.5rem 0;">
  <div class="bench-row">
    <span class="bench-label">tiderace</span>
    <div class="bench-bar-wrap">
      <div class="bench-bar tiderace" style="width: 8%;">0.7s</div>
    </div>
  </div>
  <div class="bench-row">
    <span class="bench-label">testmon</span>
    <div class="bench-bar-wrap">
      <div class="bench-bar pytest" style="width: 35%;">3.1s</div>
    </div>
  </div>
  <div class="bench-row">
    <span class="bench-label">xdist</span>
    <div class="bench-bar-wrap">
      <div class="bench-bar pytest" style="width: 75%;">6.8s</div>
    </div>
  </div>
  <div class="bench-row">
    <span class="bench-label">pytest</span>
    <div class="bench-bar-wrap">
      <div class="bench-bar pytest" style="width: 100%;">9.2s</div>
    </div>
  </div>
</div>

!!! tip "Why so fast?"
    After one coverage run, tiderace knows exactly which tests import which source files. Change one file, run only its tests. The Rust binary itself starts in <5ms.

## Quick Install

!!! note "Pre-release"
    tiderace isn't published to crates.io / GitHub Releases yet — build from source for now. The download and `cargo install` lines below are how it will work once published.

```bash
# Build from source (works today) — needs Rust + Python with pytest & coverage
git clone https://github.com/snoodleboot-io/tiderace
cd tiderace && cargo build --release
./target/release/tiderace tests/

# Future (once published): prebuilt binary
curl -sSfL https://github.com/snoodleboot-io/tiderace/releases/latest/download/tiderace-linux-x86_64 \
  -o tiderace && chmod +x tiderace

# Future (once published): from crates.io
cargo install tiderace
```

## How It Works

```
┌─────────────────────────────────────────────────────────┐
│                        tiderace                          │
│                                                         │
│  1. COLLECT   Scan .py files via fast regex (Rust)     │
│       ↓                                                 │
│  2. HASH      SHA-256 fingerprint every source file     │
│       ↓                                                 │
│  3. DIFF      Compare against SQLite state DB           │
│       ↓                                                 │
│  4. IMPACT    Map changed files → affected tests        │
│       ↓                                                 │
│  5. RUN       Rayon parallel worker pool                │
│       ↓                                                 │
│  6. PERSIST   Store results + coverage dep graph        │
└─────────────────────────────────────────────────────────┘
```

[Read the architecture docs →](design/architecture.md)
