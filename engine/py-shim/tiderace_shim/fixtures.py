"""The fixture model (TID-124, step 5): a `FixtureDef` — what a `@pytest.fixture` or a native provider
declares — the `Registry` that holds them by name and by type with pytest's nearest-override rule,
the closure a test needs widest-first, and what a live wider-scope fixture is to the engine.
"""
from __future__ import annotations

import inspect
import os
import typing

from .nodes import class_method as _class_method, module_key as _module_key
from .safe import safe_getattr as _safe_getattr, safe_hasattr as _safe_hasattr


_SCOPE_RANK = {"function": 0, "class": 1, "module": 2, "package": 3, "session": 4}


def _test_dir(module_key: str) -> str:
    return os.path.dirname(module_key)


def _is_ancestor_dir(loc: str, test_dir: str) -> bool:
    """True if directory `loc` is `test_dir` or an ancestor of it (''=root, ancestor of all)."""
    if loc == "":
        return True
    if loc.startswith(".."):
        return True  # above the run root (TID-19) ⇒ ancestor of every test inside it
    return test_dir == loc or test_dir.startswith(loc + "/")


def _location_depth(loc: str) -> int:
    """How specific a conftest directory is — deeper wins in `Registry.resolve`.

    The run root is 0 and directories under it count their segments. A conftest ABOVE the run root
    (TID-19) is expressed as a `..`-relative path and scores NEGATIVE, one step per level up, so the
    total order stays `../.. < .. < run root < tests < tests/sessions`. That is what keeps a nearer
    conftest overriding a farther one in both directions."""
    if not loc:
        return 0
    depth = len(loc.split("/"))
    return -depth if loc.startswith("..") else depth


class FixtureDef:
    """A discovered fixture definition + the location it was declared at.

    `bindings` maps each of the function's parameter *names* to the *provider name* that satisfies it.
    For pytest-authored fixtures the two are identical (name-DI); for tiderace-native providers they may
    differ (the param is wired by **type**, ADR-E012), so callers must build kwargs from `bindings`,
    not from raw parameter names. `deps` (provider names — the registry keys the closure walks) is
    derived from the bindings."""

    __slots__ = (
        "name", "scope", "params", "autouse", "func", "location", "deps", "is_yield",
        "bindings", "provides_type", "param_ids", "owner",
    )

    def __init__(self, name, scope, params, autouse, func, location, bindings=None, provides_type=None,
                 param_ids=None, owner=None):
        self.name = name
        self.scope = scope if isinstance(scope, str) else "function"
        self.params = list(params) if params else None
        # `@pytest.fixture(params=[...], ids=[...])` — a list, or a callable applied per value.
        # Carried so a parametrized fixture's cases id the way pytest spells them (TID-25).
        self.param_ids = param_ids
        self.autouse = bool(autouse)
        self.func = func
        self.location = location  # module key ('tests/m.py') for module fixtures, or dir for conftest
        self.provides_type = provides_type  # native: the type this provider is injected by (else None)
        # The class a fixture method was defined on, or None. A fixture defined inside a test class
        # is scoped to that class in pytest — flask's `TestRoutes.app` overrides the conftest `app`
        # for that class and nowhere else — and it is called with the instance as `self` (TID-47).
        self.owner = owner
        if bindings is None:
            sig = list(inspect.signature(func).parameters)
            skip = {"request"} | ({"self", "cls"} if owner is not None else set())
            bindings = {p: p for p in sig if p not in skip}  # pytest/name-DI: identity
        self.bindings = bindings  # param_name -> provider_name
        self.deps = list(bindings.values())
        self.is_yield = inspect.isgeneratorfunction(func)

    @property
    def rank(self) -> int:
        return _SCOPE_RANK.get(self.scope, 0)

    @property
    def wants_request(self) -> bool:
        return "request" in inspect.signature(self.func).parameters


