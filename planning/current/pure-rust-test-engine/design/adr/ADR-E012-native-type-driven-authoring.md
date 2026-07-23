# ADR-E012 — Native authoring surface: type-driven DI (our own thing, not a pytest copy)

**Status:** 🟢 Accepted (direction ratified by the human) · design + first build in progress

**Relates to:** [ADR-E001](ADR-E001-pure-rust-engine-no-pytest.md) (pure-Rust engine, no pytest),
[ADR-E011](ADR-E011-shim-transport-seam.md) (transport seam). **Completes ADR-E001's open seam**: the
"future native `@tiderace.fixture`" mentioned in `engine/py-shim/shim.py` and CONTRACT §11.2.

## Context

ADR-E001 made the *engine* ours (Rust owns collection, fixture graph, scopes, scheduling, assertions).
But the *authoring surface* was never ours: today the shim discovers fixtures by duck-typing on the
attributes **pytest's** `@pytest.fixture` stamps (`_fixture_function_marker`, `.scope`, `.params`,
`.autouse`, `_fixture_function` — verified against pytest 9.1). The shim never *imports* or *runs*
pytest, but it is coupled to pytest's decorator **shape**, and users still author against pytest. So
tiderace is a pure-Rust brain wearing pytest's face.

The human's directive: **build our own thing, not a copy.** The single most-disliked pytest trait is
**implicit name-matching DI** (`def test(db)` — where does `db` come from?). How we answer that *is*
tiderace's identity. Decision (chosen from a 4-way design fork): **wire by type, not by name.**

## Decision

A native `tiderace` Python package — the user-facing authoring surface — built on **type-driven
dependency injection**. Resources are resolved by a parameter's **type annotation**, never its name.

```python
import tiderace

@tiderace.provides(scope="module")          # a resource (fixture)
def db() -> Db:                            # provided type = the return annotation
    conn = Db.connect(":memory:")
    yield conn                             # yield ⇒ teardown after the scope
    conn.close()

def test_insert(db: Db):                   # injected BY TYPE (Db), not by the name "db"
    db.add("ada")
    assert db.count() == 1

@tiderace.cases([(2, 3, 5), (0, 0, 0)])     # parametrization, explicit
def test_add(a, b, exp):
    assert add(a, b) == exp
```

### The native marker set (ours, explicit — no `.mark.*` namespace)

| Concern | tiderace | replaces pytest |
|---|---|---|
| Resource / DI | `@tiderace.provides(scope=, autouse=, name=, type=)` | `@pytest.fixture` |
| Parametrization | `@tiderace.cases([...], ids=)` | `@pytest.mark.parametrize` |
| Skip | `@tiderace.skip(reason=)` / `@tiderace.skip_if(cond, reason=)` | `@pytest.mark.skip(if)` |
| Expected failure | `@tiderace.xfail(reason=, strict=)` | `@pytest.mark.xfail` |
| Tag / select | `@tiderace.tag("slow")` | `@pytest.mark.<name>` / keywords |

Scopes are the **5 the Rust engine already owns** (`function|class|module|package|session`) — that's
engine mechanics we keep, not a pytest import. Plain `assert` stays the assertion surface (the shim
already catches `AssertionError`; rich-diff introspection is Phase 4).

### The attribute protocol (ours, not pytest's)

Each decorator stamps a **tiderace-owned** attribute the shim/collector reads:

| Attribute | On | Carries |
|---|---|---|
| `__tiderace_provider__` | a provider fn | `ProviderSpec{provides: type, scope, autouse, name, is_yield}` |
| `__tiderace_cases__` | a test fn | normalized parameter sets (→ Rust `ParamValue{id, index}`) |
| `__tiderace_marks__` | a test fn | `[Mark]` (skip / skip_if / xfail / tag) |

The shim's `_is_fixture` gains a **native-first** branch: `hasattr(obj, "__tiderace_provider__")` wins;
the pytest-attr path becomes **compat-only** and is deletable once migration (below) is the norm.

### The architectural win: type-DI is a *discovery-layer* concern — the Rust engine does NOT change

The frozen Phase-3 engine (`FixtureGraph`, `LayeredResolver`, `WatermarkStack`, `FixturePlan`) resolves
dependencies **by name** — a `Fixture` already carries `deps: Vec<String>`. Type-DI is satisfied
*before* the graph: at discovery the shim builds a `type → provider` index, reads each function's
parameter **annotations**, and resolves them to provider **names** — producing exactly the `deps: [name]`
the Rust graph already consumes. **No frozen contract moves.** Type-driven injection is a front-end
resolution rule layered on the existing name-keyed engine; the banner feature costs the engine nothing.

Disambiguation (two providers of one type): exact-type match wins; ambiguity is a **native** authoring
error (`TideraceResolutionError`), with `typing.Annotated[T, "name"]` + `@provides(name=...)` as the
explicit escape hatch. (Native errors are ours — the Python-authoring analogue of Rust `FixtureError`.)

## Migration (no pytest at runtime — a one-time source codemod + a mapping + a can't-map list)

Adoption is a **source-to-source translation**, run once, not a permanent compat shim. `tiderace migrate`
(libcst/ast codemod) rewrites a pytest suite into native form and emits a per-run report:

**Mechanical mappings (the mapping table):**

| pytest | → tiderace | note |
|---|---|---|
| `@pytest.fixture` | `@tiderace.provides` | `scope=`/`autouse=` copied; yield/return preserved |
| `@pytest.mark.parametrize("a,b", [...])` | `@tiderace.cases([...])` | ids preserved |
| `@pytest.mark.skipif(c, reason=r)` | `@tiderace.skip_if(c, reason=r)` | |
| `@pytest.mark.skip` / `xfail` | `@tiderace.skip` / `@tiderace.xfail` | |
| `def test(db)` (name-DI) | `def test(db: Db)` (type-DI) | type looked up from the provider's provided type |

