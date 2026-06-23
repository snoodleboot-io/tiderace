"""riptide migrate — a **one-time** pytest → riptide source codemod. No pytest at runtime, ever.

Produces two things (ADR-E012, step N4):
  1. a best-effort **migrated source**, and
  2. a **report**: the MAPPING (what was rewritten) + the CAN'T-MAP list (what you must finish by
     hand, with the reason). Because riptide wires by **type** and pytest fixtures rarely carry types,
     the can't-map list is the point — it names every gap instead of guessing.

Limitation (stated openly): the rewrite uses stdlib `ast` + `ast.unparse`, which **normalizes
formatting and drops comments**. A production migrator would use libcst to preserve them; the *report*
is exact regardless. Run report-only first (`python -m riptide.migrate <path>`), inspect, then `--write`.
"""
from __future__ import annotations

import argparse
import ast
import sys
from dataclasses import dataclass, field

# pytest builtins riptide now provides natively (ROADMAP-v2 B1): name -> riptide.builtins type. A
# request for one is rewritten to a typed param (`monkeypatch` -> `monkeypatch: MonkeyPatch`) wired by
# type, and the matching `from riptide.builtins import ...` is injected. `tmpdir` maps to `TmpPath`
# (pathlib) — its legacy py.path methods (`.join`, `.strpath`, …) still need a manual port, so it's
# mapped *with a caveat*, not silently.
BUILTIN_PROVIDERS = {
    "monkeypatch": "MonkeyPatch",
    "tmp_path": "TmpPath",
    "tmpdir": "TmpPath",
    "capsys": "Capsys",
    "capfd": "Capfd",
}

# pytest builtins riptide has no native equivalent for (yet) — requesting one can't auto-map.
BUILTIN_FIXTURES = {
    "request", "tmp_path_factory", "tmpdir_factory",
    "caplog", "recwarn", "pytestconfig", "cache", "doctest_namespace",
}


@dataclass
class Finding:
    lineno: int
    kind: str  # "mapped" | "cant_map"
    message: str


@dataclass
class Report:
    findings: list = field(default_factory=list)

    def mapped(self, lineno: int, msg: str) -> None:
        self.findings.append(Finding(lineno, "mapped", msg))

    def cant(self, lineno: int, msg: str) -> None:
        self.findings.append(Finding(lineno, "cant_map", msg))

    @property
    def cant_map(self) -> list:
        return [f for f in self.findings if f.kind == "cant_map"]

    @property
    def mappings(self) -> list:
        return [f for f in self.findings if f.kind == "mapped"]


# --------------------------------------------------------------------------- decorator helpers
def _dotted(node) -> str:
    """Dotted name of a decorator expression: `pytest.fixture` / `pytest.mark.parametrize`."""
    target = node.func if isinstance(node, ast.Call) else node
    parts: list[str] = []
    while isinstance(target, ast.Attribute):
        parts.append(target.attr)
        target = target.value
    if isinstance(target, ast.Name):
        parts.append(target.id)
    return ".".join(reversed(parts))


def _kwarg(call: ast.Call, name: str):
    if isinstance(call, ast.Call):
        for kw in call.keywords:
            if kw.arg == name:
                return kw.value
    return None


def _is_generator(fn: ast.FunctionDef) -> bool:
    for node in ast.walk(fn):
        if isinstance(node, (ast.Yield, ast.YieldFrom)) and node is not fn:
            return True
    return False


def _provided_type_src(fn: ast.FunctionDef) -> str | None:
    """The provided type, as source, from the return annotation (unwrapping `Iterator[T]`/
    `Generator[T, ...]` for yield fixtures). When the annotation is absent, fall back to **inferring**
    it from the body (B3). None ⇒ can't determine (caller flags it)."""
    ret = fn.returns
    if ret is None:
        return _infer_type_src(fn)
    if _is_generator(fn) and isinstance(ret, ast.Subscript):
        inner = ret.slice
        if isinstance(inner, ast.Tuple):  # Generator[T, S, R] -> T
            return ast.unparse(inner.elts[0])
        return ast.unparse(inner)  # Iterator[T] -> T
    return ast.unparse(ret)


