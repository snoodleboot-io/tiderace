"""riptide — native, type-driven test authoring. **No pytest.**

The user-facing surface of the pure-Rust engine (ADR-E001/E012). Resources are wired by **type**, not
by name; the engine (collection, scopes, fork) stays in Rust. Each decorator stamps a riptide-owned
attribute (`__riptide_provider__` / `__riptide_cases__`) that the shim reads — the native replacement
for duck-typing on pytest's decorator.

    import riptide

    @riptide.provides(scope="module")
    def db() -> Db:
        conn = Db.connect(":memory:")
        yield conn                      # yield ⇒ teardown at scope exit
        conn.close()

    def test_insert(db: Db):            # injected BY TYPE, not by the name "db"
        db.add("ada")
        assert db.count() == 1

    @riptide.cases([(2, 3, 5), (0, 0, 0)])
    def test_add(a, b, exp):
        assert add(a, b) == exp
"""
from __future__ import annotations

import inspect
import itertools

from ._errors import RiptideDefinitionError, RiptideError, RiptideResolutionError
from ._resolve import build_type_index, provided_type, resolve_params
from ._spec import SCOPES, Case, Mark, ProviderSpec

__all__ = [
    "provides",
    "cases",
    "skip",
    "skip_if",
    "xfail",
    "tag",
    "ProviderSpec",
    "Case",
    "Mark",
    "RiptideError",
    "RiptideDefinitionError",
    "RiptideResolutionError",
    "build_type_index",
    "resolve_params",
    "provided_type",
]


def provides(_fn=None, *, scope: str = "function", autouse: bool = False, name=None, type=None):
    """Declare a resource (riptide's fixture). The provided type is `type=` or the function's return
    annotation (unwrapping `Iterator[T]`/`Generator[T, ...]` for yield-style teardown)."""

    def deco(fn):
        ptype = type or provided_type(fn)
        if ptype is None:
            raise RiptideDefinitionError(
                f"@provides {fn.__name__}: cannot determine the provided type — annotate the return "
                f"(`def {fn.__name__}() -> T:`) or pass `type=T`"
            )
        if scope not in SCOPES:
            raise RiptideDefinitionError(
                f"@provides {fn.__name__}: unknown scope {scope!r}; one of {SCOPES}"
            )
        fn.__riptide_provider__ = ProviderSpec(
            provides=ptype,
            scope=scope,
            autouse=autouse,
            name=name or fn.__name__,
            is_yield=inspect.isgeneratorfunction(fn),
        )
        return fn

    return deco(_fn) if _fn is not None else deco


def cases(arg=None, *, ids=None, **kwargs):
    """Parametrize a test. Positional rows — `cases([(2, 3, 5), (0, 0, 0)])` — or single-param
    shorthand — `cases(x=[1, 2, 3])` (cartesian across multiple kwargs). Stamps `__riptide_cases__`."""

    def deco(fn):
        fn.__riptide_cases__ = _normalize_cases(arg, kwargs, ids)
        return fn

    return deco


def _add_mark(fn, mark: Mark):
    """Append `mark` to `fn.__riptide_marks__` (creating it), or — when `fn` is None (the decorator was
    called with arguments) — return a decorator that will."""
    if fn is None:
        return lambda f: _add_mark(f, mark)
    marks = fn.__dict__.setdefault("__riptide_marks__", [])
    marks.append(mark)
    return fn


def skip(_fn=None, *, reason: str = ""):
    """Always skip this test — `@riptide.skip` or `@riptide.skip(reason=...)`."""
    return _add_mark(_fn, Mark(kind="skip", reason=reason))


def skip_if(condition, *, reason: str = ""):
    """Skip when `condition` is truthy — `@riptide.skip_if(sys.platform == 'win32', reason=...)`."""
    return _add_mark(None, Mark(kind="skip_if", condition=bool(condition), reason=reason))


def xfail(_fn=None, *, reason: str = "", strict: bool = False):
    """Expect failure — a fail/error becomes `xfail`; a pass becomes `xpass` (a failure if `strict`)."""
    return _add_mark(_fn, Mark(kind="xfail", reason=reason, strict=strict))


def tag(*names: str):
    """Attach selection tag(s) — `@riptide.tag("slow", "db")`. Metadata only; no execution effect."""

    def deco(fn):
        for n in names:
            _add_mark(fn, Mark(kind="tag", name=n))
        return fn

    return deco


def _normalize_cases(arg, kwargs, ids) -> list[Case]:
    rows: list[tuple] = []
    if arg is not None:
        rows = [v if isinstance(v, tuple) else (v,) for v in arg]
    elif kwargs:
        names = list(kwargs)
        rows = [combo for combo in itertools.product(*(kwargs[n] for n in names))]

    out: list[Case] = []
    for index, values in enumerate(rows):
        cid = ids[index] if ids and index < len(ids) else "-".join(str(v) for v in values)
        out.append(Case(id=cid, index=index, values=values))
    return out
