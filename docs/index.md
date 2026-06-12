---
hide:
  - navigation
  - toc
---

<div class="riptide-hero">
  <h1>rip<span>tide</span> ⚡</h1>
  <p class="tagline">The Rust-powered Python test engine that only runs what changed.</p>
  <div class="badges">
    <span class="badge">Rust</span>
    <span class="badge">parallel execution</span>
    <span class="badge">impact analysis</span>
    <span class="badge">coverage</span>
    <span class="badge">SQLite state</span>
  </div>
  <div class="cta">
    <a href="guides/quickstart/" class="btn-primary">Get Started →</a>
    <a href="https://github.com/snoodleboot-io/riptide" class="btn-secondary">View on GitHub</a>
  </div>
</div>

## Why riptide?

Python test suites are slow. `pytest` runs every test every time. `pytest-xdist` parallelises but doesn't know what changed. `pytest-testmon` knows what changed but is pure Python. No tool does all three — until now.

<div class="feature-grid">
  <div class="feature-card">
    <div class="icon">⚡</div>
    <h3>Parallel by Default</h3>
    <p>Rayon-powered thread pool runs tests concurrently across all CPU cores with zero configuration.</p>
  </div>
  <div class="feature-card">
    <div class="icon">🎯</div>
    <h3>Impact Analysis</h3>
    <p>SHA-256 file fingerprinting + coverage dep maps mean riptide only reruns tests affected by your changes.</p>
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
    <p>Works with your existing pytest-compatible test files. No rewrites, no annotations required.</p>
  </div>
</div>

## Benchmarks

How riptide compares on a 200-test Python project after changing a single source file:

<div style="margin: 1.5rem 0;">
  <div class="bench-row">
    <span class="bench-label">riptide</span>
    <div class="bench-bar-wrap">
      <div class="bench-bar riptide" style="width: 8%;">0.7s</div>
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
    After one coverage run, riptide knows exactly which tests import which source files. Change one file, run only its tests. The Rust binary itself starts in <5ms.

## Quick Install

```bash
# Download the binary
curl -sSfL https://github.com/snoodleboot-io/riptide/releases/latest/download/riptide-linux-x86_64 \
  -o riptide && chmod +x riptide

# Or build from source
cargo install riptide

# Run your tests
riptide tests/
```

## How It Works

```
┌─────────────────────────────────────────────────────────┐
│                        riptide                          │
│                                                         │
│  1. COLLECT   Scan .py files via regex AST (Rust)      │
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
