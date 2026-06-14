# Design Overview

tiderace is built around three core ideas:

## 1. Only run what changed

Most test runs after the first one should be **dramatically** smaller. If you changed `src/auth.py`, you should run 8 tests, not 200. This requires knowing which tests depend on which source files — and that knowledge must be built from runtime data (coverage), not static analysis.

## 2. Run it fast in Rust

The orchestration layer — file scanning, hashing, dep lookup, process spawning — runs in a compiled Rust binary. Python is only involved when a test actually executes. This eliminates the Python interpreter startup overhead for everything except the tests themselves.

## 3. Zero configuration, persistent state

A developer should be able to drop `tiderace` into any existing pytest project and get value immediately. No annotations, no config files, no server. State accumulates automatically in a local SQLite file and gets smarter over time.

## The Three Pillars in Practice

```
Run 1 (--all --coverage):  200 tests   12s    builds dep graph
Run 2 (touch auth.py):       8 tests    0.7s   ⚡ 94% skipped
Run 3 (no changes):          0 tests    0.01s  ⚡ 100% skipped
Run 4 (touch utils.py):     23 tests    1.8s   ⚡ 88% skipped
```

The first run pays the full cost. Every run after that is proportional to what actually changed.

## Non-Goals

tiderace is not:
- A replacement for `pytest` — it delegates execution to pytest
- A test framework — no new test syntax or assertions
- A CI platform — it's a binary that slots into existing CI
- A coverage reporter beyond what's needed for dep mapping — use `coverage html` or `codecov` for rich coverage UI