def _fixture_marker(obj):
    """The `FixtureFunctionMarker` for a `@pytest.fixture`, on any pytest version, or None.

    pytest moved it in 8.4, and the old location is the only one many real suites have (TID-44):

    | pytest  | `@pytest.fixture` returns     | marker                     | real function            |
    | ------- | ----------------------------- | -------------------------- | ------------------------ |
    | < 8.4   | the function, wrapped         | `_pytestfixturefunction`   | `__pytest_wrapped__.obj` |
    | >= 8.4  | a `FixtureFunctionDefinition` | `_fixture_function_marker` | `_fixture_function`      |

    Only the new names were recognised, so on any suite pinning an older pytest *every* fixture was
    invisible and every test requesting one failed with a missing positional argument. click (pytest
    7.4) and flask (8.1) lost 363 and 387 tests that way. Nothing caught it because every corpus the
    engine had been validated against happened to run pytest 9.

    The marker itself is the same `FixtureFunctionMarker` with the same fields on both sides of the
    move, so only *finding* it differs."""
    marker = _safe_getattr(obj, "_fixture_function_marker", None)  # pytest >= 8.4
    if marker is None:
        marker = _safe_getattr(obj, "_pytestfixturefunction", None)  # pytest < 8.4
    return marker


def _fixture_function(obj):
    """The callable a fixture actually runs, on any pytest version, or None.

    Never the decorated object itself: on every pytest version that is a wrapper whose job is to
    raise `Failed: Fixture "x" called directly`. `Failed` derives from `BaseException`, so a caller
    catching `Exception` would not even see it happen."""
    func = _safe_getattr(obj, "_fixture_function", None)  # pytest >= 8.4
    if func is None:
        wrapped = _safe_getattr(obj, "__pytest_wrapped__", None)  # pytest < 8.4
        func = _safe_getattr(wrapped, "obj", None) if wrapped is not None else None
    return func


def _is_fixture(obj) -> bool:
    return _fixture_marker(obj) is not None and _fixture_function(obj) is not None


def _is_native_provider(obj) -> bool:
    """A tiderace-native provider (ADR-E012) — carries the tiderace-owned marker, not pytest's."""
    return _safe_hasattr(obj, "__tiderace_provider__")


def _safe_type_hints(func) -> dict:
    try:
        return typing.get_type_hints(func, include_extras=True)
    except Exception:  # noqa: BLE001 — an unresolved annotation ⇒ treat as untyped (name fallback)
        return {}


def _provider_for_type(annotation, type_index: dict):
    """The single provider name registered for `annotation`'s type, or None (0 or >1 ⇒ name fallback).
    `Annotated[T, "name"]` disambiguates. Strict ambiguity errors are the `tiderace` package's job at
    author time; the shim stays lenient so mixed/compat suites keep running."""
    key, want = annotation, None
    if typing.get_origin(annotation) is typing.Annotated:
        key, *meta = typing.get_args(annotation)
        want = next((m for m in meta if isinstance(m, str)), None)
    candidates = list(type_index.get(key, ()))
    if want is not None:
        candidates = [c for c in candidates if c == want]
    return candidates[0] if len(candidates) == 1 else None


def _bind_by_type(func, type_index: dict) -> dict:
    """`param_name -> provider_name`, wired by TYPE (ADR-E012). Falls back to the param *name* when the
    parameter is untyped or its type has no unique provider — which makes pytest-authored suites
    (untyped fixture args, empty type index) resolve exactly as before."""
    hints = _safe_type_hints(func)
    out = {}
    for pname in inspect.signature(func).parameters:
        if pname in ("self", "cls", "request"):
            continue
        annotation = hints.get(pname)
        provider = _provider_for_type(annotation, type_index) if annotation is not None else None
        out[pname] = provider if provider is not None else pname
    return out