**The hard edge — and the explicit "cannot map" list** (emitted as TODOs + a report, never silently
dropped):

1. **Untyped fixtures / un-inferable provided type.** Type-DI needs a type; a pytest fixture with no
   return annotation and no inferable `return`/`yield` type **cannot** be auto-typed → flagged "annotate
   manually." This is the single biggest migration cost of choosing type-DI, and we own it openly.
2. **`request` introspection** (`request.getfixturevalue`, `request.node`, `request.config`) — dynamic,
   not statically resolvable → flagged, no auto-rewrite.
3. **`request.addfinalizer`** — only yield-style teardown maps; addfinalizer flagged.
4. **`@pytest.mark.usefixtures("x")`** (string name, no value/type) → needs the type; flagged if
   un-inferable.
5. **Indirect parametrization / `pytest.param(..., marks=...)` / id-functions** — partial map, remainder
   flagged.
6. **Plugins & `pytest_*` hooks** — out of scope (tiderace gets its own hook host later); flagged.
7. **Builtin fixtures** (`tmp_path`, `monkeypatch`, `capsys`, …) — map to tiderace's own builtins where
   they exist; unmapped ones flagged.

## Consequences

- ➕ tiderace is now its own thing **end to end** — engine *and* surface; pytest is neither imported nor
  authored-against in native mode.
- ➕ Type-DI is a real, modern differentiator (Python type hints as the wiring), not `s/pytest/tiderace/`.
- ➕ The Rust engine is untouched — type-DI rides the existing name-keyed `deps` (frozen contract safe).
- ➕ Migration is honest: a one-time codemod + a mapping table + a **named** can't-map list, not a
  forever-pytest dependency.
- ➖ Type-DI imposes an annotation burden pytest never did — the migration's main friction (owned, #1
  above).
- ➖ Real reimplementation surface: the `tiderace` package (decorators + resolver), the shim's native
  discovery branch, the codemod. Sequenced below.
- ⚠️ Ambiguous-type resolution must be a hard, clear error — implicit "first match wins" would reintroduce
  exactly the spooky-action we're rejecting.

## Alternatives considered (the 4-way fork)

1. **Type-driven DI** — *chosen.* Distinctive, explicit, leans on type hints.
2. **One explicit `@tiderace.test(use=[...], cases=[...])` decorator** — simple, but less of an identity;
   keep `use=` as a possible disambiguation affordance.
3. **Registry + `ctx` object** — most explicit, most verbose; rejected as boilerplate-heavy.
4. **Pytest-shaped, rebranded** — rejected: it is the copy the human explicitly pushed back on.

## Build sequence

- **N1 — `tiderace` package core** (this change): `provides` + type-DI resolver + `cases`; native errors;
  a pure-Python proof (no pytest) that resolves by type and runs bodies with teardown.
- **N2 — marks**: `skip`/`skip_if`/`xfail`/`tag` + their `__tiderace_marks__` protocol.
- **N3 — shim native discovery**: native-first `_is_fixture`, type→name resolution feeding the Rust graph;
  pytest path becomes compat-only.
- **N4 — `tiderace migrate`**: the codemod + mapping table + can't-map report.
- **N5 — conformance**: migrate a real OSS suite, measure auto-map rate, grow the can't-map list from
  reality.
- **B1 — native builtin resources** (ROADMAP-v2; delivered 2026-06-21): `tiderace.builtins` ships
  `monkeypatch`/`tmp_path`/`capsys`/`capfd` as ordinary function-scoped yield providers, auto-registered
  globally by the shim. **Decision — builtins are injected by *distinct* types, not bare stdlib types:**
  `MonkeyPatch`, `Capsys`, `Capfd`, and `TmpPath(pathlib.Path)` (a real `Path` subclass). A bare `Path`
  parameter would collide with user providers and wrongly capture *every* `Path` param, which violates
  the unambiguous-type-DI rule above. `migrate` rewrites builtin requests to typed params + injects the
  import; `tmpdir` maps to `TmpPath` with a py.path caveat. Measured: click `70% → 93%` auto-map
  (`conformance/CONFORMANCE.md`).

## B4 — `request` introspection decision (2026-06-23)

Conformance showed `request` usage splits into three cases; the native answer is decided per case
rather than offering a broad `Request` object:

- **`request.param`** — **supported** natively via provider-level params (B5, `@tiderace.provides(params=...)`).
  `migrate` no longer flags `request` on a parametrized provider.
- **`request.getfixturevalue(...)`** — **permanent can't-map.** Dynamic, name-keyed fixture lookup is
  the exact spooky-action type-DI rejects; the migration tells the user to request the provider as a
  typed parameter instead.
- **other `request.*`** (`node`, `config`, `addfinalizer`, …) — **manual port** (flagged). A narrow
  native `Request` is deliberately *not* introduced now; `addfinalizer` maps to yield-teardown, and
  node/config introspection is rare enough to leave to a hand-port until data says otherwise.

Measured: clearing the `request.param` false-positives lifted anyio `89% → 99%`, total `87% → 89%`.

## Revisit trigger

If real-world migration shows the type-annotation burden (#1) stalls adoption, add an opt-in
name-fallback resolver (`@provides(name=)` + a name-matched arg) as a *disambiguation* affordance —
without making implicit name-DI the default. Identity stays "wire by type."

If the "other `request.*`" bucket grows on a broader corpus, revisit introducing a **narrow** native
`Request` (`.node`/`.config` read-only) — but never `getfixturevalue` (which would reopen name-DI).
