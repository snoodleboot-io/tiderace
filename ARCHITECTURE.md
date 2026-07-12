# tiderace — Architecture

**A pure-Rust test engine for Python.** tiderace owns test collection, the fixture graph, scheduling,
isolation, coverage, and impact analysis in compiled Rust; a thin Python *shim* is the only thing that
runs inside CPython, and it exists solely to import user code and invoke test bodies. There is **no
pytest at runtime** — tiderace is the runner, not a wrapper around one.

> **Naming.** The project is **tiderace**. During the pure-Rust rebuild the engine was codenamed
> *riptide*; that name is retired. Some binaries and identifiers still carry it (`riptide`,
> `riptide-daemon`, `RIPTIDE_*`, `.riptide-state.json`) pending a mechanical rename — read them as
> tiderace. An earlier generation — a separate `tiderace` binary that orchestrated *pytest* workers —
> has been removed.

---

## 1. The big picture

Two processes, one seam. Rust holds all the logic and state; CPython is a dumb execution substrate
reached over a narrow, swappable transport.

```mermaid
flowchart LR
    subgraph rust["Rust — the engine (owns all logic & state)"]
        CLI["riptide / riptide-daemon<br/>(CLI &amp; warm daemon)"]
        CORE["engine-core<br/>collection · fixtures · scheduler<br/>coverage · impact · cache · exec"]
        CLI --> CORE
    end

    subgraph seam["ShimTransport seam"]
        PIPE["PipeTransport<br/>(length-prefixed JSON frames)"]
        INPROC["InProcessTransport<br/>(PyO3 FFI — experimental ②)"]
    end

    subgraph py["Python — the substrate (no logic)"]
        SHIM["py-shim/shim.py<br/>import user code · invoke test body<br/>fork / no-fork+restore · coverage · purity"]
        USER["user tests + fixtures<br/>(+ py-riptide authoring pkg)"]
        SHIM --> USER
    end

    CORE <-->|ExecRequest / ExecResponse| PIPE
    CORE <-.experimental.-> INPROC
    PIPE <-->|stdin/stdout| SHIM
    INPROC <-.in-memory.-> SHIM
```

