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
    """The provided type, as source, inferred from the return annotation (unwrapping
    `Iterator[T]`/`Generator[T, ...]` for yield fixtures). None ⇒ can't infer."""
    ret = fn.returns
    if ret is None:
        return None
    if _is_generator(fn) and isinstance(ret, ast.Subscript):
        inner = ret.slice
        if isinstance(inner, ast.Tuple):  # Generator[T, S, R] -> T
            return ast.unparse(inner.elts[0])
        return ast.unparse(inner)  # Iterator[T] -> T
    return ast.unparse(ret)


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
                names = ", ".join(a.value for a in deco.args if isinstance(a, ast.Constant))
                self.report.cant(node.lineno, f"`usefixtures({names!r})` — string names carry no type; "
                                 "request them as typed params or mark the provider autouse=True")
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