def _native_fixture_def(obj, location: str, type_index: dict) -> FixtureDef:
    spec = obj.__tiderace_provider__
    return FixtureDef(
        name=spec.name,
        # B5: provider-level params fan the provider out (read via `request.param`); `()` ⇒ unparametrized.
        params=list(spec.params) if getattr(spec, "params", ()) else None,
        scope=spec.scope,
        autouse=spec.autouse,
        func=obj,
        location=location,
        bindings=_bind_by_type(obj, type_index),  # provider→provider deps, by type
        provides_type=spec.provides,
    )


def _fixture_def(obj, location: str, owner=None, attr_name: str | None = None) -> FixtureDef:
    # Both accessors handle pytest before and after 8.4 (TID-44); see `_fixture_marker`.
    marker = _fixture_marker(obj)
    func = _fixture_function(obj)
    # pytest names a fixture by `name=` when given, else by the **attribute** it is bound to in its
    # module or class — not by the function's `__name__`. `mocker = pytest.fixture()(_mocker)` and
    # its four scope-siblings are five fixtures wrapping one function (TID-87).
    return FixtureDef(
        name=getattr(marker, "name", None) or attr_name or func.__name__,
        scope=getattr(marker, "scope", "function"),
        params=getattr(marker, "params", None),
        autouse=getattr(marker, "autouse", False),
        func=func,
        location=location,
        param_ids=getattr(marker, "ids", None),
        owner=owner,
    )


class Registry:
    """All discovered fixtures, indexed by name (a name may have several location-scoped defs)."""

    def __init__(self):
        self.by_name: dict[str, list[FixtureDef]] = {}
        self.by_type: dict[type, list[str]] = {}  # native: provided-type -> [provider name]

    def add(self, fdef: FixtureDef) -> None:
        self.by_name.setdefault(fdef.name, []).append(fdef)
        if fdef.provides_type is not None:
            self.by_type.setdefault(fdef.provides_type, []).append(fdef.name)

    def bind_params(self, func) -> dict:
        """`param_name -> provider_name` for a test/provider, wired by type (name fallback)."""
        return _bind_by_type(func, self.by_type)

    def is_provider(self, name) -> bool:
        """Whether `name` is a discovered provider (vs. a bare test param filled by @cases)."""
        return name in self.by_name

    def resolve(self, name: str, module_key: str, classes: tuple = (),
                below: int | None = None) -> FixtureDef | None:
        """Nearest-override: among defs of `name` visible here, pick the most specific.

        Order, narrowest first: a fixture defined in the test's own class (or a base of it) beats one
        defined at module level, which beats a conftest, and a deeper conftest beats a shallower one.
        `classes` is the test class's MRO names — pytest collects fixtures from base classes too.
        `below` looks *past* an override: a fixture may request the very name it overrides, and must
        then be given the definition it shadows rather than itself (TID-47)."""
        best = None
        for spec, d in self.visible(name, module_key, classes):
            if below is not None and spec >= below:
                continue  # looking *past* an override, for the def it shadows
            if best is None or spec > best[0]:
                best = (spec, d)
        return best[1] if best else None

    def visible(self, name: str, module_key: str, classes: tuple = ()):
        """`(specificity, def)` for every def of `name` in scope here — narrower is larger."""
        test_dir = _test_dir(module_key)
        for d in self.by_name.get(name, ()):
            if "::" in d.location:  # class fixture: visible only inside its own class
                owner_module, _, owner_cls = d.location.partition("::")
                if owner_module != module_key or owner_cls not in classes:
                    continue
                yield 20_000, d      # narrower than anything else that can define this name
            elif d.location.endswith(".py"):  # module fixture: visible only in its own module
                if d.location == module_key:
                    yield 10_000, d
            elif _is_ancestor_dir(d.location, test_dir):
                yield _location_depth(d.location), d  # deeper dir = more specific

    def specificity(self, fdef: FixtureDef, module_key: str, classes: tuple = ()) -> int | None:
        """How specific `fdef` is here — the ceiling to look below when it requests the name it
        overrides."""
        for spec, d in self.visible(fdef.name, module_key, classes):
            if d is fdef:
                return spec
        return None

    def autouse_for(self, module_key: str, classes: tuple = ()) -> list[FixtureDef]:
        """Every autouse fixture visible to `module_key` (and the test's class), widest scope first."""
        test_dir = _test_dir(module_key)
        out = []
        for defs in self.by_name.values():
            for d in defs:
                if not d.autouse:
                    continue
                if "::" in d.location:  # class fixture: autouse only inside its own class (TID-47)
                    owner_module, _, owner_cls = d.location.partition("::")
                    visible = owner_module == module_key and owner_cls in classes
                elif d.location.endswith(".py"):
                    visible = d.location == module_key
                else:
                    visible = _is_ancestor_dir(d.location, test_dir)
                if visible:
                    out.append(d)
        out.sort(key=lambda d: -d.rank)
        return out