def _infer_type_src(fn: ast.FunctionDef) -> str | None:
    """Infer a provider's type from what it returns/yields, when untyped (B3 — migration type
    inference). **Precision over recall**: only confident shapes are inferred, so `migrate` never
    emits a *wrong* annotation. Inferred:
      • `return/yield ClassName(...)` → `ClassName` (Name/Attribute whose final segment is
        Capitalized — the PEP 8 class convention; lowercase factory calls are NOT inferred);
      • a literal → its builtin type (`str`/`int`/`float`/`bool`/`bytes`/`list`/`dict`/`set`/`tuple`).
    A returned/yielded local name is resolved one level through a simple assignment (the very common
    `d = Db(); yield d` shape). Multiple conflicting shapes, or anything else (an unresolved name, a
    lowercase call) ⇒ None (flag it)."""
    own = _own_nodes(fn)
    assigns = _simple_assignments(own)
    inferred = set()
    for value in _return_values(own):
        node = assigns.get(value.id, value) if isinstance(value, ast.Name) else value
        inferred.add(_infer_one(node))
    inferred.discard(None)
    return inferred.pop() if len(inferred) == 1 else None


def _own_nodes(fn: ast.FunctionDef) -> list:
    """All AST nodes belonging to `fn` itself — excluding those inside nested defs/lambdas (whose
    statements belong to a different callable and would poison inference)."""
    nested = {id(n) for d in ast.walk(fn) if isinstance(d, (ast.FunctionDef, ast.AsyncFunctionDef,
              ast.Lambda)) and d is not fn for n in ast.walk(d)}
    return [n for n in ast.walk(fn) if id(n) not in nested]


def _return_values(own: list) -> list:
    """The `return`/`yield` value expressions among `own` nodes."""
    out = []
    for node in own:
        if isinstance(node, ast.Return) and node.value is not None:
            out.append(node.value)
        elif isinstance(node, ast.Yield) and node.value is not None:
            out.append(node.value)
    return out


def _simple_assignments(own: list) -> dict:
    """`name -> value node` for single-target `name = <expr>` assignments (last write wins)."""
    m: dict = {}
    for node in own:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            m[node.targets[0].id] = node.value
    return m


_LITERAL_NODES = {ast.List: "list", ast.Dict: "dict", ast.Set: "set", ast.Tuple: "tuple"}


def _infer_one(node) -> str | None:
    """The inferred type-source for one return/yield value, or None when not confidently inferable."""
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name) and func.id[:1].isupper():
            return func.id  # `Db()` -> Db
        if isinstance(func, ast.Attribute) and func.attr[:1].isupper():
            return ast.unparse(func)  # `mod.Client()` -> mod.Client
        return None  # lowercase call ⇒ a factory, return type unknown — don't guess
    if isinstance(node, ast.Constant):
        return type(node.value).__name__ if node.value is not None else None
    return _LITERAL_NODES.get(type(node))


# --------------------------------------------------------------------------- pass 1: fixture types
def _fixture_types(tree: ast.Module) -> dict:
    """fixture name -> provided-type source string (or None when it can't be inferred)."""
    out: dict = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef):
            for deco in node.decorator_list:
                if _dotted(deco) in ("pytest.fixture", "fixture"):
                    name = _kwarg(deco, "name")
                    fx_name = name.value if isinstance(name, ast.Constant) else node.name
                    out[fx_name] = _provided_type_src(node)
    return out


def _parametrize_argnames(deco: ast.Call) -> list:
    if not deco.args:
        return []
    first = deco.args[0]
    if isinstance(first, ast.Constant) and isinstance(first.value, str):
        return [s.strip() for s in first.value.replace(",", " ").split()]
    return []


