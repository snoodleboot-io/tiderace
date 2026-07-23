# Migrating pytest → tiderace

tiderace is its own framework ([ADR-E001](../../planning/current/pure-rust-test-engine/design/adr/ADR-E001-pure-rust-engine-no-pytest.md)/[E012](../../planning/current/pure-rust-test-engine/design/adr/ADR-E012-native-type-driven-authoring.md)) — it does **not** run pytest. Migration is a **one-time source codemod**, not a runtime compat shim. You translate once; pytest never runs again.

```bash
# report only (writes nothing) — inspect first
python -m tiderace.migrate tests/test_foo.py

# apply: writes tests/test_foo.tiderace.py
python -m tiderace.migrate tests/test_foo.py --write
```

Exit code is **non-zero while anything remains in the can't-map list** — wire it into an adoption gate. The codemod only `ast.parse`s your source, so **pytest need not be installed** to migrate.

> The rewrite uses stdlib `ast` + `ast.unparse`: it **normalizes formatting and drops comments**. The *report* is exact regardless. (A later version can swap in libcst to preserve formatting.)

## Mapping table (what migrates automatically)

| pytest | → tiderace | notes |
|---|---|---|
| `import pytest` | `import tiderace` | |
| `@pytest.fixture` | `@tiderace.provides` | `scope=` / `autouse=` / `name=` carried over |
| fixture with `-> T` return type | `@tiderace.provides` + `T` becomes the inject-by type | the type is what test params wire to |
| `def test(db)` where `db` is a typed fixture | `def test(db: Db)` | **type inferred** from the provider's return type |
| `@pytest.mark.parametrize("a,b", [...])` | `@tiderace.cases([...])` | `ids=` preserved |
| `@pytest.mark.skipif(c, reason=r)` | `@tiderace.skip_if(c, reason=r)` | |
| `@pytest.mark.skip` / `xfail` | `@tiderace.skip` / `@tiderace.xfail` | |
| `@pytest.mark.<name>` (other) | `@tiderace.tag("<name>")` | selection metadata |

## Cannot map — finish by hand (the report names each one)

Because tiderace wires by **type** and pytest fixtures rarely carry types, these need you:

1. **Untyped fixture** — no `-> T` and no inferable type. The codemod rewrites the decorator but flags it: add `-> <Type>`. *(This is the single biggest migration cost of type-DI — owned openly.)*
2. **Test param off an untyped fixture** — can't annotate `param: ?`; flagged for manual annotation.
3. **Parametrized fixture** (`@pytest.fixture(params=[...])`) — provider-level params aren't in tiderace yet; convert to `@tiderace.cases` on the tests, or split the resource.
4. **`request`** (incl. `request.getfixturevalue` / `request.addfinalizer`) — dynamic; port to typed deps + yield teardown.
5. **`@pytest.mark.usefixtures("x")`** — a string name carries no type; request it as a typed param, or mark the provider `autouse=True`.
6. **pytest builtins** (`tmp_path`, `monkeypatch`, `capsys`, …) — no tiderace equivalent yet; provide your own resource.
7. **`pytest_*` hooks / `from pytest import …`** — tiderace gets its own hook host later; port manually.

## After migrating

The output authors against tiderace only. An **untyped provider will fail at import** (tiderace needs the type) — that's deliberate: the gap fails loud rather than silently staying on pytest. Add the types the report asks for, then run with the tiderace engine — no pytest in the loop.
