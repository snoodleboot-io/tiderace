# Phase 3 — Frozen Contract (Fixtures + Watermarks; consumed by Phases 4–6)

> Status: 🧊 **FROZEN at the contract step** (2026-06-17) by `architect-agent`. Implemented (data
> shapes + behavior seams) in `engine/crates/engine-core/src/fixtures/**` and `…/exec/**`.
> Extends — does **not** break — the [Phase 2 contract](../../phase-2-workspace-domain-collection/CONTRACT.md).
> Changes to anything here require **pausing all lanes and re-presenting the plan** (PLAN §10): a
> `FixturePlan`/`Watermark`/`ScopeLayer` shape change invalidates WM/FB/ATDD work in flight.
>
> **Compiles clean:** `cargo build -p engine-core` ✓, `cargo clippy -p engine-core -- -D warnings` ✓,
> Phase 2 lib tests (18) still green. The contract-freeze scaffolds (`unimplemented!("LANE: …")`)
> are the lanes' starting points (§7) and are the **only** allowed `unimplemented!` — the final
> enforcement gate forbids any remaining.

---

## 1. What this phase freezes (and why it is a barrier)

Every code lane builds on these shapes. They are split into **pure-data types** (fully defined here —
lanes consume them as-is) and **behavior seams** (public type/trait + method signatures defined here;
bodies are clearly-marked `unimplemented!("LANE: …")` scaffolds the owning lane replaces). The wire
framing (Phase 2 §3) is unchanged.

---

## 2. Frozen public surfaces

### 2.1 `FixtureError` taxonomy — `engine_core::fixtures::FixtureError`  (RATIFIED, C-FX)

A `thiserror` enum; a fixture-graph **validity** error surfaced at build/resolve time (distinct from
`EngineError`, and distinct from a test `Outcome::Error`). Frozen variants (new variants may be
*added* later without breaking consumers; existing variants/fields do not change):