# --------------------------------------------------------------------------- pass 2: transform
class _Migrator(ast.NodeTransformer):
    def __init__(self, fixture_types: dict, report: Report):
        self.fixture_types = fixture_types
        self.report = report
        self.used_builtins: set = set()  # riptide.builtins type names that need importing

    def visit_Import(self, node: ast.Import):
        names = []
        for alias in node.names:
            if alias.name == "pytest":
                self.report.mapped(node.lineno, "`import pytest` → `import riptide`")
                names.append(ast.alias(name="riptide", asname=None))
            else:
                names.append(alias)
        node.names = names
        return node

    def visit_ImportFrom(self, node: ast.ImportFrom):
        if node.module == "pytest":
            self.report.cant(node.lineno, f"`from pytest import {', '.join(a.name for a in node.names)}`"
                             " — rewrite imports to riptide equivalents manually")
        return node

    def visit_FunctionDef(self, node: ast.FunctionDef):
        self.generic_visit(node)
        is_fixture = any(_dotted(d) in ("pytest.fixture", "fixture") for d in node.decorator_list)
        if node.name.startswith("pytest_"):
            self.report.cant(node.lineno, f"hook `{node.name}` — riptide gets its own hook host later; port manually")
            return node
        if is_fixture:
            return self._fixture(node)
        if node.name.startswith("test_") or node.name.endswith("_test"):
            return self._test(node)
        return node

    # ---- fixture → provider ----
    def _fixture(self, node: ast.FunctionDef):
        new_decos = []
        for deco in node.decorator_list:
            dotted = _dotted(deco)
            if dotted in ("pytest.fixture", "fixture"):
                keep = []
                if (scope := _kwarg(deco, "scope")) is not None:
                    keep.append(ast.keyword(arg="scope", value=scope))
                if (autouse := _kwarg(deco, "autouse")) is not None:
                    keep.append(ast.keyword(arg="autouse", value=autouse))
                if (name := _kwarg(deco, "name")) is not None:
                    keep.append(ast.keyword(arg="name", value=name))
                if _kwarg(deco, "params") is not None:
                    self.report.cant(node.lineno, f"fixture `{node.name}` is parametrized (`params=`) — "
                                     "provider-level params aren't in riptide yet; convert to @riptide.cases on tests")
                new_decos.append(ast.copy_location(
                    ast.Call(func=_attr("riptide", "provides"), args=[], keywords=keep), deco))
                self.report.mapped(node.lineno, f"@pytest.fixture → @riptide.provides ({node.name})")
                if node.returns is None:
                    inferred = _infer_type_src(node)  # B3: infer the type from the body
                    if inferred is not None:
                        node.returns = ast.parse(inferred, mode="eval").body
                        self.report.mapped(node.lineno, f"provider `{node.name}`: return type inferred "
                                           f"→ `-> {inferred}` (B3)")
                    else:
                        self.report.cant(node.lineno, f"provider `{node.name}` has no return type — riptide wires by "
                                         f"type; add `-> <Type>` (e.g. `def {node.name}() -> Db:`)")
            else:
                new_decos.append(deco)
        node.decorator_list = new_decos
        if any(a.arg == "request" for a in node.args.args):
            self.report.cant(node.lineno, f"fixture `{node.name}` uses `request` — port to typed deps / yield teardown")
        return node

    # ---- test: marks + type-annotate fixture params ----
    def _test(self, node: ast.FunctionDef):
        param_names = {a.arg for a in node.args.args}
        new_decos = []
        for deco in node.decorator_list:
            dotted = _dotted(deco)
            if dotted in ("pytest.mark.parametrize", "mark.parametrize"):
                argvals = deco.args[1] if len(deco.args) > 1 else ast.List(elts=[], ctx=ast.Load())
                ids = _kwarg(deco, "ids")
                kws = [ast.keyword(arg="ids", value=ids)] if ids is not None else []
                new_decos.append(ast.copy_location(
                    ast.Call(func=_attr("riptide", "cases"), args=[argvals], keywords=kws), deco))
                self.report.mapped(node.lineno, f"@pytest.mark.parametrize → @riptide.cases ({node.name})")
                for an in _parametrize_argnames(deco):
                    param_names.discard(an)  # parametrize args are NOT fixtures — don't type-annotate
            elif dotted in ("pytest.mark.skipif", "mark.skipif"):
                cond = deco.args[0] if deco.args else ast.Constant(value=True)
                kws = []
                if (r := _kwarg(deco, "reason")) is not None:
                    kws.append(ast.keyword(arg="reason", value=r))
                new_decos.append(ast.copy_location(
                    ast.Call(func=_attr("riptide", "skip_if"), args=[cond], keywords=kws), deco))
                self.report.mapped(node.lineno, f"@pytest.mark.skipif → @riptide.skip_if ({node.name})")
            elif dotted in ("pytest.mark.skip", "mark.skip"):
                new_decos.append(ast.copy_location(_call("riptide", "skip", _kwarg(deco, "reason")), deco))
                self.report.mapped(node.lineno, f"@pytest.mark.skip → @riptide.skip ({node.name})")
            elif dotted in ("pytest.mark.xfail", "mark.xfail"):
                new_decos.append(ast.copy_location(_call("riptide", "xfail", _kwarg(deco, "reason")), deco))
                self.report.mapped(node.lineno, f"@pytest.mark.xfail → @riptide.xfail ({node.name})")
            elif dotted in ("pytest.mark.usefixtures", "mark.usefixtures"):
                names = [a.value for a in deco.args if isinstance(a, ast.Constant)]
                types = [self.fixture_types.get(n) for n in names]
                if names and all(t is not None for t in types):
                    # @pytest.mark.usefixtures("a","b") → @riptide.uses(A, B), wired by type (B2)
                    args = [ast.parse(t, mode="eval").body for t in types]
                    new_decos.append(ast.copy_location(
                        ast.Call(func=_attr("riptide", "uses"), args=args, keywords=[]), deco))
                    self.report.mapped(node.lineno, f"@pytest.mark.usefixtures({names}) → "
                                       f"@riptide.uses({', '.join(types)}) ({node.name})")
                else:
                    unknown = [n for n, t in zip(names, types) if t is None]
                    self.report.cant(node.lineno, f"`usefixtures({unknown})` in `{node.name}` — those "
                                     "fixtures are untyped/unknown, so no type to wire; annotate the "
                                     "provider's return type or mark it autouse")
            elif dotted.startswith("pytest.mark.") or dotted.startswith("mark."):
                tagname = dotted.rsplit(".", 1)[-1]
                new_decos.append(ast.copy_location(
                    ast.Call(func=_attr("riptide", "tag"), args=[ast.Constant(value=tagname)], keywords=[]), deco))
                self.report.mapped(node.lineno, f"@pytest.mark.{tagname} → @riptide.tag({tagname!r}) ({node.name})")
            else:
                new_decos.append(deco)
        node.decorator_list = new_decos

        # Type-annotate fixture params from the fixture-type map; flag the un-inferable ones.
        for arg in node.args.args:
            if arg.arg not in param_names or arg.arg in ("self", "cls") or arg.annotation is not None:
                continue
            if arg.arg in BUILTIN_PROVIDERS:
                tname = BUILTIN_PROVIDERS[arg.arg]
                arg.annotation = ast.Name(id=tname, ctx=ast.Load())
                self.used_builtins.add(tname)
                caveat = " — port py.path calls (.join/.strpath) to pathlib" if arg.arg == "tmpdir" else ""
                self.report.mapped(node.lineno, f"builtin `{arg.arg}` → `{arg.arg}: {tname}` "
                                   f"(riptide.builtins, type-DI) in {node.name}{caveat}")
                continue
            if arg.arg in BUILTIN_FIXTURES:
                self.report.cant(node.lineno, f"test `{node.name}` requests pytest builtin `{arg.arg}` — "
                                 "no riptide equivalent yet; provide your own resource")
                continue
            if arg.arg in self.fixture_types:
                tsrc = self.fixture_types[arg.arg]
                if tsrc is None:
                    self.report.cant(node.lineno, f"test `{node.name}`: fixture param `{arg.arg}` — provider is "
                                     f"untyped, so the type can't be inferred; annotate `{arg.arg}: <Type>` manually")
                else:
                    arg.annotation = ast.parse(tsrc, mode="eval").body
                    self.report.mapped(node.lineno, f"param `{arg.arg}` → `{arg.arg}: {tsrc}` (type-DI) in {node.name}")
        return node


