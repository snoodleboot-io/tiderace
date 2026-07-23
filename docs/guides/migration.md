# Migrating from pytest

tiderace is **its own framework**, not a pytest wrapper ([ADR-E001](../design/decisions.md) /
[E012](../design/decisions.md)) — it does not run pytest at runtime. Migration is a **one-time source
codemod**: `tiderace migrate` rewrites a pytest test file into tiderace's native model, and after that
pytest is never in the loop.

You don't *have* to migrate — tiderace already runs ordinary pytest-style function, method, and
`unittest.TestCase` tests with fixtures. Migration is for suites that want to **drop the pytest
dependency entirely** and author against tiderace's type-driven model.

## How it works

The codemod only `ast.parse`s your source, so **pytest need not be installed** to migrate.

```bash
# Report only — writes nothing. Inspect what would change first.
python -m tiderace.migrate tests/test_foo.py

# Apply — writes tests/test_foo.tiderace.py alongside the original.
python -m tiderace.migrate tests/test_foo.py --write
```

The exit code is **non-zero while anything remains in the can't-map list**, so you can wire the report
straight into an adoption gate ("fail CI until this suite is fully migratable").

!!! note "Formatting"
    The rewrite uses stdlib `ast` + `ast.unparse`: it **normalizes formatting and drops comments** in
    the written output. The *report* is exact regardless. (A later version may swap in libcst to
    preserve formatting.)

## What migrates automatically

tiderace resolves fixtures **by type** — the return type of a provider is what a test parameter wires
to. The codemod translates the mechanical parts of pytest to that model:

| pytest | → tiderace | notes |
|---|---|---|
| `import pytest` | `import tiderace` | |
| `@pytest.fixture` | `@tiderace.provides` | `scope=` / `autouse=` / `name=` carried over |
| fixture with `-> T` return type | `@tiderace.provides` + inject-by `T` | the type is what tests wire to |
| `def test(db)` where `db` is a typed fixture | `def test(db: Db)` | **type inferred** from the provider's return type |
| `@pytest.mark.parametrize("a,b", [...])` | `@tiderace.cases([...])` | `ids=` preserved |
| `@pytest.mark.skipif(c, reason=r)` | `@tiderace.skip_if(c, reason=r)` | |
| `@pytest.mark.skip` / `xfail` | `@tiderace.skip` / `@tiderace.xfail` | |
| `@pytest.mark.<name>` (other) | `@tiderace.tag("<name>")` | selection metadata |

## What needs a human (the report names each one)

Because tiderace wires by **type** and pytest fixtures rarely carry types, these are flagged for you
rather than guessed:

1. **Untyped fixture** — no `-> T` and no inferable type. The decorator is rewritten but flagged: add
   `-> <Type>`. *(This is the single biggest migration cost of type-DI — owned openly.)*
2. **Test param off an untyped fixture** — can't annotate `param: ?`; flagged for manual annotation.
3. **Parametrized fixture** (`@pytest.fixture(params=[...])`) — provider-level params aren't in
   tiderace yet; convert to `@tiderace.cases` on the tests, or split the resource.
4. **`request`** (incl. `request.getfixturevalue` / `request.addfinalizer`) — dynamic; port to typed
   deps + `yield` teardown.
5. **`@pytest.mark.usefixtures("x")`** — a string name carries no type; request it as a typed param, or
   mark the provider `autouse=True`.
6. **pytest builtins** (`tmp_path`, `monkeypatch`, `capsys`, …) — provide your own resource.
7. **`pytest_*` hooks / `from pytest import …`** — port manually.

## After migrating

The output authors against tiderace only. An **untyped provider fails at import** — tiderace needs the
type — which is deliberate: the gap fails loudly rather than silently keeping you on pytest. Add the
types the report asks for, then run with the engine (see [Quick Start](quickstart.md)); no pytest in the
loop.

## How complete is it?

Auto-map rate is tracked against pinned real-world suites (`conformance/`):

| repo | auto-mapped |
|---|---:|
| pallets/click | **95%** |
| agronholm/anyio | **99%** |
| pallets/flask | **83%** |
| **overall** | **91%** |

The remaining gap is dominated by **untyped providers and untyped fixture params** — exactly the cases
above where the codemod flags rather than guesses, because inventing a type would be wrong more often
than right.