| Variant | Fields | Meaning |
|---|---|---|
| `Cycle` | `path: Vec<String>` | dependency cycle (`a→b→a`); abort collection for the scope path, don't deadlock. `path` = fixture names along the back-edge, request order. |
| `ScopeWiden` | `narrow: Scope`, `wide: Scope` | a **wider**-scoped fixture depends on a **narrower** one. `wide` is the **offender** (the depending fixture's own scope); `narrow` is the illegal dependency's scope. Invariant for a *legal* edge: `dep.scope.outlives(node.scope) || dep.scope == node.scope`. |
| `Unresolved` | `name: String`, `scope_path: ScopePath` | a (transitively) required fixture name has no definition visible from `scope_path` (no override entry is a prefix of the request location). |
| `DuplicateAutouse` | `name: String` | two autouse fixtures collide at the same effective location (minimal identifier = the name). |
| `ParamShapeMismatch` | `name: String` | a parametrized fixture's declared parameter shape is inconsistent (e.g. `ids` length ≠ `params` length). |

`FixtureError` derives `Debug, Error, Clone, PartialEq, Eq` (cloneable + comparable so Phases 4/5/7
can report on and assert against it).

### 2.2 `Fixture` (W1) — pure data — `engine_core::fixtures::Fixture`

`{ node_id: NodeId, name: String, scope: Scope, deps: Vec<String>, autouse: bool,
params: Option<Vec<ParamValue>>, is_yield: bool, reinit_after_fork: bool, scope_path: ScopePath }`.
`deps` are **names** (resolved to node ids in the graph, because the same name resolves differently
per requesting location). Builder setters: `new`, `with_deps`, `autouse`, `with_params`, `yielding`,
`reinit_after_fork`; accessors `node_id`, `scope`, `is_parametrized`.

### 2.3 `ParamValue` — pure data — `engine_core::fixtures::ParamValue`

`{ id: String, index: usize }`. **Design decision (param value repr).** A parametrized fixture's
actual values are arbitrary Python objects that must **not** cross the Rust↔shim boundary as live
values. pytest reasons over the **param id** (the bracketed token in a node id) + the **index**, not
the value. The Rust engine needs exactly (1) a stable hashable identity so each variant gets a
distinct `closure_hash`, and (2) the index the shim uses to select the live value in-child.
`ParamValue` carries precisely those — `Hash + Eq` so it feeds `closure_hash` directly.

### 2.4 `FixtureInstance` (W5) — pure data — `engine_core::fixtures::FixtureInstance`

`{ fixture: NodeId, param: Option<ParamValue>, closure_hash: ClosureHash }`. A definition bound to a
concrete parameter selection. **Each parametrization variant carries a distinct `closure_hash`**
(invariant §4). Unparametrized ⇒ `param = None`, one instance.

### 2.5 `ClosureHash` (W14) — pure data — `engine_core::fixtures::ClosureHash`

Newtype over `[u8; 32]` (`from_bytes`, `as_bytes`, `to_hex`). The `fixture_closure` term of the
[ADR-E004](../../design/adr/ADR-E004-content-addressed-cache.md) cache key. Phase 5 consumes it. The
*builder* that walks the closure and computes the digest is Lane FX-graph's (fx-hash); the *type* is
frozen.

### 2.6 `Finalizer` (W7) — pure data — `engine_core::fixtures::Finalizer`

`{ instance: FixtureInstance, scope: Scope, continuation: ShimHandle }`. The teardown half of a
yield-style fixture. **Rust owns ordering; the shim owns invocation** — so there is no `run()` method
on this type (invocation is the worker/shim's job, not a method that would need a scaffold). Replayed
in strict reverse capture order at owning-scope exit. `ShimHandle` is an opaque `u64` token the shim
assigns to a registered Python continuation.

### 2.7 `ScopeLayer` (W8) — pure data — `engine_core::fixtures::ScopeLayer`

`{ scope: Scope, scope_path: ScopePath, setup: Vec<FixtureInstance>, snapshot: Option<WatermarkId>,
reinit_in_child: Vec<NodeId>, finalizers: Vec<Finalizer> }`.

- **Snapshot-handle decision:** `snapshot` is `Option<WatermarkId>` — a **newtype over a watermark
  id defined in `exec`**, *not* the full `Watermark`. This decouples the resolver's *plan* from live
  wellspring runtime state (pid, `rss_bytes`, `is_live`). The resolver emits a plan referencing a
  layer id; `exec` mints the actual `Watermark` carrying that id. `Some` ⇒ this layer is a live fork
  source; `None` ⇒ not (yet) snapshotted.
- **`reinit_in_child` encoding:** the node ids of fork-fragile resource fixtures whose **pure part**
  is snapshotted at this layer but whose **handle** must be rebuilt per child (W11, split-setup).
  This is how the layer encodes "snapshot the migration, reopen the socket per child."

### 2.8 `FixturePlan` (W8) — pure data — `engine_core::fixtures::FixturePlan`

`{ test: NodeId, layers: Vec<ScopeLayer>, fork_from: Option<WatermarkId>,
post_fork: Vec<FixtureInstance>, fixture_args: FixtureArgs, closure_hash: ClosureHash }`.
The deliverable the executor + scheduler consume.

- `layers` ordered **widest → narrowest** (Session → Package → Module → Class); **never Function**.
- `fork_from = Some(deepest_shared)` (the narrowest-scoped live snapshot shared by the test), or
  `None` to fork the wellspring base (Layer 1; no wider-scope fixtures apply).
- `post_fork` = Function-scope instances (+ `reinit_after_fork` resources) run **in the child**.
- `fixture_args` (`FixtureArgs`) = `BTreeMap<String, FixtureInstance>` (deterministic order) — the
  argument-name → satisfying-instance binding the shim uses to call the body.

**Consumers:** Phase 4 → `post_fork` + `fixture_args`; Phase 5 → `closure_hash`; Phase 6 → the layers'
watermark `rss_bytes` (via `MemoryGovernor`) and `deepest_shared` scope for locality scheduling.

### 2.9 `Watermark` (W9) — pure data — `engine_core::exec::Watermark`

`{ id: WatermarkId, scope: Scope, scope_path: ScopePath, rss_bytes: u64, wellspring_pid: i64,
is_live: bool }`. A forkable point in wellspring memory (not a copy). `WatermarkId` = newtype over
`u64`. **`rss_bytes` is the load-bearing input to the Phase 6 `MemoryGovernor`** (seeds
`per_fork_estimate`). `is_live` flips to `false` on invalidate/retire.

### 2.10 Behavior seams (signatures frozen; bodies are lane scaffolds)

| Seam | Kind | Owner lane | Methods (signatures frozen) |
|---|---|---|---|
| `OverrideTable` | struct | FX-graph (fx-model) | `new`, `insert` (defined), `nearest(name, &ScopePath) -> Option<NodeId>` (scaffold, W6) |
| `FixtureGraph` | struct | FX-graph (fx-graph/resolver) | `build`, `detect_cycles`, `check_scope_monotonicity`, `topo_order`, `closure_of` (scaffolds); `fixture`, `deps_of` (defined) |
| `FixtureResolver` | **trait** | FX-graph (fx-resolver) | `resolve`, `layer_assignment`, `plan_for` (pure signatures — no scaffold) |
| `LayeredResolver` | struct (impl `FixtureResolver`) | FX-graph (fx-resolver) | the trait methods (scaffolds, W4/W8); `new`, `override_table` (defined) |
| `WatermarkStack` | struct | WM (wm-stack) | `deepest_shared`, `push_layer`, `invalidate_from`, `retire_layer` (scaffolds, W9); `new`, `layers` (defined) |
| `ForkPlan` | struct | WM (wm-fork) | `from(&FixturePlan, &WatermarkStack) -> ForkPlan` (scaffold, W10); fields + `fork_from` accessor (defined) |
| `MemoryGovernor` | struct | FALLBACK (fb-governor) | `max_concurrent_forks`, `admit`, `observe_rss` (scaffolds, W13); `new` (defined) |
| `ForkPermit` | struct | FALLBACK (fb-governor) | `new`, `charged_bytes` (defined; release-on-drop wired by lane) |
| `SubprocessWorker` | struct (impl `Worker`) | FALLBACK (fb-subproc) | `Worker::run` (scaffold, W12); `new`, `capabilities` (defined) |
| `WorkerCaps` | struct | (frozen, no lane) | `fork`, `subprocess` (defined) |

**Trait-vs-concrete rationale:** `FixtureResolver` is a **trait** because it is the abstraction the
scheduler/executor depend on (DIP, design 04 §3) and a cache-aware resolver should slot in later
without touching consumers — making it a trait means **zero scaffolds** (pure signatures).
`FixtureGraph`, `WatermarkStack`, `MemoryGovernor`, `SubprocessWorker`, `ForkPlan`, `OverrideTable`
are **concrete** because they own state (nodes/edges, layer vec, budget counters, pool config) with a
single Phase-3 implementation; their data-only accessors are defined, only the behavior is scaffolded.

---

## 3. The fixture-scope → Watermark-layer mapping (the deliverable for Phases 4–6)

| Fixture `Scope` | Watermark layer | Set up | Snapshotted? | Finalizer runs | In `FixturePlan` as |
|---|---|---|---|---|---|
| — (interpreter+stdlib) | Layer 0 (wellspring boot) | once per wellspring | implicit base | on wellspring death | — |
| — (project imports) | Layer 1 | once per wellspring | implicit base | on wellspring death | — (`fork_from = None` forks here) |
| `Session` | Layer 2 — `Watermark S` | once in wellspring lineage | **yes** | once, when layer retires | `ScopeLayer{scope:Session, snapshot:Some(S)}` |
| `Package` | Layer 2.5 (between S and M) | once per package path | **yes** (if shared) | once, when layer retires | `ScopeLayer{scope:Package, snapshot:Some(..)}` |
| `Module` | Layer 3 — `Watermark M` | once per module | **yes** | once, when layer retires | `ScopeLayer{scope:Module, snapshot:Some(M)}` |
| `Class` | Layer 4 — `Watermark C` (deepest) | once per class | **yes** | once, when layer retires | `ScopeLayer{scope:Class, snapshot:Some(C)}` |
| **`Function`** | **post-fork (no layer)** | **in the forked child** | **no** | **per test, in-child, reverse order** | `FixturePlan.post_fork[]` |
| any scope + `reinit_after_fork` | declared layer (pure part) **+ post-fork (fragile handle)** | pure part once; handle **per child** | pure part yes; handle no | pure part once at layer retire; handle per child | `ScopeLayer.reinit_in_child[]` + `ExecRequest.reinit[]` |

---

## 4. Invariants (relied on by Phases 4–6; enforced at the gates)

1. **Layers are append-only and scope-monotonic.** No `Function` state ever enters a snapshot
   (guaranteed upstream by the graph's scope-monotonicity check, `FixtureError::ScopeWiden`). A
   snapshot at scope *s* is a clean *wider-than-Function* world.
2. **`fork_from = WatermarkStack::deepest_shared(plan)`** — the narrowest-scoped **live** snapshot
   shared by the test; `None` ⇒ the wellspring base.
3. **The parent wellspring never runs a test body** — it stays a pristine fork source for the whole
   batch.
4. **Each `FixtureInstance` (incl. each parametrization) carries a distinct `closure_hash`** feeding
   the ADR-E004 cache key; parameter variants cache independently.
5. **The `SubprocessWorker` (no-COW) path is result-identical to the fork path** — it re-runs
   wider-scope setup **once per worker** instead of snapshotting (verified at §8 boundary 3). It
   advertises `WorkerCaps.supports_cow == false`.
6. **Snapshotted-scope finalizers run once** (at layer retire); only **Function** finalizers run
   per child, in reverse order.

---

## 5. Package-scope override tie-break  — ⚠️ PROPOSED, AWAITING RATIFICATION (PLAN §9 F3)

When two sibling packages define the same fixture name, the proposed rule is:
**longest-prefix `ScopePath` match wins** — resolution for a test at path `P` selects the definition
whose declaring `ScopePath` is the longest prefix of `P` (the existing nearest-override mechanism,
extended to the package layer). This is consistent with §2.7 / design 04 §1.4 and requires **no new
machinery**. It is **not yet ratified by the human**; Lane FX-graph implements the 5 scopes and this
prefix rule, but the tie-break stays flagged here until ratified. If the human chooses a different
rule (e.g. explicit-declaration-order, or error-on-ambiguity), only `OverrideTable::nearest` changes
— no frozen *shape* moves.

---

## 6. How `ExecRequest` / `ExecEvent` extend — **without changing the Phase 2 JSON framing**

The Phase 2 framing (length-prefixed `u32` LE + UTF-8 JSON) and the existing `ExecRequest` wire
fields (`node_id`, `style`, `deadline_ms`) are **unchanged**. Phase 3 adds three fixture fields to
`ExecRequest`, each `#[serde(default, skip_serializing_if = …)]` so a **fixtureless** request
serializes **byte-identically** to a Phase 2 frame:

| New field | Type | `skip_serializing_if` | Meaning |
|---|---|---|---|
| `post_fork` | `Vec<FixtureInstance>` | `Vec::is_empty` | Function-scope instances to set up in-child, topo order |
| `reinit` | `Vec<String>` | `Vec::is_empty` | `reinit_after_fork` fixture node ids to rebuild in-child (W11) |
| `fixture_args` | `FixtureArgs` | `FixtureArgs::is_empty` | the assembled argument map the body is called with |

`ExecRequest::bare(node_id, style, deadline_ms)` constructs the Phase-2-shaped (empty-fixture) form;
all existing call sites use it, keeping current frames identical. **`ExecEvent`** (the streaming
shim→Rust enum: `Started`/`Stdout`/`Stderr`/`Log`/`AssertionFailure`/`CoverageDelta`/`Finished`/
`FixtureError`, design 05 §5.2) is **forward-declared** here as the streaming upgrade of the Phase 2
single `ExecResponse`; Phase 3 keeps the frozen Phase 2 request/response round-trip and only *adds*
the fixture fields. The shim (`engine/py-shim/shim.py`, owned by Lane WM) gains: run `post_fork`
fixtures + `reinit` in the child, and register `Finalizer` continuations — the shim stays *dumb*
(ordering/policy lives in Rust).

> **Contract note for lanes:** `shim_protocol.rs` carries these frozen `ExecRequest` fields. WM/FB
> must **not** re-shape `ExecRequest`/`ExecEvent` mid-run; a change is a contract-change retry that
> pauses all lanes (PLAN §10).

---

## 7. Contract-freeze scaffolds (the lanes' starting points)

Every `unimplemented!("LANE: …")` below is a clearly-marked scaffold the **owning lane overwrites**.
These are the **only** permitted `unimplemented!`; the enforcement gate fails on any that remain.

| File | Method | Lane (subagent) | W# |
|---|---|---|---|
| `fixtures/override_table.rs` | `nearest` | FX-graph (fx-model) | W6 |
| `fixtures/fixture_graph.rs` | `build` | FX-graph (fx-graph) | W2/W3 |
| `fixtures/fixture_graph.rs` | `detect_cycles` | FX-graph (fx-graph) | W3 |
| `fixtures/fixture_graph.rs` | `check_scope_monotonicity` | FX-graph (fx-graph) | W3 |
| `fixtures/fixture_graph.rs` | `topo_order` | FX-graph (fx-resolver) | W4 |
| `fixtures/fixture_graph.rs` | `closure_of` | FX-graph (fx-resolver) | W4 |
| `fixtures/layered_resolver.rs` | `resolve` | FX-graph (fx-resolver) | W4/W8 |
| `fixtures/layered_resolver.rs` | `layer_assignment` | FX-graph (fx-resolver) | W8 |
| `fixtures/layered_resolver.rs` | `plan_for` | FX-graph (fx-resolver) | W8 |
| `exec/watermark_stack.rs` | `deepest_shared` | WM (wm-stack) | W9/W10 |
| `exec/watermark_stack.rs` | `push_layer` | WM (wm-stack) | W9 |
| `exec/watermark_stack.rs` | `invalidate_from` | WM (wm-stack) | W9 |
| `exec/watermark_stack.rs` | `retire_layer` | WM (wm-stack) | W9 |
| `exec/fork_plan.rs` | `from` | WM (wm-fork) | W10 |
| `exec/memory_governor.rs` | `max_concurrent_forks` | FALLBACK (fb-governor) | W13 |
| `exec/memory_governor.rs` | `admit` | FALLBACK (fb-governor) | W13 |
| `exec/memory_governor.rs` | `observe_rss` | FALLBACK (fb-governor) | W13 |
| `exec/subprocess_worker.rs` | `Worker::run` | FALLBACK (fb-subproc) | W12 |

`closure_hash` (W14, the `ClosureHash` *builder*) and parametrization fan-out (W5) have **no scaffold
method** — they are new logic Lane FX-graph adds in its own files (fx-hash, fx-param); the *types*
they produce (`ClosureHash`, `FixtureInstance`) are frozen.

---

## 8. Integration boundaries this contract must satisfy (live, no mocks)

Unchanged from PLAN §8: (1) fork-from-Watermark with real fixtures across all scopes (outcomes +
teardown order match pytest differentially; wider bodies run once); (2) non-fork-safe resource
re-initialized post-fork (each child gets a **fresh** sqlite connection; parent's never used
in-child); (3) no-COW `SubprocessWorker` path produces **identical** results to the fork path.

---

## 9. File-ownership map (parallel-safety — each lane only OVERWRITES its owned files)

`mod.rs` / `lib.rs` / `shim_protocol.rs` are owned by **NO lane** (frozen at the contract step). Lanes
never edit them; they only overwrite their owned files below. WM and FALLBACK both live under
`exec/**` but own **disjoint** files (the §PLAN-4 two-writer rule).

### Lane FX-graph — `engine-core/src/fixtures/**`
- `fixture.rs`, `param_value.rs` *(data — defined; lane may extend tests only)*
- `override_table.rs` (W1/W6: model + nearest/longest-prefix)
- `fixture_graph.rs`, `fixture_closure.rs` (W2/W3/W4: build + cycle + scope-monotone + topo/closure)
- `fixture_resolver.rs` *(trait — frozen)*, `layered_resolver.rs` (W4/W8: resolver + layering →
  `FixturePlan`)
- `scope_layer.rs`, `fixture_plan.rs`, `fixture_args.rs`, `fixture_instance.rs` *(data — defined; lane
  populates via resolver)*
- `finalizer.rs`, `shim_handle.rs` (W7: finalizer capture + reverse-order teardown ordering)
- `closure_hash.rs` (W14: the digest builder)
- parametrization fan-out (W5) → new instances in `fixture_instance.rs` path

### Lane WM — `engine-core/src/exec/**` + the shim
- `wellspring.rs` (W9: `ensure_layer`/`snapshot`/`retire_layer`/`invalidate_from`)
- `watermark.rs` *(data — defined)*, `watermark_stack.rs` (W9: stack + `deepest_shared`)
- `fork_plan.rs` (W10: `ForkPlan::from` + fork-from-deepest)
- `fork_worker.rs` (W10/W11: child runs `post_fork` then body)
- `engine/py-shim/shim.py` (W10/W11: run `post_fork` + `reinit` + register finalizers — **sole writer**)

### Lane FALLBACK — `engine-core/src/exec/**` (disjoint from WM)
- `subprocess_worker.rs` (W12: no-COW scope re-run, result-identical)
- `memory_governor.rs`, `fork_permit.rs` (W13: RSS budget + `admit` permit)
- `worker_caps.rs` *(data — defined; `supports_cow=false` is fixed)*

### Owned by NO lane (frozen — contract step)
- `fixtures/mod.rs`, `exec/mod.rs`, `lib.rs` (module wiring + exports)
- `exec/shim_protocol.rs` (the `ExecRequest`/`ExecEvent` wire shapes — §6)

---

## 10. Contract decisions the human should be aware of

1. **`ParamValue = {id, index}`** (not a serialized Python value) — see §2.3 rationale.
2. **`ScopeLayer.snapshot = Option<WatermarkId>`** (newtype over an exec id, not the full
   `Watermark`) — decouples the plan from live runtime state (§2.7).
3. **`Finalizer` has no `run()` method** — Rust owns ordering, the shim owns invocation; making
   `run()` a method would force a scaffold that misrepresents ownership (§2.6).
4. **`ExecRequest` extended in-place with skipped-when-empty fields**; the Phase 2 frame for a
   fixtureless test is byte-identical (§6). `ExecEvent` streaming enum is forward-declared, not yet
   replacing the Phase 2 round-trip.
5. **Package tie-break = longest-prefix (PROPOSED, not ratified)** — §5.
6. **`Worker` trait is unchanged from Phase 2** (`run(&mut self, &[TestItem]) -> Result<Vec<TestResult>>`).
   The design's richer `execute(batch, plan)` / `capabilities()` / `shutdown()` is **not** retrofitted
   onto the frozen trait this phase; `SubprocessWorker::capabilities()` is an inherent method instead.
   Flag: a later phase may widen the `Worker` trait (batch + plan) — that is a Phase 2-contract change,
   out of scope here.