def _attr(base: str, attr: str) -> ast.Attribute:
    return ast.Attribute(value=ast.Name(id=base, ctx=ast.Load()), attr=attr, ctx=ast.Load())


def _call(base: str, attr: str, reason) -> ast.Call:
    kws = [ast.keyword(arg="reason", value=reason)] if reason is not None else []
    return ast.Call(func=_attr(base, attr), args=[], keywords=kws)


# --------------------------------------------------------------------------- public API
def migrate_source(src: str) -> tuple[str, Report]:
    """Return `(migrated_source, report)` for one module's source."""
    tree = ast.parse(src)
    report = Report()
    fixture_types = _fixture_types(tree)
    migrator = _Migrator(fixture_types, report)
    new_tree = migrator.visit(tree)
    _inject_builtins_import(new_tree, migrator.used_builtins)
    ast.fix_missing_locations(new_tree)
    return ast.unparse(new_tree), report


def _inject_builtins_import(tree: ast.Module, used: set) -> None:
    """Add `from riptide.builtins import <types>` after the module docstring + any `__future__`
    imports (which must stay first), so migrated builtin params resolve by type."""
    if not used:
        return
    node = ast.ImportFrom(
        module="riptide.builtins",
        names=[ast.alias(name=n, asname=None) for n in sorted(used)],
        level=0,
    )
    idx = 0
    body = tree.body
    if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant):
        idx = 1  # keep the module docstring first
    while idx < len(body) and isinstance(body[idx], ast.ImportFrom) and body[idx].module == "__future__":
        idx += 1  # `from __future__ import ...` must precede other imports
    body.insert(idx, node)


