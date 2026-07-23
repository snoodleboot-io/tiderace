# 01 — Architecture (C4 + Module Map + Master Class Diagram)

> **Status:** ✅ draft for discussion
> Prereq: [00-vision](00-vision.md). Drills down in: [02-domain-model](02-domain-model.md) onward.

This document gives the top-down structure: C4 context → container → component, the proposed
crate/module layout (honoring one-class-per-file + SOLID), and a **master classifier (class)
diagram** of the engine's core traits and types. Each subsystem then gets its own deep-dive doc
with its own classifier + behavioral diagrams.

---

## 1. C4 Level 1 — System Context

Who/what the engine talks to.

```mermaid
graph TD
    dev["👤 Developer"]
    ci["🤖 CI runner"]
    ide["🧩 IDE / Test Explorer<br/>(VS Code, PyCharm)"]

    engine["⛵ Tiderace Test Engine<br/>(Rust binary + daemon)"]

    cpython["🐍 CPython substrate<br/>(user code, fixtures, tests)"]
    remote["☁️ Remote result cache<br/>(shared, content-addressed)"]
    fsys["📁 Project source tree<br/>+ pyproject.toml"]

    dev -->|"run / watch / query"| engine
    ci -->|"run --ci"| engine
    ide <-->|"discover / run / stream results (JSON-RPC)"| engine
    engine -->|"fork + execute bodies"| cpython
    engine <-->|"get/put cache entries"| remote
    engine -->|"read sources, hashes, config"| fsys
```

## 2. C4 Level 2 — Containers

The deployable/runnable pieces.

```mermaid
graph TD
    subgraph host["Developer / CI host"]
        cli["CLI front-end<br/>(thin: parse args → talk to core)"]
        daemon["Test Daemon<br/>(long-lived, warm; JSON-RPC server)"]
        core["Engine Core library<br/>(collection, graph, scheduler, cache, hooks)"]

        wellspring["Wellspring process(es)<br/>(CPython, fully imported)"]
        workers["Fork workers<br/>(one CPython per test, COW)"]
        shim["Python shim<br/>(Rust-shipped, ~tiny)"]

        store[("Local cache + metadata<br/>(content store + SQLite index)")]
    end

    remote[("Remote cache<br/>(optional)")]

    cli --> core
    daemon --> core
    core --> wellspring
    wellspring -.->|"fork()"| workers
    workers --> shim
    core <--> store
    store <--> remote
    daemon <-->|JSON-RPC| ide["IDE"]
```

**Notes**
- `cli` and `daemon` are two front-ends over the **same `core` library** — DIP: front-ends
  depend on core abstractions, not vice versa.
- The **wellspring** imports the project once; **fork workers** are COW children that run exactly
  one test each then exit (free isolation). See [05-execution-wellspring](05-execution-wellspring.md).
- The **shim** is the only Python we ship; it is dumb on purpose (receives "import X, call Y").

## 3. C4 Level 3 — Components (Engine Core)

Internal components of the `core` library and their dependencies.

```mermaid
graph TD
    cfg["Config Loader"]
    col["Collector"]
    dm["Domain Model<br/>(TestItem, Fixture, Scope…)"]
    fg["Fixture Graph Resolver"]
    cache["Cache (content-addressed)"]
    impact["Impact Analyzer"]
    sched["Scheduler<br/>(bin-pack + scope locality)"]
    exec["Execution Engine<br/>(Worker trait + Wellspring)"]
    assert["Assertion Introspector"]
    cov["Coverage Collector<br/>(sys.monitoring)"]
    hooks["Plugin / Hook Host"]
    rep["Reporter(s)"]
    orch["Run Orchestrator"]

    cfg --> orch
    col --> dm
    dm --> fg
    orch --> col
    orch --> cache
    orch --> impact
    orch --> sched
    orch --> exec
    orch --> rep
    impact --> cache
    sched --> fg
    exec --> fg
    exec --> assert
    exec --> cov
    cov --> impact
    hooks -. observes .-> orch
    hooks -. observes .-> exec
```