**Why this split.** Everything that benefits from being fast, typed, and parallel (graph building,
scheduling, hashing, impact) lives in Rust. The one thing that *must* be in Python — running Python — is
a few hundred lines of shim. The [`ShimTransport`](#7-the-transport-seam) trait means the engine never
knows whether Python is a subprocess over pipes or an embedded interpreter over FFI.

---

## 2. Component map

```mermaid
flowchart TB
    subgraph bins["Binaries"]
        RIP["riptide<br/>(engine-cli)<br/>collect · run"]
        DAE["riptide-daemon<br/>(engine-daemon)<br/>run · serve · watch · bench"]
        PROBE["inproc-probe<br/>(engine-inproc) ②"]
    end

    subgraph core["engine-core (library — the engine)"]
        COL["collection<br/>RegexCollector"]
        DOM["domain<br/>NodeId · Scope · Outcome<br/>TestItem · TestResult"]
        FIX["fixtures<br/>FixtureGraph · resolver<br/>closure · finalizers · overrides"]
        SCH["scheduler<br/>LocalityScheduler<br/>WorkerBatch (LPT)"]
        EXEC["exec<br/>Wellspring · ForkWorker<br/>WatermarkStack · transport<br/>shim_protocol"]
        COV["coverage<br/>DepGraph · CoverageReport"]
        IMP["impact<br/>ImpactAnalyzer · Selection"]
        CACHE["cache<br/>CacheKey · Tiered/Local/Null<br/>purity"]
        REP["reporter<br/>terminal · json · junit<br/>github · sarif"]
        HOOK["hooks<br/>HookHost · events"]
    end

    subgraph daemon["engine-daemon (warm server)"]
        EH["EngineHandler"]
        POOL["pool (parallel wellsprings)"]
        PERS["persist (.riptide-state.json)"]
        WATCH["watch · fs_watcher · invalidator"]
        RPC["rpc_server · socket · session"]
    end

    subgraph python["Python"]
        SHIM["py-shim/shim.py<br/>(executor)"]
        AUTH["py-riptide/riptide<br/>(@provides · @cases · @uses · migrate)"]
    end

    RIP --> core
    DAE --> daemon
    daemon --> core
    PROBE --> core
    EXEC -->|frames| SHIM
    AUTH -.imported by.-> SHIM
```

| Layer | Crate / dir | Responsibility |
|---|---|---|
| CLI | `engine-cli` → `riptide` | one-shot `collect` / `run` |
| Daemon | `engine-daemon` → `riptide-daemon` | warm server: impact-aware `run`, `serve` (RPC), `watch`, parallel pool |
| Engine | `engine-core` | all collection/graph/schedule/exec/coverage/impact/cache logic |
| ② | `engine-inproc` → `inproc-probe` | embedded-CPython transport experiment (PyO3) |
| Substrate | `py-shim/shim.py` | import user code, invoke bodies, isolation, coverage, purity |
| Authoring | `py-riptide/riptide` | native type-DI decorators + `migrate` codemod |

---

## 3. The run pipeline

One full run, end to end:

```mermaid
sequenceDiagram
    participant U as user (CLI)
    participant C as Collector (Rust)
    participant F as FixtureGraph (Rust)
    participant S as LocalityScheduler (Rust)
    participant P as Pool (Rust)
    participant W as Wellspring(s) (CPython + shim)
    participant R as Reporter (Rust)

    U->>C: run <path>
    C->>C: discover test files & node ids (regex collect)
    C->>F: tests + requested params
    F->>F: build fixture closure per test (scopes, overrides)
    F->>S: ScheduledTest list (node, locality key, weight)
    S->>S: group by module (locality) + LPT-balance across N workers
    S->>P: WorkerBatches
    par one wellspring per core
        P->>W: launch (import project ONCE), then per test:
        loop each test in batch
            P->>W: ExecRequest{node, style, force_no_fork}
            W->>W: isolate (pure / restore / fork) · run body · capture coverage+purity
            W-->>P: ExecResponse{outcome, detail, coverage, pure}
        end
    end
    P->>R: TestResults (+ touched files)
    R-->>U: report + exit code
```

Collection and graph-building are pure Rust and cheap. The cost is execution — which is why the
[isolation ladder](#6-the-isolation-ladder) and [impact analysis](#8-coverage--impact-analysis) exist.

---

## 4. Execution: the warm wellspring & fork model

Isolation without paying interpreter startup per test. A **Wellspring** is a CPython process that imports
the project **once**; tests are then run as children via `fork()` (copy-on-write), so each test gets a
pristine view of imported state for ~the cost of a `fork`, not a fresh `python -c`.

```mermaid
flowchart TB
    L["launch wellspring"] --> I["import project ONCE<br/>(numpy, conftest, user modules)"]
    I --> RDY["ready (warm)"]
    RDY --> REQ{"ExecRequest<br/>per test"}
    REQ -->|"fork path"| FK["fork() → COW child<br/>run body → report → _exit"]
    REQ -->|"no-fork path"| NF["run in-process<br/>(snapshot/restore — §6)"]
    FK --> REQ
    NF --> REQ
```

- **Import once, fork many** — the warm import is the expensive part; COW children share it. (ADR-E003)
- **Per-test deadline** — a child exceeding its deadline is killed and reported `Error`.
- **WatermarkStack** — tracks fixture setup/teardown across scopes so finalizers run in the right order
  as the engine moves between modules/classes.
- **Parallelism** — the daemon runs **N wellsprings, one per core** (`engine-daemon/pool.rs`), each its
  own warm import; the [`LocalityScheduler`](#5-scheduling) keeps a module's tests on one worker.

> **Historical note:** `fork()` per test (~4.5 ms) was the dominant cost. The isolation ladder (§6) now
> avoids the fork wherever it's sound, so most tests never pay it.

---

## 5. Scheduling

```mermaid
flowchart LR
    T["tests + weights<br/>(timing history or equal)"] --> G["group by locality key<br/>(module = file part of node id)"]
    G --> LPT["LPT bin-packing<br/>(longest-processing-time first)<br/>across N workers"]
    LPT --> B["WorkerBatches<br/>(balanced, module-coherent)"]
```

The `LocalityScheduler` (ADR-E010) does two things at once: **scope locality** (a module's tests land on
the same worker, so its module/session fixtures are set up once) and **load balance** (LPT greedy packing
keeps workers evenly busy). A `RoundRobinScheduler` exists as a simpler baseline.

---

## 6. The isolation ladder ⭐

The heart of tiderace's speed. We `fork()` to isolate tests from each other — but most tests don't need a
fork. The engine classifies each test and runs it the cheapest **sound** way. This is automatic; there is
no user flag.

```mermaid
flowchart TD
    START["test to run"] --> STATIC{"static pre-filter<br/>(AST): obviously<br/>mutates shared state?"}
    STATIC -->|"yes (global / env / process-global call)"| FORK
    STATIC -->|"no obvious impurity"| RESTORABLE{"module<br/>snapshot-restorable?<br/>(no opaque globals)"}
    RESTORABLE -->|"no (opaque globals)"| FORK["FORK<br/>COW child<br/>~4.5 ms · bulletproof"]
    RESTORABLE -->|"yes"| KNOWN{"known pure?<br/>(recorded verdict)"}
    KNOWN -->|"yes"| BARE["BARE NO-FORK<br/>run in-process, no snapshot<br/>~0.05 ms (90×)"]
    KNOWN -->|"unknown / impure"| RESTORE["NO-FORK + RESTORE<br/>snapshot → run → undo mutation<br/>~0.4–0.9 ms (5–14×)"]
    RESTORE --> VERIFY["purity guard verifies<br/>(records verdict for next time)"]
    BARE --> DONE["outcome + coverage + purity"]
    VERIFY --> DONE
    FORK --> DONE
```

Three tiers, picked per test:

| Tier | When | Isolation mechanism | Rel. cost |
|---|---|---|---|
| **bare no-fork** | test is *known pure* (recorded verdict) | nothing to isolate | ~0.05 ms (90×) |
| **no-fork + restore** | *restorable* footprint, purity unknown/impure | deep-copy snapshot of module globals + `os.environ`, run, restore | ~0.4–0.9 ms (5–14×) |
| **fork** | module has *opaque* (un-deep-copyable) globals | copy-on-write child | ~4.5 ms (1×) |

Key properties:

- **Sound by construction.** No-fork + restore *contains* mutation rather than predicting it; a
  non-restorable module always falls back to fork (`shim._restorable()`). So correctness never depends on
  the purity verdict — the verdict is only an optimization (lets a known-pure test skip the snapshot).
- **No learning pass.** Restore works on the very first run; the **purity guard** records verdicts as a
  free side effect of running, so subsequent runs can promote pure tests to the bare tier.
- **Static pre-filter** (`shim.static_impurity`) is a cheap AST scan that flags obvious mutators (`global`,
  writes to free/module names, `os.environ`/`os.chdir`/`random.seed`-style calls) without running — a
  sufficient (conservative) impurity test that seeds the tier decision.

The daemon enables this by default: it sets `RIPTIDE_RESTORE=1` and requests no-fork on every test; the
shim downgrades to fork only where unsound. `RIPTIDE_FORCE_FORK=1` reverts to fork-per-test (debug /
benchmark baseline only — not a user flag).

---

## 7. The transport seam

`ShimTransport` is the one boundary between the Rust engine and Python. It is a single synchronous
exchange: send an `ExecRequest`, block for an `ExecResponse`.

```mermaid
classDiagram
    class ShimTransport {
        <<trait>>
        +ready() ReadyInfo
        +exchange(ExecRequest) ExecResponse
    }
    class PipeTransport {
        length-prefixed JSON
        over stdin/stdout
    }
    class InProcessTransport {
        PyO3 FFI into
        embedded CPython (②)
    }
    class ScriptedShim {
        pure-Rust test double
        (no process, no syscall)
    }
    ShimTransport <|.. PipeTransport
    ShimTransport <|.. InProcessTransport
    ShimTransport <|.. ScriptedShim
```

- **PipeTransport** (production) — frames over a child process's pipes (the wellspring).
- **InProcessTransport** (experimental, ②, ADR-E013) — one embedded CPython driven by FFI; no subprocess,
  no pipe. Proven to work; benchmarking showed the pipe was *not* the bottleneck (the fork was), so it
  remains a research path toward *import-once + parallel fork*.
- **ScriptedShim** (tests) — lets the entire `Worker → frames → TestResult` path run in one thread with no
  Python at all, so execution logic is testable offline.

The wire frame is additive and back-compatible: new fields (`coverage`, `force_no_fork`, `pure`) are
`skip-if-default`, so an old frame is byte-identical.

---

## 8. Coverage → impact analysis

The "only re-run what changed" engine. Each test's executed-source footprint is captured via CPython's
`sys.monitoring` (ADR-E006), folded into a dependency graph, and persisted; on the next run only tests
whose dependencies changed are executed.

```mermaid
flowchart TB
    subgraph run1["Run (coverage on)"]
        EXE["execute test"] --> MON["sys.monitoring<br/>records touched lines/files"]
        MON --> CR["CoverageReport<br/>(per test: files → lines)"]
        CR --> DG["DepGraph<br/>(test ↔ source files)"]
        DG --> ST["persist .riptide-state.json<br/>(per-test deps + file content hashes)"]
    end

    subgraph run2["Next run"]
        HASH["hash current files"] --> CHG{"which files changed?<br/>(content hash diff)"}
        CHG --> PLAN["plan():<br/>to_run = tests touching changed files<br/>cached = the rest"]
        PLAN -->|to_run| RERUN["execute (isolation ladder)"]
        PLAN -->|cached| SERVE["serve prior outcome<br/>(no execution)"]
    end

    ST -.feeds.-> CHG
```

Two complementary layers:

- **Impact-skip (active path, `engine-daemon/persist.rs`).** Per-run, local: `.riptide-state.json` stores
  each test's dependency files (from coverage) + file content hashes. On re-run, `changed_files()` +
  `plan()` select only impacted tests; with **no** changes nothing runs — the wellspring isn't even
  launched.
- **Content-addressed cache (`engine-core/cache`, ADR-E004).** The cross-machine layer: a test's outcome
  keyed by its full input closure (`CacheKey`), in a `TieredCache(Local, Remote)`. A cache *hit* means the
  test is never run — and because the key is content-addressed, a result CI computed is reusable on any
  machine with the same inputs. The `purity` gate excludes nondeterministic tests from caching. The remote
  tier is a shareable **`DirCache`** (a directory: a CI cache path / shared mount / artifact); an HTTP or
  object-store client is a drop-in behind the same `Cache` trait. The daemon consults it in `run`
  (**cache hit → impact-skip → run**): set `RIPTIDE_CACHE_DIR` to a shared directory and a result CI
  computed is served without re-running, even when this machine's local impact state is stale. Only
  *pure* outcomes are cached (the purity gate keeps it sound).

---

## 9. The warm daemon lifecycle

The daemon is the product's inner loop: keep CPython warm, re-run only what changed, react to file saves.

```mermaid
stateDiagram-v2
    [*] --> Cold
    Cold --> Warm: first run launches<br/>wellspring(s) (import once)
    Warm --> Warm: run / RPC Run<br/>(reuse warm import)
    Warm --> ImpactSkip: run (no changes)<br/>serve cached, no exec
    ImpactSkip --> Warm: file changes
    Warm --> Watch: watch <root>
    Watch --> Watch: on save → re-run<br/>impacted only
    Warm --> Serve: serve (Unix socket)
    Serve --> Serve: RPC: Discover / Run /<br/>Health / Recycle
    Serve --> [*]: Shutdown
    Recycle: drop stale interpreter
    Serve --> Recycle: Recycle
    Recycle --> Serve: relaunch on next Run
```

Modes (`riptide-daemon <mode> <root>`):

- **`run`** — impact-aware one-shot (coverage on; runs only changed tests, parallel pool).
- **`run --all`** — full run across the parallel pool.
- **`serve`** — bind the per-project Unix socket; answer RPC (`Discover`, `Run`, `Health`, `Recycle`,
  `Shutdown`) over a persistent warm session.
- **`watch`** — block and re-run impacted tests on each save (`fs_watcher` + `invalidator` + the DepGraph).
- **`bench`** — time cold vs warm passes.

---

## 10. Authoring & migration (py-riptide)

tiderace runs ordinary pytest-style tests (function/method/unittest styles, fixtures) **and** offers a
native, type-driven authoring model so suites can drop the pytest dependency entirely.

- **Native type-DI** (ADR-E012): `@riptide.provides` (declare a provider by return type),
  `@riptide.cases` (parametrization), `@riptide.uses` (set up by type, not injected). Fixtures resolve by
  **type**, built by the Rust fixture graph.
- **`riptide migrate`** — an AST codemod (`py-riptide/riptide/migrate.py`) that rewrites a pytest suite to
  the native model; conformance is tracked by auto-map % over pinned real-world repos.

---

## 11. Design decisions (ADR index)

The authoritative rationale lives in `planning/current/pure-rust-test-engine/design/adr/`:

| ADR | Decision |
|---|---|
| E001 | Pure-Rust engine, no pytest at runtime |
| E002 | Python shim as the execution substrate |
| E003 | Fork-from-warm-wellspring snapshot isolation |
| E004 | Content-addressed result cache |
| E005 | Workspace trait seams (testable boundaries) |
| E006 | Coverage via `sys.monitoring` |
| E007 | Warm daemon |
| E008 | Cross-platform strategy |
| E009 | Lazy assertion introspection |
| E010 | Locality scheduler (LPT + scope locality) |
| E011 | Shim transport seam |
| E012 | Native type-driven authoring |
| E013 | In-process / FFI isolation (②) |
| **E014** | **No-fork + restore isolation ladder (the default execution path)** |
| E015 | Conditional sub-interpreter tier (`SubInterpWorker`) for Windows parallelism (design; spiked) |

---

## 12. Where to look in the code

| You want… | Start here |
|---|---|
| How tests are found | `engine-core/src/collection/regex_collector.rs` |
| The fixture graph | `engine-core/src/fixtures/fixture_graph.rs`, `layered_resolver.rs` |
| Scheduling | `engine-core/src/scheduler/locality_scheduler.rs` |
| The fork model | `engine-core/src/exec/wellspring.rs`, `fork_worker.rs` |
| The transport seam | `engine-core/src/exec/transport.rs`, `shim_protocol.rs` |
| The isolation ladder | `py-shim/shim.py` (`static_impurity`, `_restorable`, `_restore_shared`, `Engine.run`) |
| Impact-skip | `engine-daemon/src/persist.rs`, `engine_handler.rs` (`run_impacted`) |
| The cache | `engine-core/src/cache/` (`cache_key.rs`, `tiered_cache.rs`, `purity.rs`) |
| Parallel pool | `engine-daemon/src/pool.rs` |
| Daemon modes | `engine-daemon/src/main.rs`, `rpc_server.rs`, `watch.rs` |
| Authoring / migration | `py-riptide/riptide/` (`builtins`, `_resolve.py`, `migrate.py`) |
| Benchmarks | `benchmarks/RESULTS-3way.md`, `RESULTS-inproc.md`, `bench_3way.sh` |