def _closure(reg: Registry, module_key: str, requested: dict, extra: list | None = None,
             classes: tuple = ()) -> list[FixtureDef]:
    """Resolved fixture closure for a test, dependencies-before-dependents (topo). Includes
    requested fixtures (the provider names of `requested`'s param→provider bindings), `extra` provider
    names (e.g. `@tiderace.uses` — set up but not injected), all in-scope autouse fixtures, and their
    transitive deps."""
    ordered: list[FixtureDef] = []
    # Keyed by definition, not by name: `app` overriding `app` is two defs that both have to be set
    # up, outer first, so the override receives the value it wraps (TID-47).
    seen: set = set()
    visiting: set = set()

    def visit(name: str, below: int | None = None) -> None:
        d = reg.resolve(name, module_key, classes, below=below)
        if d is None:
            return  # unknown name (e.g. a non-fixture arg) — the body call will surface it
        key = (d.name, d.location)
        if key in seen or key in visiting:
            return
        visiting.add(key)
        for dep in d.deps:
            # A fixture requesting its own name wants the definition it overrides — pytest's
            # override-and-extend idiom, e.g. `def app(self, app)` inside a test class.
            visit(dep, below=reg.specificity(d, module_key, classes) if dep == d.name else None)
        visiting.discard(key)
        if key not in seen:
            seen.add(key)
            ordered.append(d)

    # pytest's closure order, which is also the order its parametrised-fixture axes take in a node
    # id: the autouse fixtures, then `usefixtures` (and what a marker implies — anyio's backend),
    # then the signature's arguments. anyio's `TestConnectedUDPSocket.test_iterate(family)` is
    # `[asyncio-ipv4]` under pytest, the backend the plugin's `usefixtures` injects before the
    # `family` the test asks for (TID-87).
    for d in reg.autouse_for(module_key, classes):
        visit(d.name)
    for provider_name in extra or ():
        visit(provider_name)
    for provider_name in requested.values():
        visit(provider_name)
    return ordered


class _Active:
    """A live wider-scope fixture: its definition, its scope-instance key, its value and the handle
    that tears it down."""

    __slots__ = ("fdef", "key", "value", "handle")

    def __init__(self, fdef, key, value, handle):
        self.fdef = fdef
        self.key = key
        self.value = value
        self.handle = handle


def _instance_key(fdef: FixtureDef, node_id: str):
    s = fdef.scope
    if s == "session":
        return ("session", fdef.name)
    if s == "package":
        return ("package", fdef.name, fdef.location)
    if s == "module":
        return ("module", fdef.name, _module_key(node_id))
    if s == "class":
        return ("class", fdef.name, _module_key(node_id), _class_method(node_id)[0])
    return ("function", fdef.name, node_id)


def _async_provider(func) -> bool:
    """An `async def` provider (coroutine) or `async def ... yield` provider (async generator)."""
    return inspect.iscoroutinefunction(func) or inspect.isasyncgenfunction(func)