**The preference order the Orchestrator enforces** (vision principle #1):

```mermaid
graph LR
    A[selected tests] --> B{cache hit?}
    B -->|yes| C[serve cached result]
    B -->|no| D{impacted by change?}
    D -->|no| C
    D -->|yes| E[schedule + fork + run]
    E --> F[write cache + report]
```

---

## 4. Proposed crate / module layout

Migrating from the current single `tiderace` binary to a **Cargo workspace** so the reusable
engine is a library (testable, embeddable by both CLI and daemon) — DIP + ISP at the crate
level. One class/type per file per conventions.

```text
crates/
├── engine-core/            # the library; no I/O front-end concerns
│   └── src/
│       ├── domain/         # TestItem, Fixture, Scope, NodeId, Outcome … (02)
│       ├── collection/     # Collector trait + impls (03)
│       ├── fixtures/       # FixtureGraph, resolver, scope layers (04)
│       ├── exec/           # Worker trait, Wellspring, ForkWorker … (05)
│       ├── scheduler/      # Scheduler trait + LocalityScheduler (06)
│       ├── cache/          # Cache trait, ContentStore, KeyBuilder (07)
│       ├── impact/         # ImpactAnalyzer (11)
│       ├── coverage/       # CoverageCollector (11)
│       ├── assertion/      # Introspector (09)
│       ├── hooks/          # HookHost, Hook trait (12)
│       ├── report/         # Reporter trait + impls (13)
│       ├── config/         # Config loader (13)
│       └── error.rs        # typed errors (thiserror) (13)
├── engine-cli/             # thin CLI front-end
├── engine-daemon/          # JSON-RPC test server (08)
└── py-shim/                # the Rust-shipped Python substrate shim
    └── shim.py
```

> The existing `tiderace/*.rs` modules (`collector`, `hasher`, `db`, `impact`, `pool`,
> `runner`, `watcher`) are the **conceptual ancestors** of these — several port forward (the
> regex collector, SHA-256 hasher, SQLite index, impact analyzer). The runner/pool are
> replaced by `exec/` (wellspring/fork) since we no longer drive pytest.

---

## 5. Master classifier (class) diagram — core traits & types

High-level relationships only; attributes/operations are detailed per-subsystem in 02–13.
Traits (interfaces) are the seams for DIP; concrete types are the default impls.

```mermaid
classDiagram
    direction LR

    class RunOrchestrator {
        +run(selection) RunReport
    }

    %% ---- Collection ----
    class Collector {
        <<trait>>
        +collect(roots) Vec~TestItem~
    }
    class RegexCollector
    class AstCollector
    Collector <|.. RegexCollector
    Collector <|.. AstCollector

    %% ---- Domain ----
    class TestItem {
        +node_id NodeId
        +style TestStyle
        +scope_path ScopePath
        +marks Vec~Mark~
    }
    class TestStyle {
        <<enumeration>>
        PytestFunction
        PytestClassMethod
        UnittestMethod
    }
    class Fixture {
        +name String
        +scope Scope
        +deps Vec~String~
    }
    class Scope {
        <<enumeration>>
        Function
        Class
        Module
        Package
        Session
    }

    %% ---- Fixtures ----
    class FixtureGraph {
        +resolve(test) FixturePlan
        +topo_order() Vec~Fixture~
    }

    %% ---- Cache ----
    class Cache {
        <<trait>>
        +get(key) Option~CachedOutcome~
        +put(key, outcome)
    }
    class CacheKeyBuilder {
        +key_for(test, closure) CacheKey
    }
    class TieredCache
    class LocalCache
    class RemoteCache
    Cache <|.. TieredCache
    Cache <|.. LocalCache
    Cache <|.. RemoteCache
    TieredCache o-- LocalCache
    TieredCache o-- RemoteCache

    %% ---- Scheduling ----
    class Scheduler {
        <<trait>>
        +plan(tests, history) Vec~WorkerBatch~
    }
    class LocalityScheduler
    Scheduler <|.. LocalityScheduler

    %% ---- Execution ----
    class Worker {
        <<trait>>
        +execute(batch, plan) Vec~TestResult~
    }
    class Wellspring {
        +snapshot(scope) Watermark
        +fork_at(handle) Worker
    }
    class ForkWorker
    class ThreadWorker
    class RemoteWorker
    Worker <|.. ForkWorker
    Worker <|.. ThreadWorker
    Worker <|.. RemoteWorker
    Wellspring ..> ForkWorker : creates

    %% ---- Cross-cutting ----
    class AssertionIntrospector {
        +explain(failure) RichDiff
    }
    class CoverageCollector {
        <<trait>>
        +contexts() DepGraph
    }
    class HookHost {
        +emit(event)
    }
    class Reporter {
        <<trait>>
        +on_result(TestResult)
        +finish(RunReport)
    }

    %% ---- Relationships ----
    RunOrchestrator --> Collector
    RunOrchestrator --> Cache
    RunOrchestrator --> Scheduler
    RunOrchestrator --> Worker
    RunOrchestrator --> Reporter
    RunOrchestrator --> HookHost
    Collector --> TestItem
    TestItem --> TestStyle
    TestItem --> Fixture
    Fixture --> Scope
    FixtureGraph --> Fixture
    Scheduler --> FixtureGraph
    Worker --> FixtureGraph
    Worker --> AssertionIntrospector
    Worker --> CoverageCollector
    Cache --> CacheKeyBuilder
    CoverageCollector --> CacheKeyBuilder : feeds closure
```

---

## 6. Concurrency & process model (overview)

- **Rust side:** the orchestrator runs on a bounded async/thread pool. Result aggregation uses
  `par_iter().map().collect()`-style fan-in (no shared `Mutex<Vec<_>>`), carried over from the
  current design's lock-poisoning-free approach.
- **Process side:** parallelism is **multi-process** (each fork worker has its own GIL).
  In-process `ThreadWorker` is reserved for free-threaded CPython (PEP 703) as a drop-in behind
  the `Worker` trait — no rewrite needed when it matures.
- **Isolation:** every test runs in its own forked interpreter; a crash/timeout kills only that
  child (process-group kill, as today's `procutil` does).

Detailed lifecycle/state machines live in [05-execution-wellspring](05-execution-wellspring.md) and
[08-daemon](08-daemon.md).

---

## 7. Key seams (Dependency Inversion points)

These traits are where the design stays open/closed and testable:

| Trait | Swappable implementations | Enables |
|---|---|---|
| `Worker` | `ForkWorker`, `ThreadWorker`, `RemoteWorker`, `SubprocessWorker` | platform fallback, free-threaded future, distributed exec |
| `Cache` | `LocalCache`, `RemoteCache`, `TieredCache`, `NullCache` | CI sharing; disable for debugging |
| `Collector` | `RegexCollector`, `AstCollector` | fast scan vs precise import-time info |
| `CoverageCollector` | `MonitoringCollector` (3.12+), `TraceCollector` (≤3.11) | version portability |
| `Reporter` | terminal, JSON, JUnit, GitHub, SARIF | CI/IDE integrations |
| `Scheduler` | `LocalityScheduler`, `RoundRobinScheduler` | tuning makespan vs fixture reuse |

---

## 8. What this buys us vs the old design

| Concern | Old (orchestrate pytest) | New (this design) |
|---|---|---|
| Framework ownership | pytest owns fixtures/asserts/marks | **engine owns them** |
| Per-test isolation | too expensive (process per test) | **free** (fork from snapshot) |
| Result caching | impact-skip only | **content-addressed, shareable** |
| Startup cost | per worker | **once** (wellspring) |
| Extensibility | bounded by pytest | **own trait-based hook host** |