def format_report(report: Report, path: str = "<source>") -> str:
    lines = [f"# migration report — {path}", ""]
    lines.append(f"## mapped ({len(report.mappings)})")
    for f in report.mappings:
        lines.append(f"  L{f.lineno}: {f.message}")
    cm = report.cant_map
    lines.append("")
    lines.append(f"## CANNOT MAP — finish by hand ({len(cm)})")
    if not cm:
        lines.append("  (none — fully migrated)")
    for f in cm:
        lines.append(f"  L{f.lineno}: {f.message}")
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="riptide migrate", description="pytest → riptide source codemod")
    ap.add_argument("path", help="a .py file to migrate")
    ap.add_argument("--write", action="store_true", help="write <path>.riptide.py (default: report only)")
    args = ap.parse_args(argv)

    with open(args.path) as fh:
        src = fh.read()
    migrated, report = migrate_source(src)
    print(format_report(report, args.path))
    if args.write:
        out = args.path[:-3] + ".riptide.py" if args.path.endswith(".py") else args.path + ".riptide"
        with open(out, "w") as fh:
            fh.write(migrated)
        print(f"\nwrote {out}")
    # Non-zero exit when manual work remains — useful in CI adoption gates.
    return 1 if report.cant_map else 0


if __name__ == "__main__":
    sys.exit(main())
