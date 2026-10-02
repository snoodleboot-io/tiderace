"""What a run selects (TID-124): `-k`, `-m`, `--strict-markers` and the marks the project declared,
as one [`Selection`] — loaded from the project's own `addopts` and the environment, replaced per
run by the daemon's patch ([`Selection.override`]) — with the grammar both filters share (TID-63)
and the names `-k` matches against (TID-100).

Four module globals used to carry this (`_KEYWORD_EXPR`, `_MARKER_EXPR`, `_STRICT_MARKS`,
`_DECLARED_MARKS`), set by discovery and reset by `_apply_selection` in every pool worker, and
read per node by the gate. Now the engine holds one `Selection` and the gate asks it.
"""
from __future__ import annotations

import functools
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Callable, Mapping

from .log import warn

if TYPE_CHECKING:
    from .config import ProjectConfig

# pytest's identifier class for `-m` / `-k`: a keyword may be a parametrize id, `test_x[1-a]`.
IDENT = re.compile(r"[\w.:+\-\[\]\\/]+")

# Marks pytest itself defines; `--strict-markers` never complains about these.
BUILTIN_MARKS = frozenset({
    "skip", "skipif", "xfail", "parametrize", "usefixtures", "filterwarnings", "tryfirst", "trylast",
})


# ------------------------------------------------------------------------------ the grammar
def parse_expr(expr: str):
    """pytest's selection grammar — identifiers, `and`, `or`, `not`, parentheses — as a tree.

    Shared by `-m` and `-k` (TID-63): it is one grammar over two predicates. Parsed by hand rather
    than with `ast`, which the `-m` path used to use: an `ast` identifier cannot contain `-` or
    `[`, and `-k "test_x[1-a]"` is the ordinary way to name one parametrize case. Raises
    `ValueError` on anything outside the grammar, so a filter we cannot read is reported once and
    selects everything rather than silently deselecting the wrong tests.

    Tree shape: `("ident", s)`, `("not", t)`, `("and", [ts])`, `("or", [ts])`."""
    tokens: list = []
    i = 0
    while i < len(expr):
        c = expr[i]
        if c.isspace():
            i += 1
            continue
        if c in "()":
            tokens.append(c)
            i += 1
            continue
        m = IDENT.match(expr, i)
        if not m:
            raise ValueError(f"unexpected character {c!r} at position {i}")
        tokens.append(m.group(0))
        i = m.end()
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def take():
        nonlocal pos
        pos += 1
        return tokens[pos - 1]

    def parse_or():
        items = [parse_and()]
        while peek() == "or":
            take()
            items.append(parse_and())
        return items[0] if len(items) == 1 else ("or", items)

    def parse_and():
        items = [parse_not()]
        while peek() == "and":
            take()
            items.append(parse_not())
        return items[0] if len(items) == 1 else ("and", items)

    def parse_not():
        if peek() == "not":
            take()
            return ("not", parse_not())
        t = peek()
        if t is None:
            raise ValueError("expected an identifier")
        if t == "(":
            take()
            inner = parse_or()
            if peek() != ")":
                raise ValueError("missing ')'")
            take()
            return inner
        if t in (")", "and", "or"):
            raise ValueError(f"unexpected {t!r}")
        return ("ident", take())

    tree = parse_or()
    if pos != len(tokens):
        raise ValueError(f"unexpected {tokens[pos]!r}")
    return tree


def evaluate(tree, resolve):
    """Evaluate a selection tree; `resolve(ident)` is True, False, or None for "cannot tell yet".

    Three-valued so a node can be decided *before* its parametrize cases exist (TID-63). A `-k`
    identifier that does not match the plain node id might still match a case id — `-k 1-a` against
    `test_x[1-a]` — so at node level a non-match is "unknown", not "no". Kleene's rules: `not` of
    unknown is unknown; `and` is False on any False, else unknown on any unknown; `or` is True on
    any True, else unknown on any unknown. A False here is a False for every case that node could
    produce, which is what makes deselecting it up front — before any fixture is built — sound."""
    kind = tree[0]
    if kind == "ident":
        return resolve(tree[1])
    if kind == "not":
        v = evaluate(tree[1], resolve)
        return None if v is None else not v
    values = [evaluate(t, resolve) for t in tree[1]]
    if kind == "and":
        if any(v is False for v in values):
            return False
        return None if any(v is None for v in values) else True
    if any(v is True for v in values):
        return True
    return None if any(v is None for v in values) else False


def compile_tree(expr: str, flag: str):
    """`expr` as a tree, or None — reported once — when it is outside the grammar."""
    try:
        return parse_expr(expr)
    except ValueError as exc:
        warn(f"ignoring {flag} {expr!r}: {exc}")
        return None


