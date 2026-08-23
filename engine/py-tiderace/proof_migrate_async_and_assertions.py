#!/usr/bin/env python3
"""Proof: `migrate` handles coroutines, and never emits a module naming an unbound `pytest`.

Covers TID-36 and TID-38, which are one bug wearing two hats: the migrator rewrote
`import pytest → import tiderace` unconditionally while leaving parts of the module still saying
`pytest`, so the output *looked* migrated and raised `NameError` at call time. The report claimed
success, so nothing surfaced until the suite ran. On one real 542-file suite that was 86 modules.

Run: python proof_migrate_async_and_assertions.py
"""
import ast
import sys

from tiderace.migrate import migrate_source

FAILURES: list[str] = []


def check(label: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  PASS  {label}")
    else:
        FAILURES.append(label)
        print(f"  FAIL  {label}{(' — ' + detail) if detail else ''}")


def names_pytest(src: str) -> bool:
    """Whether the source references the bare name `pytest` anywhere."""
    return any(
        isinstance(n, ast.Name) and n.id == "pytest" and isinstance(n.ctx, ast.Load)
        for n in ast.walk(ast.parse(src))
    )


def imports_pytest(src: str) -> bool:
    return any(
        isinstance(n, ast.Import) and any(a.name == "pytest" for a in n.names)
        for n in ast.walk(ast.parse(src))
    )


print("TID-36 — coroutines are visited")
out, _ = migrate_source(
    'import pytest\n'
    '\n'
    'class T:\n'
    '    @pytest.mark.parametrize("k", ["a", "b"])\n'
    '    def test_sync(self, k: str) -> None: pass\n'
    '\n'
    '    @pytest.mark.parametrize("k", ["a", "b"])\n'
    '    async def test_async(self, k: str) -> None: pass\n'
)
check("async test's parametrize is rewritten", out.count("tiderace.cases") == 2, out)
check("the async def stays async", "async def test_async" in out, out)
check("no unbound pytest is left behind", not (names_pytest(out) and not imports_pytest(out)), out)

out, _ = migrate_source(
    "import pytest\n"
    "\n"
    "@pytest.fixture\n"
    "async def conn() -> int:\n"
    "    yield 1\n"
)
check("async fixture becomes a provider", "tiderace.provides" in out, out)
check("async fixture stays a coroutine", "async def conn" in out, out)


print("\nTID-38 — assertion helpers, and the import when they are not enough")
out, rep = migrate_source(
    "import pytest\n"
    "\n"
    "def test_a():\n"
    "    with pytest.raises(ValueError):\n"
    "        raise ValueError()\n"
    "    assert 0.1 + 0.2 == pytest.approx(0.3)\n"
)
check("pytest.raises → tiderace.raises", "tiderace.raises" in out, out)
check("pytest.approx → tiderace.approx", "tiderace.approx" in out, out)
check("a fully-mappable module does NOT keep the import", not imports_pytest(out), out)

out, rep = migrate_source(
    "import pytest\n"
    "\n"
    "def test_a():\n"
    "    pytest.importorskip('numpy')\n"
)
check("an unmappable module KEEPS import pytest", imports_pytest(out), out)
cants = [f.message for f in rep.findings if f.kind == "cant_map"]
check("...and says so as a can't-map finding", any("importorskip" in c for c in cants), str(cants))

# The mirror of the bug: a decorator that *is* mappable must not drag the import back in. Detecting
# residual references during the AST walk gets this wrong, because the node is replaced afterwards.
out, _ = migrate_source(
    "import pytest\n"
    "\n"
    '@pytest.mark.skipif(True, reason="x")\n'
    "def test_a(): pass\n"
)
check("a mapped decorator does not resurrect the import", not imports_pytest(out), out)


print("\nThe helpers themselves behave like the pytest ones they replace")
import tiderace

with tiderace.raises(ValueError, match="bad"):
    raise ValueError("very bad")
check("raises matches on message", True)

with tiderace.raises(KeyError) as caught:
    {}["k"]
check("raises exposes .value", isinstance(caught.value, KeyError), repr(caught.value))

try:
    with tiderace.raises(ValueError):
        pass
except AssertionError:
    check("a block that does not raise fails", True)
else:
    check("a block that does not raise fails", False, "no AssertionError")

try:
    with tiderace.raises(ValueError):
        raise TypeError("unrelated")
except TypeError:
    check("an unrelated exception propagates", True)
else:
    check("an unrelated exception propagates", False, "TypeError was swallowed")

check("approx compares scalars", 0.1 + 0.2 == tiderace.approx(0.3))
check("approx compares sequences", [1.0, 2.0] == tiderace.approx([1.0, 2.0000001]))
check("approx compares mappings", {"a": 1.0} == tiderace.approx({"a": 1.0}))
check("approx still distinguishes", 1.0 != tiderace.approx(2.0))
check("approx handles nan", float("nan") == tiderace.approx(float("nan")))

print()
if FAILURES:
    print(f"FAILED: {len(FAILURES)} check(s): {', '.join(FAILURES)}")
    sys.exit(1)
print("all checks passed")