def compile_marker_expr(expr: str):
    """A predicate over a set of mark names for one pytest `-m` expression.

    The same grammar `-k` uses (TID-63), over "is this identifier one of the node's marks"."""
    tree = compile_tree(expr, "-m")
    if tree is None:
        return None
    return lambda marks: bool(evaluate(tree, lambda ident: ident in marks))


def keyword_matches(ident: str, names: list) -> bool:
    """pytest's rule: a case-insensitive substring of any of the names."""
    needle = ident.lower()
    return any(needle in name.lower() for name in names)


# ------------------------------------------------------------------------------ the names `-k` sees
def pytest_major() -> int:
    """The major version of the pytest the suite's interpreter has, or the current one's behaviour
    (a large number) when there is none to ask."""
    try:
        import pytest
        return int(str(pytest.__version__).split(".")[0])
    except Exception:  # noqa: BLE001 — no pytest, or an unparsable version
        return 99


@functools.lru_cache(maxsize=None)  # per module: fixed for the life of the process
def path_names(module_key: str, root: str, rootdir: str) -> tuple:
    """The names pytest's `-k` takes from a module's *path* (TID-100); `root` is the run root,
    absolute, and `rootdir` the project's.

    pytest's `KeywordMatcher` takes the name of every node on the item's chain except the session
    and the root `Directory`, and a node's name is its path relative to its parent node's. Since
    pytest 8 every directory is a `Dir` / `Package` node, so the names are each directory below
    the rootdir and the module's file name — `-k unit` selects everything under `tests/unit/`,
    4,557 of pirn-core's tests, where matching the file and test names alone selected none.
    pytest 7 nests nothing: a module whose own directory holds an `__init__.py` sits under that
    one `Package`, named by its basename, and is named by its own; any other module sits under
    the session, named by its whole path from the rootdir — `tests/test_arguments.py`, which is
    how click's `-k tests` matches on 7. The rootdir — the directory the ini was read from, else
    the run root — is the root `Directory`, whose name pytest leaves out."""
    module_path = os.path.join(root, module_key)
    try:
        rel = os.path.relpath(module_path, rootdir)
        if rel.startswith(os.pardir):  # the ini sits beside, not above: the run root is the rootdir
            rootdir, rel = root, os.path.relpath(module_path, root)
    except ValueError:  # Windows: the config and the run root on different drives — no common
        rootdir, rel = root, module_key  # ancestor; the run root is the rootdir then
    parts = [p for p in rel.replace("\\", "/").split("/") if p and p != os.curdir and p != os.pardir]
    if pytest_major() >= 8:
        return tuple(parts)
    if len(parts) > 1 and os.path.exists(os.path.join(os.path.dirname(module_path), "__init__.py")):
        return (parts[-2], parts[-1])
    return ("/".join(parts),)


# ------------------------------------------------------------------------------ declared marks
def registered_marks(project: ProjectConfig, env: Mapping[str, str] = os.environ) -> tuple:
    """`(declared names, strict)` — which marks the project declared, and whether it wants them checked.

    pytest projects declare marks as `markers = ["slow: ...", ...]` in their config and opt into
    validation with `--strict-markers`; a tiderace-native project says the same thing under
    `[tool.tiderace]`. Both are read, because a suite mid-migration has both kinds of test in it."""
    names: set = set()
    strict = project.flag("--strict-markers") or project.flag("--strict")
    for raw in project.values("markers"):
        # pytest's spelling is "name: description" or a bare name; only the name selects.
        name = str(raw).split(":", 1)[0].strip()
        if name:
            names.add(name.partition("(")[0].strip())  # `name(args)` in a few suites
    if project.values("strict_markers"):
        strict = True
    # The native declaration surface (TID-67): `tiderace.mark.register("slow", ...)` in a conftest.
    # Every conftest has been imported by the time this runs, so whatever they registered is here.
    # Without it a suite written natively — no `import pytest` anywhere — still needed a *pytest*
    # config block to declare its own marks, or strict checking rejected them.
    try:
        import tiderace
        names.update(tiderace.mark.registered())
    except Exception:  # noqa: BLE001 — no native package on this interpreter ⇒ no native marks
        pass
    # `--strict-markers` on the command line (TID-67), for the same reason `-m` and `-k` travel
    # this way: a project with no config file at all has nowhere else to say it.
    if env.get("TIDERACE_STRICT_MARKERS") == "1":
        strict = True
    return frozenset(names), strict


@functools.lru_cache(maxsize=None)  # asked once per process, whatever it answers (TID-91)
def plugin_marks(root: str) -> frozenset | None:
    """Every marker the installed pytest plugins register, or `None` if we could not find out.

    A plugin registers its markers at runtime — pytest-timeout's `timeout`, pytest-benchmark's
    `benchmark`, pytest-django's `django_db` — by calling `addinivalue_line("markers", ...)` from
    `pytest_configure`. None of that is in the project's own `markers` list, and tiderace does not run
    plugins, so a hand-written allowlist would flag every one of them as a typo. pirn-core showed
    exactly that: 60 tests erroring on `@pytest.mark.timeout`, which pytest accepts without comment
    (TID-60).

    So ask pytest, which already knows: `--markers` prints the registered set, plugins included. One
    subprocess, only when a project has turned strict checking on, cached for the run.

    `None` means we could not get an answer, and the caller must then **not** enforce: a false error
    on a valid mark is worse than a missed typo, because it fails a suite that is correct."""
    try:
        out = subprocess.run(
            [sys.executable, "-m", "pytest", "--markers"],
            capture_output=True, text=True, timeout=60, cwd=root or None,
        ).stdout
    except Exception:  # noqa: BLE001 — no pytest, or it refused to start
        return None
    names = frozenset(re.findall(r"^@pytest\.mark\.(\w+)", out, re.M))
    return names or None  # an empty answer is not an answer


# ------------------------------------------------------------------------------ the selection
@dataclass(frozen=True)
class Selection:
    """This run's `-k` / `-m` / `--strict-markers`, and the marks the project declared."""

    keyword: Any = None  # `-k EXPR` as a parsed tree, or None when no name filter applies (TID-63)
    marker: Callable[[set], bool] | None = None  # the `-m` predicate (TID-32), or None
    strict_markers: bool = False  # --strict-markers: using an undeclared mark is an error, as in pytest
    declared_marks: frozenset = frozenset()  # names the project declared via `markers = [...]`

    @classmethod
    def load(cls, project: ProjectConfig, env: Mapping[str, str] = os.environ) -> Selection:
        """The project's own `addopts`, overridden by the command line — `TIDERACE_MARKER_EXPR`,
        `TIDERACE_KEYWORD_EXPR`, `TIDERACE_STRICT_MARKERS` — as it is in pytest: a config filter is
        the project's default, and `-m` / `-k` on the command line is this run's intent (TID-59,
        TID-63). A malformed expression is reported once and selects everything."""
        expr = env.get("TIDERACE_MARKER_EXPR") or project.opt("-m")
        kexpr = env.get("TIDERACE_KEYWORD_EXPR") or project.opt("-k")
        declared, strict = registered_marks(project, env)
        return cls(keyword=compile_tree(kexpr, "-k") if kexpr else None,
                   marker=compile_marker_expr(expr) if expr else None,
                   strict_markers=strict, declared_marks=declared)

    def override(self, patch: dict | None) -> Selection:
        """This selection with the daemon's per-run patch applied — `{"keyword", "marker",
        "strict_markers"}` — for a worker forked off a warm image (TID-90). A field that is absent
        *or null* keeps the image's value: the daemon serialises the run's selection with every
        field present, `null` for the ones the run did not give, and reading `null` as "clear it"
        dropped the project's own `addopts -m` on every `-k` run through the daemon — 32 tests
        pirn-core's config deselects ran (TID-102). An explicit empty string clears (a `-k ""`)."""
        if not patch:
            return self
        keyword, marker, strict = self.keyword, self.marker, self.strict_markers
        if patch.get("keyword") is not None:
            kexpr = patch["keyword"]
            keyword = compile_tree(kexpr, "-k") if kexpr else None
        if patch.get("marker") is not None:
            expr = patch["marker"]
            marker = compile_marker_expr(expr) if expr else None
        if patch.get("strict_markers"):
            strict = True
        return Selection(keyword, marker, strict, self.declared_marks)

    def keyword_verdict(self, names: list, final: bool):
        """`-k` applied to a node whose `-k` names are `names`: True (run it), False (deselect it),
        or None (decide per case). `final=True` — the id is a complete case id, or the node has no
        cases — turns every non-match into a No. True when there is no `-k`."""
        if self.keyword is None:
            return True

        def resolve(ident: str):
            if keyword_matches(ident, names):
                return True
            return False if final else None

        return evaluate(self.keyword, resolve)

    def marker_allows(self, marks: set) -> bool:
        """`-m` applied to a node's mark names; True when there is no `-m`."""
        return self.marker is None or bool(self.marker(marks))

    def unknown_marks(self, marks: set, root: str) -> list:
        """Under `--strict-markers`: the marks in `marks` nothing declared — not the project, not
        pytest itself, not an installed plugin (asked once, `plugin_marks`). Empty when checking is
        off, and when pytest could not say: a false error on a valid mark fails a suite that is
        correct, which is worse than missing a typo (TID-59, TID-60)."""
        if not self.strict_markers:
            return []
        unknown = sorted(n for n in marks if n and n not in self.declared_marks and n not in BUILTIN_MARKS)
        if unknown:
            registered = plugin_marks(root)
            unknown = [n for n in unknown if n not in registered] if registered is not None else []
        return unknown
